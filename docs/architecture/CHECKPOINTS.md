# Checkpoints (R2 / deferred)

> R2 deliverable. Documents the checkpoint strategy and explains what is
> and is not implemented in R2.

## 1. R2 state (honest)

R2 does **not** implement physical checkpointing. There is no
`Checkpoint` record written by `Engine` on normal operation (the tag exists
in the WAL format for forward-compatibility). The database is recovered by
replaying the entire WAL from LSN 1 on every `open_db`.

The WAL grows without bound until R3 lands on-disk structures and a
`WalTruncator` or similar checkpoint writer. For small databases and
short test runs, this is not a practical problem. For production
deployment at scale, it is a hard blocker (the R2 non-goal list in
`DURABILITY_CONTRACT.md` names this explicitly).

## 2. Why this is acceptable in R2

- **R2's scope is durability proof, not performance.** The WAL replay
  contract is proven by crash-recovery tests; the speed of replay at R2
  sizes (test datasets) is irrelevant.
- **Checkpointing depends on an on-disk row store.** A real checkpoint must
  flush dirty pages and write a consistent snapshot header. Without on-disk
  rows and buffer manager (R3 scope), there are no dirty pages to flush.
- **R2 proves the recovery path is correct end-to-end.** Once the data is
  correct on restart, performance of that restart can be optimized later
  without changing the contract.

## 3. Planned design (R3+, deferred to ADR-013)

The planned checkpoint is based on a lightweight incremental approach:

- A new `Checkpoint { lsn, next_lsn }` frame is written periodically.
- On checkpoint: flush all dirty in-memory pages to disk (R3 buffer manager).
- On truncation: all WAL frames before the checkpoint's LSN are safe to
  discard (their data is now in the page files).
- On open: only WAL frames after the last checkpoint's LSN need to be
  replayed (the rest are in the snapshot).
- The checkpoint interval is configurable; the recommended default is
  "every N bytes of WAL growth" or "every M seconds" (TBD in ADR-013).

The `Checkpoint` tag (tag 3) is already present in the WAL record format
but is ignored by `redo_from_records` because no code writes it today.
This forward-compatible choice means checkpoint writes can land without a
migration.

## 4. What R2 proves anyway

- `WalReader::replay` correctly recovers the full committed state from LSN 1.
- `torn_tail` truncation works correctly for a mid-write crash.
- DDL catalog is rebuilt correctly from the full log.
- Index rebuild from committed rows works correctly after full replay.

These proofs remain valid under any future checkpoint implementation:
checkpoints make the same recovery path start later in the log, not run a
different path.