//! Delta capture + async apply: WAL → columnar segments (M8b).
//!
//! After a transaction commits, its key-value pairs are captured and
//! periodically flushed into columnar segment files. This enables
//! OLAP queries to read from compact columnar storage without blocking
//! OLTP writes to the row store.

use crate::columnar::{ColumnSegmentBuilder, ColValue};
use crate::error::Result;
use crate::wal::{Lsn, TxnId, WalRecord};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// ── Delta row ──────────────────────────────────────────────────────────────

/// A committed row captured from WAL Put records.
#[derive(Debug, Clone)]
pub struct DeltaRow {
    pub txn: TxnId,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

// ── Schema mapping ─────────────────────────────────────────────────────────

/// Maps table name + column names to the key-encoding scheme used by the
/// row store. This enables translating row-store KV pairs back into
/// column-oriented layout.
#[derive(Debug, Clone)]
pub struct TableSchema {
    pub table_name: String,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub col_type: ColumnDataType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnDataType {
    Int,
    Text,
}

// ── Delta buffer ───────────────────────────────────────────────────────────

/// Accumulates committed rows and flushes them to columnar segments.
pub struct DeltaBuffer {
    table: TableSchema,
    buffer: Vec<Vec<ColValue>>,
    flush_threshold: usize,
    segment_dir: PathBuf,
    next_segment_id: u64,
    last_applied_lsn: Lsn,
}

impl DeltaBuffer {
    pub fn new(table: TableSchema, segment_dir: PathBuf, flush_threshold: usize) -> Self {
        let next_segment_id = count_existing_segments(&segment_dir);
        Self {
            table,
            buffer: Vec::new(),
            flush_threshold,
            segment_dir,
            next_segment_id,
            last_applied_lsn: 0,
        }
    }

    /// Current LSN watermark (all WAL records up to here are applied).
    pub fn last_applied_lsn(&self) -> Lsn {
        self.last_applied_lsn
    }

    /// Set the applied LSN watermark.
    pub fn set_last_applied_lsn(&mut self, lsn: Lsn) {
        self.last_applied_lsn = lsn;
    }

    /// Number of rows buffered (not yet flushed).
    pub fn buffered_rows(&self) -> usize {
        self.buffer.len()
    }

    /// Append a row of column values to the buffer.
    pub fn append_row(&mut self, values: Vec<ColValue>) {
        self.buffer.push(values);
    }

    /// True when the buffer has reached the flush threshold.
    pub fn should_flush(&self) -> bool {
        self.buffer.len() >= self.flush_threshold
    }

    /// Flush buffered rows to a new columnar segment file.
    /// Returns the number of rows flushed.
    pub fn flush(&mut self) -> Result<usize> {
        if self.buffer.is_empty() {
            return Ok(0);
        }

        let num_rows = self.buffer.len() as u32;
        let num_cols = self.table.columns.len() as u32;
        let mut builder = ColumnSegmentBuilder::new(num_cols, num_rows);

        // Transpose row-oriented buffer into column-oriented arrays.
        for col_idx in 0..self.table.columns.len() {
            let col = &self.table.columns[col_idx];
            match col.col_type {
                ColumnDataType::Int => {
                    let vals: Vec<Option<i64>> = self.buffer.iter().map(|row| {
                        row.get(col_idx).and_then(|v| match v {
                            ColValue::Int(n) => Some(*n),
                            ColValue::Null => None,
                            _ => None,
                        })
                    }).collect();

                    let unique_ints: std::collections::HashSet<i64> =
                        vals.iter().filter_map(|v| *v).collect();
                    if unique_ints.len() <= vals.len() / 4 && unique_ints.len() <= 64 {
                        builder.push_int_rle(&vals);
                    } else {
                        builder.push_int_raw(&vals);
                    }
                }
                ColumnDataType::Text => {
                    let vals: Vec<Option<&str>> = self.buffer.iter().map(|row| {
                        row.get(col_idx).and_then(|v| match v {
                            ColValue::Text(s) => Some(s.as_str()),
                            ColValue::Null => None,
                            _ => None,
                        })
                    }).collect();

                    let unique_texts: std::collections::HashSet<&str> =
                        vals.iter().filter_map(|v| *v).collect();
                    if unique_texts.len() <= vals.len() / 3 && unique_texts.len() <= 128 {
                        builder.push_text_dict(&vals);
                    } else {
                        builder.push_text_raw(&vals);
                    }
                }
            }
        }

        let seg_id = self.next_segment_id;
        self.next_segment_id += 1;
        let path = self.segment_dir.join(format!("col_{seg_id:06}.seg"));
        builder.write_to(&path)?;

        let flushed = self.buffer.len();
        self.buffer.clear();
        Ok(flushed)
    }
}

// ── Delta applier ──────────────────────────────────────────────────────────

/// Processes WAL records and applies committed writes to delta buffers.
pub struct DeltaApplier {
    buffers: HashMap<String, DeltaBuffer>,
    segment_dir: PathBuf,
    flush_threshold: usize,
}

impl DeltaApplier {
    pub fn new(segment_dir: PathBuf, flush_threshold: usize) -> Self {
        Self {
            buffers: HashMap::new(),
            segment_dir,
            flush_threshold,
        }
    }

