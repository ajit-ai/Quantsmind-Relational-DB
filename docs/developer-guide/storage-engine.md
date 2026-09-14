# Storage Engine (Developer Guide, R2)

> R2 deliverable. How the engine durability layer fits together.

## Lifecycle APIs (`crates/qmind-sql/src/engine.rs`)

For file-backed databases, the engine gains three durable lifecycle
functions on `impl Engine<std::fs::File>`:

```rust
Engine::create_db(path) -> Result<Self, Error>   // fresh database
Engine::open_db(path)   -> Result<Self, Error>   // recover existing database
Engine::close(self)     -> Result<(), Error>     // flush + release
```

- `create_db` writes `db.meta` + the reserved directory tree, then opens a
  WAL writer with the fsync syncer. It fails if the directory already
  contains a database.
- `open_db` runs the full recovery pipeline: meta validation, WAL replay,
  torn-tail truncation, DDL catalog rebuild, MVCC redo, `next_row_id`
  recomputation, index rebuild, then resumes the WAL at the correct LSN.
- `close` calls `commit_group()` to flush/sync any pending WAL group, then
  frees resources. It is hygiene, not a durability requirement.

The generic `Engine::new(wal_sink)` from R1 remains for in-memory /
sink-based usage (embed tests, in-memory servers).

## DDL autocommit

`create_table`, `create_index`, and `drop_index`:

1. Validate the statement (name collisions, column references, existing
   indexes).
2. Append the corresponding DDL `WalRecord` and `commit_group()`.
3. On WAL failure, return `Err` and mutate nothing in memory.
4. Only a successful WAL commit mutates the in-memory catalog.

This ordering (log first, then apply) is what makes DDL survive a crash
between steps.

## WAL writer syncer

`WalWriter` is generic over the sink. `WalWriter::with_syncer(sink, fn)` is
the durable constructor; the engine passes the module-level `sync_file`
function (`f.sync_data()`). The syncer runs inside `commit_group` after
`flush()`, so no byte is acknowledged before it is durable.

## dbdir helper (`crates/qmind-sql/src/dbdir.rs`)

See `STORAGE_LAYOUT.md` for the on-disk map. `dbdir` owns all path and
binary-header logic; engine callers never construct paths themselves.

## Tests that pin this down

- `crates/qmind-sql/tests/persistence.rs` -- in-process create/insert/
  close/reopen, index rebuild, torn-tail truncation, corruption failure,
  version gating.
- `crates/qmind-sql/tests/crash_recovery.rs` -- real subprocess kills
  proving committed data survives and uncommitted data rolls back.