# QuantsMind Engine — Architecture & Design

> Status: **v0.1 Developer Preview (Experimental)** · 2026-09
> Decisions in §1.3 are locked. Milestone status: see [ROADMAP.md](./ROADMAP.md).
> Honest status language: features below are marked **Implemented** or **Planned**.

## 1. Vision

QuantsMind is an embeddable-first relational database engine written in Rust,
designed for hybrid transactional + analytical workloads (HTAP), with a
desktop GUI studio and Postgres-wire-compatible server mode.

**Design pillars**

1. **Kernel-first layering** — the core is a generic *KV + index + MVCC + WAL*
   storage kernel; Relational / Document / Key-Value are **model layers** on top.
   New data models are additive features, not rewrites.
2. **HTAP from day one in execution, staged in storage** — row store serves OLTP;
   a persistent columnar replica serves OLAP (M8) without blocking writes.
3. **Performance is a contract** — every milestone has numeric exit criteria,
   enforced by benchmarks. Measured targets are marked; unmeasured ones are
   explicitly labeled Planned (see §1.1).
4. **Correctness over speed of delivery** — MVCC and recovery are fuzzed and
   property-tested from M2 onward; silent corruption is the only unacceptable bug.

### 1.1 Performance targets (contract)

| Metric | Target | Status |
|---|---|---|
| Bulk insert | ≥ 1M rows/s | B+Tree insert measured at **3.56M elem/s** (release); WAL group commit implemented |
| Point SELECT (hot) | ≥ 500K qps | **Not measured** |
| Scan + filter (vectorized) | ≥ 50M rows/s | **Not measured** — executor is Volcano/batched, not SIMD-vectorized |
| TPC-H Q1/Q6 (SF 0.1) | ≤ 5× DuckDB | **Not measured** — no TPC-H runner yet |
| Recovery | zero committed-txn loss | Validated by WAL replay / restart round-trip tests; **Planned:** kill-9 chaos harness |

### 1.2 Non-goals (v1)

- Distributed / multi-node clustering (single-node first)
- Full SQL standard coverage (subset grows per milestone)
- Stored procedures / triggers (post-1.0 candidates)

### 1.3 Locked decisions

| ID | Decision |
|---|---|
| D-001 | **HTAP**: OLTP = row-oriented B+Tree store; OLAP = persistent columnar replica (M8). Hybrid TiDB/DuckDB-style. |
| D-002 | **Layered kernel**: engine = KV + index + MVCC + WAL kernel; Relational (v1), Document, Key-Value are model layers above it. |
| D-003 | **Storage format is a versioned contract from day 1**: magic bytes + format version in every file header; forward migration policy documented. |
| D-004 | **Postgres wire protocol compatibility** for server mode (ecosystem leverage: psql, DBeaver, drivers). |

## 2. System overview

```
┌───────────────────────────────────────────────────────────────┐
│ Clients                                                       │
│   Desktop Studio (Tauri 2 + React)   CLI shell   wire clients │
├───────────────────────────────────────────────────────────────┤
│ Server layer            [qmind-server]                        │
│   PG wire v3 listener (simple Query, trust auth) · threads    │
├───────────────────────────────────────────────────────────────┤
│ SQL layer               [qmind-sql]                           │
│   handwritten parser → SQL subset AST → Volcano executor      │
│   DDL/DML/SELECT · filter · project · joins · aggregates      │
│   OLTP row path + columnar OLAP read path (M9 integration)    │
├───────────────────────────────────────────────────────────────┤
│ Model layer
│   v1: relational catalog/DDL/DML (implemented)
│   later: document, key-value adapters over same kernel (planned)
├───────────────────────────────────────────────────────────────┤
│ KERNEL                  [qmind-kernel]   ← the defensible IP  │
│   Buffer pool · B+Tree · row pages · MVCC · WAL · recovery    │
│   Columnar segments · delta applier · columnar reader (M8)    │
│   Versioned on-disk format (D-003)                            │
└───────────────────────────────────────────────────────────────┘
```

## 3. Kernel design (all Implemented)

### 3.1 Pages & buffer pool (M1)
- Fixed page size **8 KiB** (`page::PAGE_SIZE`); typed header + payload, full-page
  CRC32 — corruption detected at read, never propagated. **Implemented.**
- Buffer pool: configurable frame count, **clock-sweep** eviction, write-back
  eviction, validate-on-load. File-backing via segment store. **Implemented.**

### 3.2 Row store & B+Tree (M1–M4)
- Heap-style row pages keyed by implicit row id; primary structure is the B+Tree
  acting as the row heap. **Implemented.**
- B+Tree: insert/get/get_all/range-scan, run-preserving splits, duplicate
  `(key,value)` ordering, differential test vs `BTreeMap`. **Implemented.**
- **Secondary indexes: Planned (not built).** No latch crabbing yet — SQL
  requests serialize on an engine mutex (single writer; see §5).

### 3.3 WAL & recovery (M1–M2)
- CRC-checksummed WAL frames, **group commit** (one syscall per group),
  torn-tail-safe replay, committed-prefix crash semantics. **Implemented.**
- Recovery: checkpoint + redo/undo replay from WAL; restart round-trip tests.
  **Implemented.** *Full `kill -9` chaos harness: Planned.*

