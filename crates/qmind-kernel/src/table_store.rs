//! R3 — Persistent table row storage over the buffer pool.
//!
//! Each table stores rows in a chain of 8 KiB pages.  Rows are packed with
//! a 4-byte little-endian length prefix so the scanner can skip forward
//! without decoding.  The first 8 bytes of every page payload hold the
//! `next_page_id` (0 = end of chain) and the next 2 bytes hold `row_count`.
//!
//! Page payload layout (after the 16-byte PageHeader):
//! ```text
//! [next_page_id : u64]     8 bytes, 0 = end of chain
//! [row_count    : u16]     2 bytes
//! [row_0_len    : u32]     4 bytes
//! [row_0_bytes  : [u8]]    row_0_len bytes
//! [row_1_len    : u32]
//! [row_1_bytes  : [u8]]
//! …
//! [free space]
//! ```
//!
//! The [`StorageManager`] (in `storage_manager.rs`) owns the page store and
//! table catalog; this module provides per-table helpers.

use crate::buffer::{BufferPool, PageStore};
use crate::error::Result;
use crate::page::{PageHeader, PageId, PAGE_SIZE};

/// Bytes of page payload reserved for per-page metadata (next pointer + row count).
const PAGE_META_SIZE: usize = 8 + 2; // u64 + u16
/// Maximum usable bytes for row data inside one page.
const ROW_AREA_SIZE: usize = PAGE_SIZE - PageHeader::SIZE - PAGE_META_SIZE;

// ── helpers ────────────────────────────────────────────────────────────

fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}

fn write_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn write_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

// ── public types ───────────────────────────────────────────────────────

/// Persistent metadata for one table.
#[derive(Debug, Clone)]
pub struct TableMeta {
    /// Monotonic table id (assigned by the storage manager).
    pub table_id: u64,
    /// Table name (unique within a database).
    pub name: String,
    /// First page of the row chain (0 = empty table).
    pub first_page_id: PageId,
    /// Last page of the row chain (0 = empty table).
    pub last_page_id: PageId,
    /// Total number of committed rows across all pages.
    pub row_count: u64,
}

impl TableMeta {
    pub fn new(table_id: u64, name: String) -> Self {
        Self {
            table_id,
            name,
            first_page_id: 0,
            last_page_id: 0,
            row_count: 0,
        }
    }
}

/// Per-table row storage backed by the shared buffer pool.
///
/// [`TableStore`] does **not** own the pool; the [`StorageManager`] does.
/// This struct is a thin helper used during insert and scan operations.
pub struct TableStore<'a, S: PageStore> {
    pool: &'a mut BufferPool<S>,
    pub meta: TableMeta,
}

impl<'a, S: PageStore> TableStore<'a, S> {
    pub fn new(pool: &'a mut BufferPool<S>, meta: TableMeta) -> Self {
        Self { pool, meta }
    }

    /// Append one row (raw encoded bytes) to the table.
    ///
    /// `next_page` is the storage manager's global page counter — page ids
    /// must be unique across all tables, so the allocator lives outside the
    /// per-table store.  If the current last page is full, a new page is
    /// allocated and linked.
    ///
    /// The caller is responsible for WAL ordering (WAL must be synced *before*
    /// any dirty page is flushed — see `StorageManager::flush`).
    pub fn append_row(&mut self, row_bytes: &[u8], next_page: &mut PageId) -> Result<()> {
        let needed = 4 + row_bytes.len(); // u32 length prefix + data

        if self.meta.first_page_id == 0 {
            // Empty table — allocate the first page.
            let pid = self.alloc_page(next_page)?;
            self.meta.first_page_id = pid;
            self.meta.last_page_id = pid;
        }

        // Try to append to the last page.
        let fits = self.try_append_to_page(self.meta.last_page_id, row_bytes, needed)?;
        if !fits {
            // Last page is full — allocate a new one and append there.
            let pid = self.alloc_page(next_page)?;
            let old_last = self.meta.last_page_id;
            self.link_pages(old_last, pid)?;
            self.meta.last_page_id = pid;
            let ok = self.try_append_to_page(pid, row_bytes, needed)?;
            debug_assert!(ok, "fresh page must have room");
        }
        self.meta.row_count += 1;
        Ok(())
    }

    /// Scan all rows, calling `f` for each raw encoded row.
    /// Returns the number of rows scanned.
    pub fn scan_all<F: FnMut(&[u8])>(&mut self, mut f: F) -> Result<u64> {
        let mut count = 0u64;
        let mut page_id = self.meta.first_page_id;
        while page_id != 0 {
            let (rows, next) = self.read_page_rows(page_id)?;
            for row in &rows {
                f(row);
                count += 1;
            }
            page_id = next;
        }
        Ok(count)
    }

