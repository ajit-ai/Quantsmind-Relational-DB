# Write-Ahead Log (Developer Guide, R2)

> R2 deliverable. How the WAL works and how it is extended.

## Frame format

Each frame is `[len: u32][crc32: u32][payload: [u8; len]]`. `len` is the
payload length (>= 1, <= `MAX_RECORD_BYTES = 1 << 20`). The CRC-32 covers
the payload only. Frames are appended contiguously to `wal/wal.log`.

## Record tags (R2)

| Tag | Variant | Fields |
|-----|---------|--------|
| 0 | `Put` | txn, key, value |
| 1 | `Commit` | txn |
| 2 | `Begin` | txn |
| 3 | `Checkpoint` | txn (reserved, never written yet) |
| 4 | `Delete` | txn, key |
| 5 | `CreateTable` | name, columns (Vec<CatalogColumn>) |
| 6 | `CreateIndex` | name, table, column |
| 7 | `DropIndex` | name |

Tags 5-7 are DDL records with no txn field; committed by construction.

## Writer

`WalWriter::new(W)` and `WalWriter::with_syncer(W, fn)` manage
`next_lsn` and `durable_lsn`.

- `append(&WalRecord)` queues the encoded frame.
- `commit_group()` writes all queued frames, `flush()`s, then runs the
  syncer (if any). On success, `durable_lsn` advances. On syncer failure,
  the pending group is dropped and an error is returned.
- `resume(next_lsn)` repositions the writer after recovery: the next
  appended frame gets LSN `next_lsn`, and `durable_lsn = next_lsn - 1`.
- The frame layout is decoded lazily by `WalReader`.

## Reader / replay

`WalReader::replay(cursor)` returns `ReplayResult { records, end_offset,
torn_tail }`:

- Walks frames from the start.
- On a structural impossibility `(len == 0 || len > MAX_RECORD_BYTES)`, or a
  CRC mismatch on an otherwise complete frame: `Err(WalCorrupt { at })`.
- On an incomplete final frame (header present, payload short:
  `pos + 8 + len > file_len`): returns `torn_tail = true` with
  `end_offset = pos` (the offset after the last complete frame).

The engine uses `end_offset` to truncate the tail before resuming.

## Adding a new record type

1. Add a variant to `qmind_kernel::wal::WalRecord`.
2. Allocate the next free tag number.
3. Implement encoding in `WalWriter::append` and decoding in
   `WalReader::read`, keeping lengths within the frame bounds.
4. Update the fixtures and documentation in this directory.

The `max_lsn`/`next_lsn` bookkeeping is automatic once the variant is added.