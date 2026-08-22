# QuantsMind Engine — Architecture & Design

> Status: **v0.1 draft (M0)** · Decisions in §1.3 are locked.
> Companion doc: [ROADMAP.md](./ROADMAP.md)

## 1. Vision

QuantsMind is a production-grade, embeddable-first relational database engine written
in Rust, designed for hybrid transactional + analytical workloads (HTAP), with a
desktop GUI studio and Postgres-wire-compatible server mode.

**Design pillars**

1. **Kernel-first layering** — the core is a generic *KV + index + MVCC + WAL*
   storage kernel; Relational / Document / Key-Value are **model layers** on top.
   New data models are additive features, not rewrites.
2. **HTAP from day one in execution, staged in storage** — row store serves OLTP;
   a vectorized columnar-batch executor serves OLAP over the same data without a
   second copy initially. A persistent columnar replica arrives later (M8).
3. **Performance is a contract** — every milestone has numeric exit criteria,
   enforced by benchmarks in CI.
4. **Correctness over speed of delivery** — MVCC and recovery are fuzzed and
   property-tested from M2 onward; silent corruption is the only unacceptable bug.

### 1.1 Performance targets (contract, not aspiration)

| Metric | Target | Measured how |
|---|---|---|
| Bulk insert | ≥ 1M rows/s | batched 10K-row txns, WAL group-commit on, NVMe/SSD |
| Point SELECT (hot) | ≥ 500K qps | 16 threads, PK lookups, buffer pool warm |
| Scan + filter (vectorized) | ≥ 50M rows/s | single thread, hot cache, SIMD-friendly predicate |
| TPC-H Q1/Q6 (SF 0.1) | ≤ 5× DuckDB | criterion harness, same hardware |
| Recovery | zero committed-txn loss | kill -9 chaos tests at every commit boundary |

Targets are re-validated each milestone release; regressions fail CI.

### 1.2 Non-goals (v1)

- Distributed / multi-node clustering (single-node first)
- Full SQL standard coverage (subset grows per milestone)
- Stored procedures / triggers (post-1.0 candidates)

### 1.3 Locked decisions (2026-08)

| ID | Decision |
|---|---|
| D-001 | **HTAP**: OLTP = row-oriented B+Tree store; OLAP = vectorized columnar-batch execution over row data now, persistent columnar replica later (M8). Hybrid TiDB/DuckDB-style. |
| D-002 | **Layered kernel**: engine = KV + index + MVCC + WAL kernel; Relational (v1), Document, Key-Value are model layers above it. |
| D-003 | **Storage format is a versioned contract from day 1**: magic bytes + format version in every file header; forward migration policy documented before any on-disk layout ships. |
| D-004 | **Postgres wire protocol compatibility** for server mode (ecosystem leverage: psql, DBeaver, drivers). |

## 2. System overview

```
┌───────────────────────────────────────────────────────────────┐
│ Clients                                                       │
│   Desktop Studio (Tauri 2 + React)   CLI shell   psql/drivers │
├───────────────────────────────────────────────────────────────┤
│ Server layer            [qmind-server]                        │
│   PG wire protocol listener · auth · session pool              │
├───────────────────────────────────────────────────────────────┤
│ SQL layer               [qmind-sql]                           │
│   parser (sqlparser-rs) → logical plan → optimizer rules      │
│   → vectorized push-based executor (columnar batches)         │
│   OLTP path: point plans    OLAP path: full vectorization     │
├───────────────────────────────────────────────────────────────┤
│ Model layer             [qmind-models]                        │
│   v1: relational catalog/DDL/DML                              │
│   later: document, key-value adapters over same kernel        │
├───────────────────────────────────────────────────────────────┤
│ KERNEL                  [qmind-kernel]   ← the defensible IP  │
│   Buffer pool · B+Tree index · row heap pages                 │
│   MVCC snapshots · WAL (group commit) · recovery              │
│   Versioned on-disk format (D-003)                            │
└───────────────────────────────────────────────────────────────┘
```

## 3. Kernel design

