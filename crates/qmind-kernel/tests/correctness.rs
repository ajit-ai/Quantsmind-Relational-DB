//! P3 — Deterministic property/fuzz harnesses over the storage kernel.
//!
//! Zero external deps on purpose: these tests stay hermetic and every run is
//! byte-for-byte reproducible from a fixed seed.

use qmind_kernel::recovery::recover;
use qmind_kernel::wal::{WalReader, WalRecord, WalWriter};
use qmind_kernel::{BTree, Error, MvccStore};
use std::collections::BTreeMap;
use std::io::Cursor;

// ─────────────────────────────── PRNG ───────────────────────────────

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(splitmix64(seed))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        splitmix64(self.0)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn usize_below(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

// ─────────────────────────────── B+TREE ─────────────────────────────

fn ref_flatten(m: &BTreeMap<Vec<u8>, Vec<u64>>) -> Vec<(Vec<u8>, u64)> {
    let mut out = Vec::new();
    for (k, vs) in m {
        for v in vs {
            out.push((k.clone(), *v));
        }
    }
    out
}

fn collect_scan<'a, I>(iter: I) -> Vec<(Vec<u8>, u64)>
where
    I: Iterator<Item = (&'a Vec<u8>, u64)>,
{
    iter.map(|(k, v)| (k.clone(), v)).collect()
}

/// Random upserts against an in-memory BTreeMap oracle: full-order scan,
/// `get_all`, `len` and lower-bounded scans must all agree with the oracle.
#[test]
fn btree_matches_reference_model_under_random_inserts() {
    const TOTAL: usize = 4000;
    let mut rng = Rng::new(0xB1EF_00DD);
    let mut bt = BTree::new();
    let mut model: BTreeMap<Vec<u8>, Vec<u64>> = BTreeMap::new();
    let pool: Vec<Vec<u8>> = (0..64)
        .map(|i| format!("key-{:02}", i).into_bytes())
        .collect();

    for step in 0..TOTAL {
        let key = if rng.below(10) < 7 {
            pool[rng.usize_below(pool.len())].clone()
        } else {
            format!("key-{:02}", 64 + (step % 16)).into_bytes()
        };
        let val = rng.next() >> 32;
        bt.insert(&key, val);

        let entry = model.entry(key).or_default();
        let pos = entry.partition_point(|&x| x < val);
        entry.insert(pos, val);

        if step % 257 == 0 || step == TOTAL - 1 {
            let full_expected = ref_flatten(&model);
            assert_eq!(
                collect_scan(bt.iter()),
                full_expected,
                "full-order mismatch at step {step}"
            );
            assert_eq!(bt.len(), step + 1, "entry count mismatch at step {step}");

            let probe = &pool[rng.usize_below(pool.len())];
            let want = model.get(probe).cloned().unwrap_or_default();
            assert_eq!(bt.get_all(probe), want, "get_all mismatch at step {step}");

            let bound = format!("key-{:02}", rng.usize_below(65)).into_bytes();
            let scanned: Vec<(Vec<u8>, u64)> = collect_scan(bt.scan_from(&bound));
            let want_scan: Vec<(Vec<u8>, u64)> = full_expected
                .iter()
                .filter(|(k, _)| k >= &bound)
                .cloned()
                .collect();
            assert_eq!(scanned, want_scan, "scan_from({bound:?}) mismatch");
        }
    }
}

// ─────────────────────────────── WAL ────────────────────────────────

/// Independent re-derivation of a record's on-log byte footprint
/// (frame = `[len u32][crc u32][payload]`). Used only for placement math,
/// never to feed replay its own answers.
fn frame_len(rec: &WalRecord) -> usize {
    let payload = match rec {
        WalRecord::Begin { .. } | WalRecord::Commit { .. } | WalRecord::Abort { .. } => 1 + 8,
        WalRecord::Checkpoint { active } => 1 + 4 + 8 * active.len(),
        WalRecord::Put { key, value, .. } => 1 + 8 + 4 + key.len() + 4 + value.len(),
        WalRecord::CreateTable { name, columns } => {
            1 + 4
                + name.len()
                + 4
                + columns
                    .iter()
                    .map(|c| 4 + c.name.len() + 1 + 1)
                    .sum::<usize>()
        }
        WalRecord::CreateIndex {
            name,
            table,
            column,
        } => 1 + 4 + name.len() + 4 + table.len() + 4 + column.len(),
        WalRecord::DropIndex { name } => 1 + 4 + name.len(),
    };
    8 + payload
}

/// `(records, cumulative end offset of every frame, per-group (end, count))`
/// produced by building a realistic multi-group log.
struct LogFixture {
    /// The exact byte stream a clean shutdown would persist.
    bytes: Vec<u8>,
    /// Every appended record, in log order, numbered by LSN.
    recs: Vec<(u64, WalRecord)>,
    /// `ends[i]` is the byte offset just past record `i`'s frame.
    ends: Vec<usize>,
    /// For each `commit_group`, the `(stream offset, records so far)` it made durable.
    groups: Vec<(usize, usize)>,
}

fn build_log(rng: &mut Rng, groups: usize, start_txn: u64) -> LogFixture {
    let mut sink = Vec::new();
    let mut recs: Vec<(u64, WalRecord)> = Vec::new();
    let mut ends: Vec<usize> = Vec::new();
    let mut groups_out: Vec<(usize, usize)> = Vec::new();
    let mut cursor = 0usize;
    let mut txn = start_txn;

    let mut w = WalWriter::new(&mut sink);
    for _ in 0..groups {
        let t = txn;
        txn += 1;
        for rec in [
            WalRecord::Begin { txn: t },
            WalRecord::Put {
                txn: t,
                key: vec![
                    rng.below(250) as u8,
                    rng.below(250) as u8,
                    rng.below(250) as u8,
                ],
                value: rng.next().to_le_bytes().to_vec(),
            },
            WalRecord::Put {
                txn: t,
                key: vec![
                    rng.below(250) as u8,
                    rng.below(250) as u8,
                    rng.below(250) as u8,
                ],
                value: rng.next().to_le_bytes().to_vec(),
            },
            if rng.below(4) == 0 {
                WalRecord::Abort { txn: t }
            } else {
                WalRecord::Commit { txn: t }
            },
        ] {
            w.append(&rec);
            recs.push((w.next_lsn() - 1, rec));
        }

        if rng.below(4) == 0 {
            let active = vec![txn, txn + 1];
            w.append(&WalRecord::Checkpoint {
                active: active.clone(),
            });
            recs.push((w.next_lsn() - 1, WalRecord::Checkpoint { active }));
        }

        for rec in &recs[cursor..] {
            let end = ends.last().copied().unwrap_or(0) + frame_len(&rec.1);
            ends.push(end);
        }
        cursor = recs.len();

        w.commit_group().unwrap();
        groups_out.push((*ends.last().unwrap(), recs.len()));
    }
    drop(w);

    assert_eq!(
        ends.last().copied().unwrap_or(0),
        sink.len(),
        "placement math must pin the byte length"
    );
    LogFixture {
        bytes: sink,
        recs,
        ends,
        groups: groups_out,
    }
}

/// The core crash contract: every byte position of truncation resumes the log
/// as *exactly* the set of frames whose end <= cut — live or dead, torn or
/// clean, with `torn_tail` reported consistently.
#[test]
fn wal_every_byte_of_truncation_recovers_exact_committed_prefix() {
    let mut rng = Rng::new(0x00C0_FFEE);
    let fx = build_log(&mut rng, 40, 1);
    eprintln!("wal log bytes: {}", fx.bytes.len());

    for cut in 0..=fx.bytes.len() {
        let res = WalReader::replay(Cursor::new(&fx.bytes[..cut])).expect("prefix must replay");
        let k = fx.ends.iter().take_while(|&&e| e <= cut).count();
        assert_eq!(res.records.len(), k, "record count at cut {cut}");
        for (i, (lsn, rec)) in res.records.iter().enumerate() {
            assert_eq!(*lsn, fx.recs[i].0, "LSN chain must stay dense");
            assert_eq!(
                *rec, fx.recs[i].1,
                "record {i} must match written record at cut {cut}"
            );
        }
        let torn = if k == 0 {
            cut != 0
        } else {
            fx.ends[k - 1] != cut
        };
        assert_eq!(res.torn_tail, torn, "torn_tail misreported at cut {cut}");
    }
}

/// Group-commit boundaries are exact durability points: replay at any group
/// boundary is clean and yields precisely that group's committed records.
#[test]
fn wal_group_boundaries_are_clean_durability_points() {
    let mut rng = Rng::new(0xD0D0_F00D);
    let fx = build_log(&mut rng, 25, 1);

    for &(off, count) in &fx.groups {
        let res = WalReader::replay(Cursor::new(&fx.bytes[..off])).unwrap();
        assert!(!res.torn_tail, "group end {off} must replay clean");
        assert_eq!(res.records.len(), count, "durability point at {off}");
        for (i, (lsn, rec)) in res.records.iter().enumerate() {
            assert_eq!(*lsn, fx.recs[i].0);
            assert_eq!(*rec, fx.recs[i].1);
        }
    }
}

/// Untampered logs must replay exactly; a tampered log must either detect the
/// corruption (stop before the bad frame with `torn_tail`) or fail loudly —
/// never emit a phantom record.
#[test]
fn wal_corruption_detected_or_torn_never_silent() {
    let mut rng = Rng::new(0x00C0_FFEE);
    let fx = build_log(&mut rng, 20, 1);

    // Sanity: the pristine log replays losslessly.
    let pristine = WalReader::replay(Cursor::new(&fx.bytes)).unwrap();
    assert!(!pristine.torn_tail);
    assert_eq!(pristine.records, fx.recs);

    for trial in 0..400 {
        let pos = rng.usize_below(fx.bytes.len());
        let mut damaged = fx.bytes.clone();
        damaged[pos] ^= 0xA5;
        match WalReader::replay(Cursor::new(&damaged)) {
            Ok(res) => {
                let k = fx.ends.iter().take_while(|&&e| e <= pos).count();
                assert_eq!(
                    res.records.len(),
                    k,
                    "bad frame {pos} leaked through (trial {trial})"
                );
                for (i, (lsn, rec)) in res.records.iter().enumerate() {
                    assert_eq!(*lsn, fx.recs[i].0);
                    assert_eq!(
                        *rec, fx.recs[i].1,
                        "phantom record after corruption at {pos}"
                    );
                }
                assert!(res.torn_tail, "CRC failure at {pos} must be flagged");
            }
            Err(e) => {
                assert!(
                    matches!(e, Error::WalCorrupt { .. }),
                    "unexpected error kind at {pos}"
                );
            }
        }
    }
}

// ─────────────────────────────── MVCC ───────────────────────────────

fn bval(v: u64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

fn kbyte(k: u8) -> Vec<u8> {
    vec![k]
}

/// Model of committed state: key -> (commit watermark, value). Visibility at a
/// snapshot = the entry with the largest watermark <= snapshot read_ts.
type Model = BTreeMap<u8, (u64, u64)>;

fn visible(model: &Model, key: u8, wm: u64) -> Option<Vec<u8>> {
    model
        .get(&key)
        .and_then(|(cwm, v)| (*cwm <= wm).then(|| bval(*v)))
}

/// Interleaved transactions, overlapping writers, aborts, first-committer-wins
/// conflicts and a long-lived reader, all checked against a serial-history
/// model (watermark order == commit order).
#[test]
fn mvcc_snapshot_isolation_matches_serial_history() {
    const STEPS: usize = 1200;
    let mut rng = Rng::new(0x5EED_5EED);
    let mut store = MvccStore::new();
    let mut model: Model = BTreeMap::new();
    // in-flight writers: (txn, key, value, snapshot read_ts)
    let mut inflight: Vec<(u64, u8, u64, u64)> = Vec::new();
    let mut long_reader: Option<(u64, qmind_kernel::Snapshot, Model)> = None;

    for step in 0..STEPS {
        let action = rng.below(10);
        match action {
            0..=2 => {
                // begin a fresh writer
                let key = rng.below(8) as u8;
                let value = rng.next() >> 32;
                let (txn, snap) = store.begin();
                match store.set(txn, &kbyte(key), bval(value)) {
                    Ok(()) => {
                        assert_eq!(
                            store.get(txn, &kbyte(key), &snap),
                            Some(bval(value)),
                            "read-your-own-writes broken"
                        );
                        inflight.push((txn, key, value, snap.read_ts));
                    }
                    Err(_) => {
                        // Strict 2PL: another live writer already holds the key
                        // (no two in-flight writers share a key). The conflict
                        // is deterministic and non-blocking — abort immediately.
                        store.abort(txn);
                    }
                }
            }
            3..=7 => {
                // resolve one writer: commit or abort
                if inflight.is_empty() {
                    continue;
                }
                let idx = rng.usize_below(inflight.len());
                let (txn, key, value, read_ts) = inflight.remove(idx);
                if rng.below(4) == 0 {
                    store.abort(txn);
                    continue;
                }
                let expected_conflict = model
                    .get(&key)
                    .map(|(cwm, _)| *cwm > read_ts)
                    .unwrap_or(false);
                match store.commit::<()>(txn, |_| Ok(())).unwrap() {
                    Ok(()) => {
                        assert!(
                            !expected_conflict,
                            "first-committer-wins must reject stale writer at step {step}"
                        );
                        let wm = store.commit_watermark();
                        model.insert(key, (wm, value));
                    }
                    Err(conflict) => {
                        assert!(
                            expected_conflict,
                            "spurious conflict on key {conflict:?} at step {step}"
                        );
                        store.abort(txn);
                    }
                }
            }
            _ => {
                // snapshot a long-lived reader once, then keep it open
                if long_reader.is_none() {
                    let (txn, snap) = store.begin();
                    long_reader = Some((txn, snap, model.clone()));
                }
            }
        }

        // Fresh-snapshot reads must always match the model at current watermark.
        if step % 97 == 0 {
            let (_, snap) = store.begin();
            for key in 0..8u8 {
                assert_eq!(
                    store.get_raw(&kbyte(key), &snap),
                    visible(&model, key, snap.read_ts),
                    "committed-state mismatch at step {step}, key {key}"
                );
            }
        }
    }

    // Snapshot isolation: the long-lived reader still sees its frozen world.
    if let Some((txn, snap, frozen)) = long_reader {
        for key in 0..8u8 {
            let want = visible(&frozen, key, snap.read_ts);
            let got = store.get(txn, &kbyte(key), &snap);
            assert_eq!(
                got, want,
                "long reader saw a post-snapshot write on key {key}"
            );
        }
    }
}

/// End-to-end crash contract: a txn's writes enter recovered state iff its
/// entire commit group reached storage before the cut — i.e. zero committed
/// loss, zero phantom commits, nothing published for a torn group.
#[test]
fn mvcc_wal_crash_recovers_exactly_the_committed_survivors() {
    const COMMITS: usize = 300;
    let mut rng = Rng::new(0x5EED_CAFE);
    let mut log = Vec::new();
    let mut wal = WalWriter::new(&mut log);
    let mut store = MvccStore::new();

    // Per successful commit: (begin rec idx, commit rec idx, txn, key, value)
    // in log order — which is exactly commit order (watermark order).
    let mut spans: Vec<(usize, usize, u64, u8, u64)> = Vec::new();
    let mut all_recs: Vec<WalRecord> = Vec::new();
    let mut model: Model = BTreeMap::new();

    for _ in 0..COMMITS {
        let key = rng.below(8) as u8;
        let value = rng.next() >> 32;
        let (txn, _snap) = store.begin();
        store.set(txn, &kbyte(key), bval(value)).unwrap();

        let mut span: Option<(usize, usize)> = None;
        match store
            .commit(txn, |records| -> qmind_kernel::Result<()> {
                let start = all_recs.len();
                for r in records {
                    wal.append(r);
                    all_recs.push(r.clone());
                }
                wal.commit_group()?;
                span = Some((start, all_recs.len() - 1));
                Ok(())
            })
            .unwrap()
        {
            Ok(()) => {
                let (begin, commit) = span.expect("span recorded on every commit");
                let wm = store.commit_watermark();
                model.insert(key, (wm, value));
                spans.push((begin, commit, txn, key, value));
            }
            Err(_conflict) => store.abort(txn),
        }
    }
    drop(wal);

    // Byte offset just past each record's frame, derived independently.
    let mut ends = Vec::new();
    for r in &all_recs {
        ends.push(ends.last().copied().unwrap_or(0) + frame_len(r));
    }
    let log_len = ends.last().copied().unwrap_or(0);
    assert_eq!(
        log_len,
        log.len(),
        "fixture placement must pin the byte length"
    );

    // The store's in-memory committed state must match the model end-to-end.
    let (_, final_snap) = store.begin();
    for key in 0..8u8 {
        assert_eq!(
            store.get_raw(&kbyte(key), &final_snap),
            visible(&model, key, final_snap.read_ts),
            "in-memory MVCC state drifted from model on key {key}"
        );
    }

    // Sample cuts: every group durability point, the endpoints, plus random
    // mid-group tears.
    let mut cuts: Vec<usize> = spans
        .iter()
        .map(|&(_, commit, _, _, _)| ends[commit])
        .collect();
    cuts.push(0);
    cuts.push(log_len);
    let mut rng2 = Rng::new(0xCAC0);
    for _ in 0..250 {
        let c = rng2.usize_below(log_len + 1);
        if !cuts.contains(&c) {
            cuts.push(c);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    for cut in cuts {
        let recovered = recover(&log[..cut]).expect("crash-recovery must not fail");

        // Redo state == surviving committed writers, last-writer-wins in commit order.
        let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for &(begin, commit, _, key, value) in &spans {
            let survived = ends[begin] <= cut && ends[commit] <= cut;
            if survived {
                expected.insert(vec![key], bval(value));
            }
        }
        assert_eq!(
            recovered.data, expected,
            "redo mismatch after crash at {cut}"
        );

        // Status == Committed iff the whole group survived; otherwise the txn
        // is at-crash (InFlight) and must not be reported committed.
        for &(begin, commit, txn, _, _) in &spans {
            let survived = ends[begin] <= cut && ends[commit] <= cut;
            let got = recovered.status.get(&txn);
            if survived {
                assert_eq!(
                    got,
                    Some(&qmind_kernel::recovery::TxnStatus::Committed),
                    "lost a committed txn {txn} at cut {cut}"
                );
            } else {
                assert_ne!(
                    got,
                    Some(&qmind_kernel::recovery::TxnStatus::Committed),
                    "ghost-committed txn {txn} at cut {cut}"
                );
            }
        }
    }
}
