//! Write-Ahead Log — durability contract of the engine.
//!
//! M1: segment files, LSN allocation, group-commit window (default 1 ms).
//! M2: checkpoint records, redo/undo replay, torn-tail truncation.

use std::fmt;

pub type Lsn = u64;
pub type TxnId = u64;

/// Physiological log record. Payloads reference pages/row ids, not raw bytes,
/// so replay stays valid across minor format versions (D-003).
#[derive(Debug)]
pub enum WalRecord {
    Begin {
        txn: TxnId,
    },
    Commit {
        txn: TxnId,
    },
    Abort {
        txn: TxnId,
    },
    /// Row inserted into heap page `page` at slot `slot` (M1 heap layout).
    Insert {
        txn: TxnId,
        page: u64,
        slot: u16,
    },
}

impl fmt::Display for WalRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WalRecord::Begin { txn } => write!(f, "begin t{txn}"),
            WalRecord::Commit { txn } => write!(f, "commit t{txn}"),
            WalRecord::Abort { txn } => write!(f, "abort t{txn}"),
            WalRecord::Insert { txn, page, slot } => write!(f, "insert t{txn} p{page}s{slot}"),
        }
    }
}
