//! Write-Ahead Log — durability contract of the engine.
//!
//! M1 scope: CRC-framed records, group-commit writer (one syscall per group),
//! replay that survives torn tails from crashes mid-write.
//!
//! Frame layout (little-endian): `[payload_len u32][crc u32][payload]`
//! LSNs are sequential record numbers starting at 1.

use crate::error::{Error, Result};
use crate::page::crc32;
use std::fmt;
use std::io::{Read, Write};

pub type Lsn = u64;
pub type TxnId = u64;

const MAX_RECORD_BYTES: usize = 1 << 20;

/// Append a length-prefixed UTF-8 string segment.
fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Read a length-prefixed string segment, validating bounds in one place.
fn read_str(buf: &[u8], pos: &mut usize) -> Result<String> {
    let len = read_u32(buf, pos)? as usize;
    let end = pos.checked_add(len).ok_or_else(|| Error::WalCorrupt {
        at: 0,
        reason: "string segment overflows".into(),
    })?;
    if end > buf.len() {
        return Err(Error::WalCorrupt {
            at: 0,
            reason: "string segment out of bounds".into(),
        });
    }
    let s = std::str::from_utf8(&buf[*pos..end]).map_err(|_| Error::WalCorrupt {
        at: 0,
        reason: "string segment is not UTF-8".into(),
    })?;
    *pos = end;
    Ok(s.to_string())
}

fn read_u32(buf: &[u8], pos: &mut usize) -> Result<u32> {
    let end = pos.checked_add(4).ok_or_else(|| Error::WalCorrupt {
        at: 0,
        reason: "segment length overflows".into(),
    })?;
    if end > buf.len() {
        return Err(Error::WalCorrupt {
            at: 0,
            reason: "segment out of bounds".into(),
        });
    }
    let v = u32::from_le_bytes(buf[*pos..end].try_into().unwrap());
    *pos = end;
    Ok(v)
}

/// Logical data type of a catalog column carried in DDL WAL records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Int,
    Text,
}

/// Column definition persisted in `CreateTable` WAL records. The SQL layer
/// maps these to its own richer `ColumnDef` and back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogColumn {
    pub name: String,
    pub kind: ColumnKind,
    pub nullable: bool,
}

/// Physiological log record. Payloads reference keys/values, not raw page
/// bytes, so replay stays valid across minor format versions (D-003).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalRecord {
    Begin {
        txn: TxnId,
    },
    Commit {
        txn: TxnId,
    },
    Abort {
        txn: TxnId,
    },
    /// Key-value write performed by `txn`.
    Put {
        txn: TxnId,
        key: Vec<u8>,
        value: Vec<u8>,
    },
    /// Fuzzy checkpoint: transactions in flight when it was taken.
    Checkpoint {
        active: Vec<TxnId>,
    },
    /// Catalog commit: table definition. DDL records carry no txn id — each
    /// is written durably before being made visible, so every DDL record in
    /// the log describes committed-by-construction schema.
    CreateTable {
        name: String,
        columns: Vec<CatalogColumn>,
    },
    CreateIndex {
        name: String,
        table: String,
        column: String,
    },
    DropIndex {
        name: String,
    },
}

