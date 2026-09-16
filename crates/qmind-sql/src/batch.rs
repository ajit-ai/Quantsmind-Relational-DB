//! R3.12 — Batch execution model (column-oriented).
//!
//! A `Batch` is `N` rows × `M` columns stored column-wise (`Vec<Vec<SqlValue>>`)
//! so an operator touches one column (one cache line run) instead of one heap
//! object per row.  Values are owned `SqlValue`s — the row-oriented type stays
//! as the conversion currency between the Volcano executor and the batch
//! pipeline (R3.14 adapters) and with the storage codec.
//!
//! Concepts (R3.12):
//! - `Batch`          — columnar N×M unit of execution
//! - `ColumnVector`   — one typed column (`Vec<SqlValue>`)
//! - `SelectionVector`— row indices surviving a predicate (lazy filtering)
//! - `NullBitmap`     — dense null mask (kept for the contract; nulls are also
//!   representable inline as `SqlValue::Null`)
//! - `RowCount`       — logical rows in a batch
//!
//! Memory lifecycle: batches are produced by scans in bounded sizes
//! (`batch_size` rows), flow through operators, and are dropped as soon as the
//! consumer pushes the next batch downstream.  `ResultStream` (result_stream.rs)
//! sits on top to bound the client-visible result set.

use crate::codec::{decode_row, encode_row, ColumnDef, SqlValue};
use crate::executor::Row;

/// Logical row counter / capacity bound (R3.13). Considered a provisional
/// default until a benchmark-backed choice replaces it (see R3.13).
pub const DEFAULT_BATCH_SIZE: usize = crate::BATCH_ROWS;

/// One typed column (R3.12 `ColumnVector`). Nulls are represented inline as
/// `SqlValue::Null`.
pub type ColumnVector = Vec<SqlValue>;

/// Dense null bitmap. `null_at(i)` is `true` when the row index `i` is NULL.
/// Values are also visible in the column vectors; the bitmap exists to keep
/// the R3.12 concept and to give later vectorized kernels a cheap null check.
#[derive(Debug, Clone, Default)]
pub struct NullBitmap {
    bits: Vec<u64>,
    len: usize,
}

impl NullBitmap {
    pub fn new(len: usize) -> Self {
        Self {
            bits: vec![0; len.div_ceil(64)],
            len,
        }
    }

    pub fn set_null(&mut self, i: usize) {
        if i < self.bits.len() * 64 {
            self.bits[i / 64] |= 1 << (i % 64);
        }
    }

    pub fn clear_null(&mut self, i: usize) {
        if i < self.bits.len() * 64 {
            self.bits[i / 64] &= !(1 << (i % 64));
        }
    }

    pub fn is_null(&self, i: usize) -> bool {
        i < self.len && (self.bits[i / 64] >> (i % 64)) & 1 == 1
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Lazy selection vector (R3.12): a subset of row indices (`[0, row_count)`)
/// that survive a predicate.  Allows filtering without copying rows; the
/// consumer materializes on demand via [`Batch::apply_selection`].
#[derive(Debug, Clone, Default)]
pub struct SelectionVector {
    indices: Vec<u32>,
}

impl SelectionVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, row_index: usize) {
        self.indices.push(row_index as u32);
    }

    pub fn len(&self) -> usize {
        self.indices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn as_slice(&self) -> &[u32] {
        &self.indices
    }

    pub fn into_vec(self) -> Vec<u32> {
        self.indices
    }
}

/// Column-oriented batch: `row_count` rows × `columns` column vectors.
#[derive(Debug, Clone, Default)]
pub struct Batch {
    columns: Vec<ColumnVector>,
    row_count: usize,
}

impl Batch {
    /// Empty batch with `ncols` columns.
    pub fn new(ncols: usize) -> Self {
        Self {
            columns: vec![Vec::new(); ncols],
            row_count: 0,
        }
    }

    /// Empty batch laid out for the given schema (one column per `ColumnDef`).
    pub fn from_schema(schema: &[ColumnDef]) -> Self {
        Self::new(schema.len())
    }

