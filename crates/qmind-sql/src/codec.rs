//! Row <-> KV byte codec. Layout per table row value bytes:
//! for each column, in definition order:
//! - Int(i64): tag 0 + 8 LE bytes
//! - Text:     tag 1 + u32 len + UTF-8
//! - Null:     tag 2
//!
//! Row keys: `{table}\u{1}{row_id:020}` — lexicographic order == insertion
//! order within a table.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SqlValue {
    Int(i64),
    Text(String),
    Null,
}

impl fmt::Display for SqlValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SqlValue::Int(v) => write!(f, "{v}"),
            SqlValue::Text(s) => write!(f, "{s}"),
            SqlValue::Null => write!(f, "NULL"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Int,
    Text,
}

impl ColumnType {
    pub fn name(self) -> &'static str {
        match self {
            ColumnType::Int => "INTEGER",
            ColumnType::Text => "TEXT",
        }
    }

    pub fn check(self, v: &SqlValue) -> bool {
        matches!(
            (self, v),
            (ColumnType::Int, SqlValue::Int(_))
                | (ColumnType::Text, SqlValue::Text(_))
                | (_, SqlValue::Null)
        )
    }
}

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
    pub nullable: bool,
}

pub fn encode_row(row: &[SqlValue]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    for v in row {
        match v {
            SqlValue::Int(i) => {
                out.push(0);
                out.extend_from_slice(&i.to_le_bytes());
            }
            SqlValue::Text(s) => {
                out.push(1);
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            SqlValue::Null => out.push(2),
        }
    }
    out
}

/// Decode against the table's column types; a truncated stream is a bug in
/// the writer, surfaced as None.
pub fn decode_row(bytes: &[u8], schema: &[ColumnDef]) -> Option<Vec<SqlValue>> {
    let mut pos = 0usize;
    let mut out = Vec::with_capacity(schema.len());
    for col in schema {
        if !col.nullable && pos >= bytes.len() {
            return None;
        }
        match bytes.get(pos)? {
            0 => {
                let b = bytes.get(pos + 1..pos + 9)?;
                out.push(SqlValue::Int(i64::from_le_bytes(b.try_into().unwrap())));
                pos += 9;
            }
            1 => {
                let len =
                    u32::from_le_bytes(bytes.get(pos + 1..pos + 5)?.try_into().unwrap()) as usize;
                let s = bytes.get(pos + 5..pos + 5 + len)?;
                out.push(SqlValue::Text(String::from_utf8_lossy(s).into_owned()));
                pos += 5 + len;
            }
            2 => {
                out.push(SqlValue::Null);
                pos += 1;
            }
            _ => return None,
        }
    }
    Some(out)
}

pub fn row_key(table: &str, row_id: u64) -> Vec<u8> {
    format!("{table}\u{1}{row_id:020}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Vec<ColumnDef> {
        vec![
            ColumnDef {
                name: "id".into(),
                ty: ColumnType::Int,
                nullable: false,
            },
            ColumnDef {
                name: "name".into(),
                ty: ColumnType::Text,
                nullable: true,
            },
        ]
    }

    #[test]
    fn row_roundtrip_including_nulls_and_unicode() {
        let rows = vec![
            vec![SqlValue::Int(7), SqlValue::Text("héllo wörld".into())],
            vec![SqlValue::Int(-42), SqlValue::Null],
            vec![SqlValue::Int(i64::MIN), SqlValue::Text(String::new())],
        ];
        for r in rows {
            let e = encode_row(&r);
            assert_eq!(decode_row(&e, &schema()), Some(r));
        }
    }

    #[test]
    fn corrupted_stream_rejected() {
        let mut e = encode_row(&[SqlValue::Int(1), SqlValue::Text("abc".into())]);
        e.truncate(8); // cut inside text length prefix
        assert_eq!(decode_row(&e, &schema()), None);
        e = encode_row(&[SqlValue::Int(1)]);
        e[0] = 9; // bad tag
        assert_eq!(decode_row(&e, &schema()), None);
    }
}