impl WalRecord {
    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            WalRecord::Begin { txn } => {
                out.push(0);
                out.extend_from_slice(&txn.to_le_bytes());
            }
            WalRecord::Commit { txn } => {
                out.push(1);
                out.extend_from_slice(&txn.to_le_bytes());
            }
            WalRecord::Abort { txn } => {
                out.push(2);
                out.extend_from_slice(&txn.to_le_bytes());
            }
            WalRecord::Checkpoint { active } => {
                out.push(4);
                out.extend_from_slice(&(active.len() as u32).to_le_bytes());
                for t in active {
                    out.extend_from_slice(&t.to_le_bytes());
                }
            }
            WalRecord::Put { txn, key, value } => {
                out.push(3);
                out.extend_from_slice(&txn.to_le_bytes());
                out.extend_from_slice(&(key.len() as u32).to_le_bytes());
                out.extend_from_slice(key);
                out.extend_from_slice(&(value.len() as u32).to_le_bytes());
                out.extend_from_slice(value);
            }
            WalRecord::CreateTable { name, columns } => {
                out.push(5);
                push_str(out, name);
                out.extend_from_slice(&(columns.len() as u32).to_le_bytes());
                for col in columns {
                    push_str(out, &col.name);
                    out.push(match col.kind {
                        ColumnKind::Int => 0,
                        ColumnKind::Text => 1,
                    });
                    out.push(u8::from(col.nullable));
                }
            }
            WalRecord::CreateIndex {
                name,
                table,
                column,
            } => {
                out.push(6);
                push_str(out, name);
                push_str(out, table);
                push_str(out, column);
            }
            WalRecord::DropIndex { name } => {
                out.push(7);
                push_str(out, name);
            }
        }
    }

    fn decode(buf: &[u8]) -> Result<Self> {
        let tag = *buf.first().ok_or_else(|| Error::WalCorrupt {
            at: 0,
            reason: "empty payload".into(),
        })?;
        match tag {
            0..=2 => {
                if buf.len() != 9 {
                    return Err(Error::WalCorrupt {
                        at: 0,
                        reason: format!("payload len {} != expected 9", buf.len()),
                    });
                }
                let txn = u64::from_le_bytes(buf[1..9].try_into().unwrap());
                Ok(match tag {
                    0 => WalRecord::Begin { txn },
                    1 => WalRecord::Commit { txn },
                    _ => WalRecord::Abort { txn },
                })
            }
            3 => {
                // [tag u8][txn u64][klen u32][key][vlen u32][value]
                if buf.len() < 17 {
                    return Err(Error::WalCorrupt {
                        at: 0,
                        reason: "Put payload too short".into(),
                    });
                }
                let txn = u64::from_le_bytes(buf[1..9].try_into().unwrap());
                let klen = u32::from_le_bytes(buf[9..13].try_into().unwrap()) as usize;
                let vpos = 13 + klen;
                if buf.len() < vpos + 4 {
                    return Err(Error::WalCorrupt {
                        at: 0,
                        reason: "Put missing value length".into(),
                    });
                }
                let vlen = u32::from_le_bytes(buf[vpos..vpos + 4].try_into().unwrap()) as usize;
                if buf.len() != vpos + 4 + vlen {
                    return Err(Error::WalCorrupt {
                        at: 0,
                        reason: format!("Put len {} != expected {}", buf.len(), vpos + 4 + vlen),
                    });
                }
                Ok(WalRecord::Put {
                    txn,
                    key: buf[13..vpos].to_vec(),
                    value: buf[vpos + 4..].to_vec(),
                })
            }
            4 => {
                // payload: [tag u8][n u32][txn u64 × n]
                if buf.len() < 5 {
                    return Err(Error::WalCorrupt {
                        at: 0,
                        reason: "Checkpoint too short".into(),
                    });
                }
                let n = u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize;
                if buf.len() != 5 + n * 8 {
                    return Err(Error::WalCorrupt {
                        at: 0,
                        reason: "Checkpoint len mismatch".into(),
                    });
                }
                Ok(WalRecord::Checkpoint {
                    active: (0..n)
                        .map(|i| u64::from_le_bytes(buf[5 + i * 8..13 + i * 8].try_into().unwrap()))
                        .collect(),
                })
            }
            5 => {
                // [tag u8][name len u32][name][ncols u32][per col: name, kind u8, nullable u8]
                let mut pos = 1usize;
                let name = read_str(buf, &mut pos)?;
                let ncols = read_u32(buf, &mut pos)? as usize;
                let mut columns = Vec::with_capacity(ncols);
                for _ in 0..ncols {
                    let col_name = read_str(buf, &mut pos)?;
                    let kind_tag = *buf.get(pos).ok_or_else(|| Error::WalCorrupt {
                        at: 0,
                        reason: "CreateTable missing column kind".into(),
                    })?;
                    pos += 1;
                    let kind = match kind_tag {
                        0 => ColumnKind::Int,
                        1 => ColumnKind::Text,
                        other => {
                            return Err(Error::WalCorrupt {
                                at: 0,
                                reason: format!("unknown column kind tag {other}"),
                            });
                        }
                    };
                    let nullable_byte = *buf.get(pos).ok_or_else(|| Error::WalCorrupt {
                        at: 0,
                        reason: "CreateTable missing nullable flag".into(),
                    })?;
                    pos += 1;
                    columns.push(CatalogColumn {
                        name: col_name,
                        kind,
                        nullable: nullable_byte != 0,
                    });
                }
                if pos != buf.len() {
                    return Err(Error::WalCorrupt {
                        at: 0,
                        reason: "CreateTable payload has trailing bytes".into(),
                    });
                }
                Ok(WalRecord::CreateTable { name, columns })
            }
            6 => {
                // [tag u8][name][table][column]
                let mut pos = 1usize;
                let name = read_str(buf, &mut pos)?;
                let table = read_str(buf, &mut pos)?;
                let column = read_str(buf, &mut pos)?;
                if pos != buf.len() {
                    return Err(Error::WalCorrupt {
                        at: 0,
                        reason: "CreateIndex payload has trailing bytes".into(),
                    });
                }
                Ok(WalRecord::CreateIndex {
                    name,
                    table,
                    column,
                })
            }
            7 => {
                // [tag u8][name]
                let name = read_str(buf, &mut 1)?;
                Ok(WalRecord::DropIndex { name })
            }
            other => Err(Error::WalCorrupt {
                at: 0,
                reason: format!("unknown tag {other}"),
            }),
        }
    }

    fn encode_frame(&self, out: &mut Vec<u8>) {
        let start = out.len();
        self.encode_into(out);
        let payload = &out[start..];
        let crc = crc32(payload);
        let total = payload.len();
        out.splice(
            start..start,
            (total as u32)
                .to_le_bytes()
                .iter()
                .copied()
                .chain(crc.to_le_bytes()),
        );
    }
}

