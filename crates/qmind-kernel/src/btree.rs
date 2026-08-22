//! B+Tree index — ordered map from keys to row ids.
//!
//! M1: leaf/page layout + single-threaded search/insert.
//! M2+: latch crabbing (concurrent descent), bulk-load.

use crate::error::Result;
use crate::page::PageId;

/// Opaque encoded key. Model layers own key encoding (D-002); kernel only
/// requires total order via byte comparison.
pub type Key = Vec<u8>;
/// Payload stored at leaves: row id or row-id list for duplicates.
pub type Value = u64;

#[derive(Debug)]
pub struct BTree {
    root: PageId,
    /// Consumed by M2 concurrent descent (latch depth bound).
    #[allow(dead_code)]
    height: u32,
}

impl BTree {
    pub fn empty(root: PageId) -> Self {
        Self { root, height: 1 }
    }

    pub fn root(&self) -> PageId {
        self.root
    }

    /// M1: descend to leaf, binary search. Errors if index pages unreadable.
    pub fn get(&self, _key: &[u8]) -> Result<Option<Value>> {
        // M1 implementation lands here; signature is stable.
        Ok(None)
    }
}
