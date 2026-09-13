//! B+Tree index — ordered map from byte keys to u64 row ids.
//!
//! M1 scope: single-threaded in-memory arena; node shapes mirror the planned
//! on-disk layout so M1-later serialization is mechanical. Duplicate keys are
//! allowed (ordered by key, then value) — model layers decide uniqueness.
//! Latch crabbing for concurrent descent lands in M2.

use std::fmt;

/// Opaque encoded key. Model layers own key encoding (D-002); kernel requires
/// only lexicographic total order.
pub type Key = Vec<u8>;
/// Leaf payload: row id (or first row id for duplicate runs).
pub type Value = u64;

const LEAF_MAX: usize = 64;
const INTERNAL_MAX_KEYS: usize = 63;

type NodeId = usize;

#[derive(Debug)]
struct Leaf {
    entries: Vec<(Key, Value)>,
    next: Option<NodeId>,
}

impl Leaf {
    /// First position with key >= target (lookup + insert boundary).
    fn lower_bound(&self, key: &[u8]) -> usize {
        self.entries.partition_point(|(k, _)| k.as_slice() < key)
    }
}

#[derive(Debug)]
struct Internal {
    keys: Vec<Key>,
    children: Vec<NodeId>,
}

impl Internal {
    /// Routing: equal separators go RIGHT. Leaf splits guarantee every
    /// duplicate run lives entirely in one leaf, so right-routing reaches
    /// the whole run (separator = smallest key of the right sibling).
    fn child_for(&self, key: &[u8]) -> NodeId {
        let idx = self.keys.partition_point(|k| k.as_slice() <= key);
        self.children[idx]
    }

    fn split(&mut self) -> (Key, Internal) {
        let mid = self.keys.len() / 2;
        let sep = self.keys[mid].clone();
        let right = Internal {
            keys: self.keys.split_off(mid + 1),
            children: self.children.split_off(mid + 1),
        };
        // popped separator is promoted, never stored in either half
        self.keys.pop();
        (sep, right)
    }
}

#[derive(Debug)]
enum Node {
    Leaf(Leaf),
    Internal(Internal),
}

pub struct BTree {
    nodes: Vec<Node>,
    root: NodeId,
    height: u32,
    entries: usize,
}

