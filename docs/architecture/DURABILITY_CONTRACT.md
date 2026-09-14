# Durability Contract (R2)

> R2 deliverable. Defines exactly what "committed" means and what survives a
> crash, plus the non-goals that keep the claim honest.

## 1. Durable entities

A transaction is **committed** only when its bytes have crossed the WAL
durability boundary. Every `Engine` autocommit performs, in order:

1. append the transaction's WAL records (`Begin`..`Put`..`Commit` frames),
2. `flush()` the WAL file handle,
3. `sync_data()` on the underlying file (the syncer),
4. only then return success to the caller.

`commit_group()` in the kernel implements exactly this: `write_all` ->
`flush` -> `syncer` (`fn(&mut W) -> io::Result<()>`), with a failed sync
surfacing as an error and leaving no pending group to retry. The SQL engine
installs `sync_file` (`f.sync_data()`) as the syncer for file-backed databases.

Coarser-grained durability modes (group commit across many transactions,
`none`/`fsync-every-txn` policies) are deferred; today every autocommit is an
fsync-point. This is the simplest mode that satisfies the contract and there
is no user-facing toggle yet.

## 2. What a crash may and may not lose

After any process death (panic, killed, power loss, `std::process::exit`),
opening the database with `Engine::open_db` yields a state where:

| Guarantee                                        | Enforced by |
| ------------------------------------------------ | ----------- |
| Every transaction whose `commit()` returned `Ok` is present | fsync-before-return; WAL records replayed in LSN order |
| No transaction that never committed is visible   | redo replays only records whose `Commit` frame is in the log |
| DDL that returned `Ok` (CREATE/DROP TABLE/INDEX) survives | DDL records are autocommitted WAL records |
| A torn tail (partial final frame) is discarded, prefix intact | replay returns `torn_tail`; `open_db` truncates to `end_offset` |
| Interior corruption is detected, never guessed   | CRC + structural validation fail the open loudly (`WalCorrupt`) |
| Indexes are coherent with committed rows after restart | catalog DDL persisted; index trees rebuilt from rows on open |
| Format mismatches refuse to open                  | versioned `db.meta` gate |

The durability boundary test (`crash_recovery.rs`) proves these against real
subprocess crashes that bypass `close()`, including:
- committed rows survive a kill mid-write-loop,
- durable-but-uncommitted WAL bytes (appended + fsynced, no `Commit` record)
  are rolled back on restart,
- index DDL survives a kill and lookups work immediately after reopen,
- repeated kill/restart cycles accumulate committed data exactly.

## 3. Non-goals (honest limits)

- **No physical checkpointing yet.** R2 recovery replays the full WAL from
  LSN 1 on every open. WAL size therefore grows unbounded until R3
  checkpoints land (`CHECKPOINTS.md`, `ADR-013`).
- **No user-visible transactions.** Every SQL statement autocommits; the
  `insert-uncommitted` fixture prepares an uncommitted group via the kernel
  `WalWriter` to exercise the rollback path.
- **No multi-writer durability ordering.** Single writer still; the durable
  ordering contract among concurrent writers arrives with R4 multi-writer.
- **No disk-write flush for the (still in-memory) row/index stores.** The
  stores are rebuilt from the WAL on open; nothing else is on disk yet.
- **No claim of full ARIES.** This is an append-only, redo-only, logical
  (record) recovery model. The terms used in this repository are
  "ARIES-style/logical WAL recovery" at most; "full ARIES" is never claimed
  because there is no physical undo/redo page logging.
- **`close()` is best-effort hygiene, not a durability mechanism.** The
  engine flushes on close, but durability never depends on it — every crash
  test kills the process without running destructors.

## 4. Failure postures

- **fsync failure during commit**: the statement returns an error; the
  pending group is discarded (no retry loop). Policy mirrors Postgres's
  PANIC-on-fsync-failure stance: after losing trust in the storage
  subsystem, the engine refuses to continue pretending data is durable.
- **Corrupt interior frame**: `open_db` fails with `WalCorrupt { at: LSN }`.
  Replay never fabricates state, never silently drops an interior record,
  never guesses a length.
- **Torn tail**: structural `pos + 8 + len > file_len` for the final frame
  is the *only* silent-truncation case, and it is exactly the mid-write
  crash case; the LSN-prefix that precedes it is intact by construction
  (frames are appended, never overwritten).

## 5. Verification

- Kernel: `WalWriter` fsync-ordering test, `resume` LSN continuation, torn
  tail `end_offset` reconstruction, invalid-length-header corruption, DDL
  record roundtrip.
- Engine: `persistence.rs` (12 tests: reopen preserves committed rows,
  index rebuild, torn-tail truncation, loud corruption failure, format
  version gate). `crash_recovery.rs` (7 tests, real subprocess kills,
  including the R2.25 acceptance scenario).

See `RECOVERY_INVARIANTS.md` for the replay rules and
`STORAGE_LAYOUT.md` for the on-disk files that carry this contract.