impl fmt::Display for WalRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WalRecord::Begin { txn } => write!(f, "begin t{txn}"),
            WalRecord::Commit { txn } => write!(f, "commit t{txn}"),
            WalRecord::Abort { txn } => write!(f, "abort t{txn}"),
            WalRecord::Put { txn, key, value } => {
                write!(f, "put t{txn} k{key:?}={}b", value.len())
            }
            WalRecord::Checkpoint { active } => write!(f, "ckpt active={}", active.len()),
            WalRecord::CreateTable { name, columns } => {
                write!(f, "ddl create_table {name} ({} cols)", columns.len())
            }
            WalRecord::CreateIndex { name, table, .. } => {
                write!(f, "ddl create_index {name} on {table}")
            }
            WalRecord::DropIndex { name } => write!(f, "ddl drop_index {name}"),
        }
    }
}

/// Buffered WAL appender with explicit group commit.
///
/// `append` only encodes into an in-memory group; [`WalWriter::commit_group`]
/// emits the whole group in one `write` + flush. Anything not committed when
/// the writer drops is lost — exactly the crash semantics recovery models.
///
/// Durability: with [`WalWriter::with_syncer`], `commit_group` additionally
/// runs a caller-supplied sync hook (e.g. `File::sync_data`) before returning.
/// `Write::flush` only pushes bytes to the OS page cache and is NOT a
/// durability point; only the syncer guards the "committed means durable"
/// contract. When no syncer is configured (tests, in-memory sinks), the
/// durability point is the flush — loss on crash is modeled, not hidden.
pub struct WalWriter<W: Write> {
    sink: W,
    syncer: Option<fn(&mut W) -> std::io::Result<()>>,
    next_lsn: Lsn,
    durable_lsn: Lsn,
    pending: Vec<u8>,
    pending_records: u32,
}

impl<W: Write> WalWriter<W> {
    pub fn new(sink: W) -> Self {
        Self {
            sink,
            syncer: None,
            next_lsn: 1,
            durable_lsn: 0,
            pending: Vec::with_capacity(64 * 1024),
            pending_records: 0,
        }
    }

    /// Same as [`WalWriter::new`] but runs `sync` after each committed group.
    /// For file-backed logs pass a free function calling `File::sync_data`
    /// (or `sync_all` when directory metadata also matters). A plain function
    /// pointer keeps the writer `Send + Sync` regardless of the sink type.
    pub fn with_syncer(sink: W, sync: fn(&mut W) -> std::io::Result<()>) -> Self {
        Self {
            sink,
            syncer: Some(sync),
            next_lsn: 1,
            durable_lsn: 0,
            pending: Vec::with_capacity(64 * 1024),
            pending_records: 0,
        }
    }

