//! R4-CONCURRENCY / R4-LOCK — deterministic concurrency-semantics proofs at
//! the kernel level.
//!
//! `MvccStore` hosts several live transactions in one store, so the concurrency
//! rules that the single-writer SQL layer can only enforce (not exercise) are
//! proven here with exact interleavings — no sleeps, no timing. These tests
//! pin the contract the engine relies on:
//!
//! 1. Two readers coexist without blocking each other; each holds its own
//!    immutable snapshot while a third transaction commits.
//! 2. A losing writer is rejected deterministically at `set` (strict-2PL no-
//!    wait Busy) or at commit (first-committer-wins over a stale snapshot),
//!    leaves no partial state behind, aborts cleanly, and a fresh writer can
//!    take over.
//! 3. A WAL-sink failure mid-commit discards everything, releases the txn id
//!    and its locks, and the store remains fully usable for the next
//!    transaction.

use qmind_kernel::lock::LockError;
use qmind_kernel::wal::WalRecord;
use qmind_kernel::{Conflict, MvccStore};

fn noop(_: &[WalRecord]) -> Result<(), ()> {
    Ok(())
}

/// Requirement 1 + 2 + 6: two concurrent readers, a committing writer, and a
/// fresh transaction that begins after the commit.
///
/// Sequence: committed baseline `a`; T1 and T2 read it; W writes `b` and
/// commits; T1 and T2 re-read and must still see only `a`; a new T3 must see
/// both `a` and `b`.
#[test]
fn two_concurrent_readers_keep_stable_snapshots_while_writer_commits() {
    let mut s = MvccStore::new();

    let (baseline, _) = s.begin();
    s.set(baseline, b"a", vec![1]).unwrap();
    s.commit(baseline, noop).unwrap().unwrap();

    // Both readers snapshot the committed world {a}.
    let (r1, snap1) = s.begin();
    let (r2, snap2) = s.begin();
    assert_eq!(s.read(Some(r1), b"a", &snap1), Some(vec![1]));
    assert_eq!(s.read(Some(r2), b"a", &snap2), Some(vec![1]));

    // Writer commits a new key after both snapshots were taken.
    let (w, _) = s.begin();
    s.set(w, b"b", vec![2]).unwrap();
    s.commit(w, noop).unwrap().unwrap();
    assert_eq!(s.commit_watermark(), 2);

    // Readers must not observe the post-snapshot commit under SI.
    assert_eq!(s.read(Some(r1), b"a", &snap1), Some(vec![1]));
    assert_eq!(s.read(Some(r2), b"a", &snap2), Some(vec![1]));
    assert_eq!(s.read(Some(r1), b"b", &snap1), None);
    assert_eq!(s.read(Some(r2), b"b", &snap2), None);

    // Both open readers finish without interfering with each other.
    s.abort(r1);
    s.abort(r2);

    // A transaction begun after the commit sees the new row.
    let (r3, snap3) = s.begin();
    assert_eq!(s.read(Some(r3), b"a", &snap3), Some(vec![1]));
    assert_eq!(s.read(Some(r3), b"b", &snap3), Some(vec![2]));
    assert_eq!(s.active_txns(), 1);
}

/// Requirement 3 + 4 + 5: a conflicting writer is rejected deterministically —
/// either by the strict-2PL no-wait lock at `set` or by first-committer-wins at
/// commit over a stale snapshot — releases ownership via abort, and a fresh
/// transaction can write the same key successfully; the store is never wedged
/// by the loser.
#[test]
fn conflicting_writer_aborts_and_a_fresh_txn_retries_cleanly() {
    let mut s = MvccStore::new();

    // All three writers begin against snapshot ts=0.
    let (t1, _) = s.begin();
    let (t2, _) = s.begin();
    let (t2b, _) = s.begin();

    // t2 shares the snapshot but cannot hold the key while t1 owns it:
    // strict-2PL surfaces the conflict deterministically at `set`.
    s.set(t1, b"x", vec![10]).unwrap();
    assert!(matches!(
        s.set(t2, b"x", vec![20]).unwrap_err(),
        LockError::Busy
    ));
    s.abort(t2);
    s.commit(t1, noop).unwrap().unwrap();
    assert_eq!(s.active_txns(), 1); // only t2b (the stale snapshot) remains

    // t2b's snapshot predates t1's commit, so even though the lock was
    // strictly released at commit, first-committer-wins still rejects it.
    s.set(t2b, b"x", vec![20]).unwrap();
    let res = s.commit(t2b, noop).unwrap();
    assert_eq!(res, Err(Conflict { key: b"x".to_vec() }));

    // The loser published nothing.
    assert_eq!(s.commit_watermark(), 1);
    {
        let snap = s.snapshot();
        assert_eq!(s.get_raw(b"x", &snap), Some(vec![10]));
    }
    s.abort(t2b);
    assert_eq!(s.active_txns(), 0);

    // A brand-new writer takes over the key and commits normally.
    let (t3, _) = s.begin();
    s.set(t3, b"x", vec![30]).unwrap();
    s.commit(t3, noop).unwrap().unwrap();

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"x", &snap), Some(vec![30]));
}

