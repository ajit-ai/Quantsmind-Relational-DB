# Recovery Invariants (R2)

> R2 deliverable. The exact rules by which `open_db` reconstructs the
> committed database state from the on-disk WAL + catalog metadata.

## 1. Startup recovery order

```
Engine::open_db(path):
  1. read_meta(path)        -- validates magic + all version fields
  2. WalReader::replay(wal) -- scans every frame, returns { records, end_offset, torn_tail }
  3. if torn_tail:          -- truncate file to end_offset; sync; set torn_tail flag
  4. apply DDL records      -- CreateTable / CreateIndex / DropIndex from replay.records
  5. MvccStore::redo_from_records(&replayed_records)
  6. recompute next_row_id  -- scan_prefix("{table}\x01") for max rid + 1
  7. rebuild index trees    -- for each index, iterate rid 0..next_row_id, get row, insert key
  8. open new WalWriter     -- resume at replay.end_offset (LSN = next_offset + 1)
```

Step 4-7 run under the single writer guard (no concurrent writes during open).

## 2. WAL replay rules

`WalReader::replay(cursor)` walks the file by reading frame headers (8
bytes), then the full payload, validating the CRC32 on each frame.

### Normal termination

When the next read returns EOF after one or more complete frames, replay
stops. `end_offset = pos` (file length). `torn_tail = false`.

### Torn tail

If the frame header is fully present (`pos + 8 <= len`) but
`pos + 8 + payload_len > len` -- the payload was never fully written. This
is the **only** case where trailing data is silently discarded: replay
returns `torn_tail = true` and `end_offset = pos` (the offset just after the
last *complete* frame). The caller truncates the file to `end_offset` and
then opens normally.

### Corruption (not a tear)

Any of these is classified as **corruption** and makes replay return
`Err(Error::WalCorrupt { at: lsn, reason })`:

- Header present with `payload_len == 0` or `payload_len > MAX_RECORD_BYTES`
- Frame fully present but CRC32 mismatch
- Payload bytes do not decode as a valid `WalRecord` tag

Corruption fails loudly. The engine never guesses, never silently truncates
an interior frame, and never skips past an interior corrupt record.

### Mid-header tear

If the file has only a partial header (`pos < len && pos + 8 > len`), this
is treated identically to the torn-tail case above: the partial bytes are
dropped; `torn_tail = true`.

## 3. Catalog rebuild (DDL replay)

DDL records (`CreateTable`, `CreateIndex`, `DropIndex`) do not carry a
transaction field; they are committed by construction (autocommit).
`open_db` applies them in LSN order:

- `CreateTable { name, columns }` -- inserts into the `tables` HashMap.
- `CreateIndex { name, table, column }` -- inserts into the `indexes` HashMap.
- `DropIndex { name }` -- removes from the `indexes` HashMap.

This rebuilds the exact catalog the committed DDL history implies. The WAL
crash-injection harness proves that an uncommitted DDL (Begin without Commit
wrapping the DDL -- the engine never produces this today, but the replay
path explicitly ignores any DDL outside a committed transaction) is not
applied.

## 4. MVCC redo (`MvccStore::redo_from_records`)

The redo function walks `(lsn, WalRecord)` in file order:

- `Begin` -- records `{ txn, start_lsn: lsn }` in a local in-flight map.
- `Put` / `Delete` -- buffers the write in the in-flight map for that txn.
- `Commit` -- moves the in-flight writes into the committed state. Assigns
  `commit_ts = next_txn` (strictly ascending LSN order guarantees monotonic
  timestamps). Increments `next_txn`.
- `Checkpoint`, `CreateTable`, `CreateIndex`, `DropIndex` -- skipped (no
  data-plane effect in redo).
- Any `Begin` without a matching `Commit` after it -- the writes for that
  transaction are discarded (in-flight, not committed).

After replay, `mvcc.commit_watermark = max_committed_ts`. The stored
`mvcc.next_txn` is set to `max_committed_ts + 1`, guaranteeing new
transactions start above the highest previously committed timestamp.

## 5. next_row_id recomputation

The engine does not persist `next_row_id`; it is recomputed from the
committed data. For each table, `scan_prefix(table_key_prefix)` iterates
all committed `Row` entries in the MVCC store. Each row key ends with the
10-digit decimal representation of the row ID. The highest observed value
plus one becomes the new `next_row_id`.

This is deterministic because `commit_ts` is assigned in LSN order, and
the scan iterates in key order. A new transaction inserting at the
recomputed `next_row_id` never collides with any previously committed row.

## 6. Index rebuild (R2)

R2 indexes are rebuilt from the committed row data on every `open_db`:

1. For each index definition in the catalog, create a fresh in-memory B-Tree.
2. Iterate row IDs from `0` to `next_row_id - 1`.
3. For each ID, fetch the raw bytes via `MvccStore::get_raw`.
4. If the row exists and the indexed column value is not NULL, insert
   `index_key -> rid` into the B-Tree.
5. After iteration, the index tree is identical to what it would have been
   if it had been maintained transactionally during the original inserts.

A rebuild failure (e.g., a row stored in a format the index key encoder
cannot handle) surfaces as `Error::CatalogCorrupt { table, reason }` and
refuses to open. This is the integrity failure mode: rather than open with
a half-built index, the engine fails loudly.

## 7. Resume

After steps 2-7, `open_db` opens a new `WalWriter` positioned at the first
unwritten byte after the replayed log, and calls `WalWriter::resume(n)`
where `n = next_lsn` (LSN of the first new frame to write, equal to the
number of the next transaction the engine will run). `durable_lsn` is set
to `next_lsn - 1` (the LSN of the last frame currently in the file).

New inserts now append beyond the old log tail and get their own LSNs,
strictly monotonically increasing. No LSN reuse occurs across restarts.

## 8. Post-recovery state guarantees

On `Ok(...)` return from `open_db`:

- `tables` contains exactly the DDL history of all committed CREATE/DROP.
- `indexes` contains the index definitions matching the current catalog.
- Every committed transaction's Put/Delete values are present in the MVCC
  store, visible to the next reader.
- No in-flight or aborted transaction is visible.
- `next_row_id` for every table is above every existing committed row ID.
- The WAL writer is ready to append from the next available LSN.

These are the invariants proven by `persistence.rs` and `crash_recovery.rs`.