    /// Column-oriented batch pre-sized for `capacity` rows (still empty).
    pub fn with_capacity(ncols: usize, capacity: usize) -> Self {
        Self {
            columns: (0..ncols).map(|_| Vec::with_capacity(capacity)).collect(),
            row_count: 0,
        }
    }

    pub fn num_rows(&self) -> usize {
        self.row_count
    }

    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    pub fn is_full(&self, batch_size: usize) -> bool {
        self.row_count >= batch_size
    }

    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    pub fn column(&self, i: usize) -> &[SqlValue] {
        &self.columns[i]
    }

    pub fn column_mut(&mut self, i: usize) -> &mut ColumnVector {
        &mut self.columns[i]
    }

    /// Materialize one row from the batch (reconstructs a `Row`).
    pub fn row(&self, i: usize) -> Row {
        self.columns
            .iter()
            .map(|c| c.get(i).cloned().unwrap_or(SqlValue::Null))
            .collect()
    }

    /// Append a row-shaped value into the columnar layout.
    pub fn push_row(&mut self, row: &[SqlValue]) {
        debug_assert!(row.len() == self.columns.len());
        for (col, v) in self.columns.iter_mut().zip(row.iter()) {
            col.push(v.clone());
        }
        self.row_count += 1;
    }

    /// Convert the whole batch back into row representation.
    pub fn into_rows(self) -> Vec<Row> {
        (0..self.row_count).map(|i| self.row(i)).collect()
    }

    /// Materialize the selected rows into a fresh batch (drops the selection).
    /// Used by batch operators that need a concrete batch before pushing it
    /// downstream (e.g. into a hash join build side or a sort).
    pub fn apply_selection(&self, sel: &[u32]) -> Batch {
        let mut out = Batch::with_capacity(self.columns.len(), sel.len());
        for &i in sel {
            out.push_row(&self.row(i as usize));
        }
        out
    }

    /// Project (and reorder) columns. `indexes` must be within range.
    pub fn project(&self, indexes: &[usize]) -> Batch {
        let mut columns = Vec::with_capacity(indexes.len());
        for &i in indexes {
            columns.push(
                self.columns[i]
                    .iter()
                    .take(self.row_count)
                    .cloned()
                    .collect(),
            );
        }
        Batch {
            columns,
            row_count: self.row_count,
        }
    }

    /// Clear all rows, keeping column vectors (reused buffers avoid allocation
    /// churn across batches).
    pub fn clear(&mut self) {
        for c in &mut self.columns {
            c.clear();
        }
        self.row_count = 0;
    }

    /// Consume and return the underlying column vectors.
    pub fn into_columns(self) -> Vec<ColumnVector> {
        self.columns
    }

