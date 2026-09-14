================
Architecture
================

.. meta::
   :description: Design and architecture of the QuantsMind engine — kernel-first
      layering, HTAP storage, SQL layer, concurrency model, testing strategy.

Status: **v0.1 Developer Preview (Experimental)** · 2026-09.
Decisions in §1.3 are locked. Milestone status: see :doc:`roadmap`.
Features below are marked **Implemented** or **Planned**.

Vision
======

QuantsMind is an embeddable-first relational database engine written in Rust,
designed for hybrid transactional + analytical workloads (HTAP), with a desktop
GUI studio and Postgres-wire-compatible server mode.

Design pillars

1. **Kernel-first layering** — the core is a generic *KV + index + MVCC + WAL*
   storage kernel; Relational / Document / Key-Value are **model layers** on top.
   New data models are additive features, not rewrites.
2. **HTAP from day one in execution, staged in storage** — row store serves
   OLTP; a persistent columnar replica serves OLAP (M8) without blocking writes.
3. **Performance is a contract** — every milestone has numeric exit criteria,
   enforced by benchmarks. Measured targets are marked; unmeasured ones are
   explicitly labeled Planned (see §1.1).
4. **Correctness over speed of delivery** — MVCC and recovery are fuzzed and
   property-tested from M2 onward; silent corruption is the only unacceptable bug.

Performance targets (contract)
------------------------------

.. list-table::
   :header-rows: 1

   * - Metric
     - Target
     - Status
   * - Bulk insert
     - ≥ 1M rows/s
     - B+Tree insert measured at **3.56M elem/s** (release); WAL group commit implemented
   * - Point SELECT (hot)
     - ≥ 500K qps
     - **Not measured**
   * - Scan + filter (vectorized)
     - ≥ 50M rows/s
     - **Not measured** — executor is Volcano/batched, not SIMD-vectorized
   * - TPC-H Q1/Q6 (SF 0.1)
     - ≤ 5× DuckDB
     - **Not measured** — no TPC-H runner yet
   * - Recovery
     - zero committed-txn loss
     - WAL replay / restart round-trip tests + P3 crash-injection harness (exhaustive byte-truncation, byte-flip corruption, 300-txn MVCC→WAL→recovery property test) + R2 subprocess crash-recovery harness (`std::process::exit` kills, uncommitted rollback, index rebuild)

Non-goals (v1)
--------------

- Distributed / multi-node clustering (single-node first)
- Full SQL standard coverage (subset grows per milestone)
- Stored procedures / triggers (post-1.0 candidates)

Locked decisions
----------------

.. list-table::
   :header-rows: 1

   * - ID
     - Decision
   * - D-001
     - **HTAP**: OLTP = row-oriented B+Tree store; OLAP = persistent columnar replica (M8). Hybrid TiDB/DuckDB-style.
   * - D-002
     - **Layered kernel**: engine = KV + index + MVCC + WAL kernel; Relational (v1), Document, Key-Value are model layers above it.
   * - D-003
     - **Storage format is a versioned contract from day 1**: magic bytes + format version in every file header; forward migration policy documented.
   * - D-004
     - **Postgres wire protocol compatibility** for server mode (ecosystem leverage: psql, DBeaver, drivers).

System overview
===============

.. code-block:: text

   ┌───────────────────────────────────────────────────────────────┐
   │ Clients                                                       │
   │   Desktop Studio (Tauri 2 + React)   CLI shell   wire clients │
   ├───────────────────────────────────────────────────────────────┤
   │ Server layer            [qmind-server]                        │
   │   PG wire v3 listener (simple Query, trust auth) · threads    │
   ├───────────────────────────────────────────────────────────────┤
   │ SQL layer               [qmind-sql]                           │
   │   handwritten parser → SQL subset AST → Volcano executor      │
   │   DDL/DML/SELECT · filter · project · expressions · ORDER BY │
   │   joins · aggregates · secondary indexes                      │
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

Kernel design (all Implemented)
===============================

Pages & buffer pool (M1)
------------------------

- Fixed page size **8 KiB** (`page::PAGE_SIZE`); typed header + payload,
  full-page CRC32 — corruption detected at read, never propagated.
  **Implemented.**
- Buffer pool: configurable frame count, **clock-sweep** eviction, write-back
  eviction, validate-on-load. File-backing via segment store. **Implemented.**

Row store & B+Tree (M1–M4)
--------------------------

- Heap-style row pages keyed by implicit row id; primary structure is the
  B+Tree acting as the row heap. **Implemented.**
