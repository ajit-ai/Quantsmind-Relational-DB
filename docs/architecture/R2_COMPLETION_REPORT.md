# R2 Completion Report

> R2 deliverable -- `docs/architecture/R2_COMPLETION_REPORT.md`
>
> Final report for **R2 -- Durable Storage Foundation & Crash Recovery**.
> Evidence-based; every claim cites the file and line where the code lives,
> or the test that proves it. No R3/R5/R7/R8 work was implemented.

---

## 1. R2 objective

Make the engine durable and restart-recoverable: committed data survives any
process crash, uncommitted data is invisible after restart, DDL is durable,
indexes are coherent, the storage format is versioned, and the entire claim
is pinned by executable crash-recovery tests. Honest about what this is
*not*: no physical checkpointing, no group-commit batching, no multi-writer,
no on-disk row store.

## 2. Initial state

R1 shipped with 143 tests, a write-only WAL (no fsync, no replay in the
engine), in-memory catalog and indexes, and no persistent open/close
lifecycle. The engine had no durable storage -- a restart returned an empty
database. R2 fixes exactly that and nothing else.

## 3. WAL changes

- `WalWriter` gains `with_syncer(W, fn(&mut W) -> io::Result<()>)` and a
  `resume(next_lsn)` method. The syncer is a bare function pointer
  (`Option<fn(...)>`) to keep `Engine<W>` unconditionally `Send+Sync`.
- `commit_group()` now writes `write_all -> flush -> syncer` in that order;
  a failed syncer surfaces `Err` and clears the pending group.
- `WalRecord` gains three DDL variants: `CreateTable` (tag 5), `CreateIndex`
  (tag 6), `DropIndex` (tag 7). They carry no `txn` field; committed by
  construction (autocommit).
- `WalReader` gains frame-length helpers (`read_str`, `read_u32`) and the
  `replay()` method returns `ReplayResult { records, end_offset, torn_tail }`
  with the semantic: `end_offset` is always the offset of the first
  unwritten byte at every return path; structural invalidity
  (`len == 0 || len > MAX_RECORD_BYTES`) returns `Err(WalCorrupt)`.
- `MvccStore::redo_from_records()` walks `(lsn, WalRecord)` in file order
  to reconstruct committed state; `MvccStore::scan_prefix()` supports
  recomputing `next_row_id`.
  `qmind-kernel/src/wal.rs`, `qmind-kernel/src/mvcc.rs`.

## 4. Durability semantics

Every SQL autocommit is an fsync-point: append -> flush -> `f.sync_data()`.
A failed sync surfaces `Err` and refuses to acknowledge durability (Postgres
PANIC posture). The engine is single-writer, so LSN order = commit order.
`close()` calls `commit_group()` for hygiene but durability never depends on
it. See `DURABILITY_CONTRACT.md`, `ADR-008`.

## 5. Persistent catalog

DDL records (CREATE TABLE/INDEX, DROP INDEX) are appended to the WAL as
regular CRC-protected frames. On `open_db`, DDL records are applied to the
`tables`/`indexes` maps in LSN order before any data redo. No separate
catalog file exists in R2; the WAL is the only durable catalog store.
`qmind-sql/src/engine.rs` (recovery code in `open_db`), `ADR-009`.

## 6. Persistent storage

The engine does not have on-disk row store or on-disk index B-Trees in R2.
All committed data lives in the WAL; recovery replays the full WAL and
rebuilds the in-memory structures. `STORAGE_LAYOUT.md` describes the
canonical directory layout including reserved subdirectories for R3.
`qmind-sql/src/dbdir.rs`, `ADR-010`.

## 7. Index persistence/rebuild

Index definitions are durable (DDL WAL records). Index *content* is rebuilt
from committed rows on every `open_db`: iterate rids `0..next_row_id`,
fetch each committed row, encode the indexed column, insert into a fresh
in-memory B-Tree. A rebuild failure surfaces `CatalogCorrupt` and refuses
to open. `qmind-sql/src/engine.rs` (rebuild block in `open_db`),
`RECOVERY_INVARIANTS.md` section 6, `ADR-011`.

