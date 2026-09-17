//! R3 — Storage manager: owns the buffer pool, table catalog, and coordinates
//! WAL ordering (WAL must be synced before dirty pages are flushed).

use crate::buffer::{BufferPool, PageStore};
use crate::error::Result;
use crate::page::PageId;
use crate::table_store::{RowIter, TableMeta, TableStore};

/// Database-level storage manager. Owns the buffer pool and table metadata.
///
/// # WAL ordering contract
///
/// The caller **must** sync the WAL *before* calling [`StorageManager::flush`].
/// This module does not own the WAL writer — that responsibility stays with
/// the SQL engine — but the ordering requirement is documented here and
/// enforced by the engine's insert path.
pub struct StorageManager<S: PageStore> {
    pool: BufferPool<S>,
    tables: std::collections::HashMap<String, TableMeta>,
    next_table_id: u64,
    /// Global page id counter — guarantees unique page ids across all tables.
    next_page_id: PageId,
}

impl<S: PageStore> StorageManager<S> {
    pub fn new(pool: BufferPool<S>) -> Self {
        Self {
            pool,
            tables: std::collections::HashMap::new(),
            next_table_id: 1,
            next_page_id: 1, // page 0 is reserved
        }
    }

    /// Create a new table in persistent storage.
    pub fn create_table(&mut self, name: &str) -> Result<()> {
        if self.tables.contains_key(name) {
            return Err(crate::error::Error::Other(format!(
                "table `{name}` already exists in storage"
            )));
        }
        let id = self.next_table_id;
        self.next_table_id += 1;
        self.tables
            .insert(name.to_string(), TableMeta::new(id, name.to_string()));
        Ok(())
    }

    /// Drop a table from storage (does not reclaim pages yet).
    pub fn drop_table(&mut self, name: &str) -> Result<()> {
        self.tables
            .remove(name)
            .ok_or_else(|| crate::error::Error::Other(format!("no table `{name}`")))?;
        Ok(())
    }

    /// Append a row (raw encoded bytes) to the named table.
    pub fn insert_row(&mut self, table: &str, row_bytes: &[u8]) -> Result<()> {
        let meta = self
            .tables
            .get(table)
            .ok_or_else(|| crate::error::Error::Other(format!("no table `{table}`")))?
            .clone();
        let mut ts = TableStore::new(&mut self.pool, meta);
        ts.append_row(row_bytes, &mut self.next_page_id)?;
        // Write back updated metadata.
        self.tables.insert(table.to_string(), ts.meta);
        Ok(())
    }

    /// Scan all rows from the named table.
    pub fn scan_all_rows<F: FnMut(&[u8])>(&mut self, table: &str, f: F) -> Result<u64> {
        let meta = self
            .tables
            .get(table)
            .ok_or_else(|| crate::error::Error::Other(format!("no table `{table}`")))?
            .clone();
        let mut ts = TableStore::new(&mut self.pool, meta);
        ts.scan_all(f)
    }

    /// Scan rows in bounded batches. The consumer is called for each batch.
    pub fn scan_batched<F: FnMut(Vec<Vec<u8>>) -> Result<()>>(
        &mut self,
        table: &str,
        batch_size: usize,
        consumer: F,
    ) -> Result<u64> {
        let meta = self
            .tables
            .get(table)
            .ok_or_else(|| crate::error::Error::Other(format!("no table `{table}`")))?
            .clone();
        let mut ts = TableStore::new(&mut self.pool, meta);
        ts.scan_batched(batch_size, consumer)
    }

    /// Row count for a table (O(1)).
    pub fn row_count(&self, table: &str) -> Result<u64> {
        self.tables
            .get(table)
            .map(|m| m.row_count)
            .ok_or_else(|| crate::error::Error::Other(format!("no table `{table}`")))
    }

    /// Check if a table exists.
    pub fn has_table(&self, table: &str) -> bool {
        self.tables.contains_key(table)
    }

    /// Return a list of table names.
    pub fn table_names(&self) -> Vec<String> {
        self.tables.keys().cloned().collect()
    }

