# Production Roadmap (R1–R10)

> R1 deliverable — `docs/roadmap/PRODUCTION_ROADMAP.md`
>
> The new production roadmap from R1 through **R10 = General Availability
> Certification**. R1 establishes the plan; only R1 is implemented here.
> Benchmarks referenced are the stems defined in `benchmarks/`.

Every stage defines: objective, dependencies, implementation areas, tests,
benchmark requirements, and an acceptance gate. "Qualification runs" below
means executing the real workloads in `benchmarks/` and recording results in
`benchmarks/results/` — never fabricating numbers.

---

## R1 — Architecture Realignment

- **Objective**: complete, evidence-based architecture assessment; decisions
  for every subsystem (KEEP/REWORK/REPLACE/REMOVE/DEFER); billion-row
  foundation documented.
- **Dependencies**: none (audit of existing repo).
- **Implementation areas**: storage, transactions, durability, query engine,
  SQL surface, interfaces; minimal code changes only (no engine rewrites).
- **Tests**: `cargo fmt --all -- --check`; `cargo test --workspace`;
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  (repo CI runs the documented `--all-targets` variant legally);
  Sphinx `-W` docs build.
- **Benchmark requirements**: create `benchmarks/` structure + 10 workload
  specs at scales 1M/10M/100M/1B. No runs required.
- **Acceptance gate**: assessment + tech decision + target architecture +
  boundaries + studio decision + billion-row requirements + ADRs +
  completion report committed; gates green; `R1 STATUS` reported. No R2
  implementation begins automatically.

---

## R2 — Storage Engine (durability first)

- **Objective**: a durable, restart-recoverable engine.
- **Dependencies**: R1 decisions.
- **Implementation areas**:
  - WAL: fsync-disciplined group commit, WAL segment files, flush semantics
    configurable (`none`/group/fsync-every-txn).
  - Recovery: startup open→replay→catalog-rebuild pipeline
    (`recovery.rs` wired into `Engine::new`/server/main `start`), torn
    record resumption.
  - Catalog: versioned, durable system catalog + DDL journaling (CREATE
    TABLE/INDEX become recoverable).
  - Checkpointing: LSN-based checkpoint, dirty-page flush, WAL truncation.
- **Tests**: crash-injection restart tests (kernel property + integration);
  end-to-end "insert → kill → restart → data present".
- **Benchmark requirements**: `wf_recovery` at S1/S2; baseline
  `wf_bulk_insert`.
- **Acceptance gate**: kill-test restart always restores last committed state;
  DDL survives restart; recovery time bounded at S2; gates green.

---

## R3 — Storage Engine (scale) + Batch Execution

- **Objective**: on-disk row store + indexes + real buffer management;
  batch execution foundation.
- **Dependencies**: R2 (durability).
- **Implementation areas**:
  - Row store: slotted on-disk pages, MVCC version rows, heap.
  - Buffer pool: resizable, eviction policy, write-behind coordinated with
    checkpoints; free-space management.
  - Indexes: disk-resident B+Tree; UNIQUE/PK enforcement.
  - Execution: batch operator interface (`Chunk` in/out), vectorized
    expressions, spill operators (sort/agg), per-query memory budgets
    (ADR-006).
  - Columnar hardening: RLE-null verification (flag from R1), zone maps.
- **Tests**: buffer/eviction correctness, spill determinism, batch equivalence
  vs row-at-a-time reference results.
- **Benchmark requirements**: `wf_point_lookup`, `wf_filtered_scan`,
  `wf_full_scan`, `wf_sort` at S2/S3; columnar comparison at S3.
- **Acceptance gate**: S3 scans/sorts within fixed memory budget; restart
  invariants preserved; spill correctness proven.

---

## R4 — Transactions & Reliability

- **Objective**: multi-writer OLTP, SQL transactions, robust recovery at
  scale.
- **Dependencies**: R3.
- **Implementation areas**:
  - Multi-writer with row-level versioning + conflict detection; lock manager
    + deadlock detection; txn-scoped snapshots; MVCC GC/watermark on
    checkpoint.
  - SQL surface: `UPDATE`/`DELETE`, `BEGIN`/`COMMIT`/`ROLLBACK`, statement
    atomicity in batches.
  - Columnar pushdown: predicate/aggregation pushdown into columnar scan.
- **Tests**: concurrency stress (multi-writer + readers), deadlock
  resolution, GC/watermark invariants.
- **Benchmark requirements**: `wf_mixed_oltp`, `wf_htap` at S2/S3.
- **Acceptance gate**: N≥8 writers with readers meet the workload contract;
  HP isolation preserved; GC bounds heap growth at S3.

---

## R5 — SQL & Optimizer