### 3.1 Pages & buffer pool (M1)
- Fixed page size **8 KiB** (`page::PAGE_SIZE`); page = 18-byte typed header + payload,
  full-page CRC32 (header + payload, checksum slot excluded).
- Checksummed headers (CRC32) — corruption detected at read, never propagated.
- Buffer pool: configurable frame count, clock-sweep eviction, pin/unpin API,
  dirty-page tracking with checkpoint integration.

### 3.2 Row store & B+Tree (M1–M4)
- Heap-style row pages keyed by implicit row id; secondary indexes are B+Trees
  mapping key → row id list.
- Variable-length encoding; NULL bitmap per page.
- Leaf split/merge with parent handoff; latch crabbing for concurrent descent.

### 3.3 WAL & recovery (M1–M2)
- Physiological logging; LSN-monotonic segments; **group commit** window
  (default 1 ms) to amortize fsync cost — this is what makes 1M inserts/s honest.
- Recovery = redo from last checkpoint LSN + undo via MVCC abort marks.
- `kill -9` at any instruction boundary must recover to a consistent snapshot.

### 3.4 Transactions / MVCC (M2)
- Snapshot Isolation first; Read Committed via per-statement snapshots.
- Txn timestamps are u64 counters; visibility check is branch-free where hot.
- Write-write conflicts: first-committer-wins validation.
- Serializable (SSI) is a post-M4 stretch goal.

### 3.5 HTAP execution (M3–M4, D-001)
- Executor is **push-based, vectorized**: operators consume/produce batches of
  ~2048 rows in columnar memory layout, converted on-the-fly from row pages.
- Small point queries take an optimized OLTP path (index probe → row decode).
- Large scans/aggregates take the vectorized path; planner routes by estimated
  row count (rule-based thresholds until cost model exists in M7+).
- M8 adds a persistent columnar replica (asynchronous delta apply, TiFlash-style).

## 4. SQL layer

- Parser: `sqlparser-rs` (AST) — do not hand-roll.
- Planner: logical plan IR → rule-based rewrite (predicate pushdown, projection
  pruning, constant folding) → physical vectorized plan.
- Dialect: Postgres-compatible surface syntax from day one.

## 5. Server & clients

- **Server (M5)**: async runtime (tokio), PG wire protocol (v3) subset:
  startup/auth (scram-sha-256), simple + extended query protocol.
- **CLI (M5)**: REPL speaking the wire protocol against localhost or remote.
- **Desktop Studio (M6)**: Tauri 2 app embedding the engine natively (no socket);
  reuses React UI patterns already present in this repo's web prototype.

## 6. Testing strategy

| Layer | Method | From |
|---|---|---|
| Kernel units | unit tests + CRC/format roundtrips | M1 |
| Concurrency | loom (eventual), stress suites | M2 |
| MVCC/recovery correctness | proptest + custom crash-injector | M2 |
| SQL semantics | sqllogictest-rs files grow per milestone | M3 |
| Robustness | cargo-fuzz targets on parser + WAL replay | M3+ |
| Perf | criterion benches gated in CI (compile always, run nightly) | M1 |

## 7. Repository layout

```
crates/
  qmind-kernel/   # storage kernel (pages, btree, wal, mvcc)  ← M1+
  qmind-sql/      # parser→planner→executor                    ← M3+
  qmind-server/   # PG wire server                             ← M5+
  qmind-cli/      # interactive shell                          ← M5+
docs/             # this architecture + roadmap
.github/workflows # CI gates
src/ package.json # existing React web prototype → becomes Tauri UI (M6)
```

## 8. Risk register

| Risk | Mitigation |
|---|---|
| MVCC/recovery subtle bugs | crash-injection harness from M2, fuzzing, conservative invariants |
| Optimizer scope creep | rule-based first; cost model only in M7+ |
| Format churn before M1 freeze | no durable format ships until versioning policy (D-003) is implemented |
| Perf tuning death valley | numeric exit criteria per milestone; tune continuously, not at the end |
| Solo bandwidth | milestones ship independently usable artifacts; kernel is embeddable by M2 |
