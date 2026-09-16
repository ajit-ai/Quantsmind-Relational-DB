R3 completion report
====================

Status: **R3 complete** (this is the official RST R3 completion report).

1. Executive summary
--------------------

R3 delivered the execution/storage milestones required by the R3 plan without
rewriting the R2 WAL recovery contract:

* a **reproducible benchmark harness** (``crates/qmind-sql/src/bin/bench_r3.rs``)
  with capture of commit / rustc / CPU / RAM into every report,
* **batch aggregation and GROUP BY** wired into the persistent-storage streaming
  path (``stream_query``) via ``BatchAggregate`` (R3-EXEC-1),
* **batch INNER equality JOIN** wired into ``stream_query`` via
  ``BatchHashJoin`` over persistent storage (R3-EXEC-2),
* an **ORDER BY / LIMIT memory review** confirming the streaming LIMIT is
  early-terminating and that ORDER BY, aggregate, and join inputs occupy
  documented, materialized memory footprints (R3-EXEC-3),
* **StorageManager / FilePageStore hardening** with regression tests for
  reconstructed page-store fidelity after real crashes, stale-page wiping on
  open, and empty-table streaming across reopen (R3-STORAGE),
* an updated Sphinx/RST documentation set (0 build warnings).

2. Starting point
------------------

The R3 phase started from the R2 state: 208/208 tests passing, an R2 WAL-formula
durability model, and a new-but-not-yet-fully-wired ``StorageManager`` /
``FilePageStore`` integration. ``stream_query`` existed and supported scan /
WHERE / projection / LIMIT / ORDER BY (materialized fallback) while rejecting
aggregates, GROUP BY, and JOIN with descriptive unsupported errors.

3. Implemented capabilities
---------------------------

Only functionality that actually exists is listed here.

Storage:

* ``StorageManager`` owns the buffer pool, table catalog, and the global
  page-id / table-id allocators (page ids start at 1; page 0 is reserved).
* Persistent tables live in an append-only per-table chain of 8 KiB
  CRC-protected pages under ``tables/`` (segmented ``FilePageStore``).
* WAL-first insert ordering is enforced: the WAL group is synced before any
  dirty page is appended; ``close()`` flushes pending WAL then dirty pages.
* ``open_db`` replays the WAL (the authority), rebuilds committed MVCC state,
  **wipes** the page-store directory, and rebuilds every table from committed
  rows in deterministic (sorted) name order. Uncommitted/phantom rows never
  reach the rebuilt page store (regression-tested).

Execution (``stream_query`` over persistent storage):

* full page-chain scans (lazy ``RowIter``),
* WHERE filtering (inline selection vector per batch),
* expression projection,
* LIMIT (early-terminating when there is no ORDER BY),
* ORDER BY through a materialized fallback,
* aggregate functions ``COUNT/SUM/AVG/MIN/MAX`` with and without GROUP BY
  (``BatchAggregate``; empty-input ungrouped aggregates emit one row — 0 for
  COUNT, NULL for value aggregates),
* INNER equality JOIN with a post-join WHERE and optional ORDER BY / LIMIT
  (``BatchHashJoin``; both sides materialized from persistent storage;
  self-joins and non-equality predicates are rejected with descriptive errors).

Quality:

* new streaming parity tests compare ``stream_query`` against the Volcano
  executor (deterministic for ORDER BY queries; unordered shapes compared as
  sorted multisets),
* new crash-recovery regressions drive post-crash reads through the **page
  store** (``stream_query``) and cross-check them against WAL-recovered MVCC,
* stale/corrupt page files left in ``tables/`` are wiped and rebuilt on open.

4. Architecture
---------------

The final R3 architecture is intentionally **transitional**:

* the **WAL** remains the recovery authority; page storage is **derived**,
* **Volcano** (``execute``) still runs row-at-a-time over the in-memory MVCC
  store and supports the full SQL subset,
