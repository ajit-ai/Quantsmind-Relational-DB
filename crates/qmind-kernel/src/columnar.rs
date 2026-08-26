//! Persistent columnar segment storage (M8a).
//!
//! Layout per D-003 versioning: each segment file opens with
//! `[magic "QMINDCOL"][u16 format_version][u32 num_columns][u32 num_rows][pad to 64]`,
//! followed by column metadata, then encoded column data chunks.
//!
//! Encodings: Raw (no compression), Dictionary (TEXT), RLE (low-cardinality INT).

use crate::error::{Error, Result};
use crate::page::crc32;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

// ── Constants ──────────────────────────────────────────────────────────────

pub const COL_SEGMENT_MAGIC: &[u8; 8] = b"QMINDCOL";
pub const COL_FORMAT_VERSION: u16 = 1;
pub const COL_HEADER_SIZE: u64 = 64;
pub const COL_META_ENTRY_SIZE: u32 = 32;

// ── Column types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ColumnType {
    Null = 0,
    Int = 1,
    Text = 2,
}

impl ColumnType {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Null),
            1 => Ok(Self::Int),
            2 => Ok(Self::Text),
            _ => Err(Error::Other(format!("unknown column type: {v}"))),
        }
    }
}

// ── Encodings ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Encoding {
    Raw = 0,
    Dict = 1,
    Rle = 2,
}

impl Encoding {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Raw),
            1 => Ok(Self::Dict),
            2 => Ok(Self::Rle),
            _ => Err(Error::Other(format!("unknown encoding: {v}"))),
        }
    }
}

// ── Value ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ColValue {
    Null,
    Int(i64),
    Text(String),
}

// ── Column metadata ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub col_type: ColumnType,
    pub encoding: Encoding,
    pub num_values: u32,
    pub null_count: u32,
    pub data_offset: u64,
    pub data_length: u64,
}

// ── Column segment file ────────────────────────────────────────────────────

pub struct ColumnSegment {
    num_columns: u32,
    num_rows: u32,
    columns: Vec<ColumnMeta>,
    data_blob: Vec<u8>,
}

impl ColumnSegment {
    /// Open an existing column segment file.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new().read(true).open(&path)?;

        // Read and validate header.
        let mut hdr = [0u8; COL_HEADER_SIZE as usize];
        file.read_exact(&mut hdr)?;

        if &hdr[..8] != COL_SEGMENT_MAGIC {
            return Err(Error::Other(format!(
                "bad column segment magic: expected {:?}, got {:?}",
                COL_SEGMENT_MAGIC,
                &hdr[..8]
            )));
        }
        let version = u16::from_le_bytes([hdr[8], hdr[9]]);
        if version > COL_FORMAT_VERSION {
            return Err(Error::Other(format!(
                "unsupported column format version {version}"
            )));
        }
        let num_columns = u32::from_le_bytes([hdr[10], hdr[11], hdr[12], hdr[13]]);
        let num_rows = u32::from_le_bytes([hdr[14], hdr[15], hdr[16], hdr[17]]);

        // Read column metadata.
        let meta_start = COL_HEADER_SIZE;
        let mut columns = Vec::with_capacity(num_columns as usize);
        for i in 0..num_columns {
            let offset = meta_start + (i as u64) * (COL_META_ENTRY_SIZE as u64);
            file.seek(SeekFrom::Start(offset))?;
            let mut entry = [0u8; COL_META_ENTRY_SIZE as usize];
            file.read_exact(&mut entry)?;

            columns.push(ColumnMeta {
                col_type: ColumnType::from_u8(entry[0])?,
                encoding: Encoding::from_u8(entry[1])?,
                num_values: u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]),
                null_count: u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]),
                data_offset: u64::from_le_bytes([
                    entry[16], entry[17], entry[18], entry[19], entry[20], entry[21], entry[22],
                    entry[23],
                ]),
                data_length: u64::from_le_bytes([
                    entry[24], entry[25], entry[26], entry[27], entry[28], entry[29], entry[30],
                    entry[31],
                ]),
            });
        }

        // Read all column data into memory.
        let data_start = meta_start + (num_columns as u64) * (COL_META_ENTRY_SIZE as u64);
        let mut data_blob = Vec::new();
        file.seek(SeekFrom::Start(data_start))?;
        file.read_to_end(&mut data_blob)?;

        Ok(Self {
            num_columns,
            num_rows,
            columns,
            data_blob,
        })
    }

    pub fn num_rows(&self) -> u32 {
        self.num_rows
    }
    pub fn num_columns(&self) -> u32 {
        self.num_columns
    }
    pub fn column_meta(&self, idx: usize) -> Option<&ColumnMeta> {
        self.columns.get(idx)
    }

    /// Decode all values for a column.
    pub fn decode_column(&self, idx: usize) -> Result<Vec<ColValue>> {
        let meta = self
            .columns
            .get(idx)
            .ok_or_else(|| Error::Other(format!("column index {idx} out of range")))?;

        let start = meta.data_offset as usize;
        let end = start + meta.data_length as usize;
        let chunk = self
            .data_blob
            .get(start..end)
            .ok_or_else(|| Error::Other("column data range out of bounds".into()))?;

        match meta.encoding {
            Encoding::Raw => decode_raw(chunk, meta),
            Encoding::Dict => decode_dict(chunk, meta),
            Encoding::Rle => decode_rle(chunk, meta),
        }
    }
}

