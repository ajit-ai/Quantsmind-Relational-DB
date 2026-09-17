# ADR-016 -- Transitional Page Reconstruction

- **Status**: Accepted (R3)
- **Date**: R3 architecture gate

## Context

R2 established that the WAL is the recovery authority: opening a database
replays the log and rebuilds the committed state in memory. R3 adds a
persistent page store, but does not (yet) add a full checkpointed-storage
lifecycle where pages are authoritative and the WAL only bridges the gap
between checkpoints.

## Decision

Keep the page store as a **derived structure**: on `open_db`, after WAL
recovery:

1. the stale `tables/` page segments from a previous session are removed;
2. every recovered table is re-registered in the storage manager;
3. every committed row is re-inserted into the page store.

This makes a page scan consistent with the MVCC/WAL view by construction. The
replacement architecture — durable, independently authoritative pages with
incremental flushing and WAL replay only for the post-checkpoint delta — is
future work and is **not** claimed by R3.

## Rationale

- Correctness: rebuilding from recovered state guarantees no page can be
  missing a committed row and no stale page can alias a rebuilt table.
- Simplicity: no page-store reconciliation, dirty-page tracking, or
  checkpoint coordination is needed yet.
- Transparency: the open-time cost scales with the recovered dataset, which is
  acceptable while datasets and single-writer scope are bounded.

## Consequences

- Reopening a database incurs a full page-store rebuild after WAL recovery.
- Page files are fully disposable: they can always be regenerated from the WAL.
- This architecture is explicitly transitional; the final billion-row
  architecture requires the checkpointed lifecycle (R6 qualification path).

## References

- `crates/qmind-sql/src/engine.rs` (`open_db`)
- `docs/architecture/wal-storage-ordering.rst`
- `docs/architecture/r3-architecture.rst`