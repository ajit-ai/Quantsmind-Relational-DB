//! E2 — ARIES-style recovery: Analysis / Redo / Undo over the log stream.
//!
//! Pass 1 (Analysis): replay the log, classify every txn
//!   (committed / aborted / in-flight-at-crash) and locate the newest
//!   checkpoint. Output: the txn status table + redo set.
//! Pass 2 (Redo): apply every Put from COMMITTED txns into the state map.
//!   Map-building makes redo idempotent by construction (last-writer-wins).
//! Pass 3 (Undo): txns still in flight at the crash contribute nothing;
//!   their would-be writes are reported as CLR-equivalents for audit.
//!
//! Page-level physical redo (with CLRs written back to the log) lands with
//! the page store integration; the pass structure and API are final.

use crate::wal::{WalReader, WalRecord};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxnStatus {
    Committed,
    Aborted,
    InFlight,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecoveredState {
    /// Committed key/value state after Redo.
    pub data: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Final status per txn seen in the log.
    pub status: HashMap<u64, TxnStatus>,
    /// Txns in flight at crash — their buffered writes were undone.
    pub undone: BTreeSet<u64>,
    /// Txns explicitly aborted in the log.
    pub aborted: BTreeSet<u64>,
    /// Active-txn set from the newest checkpoint, if any.
    pub checkpoint_active: Vec<u64>,
}

/// Run the three ARIES passes over a raw log.
pub fn recover(log: &[u8]) -> Result<RecoveredState, crate::Error> {
    let replay = WalReader::replay(std::io::Cursor::new(log))?;

    // ── Analysis ──
    let mut status: HashMap<u64, TxnStatus> = HashMap::new();
    let mut checkpoint_active = Vec::new();
    for (_, rec) in &replay.records {
        match rec {
            WalRecord::Begin { txn } => {
                status.insert(*txn, TxnStatus::InFlight);
            }
            WalRecord::Commit { txn } => {
                status.insert(*txn, TxnStatus::Committed);
            }
            WalRecord::Abort { txn } => {
                status.insert(*txn, TxnStatus::Aborted);
            }
            WalRecord::Checkpoint { active } => {
                checkpoint_active = active.clone();
                // txns named in a checkpoint that never finish stay in-flight
                for t in active {
                    status.entry(*t).or_insert(TxnStatus::InFlight);
                }
            }
            WalRecord::Put { .. } => {}
        }
    }
    // torn tail: trailing InFlight txns were in-flight at crash
    let mut in_flight: BTreeSet<u64> = status
        .iter()
        .filter(|(_, s)| **s == TxnStatus::InFlight)
        .map(|(t, _)| *t)
        .collect();
    let mut aborted: BTreeSet<u64> = status
        .iter()
        .filter(|(_, s)| **s == TxnStatus::Aborted)
        .map(|(t, _)| *t)
        .collect();
    let _ = &mut in_flight;
    let _ = &mut aborted;

    // ── Redo ──
    let mut data = BTreeMap::new();
    for (_, rec) in &replay.records {
        if let WalRecord::Put { txn, key, value } = rec {
            if status.get(txn) == Some(&TxnStatus::Committed) {
                data.insert(key.clone(), value.clone());
            }
        }
    }

    // ── Undo ──
    // In-flight txns' Puts never entered `data`; report them as undone.
    let mut undone = BTreeSet::new();
    for (_, rec) in &replay.records {
        if let WalRecord::Put { txn, .. } = rec {
            if status.get(txn) == Some(&TxnStatus::InFlight) {
                undone.insert(*txn);
            }
        }
    }

    Ok(RecoveredState {
        data,
        status,
        undone,
        aborted,
        checkpoint_active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wal::WalWriter;

    fn build_log(f: impl FnOnce(&mut WalWriter<&mut Vec<u8>>)) -> Vec<u8> {
        let mut log = Vec::new();
        let mut w = WalWriter::new(&mut log);
        f(&mut w);
        w.commit_group().unwrap();
        log
    }

    #[test]
    fn three_pass_recovery_committed_aborted_inflight() {
        let log = build_log(|w| {
            w.append(&WalRecord::Begin { txn: 1 });
            w.append(&WalRecord::Put {
                txn: 1,
                key: b"a".to_vec(),
                value: vec![1],
            });
            w.append(&WalRecord::Commit { txn: 1 });
            w.append(&WalRecord::Begin { txn: 2 });
            w.append(&WalRecord::Put {
                txn: 2,
                key: b"b".to_vec(),
                value: vec![2],
            });
            w.append(&WalRecord::Abort { txn: 2 });
            w.append(&WalRecord::Begin { txn: 3 });
            w.append(&WalRecord::Put {
                txn: 3,
                key: b"c".to_vec(),
                value: vec![3],
            });
            // txn 3 in flight at crash
        });

        let st = recover(&log).unwrap();
        assert_eq!(
            st.data.get(b"a".as_slice()).map(|v| v.as_slice()),
            Some(&[1][..])
        );
        assert!(!st.data.contains_key(b"b".as_slice()));
        assert!(!st.data.contains_key(b"c".as_slice()));
        assert_eq!(st.status[&1], TxnStatus::Committed);
        assert_eq!(st.status[&2], TxnStatus::Aborted);
        assert_eq!(st.status[&3], TxnStatus::InFlight);
        assert!(st.undone.contains(&3));
        assert!(st.aborted.contains(&2));
    }

    #[test]
    fn last_writer_wins_makes_redo_idempotent() {
        let log = build_log(|w| {
            for txn in 1..=3u64 {
                w.append(&WalRecord::Begin { txn });
                w.append(&WalRecord::Put {
                    txn,
                    key: b"k".to_vec(),
                    value: vec![txn as u8],
                });
                w.append(&WalRecord::Commit { txn });
            }
        });
        let s1 = recover(&log).unwrap();
        let s2 = recover(&log).unwrap();
        assert_eq!(s1, s2, "recovery must be idempotent");
        assert_eq!(
            s1.data.get(b"k".as_slice()).map(|v| v.as_slice()),
            Some(&[3][..])
        );
    }

    #[test]
    fn checkpoint_records_active_set() {
        let log = build_log(|w| {
            w.append(&WalRecord::Begin { txn: 9 });
            w.append(&WalRecord::Checkpoint {
                active: vec![9, 10],
            });
            w.append(&WalRecord::Commit { txn: 9 });
        });
        let st = recover(&log).unwrap();
        assert_eq!(st.checkpoint_active, vec![9, 10]);
        assert_eq!(st.status[&9], TxnStatus::Committed);
        assert_eq!(
            st.status[&10],
            TxnStatus::InFlight,
            "checkpointed txn that never finished"
        );
    }

    #[test]
    fn torn_tail_still_recovers_committed_prefix() {
        // Two groups: the tear lands inside the SECOND group's final frame,
        // so group 1's commit survives and group 2 is correctly discarded.
        let mut log = build_log(|w| {
            w.append(&WalRecord::Begin { txn: 1 });
            w.append(&WalRecord::Put {
                txn: 1,
                key: b"x".to_vec(),
                value: vec![7],
            });
            w.append(&WalRecord::Commit { txn: 1 });
            w.commit_group().unwrap();
            w.append(&WalRecord::Begin { txn: 2 });
            w.append(&WalRecord::Put {
                txn: 2,
                key: b"y".to_vec(),
                value: vec![8],
            });
            w.append(&WalRecord::Commit { txn: 2 });
        });
        log.truncate(log.len() - 3); // tear the tail of group 2

        let st = recover(&log).unwrap();
        assert!(st.status[&2] == TxnStatus::InFlight || st.status.get(&2).is_none());
        assert_eq!(
            st.data.get(b"x".as_slice()).map(|v| v.as_slice()),
            Some(&[7][..])
        );
        assert!(!st.data.contains_key(b"y".as_slice()));
    }
}