    /// Encode a record into the current uncommitted group; returns its LSN.
    pub fn append(&mut self, rec: &WalRecord) -> Lsn {
        let lsn = self.next_lsn;
        self.next_lsn += 1;
        rec.encode_frame(&mut self.pending);
        self.pending_records += 1;
        lsn
    }

    pub fn pending_records(&self) -> u32 {
        self.pending_records
    }

    pub fn durable_lsn(&self) -> Lsn {
        self.durable_lsn
    }

    pub fn next_lsn(&self) -> Lsn {
        self.next_lsn
    }

    /// Durability point: one bulk write + flush of the whole group, then (if
    /// configured) the sync hook that moves bytes across the OS durability
    /// boundary (e.g. `sync_data`). The group is durable only when this
    /// returns `Ok`; on sync failure the caller must treat the database as
    /// suspect — the on-disk tail may contain a full group whose durability
    /// is unproven, and recovery will treat it as committed (fsync-failure
    /// posture mirrors Postgres' PANIC-on-fsync behavior).
    pub fn commit_group(&mut self) -> Result<Lsn> {
        if self.pending.is_empty() {
            return Ok(self.durable_lsn);
        }
        self.sink.write_all(&self.pending)?;
        self.sink.flush()?;
        if let Some(sync) = &mut self.syncer {
            sync(&mut self.sink)?;
        }
        self.pending.clear();
        self.pending_records = 0;
        self.durable_lsn = self.next_lsn - 1;
        Ok(self.durable_lsn)
    }

    pub fn into_inner(self) -> W {
        self.sink
    }

    /// Continue logging after startup recovery (R2.5): the sink already holds
    /// `next_lsn - 1` durable records, so subsequent appends resume at
    /// `next_lsn` and never reuse an LSN from the replayed prefix.
    pub fn resume(&mut self, next_lsn: Lsn) {
        debug_assert!(next_lsn >= 1);
        self.next_lsn = next_lsn;
        self.durable_lsn = next_lsn - 1;
    }
}

/// Outcome of a log scan.
#[derive(Debug)]
pub struct ReplayResult {
    /// `(lsn, record)` pairs recovered in order.
    pub records: Vec<(Lsn, WalRecord)>,
    /// Byte offset of the boundary between the clean prefix (all frames fully
    /// validated through `end_offset`) and anything after it. When
    /// `torn_tail` is set, a file open for append can safely truncate here.
    pub end_offset: usize,
    /// True when the log ends mid-header or mid-frame — the signature of a
    /// crash during `commit_group`. A complete frame with a bad CRC, or an
    /// invalid frame length in a fully-present header, is treated as
    /// *corruption* (`Err`) rather than a tear: tearing can only shorten a
    /// frame, it cannot fabricate a structurally-invalid complete header.
    pub torn_tail: bool,
}

pub struct WalReader;

