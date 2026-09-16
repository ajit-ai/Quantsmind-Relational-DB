//! R4-LOCK — strict two-phase locking exercised at the kernel level.
//!
//! `MvccStore::set` acquires an exclusive lock on the row key (non-blocking,
//! deterministic) that is held until COMMIT/ROLLBACK/abort. Because the SQL
//! layer is single-writer, actual contention can only be constructed here,
//! where several live transactions share one store. These tests pin the
//! strict-2PL contract:
//!
//! 1.  A write lock is acquired at `set` and held until commit or abort.
//! 2.  On conflict the requester gets `LockError::Busy` immediately, leaves
//!     no queue residue, and can retry after the holder finishes.
//! 3.  First-committer-wins still guards writes that begin on a stale
//!     snapshot and land after the lock is released.
//! 4.  A WAL-sink failure reaps the txn (id + locks) with no caller cleanup.
//! 5.  `lock_write` reserves row keys up front; reservations and writes are
//!     re-entrant and all released together at the transaction boundary.

use qmind_kernel::lock::LockError;
use qmind_kernel::wal::WalRecord;
use qmind_kernel::{Conflict, MvccStore, Snapshot};

fn noop(_: &[WalRecord]) -> Result<(), ()> {
    Ok(())
}

#[test]
fn exclusive_lock_held_from_set_until_commit() {
    let mut s = MvccStore::new();

    let (t1, _) = s.begin();
    s.set(t1, b"k", vec![1]).unwrap();
    // A second writer cannot hold the key while t1 owns it.
    let (t2, snap) = s.begin();
    assert!(matches!(
        s.set(t2, b"k", vec![2]).unwrap_err(),
        LockError::Busy
    ));
    assert_eq!(
        s.read(Some(t2), b"k", &snap),
        None,
        "the conflicting set buffered nothing"
    );
    s.commit(t1, noop).unwrap().unwrap();

    // Strict 2PL: the commit released the lock, so a fresh writer proceeds.
    let (t3, _) = s.begin();
    s.set(t3, b"k", vec![3]).unwrap();
    s.commit(t3, noop).unwrap().unwrap();

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"k", &snap), Some(vec![3]));
}

#[test]
fn exclusive_lock_released_on_abort() {
    let mut s = MvccStore::new();

    let (t1, _) = s.begin();
    s.set(t1, b"k", vec![1]).unwrap();
    let (t2, _) = s.begin();
    assert!(matches!(
        s.set(t2, b"k", vec![2]).unwrap_err(),
        LockError::Busy
    ));
    s.abort(t1);

    // The abort released the key — no queue residue blocks t2's retry.
    s.set(t2, b"k", vec![2]).unwrap();
    s.commit(t2, noop).unwrap().unwrap();

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"k", &snap), Some(vec![2]));
}

#[test]
fn lock_conflict_leaves_no_queue_residue_and_requester_retries() {
    let mut s = MvccStore::new();

    let (t1, _) = s.begin();
    s.set(t1, b"k", vec![10]).unwrap();

    // A no-wait conflict must not occupy the FIFO queue: after the holder
    // aborts, the very same transaction can acquire without a stale waiter
    // entry sitting ahead of it.
    let (t2, snap) = s.begin();
    assert!(matches!(
        s.set(t2, b"k", vec![20]).unwrap_err(),
        LockError::Busy
    ));
    s.abort(t1);
    s.set(t2, b"k", vec![20]).unwrap();
    s.commit(t2, noop).unwrap().unwrap();

    // t2's snapshot predates t1's abort; nothing committed over the key.
    assert_eq!(s.read(Some(t2), b"k", &snap), None);

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"k", &snap), Some(vec![20]));
}

#[test]
fn first_committer_wins_survives_lock_release_on_stale_snapshot() {
    let mut s = MvccStore::new();

    // T2 begins before T1 commits: its snapshot stays stale at ts=0.
    let (t1, _) = s.begin();
    let (t2, _) = s.begin();
    s.set(t1, b"x", vec![1]).unwrap();
    assert!(matches!(
        s.set(t2, b"x", vec![2]).unwrap_err(),
        LockError::Busy
    ));

    s.commit(t1, noop).unwrap().unwrap();
    // The lock is released at commit, but T2's own-snapshot check must still
    // reject the write to a row committed after T2 began (first-committer-wins).
    s.set(t2, b"x", vec![2]).unwrap();
    let res = s.commit(t2, noop).unwrap();
    assert_eq!(res, Err(Conflict { key: b"x".to_vec() }));
    s.abort(t2);

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"x", &snap), Some(vec![1]));
}

