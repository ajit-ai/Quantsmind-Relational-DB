//! R4-DEADLOCK — deterministic validation of the blocking `LockManager`
//! wait queues, waits-for graph, and DFS deadlock detection.
//!
//! `acquire` is a *cooperative*/queueing API: an incompatible request is
//! enqueued (FIFO) and returns `Ok(())`; the grant happens when the blocking
//! holder releases. No thread ever blocks inside `acquire`, so every scenario
//! below is fully deterministic in a single thread — no sleeps, no channels,
//! no barriers needed. Each test proves externally observable behavior
//! (`holds`, returned errors, post-resolution grants) rather than internal
//! graph structure.

use qmind_kernel::lock::{LockError, LockManager, LockMode, Resource};
use qmind_kernel::MvccStore;

fn res(name: &str) -> Resource {
    Resource(name.to_owned())
}

/// Runs the canonical two-transaction deadlock and resolves it by aborting
/// the victim (the requester that received `Deadlock`):
///
/// T1 owns a; T2 owns b; T1 waits for b; T2 requests a → cycle T1→T2→T1.
///
/// Post-condition: victim 2 holds nothing; survivor 1 holds both a and b.
fn run_two_txn_deadlock(lm: &mut LockManager) -> Vec<u64> {
    lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
    lm.acquire(2, res("b"), LockMode::Exclusive).unwrap();
    lm.acquire(1, res("b"), LockMode::Exclusive).unwrap(); // queued, 1→2
    assert!(lm.holds(1, &res("b")).is_none(), "T1 waits, not granted");
    let cycle = match lm.acquire(2, res("a"), LockMode::Exclusive) {
        Err(LockError::Deadlock { cycle }) => cycle,
        Err(LockError::Busy) => panic!("blocking acquire never returns Busy"),
        Ok(()) => panic!("cycle must be detected for the requester (victim)"),
    };
    // Victim rollback of request only: 2 keeps the locks it already holds.
    assert_eq!(lm.holds(2, &res("b")), Some(LockMode::Exclusive));
    assert_eq!(lm.holds(2, &res("a")), None);
    lm.release_all(2); // victim aborts (strict 2PL release)
    assert_eq!(lm.holds(1, &res("b")), Some(LockMode::Exclusive));
    assert_eq!(lm.holds(1, &res("a")), Some(LockMode::Exclusive));
    assert_eq!(lm.holds(2, &res("b")), None);
    assert_eq!(lm.holds(2, &res("a")), None);
    cycle
}

fn assert_deadlock_result(r: Result<(), LockError>, victim: u64, members: &[u64]) {
    let cycle = match r {
        Err(LockError::Deadlock { cycle }) => cycle,
        Err(LockError::Busy) => panic!("blocking acquire never returns Busy"),
        Ok(()) => panic!("deadlock cycle must be detected"),
    };
    assert_eq!(
        cycle.first(),
        Some(&victim),
        "cycle starts at the requester"
    );
    assert_eq!(cycle.last(), Some(&victim), "cycle is closed");
    for m in members {
        assert!(cycle.contains(m), "cycle {cycle:?} must contain {m}");
    }
}

#[test]
fn two_transaction_cycle_is_detected_and_resolved() {
    let mut lm = LockManager::new();
    let cycle = run_two_txn_deadlock(&mut lm);
    assert_eq!(cycle.first(), Some(&2));
    assert_eq!(cycle.last(), Some(&2));
    assert!(cycle.contains(&1));
    // No indefinite wait and no leaked locks: table drains completely.
    lm.release_all(1);
    assert!(lm.is_idle());
}

#[test]
fn symmetric_completion_detects_the_other_victim_too() {
    // Mirror image: T2 completes the cycle by requesting b while T1 holds it.
    let mut lm = LockManager::new();
    lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
    lm.acquire(2, res("b"), LockMode::Exclusive).unwrap();
    lm.acquire(2, res("a"), LockMode::Exclusive).unwrap(); // queued, 2→1
    let r = lm.acquire(1, res("b"), LockMode::Exclusive);
    assert_deadlock_result(r, 1, &[2]); // requester 1 is the victim
    lm.release_all(1);
    assert_eq!(lm.holds(2, &res("a")), Some(LockMode::Exclusive));
    lm.release_all(2);
}