impl Default for BTree {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a recursive insert that split its subtree.
struct Split {
    sep: Key,
    right: NodeId,
}

impl BTree {
    pub fn new() -> Self {
        Self {
            nodes: vec![Node::Leaf(Leaf {
                entries: Vec::new(),
                next: None,
            })],
            root: 0,
            height: 1,
            entries: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries == 0
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Insert `value` under `key`. Duplicates append after existing equal keys.
    pub fn insert(&mut self, key: &[u8], value: Value) {
        if let Some(Split { sep, right }) = self.insert_at(self.root, key.to_vec(), value) {
            let new_root = self.nodes.len();
            self.nodes.push(Node::Internal(Internal {
                keys: vec![sep],
                children: vec![self.root, right],
            }));
            self.root = new_root;
            self.height += 1;
        }
        self.entries += 1;
    }

    fn insert_at(&mut self, node: NodeId, key: Key, value: Value) -> Option<Split> {
        let is_leaf = matches!(self.nodes[node], Node::Leaf(_));
        if is_leaf {
            return self.insert_leaf(node, key, value);
        }
        // Borrow of `self.nodes` ends before the recursive call.
        let child = match &self.nodes[node] {
            Node::Internal(i) => i.child_for(&key),
            Node::Leaf(_) => unreachable!(),
        };
        let split = self.insert_at(child, key, value)?;
        self.insert_separator(node, split.sep, split.right)
    }

    /// Reserve the future node id up front so the leaf borrow can live across
    /// payload mutation; nothing else pushes during this call.
    fn insert_leaf(&mut self, node: NodeId, key: Key, value: Value) -> Option<Split> {
        let right_id = self.nodes.len();
        let Node::Leaf(leaf) = &mut self.nodes[node] else {
            unreachable!("insert_leaf called on non-leaf")
        };
        // Global (key, value) pair order — duplicates stay sorted by value.
        let start = leaf.lower_bound(&key);
        let pos = start
            + leaf.entries[start..]
                .partition_point(|(k, v)| k.as_slice() == key.as_slice() && *v < value);
        leaf.entries.insert(pos, (key, value));
        if leaf.entries.len() <= LEAF_MAX {
            return None;
        }
        let mut mid = leaf.entries.len() / 2;
        // Never cut through a duplicate run: advance to the boundary after
        // the run containing `mid`. Right-routing (equal keys descend to the
        // right of the separator) depends on whole runs being owned by the
        // right sibling.
        while mid < leaf.entries.len() && mid > 0 && leaf.entries[mid - 1].0 == leaf.entries[mid].0
        {
            mid += 1;
        }
        if mid == leaf.entries.len() {
            // The whole leaf is one duplicate run. A raw cut here would park
            // the run's left half behind the separator, where equal-keys-right
            // routing makes it unreachable from `get_all`. Let the leaf
            // overfill instead: the run stays contiguous, `get_all`'s chain
            // walk keeps every copy visible, and ordering is undisturbed.
            return None;
        }
        let sep = leaf.entries[mid].0.clone();
        let right = Leaf {
            entries: leaf.entries.split_off(mid),
            next: leaf.next,
        };
        leaf.next = Some(right_id);
        self.nodes.push(Node::Leaf(right));
        Some(Split {
            sep,
            right: right_id,
        })
    }

    fn insert_separator(&mut self, node: NodeId, sep: Key, right_child: NodeId) -> Option<Split> {
        let right_id = self.nodes.len();
        let Node::Internal(internal) = &mut self.nodes[node] else {
            unreachable!("insert_separator called on non-internal")
        };
        let pos = internal
            .keys
            .partition_point(|k| k.as_slice() < sep.as_slice());
        internal.keys.insert(pos, sep);
        internal.children.insert(pos + 1, right_child);
        if internal.keys.len() <= INTERNAL_MAX_KEYS {
            return None;
        }
        let (promoted, right_node) = internal.split();
        self.nodes.push(Node::Internal(right_node));
        Some(Split {
            sep: promoted,
            right: right_id,
        })
    }

    /// Smallest value stored under `key`, if any.
    pub fn get(&self, key: &[u8]) -> Option<Value> {
        let mut node = self.root;
        loop {
            match &self.nodes[node] {
                Node::Internal(i) => node = i.child_for(key),
                Node::Leaf(l) => {
                    let pos = l.lower_bound(key);
                    return l
                        .entries
                        .get(pos)
                        .filter(|(k, _)| k == key)
                        .map(|(_, v)| *v);
                }
            }
        }
    }

    /// All values under `key`, ascending by value. Duplicate runs live in one
    /// leaf (split policy guarantees it); the `next`-link walk is a safety net
    /// for pathological single-key overflow leaves.
    pub fn get_all(&self, key: &[u8]) -> Vec<Value> {
        let mut out = Vec::new();
        let mut node = self.root;
        while let Node::Internal(i) = &self.nodes[node] {
            node = i.child_for(key);
        }
        loop {
            let Node::Leaf(leaf) = &self.nodes[node] else {
                unreachable!("descent stays within leaves")
            };
            for (k, v) in &leaf.entries[leaf.lower_bound(key)..] {
                if k.as_slice() != key {
                    return out;
                }
                out.push(*v);
            }
            let next_id = match leaf.next {
                Some(n) => n,
                None => return out,
            };
            let Node::Leaf(next_leaf) = &self.nodes[next_id] else {
                unreachable!("leaf chain stays within leaves")
            };
            match next_leaf.entries.first() {
                Some((k, _)) if k.as_slice() == key => node = next_id,
                _ => return out,
            }
        }
    }

    pub fn contains_key(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    /// Ascending scan of all `(key, value)` pairs with `key >= start`.
    pub fn scan_from(&self, start: &[u8]) -> ScanIter<'_> {
        let mut node = self.root;
        while let Node::Internal(i) = &self.nodes[node] {
            node = i.child_for(start);
        }
        if let Node::Leaf(l) = &self.nodes[node] {
            let pos = l.lower_bound(start);
            return ScanIter {
                tree: self,
                node,
                pos,
            };
        }
        unreachable!("descent always terminates at a leaf")
    }

    pub fn iter(&self) -> ScanIter<'_> {
        self.scan_from(&[])
    }
}

pub struct ScanIter<'a> {
    tree: &'a BTree,
    node: NodeId,
    pos: usize,
}

impl<'a> Iterator for ScanIter<'a> {
    type Item = (&'a Key, Value);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match &self.tree.nodes[self.node] {
                Node::Leaf(l) => {
                    if self.pos < l.entries.len() {
                        let item = &l.entries[self.pos];
                        self.pos += 1;
                        return Some((&item.0, item.1));
                    }
                    self.node = l.next?;
                    self.pos = 0;
                }
                Node::Internal(_) => unreachable!("iterator stays within leaves"),
            }
        }
    }
}