#[test]
fn wal_failure_reaps_txn_and_releases_locks_automatically() {
    let mut s = MvccStore::new();

    let (t1, _) = s.begin();
    s.set(t1, b"k", vec![1]).unwrap();
    let res: Result<Result<(), Conflict>, String> =
        s.commit(t1, |_| Err(String::from("disk full")));
    assert!(res.is_err());
    // The failed commit cleaned up the txn id and its locks immediately.
    assert_eq!(s.active_txns(), 0);

    let (t2, _) = s.begin();
    s.set(t2, b"k", vec![2]).unwrap(); // same key: the lock was released
    s.commit(t2, noop).unwrap().unwrap();

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"k", &snap), Some(vec![2]));
}

#[test]
fn lock_write_reservation_is_all_or_nothing_at_the_lock_level() {
    let mut s = MvccStore::new();

    let (t1, _) = s.begin();
    s.lock_write(t1, b"k1").unwrap();
    s.lock_write(t1, b"k2").unwrap();

    // T2 trips on one of the reserved keys: the multi-row statement fails.
    let (t2, _) = s.begin();
    assert!(matches!(
        s.lock_write(t2, b"k2").unwrap_err(),
        LockError::Busy
    ));
    s.abort(t2);

    // The failed statement's earlier reservations stay T1's until *its* own
    // boundary (strict 2PL); re-reserving the row is a re-entrant no-op.
    assert!(s.lock_write(t1, b"k2").is_ok());

    // The transaction boundary releases every reservation and write at once.
    s.abort(t1);

    // After T1's abort, a fresh transaction reserves and writes both keys.
    let (t3, _) = s.begin();
    s.lock_write(t3, b"k1").unwrap();
    s.lock_write(t3, b"k2").unwrap();
    s.set(t3, b"k1", vec![1]).unwrap();
    s.set(t3, b"k2", vec![2]).unwrap();
    s.commit(t3, noop).unwrap().unwrap();

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"k1", &snap), Some(vec![1]));
    assert_eq!(s.read(Some(r), b"k2", &snap), Some(vec![2]));
}

#[test]
fn distinct_keys_never_conflict() {
    let mut s = MvccStore::new();

    let (a, _) = s.begin();
    let (b, _) = s.begin();
    s.set(a, b"k1", vec![1]).unwrap();
    s.set(b, b"k2", vec![2]).unwrap();
    s.commit(a, noop).unwrap().unwrap();
    s.commit(b, noop).unwrap().unwrap();

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"k1", &snap), Some(vec![1]));
    assert_eq!(s.read(Some(r), b"k2", &snap), Some(vec![2]));
}

#[test]
fn same_transaction_writes_same_row_repeatedly() {
    let mut s = MvccStore::new();

    let (t, _) = s.begin();
    s.set(t, b"k", vec![1]).unwrap();
    s.set(t, b"k", vec![2]).unwrap(); // re-entrant on the same exclusive lock
    s.set(t, b"k", vec![3]).unwrap();
    s.commit(t, noop).unwrap().unwrap();

    let (r, snap) = s.begin();
    assert_eq!(s.read(Some(r), b"k", &snap), Some(vec![3]));
}

#[test]
fn snapshot_reads_take_no_locks_while_writer_holds_row() {
    let mut s = MvccStore::new();

    let (base, _) = s.begin();
    s.set(base, b"k", vec![1]).unwrap();
    s.commit(base, noop).unwrap().unwrap();
    assert_eq!(s.commit_watermark(), 1);

    // A live reader snapshot is taken before the writer begins.
    let (reader, rsnap) = s.begin();
    assert_eq!(s.read(Some(reader), b"k", &rsnap), Some(vec![1]));

    let (w, _) = s.begin();
    s.set(w, b"k", vec![2]).unwrap();
    s.set(w, b"z", vec![9]).unwrap();

    // Reads are lock-free: the reader never contends with the writer's locks
    // and never observes its pending writes.
    assert_eq!(s.read(Some(reader), b"k", &rsnap), Some(vec![1]));
    assert_eq!(s.read(Some(reader), b"z", &rsnap), None);
    s.commit(w, noop).unwrap().unwrap();

    // SI holds: even after the commit, the open reader keeps its snapshot.
    let fresh: Snapshot = s.snapshot();
    assert_eq!(s.read(Some(reader), b"k", &rsnap), Some(vec![1]));
    assert_eq!(s.read(Some(reader), b"z", &rsnap), None);
    assert_eq!(s.get_raw(b"k", &fresh), Some(vec![2]));
    assert_eq!(s.get_raw(b"z", &fresh), Some(vec![9]));
    s.abort(reader);
}
