# Target Architecture (R1.4)

> R1 deliverable — `docs/architecture/TARGET_ARCHITECTURE.md`
>
> Defines the intended architecture for **R2–R10**. R1 does not implement this;
> it fixes the architectural location and interfaces of each capability.

North star: a production-grade general-purpose relational **HTAP** database
operating on billion-row single-machine datasets, with a future path toward
distribution. Every capability below names its target R stage; see
`docs/roadmap/PRODUCTION_ROADMAP.md` for phase detail.

---

## 1. Layered model

```text
Clients
   │   Studio (replaceable) / drivers / psql / embed host
   ▼
Server            PostgreSQL wire protocol (v3 → extended), auth, TLS, sessions
   ▼
Embedded API       JSON/typed contract, FFI-safe, zero-network embed
   ▼
Database Engine
   ├── Query Engine ──────────► Parser → Binder → Semantic Analysis →
   │                             Logical Plan → Optimizer → Physical Plan →
   │                             Execution Engine (batch/vectorized/parallel)
   ├── Transaction Manager ───► MVCC, txn log, conflict detection, GC/watermark
   ├── Recovery ──────────────► WAL replay, checkpoint restore, catalog rebuild
   ├── WAL ───────────────────► fsync-disciplined group commit, segments
   ├── Buffer/Cache ──────────► page pool, eviction, write-behind
   └── Storage Manager ───────► Row Store | Column Store | Indexes
                                 │  (sharded/data-region aware for later R7)
                                 ▼
                             OS filesystem (single-node)
```

---

## 2. Target query path

```text
SQL
 ↓
Parser              — tokenizer + grammar → concrete AST      (exists, KEEP; grow in R5)
 ↓
Binder              — resolve names/types against catalog      (REPLACE — build in R5)
 ↓
Semantic Analysis   — type checking, constraint/validation     (REPLACE — build in R5)
 ↓
Logical Plan        — canonical relational algebra tree        (build in R5)
 ↓
Optimizer           — rule + cost-based, stats, reordering     (build in R5)
 ↓
Physical Plan       — operator choice incl. scans/joins/agg    (build in R5)
 ↓
Execution Engine    — batch/vectorized Volcano, parallel       (KEEP core; batch in R3)
 ↓
Storage             — row/column/index access via a capability-aware API
```

Interfaces to define with impl in R5:

- `Binder: Catalog -> (Query, BoundSchema)`
- `LogicalPlan = PlanNode` (Scan/Filter/Project/Join/Aggregate/Sort/Limit)
- `Optimizer: (LogicalPlan, Stats) -> LogicalPlan`
- `PhysicalPlan: LogicalPlan -> OperatorTree`
- `Operator: execute(Batch) -> Batch` (batch-oriented)

---

## 3. Target storage path

```text
OLTP  ──► Row Store               slotted pages, MVCC versions, heap
OLAP  ──► Column Store            compressed segments, zone maps, pushdown
HTAP  ──► Coordinated Row+Column  coherent ingestion → columnar refresh,
                                  single MVCC timeline, snapshot-consistent reads
```

Design rules:

1. **A single MVCC timeline** serves both stores; any committed row must be
   addressable from either representation at the same snapshot.
2. **Column store lag is explicit and monotonic**: delta buffers (existing
   `column_delta.rs`) flush deterministically and only advance read
   visibility behind commit — no torn analytical reads.
3. **Storage is the single source of truth**; neither Studio nor the server
   maintains a parallel database (see `ENGINE_BOUNDARIES.md`).

Concrete storage targets:

| Target | Location | R stage |
|---|---|---|
| Durable system catalog (versioned) | engine catalog tables | R2 |
| DDL journal in WAL | WAL record types | R2 |
| Runtime open/start recovery | recovery pipeline | R2 |
| On-disk slotted row store | Storage Manager / Row Store | R3 |
| On-disk B+Tree index/uniqueness | Storage Manager / Indexes | R3 |
| Full buffer management (pool, eviction, write-behind) | Buffer/Cache | R3 |
| Columnar hardening (RLE fix, zone maps, pushdown) | Storage Manager / Column Store | R4 |
| Free-space management | Storage Manager | R3 |
| Range/hash partitioning + pruning | Storage Manager | R5 |
| Spill-to-disk operators | Execution Engine | R3 |

---

## 4. Cross-cutting capabilities

These are architectural locations, not R1 implementations.

| Capability | Architectural location | R stage |
|---|---|---|
| Partitioning | Storage Manager (partition catalog + placement) + Planner (pruning) | R5 |
| Compression | Column Store (RLE→dictionary/bit-packing); Row Store (optional) | R4 |
| Metadata statistics | Catalog + Background stats collector (used by optimizer) | R5 |
| Predicate pushdown | Planner (physical) + Storage (zone maps / early-out) | R4/R5 |
| Projection pruning | Planner (physical) | R5 |
| Vectorized execution | Execution Engine (batch operators + simd exprs) | R3 |
| Parallel execution | Execution Engine (operator/task parallelism) | R3/R7 |
| Bounded memory | Execution Engine (memory budget per query) | R3 |
| External spill | Execution Engine (sort/agg spill files) | R3 |
| Large scans/aggregations | Column Store + batch exec | R3/R4 |
| Concurrent OLTP | Txn manager (multi-writer, conflicts), server sessions | R4 |

---

## 5. Transaction / durability architecture (target)

```text
Client txn ─► begin(txn_id) ─► write set (MVCC) ─► prepare → WAL fsync (group)
                                  │  ──────────────────────────► commit → update watermark
                                  ▼
                          checkpoint(LSN) → flush dirty pages → truncate WAL
```

- Durability levels configurable: `none` (dev), `group-commit fsync`,
  `fsync-every-txn`.
- Recovery order: open WAL segment list → replay committed txns → rebuild
  catalog → truncate at last checkpoint.
- MVCC GC: watermark = oldest active `read_ts`; versions below watermark
  reclaimed at checkpoints.

---

## 6. Server architecture (target)

- PGv3-extended protocol: extended query (`Parse/Bind/Execute`), prepared
  statements, typed parameter formats, OID result typing.
- Auth: SCRAM-SHA-256 + password file/PAM; TLS.
- Sessions: connection pools + resource limits (`max_connections`,
  per-query memory/CPU budgets, statement timeout, `pg_terminate_backend`
  equivalent).
- Observability: per-connection metrics, slow-query log, engine-level
  counters.
- Concurrency: read/write engine handles (RwLock futures), never the
  engine-internal locks leaking into network threads.

---

## 7. Studio architecture (target)

- Studio is a **pure client**. SQL semantics live in exactly one place: the
  engine.
- Connectivity: embedded API (in-process) or TCP server; PGlite removed.
- Desktop packaging (Tauri or alternative) is undecided until engine/server
  stabilize; the Studio must remain swappable independently.
See `STUDIO_ARCHITECTURE.md`.

---

## 8. What the architecture explicitly does NOT do in R1

- No distributed consensus, no multi-node sharding implementations, no
  replication protocol (reserved, `ADR-007`).
- No new document/KV/graph/vector engines.
- No GUI redesign, no Tauri migration.
- No full rewrites of storage, SQL, executor, or GUI (assessment says
  `KEEP`/`REWORK`, never wholesale `REPLACE` of working components).