/// Requirement 4 + 5: a WAL-sink failure surfaces as an error, publishes
/// nothing, and the aborted attempt does not block later transactions.
#[test]
fn aborted_after_wal_failure_leaves_no_state_and_store_stays_usable() {
    let mut s = MvccStore::new();

    let (t1, _) = s.begin();
    s.set(t1, b"z", vec![9]).unwrap();
    // Sink failure: the commit errors out before publishing and reaps the
    // txn (pending id + locks released) immediately.
    let res: Result<Result<(), Conflict>, String> =
        s.commit(t1, |_| Err(String::from("disk full")));
    assert!(res.is_err());

    // The failed commit cleaned up after itself; abort is an idempotent no-op.
    assert_eq!(s.active_txns(), 0);
    s.abort(t1);
    assert_eq!(s.active_txns(), 0);

    // Nothing leaked.
    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"z", &snap), None);

    // Next transaction works normally (fresh key, clean WAL sink).
    let (t2, _) = s.begin();
    s.set(t2, b"z", vec![5]).unwrap();
    s.commit(t2, noop).unwrap().unwrap();

    let (r2, snap2) = s.begin();
    assert_eq!(s.read(Some(r2), b"z", &snap2), Some(vec![5]));
}

/// Requirement 8 (aggregate / GROUP BY building block): the SQL execution
/// paths pair with these rules because `COUNT`, `GROUP BY` and `JOIN` all read
/// through the same prefix scan / snapshot reads. A stable scan of an open
/// transaction is what keeps per-row counts and groups snapshot-stable.
#[test]
fn snapshot_scans_stay_stable_across_concurrent_writer_commits() {
    let mut s = MvccStore::new();

    // Committed baseline: a 3-row table.
    for i in 1..=3u8 {
        let (t, _) = s.begin();
        s.set(t, &[b't', i], vec![i]).unwrap();
        s.commit(t, noop).unwrap().unwrap();
    }

    // T1 snapshots the 3-row committed table.
    let (r1, snap1) = s.begin();
    assert_eq!(s.scan_prefix(b"t", &snap1).len(), 3);

    // A concurrent writer commits three more rows after T1's snapshot.
    for i in 4..=6u8 {
        let (t, _) = s.begin();
        s.set(t, &[b't', i], vec![i]).unwrap();
        s.commit(t, noop).unwrap().unwrap();
    }

    // T1's scan (hence its COUNT/GROUP BY/JOIN input) is unchanged.
    let mut keys: Vec<Vec<u8>> = s
        .scan_prefix(b"t", &snap1)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    keys.sort();
    let expected: Vec<Vec<u8>> = (1..=3u8).map(|i| vec![b't', i]).collect();
    assert_eq!(keys, expected, "open-reader scan must be snapshot-stable");

    // A transaction begun after the commit scans the full committed set.
    let (r2, snap2) = s.begin();
    assert_eq!(s.scan_prefix(b"t", &snap2).len(), 6);

    s.abort(r1);
    s.abort(r2);
}

/// R4-MULTIWRITER — row-local conflict granularity and fail-fast delivery.
///
/// Two live writer transactions hold strict-2PL locks on *rows* of the same
/// logical table, not the table itself: a third writer can take an untouched
/// row concurrently (row-local isolation), while a write to either held row is
/// rejected deterministically with no-wait `Busy` — never blocked, never
/// queued, never deadlocked. After the winner commits, a stale-snapshot writer
/// is still rejected by first-committer-wins at commit even though the lock has
/// been released. This is the exact conflict contract the SQL engine maps to
/// `statement failed: row locked by another transaction`.
#[test]
fn row_local_conflicts_fail_fast_without_blocking_concurrent_rows() {
    let mut s = MvccStore::new();

    // All writers begin against snapshot ts=0 (stale relative to the commits
    // below), exactly like live concurrent SQL sessions.
    let (w1, _) = s.begin();
    let (w2, _) = s.begin();
    let (w3, _) = s.begin();
    let (w4, _) = s.begin();
    let (w5, _) = s.begin();

    // Writer W1 locks rows `t/1` and `t/3` of "table" t.
    s.set(w1, b"t/1", vec![1]).unwrap();
    s.set(w1, b"t/3", vec![3]).unwrap();

    // Row-local isolation: W2 writes row `t/2` (untouched) concurrently,
    // despite W1 holding two other rows of the same table.
    s.set(w2, b"t/2", vec![2]).unwrap();

    // Fail-fast no-wait: W3 targets W1's held row `t/3` and is rejected
    // immediately with Busy (no queueing, no deadlock).
    assert!(matches!(
        s.set(w3, b"t/3", vec![30]).unwrap_err(),
        LockError::Busy
    ));
    s.abort(w3);
    // The same for W1's other held row.
    assert!(matches!(
        s.set(w4, b"t/1", vec![10]).unwrap_err(),
        LockError::Busy
    ));
    s.abort(w4);

    // W2 (its own row) commits first; W1 commits its two rows; all three rows
    // are present and every lock was released (strict 2PL).
    s.commit(w2, noop).unwrap().unwrap();
    s.commit(w1, noop).unwrap().unwrap();
    assert_eq!(s.active_txns(), 1); // only the stale-snapshot w5 remains
    let snap = s.snapshot();
    assert_eq!(s.get_raw(b"t/1", &snap), Some(vec![1]));
    assert_eq!(s.get_raw(b"t/2", &snap), Some(vec![2]));
    assert_eq!(s.get_raw(b"t/3", &snap), Some(vec![3]));

    // A stale-snapshot writer (w5, begun before W1's commit) takes the now-free
    // row, but first-committer-wins rejects its commit: the lock being free
    // after commit is not enough.
    s.set(w5, b"t/3", vec![203]).unwrap();
    let res = s.commit(w5, noop).unwrap();
    assert_eq!(
        res,
        Err(Conflict {
            key: b"t/3".to_vec()
        })
    );
    s.abort(w5);
    assert_eq!(s.active_txns(), 0);

    // A fresh writer after the commit succeeds normally.
    let (w6, _) = s.begin();
    s.set(w6, b"t/3", vec![4]).unwrap();
    s.commit(w6, noop).unwrap().unwrap();
    let snap = s.snapshot();
    assert_eq!(s.get_raw(b"t/3", &snap), Some(vec![4]));
}
