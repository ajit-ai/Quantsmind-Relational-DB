# R2 Benchmark Results -- NOT MEASURED

> Honest status: R2 did **not** run the `recovery` workload
> (`benchmarks/workloads/recovery.md`) at any scale.

R2's acceptance gate hinges on correctness of the crash/recovery contract,
which is pinned by executable tests, not microbenchmarks:

- `crates/qmind-sql/tests/crash_recovery.rs` (7 tests, real subprocess
  kills)
- `crates/qmind-sql/tests/persistence.rs` (12 tests, in-process restart
  round-trips)
- `qmind-kernel` WAL/recovery/scratch property tests

Per the repository's benchmark discipline ("no results until real runs,
never invent numbers"), the `wf_recovery` measurements named in
`PRODUCTION_ROADMAP.md` R2 are recorded here as **not performed**.

Planned for R3 when recovery replay may still be full-WAL (checkpointing
deferred): measure (a) restart wall time vs WAL bytes, (b) WAL append
throughput with the fsync-per-commit syncer, on the S1/S2 profiles
described in `benchmarks/datasets/README.md`.

## R2 numbers that ARE real

- 170 workspace tests green (was 143 at R1) -- see
  `docs/architecture/R2_COMPLETION_REPORT.md` `Tests executed`.
- Every committed row survives a hard `std::process::exit` crash; every
  uncommitted group is rolled back -- proven by
  `crash_recovery.rs::r2_25_acceptance_crash_recovery_scenario`.

Nothing else in this directory is a measured performance claim.