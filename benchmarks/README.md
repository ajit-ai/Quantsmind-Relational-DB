# QuantsMind DB Benchmarks

> R1 deliverable — `benchmarks/`
>
> Benchmark structure and workload specifications for the production roadmap.
> **Results are never fabricated.** This directory defines the *how*; actual
> numbers are filled only by real runs and published in `results/`.

## Layout

```text
benchmarks/
├── README.md         ← this file
├── workloads/        ← 10 workload specifications (markdown contracts)
├── datasets/         ← dataset definitions + generator contract
└── results/          ← real, reproducible results (empty until runs happen)
```

## Scales

| Scale | Rows |
|---|---|
| S1 | 1M |
| S2 | 10M |
| S3 | 100M |
| S4 | 1B |

S4 generation is **not** an R1 requirement (R1 requires the benchmark
architecture and workload definitions only). Generation tooling lands with the
R3/R6 qualification stages.

## Shared dataset contract

The canonical synthetic dataset (see `datasets/README.md`):

```text
ticks (symbol char(8), ts timestamp, px f64, n i64, flag bool, id i64 PK)
dim   (dim_id i64 PK, grp i64, name text)
```

Deterministic generator (fixed seed) so all results are reproducible across
machines and stages.

## Workload registry

| File | Workload | Primary scale target |
|---|---|---|
| `workloads/point_lookup.md` | Indexed point lookup | S1–S4 |
| `workloads/bulk_insert.md` | Batched insert throughput | S1–S4 |
| `workloads/filtered_scan.md` | Selective (≤1%) scan | S1–S4 |
| `workloads/full_scan.md` | Full-table sequential scan | S2–S4 |
| `workloads/aggregation.md` | GROUP BY / aggregates | S1–S4 |
| `workloads/join.md` | Equi hash join (multi-table) | S1–S4 |
| `workloads/sort.md` | ORDER BY full table | S1–S4 |
| `workloads/mixed_oltp.md` | Concurrent OLTP mix | S1–S4 |
| `workloads/htap.md` | Concurrent OLTP writes + OLAP reads | S2–S4 |
| `workloads/recovery.md` | Crash/restart/checkpoint behavior | S1–S4 |

Each workload file contains: objective, SQL/shape, measurement contract
(steps, metrics, environment to record), and acceptance linkage to
`docs/architecture/BILLION_ROW_REQUIREMENTS.md`.

## Results policy

- `results/{S1,S2,S3,S4}/` holds per-scale reports from real runs only.
- Every report must record: machine (CPU/RAM/disk), commit hash, build mode
  (`release`), seed, date, tool version.
- A number with no run is a **failed report**, not a placeholder. Correlate
  every metric with its source run ID.

## Relationship to existing cargo benches

`crates/qmind-kernel/benches/kernel_bench.rs` micro-benchmarks kernel
primitives and is gated by CI (`cargo check --workspace --benches`). This
`benchmarks/` tree is the **macro/workload-level** harness companion to those
micro-benches; both are kept.

## Roadmap linkage

- R1: structure + workload definitions (this tree).
- R3/R6: generation tooling, first real S1–S3/S4 runs, results published.
- R9: soak/rerun; R10: full GA report.