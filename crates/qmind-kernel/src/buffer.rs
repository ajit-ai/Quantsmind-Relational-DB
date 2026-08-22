//! Buffer pool — page cache between executor and store.
//!
//! M1: clock-sweep eviction, pin/unpin, dirty tracking, checkpoint handoff.

use crate::error::Result;
use crate::page::{Page, PageId};

pub type FrameId = usize;

/// Page cache contract. Implementations must be safe under concurrent access
/// from the executor's worker threads (M3+).
pub trait BufferManager {
    /// Pin a page for reading; blocks while eviction loads it.
    fn pin(&mut self, id: PageId) -> Result<&Page>;

    /// Release a previously pinned frame.
    fn unpin(&mut self, id: PageId, dirty: bool);

    /// Flush dirty frames up to and including `upto_lsn` (checkpoint path).
    fn flush(&mut self, upto_lsn: Option<crate::wal::Lsn>) -> Result<usize>;
}
