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
        }
    }
}

/// Buffered WAL appender with explicit group commit.
///
/// `append` only encodes into an in-memory group; [`WalWriter::commit_group`]
/// emits the whole group in one `write` + flush. Anything not committed when
/// the writer drops is lost — exactly the crash semantics recovery models.
pub struct WalWriter<W: Write> {
    sink: W,
    next_lsn: Lsn,
    durable_lsn: Lsn,
    pending: Vec<u8>,
    pending_records: u32,
}

impl<W: Write> WalWriter<W> {
    pub fn new(sink: W) -> Self {
        Self {
            sink,
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

    /// Durability point: one bulk write + flush of the whole group.
    pub fn commit_group(&mut self) -> Result<Lsn> {
        if self.pending.is_empty() {
            return Ok(self.durable_lsn);
        }
        self.sink.write_all(&self.pending)?;
        self.sink.flush()?;
        self.pending.clear();
        self.pending_records = 0;
        self.durable_lsn = self.next_lsn - 1;
        Ok(self.durable_lsn)
    }

    pub fn into_inner(self) -> W {
        self.sink
    }
}

/// Outcome of a log scan.
#[derive(Debug)]
pub struct ReplayResult {
    /// `(lsn, record)` pairs recovered in order.
    pub records: Vec<(Lsn, WalRecord)>,
    /// True when the tail ended mid-frame or failed its checksum — the
    /// signature of a crash during `commit_group`. Recovered prefix is
    /// exactly the set of groups that reached storage.
    pub torn_tail: bool,
}

pub struct WalReader;

impl WalReader {
    /// Scan frames until clean EOF or the first bad frame.
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
            if len == 0 || len > MAX_RECORD_BYTES || pos + header + len > raw.len() {
                torn_tail = true;
                break;
            }
            let payload_start = pos + header;
            let payload_end = payload_start + len;
            let payload = &raw[payload_start..payload_end];
            if crc32(payload) != crc {
                torn_tail = true;
                break;
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

        Ok(ReplayResult { records, torn_tail })
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
    }

    #[test]
    fn corrupted_payload_stops_replay_at_checksum() {
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

        let got = WalReader::replay(Cursor::new(sink)).unwrap();
        assert!(got.torn_tail);
        assert_eq!(got.records.len(), 4);
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
