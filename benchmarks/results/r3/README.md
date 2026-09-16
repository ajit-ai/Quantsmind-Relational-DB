# R3 Benchmark Results

> Honest status: results here come **only** from real runs of the R3 harness
> (`crates/qmind-sql/src/bin/bench_r3.rs`). No number is invented; a metric
> that was not measured is not listed.

## How to reproduce a run

```powershell
# Windows (records commit, rustc, CPU, RAM into the report)
benchmarks\scripts\run_r3.ps1 -Rows 100000 -Iterations 3
```

```sh
# POSIX
bash benchmarks/scripts/run_r3.sh --rows 100000 --iterations 3
```

Reports land here as `report-<scale>-r<iters>.md`, with the environment block
(commit, rustc, CPU, RAM, OS, build mode) written into each file.

## Status

| Scale | Report | Executed on host |
|---|---|---|
| 1K | `report-1K-r1.md` | smoke run, debug build (see note) |
| 100K | `report-100K-r3.md` | release run via `run_r3.ps1` |
| … | (pending real runs) | |

> Note: smoke runs use a **debug** build and are not qualification numbers.
> Release-mode numbers are added when runs are executed on the reference
> profile defined in `benchmarks/R3_HARNESS.md`.

## What is measured and how

- Aggregation / GROUP BY run through the **batch aggregate pipeline**
  (`R3-EXEC-1`) over persistent storage (the 100K report reflects this).
- JOIN runs through the **batch hash join pipeline** (`R3-EXEC-2`) with both
  inputs materialized from persistent storage (the 100K report reflects this).
- Simpler shapes (scan / WHERE / projection / LIMIT) stream in bounded batches;
  ORDER BY and the aggregate/join inputs are materialized in RAM (spill
  deferred).

## What is NOT measured yet

- Ordering with bounded memory for the full table: ORDER BY materializes.
- Spill-to-disk for aggregate / join / sort inputs.
- `wf_recovery` (crash/restart wall time vs WAL bytes): checkpointing is
  still deferred; open/reopen timings are present, crash-replay is not.