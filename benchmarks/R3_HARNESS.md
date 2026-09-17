# R3 Benchmark Harness

> Workload-level, reproducible benchmark for the **R3 Execution Engine**.
> Companion to the cargo micro-benches (`crates/qmind-kernel/benches/`, R1)
> and the workload contracts in `benchmarks/workloads/`.

## Purpose

Measure the R3 execution features on a **real file-backed database**:

1. **Persistence & recovery** — close/reopen wall time, page-store rebuild.
2. **Streaming scan path** — full scan, filtered scan, projection, LIMIT
   (batch size 2048, bounded memory).
3. **Ordering** — ORDER BY (materialized fallback; the streaming path sorts
   in memory).
4. **Aggregation & GROUP BY** — count/sum/avg/group-by.
5. **Equi inner JOIN** — one-to-many hash join against a `dim` table.
6. **Insert throughput** — chunked inserts, fsync-per-statement today.

## Runner

- Binary: `crates/qmind-sql/src/bin/bench_r3.rs`
- Reproducible driver: `benchmarks/scripts/run_r3.ps1` (Windows) and
  `benchmarks/scripts/run_r3.sh` (POSIX). These capture the environment block
  (commit, rustc, CPU, RAM) and run the **release** build.
- Results: `benchmarks/results/r3/report-<scale>-r<iters>.md`

```text
cargo run -p qmind-sql --release --bin bench_r3 -- \
  --rows 1000000 --iterations 3 --out benchmarks/results/r3
```

## Dataset contract

Deterministic, fixed-seed synthetic data (LCG, seed `0x9E3779B97F4A7C15`):

```text
ticks(id INTEGER NOT NULL, sym TEXT NOT NULL, val INTEGER NOT NULL, g INTEGER NOT NULL)
dim  (dg INTEGER NOT NULL, label TEXT NOT NULL)
```

- `id` = row ordinal; `sym` = `s000..s999`; `val` = LCG-derived in `[0, 1e6)`;
  `g` = `id % 4`.
- `dim` has 4 rows (`dg = 0..3`) so every `ticks.g` matches exactly one row.
- Joint deterministic ⇒ the same report shape is reproducible across machines.

## Procedure

1. `create_db` in a scratch temp dir; `CREATE TABLE ticks` and `dim`.
2. Insert `--rows` rows in `--chunk`-row INSERT statements (500 default).
3. Execute each scan/ordering workload `--iterations` times via `stream_query`
   (bounded streaming) where supported; aggregate/join workloads via
   `execute()`.
4. `close`, then `open_db` (recovery + page rebuild), re-scan and verify the
   row count matches (refuses to write results on mismatch).
5. Record `wal.log` and `tables/` sizes; write the Markdown report.

## Metrics

| Metric | Unit | Source |
|---|---|---|
| rows processed | rows | sink output |
| mean wall per step | ms | `Instant::now()` |
| rows/s | rows/s | rows / (ms/1000) |
| close / open wall | ms | close() / open_db() |
| WAL size | bytes | `wal/wal.log` |
| table storage size | bytes | `tables/` recursive sum |

## Environment (recorded per report)

commit (git `HEAD`), rustc version, CPU, RAM, OS/arch, build mode, seed,
dataset rows, iterations, insert chunk.

## Acceptance linkage

| Workload | Contract |
|---|---|
| bulk insert | `benchmarks/workloads/bulk_insert.md` |
| full/filtered scan | `benchmarks/workloads/full_scan.md`, `filtered_scan.md` |
| sort | `benchmarks/workloads/sort.md` |
| aggregation / group by | `benchmarks/workloads/aggregation.md` |
| join | `benchmarks/workloads/join.md` |
| persistence | `benchmarks/workloads/recovery.md` |

Numbers are **real** only when produced by the harness; the README in
`benchmarks/results/r3/` states exactly what has and has not been measured.