=========================================
QuantsMind Relational Database Engine
=========================================

.. meta::
   :description: Embedded-first relational database engine in Rust — HTAP (row
      store for OLTP, persistent columnar replica for OLAP), Postgres-wire server,
      CLI, and desktop studio.

An :ref:`embeddable relational database engine <quickstart>` written in Rust,
designed for hybrid transactional + analytical workloads (HTAP). Ships as a
library, a Postgres-wire-compatible server, a CLI shell, and a native desktop
GUI studio.

.. note:: **Status: Developer Preview / Experimental**

   Feature-complete for its documented SQL subset; not yet production-hardened.
   Honest gaps are listed in :doc:`architecture` (§1.1, §4–§6) and in the
   :doc:`roadmap`.

**Version 0.1.0** · MIT License · :doc:`architecture` · :doc:`roadmap` ·
:doc:`quickstart`

.. toctree::
   :maxdepth: 2
   :caption: Documentation

   architecture
   roadmap
   quickstart
   concepts/index
   developer-guide/index
   architecture/index
   benchmarks/index

Design pillars
==============

1. **Kernel-first layering** — the core is a generic *KV + index + MVCC + WAL*
   storage kernel. Relational (v1), Document, and Key-Value are model layers on
   top. New data models are additive features, not rewrites.
2. **HTAP from day one** — a row store serves OLTP; a persistent columnar
   replica (M8) serves OLAP reads. The OLAP path is batched, not yet
   SIMD-vectorized.
3. **Performance is a contract** — every milestone has numeric exit criteria,
   enforced by benchmarks in CI. No aspirational numbers.
4. **Correctness over speed** — MVCC and recovery are fuzzed and property-tested.
   Silent corruption is the only unacceptable bug.

System layers
=============

.. code-block:: text

   ┌──────────────────────────────────────────────────────────┐
   │ Clients                                                  │
   │   Desktop Studio (Tauri 2 + React)   CLI   wire clients  │
   ├──────────────────────────────────────────────────────────┤
   │ Server layer          [qmind-server]                     │
   │   PG wire v3 listener (simple Query, trust auth)         │
   ├──────────────────────────────────────────────────────────┤
   │ SQL layer             [qmind-sql]                        │
   │   handwritten parser → SQL subset AST → Volcano executor │
   │   DDL/DML/SELECT · filter · project · expressions ·      │
   │   ORDER BY · joins · aggregates · secondary indexes      │
   │   OLTP row path + columnar OLAP read path (M9)           │
   ├──────────────────────────────────────────────────────────┤
   │ Embedding API          [qmind-embed]                     │
   │   JSON contract for GUI / host integration               │
   ├──────────────────────────────────────────────────────────┤
   │ KERNEL                [qmind-kernel]  ← the defensible IP│
   │   Buffer pool · B+Tree · row pages · MVCC · WAL ·        │
   │   recovery · columnar segments · delta applier           │
   │   Versioned on-disk format (D-003)                       │
   └──────────────────────────────────────────────────────────┘

Feature surface (as of 0.1.0)
=============================

* SQL: ``CREATE TABLE``, ``INSERT`` (multi-row ``VALUES``), ``SELECT`` with
  expressions, rich predicates (arithmetic, comparison, ``AND/OR/NOT``,
  ``LIKE``, ``IN``, ``BETWEEN``, scalar functions), ``ORDER BY``/``LIMIT``,
  ``GROUP BY`` + aggregates, hash INNER JOIN, ``CREATE/DROP INDEX``,
  ``SHOW TABLES``.
* Transactions: Snapshot Isolation, first-committer-wins, WAL group commit +
  ARIES-style recovery, crash-injection property harness.
* Storage: 8 KiB CRC32 pages, clock-sweep buffer pool, B+Tree, MVCC, versioned
  columnar replica (Raw/Dict/RLE), delta capture.
* Runtime: zero runtime dependencies in the kernel; embeddable library.
* Durability (R2): fsync-disciplined WAL with full-log replay, durable catalog,
  index rebuild on open, torn-tail truncation, format versioning, subprocess
  crash-recovery tests.
* Execution (R3): persistent storage manager over the page store, batch
  execution with streaming results, and a persistent-storage query path.
  :doc:`See the R3 documentation <architecture/index>`.

Test suite
==========

208 tests green across the workspace, with ``cargo fmt --check`` and
``cargo clippy -D warnings`` enforced on CI (Ubuntu + Windows). See
:ref:`testing` for the full breakdown.