//! E3 — Strict 2PL lock manager.
//!
//! S/X locks keyed by arbitrary string resources (table names, row keys).
//! Conflicting requests queue FIFO; waits-for graph tracks edges from
//! waiters to holders; a DFS cycle check runs on every edge addition and
//! rejects the request with `Deadlock` (victim = the requester, per
//! no-wait prevention). `release_all` implements strict-2PL semantics:
//! a txn holds every lock until commit/abort, and cancellation is total —
//! queued-but-ungranted requests of a terminated txn are purged and the txn
//! is removed from every other waiter's wait set, so no stale edge or stale
//! grant can ever survive a termination.

use std::collections::{BTreeSet, HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Resource(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Debug)]
pub enum LockError {
    Deadlock {
        cycle: Vec<u64>,
    },
    /// A no-wait request was rejected because another transaction holds the
    /// resource in an incompatible mode. Nothing was enqueued; the caller may
    /// retry later (R4-LOCK deterministic, non-blocking conflict).
    Busy,
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
    /// otherwise queues the request (FIFO) and runs deadlock detection. Returns
    /// ``Ok(())`` once the request is granted *or* safely enqueued — a queued
    /// request is granted when the blocking holder releases. On deadlock the
    /// request is removed from the queue, the cycle is reported, and the caller's
    /// existing locks are left untouched (the victim resolves by aborting via
    /// [`LockManager::release_all`]).
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

