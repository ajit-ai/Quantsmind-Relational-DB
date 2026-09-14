# Storage Layout (R2)

> R2 deliverable. Canonical map of the database directory and the binary
> format of every durable file in the engine.

## 1. Directory structure

```
<root>/
  db.meta          versioned metadata header (28 bytes)
  wal/
    wal.log        append-only WAL (zero or more CRC-protected frames)
  catalog/         reserved, not yet used (R3 checkpoint metadata)
  tables/          reserved, not yet used (R3 on-disk row store)
  indexes/         reserved, not yet used (R3 on-disk index B-Trees)
  columnar/        reserved, not yet used (R3 columnar segments)
  checkpoints/     reserved, not yet used (R3 checkpoint writes)
```

`db.meta` is the unique identifier of a QuantsMind database directory. Its
presence distinguishes a fresh run (where `create_db` writes the layout) from
a reuse run (where `open_db` reads it).

## 2. db.meta format

Fixed 28-byte file, all little-endian:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 8 | magic | `b"QMINDDB\0"` (ASCII, NUL-terminated) |
| 8 | 4 | meta_version | `1u32` |
| 12 | 4 | db_version | `1u32` |
| 16 | 4 | format_version | `1u32` |
| 20 | 4 | wal_version | `1u32` |
| 24 | 4 | catalog_version | `1u32` |

All version fields are currently `1`. A mismatch in any field produces
`Error::UnsupportedFormat { entity, found, expected }` during `open_db`.
The magic check is the very first gate; a wrong magic (including a missing
file) produces `Error::CatalogCorrupt` — the directory is not a valid
QuantsMind database.

`create_db` writes the meta after creating the directory layout, with a full
`fsync_all()` on the file.

## 3. WAL file: wal/wal.log

The WAL is a sequence of CRC-protected frames, each consisting of:

| Field | Type | Description |
|-------|------|-------------|
| len | u32 LE | byte-length of the payload (must be > 0 and <= `MAX_RECORD_BYTES = 1 << 20`) |
| crc32 | u32 LE | CRC-32 of the payload bytes |
| payload | `[u8; len]` | encoded `WalRecord` |

The `WalWriter` handles frame construction. Frame boundaries are the only
unit of durability granularity; a torn write is defined as a frame whose
payload is incomplete (`pos + 8 + len > file_len`), which is the only
silent-truncation case on restart. See `DURABILITY_CONTRACT.md` for why
this is safe.

### WalRecord tags (as of R2)

| Tag | Record | Length-prefixed fields | Has txn field |
|-----|--------|----------------------|---------------|
| 0 | `Put` | key (4+len), value (4+len) | yes |
| 1 | `Commit` | none | yes |
| 2 | `Begin` | none | yes |
| 3 | `Checkpoint` | none | yes |
| 4 | `Delete` | key (4+len) | yes |
| 5 | `CreateTable` | name (4+len), columns (4+N * column) | no |
| 6 | `CreateIndex` | name (4+len), table (4+len), column (4+len) | no |
| 7 | `DropIndex` | name (4+len) | no |

Tags 5/6/7 are DDL records and are not part of an explicit user transaction;
they are committed autocommit-by-construction. Their WAL frames never have a
surrounding `Begin`/`Commit` pair in the log, but `open_db` applies them
directly to the catalog in LSN order during replay.

String fields are serialized as: `u32 LE length` (in bytes) followed by
that many raw UTF-8 bytes. A zero length is valid (empty string).

## 4. dbdir.rs (engine code)

`crate::dbdir` provides:

- `create_layout(root) -> Result<()>` -- creates the seven subdirectories
  listed above.
- `write_meta(root) -> Result<()>` -- writes the 28-byte `db.meta` header
  and calls `sync_all()`.
- `read_meta(root) -> Result<MetaHeader>` -- reads and validates the
  `db.meta` header; returns structured version information, or the
  appropriate `Error` variant.
- `wal_path(root) -> PathBuf` -- returns `root/wal/wal.log`.

These are called from `Engine::create_db` and `Engine::open_db`, never
directly by application code.