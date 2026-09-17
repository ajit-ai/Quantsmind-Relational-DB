# ADR-014 -- StorageManager Integration

- **Status**: Accepted (R3)
- **Date**: R3 architecture gate

## Context

R2 made the WAL the durable authority and introduced a versioned page format
(`page.rs`, `buffer.rs`) with a `PageStore` trait, but no persistent table
layer: the SQL engine's committed state lived in the in-memory MVCC store and
was rebuilt from the WAL on open. R3 needs a persistent, scan-friendly row
store so batch queries can read committed data from disk instead of memory.

## Decision

Introduce a kernel-level `StorageManager<S: PageStore>` and wire it into the
file-backed `Engine`:

- The manager owns the `BufferPool<S>`, the table catalog
  (`HashMap<String, TableMeta>`), and the global allocators for table ids
  (`next_table_id`, from 1) and page ids (`next_page_id`, from 1; page 0
  reserved). A single global page counter guarantees page ids are unique
  across all tables.
- Per-table row storage is provided by a lightweight `TableStore` helper over
  the shared pool: append-only row chains across 8 KiB pages
  (`next_page_id`, `row_count`, and length-prefixed rows in each payload).
- `Engine.storage` is `Option<StorageManager<FilePageStore>>`; in-memory
  engines keep `None`.
- The manager does **not** own the WAL writer; WAL ordering is a documented
  contract the engine enforces (see ADR-015).

## Consequences

- Persistent tables now exist with deterministic page allocation and a lazy
  row cursor (`RowIter`) for bounded-memory scans.
- `create_db` initializes storage; `open_db` rebuilds it from recovered state;
  `create_table`/`insert` touch storage only after the WAL durability point;
  `close` flushes the WAL group before dirty pages.
- The storage layer is single-writer and transitional (pages are derived from
  the WAL — see ADR-016).

## References

- `crates/qmind-kernel/src/storage_manager.rs`
- `crates/qmind-kernel/src/table_store.rs`
- `crates/qmind-kernel/src/buffer.rs`
- `crates/qmind-sql/src/engine.rs`