    /// Produce a lazy cursor over a table's raw rows (R3.15). The cursor
    /// borrows the pool for its lifetime; rows are pulled with
    /// [`RowIter::next_row`] and memory stays bounded to one page.
    pub fn scan_rows(&mut self, table: &str) -> Result<RowIter<'_, S>> {
        let meta = self
            .tables
            .get(table)
            .ok_or_else(|| crate::error::Error::Other(format!("no table `{table}`")))?
            .clone();
        Ok(RowIter::new(&mut self.pool, meta))
    }

    /// Flush all dirty pages to disk.
    ///
    /// **Precondition**: the WAL must already be synced (fsynced) before this
    /// call. The engine is responsible for this ordering.
    pub fn flush(&mut self) -> Result<usize> {
        self.pool.flush_all()
    }

    /// Access the underlying buffer pool (for direct page operations).
    pub fn pool(&self) -> &BufferPool<S> {
        &self.pool
    }

    /// Access the underlying buffer pool mutably.
    pub fn pool_mut(&mut self) -> &mut BufferPool<S> {
        &mut self.pool
    }

    /// Get a mutable reference to a table's metadata.
    pub fn table_meta_mut(&mut self, name: &str) -> Option<&mut TableMeta> {
        self.tables.get_mut(name)
    }

    /// Get a reference to a table's metadata.
    pub fn table_meta(&self, name: &str) -> Option<&TableMeta> {
        self.tables.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::RamPageStore;

    fn mgr(cap: usize) -> StorageManager<RamPageStore> {
        StorageManager::new(BufferPool::new(RamPageStore::new(), cap))
    }

    #[test]
    fn create_insert_scan_roundtrip() {
        let mut sm = mgr(8);
        sm.create_table("users").unwrap();
        sm.insert_row("users", b"alice").unwrap();
        sm.insert_row("users", b"bob").unwrap();

        let mut rows = Vec::new();
        sm.scan_all_rows("users", |r| rows.push(r.to_vec()))
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], b"alice");
        assert_eq!(rows[1], b"bob");
        assert_eq!(sm.row_count("users").unwrap(), 2);
    }

    #[test]
    fn multiple_tables() {
        let mut sm = mgr(8);
        sm.create_table("a").unwrap();
        sm.create_table("b").unwrap();
        sm.insert_row("a", b"1").unwrap();
        sm.insert_row("b", b"2").unwrap();
        sm.insert_row("a", b"3").unwrap();

        assert_eq!(sm.row_count("a").unwrap(), 2);
        assert_eq!(sm.row_count("b").unwrap(), 1);
        let mut names = sm.table_names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn drop_table_removes_metadata() {
        let mut sm = mgr(4);
        sm.create_table("t").unwrap();
        sm.insert_row("t", b"x").unwrap();
        sm.drop_table("t").unwrap();
        assert!(!sm.has_table("t"));
    }

    #[test]
    fn scan_batched_works() {
        let mut sm = mgr(16);
        sm.create_table("t").unwrap();
        for i in 0..100u64 {
            sm.insert_row("t", &i.to_le_bytes()).unwrap();
        }
        let mut total = 0usize;
        sm.scan_batched("t", 10, |batch| {
            total += batch.len();
            Ok(())
        })
        .unwrap();
        assert_eq!(total, 100);
    }

    #[test]
    fn duplicate_create_table_errors() {
        let mut sm = mgr(4);
        sm.create_table("t").unwrap();
        assert!(sm.create_table("t").is_err());
    }

    #[test]
    fn insert_nonexistent_table_errors() {
        let mut sm = mgr(4);
        assert!(sm.insert_row("nope", b"x").is_err());
    }

    #[test]
    fn lazy_row_iter_pulls_in_order() {
        let mut sm = mgr(8);
        sm.create_table("t").unwrap();
        for i in 0..300u64 {
            sm.insert_row("t", &i.to_le_bytes()).unwrap();
        }
        let mut got = Vec::new();
        let mut iter = sm.scan_rows("t").unwrap();
        while let Some(row) = iter.next_row().unwrap() {
            got.push(u64::from_le_bytes(row.try_into().unwrap()));
        }
        // Rows must come back in insertion order across page boundaries.
        assert_eq!(got.len(), 300);
        for (i, v) in got.iter().enumerate() {
            assert_eq!(*v, i as u64);
        }
    }
}