// ── Raw encoding ───────────────────────────────────────────────────────────

fn decode_raw(data: &[u8], meta: &ColumnMeta) -> Result<Vec<ColValue>> {
    let mut values = Vec::with_capacity(meta.num_values as usize);
    let bitmap_len = usize::div_ceil(meta.num_values as usize, 8);
    let mut pos = bitmap_len;

    for i in 0..meta.num_values as usize {
        if pos > data.len() {
            return Err(Error::Other("raw data truncated".into()));
        }
        let null_byte = data.get(i / 8).copied().unwrap_or(0);
        if null_byte & (1 << (i % 8)) != 0 {
            values.push(ColValue::Null);
            continue;
        }

        match meta.col_type {
            ColumnType::Int => {
                if pos + 8 > data.len() {
                    return Err(Error::Other("int data truncated".into()));
                }
                let val = i64::from_le_bytes([
                    data[pos], data[pos + 1], data[pos + 2], data[pos + 3], data[pos + 4],
                    data[pos + 5], data[pos + 6], data[pos + 7],
                ]);
                values.push(ColValue::Int(val));
                pos += 8;
            }
            ColumnType::Text => {
                if pos + 4 > data.len() {
                    return Err(Error::Other("text length truncated".into()));
                }
                let len = u32::from_le_bytes([
                    data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
                ]) as usize;
                pos += 4;
                if pos + len > data.len() {
                    return Err(Error::Other("text data truncated".into()));
                }
                let s = std::str::from_utf8(&data[pos..pos + len])
                    .map_err(|e| Error::Other(format!("invalid utf-8: {e}")))?;
                values.push(ColValue::Text(s.to_string()));
                pos += len;
            }
            ColumnType::Null => {
                values.push(ColValue::Null);
            }
        }
    }
    Ok(values)
}

// ── Dictionary encoding ────────────────────────────────────────────────────

fn decode_dict(data: &[u8], meta: &ColumnMeta) -> Result<Vec<ColValue>> {
    if data.len() < 4 {
        return Err(Error::Other("dict data too short".into()));
    }
    let dict_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut pos = 4;

    // Read dictionary entries.
    let mut dict: Vec<String> = Vec::with_capacity(dict_len);
    for _ in 0..dict_len {
        if pos + 4 > data.len() {
            return Err(Error::Other("dict entry length truncated".into()));
        }
        let len = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        pos += 4;
        if pos + len > data.len() {
            return Err(Error::Other("dict entry data truncated".into()));
        }
        let s = std::str::from_utf8(&data[pos..pos + len])
            .map_err(|e| Error::Other(format!("invalid utf-8 in dict: {e}")))?;
        dict.push(s.to_string());
        pos += len;
    }

    // Read index array (4 bytes each).
    let mut values = Vec::with_capacity(meta.num_values as usize);
    for _ in 0..meta.num_values {
        if pos + 4 > data.len() {
            return Err(Error::Other("dict index truncated".into()));
        }
        let idx = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        pos += 4;
        if idx == u32::MAX as usize {
            values.push(ColValue::Null);
        } else if idx < dict.len() {
            values.push(ColValue::Text(dict[idx].clone()));
        } else {
            return Err(Error::Other(format!("dict index {idx} out of range")));
        }
    }
    Ok(values)
}

