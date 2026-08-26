//! # qmind-kernel
//!
//! QuantsMind storage kernel (D-002): generic KV + index + MVCC + WAL core.
//! Relational / Document / Key-Value are model layers above this crate.
//!
//! Module map (see docs/ROADMAP.md for milestone targets):
//! - [`page`]   — 8 KiB pages, CRC-protected versioned headers   (M1)
//! - [`buffer`] — buffer pool, clock-sweep eviction              (M1)
//! - [`btree`]  — ordered index                                  (M1–M2)
//! - [`wal`]    — write-ahead log, group commit                  (M1–M2)
//! - [`mvcc`]   — snapshots & isolation                          (M2)

pub mod btree;
pub mod buffer;
pub mod column_delta;
pub mod column_reader;
pub mod columnar;
pub mod error;
pub mod eviction;
pub mod lock;
pub mod mvcc;
pub mod page;
pub mod recovery;
pub mod wal;

pub use btree::BTree;
pub use buffer::{BufferPool, PageStore, RamPageStore};
pub use error::{Error, Result};
pub use mvcc::{Conflict, IsolationLevel, MvccStore, Snapshot};
pub use page::{Page, PageHeader, PageId, PAGE_SIZE};
pub use wal::{Lsn, WalReader, WalRecord, WalWriter};