* the **batch/streaming** path (``stream_query``) executes SELECTs over the
  persistent page store and emits results in bounded batches.

The diagram in :doc:`r3-architecture` documents this split; the decision
records ADR-014 through ADR-017 explain each choice.

5. Storage model
----------------

* Per-table row chains of 8 KiB pages; each page payload starts with a
  ``next_page_id`` (u64) and a ``row_count`` (u16), followed by
  length-prefixed rows (:doc:`/developer-guide/page-format`).
* Page ids are global across tables (allocator lives in ``StorageManager``),
  so page addresses never collide between tables or across reopen cycles.
* On close the WAL group is synced, then dirty pages are flushed.
* On open, reconstruction is deterministic: tables are rebuilt in sorted name
  order, rows in ascending row-id order, from the replayed committed state.
* The current page store is **not yet an independent checkpoint**: opening a
  database destroys and rebuilds ``tables/`` from the log. This is the
  documented transitional behaviour (ADR-015, ADR-016,
  :doc:`/architecture/wal-storage-ordering`).

6. Execution model
-------------------

Two paths share one parser and the same SQL semantics:

* **Volcano** (``execute``): pull-based, row-at-a-time operators over the
  MVCC row store; fully supports the SQL subset; materializes full results.
* **Batch/streaming** (``stream_query``): column-oriented ``Batch`` pipeline
  over persistent storage, streaming ``DEFAULT_BATCH_SIZE`` (2048) rows at a
  time; ``BatchAggregate``, ``BatchHashJoin``, and ``BatchSort`` materialize
  their inputs (spill-to-disk deferred). See
  :doc:`/developer-guide/batch-execution`.

7. SQL capabilities
-------------------

Honest capability matrix for the current repository state:

.. list-table::
   :header-rows: 1
   :widths: 35 40

   * - Capability
     - Status
   * - SELECT
     - Implemented
   * - WHERE
     - Implemented
   * - Projection (expressions)
     - Implemented
   * - LIMIT
     - Implemented (streams)
   * - ORDER BY
     - Implemented (materialized fallback)
   * - Aggregation (COUNT/SUM/AVG/MIN/MAX)
     - Implemented (execute + stream_query)
   * - GROUP BY
     - Implemented (execute + stream_query)
   * - INNER JOIN (equi, 2 tables)
     - Implemented (execute + stream_query)
   * - Insert / Create Table
     - Implemented
   * - CREATE/DROP INDEX
     - Implemented
   * - UPDATE
     - Deferred
   * - DELETE
     - Deferred
   * - JOIN + GROUP BY, aggs over joins
     - Deferred
   * - Subqueries
     - Deferred
   * - Streaming/spilled join, agg, sort
     - Deferred
   * - Cost-based optimizer
     - Deferred

8. Benchmark results
--------------------

Real release-mode measurements from ``benchmarks/results/r3/report-100K-r3.md``
(100 000 rows, 3 iterations, commit bc6f4326, rustc 1.98.0):

.. list-table::
   :header-rows: 1
   :widths: 25 12 25

   * - Workload
     - Rows
     - Result
   * - bulk_insert
     - 100000
     - 39.82K rows/s
   * - wf_full_scan
     - 100000
     - 561.80K rows/s
   * - wf_filtered_scan
     - 10000
     - 50.00K rows/s
   * - wf_projection
     - 100000
     - 421.94K rows/s
   * - wf_limit
     - 1000
     - 1.00M rows/s
   * - wf_order_small
     - 100
     - 286.53 rows/s
   * - wf_order_full
     - 100000
     - 163.93K rows/s
   * - wf_agg_count / agg_sum / agg_avg
     - 1
     - 4.03 / 5.29 / 7.25 rows/s
   * - wf_group_by
     - 4 groups
     - 20.62 rows/s
   * - wf_join
     - 100000
     - 116.96K rows/s
   * - wf_close / wf_open
     - —
     - 191 ms / 1363 ms