## 8. Recovery implementation

`Engine::open_db` runs: `read_meta` -> `WalReader::replay` -> optional
torn-truncation + fsync -> DDL catalog rebuild -> `redo_from_records` ->
`next_row_id` recomputation -> index rebuild -> `WalWriter::resume` at the
correct LSN. The total recovery path is ~200 lines of engine code.
`qmind-sql/src/engine.rs`, `qmind-kernel/src/recovery.rs`,
`RECOVERY_INVARIANTS.md`.

## 9. Checkpoint status

**Not implemented in R2.** The WAL grows without bound until R3 on-disk
structures and a checkpoint writer land. The `Checkpoint` WAL tag (tag 3)
exists for forward-compatibility but is never written. Recovery replays
the full WAL from LSN 1 on every open. `CHECKPOINTS.md`, `ADR-012`.

## 10. Crash testing

`crates/qmind-sql/tests/crash_recovery.rs` (7 tests, real subprocess
kills):
- Committed rows survive kill mid-loop
- Uncommitted durable writes (appended + fsynced, no Commit record) are
  rolled back
- Committed and uncommitted coexist in one log correctly
- Multiple tables survive crash
- Index DDL survives crash and lookups work immediately after reopen
- Repeated crash/restart cycles accumulate committed data
- **R2.25 acceptance scenario**: init -> committed inserts -> index DDL ->
  uncommitted writes -> verify counts + lookup -> three more restart cycles
  with accumulating committed data and index verification

The fixture binary `crates/qmind-sql/src/bin/qmind-persistence-fixture.rs`
uses `std::process::exit(3)` to simulate a hard crash (bypasses all
destructors), proving no clean-shutdown code path is required for durability.

## 11. Corruption testing

- `corrupted_frame_fails_replay_loudly` (kernel): interior frame CRC flip
  -> `Err(WalCorrupt { at: lsn })`.
- `torn_tail_end_offset_ignores_incomplete_trailing_frame` (kernel):
  trailing partial frame -> `torn_tail = true`, `end_offset` correct.
- `invalid_length_header_fails_replay` (kernel): header with
  `len > MAX_RECORD_BYTES` -> `Err(WalCorrupt)`.
- `interior_frame_corruption_fails_open_loudly` (engine): flips a payload
  byte in an existing frame -> `open_db` returns an error (not silent
  truncation).
- `torn_wal_tail_is_truncated_and_committed_prefix_kept` (engine): appends
  a valid-header frame with a short payload -> open truncates to last
  complete frame, committed rows intact.
  `qmind-kernel/tests/`, `crates/qmind-sql/tests/persistence.rs`.

## 12. Storage format versioning

`db.meta` (28 bytes) carries: 8-byte magic (`QMINDDB\0`), five `u32 LE`
version fields (meta, db, format, wal, catalog), all currently `1`. A
mismatch in any field returns `Error::UnsupportedFormat { entity, found,
expected }`. Foreign files (wrong magic) return `CatalogCorrupt`.
`qmind-sql/src/dbdir.rs`, `STORAGE_LAYOUT.md`, `ADR-010`.

## 13. Error handling

Two new kernel `Error` variants:
- `UnsupportedFormat { entity, found, expected }` -- format version gating
- `CatalogCorrupt { table, reason }` -- metadata / data integrity failure
  in the catalog or during index rebuild
Both have Display arms. WAL corruption is `WalCorrupt { at, reason }`.
All of these surface as `Error` to the caller, never as `unwrap()` panics.
`qmind-kernel/src/error.rs`, `DURABILITY_CONTRACT.md` section 5.

## 14. Documentation

