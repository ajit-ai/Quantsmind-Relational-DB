# ADR-009 -- Persistent Catalog and DDL Journaling

- **Status**: Accepted (R2)
- **Date**: R2 architecture gate

## Context

R1 stored the catalog (table definitions and index definitions) in
in-memory `HashMap`s populated at runtime from `CREATE TABLE`/`CREATE
INDEX` statements. A restart discarded all DDL; the engine opened empty.
The product cannot claim persistence without durable schema.

## Decision

- DDL records (`CreateTable`, `CreateIndex`, `DropIndex`) are appended to
  the WAL as regular CRC-protected frames, with no enclosing `Begin`/`Commit`
  (autocommit-by-construction).
- `Engine::open_db` replays the full WAL in LSN order and applies DDL
  records to rebuild `tables` and `indexes` maps before any data redo.
- On-disk: the WAL is the only durable store of DDL history in R2. No
  separate catalog file exists (reserved `catalog/` directory is created
  but unused; R3 may add a catalog snapshot to reduce replay length).
- Format version is recorded in `db.meta` and validated on open; version
  mismatches fail with `Error::UnsupportedFormat`.

## Consequences

- DDL is durable and restarts recover it correctly.
- No need for a separate WAL segment or DDL journal file; the existing WAL
  infrastructure carries DDL with the same fsync and tear-recovery
  guarantees.
- The autocommit-only model (no user `BEGIN`/`COMMIT` around DDL) keeps
  the replay rules simple: DDL records are committed or absent, never
  in-flight.

## References

- `qmind-kernel/src/wal.rs` (DDL record types, tags 5/6/7)
- `qmind-sql/src/engine.rs` (`create_db`, `open_db`, DDL apply in recovery)
- `STORAGE_LAYOUT.md`