    /// Deterministic no-wait acquisition for the SQL/MVCC write path.
    ///
    /// Grants immediately when the request is compatible with the current
    /// holders (mirroring `acquire`'s grant rule) and otherwise returns
    /// [`LockError::Busy`] without touching the FIFO queue or the waits-for
    /// graph — a no-wait request can never wait, so it can never create a
    /// deadlock cycle. This is how the kernel surfaces a write-write conflict
    /// at `set` time instead of blocking a single-threaded runtime. Re-entrant
    /// requests (same or stronger mode already held) succeed; upgrades are
    /// granted to a sole holder like `acquire`.
    pub fn try_lock(&mut self, txn: u64, res: Resource, mode: LockMode) -> Result<(), LockError> {
        let entry = self.locks.entry(res).or_default();

        if let Some(&held) = entry.holders.get(&txn) {
            if held == mode || held == LockMode::Exclusive {
                return Ok(()); // already sufficient
            }
            // S -> X upgrade by sole holder is a direct grant.
            if entry.holders.len() == 1 {
                entry.holders.insert(txn, mode);
                return Ok(());
            }
            return Err(LockError::Busy);
        }

        let compatible = entry.holders.is_empty()
            || (mode == LockMode::Shared
                && entry.holders.values().all(|&m| m == LockMode::Shared)
                && entry.queue.is_empty());
        if compatible {
            entry.holders.insert(txn, mode);
            return Ok(());
        }
        Err(LockError::Busy)
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

    /// Release one resource held (or queued on) by `txn`; purges any of
    /// `txn`'s queued-but-ungranted requests on the resource, then promotes
    /// queued requests that are now grantable (FIFO, group grant for
    /// compatible Shared runs).
    pub fn release(&mut self, txn: u64, res: &Resource) {
        let Some(entry) = self.locks.get_mut(res) else {
            return;
        };
        entry.holders.remove(&txn);
        // A release is `txn` going away from this resource: drop its own
        // queued-but-ungranted requests too, so an aborted/committed waiter
        // can never be promoted later (which would leak the lock) and never
        // pollutes the queue for future grant decisions.
        if !entry.queue.is_empty() {
            entry.queue.retain(|(t, _)| *t != txn);
        }
        if let Some(ws) = self.waits_for.get_mut(&txn) {
            ws.clear();
        }
        // promote from queue while head is grantable; the waiter's own
        // current hold (e.g. S in an S→X upgrade) must not block itself
        while let Some(&(wt, wm)) = entry.queue.front() {
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
            // Remove precisely this resource's blockers from the promoted
            // waiter's edge set: the releaser `txn` plus every holder that
            // blocked it while queued. Dependencies a multi-resource waiter
            // still owes to *other* blocked resources stay intact, so future
            // cycle detection remains sound.
            if let Some(ws) = self.waits_for.get_mut(&wt) {
                ws.remove(&txn);
                for &h in entry.holders.keys() {
                    if h != wt {
                        ws.remove(&h);
                    }
                }
            }
        }
        if entry.holders.is_empty() && entry.queue.is_empty() {
            self.locks.remove(res);
        }
    }

    /// Strict 2PL: commit/abort releases everything the txn holds and drops
    /// every queued request it never received — the txn is completely removed
    /// from the lock table and from every other txn's wait set.
    pub fn release_all(&mut self, txn: u64) {
        let resources: Vec<Resource> = self.locks.keys().cloned().collect();
        for r in resources {
            self.release(txn, &r);
        }
        self.waits_for.remove(&txn);
        // A terminated txn must not linger in anyone's wait set: sweep every
        // incoming edge so a later cycle search can never route through a dead
        // transaction, and cancellation leaves no dependency residue.
        for ws in self.waits_for.values_mut() {
            ws.remove(&txn);
        }
    }

    pub fn holds(&self, txn: u64, res: &Resource) -> Option<LockMode> {
        self.locks
            .get(res)
            .and_then(|e| e.holders.get(&txn))
            .copied()
    }

    /// True when the manager is fully drained: no locks, no queued waiters,
    /// no waits-for edges. The strict-2PL endpoint after every transaction
    /// has committed or aborted.
    pub fn is_idle(&self) -> bool {
        self.locks.is_empty() && self.waits_for.is_empty()
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
            LockError::Busy => panic!("blocking acquire must not return Busy"),
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

    #[test]
    fn try_lock_grants_frees_and_rejects_conflicts_without_queueing() {
        let mut lm = LockManager::new();
        lm.try_lock(1, res("t"), LockMode::Exclusive).unwrap();
        // Incompatible requests fail immediately — no FIFO entry, no wait.
        assert!(matches!(
            lm.try_lock(2, res("t"), LockMode::Exclusive).unwrap_err(),
            LockError::Busy
        ));
        assert!(matches!(
            lm.try_lock(3, res("t"), LockMode::Shared).unwrap_err(),
            LockError::Busy
        ));
        assert!(
            lm.holds(2, &res("t")).is_none(),
            "nothing granted to waiter"
        );
        // Holder releases (strict 2PL commit/abort) — the no-wait requester
        // can now proceed without a stale queue entry getting in the way.
        lm.release_all(1);
        lm.try_lock(2, res("t"), LockMode::Exclusive).unwrap();
        assert_eq!(lm.holds(2, &res("t")), Some(LockMode::Exclusive));
    }

    #[test]
    fn try_lock_shared_coexists_and_same_txn_reentry_succeeds() {
        let mut lm = LockManager::new();
        lm.try_lock(1, res("t"), LockMode::Shared).unwrap();
        lm.try_lock(2, res("t"), LockMode::Shared).unwrap();
        // Re-entrant requests are satisfied by the existing hold.
        lm.try_lock(1, res("t"), LockMode::Shared).unwrap();
        lm.try_lock(2, res("t"), LockMode::Shared).unwrap();
        assert_eq!(lm.holds(1, &res("t")), Some(LockMode::Shared));
        assert_eq!(lm.holds(2, &res("t")), Some(LockMode::Shared));
    }

    #[test]
    fn try_lock_sole_holder_upgrades_shared_to_exclusive() {
        let mut lm = LockManager::new();
        lm.try_lock(1, res("t"), LockMode::Shared).unwrap();
        lm.try_lock(1, res("t"), LockMode::Exclusive).unwrap();
        assert_eq!(lm.holds(1, &res("t")), Some(LockMode::Exclusive));
    }

    #[test]
    fn aborted_waiter_is_purged_and_cannot_leak_a_grant() {
        let mut lm = LockManager::new();
        lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
        lm.acquire(2, res("a"), LockMode::Exclusive).unwrap(); // queued behind 1
        assert!(lm.holds(2, &res("a")).is_none(), "waiter stays queued");
        // 2 aborts while still waiting — its queued request must be removed.
        lm.release_all(2);
        assert!(lm.holds(2, &res("a")).is_none());
        // 1 commits; the aborted waiter must not be promoted (that would leak 'a').
        lm.release(1, &res("a"));
        assert!(lm.holds(1, &res("a")).is_none());
        assert!(
            lm.holds(2, &res("a")).is_none(),
            "aborted waiter must never be granted"
        );
        // A fresh transaction acquires 'a' cleanly.
        lm.acquire(3, res("a"), LockMode::Exclusive).unwrap();
        assert_eq!(lm.holds(3, &res("a")), Some(LockMode::Exclusive));
        lm.release_all(3);
        assert!(lm.locks.is_empty(), "lock table fully drained");
        assert!(lm.waits_for.is_empty(), "wait graph fully drained");
    }

    #[test]
    fn terminated_txn_is_swept_from_others_wait_sets() {
        let mut lm = LockManager::new();
        // Shared holders 1 and 6 jointly block an exclusive request from 4.
        lm.acquire(1, res("c"), LockMode::Shared).unwrap();
        lm.acquire(6, res("c"), LockMode::Shared).unwrap();
        lm.acquire(4, res("c"), LockMode::Exclusive).unwrap(); // queued
        assert!(lm.waits_for[&4].contains(&1));
        assert!(lm.waits_for[&4].contains(&6));
        // 1 terminates; 4 is still blocked by 6, so the edge to the gone txn
        // is swept while the live dependency stays.
        lm.release_all(1);
        assert!(
            !lm.waits_for[&4].contains(&1),
            "stale edge to terminated txn must be swept"
        );
        assert!(lm.waits_for[&4].contains(&6), "live edge preserved");
        // Resolve the remainder so the wait graph drains cleanly.
        lm.release(6, &res("c"));
        assert_eq!(lm.holds(4, &res("c")), Some(LockMode::Exclusive));
        assert!(lm.waits_for[&4].is_empty(), "granted waiter loses edges");
        lm.release_all(4);
        assert!(lm.waits_for.is_empty());
        assert!(lm.locks.is_empty());
    }

    #[test]
    fn multi_resource_waiter_keeps_other_edges_on_promotion() {
        // 1 holds "a"; 2 holds "b". W queues on both. When 1 releases, W is
        // granted "a" but must keep its dependency on 2 for "b", so a later
        // cycle through W stays visible to detection.
        let mut lm = LockManager::new();
        lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
        lm.acquire(2, res("b"), LockMode::Exclusive).unwrap();
        let w = 7;
        lm.acquire(w, res("a"), LockMode::Exclusive).unwrap(); // queued
        lm.acquire(w, res("b"), LockMode::Exclusive).unwrap(); // queued
        assert!(lm.waits_for[&w].contains(&1));
        assert!(lm.waits_for[&w].contains(&2));
        lm.release(1, &res("a"));
        assert_eq!(
            lm.holds(w, &res("a")),
            Some(LockMode::Exclusive),
            "granted a"
        );
        assert!(lm.holds(w, &res("b")).is_none(), "still waiting on b");
        assert!(!lm.waits_for[&w].contains(&1), "a-dependency cleared");
        assert!(lm.waits_for[&w].contains(&2), "b-dependency retained");
        lm.release_all(w);
        lm.release_all(2);
        assert!(lm.locks.is_empty());
        assert!(lm.waits_for.is_empty());
    }
}
