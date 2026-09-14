# ADR-006 — Execution Model

- **Status**: Accepted (R1)
- **Date**: R1 architecture gate

## Context

The executor is Volcano row-at-a-time with operator building blocks
`VecScan`, `Filter`, `Project`, `Limit`, `Sort`, `HashJoin`, `HashAggregate`
(`qmind-sql/src/executor.rs`). The product needs billion-row analytical
throughput and bounded memory; row-at-a-time per-row expression evaluation
cannot reach that alone.

## Decision

Keep the Volcano **operator framework** (structure stays: operators compose
into trees) and evolve the **data granularity** to batches/columnar vectors:

- R3: batch-oriented operator interface (operator consumes/produces vector
  batches instead of single rows), vectorized expression evaluation, spill
  operators, per-query memory budgets.
- Later: parallel operator execution under the guidance of a physical plan
  (R5 planner).
- The M9 columnar reader already returns batches
  (`engine.rs:396-398`, `lib.rs:5`); batch operators unify OLTP and OLAP
  execution shapes.

Concrete contractual change (R3): operators implement
`fn execute(&mut self) -> Chunk` where `Chunk` is a columnar batch, replacing
the row iterator; the existing per-operator tests are then migrated to
batch-valued tests.

## Consequences

- Existing operator logic is retained and re-expressed over batches; no
  behavior change at the SQL semantics level (same input → same results).
- Row-at-a-time expression code is replaced by vectorized kernels in the same
  `executor` module lineage.
- Memory-boundedness requirements (`BILLION_ROW_REQUIREMENTS.md` §6) become
  testable at the operator level.

## References

- `docs/architecture/TARGET_ARCHITECTURE.md` (§2 target query path)
- `crates/qmind-sql/src/executor.rs`
- `docs/roadmap/PRODUCTION_ROADMAP.md` (R3 execution)