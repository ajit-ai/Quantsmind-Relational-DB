# Datasets

> `benchmarks/datasets/` — dataset definitions and generator contract.

## Canonical synthetic datasets

Deterministic generator (documented fixed seed; single code path shared by all
workloads so different workloads use identical data).

```text
ticks
  id      i64   primary key, dense 0..N-1
  symbol  char(8)   ~500 distinct symbols (Zipf-ish)
  ts      timestamp random within 365 days
  px      f64       random walk
  n       i64       uniform
  flag    bool      ~50% true

dim
  dim_id  i64   primary key, dense
  grp     i64   ~10 distinct
  name    text  random strings
```

Row width (dense) ≈ 40 bytes payload; suitable for both row-store and
column-store measurement and for 1B-row scale-out planning.

## Scale notes

- S1/S2: in-repo convenience tables (generated on demand).
- S3/S4: prepared with the scale generator only during R3/R6 qualification; the
  generated files are git-ignored (never commit gigabytes).
- Generator lives in `scripts/` or a `benchmarks/` bin crate in a later stage
  (R3); this directory is the definition + contract now.

## Git-ignore policy

Large generated files must be added to `.gitignore` (e.g. `benchmarks/datasets/**/*.bin`).
Only small metadata/schema manifests are committed here.

## Reproducibility

Every dataset manifest records: generator version, seed, row count, schema
version. Any result citing a dataset must cite its manifest (commit hash
implicitly via git).