//! MVCC — snapshot isolation over the kernel (D-002 core service).
//!
//! M2 scope: transaction lifecycle, snapshots, a versioned index with
//! first-committer-wins write-write validation. In-memory structures mirror
//! the future paged layout; persistence rides the WAL at commit points.
//!
//! Model: every transaction gets a monotonic `TxnId`. A snapshot taken at
//! begin reads at watermark `xmin`: only versions created by transactions
//! with id ≤ xmin are visible, and a reader never sees later commits (SI).
//! Writers buffer locally. Commit validates that no *other* transaction with
//! id > our xmin published a version on any of our keys (first-committer-
//! wins), emits WAL records via the caller's sink BEFORE publishing, then
//! appends versions to the chains.

use crate::wal::{Lsn, TxnId, WalRecord};
use std::collections::{BTreeMap, HashMap};

/// Immutable read horizon captured at transaction start.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    /// Reads versions committed at or before this point.
    pub read_ts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadCommitted,
    SnapshotIsolation,
}

/// Error surfaced on write-write conflict at commit.
#[derive(Debug, PartialEq, Eq)]
pub struct Conflict {
    pub key: Vec<u8>,
}

/// Tracks txn ids (WAL identity), the active set, and the commit watermark
/// (version ordering). Ids and commit timestamps are deliberately separate:
/// an old txn finishing late must not look "committed before" a newer one.
#[derive(Debug, Default)]
pub struct TxnManager {
    next_txn: TxnId,
    active: std::collections::BTreeSet<TxnId>,
    /// Number of commits published so far == highest commit_ts in store.
    commit_watermark: u64,
}

impl TxnManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&mut self) -> (TxnId, Snapshot) {
        let id = self.next_txn;
        self.next_txn += 1;
        self.active.insert(id);
        (
            id,
            Snapshot {
                read_ts: self.commit_watermark,
            },
        )
    }

    pub fn finish_abort(&mut self, txn: TxnId) {
        self.active.remove(&txn);
    }

    pub fn finish_commit(&mut self, txn: TxnId) -> u64 {
        self.active.remove(&txn);
        self.commit_watermark += 1;
        self.commit_watermark
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn commit_watermark(&self) -> u64 {
        self.commit_watermark
    }
}

/// One stored version of a key.
#[derive(Debug, Clone)]
struct Version {
    commit_ts: u64,
    value: Vec<u8>,
}

/// Buffered state of an in-flight writer.
#[derive(Debug)]
struct Pending {
    snap: Snapshot,
    writes: BTreeMap<Vec<u8>, Vec<u8>>,
}

/// Snapshot-isolated KV store.
#[derive(Debug, Default)]
pub struct MvccStore {
    data: BTreeMap<Vec<u8>, Vec<Version>>,
    pending: HashMap<TxnId, Pending>,
    mgr: TxnManager,
}