impl fmt::Debug for BTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BTree")
            .field("height", &self.height)
            .field("entries", &self.entries)
            .field("nodes", &self.nodes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn key(n: u64) -> [u8; 8] {
        n.to_be_bytes()
    }

    #[test]
    fn sequential_inserts_stay_sorted_and_findable() {
        let mut t = BTree::new();
        for i in 0..2000u64 {
            t.insert(&key(i), i * 10);
        }
        assert_eq!(t.len(), 2000);
        assert!(t.height() > 1, "2000 entries must force splits");
        for i in 0..2000u64 {
            assert_eq!(t.get(&key(i)), Some(i * 10));
        }
        assert_eq!(t.get(&key(5000)), None);

        let collected: Vec<u64> = t
            .iter()
            .map(|(k, _)| u64::from_be_bytes(k[..8].try_into().unwrap()))
            .collect();
        let mut sorted = collected.clone();
        sorted.sort_unstable();
        assert_eq!(collected, sorted, "scan must yield ascending order");
    }

    #[test]
    fn random_order_matches_baseline_map() {
        // xorshift64* — deterministic without external deps.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut rng = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545F4914F6CDD1D)
        };

        let mut tree = BTree::new();
        let mut baseline: BTreeMap<Key, Vec<Value>> = BTreeMap::new();
        for _ in 0..10_000 {
            let k = rng() % 3_000; // collisions exercise duplicate paths
            let v = rng();
            tree.insert(&k.to_be_bytes(), v);
            baseline
                .entry(k.to_be_bytes().to_vec())
                .or_default()
                .push(v);
        }
        let baseline_total: usize = baseline.values().map(Vec::len).sum();
        assert_eq!(tree.len(), baseline_total);

        // Duplicates are ordered by (key, value) — sort baselines to match.
        for (k, vals) in &mut baseline {
            vals.sort_unstable();
            assert_eq!(tree.get_all(k), *vals);
        }

        let tree_pairs: Vec<(Key, Value)> = tree.iter().map(|(k, v)| (k.clone(), v)).collect();
        let base_pairs: Vec<(Key, Value)> = baseline
            .iter()
            .flat_map(|(k, vs)| vs.iter().map(move |v| (k.clone(), *v)))
            .collect();
        assert_eq!(tree_pairs.len(), base_pairs.len(), "pair counts differ");
        for (i, (t, b)) in tree_pairs.iter().zip(&base_pairs).enumerate() {
            assert_eq!(
                t,
                b,
                "first divergence at pair #{i}: tree={t:?} base={b:?} | ctx tree={:?} base={:?}",
                &tree_pairs[i.saturating_sub(2)..(i + 3).min(tree_pairs.len())],
                &base_pairs[i.saturating_sub(2)..(i + 3).min(base_pairs.len())]
            );
        }
    }

    #[test]
    fn duplicate_keys_group_together_in_order() {
        let mut t = BTree::new();
        for v in [30u64, 10, 20] {
            t.insert(b"dup", v);
        }
        t.insert(b"zzz", 99);
        assert_eq!(t.get_all(b"dup"), vec![10, 20, 30]);
        assert_eq!(t.get(b"dup"), Some(10));
        assert_eq!(t.scan_from(b"dup").count(), 4);
    }

    #[test]
    fn scan_from_respects_lower_bound_across_leaf_boundaries() {
        let mut t = BTree::new();
        for i in 0..1000u64 {
            t.insert(&key(i), i);
        }
        let got: Vec<u64> = t
            .scan_from(&key(990))
            .map(|(k, _)| u64::from_be_bytes(k[..8].try_into().unwrap()))
            .collect();
        assert_eq!(got, (990..1000).collect::<Vec<_>>());
    }

    #[test]
    fn single_key_filling_a_leaf_splits_into_no_unreachable_run() {
        // Regression: > LEAF_MAX copies of one key used to drive the leaf-split
        // cursor past the end (panic) or strand the run's left half behind the
        // separator, where equal-keys-right routing hides it from get_all.
        let mut t = BTree::new();
        // 5x LEAF_MAX identical (key, value) pairs plus surrounding keys.
        let fill = (LEAF_MAX * 5) as u64;
        for i in 0..=fill {
            t.insert(b"k", i);
            if i % LEAF_MAX as u64 == 0 {
                t.insert(&key(i), i);
            }
        }
        assert_eq!(t.get_all(b"k")[0], 0, "run must start at the first copy");
        assert_eq!(
            t.get_all(b"k").len(),
            (fill + 1) as usize,
            "no copy may be lost"
        );
        for i in 0..=fill {
            assert_eq!(
                t.scan_from(b"k")
                    .filter(|(k, v)| k.as_slice() == b"k" && *v == i)
                    .count(),
                1,
                "copy {i} must appear exactly once in scan order"
            );
        }
        // Interleaved single-key and neighboring keys keep full order intact.
        let pairs: Vec<(Vec<u8>, u64)> = t.iter().map(|(k, v)| (k.clone(), v)).collect();
        assert!(pairs
            .windows(2)
            .all(|w| w[0].0 < w[1].0 || (w[0].0 == w[1].0 && w[0].1 <= w[1].1)));
        // The giant "k" run sorts after every zero-prefixed numeric key and
        // every one of its copies is present.
        assert_eq!(
            pairs.last().map(|(k, v)| (k.clone(), *v)),
            Some((b"k".to_vec(), fill))
        );
    }
}