    /// Register a table schema for delta capture.
    pub fn register_table(&mut self, schema: TableSchema) {
        let table_dir = self.segment_dir.join(&schema.table_name);
        fs::create_dir_all(&table_dir).ok();
        self.buffers.insert(
            schema.table_name.clone(),
            DeltaBuffer::new(schema, table_dir, self.flush_threshold),
        );
    }

    /// Process a batch of WAL records. Returns the number of rows captured.
    pub fn apply_wal_batch(&mut self, records: &[WalRecord]) -> Result<usize> {
        let mut captured = 0usize;
        let mut pending_puts: HashMap<String, Vec<DeltaRow>> = HashMap::new();

        // Collect committed puts.
        for record in records {
            match record {
                WalRecord::Put { txn, key, value } => {
                    if let Some(table_name) = extract_table_from_key(key) {
                        pending_puts
                            .entry(table_name)
                            .or_default()
                            .push(DeltaRow {
                                txn: *txn,
                                key: key.clone(),
                                value: value.clone(),
                            });
                    }
                }
                WalRecord::Commit { txn: _ } => {
                    // All puts from this txn are already collected.
                }
                _ => {}
            }
        }

        // Apply to buffers.
        for (table_name, rows) in pending_puts {
            if let Some(buffer) = self.buffers.get_mut(&table_name) {
                for row in rows {
                    if let Some(values) = decode_row_values(&row.value, &buffer.table) {
                        buffer.append_row(values);
                        captured += 1;
                    }
                }

                if buffer.should_flush() {
                    buffer.flush()?;
                }
            }
        }

        Ok(captured)
    }

    /// Force-flush all buffers.
    pub fn flush_all(&mut self) -> Result<HashMap<String, usize>> {
        let mut result = HashMap::new();
        for (name, buffer) in &mut self.buffers {
            let count = buffer.flush()?;
            result.insert(name.clone(), count);
        }
        Ok(result)
    }

    /// Append a row directly to a table's buffer (for testing/manual use).
    pub fn append_row_to(&mut self, table_name: &str, values: Vec<ColValue>) {
        if let Some(buffer) = self.buffers.get_mut(table_name) {
            buffer.append_row(values);
        }
    }

