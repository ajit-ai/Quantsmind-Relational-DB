# Recovery (Developer Guide, R2)

> R2 deliverable. How startup recovery reconstructs committed state.

The recovery path lives across `qmind-kernel` (replay + redo primitives) and
`qmind-sql` (catalog + index reconstruction). The exact procedure:

1. `dbdir::read_meta(root)` -- validates the 28-byte metadata header
   (magic + five format versions). Failure to read = `CatalogCorrupt`;
   version mismatch = `UnsupportedFormat`.
2. `WalReader::replay(&mut wal_file)` -- walks every frame. Returns the
   decoded records in LSN order plus `torn_tail`/`end_offset`.
3. If `torn_tail`, truncate the file to `end_offset` and `sync_data()`.
4. Rebuild the catalog: apply `CreateTable`/`CreateIndex`/`DropIndex`
   records in LSN order to `tables`/`indexes` maps.
5. `MvccStore::redo_from_records(&records)` -- applies each committed
   transaction's `Put`/`Delete`s; discards in-flight (Begin-without-Commit)
   writes; assigns ascending `commit_ts`; sets the commit watermark and
   `next_txn`.
6. Recompute `next_row_id` per table: `scan_prefix` over committed row
   keys, parse the trailing decimal row ID, take max + 1.
7. Rebuild each index B-Tree: iterate rids `0..next_row_id`, `get_raw`
   each row, encode the indexed column (skip NULL), insert into a fresh
   tree. Failure = `CatalogCorrupt`.
8. Open a `WalWriter` at `replay.end_offset` and `resume(n)` so future
   appends continue from the correct LSN.

## Failure semantics

- Interior corruption or invalid structure: `Err(WalCorrupt)`, no state
  fabrication.
- Torn tail: truncated silently -- the only tolerated partial-write case.
- Undecodable index row: `Err(CatalogCorrupt)`.
- Bad format/version: `Err(UnsupportedFormat)`.

## Why redo-only is sufficient

The engine is single-writer; every committed transaction is fully written
to the WAL before its commit frames. There are no physical pages to undo.
A transaction is either represented in the log with a terminating
`Commit` frame (redo it) or not (discard it). This is documented honestly
as "ARIES-style/logical WAL recovery", never "full ARIES" (ADR-013).