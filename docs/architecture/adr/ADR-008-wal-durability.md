# ADR-008 -- WAL Durability and Group Commit

- **Status**: Accepted (R2)
- **Date**: R2 architecture gate

## Context

R1 confirmed the WAL had no durability guarantee: frames were written but
never synced, and `commit_group` did not call `sync_data()`. A restart lost
all uncommitted and committed data indiscriminately. The product cannot
claim persistence without fsync discipline.

## Decision

- Every `Engine` autocommit (today, every SQL statement) calls
  `WalWriter::commit_group()`, which performs: `write_all()` -> `flush()`
  -> `syncer()`. The syncer is a function pointer
  (`fn(&mut W) -> io::Result<()>`) installed by `Engine` to call
  `f.sync_data()` on the WAL file.
- A failed syncer causes `commit_group` to return `Err`, and the pending
  group is cleared (no retry). This mirrors Postgres's PANIC-on-fsync-failure:
  the engine refuses to continue pretending data is durable.
- Coarser-grained modes (user-configurable `none`/`group`/`fsync-every-txn`)
  are deferred; today `fsync-every-txn` is the only mode. No user-facing
  config exists yet.
- The syncer type is a bare function pointer (`Option<fn(...)>`) to keep
  `Engine<W>` unconditionally `Send` and `Sync` without `Box<dyn FnMut>`.

## Consequences

- Every autocommit is an fsync point; write throughput is bounded by disk
  sync latency. This is acceptable at R2 scale.
- The engine no longer silently loses data on power loss.
- `close()` calls `commit_group()` for hygiene but durability never depends
  on it.

## References

- `qmind-kernel/src/wal.rs` (`WalWriter`, `commit_group`, `with_syncer`)
- `DURABILITY_CONTRACT.md`