These numbers are environment-specific (a laptop host) and are **not**
qualification numbers. See :doc:`/benchmarks/r3` and the raw reports.

9. Test results
---------------

Final full workspace run (``cargo test --workspace --all-targets``):
**215 passed, 0 failed** (baseline was 208). Breakdown by target:

+-----------------------------+-------+
| Target                      | Count |
+=============================+=======+
| qmind-kernel lib            | 87    |
+-----------------------------+-------+
| kernel correctness          | 6     |
+-----------------------------+-------+
| kernel integration          | 3     |
+-----------------------------+-------+
| read_stress                 | 6     |
+-----------------------------+-------+
| qmind-sql lib               | 50    |
+-----------------------------+-------+
| sql concurrency             | 4     |
+-----------------------------+-------+
| sql crash_recovery          | 9     |
+-----------------------------+-------+
| sql parser_fuzz             | 4     |
+-----------------------------+-------+
| sql persistence             | 12    |
+-----------------------------+-------+
| sql soak                    | 1     |
+-----------------------------+-------+
| sql sql_e2e                 | 29    |
+-----------------------------+-------+
| embed API                   | 2     |
+-----------------------------+-------+
| server wire_e2e             | 2     |
+-----------------------------+-------+
| Total                       | 215   |
+-----------------------------+-------+

R3 test additions (over the 208 baseline):

* streaming aggregate parity + persistent reopen (sql_e2e),
* streaming GROUP BY parity + persistent reopen (sql_e2e),
* streaming JOIN parity + persistent reopen (sql_e2e),
* page-store-vs-WAL fidelity after real sub-process crashes, two tables,
  across multiple crash/restart cycles (crash_recovery),
* uncommitted rows never leak into rebuilt pages (crash_recovery),
* stale/corrupt page files are wiped and rebuilt on open (sql_e2e),
* empty table created then reopened streams zero rows (sql_e2e).

Gates that pass at the end of R3:

* ``cargo fmt --all -- --check`` — clean,
* ``cargo clippy --workspace --all-targets --all-features -- -D warnings`` — clean,
* ``cargo test --workspace --all-targets`` — 215/215,
* ``sphinx-build -W -b html docs docs/_build/html`` — 0 warnings.

10. Known limitations
---------------------

* Aggregate, GROUP BY, JOIN, and ORDER BY inputs are **materialized in RAM**;
  external spill-to-disk is deferred. A large join/aggregate can exceed
  available memory.
* The streaming JOIN materializes **both** inputs from persistent storage
  (no streaming probe); it is limited to a single INNER equi-join predicate
  spanning two tables. Self-joins and non-equality predicates are rejected.
* No JOIN + GROUP BY combination, no aggregates over joins.
* The page store is **derived** from the WAL and is rebuilt on every open;
  there is no checkpointed storage lifecycle yet.
* Insert throughput is fsync-per-statement; there is no cross-statement batching.
* Streaming results are SELECT-only today; DML flows through ``execute``.
* Benchmark numbers are from a single laptop host and are not representative
  of a reference/qualified platform.

11. Billion-row qualification status
------------------------------------

**R3 does not by itself certify billion-row capability.**

The current architecture is transitional: page storage is rebuilt from the WAL
on open, and aggregate / join / sort operators materialize their inputs in RAM.
Neither property is consistent with billion-row operation. What remains before
any R6 qualification claim:

* the final checkpointed storage lifecycle where durable pages are
  independently recoverable (R4 storage work per ADR-016),
* bounded-memory (external spilling) execution for aggregates, joins, and
  sorts,
* reference-hardware benchmark runs across the S1–S4 scales with a stable
  measurement protocol,
* defined quality gates (correctness, soak, crash/recovery) at scale.

12. Next phase
--------------

R4 is next **only after** R3 acceptance of this report and the R3 acceptance
checklist. R4 is not implemented in this task and must not be started until R3
is formally accepted.