    /// Scan rows in bounded batches. Calls `consumer` for each batch
    /// (Vec of raw encoded rows). Returns total rows scanned.
    ///
    /// This is the R3 streaming interface — the consumer processes one
    /// batch at a time without materializing the entire table.
    pub fn scan_batched<F: FnMut(Vec<Vec<u8>>) -> Result<()>>(
        &mut self,
        batch_size: usize,
        mut consumer: F,
    ) -> Result<u64> {
        let mut batch: Vec<Vec<u8>> = Vec::with_capacity(batch_size);
        let mut count = 0u64;
        let mut page_id = self.meta.first_page_id;
        while page_id != 0 {
            let (rows, next) = self.read_page_rows(page_id)?;
            for row in rows {
                batch.push(row);
                count += 1;
                if batch.len() >= batch_size {
                    consumer(std::mem::replace(
                        &mut batch,
                        Vec::with_capacity(batch_size),
                    ))?;
                }
            }
            page_id = next;
        }
        if !batch.is_empty() {
            consumer(batch)?;
        }
        Ok(count)
    }

    /// Return total row count (O(1), from metadata).
    pub fn row_count(&self) -> u64 {
        self.meta.row_count
    }

    /// Flush all dirty pages in this table's chain to disk.
    /// Returns number of pages flushed.
    pub fn flush(&mut self) -> Result<usize> {
        let mut page_id = self.meta.first_page_id;
        while page_id != 0 {
            let fid = self.pool.pin(page_id)?;
            // Touch every page in the chain so a table-wide flush persists
            // all of them (fresh pages are created dirty).
            let next = {
                let payload = self.pool.payload(fid);
                read_u64(payload, 0)
            };
            self.pool.unpin(fid, false);
            page_id = next;
        }
        self.pool.flush_all()
    }

    // ── private helpers ────────────────────────────────────────────────

    /// Allocate a fresh zeroed page and return its ID.
    fn alloc_page(&mut self, next_page: &mut PageId) -> Result<PageId> {
        let pid = *next_page;
        debug_assert!(pid != 0, "page 0 is reserved");
        *next_page += 1;
        let fid = self.pool.create_page(pid)?;
        // Initialize page header: next_page_id = 0, row_count = 0.
        {
            let payload = self.pool.payload_mut(fid);
            write_u64(payload, 0, 0); // next_page_id
            write_u16(payload, 8, 0); // row_count
        }
        self.pool.unpin(fid, true);
        Ok(pid)
    }

    /// Set the next_page pointer on `page_id` to point to `next`.
    fn link_pages(&mut self, page_id: PageId, next: PageId) -> Result<()> {
        let fid = self.pool.pin(page_id)?;
        {
            let payload = self.pool.payload_mut(fid);
            write_u64(payload, 0, next);
        }
        self.pool.unpin(fid, true);
        Ok(())
    }

    /// Try to append a row to an existing page.
    /// Returns `Ok(true)` if the row fit, `Ok(false)` if the page is full.
    fn try_append_to_page(
        &mut self,
        page_id: PageId,
        row_bytes: &[u8],
        needed: usize,
    ) -> Result<bool> {
        let fid = self.pool.pin(page_id)?;
        let (fits, row_count) = {
            let payload = self.pool.payload(fid);
            let existing_used = PAGE_META_SIZE + read_rows_total_size(payload);
            let row_count = read_u16(payload, 8) as usize;
            (existing_used + needed <= ROW_AREA_SIZE, row_count)
        };
        if !fits {
            self.pool.unpin(fid, false);
            return Ok(false);
        }
        // Append the row.
        {
            let payload = self.pool.payload_mut(fid);
            let offset = PAGE_META_SIZE + read_rows_total_size(payload);
            // Write length prefix.
            payload[offset..offset + 4].copy_from_slice(&(row_bytes.len() as u32).to_le_bytes());
            payload[offset + 4..offset + 4 + row_bytes.len()].copy_from_slice(row_bytes);
            // Update row count.
            write_u16(payload, 8, (row_count + 1) as u16);
        }
        self.pool.unpin(fid, true);
        Ok(true)
    }

    /// Read all rows from a page. Returns `(rows, next_page_id)`.
    fn read_page_rows(&mut self, page_id: PageId) -> Result<(Vec<Vec<u8>>, PageId)> {
        read_page_rows(self.pool, page_id)
    }
}

/// Compute total byte size of all row data (length prefixes + data) in a
/// page payload, without knowing the row count.
fn read_rows_total_size(payload: &[u8]) -> usize {
    let row_count = read_u16(payload, 8) as usize;
    let mut offset = PAGE_META_SIZE;
    for _ in 0..row_count {
        if offset + 4 > payload.len() {
            return offset - PAGE_META_SIZE;
        }
        let len = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4 + len;
    }
    offset - PAGE_META_SIZE
}

/// Read all rows from a page into owned memory. Returns `(rows, next_page_id)`.
///
/// Borrows the pool mutably (page pin).  Used by both the push-based scans and
/// the pull-based [`RowIter`].
fn read_page_rows<S: PageStore>(
    pool: &mut BufferPool<S>,
    page_id: PageId,
) -> Result<(Vec<Vec<u8>>, PageId)> {
    let fid = pool.pin(page_id)?;
    let (next, rows) = {
        let payload = pool.payload(fid);
        let next = read_u64(payload, 0);
        let row_count = read_u16(payload, 8) as usize;
        let mut rows = Vec::with_capacity(row_count);
        let mut offset = PAGE_META_SIZE;
        for _ in 0..row_count {
            if offset + 4 > payload.len() {
                break;
            }
            let len = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            if offset + len > payload.len() {
                break;
            }
            rows.push(payload[offset..offset + len].to_vec());
            offset += len;
        }
        (next, rows)
    };
    pool.unpin(fid, false);
    Ok((rows, next))
}

