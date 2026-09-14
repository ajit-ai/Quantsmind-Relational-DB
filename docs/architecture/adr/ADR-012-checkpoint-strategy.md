# ADR-012 -- Checkpoint Strategy

- **Status**: Accepted (R2) -- design documented, implementation deferred
- **Date**: R2 architecture gate

## Context

Without checkpoints, recovery replays the full WAL from LSN 1 on every
open. The WAL grows unboundedly. For production use at scale, this is a
hard blocker: startup time becomes proportional to the entire write history,
not just recent activity.

## Decision

Physical checkpointing is **not implemented in R2** and is explicitly
deferred to R3. The reasons:

1. Checkpointing requires an on-disk row store and buffer manager to flush
   dirty pages. R2 keeps the row store in memory, rebuilt from the WAL; there
   are no dirty pages to flush.
2. R2's scope is proof-of-correctness for durability, not performance. The
   full-WAL replay is correct and testable at R2's data scale.
3. The `Checkpoint` WAL tag (tag 3) exists in the format for forward-
   compatibility but is never written by the engine today.

The planned R3 design:

- Write a `Checkpoint { lsn, next_lsn }` frame to the WAL periodically.
- Flush all dirty in-memory pages to on-disk page files (R3 buffer manager).
- On open: start replay from the last checkpoint's LSN, not from LSN 1.
- Truncate WAL frames before the checkpoint LSN (they are in the page files).

## Consequences

- R2 recovery is simpler (no checkpoint resume logic), with the trade-off
  of slower restart at large data volumes.
- The `Checkpoint` tag's early inclusion means R3 lands without a WAL format
  migration.
- The approach is opt-in: databases created in R2 open in R3 without any
  migration. Checkpoint writes simply begin after R3 is deployed.

## References

- `CHECKPOINTS.md`
- `PRODUCTION_ROADMAP.md` (R2 acceptance gate, R3 scope)
- `DURABILITY_CONTRACT.md` (non-goals section)