// ── RLE encoding ───────────────────────────────────────────────────────────

fn decode_rle(data: &[u8], meta: &ColumnMeta) -> Result<Vec<ColValue>> {
    let mut values = Vec::with_capacity(meta.num_values as usize);
    let mut pos = 0;

    while values.len() < meta.num_values as usize {
        if pos + 8 > data.len() {
            return Err(Error::Other("rle run truncated".into()));
        }
        let run_len = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        let val = i64::from_le_bytes([
            data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7], data[pos + 8],
            data[pos + 9], data[pos + 10], data[pos + 11],
        ]);
        pos += 12;
        for _ in 0..run_len {
            values.push(ColValue::Int(val));
        }
    }
    Ok(values)
}

// ── ColumnSegment builder (write path) ─────────────────────────────────────

pub struct ColumnSegmentBuilder {
    num_columns: u32,
    num_rows: u32,
    columns: Vec<ColumnMeta>,
    data_blob: Vec<u8>,
}

impl ColumnSegmentBuilder {
    pub fn new(num_columns: u32, num_rows: u32) -> Self {
        Self {
            num_columns,
            num_rows,
            columns: Vec::with_capacity(num_columns as usize),
            data_blob: Vec::new(),
        }
    }

    /// Append a raw-encoded INT column.
    pub fn push_int_raw(&mut self, values: &[Option<i64>]) {
        let mut buf = Vec::new();
        let mut null_count = 0u32;

        // Null bitmap.
        let bitmap_len = usize::div_ceil(values.len(), 8);
        let mut bitmap = vec![0u8; bitmap_len];
        for (i, v) in values.iter().enumerate() {
            if v.is_none() {
                bitmap[i / 8] |= 1 << (i % 8);
                null_count += 1;
            }
        }
        buf.extend_from_slice(&bitmap);

        // Values (nulls already tracked in bitmap, skip them).
        for n in values.iter().flatten() {
            buf.extend_from_slice(&n.to_le_bytes());
        }

        let offset = self.data_blob.len() as u64;
        let len = buf.len() as u64;
        self.data_blob.extend_from_slice(&buf);
        self.columns.push(ColumnMeta {
            col_type: ColumnType::Int,
            encoding: Encoding::Raw,
            num_values: values.len() as u32,
            null_count,
            data_offset: offset,
            data_length: len,
        });
    }

    /// Append a raw-encoded TEXT column.
    pub fn push_text_raw(&mut self, values: &[Option<&str>]) {
        let mut buf = Vec::new();
        let mut null_count = 0u32;

        let bitmap_len = usize::div_ceil(values.len(), 8);
        let mut bitmap = vec![0u8; bitmap_len];
        for (i, v) in values.iter().enumerate() {
            if v.is_none() {
                bitmap[i / 8] |= 1 << (i % 8);
                null_count += 1;
            }
        }
        buf.extend_from_slice(&bitmap);

        for s in values.iter().flatten() {
            let bytes = s.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }

        let offset = self.data_blob.len() as u64;
        let len = buf.len() as u64;
        self.data_blob.extend_from_slice(&buf);
        self.columns.push(ColumnMeta {
            col_type: ColumnType::Text,
            encoding: Encoding::Raw,
            num_values: values.len() as u32,
            null_count,
            data_offset: offset,
            data_length: len,
        });
    }

