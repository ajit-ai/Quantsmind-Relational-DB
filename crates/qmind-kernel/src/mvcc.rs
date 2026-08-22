//! MVCC — snapshot isolation over the kernel (D-002 core service).
//!
//! M2: transaction manager, visibility checks, first-committer-wins.

use crate::wal::TxnId;

/// Immutable read horizon for a statement or transaction.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    /// Snapshots see effects of txns with id < xmin.
    pub xmin: TxnId,
    /// Next unallocated id at snapshot time.
    pub xmax: TxnId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadCommitted,
    SnapshotIsolation,
}