R2 creates or updates:
- `docs/architecture/DURABILITY_CONTRACT.md`
- `docs/architecture/STORAGE_LAYOUT.md`
- `docs/architecture/RECOVERY_INVARIANTS.md`
- `docs/architecture/CHECKPOINTS.md`
- `docs/architecture/adr/ADR-008..013`
- `docs/operations/BACKUP_RECOVERY_MODEL.md`
- `docs/operations/backup-recovery.md`
- `docs/user-guide/durability.md`
- `docs/developer-guide/{storage-engine,wal,recovery}.md`
- `benchmarks/results/r2/README.md` (honest: NOT MEASURED)
- `docs/architecture/R2_COMPLETION_REPORT.md` (this file)

Terminology: "ARIES-style/logical WAL recovery" at most; "full ARIES" is
never claimed.

## 15. Performance measurements

**Not performed.** R2's gate is correctness, not throughput. The per-statement
fsync is the correct durability mechanism at R2 scale; throughput tuning
(group-commit batching, WAL segment rotation) arrives with R3.
`benchmarks/results/r2/README.md`.

## 16. Test results

| Crate | Suite | Count | Notes |
|-------|-------|------:|-------|
| qmind-kernel | lib | 70 | includes new WAL syncer, DDL roundtrip, resume, Display, corruption, torn tests |
| qmind-kernel | read_stress | 6 | |
| qmind-kernel | kernel_integration | 3 | |
| qmind-kernel | property | 6 | |
| qmind-sql | unit (parser) | 30 | |
| qmind-sql | crash_recovery | 7 | real subprocess kills via fixture |
| qmind-sql | persistence | 12 | in-process restart, torn-tail, corruption, format gate |
| qmind-sql | concurrency | 4 | |
| qmind-sql | fuzz | 4 | |
| qmind-sql | soak | 1 | |
| qmind-sql | e2e | 23 | |
| qmind-embed | unit | 2 | |
| qmind-server | unit | 2 | |
| **Total** | | **170** | up from 143 at R1 |

Regression accounting:
- R2.24: `corrupted_payload_stops_replay_at_checksum` renamed to
  `corrupted_frame_fails_replay_loudly` (the contract changed: bad CRC on
  a fully present frame is now loud corruption, not a silent stop).
- All other R1 tests remain green and unchanged.
- No R1 test was weakened or removed.

Gates:
```
cargo fmt --all -- --check                            PASS
cargo test --workspace                                PASS (170/170, 0 failed)
cargo clippy --workspace --all-targets --all-features -- -D warnings  PASS
```

## 17. Remaining limitations

- No physical checkpointing (WAL grows unbounded).
- No group-commit batching (every statement is an fsync-point).
- No multi-writer durable ordering (single writer).
- No on-disk row store or on-disk index B-Trees (data rebuilt from WAL).
- Columnar path is not rebuilt after reopen (`columnar_dir: None`).
- No online backup or point-in-time recovery.
- No user-visible BEGIN/COMMIT (DDL and DML are both autocommit).
- No benchmark measurements in R2.

## 18. R3 prerequisites

R2 delivers:
- A correct, tested WAL with fsync durability and full-log replay.
- A versioned, durable database directory layout.
- A durable catalog and the DDL lifecycle.
- Recovery invariants proven by real subprocess crash tests.
- Index rebuild from committed rows proven correct.
- Honest documentation with no overclaims.

R3 picks up with: on-disk row store + buffer manager + eviction,
on-disk index B-Trees, physical checkpointing, WAL segment rotation,
and the batch/vectorized execution foundation. The durability layer R2
delivers is the foundation R3 builds on; no R3 work undermines or
replaces it.

---

R2 STATUS: PASS

Durable commit: PASS
Restart persistence: PASS
Catalog persistence: PASS
WAL replay: PASS
Uncommitted rollback on recovery: PASS
Index recovery: PASS
Crash test: PASS
Corruption handling: PASS
Format versioning: PASS
Regression suite: PASS