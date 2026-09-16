//! R4-CONCURRENCY — deterministic concurrency-semantics proofs at the kernel
//! level.
//!
//! `MvccStore` hosts several live transactions in one store, so the concurrency
//! rules that the single-writer SQL layer can only enforce (not exercise) are
//! proven here with exact interleavings — no sleeps, no timing. These tests
//! pin the contract the engine relies on:
//!
//! 1. Two readers coexist without blocking each other; each holds its own
//!    immutable snapshot while a third transaction commits.
//! 2. A losing writer gets a deterministic conflict, leaves no partial state
//!    behind, aborts cleanly, and a fresh writer can take over.
//! 3. A WAL-sink failure mid-commit discards everything and the store remains
//!    fully usable for the next transaction.

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
    s.set(baseline, b"a", vec![1]);
    s.commit(baseline, noop).unwrap().unwrap();

    // Both readers snapshot the committed world {a}.
    let (r1, snap1) = s.begin();
    let (r2, snap2) = s.begin();
    assert_eq!(s.read(Some(r1), b"a", &snap1), Some(vec![1]));
    assert_eq!(s.read(Some(r2), b"a", &snap2), Some(vec![1]));

    // Writer commits a new key after both snapshots were taken.
    let (w, _) = s.begin();
    s.set(w, b"b", vec![2]);
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

/// Requirement 3 + 4 + 5: a conflicting writer is rejected deterministically,
/// releases ownership via abort, and a fresh transaction can write the same key
/// successfully — the store is never wedged by the loser.
#[test]
fn conflicting_writer_aborts_and_a_fresh_txn_retries_cleanly() {
    let mut s = MvccStore::new();

    let (t1, _) = s.begin();
    let (t2, _) = s.begin(); // both begin with the same snapshot
    s.set(t1, b"x", vec![10]);
    s.set(t2, b"x", vec![20]);
    s.commit(t1, noop).unwrap().unwrap();

    // Deterministic first-committer-wins conflict on the shared key.
    let res = s.commit(t2, noop).unwrap();
    assert_eq!(res, Err(Conflict { key: b"x".to_vec() }));

    // The loser published nothing and is still responsible for its own state.
    assert_eq!(s.commit_watermark(), 1);
    {
        let snap = s.snapshot();
        assert_eq!(s.get_raw(b"x", &snap), Some(vec![10]));
    }
    assert_eq!(s.active_txns(), 1);

    // Loser aborts (ownership release) and the store stays usable.
    s.abort(t2);
    assert_eq!(s.active_txns(), 0);

    // A brand-new writer takes over the key and commits normally.
    let (t3, _) = s.begin();
    s.set(t3, b"x", vec![30]);
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
    s.set(t1, b"z", vec![9]);
    // Sink failure: the commit errors out before publishing.
    let res: Result<Result<(), Conflict>, String> =
        s.commit(t1, |_| Err(String::from("disk full")));
    assert!(res.is_err());

    // The transaction is still the pending owner; abort it cleanly.
    assert_eq!(s.active_txns(), 1);
    s.abort(t1);
    assert_eq!(s.active_txns(), 0);

    // Nothing leaked.
    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"z", &snap), None);

    // Next transaction works normally (fresh key, clean WAL sink).
    let (t2, _) = s.begin();
    s.set(t2, b"z", vec![5]);
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
        s.set(t, &[b't', i], vec![i]);
        s.commit(t, noop).unwrap().unwrap();
    }

    // T1 snapshots the 3-row committed table.
    let (r1, snap1) = s.begin();
    assert_eq!(s.scan_prefix(b"t", &snap1).len(), 3);

    // A concurrent writer commits three more rows after T1's snapshot.
    for i in 4..=6u8 {
        let (t, _) = s.begin();
        s.set(t, &[b't', i], vec![i]);
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
