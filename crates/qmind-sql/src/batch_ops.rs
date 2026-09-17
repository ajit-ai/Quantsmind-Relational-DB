//! R3.15–R3.20 — Batch-to-batch operators (Volcano-compatible batch pipeline).
//!
//! Every operator implements [`BatchOperator`]:
//!
//! ```text
//! next_batch(batch_size) -> Option<Batch>
//! ```
//!
//! Operators form a tree: a source (scan) feeds into filter/project/agg/join,
//! and the engine pulls the top operator until exhausted.
//!
//! Memory is bounded by `batch_size` rows per pull.  Large-scale aggregation
//! and sort spill: **DEFERRED** (R3 boundary — see BATCH_EXECUTION.md §External
//! Spill).

use crate::batch::{rows_to_batch, Batch, SelectionVector, DEFAULT_BATCH_SIZE};
use crate::codec::SqlValue;
use crate::codec::{decode_row, ColumnDef};
use crate::executor::{AggFn, Row};
use std::collections::HashMap;

pub trait BatchOperator {
    /// Pull the next batch of up to `batch_size` rows.  Returns `Ok(None)`
    /// when the input is exhausted.
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String>;
}

/// Default batch size helper.
pub fn default_batch_size() -> usize {
    DEFAULT_BATCH_SIZE
}

// ── Source operators ────────────────────────────────────────────────────

/// Column-oriented scan: the source closure yields raw storage bytes
/// directly.  Decodes into a `Batch` without constructing intermediate
/// `Row` objects (the hot path for persistent storage scans).
pub struct BatchScanRaw<F> {
    f: F,
    schema: Vec<ColumnDef>,
}

impl<F> BatchScanRaw<F>
where
    F: FnMut() -> Result<Option<Vec<u8>>, String>,
{
    pub fn new(f: F, schema: Vec<ColumnDef>) -> Self {
        Self { f, schema }
    }
}

impl<F> BatchOperator for BatchScanRaw<F>
where
    F: FnMut() -> Result<Option<Vec<u8>>, String>,
{
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        let mut batch = Batch::with_capacity(self.schema.len(), batch_size);
        for _ in 0..batch_size {
            match (self.f)()? {
                Some(raw) => {
                    if let Some(row) = decode_row(&raw, &self.schema) {
                        batch.push_row(&row);
                    }
                }
                None => break,
            }
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
    }
}

/// Row-oriented scan: the source closure yields `Row`s.  Produces batches
/// without materializing the full input.
pub struct BatchRowScan<F> {
    f: F,
}

impl<F> BatchRowScan<F>
where
    F: FnMut() -> Result<Option<Row>, String>,
{
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F> BatchOperator for BatchRowScan<F>
where
    F: FnMut() -> Result<Option<Row>, String>,
{
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        let mut rows: Vec<Row> = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            match (self.f)()? {
                Some(r) => rows.push(r),
                None => break,
            }
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(rows_to_batch(rows)))
        }
    }
}

/// Pre-materialized row scan (for tests and small inputs).
pub struct BatchVecScan {
    inner: std::vec::IntoIter<Row>,
}

impl BatchVecScan {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            inner: rows.into_iter(),
        }
    }
}

impl BatchOperator for BatchVecScan {
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        let mut rows: Vec<Row> = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            match self.inner.next() {
                Some(r) => rows.push(r),
                None => break,
            }
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(rows_to_batch(rows)))
        }
    }
}

// ── Filter (R3.16) ─────────────────────────────────────────────────────

/// Batch filter: evaluates a predicate row-by-row and materializes the
/// surviving rows.  Selection vector is applied inline (no intermediate
/// batch layout).
pub struct BatchFilter<F> {
    input: Box<dyn BatchOperator>,
    pred: F,
}

impl<F> BatchFilter<F>
where
    F: Fn(&Row) -> Result<bool, String>,
{
    pub fn new(input: Box<dyn BatchOperator>, pred: F) -> Self {
        Self { input, pred }
    }
}