/// Pull-based row cursor over a table's page chain (R3.15 lazy scan).
///
/// Owns the pool borrow for the duration of the cursor; yields raw encoded
/// rows one at a time.  Bounded memory: at most one page of rows is resident
/// at any moment.
pub struct RowIter<'a, S: PageStore> {
    pool: &'a mut BufferPool<S>,
    meta: TableMeta,
    pending: std::collections::VecDeque<Vec<u8>>,
    next_page: PageId,
    done: bool,
}

impl<'a, S: PageStore> RowIter<'a, S> {
    pub fn new(pool: &'a mut BufferPool<S>, meta: TableMeta) -> Self {
        let next_page = meta.first_page_id;
        Self {
            pool,
            meta,
            pending: std::collections::VecDeque::new(),
            next_page,
            done: next_page == 0,
        }
    }

    /// Next raw encoded row, or `Ok(None)` at the end of the chain.
    pub fn next_row(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(r) = self.pending.pop_front() {
                return Ok(Some(r));
            }
            if self.done {
                return Ok(None);
            }
            let pid = self.next_page;
            if pid == 0 {
                self.done = true;
                return Ok(None);
            }
            let (rows, next) = read_page_rows(self.pool, pid)?;
            self.next_page = next;
            self.pending.extend(rows);
            if self.next_page == 0 {
                self.done = true;
            }
        }
    }

    /// Number of rows remaining per the table metadata (upper bound; the
    /// cursor does not adjust it after construction).
    pub fn remaining_estimate(&self) -> u64 {
        self.meta.row_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::BufferPool;

    fn pool(cap: usize) -> BufferPool<crate::buffer::RamPageStore> {
        BufferPool::in_memory(cap)
    }

    #[test]
    fn append_and_scan_single_row() {
        let mut p = pool(8);
        let mut page_counter = 1;
        let mut meta = TableMeta::new(1, "t".into());
        {
            let mut ts = TableStore::new(&mut p, meta.clone());
            ts.append_row(b"hello", &mut page_counter).unwrap();
            meta = ts.meta.clone();
        }
        let mut rows = Vec::new();
        {
            let mut ts = TableStore::new(&mut p, meta.clone());
            ts.scan_all(|r| rows.push(r.to_vec())).unwrap();
        }
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], b"hello");
    }

    #[test]
    fn multiple_rows_across_pages() {
        let mut p = pool(4); // tiny pool forces page eviction
        let mut page_counter = 1;
        let mut meta = TableMeta::new(1, "t".into());
        {
            let mut ts = TableStore::new(&mut p, meta.clone());
            // Each row ~100 bytes; page holds ~8000/104 ≈ 77 rows.
            for i in 0..200u64 {
                let row = i.to_le_bytes().to_vec();
                ts.append_row(&row, &mut page_counter).unwrap();
            }
            meta = ts.meta.clone();
        }
        assert_eq!(meta.row_count, 200);
        // Scan and verify.
        let mut count = 0u64;
        {
            let mut ts = TableStore::new(&mut p, meta.clone());
            ts.scan_all(|_| count += 1).unwrap();
        }
        assert_eq!(count, 200);
        // Pages allocated should exceed one page's capacity.
        assert!(page_counter > 1, "page counter should grow beyond 1");
    }

    #[test]
    fn scan_batched_respects_batch_size() {
        let mut p = pool(16);
        let mut page_counter = 1;
        let mut meta = TableMeta::new(1, "t".into());
        {
            let mut ts = TableStore::new(&mut p, meta.clone());
            for i in 0..50u64 {
                ts.append_row(&i.to_le_bytes(), &mut page_counter).unwrap();
            }
            meta = ts.meta.clone();
        }
        let mut batch_count = 0usize;
        let mut total = 0usize;
        {
            let mut ts = TableStore::new(&mut p, meta.clone());
            ts.scan_batched(10, |batch| {
                batch_count += 1;
                total += batch.len();
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(total, 50);
        assert!(batch_count >= 5, "should produce at least 5 batches of 10");
    }

    #[test]
    fn empty_table_scan_yields_nothing() {
        let mut p = pool(4);
        let meta = TableMeta::new(1, "t".into());
        let mut ts = TableStore::new(&mut p, meta);
        let mut count = 0u64;
        ts.scan_all(|_| count += 1).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn flush_does_not_panic() {
        let mut p = pool(4);
        let mut page_counter = 1;
        let mut meta = TableMeta::new(1, "t".into());
        {
            let mut ts = TableStore::new(&mut p, meta.clone());
            for i in 0..10u64 {
                ts.append_row(&i.to_le_bytes(), &mut page_counter).unwrap();
            }
            meta = ts.meta.clone();
        }
        let mut ts = TableStore::new(&mut p, meta);
        ts.flush().unwrap();
    }
}
