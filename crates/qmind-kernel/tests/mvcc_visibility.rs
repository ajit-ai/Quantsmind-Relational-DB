//! R4-MVCC — snapshot-isolation visibility contract proven at the kernel
//! level with two live transactions in one store.
//!
//! The SQL engine serializes writers (single-writer constraint) so the "later
//! committed commit stays invisible to an existing snapshot" rules can only be
//! exercised faithfully here, where two transactions coexist in the same
//! `MvccStore`. These tests are the behavioral contract the engine relies on:
//!
//! Rule 1/6  own writes visible to their transaction
//! Rule 2    foreign uncommitted writes invisible
//! Rule 3    committed-before-snapshot rows visible
//! Rule 4    post-snapshot commits invisible (SI, not READ COMMITTED)
//! Rule 5    repeated reads within a transaction are stable
//! Rule 7/8  rolled-back / aborted writes disappear for every reader
//! Recovery  `redo_from_records` preserves the same visibility contract

use qmind_kernel::wal::WalRecord;
use qmind_kernel::MvccStore;

fn noop(_: &[WalRecord]) -> Result<(), ()> {
    Ok(())
}

#[test]
fn rule1_own_writes_visible_to_own_transaction() {
    let mut s = MvccStore::new();
    let (txn, snap) = s.begin();
    s.set(txn, b"k", vec![1]).unwrap();
    assert_eq!(s.read(Some(txn), b"k", &snap), Some(vec![1]));
}

#[test]
fn rule2_foreign_uncommitted_writes_invisible() {
    let mut s = MvccStore::new();
    let (ta, _) = s.begin();
    let (tb, snap_b) = s.begin();
    s.set(ta, b"k", vec![1]).unwrap();
    // B's snapshot predates A's commit and A is still open — invisible.
    assert_eq!(s.read(Some(tb), b"k", &snap_b), None);
}

#[test]
fn rule3_committed_before_snapshot_is_visible() {
    let mut s = MvccStore::new();
    let (ta, _) = s.begin();
    s.set(ta, b"k", vec![1]).unwrap();
    s.commit(ta, noop).unwrap().unwrap();

    let (tb, snap_b) = s.begin();
    assert_eq!(s.read(Some(tb), b"k", &snap_b), Some(vec![1]));
}

#[test]
fn rule4_post_snapshot_commit_stays_invisible() {
    let mut s = MvccStore::new();
    let (ta, _) = s.begin();
    s.set(ta, b"k", vec![1]).unwrap();
    s.commit(ta, noop).unwrap().unwrap();

    // B snapshots the committed world {k=1}.
    let (tb, snap_b) = s.begin();
    assert_eq!(s.read(Some(tb), b"k", &snap_b), Some(vec![1]));

    // A2 commits a *later* version of k after B's snapshot.
    let (ta2, _) = s.begin();
    s.set(ta2, b"k", vec![2]).unwrap();
    s.commit(ta2, noop).unwrap().unwrap();

    // B must still observe its own snapshot (k=1), not READ COMMITTED (k=2).
    assert_eq!(s.read(Some(tb), b"k", &snap_b), Some(vec![1]));
}

#[test]
fn rule5_repeated_reads_are_stable_across_commits() {
    let mut s = MvccStore::new();
    let (ta, _) = s.begin();
    s.set(ta, b"k", vec![1]).unwrap();
    s.commit(ta, noop).unwrap().unwrap();

    // B's snapshot is taken before any later foreign commit.
    let (tb, snap_b) = s.begin();
    let first = s.read(Some(tb), b"k", &snap_b);

    for i in 2u8..5 {
        let (t, _) = s.begin();
        s.set(t, b"k", vec![i]).unwrap();
        s.commit(t, noop).unwrap().unwrap();
    }

    let later = s.read(Some(tb), b"k", &snap_b);
    assert_eq!(first, Some(vec![1]));
    assert_eq!(
        later, first,
        "snapshot B must not advance after later commits"
    );
}