- **Objective**: real query pipeline (binder → semantic analysis → logical
  plan → optimizer → physical plan) mapped to the batch executor.
- **Dependencies**: R3 (executor), R2 (catalog).
- **Implementation areas**: binder/analyzer (`REPLACE` per assessment),
  logical/physical planner, rule+cost optimizer with catalog statistics,
  full NULL three-valued semantics, constraints (PK/UNIQUE/FK), subqueries/
  CTE, partitioning (range/hash) + pruning, join-order selection.
- **Tests**: SQL conformance suite per feature; planner equivalence tests.
- **Benchmark requirements**: `wf_aggregation`, `wf_join`, `wf_htap` at S3.
  Partition pruning verified via EXPLAIN.
- **Acceptance gate**: optimizer chooses scan/join order; plans respect
  memory budgets; conformance suite green.

---

## R6 — Billion-Row Qualification

- **Objective**: prove the S4 (1B) envelope on a single machine.
- **Dependencies**: R5.
- **Implementation areas**: scale testing, buffer/cache tuning, column-store
  compression, spill tuning; publish real results in `benchmarks/results/S4`.
- **Tests**: workload checkpoint at every S1–S4 class
  (`BILLION_ROW_REQUIREMENTS.md`), including `wf_recovery` at S4.
- **Benchmark requirements**: full matrix of 10 workloads × 1M/10M/100M/1B.
- **Acceptance gate**: S4 workloads run within documented memory budgets and
  produce correct reference-verified results; no fabricated numbers.

---

## R7 — Production Server

- **Objective**: production-grade server and embedding.
- **Dependencies**: R4/R5 (engine), R6 (scale evidence).
- **Implementation areas**: extended protocol (`Parse/Bind/Execute`),
  prepared statements, typed OIDs, SCRAM auth + TLS, connection pooling +
  resource limits (max connections, query timeouts, memory caps), slow-query
  log, metrics; embed API gains snapshot reads + streaming results.
- **Tests**: protocol conformance (psql/driver parity), auth/TLS matrix,
  resource-limit behavior.
- **Benchmark requirements**: `wf_mixed_oltp` over network; driver
  interoperability checks.
- **Acceptance gate**: external PostgreSQL drivers execute against the server
  with auth/TLS; resource limits enforced; metrics emitted.

---

## R8 — Studio

- **Objective**: the Studio becomes a pure engine client.
- **Dependencies**: R7 (server/embed contracts).
- **Implementation areas**: rewire connector to `qmind-embed` (in-process) or
  server protocol; delete PGlite path + dependency; keep UI shell/rendering;
  decide desktop packaging (Tauri or alternative) now that the engine/server
  is stable.
- **Tests**: GUI e2e across engine features; parity between GUI results and
  engine results.
- **Benchmark requirements**: none (correctness-focused); light latency sanity
  checks.
- **Acceptance gate**: zero SQL semantics in the client; PGlite absent; GUI
  shows engine data live.

---

## R9 — Production Hardening

- **Objective**: reliability, security, ops maturity.
- **Dependencies**: R6/R7.
- **Implementation areas**: fuzz/soak campaigns, corruption-injection tests,
  backup/restore story, observability polish, TLS/memory benchmarks,
  per-connection security hardening, worst-case recovery-time verification.
- **Tests**: extended fuzz + kill traffic + soak matrix; backup restore e2e.
- **Benchmark requirements**: rerun S3/S4 qualification; document deltas.
- **Acceptance gate**: defined SLO envelope (recovery time, latency p99
  bounds, corruption detection rate) demonstrated with real runs.

---

## R10 — GA Certification

- **Objective**: **General Availability Certification** — not simply
  "version 1.0".
- **Dependencies**: all prior stages.
- **Implementation areas**: release checklist: security audit, license
  compliance, docs/site, packaging (installers), support matrix, conformance
  sign-off for the full workload matrix, formal benchmark report.
- **Tests**: GA acceptance suite = full 10-workload × 4-scale matrix +
  recovery + conformance; third-party interop.
- **Benchmark requirements**: publish `benchmarks/results/` final report with
  methodology, environment, and reproducibility instructions.
- **Acceptance gate**: GA sign-off by the owners based on measured evidence,
  not aspiration; R10 declares production readiness.

---

## Cross-stage invariants

1. Durability/correctness gates precede perf gates at every R stage.
2. No stage ships fabricated benchmark numbers; results must be reproducible
   from `benchmarks/`.
3. Single database implementation: engine is the only SQL executor, for CLI,
   server, embed, and Studio alike.
4. The Studio and server never depend on engine internals, and the engine
   never depends on the Studio/Tauri/browser/PGlite.