impl MvccStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&mut self) -> (TxnId, Snapshot) {
        let (txn, snap) = self.mgr.begin();
        self.pending.insert(
            txn,
            Pending {
                snap,
                writes: BTreeMap::new(),
            },
        );
        (txn, snap)
    }

    /// Lock-free snapshot capture for pure readers (P5).
    ///
    /// A reader takes the engine-level read guard, captures this watermark,
    /// drops the guard, and scans with the immutable `Snapshot` — reads never
    /// contend with each other or block the writer's commit. The snapshot is
    /// only as fresh as the last commit seen by this thread.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            read_ts: self.mgr.commit_watermark(),
        }
    }

    /// Read `key` under `snap`, seeing the transaction's own buffered writes
    /// first (read-your-own-writes).
    pub fn get(&self, txn: TxnId, key: &[u8], snap: &Snapshot) -> Option<Vec<u8>> {
        if let Some(p) = self.pending.get(&txn) {
            if let Some(v) = p.writes.get(key) {
                return Some(v.clone());
            }
        }
        self.get_raw(key, snap)
    }

    /// Committed-value read without a transaction context.
    pub fn get_raw(&self, key: &[u8], snap: &Snapshot) -> Option<Vec<u8>> {
        self.data
            .get(key)?
            .iter()
            .rev()
            .find(|v| v.commit_ts <= snap.read_ts)
            .map(|v| v.value.clone())
    }

    /// All keys under a byte prefix, each with its latest committed value.
    /// Used at startup recovery to rebuild row-id counters and indexes from
    /// the recovered committed state (R2.9/2.10). Keys sharing the prefix are
    /// contiguous, so the scan stops at the first non-matching key.
    pub fn scan_prefix(&self, prefix: &[u8], snap: &Snapshot) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.data
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .filter_map(|(k, chain)| {
                let visible = chain.iter().rev().find(|v| v.commit_ts <= snap.read_ts)?;
                Some((k.clone(), visible.value.clone()))
            })
            .collect()
    }

    /// Buffer a write inside the transaction.
    pub fn set(&mut self, txn: TxnId, key: &[u8], value: Vec<u8>) {
        self.pending
            .entry(txn)
            .or_insert_with(|| Pending {
                snap: Snapshot { read_ts: 0 },
                writes: BTreeMap::new(),
            })
            .writes
            .insert(key.to_vec(), value);
    }

    /// Validate, log via `log_commit`, then publish buffered writes.
    ///
    /// Ordering guarantee: WAL records are emitted before any version lands
    /// in the index; if the sink errors, nothing is published and the txn
    /// stays open for explicit abort.
    pub fn commit<E>(
        &mut self,
        txn: TxnId,
        mut log_commit: impl FnMut(&[WalRecord]) -> Result<(), E>,
    ) -> Result<Result<(), Conflict>, E> {
        let pending = match self.pending.remove(&txn) {
            Some(p) => p,
            None => return Ok(Ok(())), // unknown/already finished
        };

        // First-committer-wins: reject if anyone else published on our keys
        // after our snapshot was taken.
        let conflict = pending.writes.keys().find_map(|key| {
            self.data
                .get(key)
                .and_then(|chain| chain.last())
                .filter(|v| v.commit_ts > pending.snap.read_ts)
                .map(|_| key.clone())
        });
        if let Some(key) = conflict {
            self.pending.insert(txn, pending);
            return Ok(Err(Conflict { key }));
        }

        if !pending.writes.is_empty() {
            let mut recs = Vec::with_capacity(pending.writes.len() + 2);
            recs.push(WalRecord::Begin { txn });
            for (key, value) in &pending.writes {
                recs.push(WalRecord::Put {
                    txn,
                    key: key.clone(),
                    value: value.clone(),
                });
            }
            recs.push(WalRecord::Commit { txn });
            log_commit(&recs)?;
        }

        let commit_ts = self.mgr.finish_commit(txn);
        for (key, value) in pending.writes {
            self.data
                .entry(key)
                .or_default()
                .push(Version { commit_ts, value });
        }
        Ok(Ok(()))
    }

    pub fn abort(&mut self, txn: TxnId) {
        self.pending.remove(&txn);
        self.mgr.finish_abort(txn);
    }

    pub fn active_txns(&self) -> usize {
        self.mgr.active_count()
    }

    pub fn commit_watermark(&self) -> u64 {
        self.mgr.commit_watermark()
    }

    /// Deterministic redo of a clean WAL prefix during startup recovery.
    ///
    /// Committed transactions are published in LSN order with ascending
    /// commit timestamps; abort/in-flight state contributes nothing; DDL and
    /// Checkpoint records carry no kernel state and are skipped. Afterwards
    /// the manager resumes with `next_txn` past every replayed txn id so new
    /// work never collides with recovered transactions. Repeated calls on the
    /// same log produce identical state — recovery is a pure function of the
    /// clean prefix (R2.5/2.15).
    pub fn redo_from_records(&mut self, records: &[(Lsn, WalRecord)]) {
        let mut active: HashMap<TxnId, Pending> = HashMap::new();
        let mut max_txn: TxnId = 0;
        for (_, rec) in records {
            match rec {
                WalRecord::Begin { txn } => {
                    max_txn = max_txn.max(*txn);
                    active.entry(*txn).or_insert_with(|| Pending {
                        snap: Snapshot { read_ts: 0 },
                        writes: BTreeMap::new(),
                    });
                }
                WalRecord::Put { txn, key, value } => {
                    max_txn = max_txn.max(*txn);
                    if let Some(p) = active.get_mut(txn) {
                        p.writes.insert(key.clone(), value.clone());
                    }
                }
                WalRecord::Commit { txn } => {
                    max_txn = max_txn.max(*txn);
                    if let Some(pending) = active.remove(txn) {
                        let commit_ts = self.mgr.commit_watermark + 1;
                        for (key, value) in pending.writes {
                            self.data
                                .entry(key)
                                .or_default()
                                .push(Version { commit_ts, value });
                        }
                        self.mgr.commit_watermark += 1;
                    }
                }
                WalRecord::Abort { txn } => {
                    max_txn = max_txn.max(*txn);
                    active.remove(txn);
                }
                _ => {}
            }
        }
        self.mgr.next_txn = max_txn + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_isolation_hides_uncommitted_and_future_writes() {
        let mut s = MvccStore::new();
        let (t0, _) = s.begin();
        s.set(t0, b"k", vec![1]);
        s.commit::<()>(t0, |_| Ok(())).unwrap().unwrap();

        let (reader, rsnap) = s.begin();

        let (t1, _) = s.begin();
        s.set(t1, b"k", vec![2]);
        assert_eq!(
            s.get(reader, b"k", &rsnap),
            Some(vec![1]),
            "uncommitted invisible"
        );
        s.commit::<()>(t1, |_| Ok(())).unwrap().unwrap();
        assert_eq!(
            s.get(reader, b"k", &rsnap),
            Some(vec![1]),
            "post-snapshot commit invisible under SI"
        );

        let (r2, s2) = s.begin();
        assert_eq!(s.get(r2, b"k", &s2), Some(vec![2]));
    }

    #[test]
    fn first_committer_wins_rejects_stale_writer() {
        let mut s = MvccStore::new();
        let (a, _sa) = s.begin();
        let (b, _sb) = s.begin();
        s.set(a, b"x", vec![10]);
        s.set(b, b"x", vec![20]);
        s.commit::<()>(a, |_| Ok(())).unwrap().unwrap();

        let res = s.commit::<()>(b, |_| Ok(())).unwrap();
        assert_eq!(res, Err(Conflict { key: b"x".to_vec() }));

        s.abort(b);
        let (c, sc) = s.begin();
        assert_eq!(s.get(c, b"x", &sc), Some(vec![10]));
    }

    #[test]
    fn distinct_keys_do_not_conflict() {
        let mut s = MvccStore::new();
        let (a, _) = s.begin();
        let (b, _) = s.begin();
        s.set(a, b"k1", vec![1]);
        s.set(b, b"k2", vec![2]);
        s.commit::<()>(a, |_| Ok(())).unwrap().unwrap();
        s.commit::<()>(b, |_| Ok(())).unwrap().unwrap();
        let (c, sc) = s.begin();
        assert_eq!(s.get(c, b"k1", &sc), Some(vec![1]));
        assert_eq!(s.get(c, b"k2", &sc), Some(vec![2]));
    }

    #[test]
    fn read_your_own_writes_and_abort_discards() {
        let mut s = MvccStore::new();
        let (t, snap) = s.begin();
        s.set(t, b"mine", vec![42]);
        assert_eq!(s.get(t, b"mine", &snap), Some(vec![42]));
        s.abort(t);
        let (t2, s2) = s.begin();
        assert_eq!(s.get(t2, b"mine", &s2), None);
    }

    #[test]
    fn wal_failure_rolls_back_publication() {
        let mut s = MvccStore::new();
        let (t, _) = s.begin();
        s.set(t, b"z", vec![9]);
        let res: Result<Result<(), Conflict>, String> =
            s.commit(t, |_| Err(String::from("disk full")));
        assert!(res.is_err(), "sink failure must surface");
        s.abort(t);
        let (v, vs) = s.begin();
        assert_eq!(s.get(v, b"z", &vs), None, "failed WAL publishes nothing");
    }

    #[test]
    fn aborted_writer_leaves_no_versions() {
        let mut s = MvccStore::new();
        let (t, _) = s.begin();
        s.set(t, b"gone", vec![5]);
        s.abort(t);
        let (r, rs) = s.begin();
        assert_eq!(s.get(r, b"gone", &rs), None);
        assert!(s.data.is_empty());
    }

    #[test]
    fn scan_prefix_yields_latest_committed_values_only() {
        let mut s = MvccStore::new();
        let (t, _) = s.begin();
        s.set(t, b"t\x01a", vec![1]);
        s.set(t, b"t\x01b", vec![2]);
        s.set(t, b"u\x01a", vec![9]); // different table prefix `u`
        s.commit::<()>(t, |_| Ok(())).unwrap().unwrap();

        let snap = s.snapshot();
        let rows = s.scan_prefix(b"t\x01", &snap);
        assert_eq!(rows.len(), 2);
        assert_eq!(s.scan_prefix(b"t\x01x", &snap).len(), 0);

        // Prefix must not be confused with a longer table name.
        let (t2, _) = s.begin();
        s.set(t2, b"ta\x01z", vec![3]);
        s.commit::<()>(t2, |_| Ok(())).unwrap().unwrap();
        let snap2 = s.snapshot();
        assert_eq!(s.scan_prefix(b"t\x01", &snap2).len(), 2);
        assert_eq!(s.scan_prefix(b"ta\x01", &snap2).len(), 1);
    }

    #[test]
    fn redo_reproduces_committed_state_and_drops_inflight() {
        // Build a log-equivalent record stream: one committed, one aborted,
        // one left in-flight (no Commit record).
        let records = vec![
            (1, WalRecord::Begin { txn: 0 }),
            (
                2,
                WalRecord::Put {
                    txn: 0,
                    key: b"a".to_vec(),
                    value: vec![1],
                },
            ),
            (3, WalRecord::Commit { txn: 0 }),
            (4, WalRecord::Begin { txn: 1 }),
            (
                5,
                WalRecord::Put {
                    txn: 1,
                    key: b"b".to_vec(),
                    value: vec![2],
                },
            ),
            (6, WalRecord::Abort { txn: 1 }),
            (7, WalRecord::Begin { txn: 2 }),
            (
                8,
                WalRecord::Put {
                    txn: 2,
                    key: b"c".to_vec(),
                    value: vec![3],
                },
            ),
        ];

        let mut s = MvccStore::new();
        s.redo_from_records(&records);

        let (r, rs) = s.begin();
        assert_eq!(r, 3, "replay resumes txn ids past the log");
        assert_eq!(s.get(r, b"a", &rs), Some(vec![1]), "committed survives");
        assert_eq!(s.get(r, b"b", &rs), None, "aborted leaves nothing");
        assert_eq!(s.get(r, b"c", &rs), None, "in-flight leaves nothing");
        assert_eq!(s.commit_watermark(), 1);

        // New work must not collide with replayed transaction ids.
        let (t3, s3) = s.begin();
        assert_eq!(t3, 4);
        s.set(t3, b"d", vec![4]);
        s.commit::<()>(t3, |_| Ok(())).unwrap().unwrap();
        assert_eq!(s.get(r, b"d", &s3), None, "replay assigns fresh watermark");
        let (r2, s2) = s.begin();
        assert_eq!(s.get(r2, b"d", &s2), Some(vec![4]));
    }

    #[test]
    fn redo_is_deterministic_and_ignores_ddl_records() {
        use crate::wal::{CatalogColumn, ColumnKind};
        let with_ddl = vec![
            (1, WalRecord::Begin { txn: 0 }),
            (
                2,
                WalRecord::Put {
                    txn: 0,
                    key: b"k".to_vec(),
                    value: vec![7],
                },
            ),
            (
                3,
                WalRecord::CreateTable {
                    name: "t".into(),
                    columns: vec![CatalogColumn {
                        name: "id".into(),
                        kind: ColumnKind::Int,
                        nullable: false,
                    }],
                },
            ),
            (4, WalRecord::Checkpoint { active: vec![] }),
            (5, WalRecord::Commit { txn: 0 }),
        ];

        let mut a = MvccStore::new();
        let mut b = MvccStore::new();
        a.redo_from_records(&with_ddl);
        b.redo_from_records(&with_ddl);

        let (r, rs) = a.begin();
        assert_eq!(a.get(r, b"k", &rs), Some(vec![7]));
        assert_eq!(a.commit_watermark(), 1);

        // Running recovery twice on a fresh instance yields identical state.
        let mut c = MvccStore::new();
        c.redo_from_records(&with_ddl);
        let (r2, rs2) = b.begin();
        let (r3, rs3) = c.begin();
        assert_eq!(
            a.get(r2, b"k", &rs2),
            c.get(r3, b"k", &rs3),
            "redo is a pure function of the log"
        );
        assert_eq!(b.commit_watermark(), c.commit_watermark());
    }

    #[test]
    fn long_reader_holds_stable_view_across_other_commits() {
        let mut s = MvccStore::new();
        let (setup, _) = s.begin();
        s.set(setup, b"a", 1i64.to_le_bytes().to_vec());
        s.set(setup, b"b", 1i64.to_le_bytes().to_vec());
        s.commit::<()>(setup, |_| Ok(())).unwrap().unwrap();

        let (reader, rsnap) = s.begin();
        // Stale writer: began before the storm, writes after it.
        let (stale, _ssnap) = s.begin();
        for i in 2..10u64 {
            let (w, _) = s.begin();
            let key = if i % 2 == 0 { b"a" as &[u8] } else { b"b" };
            s.set(w, key, i.to_le_bytes().to_vec());
            s.commit::<()>(w, |_| Ok(())).unwrap().unwrap();
        }
        assert_eq!(
            s.get(reader, b"a", &rsnap),
            Some(1i64.to_le_bytes().to_vec())
        );
        assert_eq!(
            s.get(reader, b"b", &rsnap),
            Some(1i64.to_le_bytes().to_vec())
        );

        s.set(stale, b"a", vec![99, 9]);
        let res = s.commit::<()>(stale, |_| Ok(())).unwrap();
        assert!(
            res.is_err(),
            "pre-stale writer must lose first-committer-wins"
        );
    }
}