    /// Append a dictionary-encoded TEXT column.
    pub fn push_text_dict(&mut self, values: &[Option<&str>]) {
        let mut null_count = 0u32;
        let mut unique: Vec<String> = Vec::new();
        let mut index_map = std::collections::HashMap::new();

        // Build dictionary.
        for v in values {
            if let Some(s) = v {
                if !index_map.contains_key(*s) {
                    index_map.insert(s.to_string(), unique.len());
                    unique.push(s.to_string());
                }
            } else {
                null_count += 1;
            }
        }

        let mut buf = Vec::new();
        // Dictionary length.
        buf.extend_from_slice(&(unique.len() as u32).to_le_bytes());
        // Dictionary entries.
        for entry in &unique {
            let bytes = entry.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
        // Index array.
        for v in values {
            if let Some(s) = v {
                let idx = index_map[*s];
                buf.extend_from_slice(&(idx as u32).to_le_bytes());
            } else {
                buf.extend_from_slice(&(u32::MAX).to_le_bytes());
            }
        }

        let offset = self.data_blob.len() as u64;
        let len = buf.len() as u64;
        self.data_blob.extend_from_slice(&buf);
        self.columns.push(ColumnMeta {
            col_type: ColumnType::Text,
            encoding: Encoding::Dict,
            num_values: values.len() as u32,
            null_count,
            data_offset: offset,
            data_length: len,
        });
    }

    /// Append an RLE-encoded INT column.
    pub fn push_int_rle(&mut self, values: &[Option<i64>]) {
        let mut null_count = 0u32;
        let mut runs: Vec<(u32, i64)> = Vec::new();

        let mut current_run: Option<(u32, i64)> = None;
        for v in values {
            match v {
                None => {
                    null_count += 1;
                    if let Some(run) = current_run.take() {
                        runs.push(run);
                    }
                }
                Some(n) => {
                    match &mut current_run {
                        Some((count, val)) if *val == *n => *count += 1,
                        Some((count, val)) => {
                            let old = (*count, *val);
                            *count = 1;
                            *val = *n;
                            runs.push(old);
                        }
                        None => current_run = Some((1, *n)),
                    }
                }
            }
        }
        if let Some(run) = current_run.take() {
            runs.push(run);
        }

        let mut buf = Vec::new();
        for (count, val) in &runs {
            buf.extend_from_slice(&count.to_le_bytes());
            buf.extend_from_slice(&val.to_le_bytes());
        }

        let offset = self.data_blob.len() as u64;
        let len = buf.len() as u64;
        self.data_blob.extend_from_slice(&buf);
        self.columns.push(ColumnMeta {
            col_type: ColumnType::Int,
            encoding: Encoding::Rle,
            num_values: values.len() as u32,
            null_count,
            data_offset: offset,
            data_length: len,
        });
    }

    /// Serialize the segment to a file.
    pub fn write_to<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;

        // Header.
        let mut hdr = [0u8; COL_HEADER_SIZE as usize];
        hdr[..8].copy_from_slice(COL_SEGMENT_MAGIC);
        hdr[8..10].copy_from_slice(&COL_FORMAT_VERSION.to_le_bytes());
        hdr[10..14].copy_from_slice(&self.num_columns.to_le_bytes());
        hdr[14..18].copy_from_slice(&self.num_rows.to_le_bytes());
        // CRC32 placeholder at bytes 18-21.
        let c = crc32(&hdr[22..]);
        hdr[18..22].copy_from_slice(&c.to_le_bytes());
        file.write_all(&hdr)?;

        // Column metadata.
        for meta in &self.columns {
            let mut entry = [0u8; COL_META_ENTRY_SIZE as usize];
            entry[0] = meta.col_type as u8;
            entry[1] = meta.encoding as u8;
            entry[4..8].copy_from_slice(&meta.num_values.to_le_bytes());
            entry[8..12].copy_from_slice(&meta.null_count.to_le_bytes());
            entry[16..24].copy_from_slice(&meta.data_offset.to_le_bytes());
            entry[24..32].copy_from_slice(&meta.data_length.to_le_bytes());
            file.write_all(&entry)?;
        }

        // Data blob.
        file.write_all(&self.data_blob)?;
        file.sync_all()?;

        Ok(())
    }
}

// ── Public helpers ─────────────────────────────────────────────────────────