    /// Get the number of buffered rows for a table.
    pub fn buffered_rows(&self, table_name: &str) -> usize {
        self.buffers
            .get(table_name)
            .map(|b| b.buffered_rows())
            .unwrap_or(0)
    }
}

// ── Key/value decoding helpers ─────────────────────────────────────────────

/// Extract table name from a key. Key format: `table_name:column_name:row_id`.
fn extract_table_from_key(key: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(key).ok()?;
    let table = s.split(':').next()?;
    if table.is_empty() {
        None
    } else {
        Some(table.to_string())
    }
}

/// Decode a serialized row value back into column values based on schema.
fn decode_row_values(value: &[u8], schema: &TableSchema) -> Option<Vec<ColValue>> {
    // Simple encoding: each column value is length-prefixed.
    // For TEXT: [u32 len][bytes]
    // For INT: [i64 value]
    // For NULL: [0u32] (zero-length)
    let mut values = Vec::with_capacity(schema.columns.len());
    let mut pos = 0;

    for col in &schema.columns {
        if pos > value.len() {
            return None;
        }
        match col.col_type {
            ColumnDataType::Text => {
                if pos + 4 > value.len() {
                    return None;
                }
                let len = u32::from_le_bytes([
                    value[pos], value[pos + 1], value[pos + 2], value[pos + 3],
                ]) as usize;
                pos += 4;
                if len == 0 {
                    values.push(ColValue::Null);
                } else if pos + len <= value.len() {
                    let s = std::str::from_utf8(&value[pos..pos + len]).ok()?;
                    values.push(ColValue::Text(s.to_string()));
                    pos += len;
                } else {
                    return None;
                }
            }
            ColumnDataType::Int => {
                if pos + 8 > value.len() {
                    return None;
                }
                let n = i64::from_le_bytes([
                    value[pos], value[pos + 1], value[pos + 2], value[pos + 3],
                    value[pos + 4], value[pos + 5], value[pos + 6], value[pos + 7],
                ]);
                values.push(ColValue::Int(n));
                pos += 8;
            }
        }
    }
    Some(values)
}

// ── Segment directory helpers ──────────────────────────────────────────────

fn count_existing_segments(dir: &Path) -> u64 {
    let mut max_id = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = name
                .strip_prefix("col_")
                .and_then(|s| s.strip_suffix(".seg"))
            {
                if let Ok(n) = id.parse::<u64>() {
                    max_id = max_id.max(n + 1);
                }
            }
        }
    }
    max_id
}

/// List all columnar segment files for a table.
pub fn list_segments(table_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut segments = Vec::new();
    if table_dir.exists() {
        for entry in fs::read_dir(table_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("col_") && name.ends_with(".seg") {
                segments.push(entry.path());
            }
        }
        segments.sort();
    }
    Ok(segments)
}

// ── LSN persistence ────────────────────────────────────────────────────────