- B+Tree: insert/get/get_all/range-scan, run-preserving splits, duplicate
  ``(key,value)`` ordering, differential test vs ``BTreeMap``. **Implemented.**
- **Secondary indexes: Implemented (P4c).** Per-table in-memory index trees over
  the same kernel B+Tree; single-column, NULL-excluded. ``CREATE INDEX`` /
  ``DROP INDEX`` DDL, backfill at create, maintenance on INSERT, and a planner
  serving ``col = literal`` equality point-lookups (residual WHERE remains a
  filter). Range lookups and NULL entries are not indexed. Writes serialize on
  a single engine writer; reads run concurrently over snapshots (§5).

WAL & recovery (M1–M2, R2)
--------------------------

- CRC-checksummed WAL frames, **group commit** (one syscall per group),
  torn-tail-safe replay, committed-prefix crash semantics. **Implemented.**
- Discovery-R2 (R2): the WAL is now fsync-disciplined -- every committed
  autocommit performs write -> flush -> ``f.sync_data()`` before returning.
- Recovery (R2): startup ``open_db`` replays the full WAL, rebuilds the
  catalog from DDL records, redos committed transactions, recomputes
  ``next_row_id``, and rebuilds indexes from committed rows. Torn tails are
  truncated; interior corruption fails the open loudly (``WalCorrupt``).
- **Implemented.** Real subprocess ``kill``-style crash harness
  (``crash_recovery.rs``: ``std::process::exit`` kills that bypass
  destructors) proves committed data survives and uncommitted data rolls
  back. Physical checkpoints and WAL rotation remain **Planned** (R3).

Transactions / MVCC (M2)
------------------------

- **Snapshot Isolation** with per-transaction read snapshots. **Implemented.**
- Write-write conflicts: **first-committer-wins** validation. **Implemented.**
- Lock table / row-version locks. **Implemented.**
- Read Committed, SSI, group-level transactions: **Planned.**

HTAP storage (M8–M9, D-001) — Implemented
-----------------------------------------

- Persistent columnar segments: QMINDCOL format (magic + version + CRC),
  ``Null | Int | Text`` types, **Raw / Dict / RLE** encodings. **Implemented.**
- Delta buffer + schema-aware **DeltaApplier** with LSN markers: OLTP writes are
  captured to a delta buffer for asynchronous apply to columnar segments.
  **Implemented.**
- **ColumnarReader**: all-rows / filtered / projected scans. **Implemented.**
- SQL-engine integration (M9): ``Engine::with_columnar()``, insert capture,
  flushes at a configurable row threshold, ``select_from_columnar()`` routing
  when columnar data exists. **Implemented** (feature-test covered in e2e).

SQL layer (Implemented; scope is a strict subset)
==================================================

- **Parser: handwritten** tokenizer + recursive-descent parser. Covers: CREATE
  TABLE, CREATE/DROP INDEX, INSERT (multi-row VALUES), SELECT with
  projection/expression/filter/GROUP BY/LIMIT/ORDER BY, INNER JOIN, SHOW
  TABLES. Full expression grammar (arithmetic, comparisons, AND/OR/NOT,
  LIKE/IN/BETWEEN, scalar functions), case-insensitive keywords. No
  column-lists in INSERT, no PRIMARY KEY modifier, no subqueries, no
  UPDATE/DELETE yet. **Implemented.**
- **Executor: Volcano-style** (pull-based) over materialized row batches;
  scan/filter/project/sort/limit/join/aggregate operators. **Not**
  SIMD-vectorized — ``BATCH_ROWS = 2048`` is a batch constant, not a vectorized
  layout.
- **Expressions**: ``eval_expr`` evaluates a full expression tree against a row
  with three-valued logic (SQL NULL); LIKE (``%``/``_``), IN-lists, BETWEEN,
  ``UPPER``/``LOWER``/``LENGTH``. Arity-safe: type/division-by-zero errors
  bubble up.
- **Sort (P4b)**: stable materializing ``Sort`` with PostgreSQL null semantics
  (NULL largest → ASC nulls-last, DESC nulls-first); multi-key; applied after
  GROUP BY against the post-aggregation output.
- **Planner: direct plan-lite** — no optimizer rules, no cost model. One
  exception (P4c): a secondary index is chosen for top-level ``col = literal``
  equality conjuncts on the row path. Joins are hash-style INNER JOIN; JOIN
  projections are plain columns only (no aggregates over joins, no
  JOIN+GROUP BY — documented limitation).
- Dialect aim: Postgres-compatible surface syntax (subset), per D-004.

Concurrency model (honest)
==========================

