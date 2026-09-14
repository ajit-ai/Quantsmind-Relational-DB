# ADR-013 -- Recovery Model

- **Status**: Accepted (R2)
- **Date**: R2 architecture gate

## Context

The engine needs a crash-recovery model that is correct, simple, and honest
about what it is. A full ARIES implementation (physical undo/redo page
logging, dirty-page tracking, compensation log records) is disproportionate
to the current engine's architecture and scope.

## Decision

R2 implements a **redo-only, logical (record-level) recovery model**
informed by ARIES concepts but not claiming to be ARIES:

- **No physical page logging.** The WAL carries logical records (Put,
  Delete, CreateIndex, etc.), not before/after page images. This is the
  same WAL format already used for the engine's in-memory structures.
- **No undo pass.** Uncommitted transactions are identified by the absence
  of their `Commit` frame; their writes are simply discarded. There is no
  compensation log record (CLR) and no undo phase.
- **Redo in LSN order** is sufficient because the engine runs under a single
  writer with no concurrent physical page writes to race.
- **Catalog DDL is replayed as part of the same WAL scan.** DDL records are
  committed by construction (autocommit); no explicit transaction wrapper
  exists around them.
- **Torn tail is the only silent correction.** Any other corruption fails
  the open loudly (`WalCorrupt`).

This model is described in documentation as "ARIES-style/logical WAL
recovery" at most; the phrase "full ARIES" is never used.

## Consequences

- Simple and testable: one WAL scan, one catalog rebuild, one MVCC redo,
  one index rebuild. The total recovery path is ~200 lines of engine code.
- No CLR, no undo, no dirty-page table -- much simpler than full ARIES,
  appropriate for the engine's current single-writer architecture.
- The model is a strict subset of what a full ARIES engine would provide;
  it is extended naturally when multi-writer MVCC (R4) arrives, which may
  add actual undo records.

## References

- `RECOVERY_INVARIANTS.md`
- `qmind-kernel/src/recovery.rs` (replay implementation)
- `qmind-kernel/src/wal.rs` (WAL reader/writer)