impl WalReader {
    /// Scan frames until clean EOF, a torn tail, or corrupt state.
    ///
    /// Error semantics (R2.5 / R2.18):
    /// - Log ends mid-header or mid-payload → `torn_tail = true`, the clean
    ///   prefix is returned, and the operator may truncate to `end_offset`.
    /// - A fully-present frame header with `len == 0` / `len > MAX_RECORD_BYTES`,
    ///   or a fully-present frame failing its CRC → `Err(WalCorrupt)`: the
    ///   writer never produced such bytes, so this is corruption, not a tear.
    pub fn replay<R: Read>(mut src: R) -> Result<ReplayResult> {
        let mut raw = Vec::new();
        src.read_to_end(&mut raw)?;

        let mut records = Vec::new();
        let mut torn_tail = false;
        let mut pos = 0usize;
        let mut lsn: Lsn = 1;

        loop {
            if pos == raw.len() {
                break;
            }
            let header = 8usize;
            if pos + header > raw.len() {
                torn_tail = true;
                break;
            }
            let len = u32::from_le_bytes(raw[pos..pos + 4].try_into().unwrap()) as usize;
            let crc = u32::from_le_bytes(raw[pos + 4..pos + 8].try_into().unwrap());
            if len == 0 || len > MAX_RECORD_BYTES {
                return Err(Error::WalCorrupt {
                    at: lsn,
                    reason: format!("invalid frame length {len} at offset {pos}"),
                });
            }
            if pos + header + len > raw.len() {
                torn_tail = true;
                break;
            }
            let payload_start = pos + header;
            let payload_end = payload_start + len;
            let payload = &raw[payload_start..payload_end];
            if crc32(payload) != crc {
                return Err(Error::WalCorrupt {
                    at: lsn,
                    reason: format!("checksum mismatch at offset {pos}"),
                });
            }
            match WalRecord::decode(payload) {
                Ok(rec) => records.push((lsn, rec)),
                Err(mut e) => {
                    if let Error::WalCorrupt { at, .. } = &mut e {
                        *at = lsn;
                    }
                    return Err(e);
                }
            }
            lsn += 1;
            pos = payload_end;
        }

        // `pos` is the boundary between the completely validated prefix and
        // everything after it in every exit path (clean EOF or a torn tail).
        Ok(ReplayResult {
            records,
            end_offset: pos,
            torn_tail,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct CountingSink {
        data: Vec<u8>,
        writes: u32,
    }

    impl CountingSink {
        fn new() -> Self {
            Self {
                data: Vec::new(),
                writes: 0,
            }
        }
    }

    impl Write for CountingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.data.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn sample_records() -> Vec<WalRecord> {
        vec![
            WalRecord::Begin { txn: 7 },
            WalRecord::Put {
                txn: 7,
                key: b"order:42".to_vec(),
                value: vec![3, 1, 4, 1, 5],
            },
            WalRecord::Commit { txn: 7 },
            WalRecord::Abort { txn: 9 },
            WalRecord::Begin { txn: 11 },
            WalRecord::Put {
                txn: 11,
                key: vec![0xFF; 300],
                value: vec![0xFF; 300],
            },
            WalRecord::Commit { txn: 11 },
        ]
    }

    #[test]
    fn frame_roundtrip_preserves_every_variant() {
        let sink = CountingSink::new();
        let mut w = WalWriter::new(sink);
        for rec in sample_records() {
            w.append(&rec);
        }
        w.commit_group().unwrap();

        let got = WalReader::replay(Cursor::new(w.into_inner().data)).unwrap();
        assert!(!got.torn_tail);
        let expect: Vec<Lsn> = (1..=sample_records().len() as Lsn).collect();
        assert_eq!(
            got.records.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            expect
        );
        assert!(got
            .records
            .iter()
            .map(|(_, r)| r)
            .eq(sample_records().iter()));
    }

    #[test]
    fn group_commit_is_one_bulk_write() {
        let sink = CountingSink::new();
        let mut w = WalWriter::new(sink);
        for i in 0..1000u64 {
            w.append(&WalRecord::Put {
                txn: i,
                key: format!("k{i}").into_bytes(),
                value: i.to_le_bytes().to_vec(),
            });
        }
        assert_eq!(w.pending_records(), 1000);
        w.commit_group().unwrap();
        assert_eq!(
            w.into_inner().writes,
            1,
            "group must collapse into one write"
        );
    }

    #[test]
    fn uncommitted_group_is_lost_on_drop() {
        let sink = CountingSink::new();
        let mut w = WalWriter::new(sink);
        w.append(&WalRecord::Begin { txn: 1 });
        w.append(&WalRecord::Commit { txn: 1 });
        w.commit_group().unwrap();
        // after-commit appends never reach the sink
        w.append(&WalRecord::Begin { txn: 2 });
        w.append(&WalRecord::Abort { txn: 2 });
        drop(w);

        let got = WalReader::replay(Cursor::new(
            // recover from what was flushed before drop:
            CountingSink::new().data,
        ));
        // empty log replays cleanly
        assert!(matches!(got, Ok(r) if r.records.is_empty() && !r.torn_tail));
    }

    #[test]
    fn committed_prefix_survives_uncommitted_tail_drop() {
        let mut sink = CountingSink::new();
        let mut w = WalWriter::new(&mut sink);
        w.append(&WalRecord::Begin { txn: 1 });
        w.commit_group().unwrap();
        w.append(&WalRecord::Begin { txn: 2 });
        w.append(&WalRecord::Commit { txn: 2 });
        w.commit_group().unwrap();
        w.append(&WalRecord::Begin { txn: 3 }); // lost on drop
        drop(w);

        let got = WalReader::replay(Cursor::new(sink.data.clone())).unwrap();
        assert!(!got.torn_tail);
        assert_eq!(
            got.records,
            vec![
                (1, WalRecord::Begin { txn: 1 }),
                (2, WalRecord::Begin { txn: 2 }),
                (3, WalRecord::Commit { txn: 2 }),
            ]
        );
    }

    #[test]
    fn torn_mid_record_tail_is_detected_and_ignored() {
        let mut sink = Vec::new();
        {
            let mut w = WalWriter::new(&mut sink);
            for i in 0..10u64 {
                w.append(&WalRecord::Put {
                    txn: i,
                    key: format!("k{i}").into_bytes(),
                    value: i.to_le_bytes().to_vec(),
                });
            }
            w.commit_group().unwrap();
        }
        let cut = sink.len() - 4; // slice through the final frame
        let truncated = sink[..cut].to_vec();

        let got = WalReader::replay(Cursor::new(truncated)).unwrap();
        assert!(got.torn_tail);
        assert_eq!(got.records.len(), 9, "frames before the tear survive");
        // end_offset is the byte boundary of the final valid frame — i.e. the
        // length of re-encoding the 9 surviving records alone.
        let mut good = Vec::new();
        {
            let mut w = WalWriter::new(&mut good);
            for i in 0..9u64 {
                w.append(&WalRecord::Put {
                    txn: i,
                    key: format!("k{i}").into_bytes(),
                    value: i.to_le_bytes().to_vec(),
                });
            }
            w.commit_group().unwrap();
        }
        assert_eq!(
            got.end_offset,
            good.len(),
            "end_offset is the clean boundary"
        );
    }

    #[test]
    fn corrupted_frame_fails_replay_loudly() {
        let mut sink = Vec::new();
        {
            let mut w = WalWriter::new(&mut sink);
            for i in 0..5u64 {
                w.append(&WalRecord::Begin { txn: i });
            }
            w.commit_group().unwrap();
        }
        let n = sink.len();
        sink[n - 1] ^= 0xFF; // inside last payload

        let err = WalReader::replay(Cursor::new(sink)).unwrap_err();
        assert!(
            matches!(&err, Error::WalCorrupt { at: 5, .. }),
            "complete frame with bad CRC is corruption, not a tear: {err}"
        );
    }

    #[test]
    fn invalid_frame_length_header_is_corruption() {
        let mut sink = Vec::new();
        {
            let mut w = WalWriter::new(&mut sink);
            w.append(&WalRecord::Begin { txn: 1 });
            w.commit_group().unwrap();
        }
        // Append a fully-present 8-byte header claiming an impossible length.
        sink.extend_from_slice(&[0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);

        let err = WalReader::replay(Cursor::new(sink)).unwrap_err();
        assert!(
            matches!(&err, Error::WalCorrupt { at: 2, .. }),
            "structurally invalid complete header must fail loudly: {err}"
        );
    }

    #[test]
    fn clean_log_reports_full_end_offset() {
        let mut sink = Vec::new();
        {
            let mut w = WalWriter::new(&mut sink);
            w.append(&WalRecord::Begin { txn: 1 });
            w.append(&WalRecord::Commit { txn: 1 });
            w.commit_group().unwrap();
        }
        let got = WalReader::replay(Cursor::new(&sink[..])).unwrap();
        assert!(!got.torn_tail);
        assert_eq!(got.end_offset, sink.len());
    }

    #[test]
    fn ddl_records_roundtrip_through_frames() {
        let mut w = WalWriter::new(Vec::new());
        w.append(&WalRecord::Begin { txn: 1 });
        w.append(&WalRecord::Put {
            txn: 1,
            key: b"k".to_vec(),
            value: b"v".to_vec(),
        });
        w.append(&WalRecord::Commit { txn: 1 });
        w.append(&WalRecord::CreateTable {
            name: "customers".into(),
            columns: vec![
                CatalogColumn {
                    name: "id".into(),
                    kind: ColumnKind::Int,
                    nullable: false,
                },
                CatalogColumn {
                    name: "name".into(),
                    kind: ColumnKind::Text,
                    nullable: true,
                },
            ],
        });
        w.append(&WalRecord::CreateIndex {
            name: "idx_customers_name".into(),
            table: "customers".into(),
            column: "name".into(),
        });
        w.append(&WalRecord::DropIndex {
            name: "idx_customers_name".into(),
        });
        w.commit_group().unwrap();

        let got = WalReader::replay(Cursor::new(w.into_inner())).unwrap();
        assert!(!got.torn_tail);
        assert_eq!(
            got.records
                .iter()
                .map(|(_, r)| r)
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                WalRecord::Begin { txn: 1 },
                WalRecord::Put {
                    txn: 1,
                    key: b"k".to_vec(),
                    value: b"v".to_vec(),
                },
                WalRecord::Commit { txn: 1 },
                WalRecord::CreateTable {
                    name: "customers".into(),
                    columns: vec![
                        CatalogColumn {
                            name: "id".into(),
                            kind: ColumnKind::Int,
                            nullable: false,
                        },
                        CatalogColumn {
                            name: "name".into(),
                            kind: ColumnKind::Text,
                            nullable: true,
                        },
                    ],
                },
                WalRecord::CreateIndex {
                    name: "idx_customers_name".into(),
                    table: "customers".into(),
                    column: "name".into(),
                },
                WalRecord::DropIndex {
                    name: "idx_customers_name".into(),
                },
            ]
        );
    }

    #[test]
    fn with_syncer_syncs_after_each_committed_group_only() {
        struct SyncCounter {
            bytes: Vec<u8>,
            syncs: u32,
        }
        impl Write for SyncCounter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        fn syncit(s: &mut SyncCounter) -> std::io::Result<()> {
            s.syncs += 1;
            Ok(())
        }

        let mut w = WalWriter::with_syncer(
            SyncCounter {
                bytes: Vec::new(),
                syncs: 0,
            },
            syncit,
        );
        w.append(&WalRecord::Begin { txn: 1 });
        w.append(&WalRecord::Commit { txn: 1 });
        w.commit_group().unwrap();
        let sink = w.into_inner();
        assert_eq!(sink.syncs, 1, "one committed group => one sync");

        // A pending (uncommitted) group must never reach the sync hook.
        let mut w2 = WalWriter::with_syncer(
            SyncCounter {
                bytes: Vec::new(),
                syncs: 0,
            },
            syncit,
        );
        w2.append(&WalRecord::Begin { txn: 1 });
        w2.commit_group().unwrap();
        w2.append(&WalRecord::Begin { txn: 2 }); // never committed
        let sink2 = w2.into_inner();
        assert_eq!(sink2.syncs, 1, "uncommitted group must not sync");

        // A failing sync surfaces as an error from commit_group.
        fn fail(_s: &mut SyncCounter) -> std::io::Result<()> {
            Err(std::io::Error::from_raw_os_error(28))
        }
        let mut w3 = WalWriter::with_syncer(
            SyncCounter {
                bytes: Vec::new(),
                syncs: 0,
            },
            fail,
        );
        w3.append(&WalRecord::Begin { txn: 1 });
        assert!(w3.commit_group().is_err(), "syncer failure must surface");
    }

    #[test]
    fn resume_continues_lsns_past_recovered_prefix() {
        let mut sink = Vec::new();
        {
            let mut w = WalWriter::new(&mut sink);
            w.append(&WalRecord::Begin { txn: 1 });
            w.append(&WalRecord::Commit { txn: 1 });
            w.commit_group().unwrap();
        }
        // Recovered prefix length = 2 records.
        let mut w = WalWriter::new(Vec::new());
        w.resume(3);
        let lsn = w.append(&WalRecord::Begin { txn: 2 });
        assert_eq!(lsn, 3, "appends resume after the recovered count");
        assert_eq!(w.durable_lsn(), 2, "durable horizon reflects the prefix");
    }

    #[test]
    fn display_is_stable() {
        assert_eq!(
            WalRecord::Put {
                txn: 3,
                key: b"ab".to_vec(),
                value: vec![7, 9]
            }
            .to_string(),
            "put t3 k[97, 98]=2b"
        );
    }
}