- **Implemented (P5).** The SQL facade lives behind ``RwLock<Engine>``.
  ``Engine::execute_read(&self)`` (SELECT / SHOW only) captures one MVCC
  snapshot under a brief shared read guard, then scans lock-free — readers
  never block each other and never wait for the writer's commit. Statements
  observe a single point-in-time even across multi-table JOINs because one
  snapshot is threaded through the whole read pipeline.
- Writes remain single-writer: only DML/DDL take the exclusive guard and the
  engine commits transactions serially (first-committer-wins validation still
  governs stale writers at the kernel level).
- **In-memory** snapshot capture is cheap (one watermark copy); persistence of
  snapshots across the WAL is the Multi-writer/durable-snapshot work that
  remains **Planned** alongside Read Committed / SSI.

Server & clients (Implement status)
===================================

- **Server (M5, partial)**: blocking threads per connection; PG wire v3 subset —
  startup, **trust auth only** (no SCRAM/TLS), simple Query protocol, values as
  TEXT (OID 25). **Implemented (dev-only).** Extended protocol, auth, TLS:
  Planned.
- **CLI (M5)**: minimal psql-like REPL over the wire (startup + query + row
  display). **Implemented.**
- **Desktop Studio (M6, shell)**: Tauri 2 app; ``qmind-desktop`` embeds the
  engine via ``qmind-embed`` (JSON API) and exposes ``run_sql``. The rich React
  UI is the legacy browser prototype (PGlite-backed) and is **not yet rewired**
  to the Rust engine; a minimal eval shell (``QmindStudio``) currently drives
  ``run_sql``. **Partially implemented.**

Testing strategy (Implemented today)
====================================

.. list-table::
   :header-rows: 1

   * - Layer
     - Method
     - Status
   * - Kernel units
     - unit tests + CRC/format roundtrips (70 tests)
     - Implemented
   * - Property/fuzz
     - deterministic differential harness: B+Tree vs oracle (4K ops), WAL every-byte truncation + 400 byte-flip corruptions, MVCC serial-history (1200 steps), 300-txn crash→recovery zero-loss (6 tests)
     - Implemented
   * - Recovery
     - restart round-trip / committed-state reconstruction tests
     - Implemented
   * - SQL semantics
     - 23 e2e tests: DDL/DML/filter/project/expression/order-by/join/aggregate/columnar/secondary-index
     - Implemented
   * - Parser robustness
     - handwritten fuzz harness (8K inputs)
     - Implemented
   * - Soak
     - 10K-row lifecycle test
     - Implemented
   * - Perf
     - criterion benches (``kernel_bench``, ``sql_bench``)
     - Implemented (run manually; bench-compile gated in CI)
   * - Concurrency
     - loom / crash-inject / sqllogictest / cargo-fuzz
     - crash-injection done (in-process torn-tail + corruption harnesses, P3); loom macro-fuzz parity still **Planned**

Repository layout
=================

.. code-block:: text

   crates/
     qmind-kernel/   # storage kernel (page, buffer, btree, wal, mvcc, lock,
                     # recovery, fs_store, eviction, columnar, column_delta,
                     # column_reader) — zero runtime dependencies
     qmind-sql/      # handwritten parser → Volcano executor → engine
     qmind-server/   # PG wire server (trust auth, simple Query)
     qmind-cli/      # interactive shell
     qmind-embed/    # JSON embedding API for GUI/FFI
   docs/             # this documentation site (reStructuredText, Sphinx)
   src-tauri/        # Tauri 2 desktop shell (qmind-desktop, outside workspace)
   src/              # React + Tailwind frontend (legacy PGlite prototype +
                     # minimal QmindStudio Rust-shell)
   packaging/        # install/uninstall scripts (Windows/Linux/macOS/FreeBSD)
   .github/workflows # CI gates (fmt, clippy -D warnings, test, bench compile)
                     # + docs build/deploy (Sphinx → GitHub Pages)

Risk register
=============

.. list-table::
   :header-rows: 1

   * - Risk
     - Mitigation
   * - MVCC/recovery subtle bugs
     - P3 crash-injection harness (torn-tail/truncation + byte-flip corruption + 300-txn recovery property test), fuzz + conservative invariants
   * - Optimizer scope creep
     - plan-lite today; rule-based only after correctness is solid
   * - Format churn
     - D-003 versioned headers implemented; migration tooling planned
   * - Perf tuning death valley
     - numeric exit criteria per milestone; columnar/vectorized metrics pending
   * - Solo bandwidth
     - milestones ship independently usable artifacts; kernel is embeddable now
   * - Maturity overclaim
     - this doc + roadmap use honest status language (Implemented / Planned)