    /// Build a batch from column vectors (must be equal-length).
    pub fn from_columns(columns: Vec<ColumnVector>) -> Self {
        let row_count = columns.first().map(|c| c.len()).unwrap_or(0);
        Self { columns, row_count }
    }
}

// ── row ↔ batch adapters (R3.14) ───────────────────────────────────────

/// Row → batch. `rows` must be of equal length (validated by debug_assert).
pub fn rows_to_batch(rows: Vec<Row>) -> Batch {
    let ncols = rows.first().map(|r| r.len()).unwrap_or(0);
    let mut b = Batch::with_capacity(ncols, rows.len());
    for r in rows {
        b.push_row(&r);
    }
    b
}

/// Decode raw storage bytes → batch, respecting the table's column types.
/// Skips malformed records (returns partial batch rather than failing).
pub fn raw_rows_to_batch(raw: Vec<Vec<u8>>, schema: &[ColumnDef]) -> Batch {
    let mut b = Batch::from_schema(schema);
    for bytes in raw {
        if let Some(row) = decode_row(&bytes, schema) {
            b.push_row(&row);
        }
    }
    b
}

/// Batch → raw storage bytes (one `encode_row` result per row).
pub fn batch_to_raw_rows(batch: &Batch) -> Vec<Vec<u8>> {
    (0..batch.num_rows())
        .map(|i| encode_row(&batch.row(i)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Vec<ColumnDef> {
        vec![
            ColumnDef {
                name: "id".into(),
                ty: crate::codec::ColumnType::Int,
                nullable: false,
            },
            ColumnDef {
                name: "name".into(),
                ty: crate::codec::ColumnType::Text,
                nullable: true,
            },
        ]
    }

    #[test]
    fn batch_push_and_vectorized_read() {
        let mut b = Batch::with_capacity(2, 3);
        b.push_row(&[SqlValue::Int(1), SqlValue::Text("a".into())]);
        b.push_row(&[SqlValue::Int(2), SqlValue::Null]);
        b.push_row(&[SqlValue::Int(3), SqlValue::Text("c".into())]);
        assert_eq!(b.num_rows(), 3);
        // Column vector read (the vectorized fast path).
        assert_eq!(
            b.column(0),
            &[SqlValue::Int(1), SqlValue::Int(2), SqlValue::Int(3)]
        );
        // Row reconstruction (the compatibility path).
        assert_eq!(b.row(1), vec![SqlValue::Int(2), SqlValue::Null]);
    }

    #[test]
    fn batch_clear_reuses_buffers() {
        let mut b = Batch::with_capacity(1, 10);
        for i in 0..5 {
            b.push_row(&[SqlValue::Int(i)]);
        }
        b.clear();
        assert_eq!(b.num_rows(), 0);
        assert_eq!(b.column_mut(0).capacity(), 10, "buffers reused after clear");
    }

    #[test]
    fn selection_vector_materialization() {
        let mut b = Batch::new(2);
        b.push_row(&[SqlValue::Int(1), SqlValue::Text("x".into())]);
        b.push_row(&[SqlValue::Int(2), SqlValue::Text("y".into())]);
        b.push_row(&[SqlValue::Int(3), SqlValue::Text("z".into())]);

        let mut sel = SelectionVector::new();
        sel.push(0);
        sel.push(2);
        let filtered = b.apply_selection(sel.as_slice());
        assert_eq!(filtered.num_rows(), 2);
        assert_eq!(
            filtered.row(0),
            vec![SqlValue::Int(1), SqlValue::Text("x".into())]
        );
        assert_eq!(
            filtered.row(1),
            vec![SqlValue::Int(3), SqlValue::Text("z".into())]
        );
    }

    #[test]
    fn null_bitmap_roundtrip() {
        let mut n = NullBitmap::new(200);
        n.set_null(3);
        n.set_null(127);
        n.set_null(128);
        assert!(n.is_null(3));
        assert!(n.is_null(127));
        assert!(n.is_null(128));
        assert!(!n.is_null(4));
        n.clear_null(127);
        assert!(!n.is_null(127));
        assert_eq!(n.len(), 200);
    }

    #[test]
    fn projection_selects_and_reorders() {
        let mut b = Batch::new(3);
        b.push_row(&[
            SqlValue::Int(1),
            SqlValue::Int(10),
            SqlValue::Text("a".into()),
        ]);
        b.push_row(&[
            SqlValue::Int(2),
            SqlValue::Int(20),
            SqlValue::Text("b".into()),
        ]);
        let p = b.project(&[2, 0]);
        assert_eq!(p.num_columns(), 2);
        assert_eq!(
            p.column(0),
            &[SqlValue::Text("a".into()), SqlValue::Text("b".into())]
        );
        assert_eq!(p.column(1), &[SqlValue::Int(1), SqlValue::Int(2)]);
    }

    #[test]
    fn raw_roundtrip_with_corrupt_row_skipped() {
        let rows: Vec<Row> = vec![
            vec![SqlValue::Int(1), SqlValue::Text("a".into())],
            vec![SqlValue::Int(2), SqlValue::Null],
        ];
        let b = rows_to_batch(rows.clone());
        let mut raw = batch_to_raw_rows(&b);
        // Corrupt the second record.
        raw[1].truncate(3);
        let back = raw_rows_to_batch(raw, &schema());
        assert_eq!(back.num_rows(), 1);
        assert_eq!(back.row(0), rows[0]);
    }

    #[test]
    fn rows_to_batch_roundtrip() {
        let rows: Vec<Row> = vec![
            vec![SqlValue::Int(1), SqlValue::Text("x".into())],
            vec![SqlValue::Int(2), SqlValue::Text("y".into())],
        ];
        let b = rows_to_batch(rows.clone());
        let back = b.into_rows();
        assert_eq!(back, rows);
    }
}
