# ADR-001 — Product Target

- **Status**: Accepted (R1)
- **Date**: R1 architecture gate

## Context

The repository is not an experimental database or desktop demo anymore. The
product target is a production-grade, general-purpose relational **HTAP**
database: billion-row single-machine operation with a future path toward
larger-scale/distributed deployment. Decisions and priorities flow from this.

## Decision

The product target is:

> Production-grade general-purpose relational HTAP database capable of reliably
> operating on billion-row datasets on a single machine, with a future path
> toward larger-scale/distributed deployment.

Priorities derived from this target:

1. Durability, recoverability, and correctness outrank benchmark numbers.
2. A single, coherent engine is the only database implementation.
3. The Studio, server, embed API, and CLI are clients/brokers of the engine.
4. The architecture is planned in stages R1–R10
   (`docs/roadmap/PRODUCTION_ROADMAP.md`); nothing beyond R1 is implemented now.

## Consequences

- Storage, WAL, recovery, and MVCC are the critical path (R2–R4).
- Perf claims are gated on measured benchmarks (`benchmarks/`), never invented.
- Feature additions must be justified against the target (e.g., no
  document/KV/graph/vector engines during R1; no premature distribution).
- Distribution is explicitly deferred but architecture-reserved.

## References

- `docs/architecture/TARGET_ARCHITECTURE.md`
- `docs/roadmap/PRODUCTION_ROADMAP.md`
- `crates/qmind-kernel`, `crates/qmind-sql` current state (assessment)