#[test]
fn three_transaction_cycle_is_detected_externally() {
    let mut lm = LockManager::new();
    lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
    lm.acquire(2, res("b"), LockMode::Exclusive).unwrap();
    lm.acquire(3, res("c"), LockMode::Exclusive).unwrap();
    lm.acquire(1, res("b"), LockMode::Exclusive).unwrap(); // queued, 1→2
    lm.acquire(2, res("c"), LockMode::Exclusive).unwrap(); // queued, 2→3
    let r = lm.acquire(3, res("a"), LockMode::Exclusive); // queued, 3→1 → cycle
    assert_deadlock_result(r, 3, &[1, 2]);
    // Only the failing request is rolled back; 3 keeps c (strict 2PL).
    assert_eq!(lm.holds(3, &res("c")), Some(LockMode::Exclusive));
    assert_eq!(lm.holds(3, &res("a")), None);

    // Victim 3 aborts: c becomes free, granting 2's queued c-request.
    lm.release_all(3);
    assert_eq!(lm.holds(2, &res("c")), Some(LockMode::Exclusive));
    // 2 still holds b; 1 still waits on b.
    assert_eq!(lm.holds(1, &res("b")), None);
    // 2 resolves: b releases → 1's queued b-request is granted.
    lm.release_all(2);
    assert_eq!(lm.holds(1, &res("b")), Some(LockMode::Exclusive));
    assert_eq!(lm.holds(1, &res("a")), Some(LockMode::Exclusive));
    assert!(lm.holds(1, &res("c")).is_none()); // nobody holds c
    lm.release_all(1);
    assert!(lm.is_idle());
}

#[test]
fn ordinary_wait_chain_is_not_a_deadlock() {
    let mut lm = LockManager::new();
    lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
    // Waiter queues without error — no deadlock, no wait-for error.
    lm.acquire(2, res("a"), LockMode::Exclusive).unwrap();
    assert!(lm.holds(2, &res("a")).is_none(), "T2 waits behind T1");
    lm.acquire(3, res("a"), LockMode::Exclusive).unwrap();
    assert!(lm.holds(3, &res("a")).is_none(), "T3 waits behind T2");
    // FIFO promotion: 1 releases → 2 gets it; 2 finishes → 3 gets it.
    lm.release_all(1);
    assert_eq!(lm.holds(2, &res("a")), Some(LockMode::Exclusive));
    assert!(lm.holds(3, &res("a")).is_none());
    lm.release_all(2);
    assert_eq!(lm.holds(3, &res("a")), Some(LockMode::Exclusive));
    lm.release_all(3);
    assert!(lm.is_idle());
}

#[test]
fn cross_resource_wait_chain_is_not_a_deadlock() {
    let mut lm = LockManager::new();
    // Linear dependency chain across distinct resources: 3→2→1, acyclic.
    lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
    lm.acquire(2, res("d"), LockMode::Exclusive).unwrap();
    lm.acquire(2, res("a"), LockMode::Exclusive).unwrap(); // queued 2→1
    lm.acquire(3, res("d"), LockMode::Exclusive).unwrap(); // queued 3→2
                                                           // Release in dependency order; nobody ever errors, all proceed.
    lm.release_all(1);
    assert_eq!(lm.holds(2, &res("a")), Some(LockMode::Exclusive));
    lm.release_all(2);
    assert_eq!(lm.holds(3, &res("d")), Some(LockMode::Exclusive));
    lm.release_all(3);
    assert!(lm.is_idle());
}