### 3.4 Transactions / MVCC (M2)
- **Snapshot Isolation** with per-transaction read snapshots. **Implemented.**
- Write-write conflicts: **first-committer-wins** validation. **Implemented.**
- Lock table / row-version locks. **Implemented.**
- Read Committed, SSI, group-level transactions: **Planned.**

### 3.5 HTAP storage (M8–M9, D-001) — Implemented
- Persistent columnar segments: QMINDCOL format (magic + version + CRC),
  `Null | Int | Text` types, **Raw / Dict / RLE** encodings. **Implemented.**
- Delta buffer + schema-aware **DeltaApplier** with LSN markers: OLTP writes are
  captured to a delta buffer for asynchronous apply to columnar segments.
  **Implemented.**
- **ColumnarReader**: all-rows / filtered / projected scans. **Implemented.**
- SQL-engine integration (M9): `Engine::with_columnar()`, insert capture,
  flushes at a configurable row threshold, `select_from_columnar()` routing when
  columnar data exists. **Implemented** (feature-test covered in e2e).

## 4. SQL layer (Implemented; scope is a strict subset)

- **Parser: handwritten** tokenizer + recursive-descent parser (decision
  superseded the earlier sqlparser-rs plan). Covers: CREATE TABLE, INSERT
  (multi-row VALUES), SELECT with projection/filter/GROUP BY/LIMIT, INNER JOIN,
  SHOW TABLES. No column-lists in INSERT, no PRIMARY KEY modifier, no ORDER BY,
  no subqueries, no UPDATE/DELETE yet. **Implemented.**
- **Executor: Volcano-style** (pull-based) over materialized row batches;
  scan/filter/project/limit/join/aggregate operators. **Not** SIMD-vectorized —
  `BATCH_ROWS = 2048` is a batch constant, not a vectorized layout.
- **Planner: direct plan-lite** — no optimizer rules, no cost model. Joins are
  hash-style INNER JOIN; JOIN projections are plain columns only (no aggregates
  over joins, no JOIN+GROUP BY — documented limitation).
- Dialect aim: Postgres-compatible surface syntax (subset), per D-004.

## 5. Concurrency model (honest)

- SQL facade serializes on `Mutex<Engine>` / `Arc<Mutex<Engine>>` — safe single
  writer, no multi-writer concurrency. MVCC gives snapshot reads, but reader
  concurrency is not yet exploited at the SQL level.
- **Planned:** snapshot-aware lock-free reads; multi-writer groundwork.

## 6. Server & clients (Implement status)

- **Server (M5, partial)**: blocking threads per connection; PG wire v3 subset —
  startup, **trust auth only** (no SCRAM/TLS), simple Query protocol, values as
  TEXT (OID 25). **Implemented (dev-only).** Extended protocol, auth, TLS: Planned.
- **CLI (M5)**: minimal psql-like REPL over the wire (startup + query + row
  display). **Implemented.**
- **Desktop Studio (M6, shell)**: Tauri 2 app; `qmind-desktop` embeds the engine
  via `qmind-embed` (JSON API) and exposes `run_sql`. The rich React UI is the
  legacy browser prototype (PGlite-backed) and is **not yet rewired** to the Rust
  engine; a minimal eval shell (`QmindStudio`) currently drives `run_sql`.
  **Partially implemented.**

## 7. Testing strategy (Implemented today)

| Layer | Method | Status |
|---|---|---|
| Kernel units | unit tests + CRC/format roundtrips (61 tests) | Implemented |
| Recovery | restart round-trip / committed-state reconstruction tests | Implemented |
| SQL semantics | e2e tests: DDL/DML/filter/project/join/aggregate/columnar | Implemented |
| Parser robustness | handwritten fuzz harness (8K inputs) | Implemented |
| Soak | 10K-row lifecycle test | Implemented |
| Perf | criterion benches (`kernel_bench`, `sql_bench`) | Implemented (run manually; bench-compile gated in CI) |
| Concurrency | loom / crash-inject / sqllogictest / cargo-fuzz | **Planned** |

## 8. Repository layout

```
crates/
  qmind-kernel/   # storage kernel (page, buffer, btree, wal, mvcc, lock,
                  # recovery, fs_store, eviction, columnar, column_delta,
                  # column_reader) — zero runtime dependencies
  qmind-sql/      # handwritten parser → Volcano executor → engine
  qmind-server/   # PG wire server (trust auth, simple Query)
  qmind-cli/      # interactive shell
  qmind-embed/    # JSON embedding API for GUI/FFI
src-tauri/        # Tauri 2 desktop shell (qmind-desktop, outside workspace)
src/              # React + Tailwind frontend (legacy PGlite prototype +
                  # minimal QmindStudio Rust-shell)
docs/             # this architecture + roadmap
packaging/        # install/uninstall scripts (Windows/Linux/macOS/FreeBSD)
.github/workflows # CI gates (fmt, clippy -D warnings, test, bench compile)
```

## 9. Risk register

| Risk | Mitigation |
|---|---|
| MVCC/recovery subtle bugs | crash-injection harness (planned), fuzz + conservative invariants |
| Optimizer scope creep | plan-lite today; rule-based only after correctness is solid |
| Format churn | D-003 versioned headers implemented; migration tooling planned |
| Perf tuning death valley | numeric exit criteria per milestone; columnar/vectorized metics pending |
| Solo bandwidth | milestones ship independently usable artifacts; kernel is embeddable now |
| Maturity overclaim | this doc + ROADMAP use honest status language (Implemented / Planned) |