#[test]
fn rule7_rolled_back_writes_disappear_for_everyone() {
    let mut s = MvccStore::new();
    let (ta, _) = s.begin();
    s.set(ta, b"k", vec![1]).unwrap();
    s.abort(ta);

    // Former writer no longer sees it; a fresh snapshot must see nothing.
    let (tb, snap_b) = s.begin();
    assert_eq!(s.read(Some(tb), b"k", &snap_b), None);
    assert_eq!(s.read(Some(ta), b"k", &snap_b), None);
}

#[test]
fn rule8_aborted_writes_never_become_visible() {
    let mut s = MvccStore::new();

    // A writes and commits a value, then a second txn overwrites and aborts —
    // the aborted version must never win over the previously committed one.
    let (ta, _) = s.begin();
    s.set(ta, b"k", vec![1]).unwrap();
    s.commit(ta, noop).unwrap().unwrap();

    let (tv, snap) = s.begin();
    s.set(tv, b"k", vec![99]).unwrap();
    s.abort(tv);

    let (tb, snap_b) = s.begin();
    assert_eq!(s.read(Some(tb), b"k", &snap_b), Some(vec![1]));
    assert_eq!(s.read(None, b"k", &snap), Some(vec![1]));
}

#[test]
fn mixed_visibility_within_single_snapshot() {
    let mut s = MvccStore::new();

    // Pre-existing committed rows.
    let (p, _) = s.begin();
    s.set(p, b"a", vec![10]).unwrap();
    s.commit(p, noop).unwrap().unwrap();

    // B's snapshot: sees committed {a}, not the later foreign writes.
    let (b_txn, snap_b) = s.begin();
    assert_eq!(s.read(Some(b_txn), b"a", &snap_b), Some(vec![10]));

    // Foreign committed write after B's snapshot.
    let (f, _) = s.begin();
    s.set(f, b"x", vec![7]).unwrap();
    s.commit(f, noop).unwrap().unwrap();

    // Foreign rolled-back write after B's snapshot.
    let (r, _) = s.begin();
    s.set(r, b"y", vec![8]).unwrap();
    s.abort(r);

    // B writes its own row and reads everything back.
    s.set(b_txn, b"b", vec![20]).unwrap();

    let read = |k: &[u8]| s.read(Some(b_txn), k, &snap_b);
    assert_eq!(read(b"a"), Some(vec![10]), "committed-before-snapshot");
    assert_eq!(read(b"b"), Some(vec![20]), "own buffered write");
    assert_eq!(read(b"x"), None, "post-snapshot committed write invisible");
    assert_eq!(read(b"y"), None, "foreign rolled-back write invisible");
}

#[test]
fn recovered_committed_rows_stay_visible_later_rows_never_leak() {
    let mut s = MvccStore::new();
    let (a, _) = s.begin();
    s.set(a, b"k", vec![1]).unwrap();
    s.commit(a, |recs| {
        let _ = recs;
        Ok::<(), ()>(())
    })
    .unwrap()
    .unwrap();

    // In-flight transaction (not yet committed) leaves no WAL footprint.
    let (inflight, _) = s.begin();
    s.set(inflight, b"junk", vec![9]).unwrap();

    // Reconstruct from the committed records (deferred logging means only the
    // committed txn's Begin/Put/Commit group is on the log).
    let records = vec![
        (1u64, WalRecord::Begin { txn: a }),
        (
            2u64,
            WalRecord::Put {
                txn: a,
                key: b"k".to_vec(),
                value: vec![1],
            },
        ),
        (3u64, WalRecord::Commit { txn: a }),
    ];
    let mut rebuilt = MvccStore::new();
    rebuilt.redo_from_records(&records);

    // A later transaction on the rebuilt store sees the committed row only.
    let (tb, snap_b) = rebuilt.begin();
    assert_eq!(rebuilt.read(Some(tb), b"k", &snap_b), Some(vec![1]));
    assert_eq!(rebuilt.read(Some(tb), b"junk", &snap_b), None);
}