impl<F> BatchOperator for BatchFilter<F>
where
    F: Fn(&Row) -> Result<bool, String>,
{
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        loop {
            let Some(input_batch) = self.input.next_batch(batch_size)? else {
                return Ok(None);
            };
            let mut sel = SelectionVector::new();
            for i in 0..input_batch.num_rows() {
                let row = input_batch.row(i);
                if (self.pred)(&row)? {
                    sel.push(i);
                }
            }
            if sel.is_empty() {
                continue; // skip empty output, try next batch
            }
            return Ok(Some(input_batch.apply_selection(sel.as_slice())));
        }
    }
}

// ── Projection (R3.17) ─────────────────────────────────────────────────

/// Column-index based projection (reorder / prune).
pub struct BatchProjectIdx {
    input: Box<dyn BatchOperator>,
    indices: Vec<usize>,
}

impl BatchProjectIdx {
    pub fn new(input: Box<dyn BatchOperator>, indices: Vec<usize>) -> Self {
        Self { input, indices }
    }
}

impl BatchOperator for BatchProjectIdx {
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        match self.input.next_batch(batch_size)? {
            None => Ok(None),
            Some(batch) => Ok(Some(batch.project(&self.indices))),
        }
    }
}

/// Expression-based projection: applies a row-level transform.
/// The transform closure receives a row and returns the projected output.
pub struct BatchEvalProject<F> {
    input: Box<dyn BatchOperator>,
    transform: F,
}

impl<F> BatchEvalProject<F>
where
    F: Fn(&Row) -> Result<Row, String>,
{
    pub fn new(input: Box<dyn BatchOperator>, transform: F) -> Self {
        Self { input, transform }
    }
}

impl<F> BatchOperator for BatchEvalProject<F>
where
    F: Fn(&Row) -> Result<Row, String>,
{
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        match self.input.next_batch(batch_size)? {
            None => Ok(None),
            Some(batch) => {
                let mut out = Vec::with_capacity(batch.num_rows());
                for i in 0..batch.num_rows() {
                    out.push((self.transform)(&batch.row(i))?);
                }
                Ok(Some(rows_to_batch(out)))
            }
        }
    }
}

// ── Aggregation (R3.18) ────────────────────────────────────────────────

/// Incremental aggregation state for one aggregate function.
#[derive(Debug, Clone)]
enum AggState {
    Count {
        counted: i64,
        counted_not_null: bool,
    },
    Sum {
        total: i64,
        empty: bool,
    },
    Avg {
        total: i64,
        count: i64,
    },
    Min {
        val: Option<SqlValue>,
    },
    Max {
        val: Option<SqlValue>,
    },
}

impl AggState {
    fn new(func: AggFn) -> Self {
        match func {
            AggFn::Count => AggState::Count {
                counted: 0,
                counted_not_null: false,
            },
            AggFn::Sum => AggState::Sum {
                total: 0,
                empty: true,
            },
            AggFn::Avg => AggState::Avg { total: 0, count: 0 },
            AggFn::Min => AggState::Min { val: None },
            AggFn::Max => AggState::Max { val: None },
        }
    }

