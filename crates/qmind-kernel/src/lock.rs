//! E3 — Strict 2PL lock manager.
//!
//! S/X locks keyed by arbitrary string resources (table names, row keys).
//! Conflicting requests queue FIFO; waits-for graph tracks edges from
//! waiters to holders; a DFS cycle check runs on every edge addition and
//! rejects the request with `Deadlock` (victim = the requester, per
//! no-wait prevention). `release_all` implements strict-2PL semantics:
//! a txn holds every lock until commit/abort.

use std::collections::{BTreeSet, HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Resource(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

impl LockMode {
    fn compatible(a: LockMode, b: LockMode) -> bool {
        matches!((a, b), (LockMode::Shared, LockMode::Shared))
    }
}

#[derive(Debug)]
pub enum LockError {
    Deadlock { cycle: Vec<u64> },
}

#[derive(Debug, Default)]
struct LockEntry {
    /// txn → mode currently granted.
    holders: HashMap<u64, LockMode>,
    /// FIFO of (txn, mode) waiting to be granted.
    queue: VecDeque<(u64, LockMode)>,
}

#[derive(Debug, Default)]
pub struct LockManager {
    locks: HashMap<Resource, LockEntry>,
    /// waiter → set of txns it waits for (holders blocking it).
    waits_for: HashMap<u64, BTreeSet<u64>>,
}

impl LockManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire `mode` on `res` for `txn`. Grants immediately when compatible;
    /// otherwise queues and runs deadlock detection. On deadlock the request
    /// is removed from the queue and the cycle reported.
    pub fn acquire(&mut self, txn: u64, res: Resource, mode: LockMode) -> Result<(), LockError> {
        // Re-entrant same-or-weaker hold: upgrade path handled below.
        let entry = self.locks.entry(res.clone()).or_default();

        if let Some(&held) = entry.holders.get(&txn) {
            if held == mode || held == LockMode::Exclusive {
                return Ok(()); // already sufficient
            }
            // S -> X upgrade by sole holder is a direct grant
            if entry.holders.len() == 1 {
                entry.holders.insert(txn, mode);
                return Ok(());
            }
        }

        let compatible = entry.holders.is_empty()
            || (mode == LockMode::Shared
                && entry.holders.values().all(|&m| m == LockMode::Shared)
                && entry.queue.is_empty());

        if compatible {
            entry.holders.insert(txn, mode);
            return Ok(());
        }

        // Must wait: enqueue + link waits-for edges (excluding self —
        // holding a weaker lock on the same resource is not self-blocking).
        entry.queue.push_back((txn, mode));
        let holders: BTreeSet<u64> = entry
            .holders
            .keys()
            .copied()
            .filter(|t| *t != txn)
            .collect();
        let waiters = self.waits_for.entry(txn).or_default();
        for h in &holders {
            waiters.insert(*h);
        }

        if let Some(cycle) = self.find_cycle(txn) {
            // rollback this request
            if let Some(e) = self.locks.get_mut(&res) {
                if let Some(pos) = e.queue.iter().rposition(|(t, _)| *t == txn) {
                    e.queue.remove(pos);
                }
            }
            self.waits_for.remove(&txn);
            for h in &holders {
                if let Some(ws) = self.waits_for.get_mut(h) {
                    ws.remove(&txn);
                }
            }
            return Err(LockError::Deadlock { cycle });
        }
        Ok(())
    }

    /// DFS from `start` following waits_for; returns the cycle containing
    /// `start` if one exists.
    fn find_cycle(&self, start: u64) -> Option<Vec<u64>> {
        let mut path = vec![start];
        let mut on_path: BTreeSet<u64> = BTreeSet::new();
        on_path.insert(start);
        fn dfs(
            node: u64,
            start: u64,
            g: &HashMap<u64, BTreeSet<u64>>,
            path: &mut Vec<u64>,
            on_path: &mut BTreeSet<u64>,
        ) -> Option<Vec<u64>> {
            if let Some(nexts) = g.get(&node) {
                for n in nexts {
                    if *n == start {
                        path.push(*n);
                        return Some(path.clone());
                    }
                    if !on_path.contains(n) {
                        on_path.insert(*n);
                        path.push(*n);
                        if let Some(c) = dfs(*n, start, g, path, on_path) {
                            return Some(c);
                        }
                        path.pop();
                        on_path.remove(n);
                    }
                }
            }
            None
        }
        dfs(start, start, &self.waits_for, &mut path, &mut on_path)
    }

    /// Release one resource held by `txn`; promotes queued requests that are
    /// now grantable (FIFO, group grant for compatible Shared runs).
    pub fn release(&mut self, txn: u64, res: &Resource) {
        let Some(entry) = self.locks.get_mut(res) else {
            return;
        };
        entry.holders.remove(&txn);
        if let Some(ws) = self.waits_for.get_mut(&txn) {
            ws.clear();
        }
        // promote from queue while head is grantable; the waiter's own
        // current hold (e.g. S in an S→X upgrade) must not block itself
        loop {
            let Some(&(wt, wm)) = entry.queue.front() else {
                break;
            };
            let remaining = entry
                .holders
                .iter()
                .filter(|(&t, _)| t != wt)
                .map(|(_, &m)| m)
                .collect::<Vec<_>>();
            let ok = remaining.is_empty()
                || (wm == LockMode::Shared && remaining.iter().all(|&m| m == LockMode::Shared));
            if !ok {
                break;
            }
            entry.queue.pop_front();
            entry.holders.insert(wt, wm);
            if let Some(ws) = self.waits_for.get_mut(&wt) {
                ws.clear();
            }
        }
        if entry.holders.is_empty() && entry.queue.is_empty() {
            self.locks.remove(res);
        }
    }

    /// Strict 2PL: commit/abort releases everything the txn holds.
    pub fn release_all(&mut self, txn: u64) {
        let resources: Vec<Resource> = self.locks.keys().cloned().collect();
        for r in resources {
            self.release(txn, &r);
        }
        self.waits_for.remove(&txn);
    }

    pub fn holds(&self, txn: u64, res: &Resource) -> Option<LockMode> {
        self.locks
            .get(res)
            .and_then(|e| e.holders.get(&txn))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(name: &str) -> Resource {
        Resource(name.into())
    }

    #[test]
    fn shared_locks_coexist() {
        let mut lm = LockManager::new();
        lm.acquire(1, res("t"), LockMode::Shared).unwrap();
        lm.acquire(2, res("t"), LockMode::Shared).unwrap();
        assert_eq!(lm.holds(1, &res("t")), Some(LockMode::Shared));
        assert_eq!(lm.holds(2, &res("t")), Some(LockMode::Shared));
    }

    #[test]
    fn exclusive_blocks_shared_until_release() {
        let mut lm = LockManager::new();
        lm.acquire(1, res("t"), LockMode::Exclusive).unwrap();
        lm.acquire(2, res("t"), LockMode::Shared).unwrap(); // queued
        assert!(lm.holds(2, &res("t")).is_none(), "must wait");
        lm.release(1, &res("t"));
        assert_eq!(lm.holds(2, &res("t")), Some(LockMode::Shared), "promoted");
    }

    #[test]
    fn exclusive_excludes_exclusive() {
        let mut lm = LockManager::new();
        lm.acquire(1, res("t"), LockMode::Exclusive).unwrap();
        lm.acquire(2, res("t"), LockMode::Exclusive).unwrap(); // queued
        assert!(lm.holds(2, &res("t")).is_none());
    }

    #[test]
    fn deadlock_detected_and_reported_with_cycle() {
        let mut lm = LockManager::new();
        lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
        lm.acquire(2, res("b"), LockMode::Exclusive).unwrap();
        lm.acquire(1, res("b"), LockMode::Exclusive).unwrap(); // 1 waits on 2
        let err = lm.acquire(2, res("a"), LockMode::Exclusive).unwrap_err();
        match err {
            LockError::Deadlock { cycle } => {
                assert_eq!(cycle.first(), Some(&2));
                assert_eq!(cycle.last(), Some(&2));
                assert!(cycle.contains(&1));
            }
        }
        // requester's request rolled back — 2 still holds only b
        assert_eq!(lm.holds(2, &res("b")), Some(LockMode::Exclusive));
        assert!(lm.holds(2, &res("a")).is_none());
    }

    #[test]
    fn release_all_grants_queued_strict_2pl() {
        let mut lm = LockManager::new();
        lm.acquire(1, res("x"), LockMode::Exclusive).unwrap();
        lm.acquire(1, res("y"), LockMode::Exclusive).unwrap();
        lm.acquire(2, res("x"), LockMode::Exclusive).unwrap(); // queued
        lm.release_all(1); // commit/abort
        assert_eq!(lm.holds(2, &res("x")), Some(LockMode::Exclusive));
        assert!(lm.holds(1, &res("y")).is_none());
    }

    #[test]
    fn sole_holder_upgrades_shared_to_exclusive() {
        let mut lm = LockManager::new();
        lm.acquire(1, res("t"), LockMode::Shared).unwrap();
        lm.acquire(1, res("t"), LockMode::Exclusive).unwrap();
        assert_eq!(lm.holds(1, &res("t")), Some(LockMode::Exclusive));
    }

    #[test]
    fn shared_upgrade_with_other_holder_deadlocks_or_waits() {
        let mut lm = LockManager::new();
        lm.acquire(1, res("t"), LockMode::Shared).unwrap();
        lm.acquire(2, res("t"), LockMode::Shared).unwrap();
        // 1 requests X: must wait (2 still holds S) — no grant, no panic
        lm.acquire(1, res("t"), LockMode::Exclusive).unwrap();
        assert_ne!(lm.holds(1, &res("t")), Some(LockMode::Exclusive));
        lm.release(2, &res("t"));
        assert_eq!(
            lm.holds(1, &res("t")),
            Some(LockMode::Exclusive),
            "promoted after release"
        );
    }
}
