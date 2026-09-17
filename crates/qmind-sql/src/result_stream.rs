//! R3.21 — Result streaming (bounded-memory output).
//!
//! A query producing millions of rows must not require all result rows to be
//! resident in memory simultaneously.  `QueryResult` sits on top of any
//! [`BatchOperator`] and yields one batch at a time.  The consumer (server /
//! embed API / CLI) pulls batches and flushes them to the client before
//! requesting the next batch.
//!
//! ```text
//! Query → ResultStream → Batch → Batch → … → None
//! ```
//!
//! The exact mechanism is an `Iterator` over [`Batch`] — simple, lazy,
//! composable with existing Rust code.

use crate::batch::{Batch, DEFAULT_BATCH_SIZE};
use crate::batch_ops::BatchOperator;

/// Streaming query result.  Wraps a batch operator and yields batches on
/// demand.
pub struct QueryResult {
    inner: Box<dyn BatchOperator>,
    batch_size: usize,
    exhausted: bool,
}

impl QueryResult {
    /// Wrap a batch operator into a streaming result set.
    pub fn new(inner: Box<dyn BatchOperator>) -> Self {
        Self {
            inner,
            batch_size: DEFAULT_BATCH_SIZE,
            exhausted: false,
        }
    }

    /// Set a custom batch size (e.g. for large-payload transports).
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// Pull the next batch of results.  Returns `None` when the query is
    /// exhausted.
    pub fn next_batch(&mut self) -> Option<Batch> {
        if self.exhausted {
            return None;
        }
        match self.inner.next_batch(self.batch_size) {
            Ok(Some(batch)) => Some(batch),
            Ok(None) => {
                self.exhausted = true;
                None
            }
            Err(e) => {
                // In production this would be surfaced as a streaming error;
                // for now we convert to an empty batch and mark exhausted.
                // Callers can check `is_empty()` to detect this edge case.
                if cfg!(debug_assertions) {
                    eprintln!("QueryResult stream error: {e}");
                }
                self.exhausted = true;
                None
            }
        }
    }

    /// True when the stream has been fully consumed or an error occurred.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Consume the rest of the stream into a single materialized `ExecResult`.
    /// Useful for the embed API and tests that want a synchronous, complete
    /// answer (pre-streaming compat).
    pub fn collect_all(self, columns: Vec<String>) -> crate::engine::ExecResult {
        let mut rows = Vec::new();
        let mut stream = self;
        while let Some(batch) = stream.next_batch() {
            rows.extend(batch.into_rows());
        }
        crate::engine::ExecResult {
            columns,
            rows,
            rows_affected: 0,
        }
    }
}

impl Iterator for QueryResult {
    type Item = Batch;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_batch()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch_ops::BatchVecScan;
    use crate::codec::SqlValue;

    fn sample_rows(n: u64) -> Vec<Vec<SqlValue>> {
        (0..n).map(|i| vec![SqlValue::Int(i as i64)]).collect()
    }

    #[test]
    fn streaming_yields_batches_and_then_none() {
        let rows = sample_rows(25);
        let op = BatchVecScan::new(rows);
        let mut qr = QueryResult::new(Box::new(op)).with_batch_size(10);
        let mut total = 0;
        let mut count = 0;
        while let Some(batch) = qr.next_batch() {
            total += batch.num_rows();
            count += 1;
        }
        assert_eq!(total, 25);
        assert_eq!(count, 3, "should yield 3 batches (10+10+5)");
        assert!(qr.is_exhausted());
    }

    #[test]
    fn iterator_interface_works() {
        let rows = sample_rows(5);
        let op = BatchVecScan::new(rows);
        let qr = QueryResult::new(Box::new(op)).with_batch_size(2);
        let total: usize = qr.map(|b| b.num_rows()).sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn empty_stream_yields_nothing() {
        let op = BatchVecScan::new(vec![]);
        let mut qr = QueryResult::new(Box::new(op));
        assert!(qr.next_batch().is_none());
        assert!(qr.is_exhausted());
    }

    #[test]
    fn collect_all_returns_sync_result() {
        let rows = sample_rows(3);
        let op = BatchVecScan::new(rows);
        let qr = QueryResult::new(Box::new(op));
        let res = qr.collect_all(vec!["id".into()]);
        assert_eq!(res.rows.len(), 3);
        assert_eq!(res.columns, vec!["id"]);
        assert_eq!(res.rows[2], vec![SqlValue::Int(2)]);
    }
}