    fn feed(&mut self, v: &SqlValue, is_count_star: bool) {
        match self {
            AggState::Count {
                counted,
                counted_not_null,
            } => {
                if is_count_star || *v != SqlValue::Null {
                    *counted += 1;
                    if !is_count_star {
                        *counted_not_null = true;
                    }
                }
            }
            AggState::Sum { total, empty } => {
                if let SqlValue::Int(i) = v {
                    *total += i;
                    *empty = false;
                }
            }
            AggState::Avg { total, count } => {
                if let SqlValue::Int(i) = v {
                    *total += i;
                    *count += 1;
                }
            }
            AggState::Min { val } => {
                if *v != SqlValue::Null {
                    match val {
                        None => *val = Some(v.clone()),
                        Some(ref mut best) => {
                            if let (SqlValue::Int(b), SqlValue::Int(n)) = (&*best, v) {
                                if n < b {
                                    *best = SqlValue::Int(*n);
                                }
                            }
                        }
                    }
                }
            }
            AggState::Max { val } => {
                if *v != SqlValue::Null {
                    match val {
                        None => *val = Some(v.clone()),
                        Some(ref mut best) => {
                            if let (SqlValue::Int(b), SqlValue::Int(n)) = (&*best, v) {
                                if n > b {
                                    *best = SqlValue::Int(*n);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn finish(self) -> SqlValue {
        match self {
            AggState::Count { counted, .. } => SqlValue::Int(counted),
            AggState::Sum { total, empty } => {
                if empty {
                    SqlValue::Null
                } else {
                    SqlValue::Int(total)
                }
            }
            AggState::Avg { total, count } => {
                if count == 0 {
                    SqlValue::Null
                } else {
                    SqlValue::Int(total / count)
                }
            }
            AggState::Min { val } | AggState::Max { val } => val.unwrap_or(SqlValue::Null),
        }
    }
}

/// GROUP BY aggregate operator.  Materializes groups on first pull (in a
/// `BTreeMap` for key ordering).  Output is one batch.
///
/// Spill-to-disk for large cardinality: **DEFERRED** — documented in
/// BATCH_EXECUTION.md.
pub struct BatchAggregate {
    input: Box<dyn BatchOperator>,
    /// Indexes of the GROUP BY key columns in the input schema.
    group_idx: Vec<usize>,
    /// Aggregate descriptors: `(agg_fn, optional_col_index)`.
    aggs: Vec<(AggFn, Option<usize>)>,
    /// Buffered output rows, populated on first pull.
    output: Option<std::vec::IntoIter<Row>>,
    /// Buffered input columns: `(group_key, agg_col_values)` per row.
    input_buf: Vec<(Row, Vec<SqlValue>)>,
}

impl BatchAggregate {
    pub fn new(
        input: Box<dyn BatchOperator>,
        group_idx: Vec<usize>,
        aggs: Vec<(AggFn, Option<usize>)>,
    ) -> Self {
        Self {
            input,
            group_idx,
            aggs,
            output: None,
            input_buf: Vec::new(),
        }
    }

    fn materialize(&mut self) -> Result<(), String> {
        let batch_size = default_batch_size();
        // Accumulate all rows.
        while let Some(batch) = self.input.next_batch(batch_size)? {
            for i in 0..batch.num_rows() {
                let row = batch.row(i);
                let key: Row = self.group_idx.iter().map(|&c| row[c].clone()).collect();
                self.input_buf.push((key, row));
            }
        }
        // Build groups.
        let mut groups: std::collections::BTreeMap<Vec<SqlValue>, Vec<&Row>> =
            std::collections::BTreeMap::new();
        for (key, row) in &self.input_buf {
            let k: Vec<SqlValue> = key.clone();
            groups.entry(k).or_default().push(row);
        }
        let mut output_rows: Vec<Row> = Vec::with_capacity(groups.len());
        for (key, bucket) in groups {
            let mut out = key;
            for (func, col) in &self.aggs {
                if *func == AggFn::Count && col.is_none() {
                    out.push(SqlValue::Int(bucket.len() as i64));
                    continue;
                }
                let mut state = AggState::new(*func);
                if let Some(idx) = col {
                    for row in bucket.iter() {
                        state.feed(&row[*idx], false);
                    }
                } else {
                    // COUNT(*) fallback
                    state.feed(&SqlValue::Null, true);
                }
                out.push(state.finish());
            }
            output_rows.push(out);
        }
        self.output = Some(output_rows.into_iter());
        self.input_buf.clear();
        Ok(())
    }
}

impl BatchOperator for BatchAggregate {
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        if self.output.is_none() {
            self.materialize()?;
        }
        let iter = self.output.as_mut().expect("materialized");
        let mut rows: Vec<Row> = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            match iter.next() {
                Some(r) => rows.push(r),
                None => break,
            }
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(rows_to_batch(rows)))
        }
    }
}

// ── Sort (R3.19) ───────────────────────────────────────────────────────

/// Materializing sort.  All input is buffered on first pull, then
/// emitted in sorted batches.
///
/// External merge sort: **DEFERRED** — see BATCH_EXECUTION.md.
pub struct BatchSort<F>
where
    F: Fn(&Row) -> Result<Vec<SqlValue>, String>,
{
    input: Box<dyn BatchOperator>,
    key_fn: F,
    desc: Vec<bool>,
    output: Option<std::vec::IntoIter<Row>>,
}

impl<F> BatchSort<F>
where
    F: Fn(&Row) -> Result<Vec<SqlValue>, String>,
{
    pub fn new(input: Box<dyn BatchOperator>, desc: Vec<bool>, key_fn: F) -> Self {
        Self {
            input,
            key_fn,
            desc,
            output: None,
        }
    }

    fn materialize(&mut self) -> Result<(), String> {
        let batch_size = default_batch_size();
        let mut keyed: Vec<(Vec<SqlValue>, Row)> = Vec::new();
        while let Some(batch) = self.input.next_batch(batch_size)? {
            for i in 0..batch.num_rows() {
                let row = batch.row(i);
                let keys = (self.key_fn)(&row)?;
                keyed.push((keys, row));
            }
        }
        keyed.sort_by(|a, b| {
            use crate::codec::total_cmp;
            for (i, (x, y)) in a.0.iter().zip(b.0.iter()).enumerate() {
                let ord = total_cmp(x, y);
                let ord = if self.desc.get(i).copied().unwrap_or(false) {
                    ord.reverse()
                } else {
                    ord
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
        self.output = Some(
            keyed
                .into_iter()
                .map(|(_, r)| r)
                .collect::<Vec<_>>()
                .into_iter(),
        );
        Ok(())
    }
}

impl<F> BatchOperator for BatchSort<F>
where
    F: Fn(&Row) -> Result<Vec<SqlValue>, String>,
{
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        if self.output.is_none() {
            self.materialize()?;
        }
        let iter = self.output.as_mut().expect("materialized");
        let mut rows: Vec<Row> = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            match iter.next() {
                Some(r) => rows.push(r),
                None => break,
            }
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(rows_to_batch(rows)))
        }
    }
}

// ── Hash Join (R3.20) ──────────────────────────────────────────────────

/// Hash join: materializes the right (build) side, probes with left (probe).
///
/// Spill-to-disk: **DEFERRED** — see BATCH_EXECUTION.md.
pub struct BatchHashJoin {
    left: Box<dyn BatchOperator>,
    hash: HashMap<SqlValue, Vec<Row>>,
    pending: Vec<Row>,
    lkey: usize,
    lcols: usize,
    probe_peek: Option<Batch>,
    batch_size: usize,
}

impl BatchHashJoin {
    pub fn new(
        left: Box<dyn BatchOperator>,
        mut right: Box<dyn BatchOperator>,
        lkey: usize,
        rkey: usize,
        lcols: usize,
    ) -> Result<Self, String> {
        let batch_size = default_batch_size();
        let mut hash: HashMap<SqlValue, Vec<Row>> = HashMap::new();
        while let Some(batch) = right.next_batch(batch_size)? {
            for i in 0..batch.num_rows() {
                let row = batch.row(i);
                if row[rkey] == SqlValue::Null {
                    continue;
                }
                hash.entry(row[rkey].clone()).or_default().push(row);
            }
        }
        Ok(Self {
            left,
            hash,
            pending: Vec::new(),
            lkey,
            lcols,
            probe_peek: None,
            batch_size,
        })
    }
}

impl BatchOperator for BatchHashJoin {
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        let mut out_rows: Vec<Row> = Vec::with_capacity(batch_size);
        loop {
            // Drain pending matched rows first.
            while out_rows.len() < batch_size {
                if let Some(r) = self.pending.pop() {
                    out_rows.push(r);
                } else {
                    break;
                }
            }
            if out_rows.len() >= batch_size {
                return Ok(Some(rows_to_batch(out_rows)));
            }
            // Pull next probe batch (peek / advance).
            let batch = match self.probe_peek.take() {
                Some(b) => Some(b),
                None => self.left.next_batch(self.batch_size)?,
            };
            let Some(batch) = batch else {
                return if out_rows.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(rows_to_batch(out_rows)))
                };
            };
            for i in 0..batch.num_rows() {
                let lrow = batch.row(i);
                if lrow[self.lkey] == SqlValue::Null {
                    continue;
                }
                if let Some(matches) = self.hash.get(&lrow[self.lkey]) {
                    for rrow in matches {
                        let mut out = lrow.clone();
                        out.extend(rrow.iter().cloned());
                        self.pending.push(out);
                    }
                    self.pending.reverse();
                }
                // If batch full after matches, put remaining probe batch aside.
                if out_rows.len() + self.pending.len() >= batch_size {
                    // Save unprocessed rows from this batch into pending for next call.
                    // For simplicity just continue processing; remaining will pick up next call.
                }
            }
            let _ = self.lcols;
            let _ = &self.probe_peek;
        }
    }
}

// ── Limit (trivial) ────────────────────────────────────────────────────

/// Batch limit: passes through at most N total rows across batches.
pub struct BatchLimit {
    input: Box<dyn BatchOperator>,
    remaining: usize,
}

impl BatchLimit {
    pub fn new(input: Box<dyn BatchOperator>, n: usize) -> Self {
        Self {
            input,
            remaining: n,
        }
    }
}

impl BatchOperator for BatchLimit {
    fn next_batch(&mut self, batch_size: usize) -> Result<Option<Batch>, String> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let take = batch_size.min(self.remaining);
        match self.input.next_batch(take)? {
            None => Ok(None),
            Some(batch) => {
                let n = batch.num_rows().min(take);
                self.remaining -= n;
                // Truncate the batch if it has more rows than requested.
                if n < batch.num_rows() {
                    let mut rows: Vec<Row> = Vec::with_capacity(n);
                    for i in 0..n {
                        rows.push(batch.row(i));
                    }
                    Ok(Some(rows_to_batch(rows)))
                } else {
                    Ok(Some(batch))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{encode_row, ColumnDef, ColumnType};

    fn int_schema() -> Vec<ColumnDef> {
        vec![ColumnDef {
            name: "v".into(),
            ty: ColumnType::Int,
            nullable: false,
        }]
    }

    fn rows(n: u64) -> Vec<Row> {
        (0..n).map(|i| vec![SqlValue::Int(i as i64)]).collect()
    }

    #[test]
    fn vec_scan_batches_exact() {
        let mut op = BatchVecScan::new(rows(10));
        let b = op.next_batch(3).unwrap().unwrap();
        assert_eq!(b.num_rows(), 3);
        let b = op.next_batch(3).unwrap().unwrap();
        assert_eq!(b.num_rows(), 3);
        let b = op.next_batch(3).unwrap().unwrap();
        assert_eq!(b.num_rows(), 3);
        let b = op.next_batch(3).unwrap().unwrap();
        assert_eq!(b.num_rows(), 1, "remainder");
        assert!(op.next_batch(3).unwrap().is_none());
    }

    #[test]
    fn raw_scan_decodes_directly() {
        let raws: Vec<Vec<u8>> = rows(4).into_iter().map(|r| encode_row(&r)).collect();
        let mut idx = 0;
        let schema = int_schema();
        let mut op = BatchScanRaw::new(
            move || {
                if idx < raws.len() {
                    let r = raws[idx].clone();
                    idx += 1;
                    Ok(Some(r))
                } else {
                    Ok(None)
                }
            },
            schema,
        );
        let b = op.next_batch(10).unwrap().unwrap();
        assert_eq!(b.num_rows(), 4);
        assert_eq!(
            b.column(0),
            &[
                SqlValue::Int(0),
                SqlValue::Int(1),
                SqlValue::Int(2),
                SqlValue::Int(3),
            ]
        );
    }

    #[test]
    fn filter_keeps_matching_rows() {
        let mut op = BatchFilter::new(Box::new(BatchVecScan::new(rows(6))), |r| {
            Ok(r[0] != SqlValue::Null && r[0] != SqlValue::Int(0) && r[0] != SqlValue::Int(3))
        });
        let b = op.next_batch(100).unwrap().unwrap();
        assert_eq!(b.num_rows(), 4);
        assert_eq!(b.row(0), vec![SqlValue::Int(1)]);
        assert_eq!(b.row(3), vec![SqlValue::Int(5)]);
    }

    #[test]
    fn project_reorders_and_prunes() {
        let input_rows: Vec<Row> = vec![
            vec![
                SqlValue::Int(1),
                SqlValue::Text("a".into()),
                SqlValue::Int(10),
            ],
            vec![
                SqlValue::Int(2),
                SqlValue::Text("b".into()),
                SqlValue::Int(20),
            ],
        ];
        let mut op = BatchProjectIdx::new(Box::new(BatchVecScan::new(input_rows)), vec![2, 0]);
        let b = op.next_batch(10).unwrap().unwrap();
        assert_eq!(b.num_columns(), 2);
        assert_eq!(b.column(0), &[SqlValue::Int(10), SqlValue::Int(20)]);
        assert_eq!(b.column(1), &[SqlValue::Int(1), SqlValue::Int(2)]);
    }

    #[test]
    fn aggregate_count_star() {
        let input_rows: Vec<Row> = rows(10);
        let mut op = BatchAggregate::new(
            Box::new(BatchVecScan::new(input_rows)),
            vec![],
            vec![(AggFn::Count, None)],
        );
        let b = op.next_batch(100).unwrap().unwrap();
        assert_eq!(b.num_rows(), 1);
        assert_eq!(b.row(0), vec![SqlValue::Int(10)]);
    }

    #[test]
    fn aggregate_group_sum() {
        // group by val%2, sum val
        let input_rows: Vec<Row> = (0..6u64)
            .map(|i| vec![SqlValue::Int(i as i64), SqlValue::Int(i as i64)])
            .collect();
        let mut op = BatchAggregate::new(
            Box::new(BatchVecScan::new(input_rows)),
            vec![1], // group by column 1
            vec![(AggFn::Sum, Some(0))],
        );
        let b = op.next_batch(100).unwrap().unwrap();
        assert_eq!(b.num_rows(), 6);
        // Sum per group: each group has one row, so sum == val
        let row0 = b.row(0);
        assert_eq!(row0[0], SqlValue::Int(0));
        assert_eq!(row0[1], SqlValue::Int(0));
    }

    #[test]
    fn sort_orders_rows() {
        let input_rows: Vec<Row> = rows(5).into_iter().rev().collect();
        let mut op = BatchSort::new(
            Box::new(BatchVecScan::new(input_rows)),
            vec![false], // ASC
            |r| Ok(vec![r[0].clone()]),
        );
        let mut all = Vec::new();
        while let Some(b) = op.next_batch(100).unwrap() {
            all.extend(b.into_rows());
        }
        assert_eq!(all.len(), 5);
        assert_eq!(all[0][0], SqlValue::Int(0));
        assert_eq!(all[4][0], SqlValue::Int(4));
    }

    #[test]
    fn hash_join_matches_correctly() {
        let left: Vec<Row> = vec![
            vec![SqlValue::Int(1), SqlValue::Text("a".into())],
            vec![SqlValue::Int(2), SqlValue::Text("b".into())],
        ];
        let right: Vec<Row> = vec![
            vec![SqlValue::Int(1), SqlValue::Int(100)],
            vec![SqlValue::Int(2), SqlValue::Int(200)],
            vec![SqlValue::Int(3), SqlValue::Int(300)], // no match
        ];
        let mut op = BatchHashJoin::new(
            Box::new(BatchVecScan::new(left)),
            Box::new(BatchVecScan::new(right)),
            0,
            0,
            2,
        )
        .unwrap();
        let mut out = Vec::new();
        while let Some(b) = op.next_batch(10).unwrap() {
            out.extend(b.into_rows());
        }
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            vec![
                SqlValue::Int(1),
                SqlValue::Text("a".into()),
                SqlValue::Int(1),
                SqlValue::Int(100)
            ]
        );
    }

    #[test]
    fn limit_caps_rows() {
        let mut op = BatchLimit::new(Box::new(BatchVecScan::new(rows(20))), 7);
        let mut all = Vec::new();
        while let Some(b) = op.next_batch(10).unwrap() {
            all.extend(b.into_rows());
        }
        assert_eq!(all.len(), 7);
    }
}