/// Open a column segment file.
pub fn open_column_segment<P: AsRef<Path>>(path: P) -> Result<ColumnSegment> {
    ColumnSegment::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qmind_col_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("segment.col")
    }

    #[test]
    fn int_raw_roundtrip() {
        let path = tmp_path("int_raw");
        let vals: Vec<Option<i64>> = vec![Some(10), Some(20), None, Some(40)];
        let mut b = ColumnSegmentBuilder::new(1, 4);
        b.push_int_raw(&vals);
        b.write_to(&path).unwrap();

        let seg = ColumnSegment::open(&path).unwrap();
        assert_eq!(seg.num_rows(), 4);
        assert_eq!(seg.num_columns(), 1);
        let decoded = seg.decode_column(0).unwrap();
        assert_eq!(decoded, vec![
            ColValue::Int(10),
            ColValue::Int(20),
            ColValue::Null,
            ColValue::Int(40),
        ]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn text_raw_roundtrip() {
        let path = tmp_path("text_raw");
        let vals: Vec<Option<&str>> = vec![Some("hello"), None, Some("world")];
        let mut b = ColumnSegmentBuilder::new(1, 3);
        b.push_text_raw(&vals);
        b.write_to(&path).unwrap();

        let seg = ColumnSegment::open(&path).unwrap();
        let decoded = seg.decode_column(0).unwrap();
        assert_eq!(decoded, vec![
            ColValue::Text("hello".into()),
            ColValue::Null,
            ColValue::Text("world".into()),
        ]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn text_dict_roundtrip() {
        let path = tmp_path("text_dict");
        let vals: Vec<Option<&str>> = vec![Some("apple"), Some("banana"), Some("apple"), None, Some("banana")];
        let mut b = ColumnSegmentBuilder::new(1, 5);
        b.push_text_dict(&vals);
        b.write_to(&path).unwrap();

        let seg = ColumnSegment::open(&path).unwrap();
        let meta = seg.column_meta(0).unwrap();
        assert_eq!(meta.encoding, Encoding::Dict);
        assert_eq!(meta.null_count, 1);
        let decoded = seg.decode_column(0).unwrap();
        assert_eq!(decoded, vec![
            ColValue::Text("apple".into()),
            ColValue::Text("banana".into()),
            ColValue::Text("apple".into()),
            ColValue::Null,
            ColValue::Text("banana".into()),
        ]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn int_rle_roundtrip() {
        let path = tmp_path("int_rle");
        let vals: Vec<Option<i64>> = vec![Some(5), Some(5), Some(5), Some(7), Some(7), Some(9)];
        let mut b = ColumnSegmentBuilder::new(1, 6);
        b.push_int_rle(&vals);
        b.write_to(&path).unwrap();

        let seg = ColumnSegment::open(&path).unwrap();
        let meta = seg.column_meta(0).unwrap();
        assert_eq!(meta.encoding, Encoding::Rle);
        let decoded = seg.decode_column(0).unwrap();
        assert_eq!(decoded, vec![
            ColValue::Int(5), ColValue::Int(5), ColValue::Int(5),
            ColValue::Int(7), ColValue::Int(7),
            ColValue::Int(9),
        ]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn multi_column_roundtrip() {
        let path = tmp_path("multi_col");
        let ids: Vec<Option<i64>> = vec![Some(1), Some(2), Some(3), None];
        let names: Vec<Option<&str>> = vec![Some("alice"), Some("bob"), Some("carol"), None];
        let mut b = ColumnSegmentBuilder::new(2, 4);
        b.push_int_raw(&ids);
        b.push_text_dict(&names);
        b.write_to(&path).unwrap();

        let seg = ColumnSegment::open(&path).unwrap();
        assert_eq!(seg.num_columns(), 2);
        assert_eq!(seg.num_rows(), 4);
        let d0 = seg.decode_column(0).unwrap();
        let d1 = seg.decode_column(1).unwrap();
        assert_eq!(d0, vec![
            ColValue::Int(1), ColValue::Int(2), ColValue::Int(3), ColValue::Null,
        ]);
        assert_eq!(d1, vec![
            ColValue::Text("alice".into()),
            ColValue::Text("bob".into()),
            ColValue::Text("carol".into()),
            ColValue::Null,
        ]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn bad_magic_is_rejected() {
        let path = tmp_path("bad_magic");
        let dir = path.parent().unwrap();
        fs::create_dir_all(dir).unwrap();
        std::fs::write(&path, b"WRONGMAGICgarbage").unwrap();
        let result = ColumnSegment::open(&path);
        assert!(result.is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_column_roundtrip() {
        let path = tmp_path("empty");
        let mut b = ColumnSegmentBuilder::new(1, 0);
        b.push_int_raw(&[]);
        b.write_to(&path).unwrap();

        let seg = ColumnSegment::open(&path).unwrap();
        assert_eq!(seg.num_rows(), 0);
        let decoded = seg.decode_column(0).unwrap();
        assert!(decoded.is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
