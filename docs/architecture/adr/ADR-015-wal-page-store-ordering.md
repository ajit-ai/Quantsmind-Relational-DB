# ADR-015 -- WAL-to-Page-Store Ordering

- **Status**: Accepted (R3)
- **Date**: R3 architecture gate

## Context

The SQL engine uses a logical, redo-only WAL (ADR-008, ADR-013). R3 adds a
persistent page store that is written back asynchronously by a buffer pool
(eviction and explicit flush). If pages could be durably written ahead of the
log records for the same commit, a crash could persist page state that the WAL
says never committed, and recovery (which trusts the WAL) would disagree with
the page scan.

## Decision

Enforce strict write-ahead ordering between the WAL and the page store:

```text
WAL write
   ↓
commit_group → flush → sync_data
   ↓
page-store update
   ↓
dirty page flush → FilePageStore → sync
```

- `insert` commits the WAL group (fsync) **before** appending the same rows to
  persistent storage.
- `close` flushes/syncs any pending WAL group **before** flushing dirty pages.
- `StorageManager::flush` documents that the caller must have synced the WAL
  first; the engine is responsible for upholding this.
- Pages may be written back at any time after the WAL durability point —
  never before it.

## Consequences

- Recovery never depends on the page store being clean or in sync with the
  log; the WAL alone can reconstruct committed state.
- The page store is strictly a lagging, derived structure (see ADR-016).
- The invariant is enforced by the engine's insert/close paths, not by the
  storage manager itself.

## References

- `crates/qmind-kernel/src/storage_manager.rs` (ordering contract)
- `crates/qmind-sql/src/engine.rs` (insert / close paths)
- `docs/architecture/wal-storage-ordering.rst`