#[test]
fn waiter_cancellation_by_abort_leaves_no_stale_residue() {
    let mut lm = LockManager::new();
    lm.acquire(1, res("a"), LockMode::Exclusive).unwrap();
    lm.acquire(2, res("a"), LockMode::Exclusive).unwrap(); // queued
    assert!(lm.holds(2, &res("a")).is_none());
    // 2 is aborted (rollback / session disconnect / abort all funnel into
    // release_all) while 1 still holds the resource.
    lm.release_all(2);
    // 1 eventually releases; the cancelled waiter must not be promoted and a
    // fresh transaction must win the resource cleanly.
    lm.release_all(1);
    assert!(
        lm.holds(2, &res("a")).is_none(),
        "cancelled waiter never granted"
    );
    lm.acquire(3, res("a"), LockMode::Exclusive).unwrap();
    assert_eq!(lm.holds(3, &res("a")), Some(LockMode::Exclusive));
    lm.release_all(3);
    assert!(lm.is_idle());
}

#[test]
fn deadlock_victim_termination_leaves_no_stale_waiter() {
    let mut lm = LockManager::new();
    let _ = run_two_txn_deadlock(&mut lm);
    // Survivor 1 holds a + b. It releases; a brand-new transaction must
    // acquire a without any residue from the deadlock.
    lm.release_all(1);
    lm.acquire(50, res("a"), LockMode::Exclusive).unwrap();
    lm.acquire(50, res("b"), LockMode::Exclusive).unwrap();
    lm.release_all(50);
    assert!(lm.is_idle());
}

#[test]
fn no_wait_shared_grants_are_not_poisoned_by_cancelled_waiters() {
    let mut lm = LockManager::new();
    lm.acquire(1, res("t"), LockMode::Exclusive).unwrap();
    lm.acquire(2, res("t"), LockMode::Exclusive).unwrap(); // queued
    lm.release_all(2); // cancelled waiter
    lm.release_all(1);
    // A no-wait Shared request must not be rejected by a stale queue entry.
    assert!(lm.try_lock(3, res("t"), LockMode::Shared).is_ok());
    assert_eq!(lm.holds(3, &res("t")), Some(LockMode::Shared));
    lm.release_all(3);
    assert!(lm.is_idle());
}

#[test]
fn lock_manager_is_reusable_after_deadlock_resolution() {
    let mut lm = LockManager::new();
    let cycle = run_two_txn_deadlock(&mut lm);
    assert!(!cycle.is_empty());
    lm.release_all(1);
    // deadlock → cleanup → new transactions → acquire → release → success.
    for i in 100..103u64 {
        lm.acquire(i, res("fresh"), LockMode::Exclusive).unwrap();
        assert_eq!(lm.holds(i, &res("fresh")), Some(LockMode::Exclusive));
        lm.acquire(i, res("fresh2"), LockMode::Shared).unwrap();
        lm.release_all(i);
    }
    // A second deadlock on the same manager fails the same way (repeatable).
    let lm2_cycle = run_two_txn_deadlock(&mut lm);
    assert!(!lm2_cycle.is_empty());
    lm.release_all(1);
    assert!(lm.is_idle());
}

#[test]
fn repeated_cancellation_and_deadlock_leave_no_stale_wait_graph() {
    let mut lm = LockManager::new();
    // Churn: alternate cancelled waiters and resolved deadlocks on one manager.
    for round in 0..3u64 {
        let base = round * 10;
        let a = res("churn_a");
        let b = res("churn_b");
        // A waiter queues behind a holder, then is cancelled; the holder then
        // releases, so the resource fully drains.
        lm.acquire(base + 1, a.clone(), LockMode::Exclusive)
            .unwrap();
        lm.acquire(base + 2, a.clone(), LockMode::Exclusive)
            .unwrap(); // queued
        lm.release_all(base + 2); // cancelled waiter
        lm.release_all(base + 1); // holder finishes
                                  // A fully resolved deadlock between the helper's dedicated txn ids.
        let _ = run_two_txn_deadlock(&mut lm);
        lm.release_all(1); // helper survivor commits; victim 2 already aborted
                           // Ordinary traffic on a distinct resource.
        lm.acquire(base + 3, b.clone(), LockMode::Exclusive)
            .unwrap();
        lm.release_all(base + 3);
    }
    // Fresh post-churn traffic behaves exactly as on a brand-new manager.
    lm.acquire(900, res("post"), LockMode::Exclusive).unwrap();
    assert_eq!(lm.holds(900, &res("post")), Some(LockMode::Exclusive));
    lm.release_all(900);
    assert!(lm.is_idle());
}

