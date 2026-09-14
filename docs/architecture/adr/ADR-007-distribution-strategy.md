# ADR-007 — Distribution Strategy

- **Status**: Accepted (R1) — **deferred execution**
- **Date**: R1 architecture gate

## Context

The product target is single-machine billion-row operation, with "future path
toward larger-scale/distributed deployment." R1 must not introduce distributed
architecture prematurely (engineering rule 8). Yet the architecture should not
block the eventual path by baking in single-node assumptions.

## Decision

- **Now**: single-node, single-process engine. No consensus, replication,
  sharding, or RPC implementation during R1–R6 (except read-only built-ins if
  explicitly scoped by later stages).
- **Reserved shape**: the row+column coordinators and the MVCC watermark model
  are already partitioned-friendly (version + LSN order). When distribution
  begins (post-R10 or a dedicated R11), the extension points are:
  - per-shard row/column stores with range/hash partitioning (R5 partitioning
    is the first seed),
  - replication log drawn from the existing WAL/checkpoint machinery,
  - timeline-synchronized MVCC across shards (single cluster-wide timestamp).
- Explicitly out of scope as product features until decided: document, KV,
  graph, vector engines.

## Consequences

- R1–R10 build a strong single-node engine with clean interfaces, avoiding
  the cost and risk of premature distribution.
- Key internal contracts (WAL record format, MVCC commit_ts, catalog) are kept
  format-forward so a later shard/log layer can adopt them without a storage
  rewrite.
- No fake distributed features are added to marketing or docs.

## References

- `docs/architecture/TARGET_ARCHITECTURE.md` (§8)
- `docs/roadmap/PRODUCTION_ROADMAP.md`
- R1 engineering rules 8–9