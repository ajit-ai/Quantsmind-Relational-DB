R3 architecture
===============

The current architecture is intentionally **transitional**: the WAL remains the
recovery authority, and the page store is derived from it. R3 adds a persistent
storage manager and a batch/streaming execution path without rewriting the
wait-for-logged durability model from R2.

Component map
-------------

.. code-block:: text

                    SQL
                     │
                     ▼
                 Parser
                     │
                     ▼
              Query Execution
                     │
            ┌────────┴────────┐
            │                 │
         Streaming        Materialized
            │                 │
            ▼                 ▼
     StorageManager      Volcano executor
            │           (in-memory MVCC store)
            ▼
       FilePageStore
            │
            ▼
       Persistent pages

        WAL ─────────► Recovery / rebuild

Responsibilities
----------------

.. list-table:: R3 components and their roles
   :header-rows: 1

   * - Component
     - Responsibility
     - Owns
   * - ``Engine`` (qmind-sql)
     - SQL surface, WAL writer, execution selection
     - MVCC store, WAL, catalog, storage wiring
   * - Volcano executor
     - Row-at-a-time SELECTs over the MVCC store (joins, aggregates, GROUP BY)
     - Operators in ``executor.rs``
   * - ``StorageManager``
     - Persistent table coordinator
     - Buffer pool, table catalog, page/table id allocators
   * - ``BufferPool``
     - Page cache, CRC-validated loads, write-back eviction, flush
     - Resident frames
   * - ``TableStore`` / ``RowIter``
     - Row-chain append/scan helpers and the lazy row cursor
     - Per-operation metadata snapshot
   * - ``FilePageStore``
     - Durable page segments under ``tables/``
     - Segment files (256 × 8 KiB per segment)
   * - WAL
     - Durable record of every commit (logical records)
     - ``wal/wal.log``

The two execution paths
-----------------------

* **``execute``** — Volcano pipeline over the in-memory MVCC row store. It is
  the complete SQL subset, fully materializing.
* **``stream_query``** — batch pipeline over persistent storage, streaming
  bounded batches. It supports scan, WHERE, projection, LIMIT, an ORDER BY
  materialized fallback, aggregate functions with and without GROUP BY, and
  INNER equality JOIN (both inputs materialized from persistent storage).
  Result output is streamed in batches; inputs for aggregate/join/sort are
  materialized in RAM (spill deferred).

See :doc:`/concepts/execution-model` and
:doc:`/developer-guide/batch-execution`.

Why the page store is derived from the WAL
------------------------------------------

Opening a database replays the WAL (the authority), rebuilds the committed
state in memory, and then **re-creates** the ``tables/`` page store from that
state. Consequences:

* Correct: a page scan can never be missing a committed row.
* Simple: no separate durable-page reconciliation at open time.
* Transitional: the final architecture will have pages as an independently
  durable, checkpointed structure with the WAL only bridging the gap between
  checkpoints.

This trade-off is recorded in ADR-016 and described in detail in
:doc:`/architecture/wal-storage-ordering`.

Storage layer summary
---------------------

* 8 KiB pages with CRC-protected, versioned headers
  (:doc:`/developer-guide/page-format`).
* Append-only row chains per table, global 64-bit page ids.
* Buffer pool: clock-sweep, write-back; every load CRC-validated.
* FilePageStore: segmented files (256 pages per 2 MiB segment).
* WAL ordering: page data never becomes durable before the WAL record
  (ADR-015).

Deliberately not in R3
----------------------

* The full checkpointed storage lifecycle.
* Multi-writer concurrency in the storage/batch layers.
* External spilling for aggregation/sort (memory-bounded only to the input
  cardinality).
* A cost-based optimizer (R5 scope).
* Distributed execution, cloud/serverless infrastructure, authentication, or
  model layers beyond relational (all out of scope).