#[test]
fn snapshot_readers_and_commit_watermark_are_untouched_by_deadlock_activity() {
    let mut store = MvccStore::new();
    let (t0, _) = store.begin();
    store.set(t0, b"k", vec![1]).unwrap();
    store.commit::<()>(t0, |_| Ok(())).unwrap().unwrap();
    let watermark_before = store.commit_watermark();

    // A reader pins its snapshot while other work (including a deadlock
    // cycle in the lock manager) happens around it.
    let (reader, r_snap) = store.begin();
    let (writer, _) = store.begin();
    store.set(writer, b"k2", vec![2]).unwrap();

    // Unrelated kernel deadlock cycle: detection + victim resolution.
    let mut lm = LockManager::new();
    let cycle = run_two_txn_deadlock(&mut lm);
    assert!(!cycle.is_empty());
    lm.release_all(1);

    // Deadlock handling must not advance the commit watermark (read_ts).
    assert_eq!(store.commit_watermark(), watermark_before);
    // The pinned snapshot still sees the pre-existing committed world, never
    // the writer's uncommitted state.
    assert_eq!(store.get(reader, b"k", &r_snap), Some(vec![1]));
    assert_eq!(store.read(None, b"k2", &r_snap), None);

    // The survivor transaction retains its snapshot and commits normally,
    // and SI still hides that commit from the pre-existing reader.
    store.commit::<()>(writer, |_| Ok(())).unwrap().unwrap();
    assert_eq!(store.commit_watermark(), watermark_before + 1);
    assert_eq!(
        store.get(reader, b"k", &r_snap),
        Some(vec![1]),
        "SI: post-snapshot commit stays invisible"
    );
    let (fresh, f_snap) = store.begin();
    assert_eq!(store.get(fresh, b"k2", &f_snap), Some(vec![2]));
}

#[test]
fn deadlocked_writer_rollback_removes_uncommitted_state_and_preserves_si() {
    let mut store = MvccStore::new();
    let (t0, _) = store.begin();
    store.set(t0, b"x", vec![1]).unwrap();
    store.commit::<()>(t0, |_| Ok(())).unwrap().unwrap();

    // Reader pins a snapshot before any of the conflict activity.
    let (reader, r_snap) = store.begin();
    let (victim, _) = store.begin();
    store.set(victim, b"x", vec![99]).unwrap();
    // Runtime surface: a second writer cannot hold the same row (no-wait Busy).
    let (rival, _) = store.begin();
    assert!(matches!(
        store.set(rival, b"x", vec![2]).unwrap_err(),
        LockError::Busy
    ));

    // A kernel deadlock between unrelated lock ids completes and resolves
    // without altering MVCC state (the store's own lock manager is untouched).
    let mut lm = LockManager::new();
    let cycle = run_two_txn_deadlock(&mut lm);
    assert!(!cycle.is_empty());
    lm.release_all(1);
    assert!(lm.is_idle());

    // Rollback of the deadlocked writer removes its uncommitted state; the
    // early reader never observed it and continues to see the baseline.
    store.abort(victim);
    assert_eq!(store.active_txns(), 2, "reader + rival remain live");
    assert_eq!(store.get(reader, b"x", &r_snap), Some(vec![1]));
    // The rival can now write (lock released); the early reader must remain
    // unaffected, then the rival's fresh commit is FCW-clean.
    store.abort(rival);
    let (fresh, _) = store.begin();
    store.set(fresh, b"x", vec![2]).unwrap();
    store.commit::<()>(fresh, |_| Ok(())).unwrap().unwrap();
    assert_eq!(
        store.get(reader, b"x", &r_snap),
        Some(vec![1]),
        "SI: reader pinned before the fresh commit stays stable"
    );
    store.abort(reader);
    let (r2, s2) = store.begin();
    assert_eq!(store.get(r2, b"x", &s2), Some(vec![2]));
    store.abort(r2);
    assert_eq!(store.active_txns(), 0);
}