/// Save the last applied LSN to a marker file.
pub fn save_lsn_marker(dir: &Path, lsn: Lsn) -> Result<()> {
    let path = dir.join("delta_lsn_marker");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(&lsn.to_le_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Load the last applied LSN from a marker file.
pub fn load_lsn_marker(dir: &Path) -> Lsn {
    let path = dir.join("delta_lsn_marker");
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let mut buf = [0u8; 8];
    if file.read_exact(&mut buf).is_err() {
        return 0;
    }
    u64::from_le_bytes(buf)
}

use std::fs::File;

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columnar::open_column_segment;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qmind_delta_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn users_schema() -> TableSchema {
        TableSchema {
            table_name: "users".into(),
            columns: vec![
                ColumnInfo { name: "id".into(), col_type: ColumnDataType::Int },
                ColumnInfo { name: "name".into(), col_type: ColumnDataType::Text },
                ColumnInfo { name: "dept".into(), col_type: ColumnDataType::Text },
            ],
        }
    }

    fn encode_int(n: i64) -> Vec<u8> {
        n.to_le_bytes().to_vec()
    }

    fn encode_text(s: &str) -> Vec<u8> {
        let bytes = s.as_bytes();
        let mut buf = (bytes.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(bytes);
        buf
    }

    fn encode_row(vals: &[Vec<u8>]) -> Vec<u8> {
        let mut buf = Vec::new();
        for v in vals {
            buf.extend_from_slice(v);
        }
        buf
    }

    #[test]
    fn extract_table_from_key_works() {
        assert_eq!(extract_table_from_key(b"users:name:0"), Some("users".into()));
        assert_eq!(extract_table_from_key(b"orders:total:5"), Some("orders".into()));
        assert_eq!(extract_table_from_key(b"only_colon:"), Some("only_colon".into()));
        assert_eq!(extract_table_from_key(b"nodelim"), Some("nodelim".into()));
    }

    #[test]
    fn decode_row_values_roundtrip() {
        let schema = users_schema();
        let row = encode_row(&[encode_int(42), encode_text("alice"), encode_text("eng")]);
        let values = decode_row_values(&row, &schema).unwrap();
        assert_eq!(values, vec![
            ColValue::Int(42),
            ColValue::Text("alice".into()),
            ColValue::Text("eng".into()),
        ]);
    }

    #[test]
    fn delta_buffer_flush_creates_segment() {
        let dir = test_dir("flush");
        let table_dir = dir.join("users");
        fs::create_dir_all(&table_dir).unwrap();

        let mut buffer = DeltaBuffer::new(users_schema(), table_dir.clone(), 100);

        // Append 5 rows.
        for i in 0..5i64 {
            buffer.append_row(vec![
                ColValue::Int(i),
                ColValue::Text(format!("user_{i}")),
                ColValue::Text("eng".into()),
            ]);
        }
        assert_eq!(buffer.buffered_rows(), 5);

        let flushed = buffer.flush().unwrap();
        assert_eq!(flushed, 5);
        assert_eq!(buffer.buffered_rows(), 0);

        // Verify segment was created.
        let segs = list_segments(&table_dir).unwrap();
        assert_eq!(segs.len(), 1);

        // Verify data roundtrips.
        let seg = open_column_segment(&segs[0]).unwrap();
        assert_eq!(seg.num_rows(), 5);
        assert_eq!(seg.num_columns(), 3);
        let id_col = seg.decode_column(0).unwrap();
        assert_eq!(id_col[0], ColValue::Int(0));
        assert_eq!(id_col[4], ColValue::Int(4));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delta_applier_captures_wal_puts() {
        let dir = test_dir("applier");
        let mut applier = DeltaApplier::new(dir.clone(), 100);
        applier.register_table(users_schema());

        // Simulate WAL records: begin, put, put, commit.
        // Each Put value must encode ALL columns per schema.
        let row_val = encode_row(&[encode_int(1), encode_text("alice"), encode_text("eng")]);
        let records = vec![
            WalRecord::Begin { txn: 1 },
            WalRecord::Put {
                txn: 1,
                key: b"users:id:0".to_vec(),
                value: row_val,
            },
            WalRecord::Commit { txn: 1 },
        ];

        let captured = applier.apply_wal_batch(&records).unwrap();
        assert_eq!(captured, 1);
        assert_eq!(applier.buffered_rows("users"), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delta_applier_auto_flushes() {
        let dir = test_dir("auto_flush");
        let mut applier = DeltaApplier::new(dir.clone(), 3);
        applier.register_table(users_schema());

        // 3 rows triggers flush at threshold.
        for i in 0..3i64 {
            let dept = format!("d{i}");
            let row_val = encode_row(&[encode_int(i), encode_text(&format!("u{i}")), encode_text(&dept)]);
            let records = vec![
                WalRecord::Put {
                    txn: i as u64,
                    key: b"users:id:0".to_vec(),
                    value: row_val,
                },
            ];
            applier.apply_wal_batch(&records).unwrap();
        }

        // Buffer should be empty after auto-flush.
        assert_eq!(applier.buffered_rows("users"), 0);

        // Segment should exist.
        let table_dir = dir.join("users");
        let segs = list_segments(&table_dir).unwrap();
        assert_eq!(segs.len(), 1);
        let seg = open_column_segment(&segs[0]).unwrap();
        assert_eq!(seg.num_rows(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lsn_marker_roundtrip() {
        let dir = test_dir("lsn");
        assert_eq!(load_lsn_marker(&dir), 0);
        save_lsn_marker(&dir, 42).unwrap();
        assert_eq!(load_lsn_marker(&dir), 42);
        save_lsn_marker(&dir, 1000).unwrap();
        assert_eq!(load_lsn_marker(&dir), 1000);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_segments_empty_dir() {
        let dir = test_dir("empty_segs");
        let segs = list_segments(&dir).unwrap();
        assert!(segs.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flush_all_returns_counts() {
        let dir = test_dir("flush_all");
        let mut applier = DeltaApplier::new(dir.clone(), 1000);
        applier.register_table(users_schema());

        for i in 0..5i64 {
            applier.append_row_to("users", vec![
                ColValue::Int(i),
                ColValue::Text(format!("u{i}")),
                ColValue::Text("eng".into()),
            ]);
        }

        let counts = applier.flush_all().unwrap();
        assert_eq!(counts.get("users"), Some(&5));

        let _ = fs::remove_dir_all(&dir);
    }
}
