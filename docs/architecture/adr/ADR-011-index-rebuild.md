# ADR-011 -- Index Persistence and Rebuild Strategy

- **Status**: Accepted (R2)
- **Date**: R2 architecture gate

## Context

R1 indexes were in-memory `BTree` maps, populated at runtime from DML.
A restart discarded them. The product cannot claim index durability without
persisting either the index structure itself or the data needed to rebuild
it.

## Decision

- R2 does **not** persist index B-Tree structure on disk. Index trees are
  rebuilt from committed row data on every `open_db`.
- The index *definitions* (which table, which column) are persisted via
  DDL WAL records (ADR-009) and are part of the catalog rebuild.
- On `open_db`, after DDL replay and MVCC redo, each index is rebuilt by
  iterating row IDs from `0` to `next_row_id - 1`, fetching committed row
  bytes, and inserting the indexed column value (skipping NULLs) into a
  fresh in-memory B-Tree.
- A rebuild failure (e.g., a row in a format the index encoder cannot handle)
  surfaces as `Error::CatalogCorrupt` and refuses to open.

## Consequences

- Simple and correct: index contents are always coherent with the committed
  row data. No separate index WAL or index-page flush needed.
- Recovery time is proportional to committed data volume (not just WAL
  length). This is acceptable for R2; R3 on-disk indexes will avoid the
  full rebuild.
- No separate disk space used for indexes in R2.

## References

- `qmind-sql/src/engine.rs` (index rebuild in `open_db`)
- `RECOVERY_INVARIANTS.md` (section 6)