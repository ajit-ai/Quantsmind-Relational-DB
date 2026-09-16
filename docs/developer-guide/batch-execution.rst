Batch execution
===============

This page describes the column-oriented batch execution model
(``crates/qmind-sql/src/batch.rs``, ``batch_ops.rs``, ``result_stream.rs``) and
how it is used by the persistent-storage query path
(``Engine::stream_query``).

Why batch execution exists
--------------------------

A query over a large persistent table should not need to fit its entire result
in memory. The batch model processes data in bounded chunks and streams the
output, while also laying data out column-wise so an operator can touch one
column (one cache-friendly run) instead of one heap object per row.

Core types
----------

``Batch``
    ``N`` rows × ``M`` columns stored column-wise
    (``Vec<ColumnVector>`` where ``ColumnVector = Vec<SqlValue>``). Has
    ``row_count``, column accessors, ``push_row``, ``row(i)``
    (row reconstruction), ``project``, ``apply_selection``, and an
    ``is_full(batch_size)`` helper.

``SelectionVector``
    A dense list of row indices that survive a predicate. Filtering builds a
    selection vector and only materializes the surviving rows on demand via
    ``apply_selection`` — no copy of every row.

``NullBitmap``
    A dense null mask kept for the R3.12 contract; nulls are also represented
    inline as ``SqlValue::Null``.

``DEFAULT_BATCH_SIZE``
    The production batch width: ``2048`` rows (``BATCH_ROWS`` in
    ``qmind-sql/src/lib.rs``). Considered a provisional default until a
    benchmark-backed choice replaces it.

Page-to-batch flow
------------------

1. ``StorageManager::scan_rows`` yields a lazy ``RowIter`` over a table's page
   chain (at most one page of raw rows buffered at a time).
2. Each raw encoded row is decoded with ``decode_row`` against the table schema
   and pushed into a ``Batch`` pre-sized for `batch_size`.
3. The batch flows through operators and is emitted once it reaches
   ``DEFAULT_BATCH_SIZE`` rows (or when the scan ends).

Batch operators
---------------

Every operator implements ``next_batch(batch_size) -> Option<Batch>``
(``BatchOperator`` trait). Operators compose into a tree: a source feeds
filter/project/agg/join/sort, and the engine pulls the top operator until
exhausted.

.. list-table:: Implemented batch operators
   :header-rows: 1

   * - Operator
     - Purpose
     - Memory behaviour
   * - ``BatchScanRaw``
     - Decodes raw storage bytes straight into batches (persistent scan hot path)
     - Bounded (one batch)
   * - ``BatchRowScan``
     - Pull-based row-source adapter
     - Bounded (one batch)
   * - ``BatchVecScan``
     - Pre-materialized row scan (tests / small inputs)
     - Input-bound
   * - ``BatchFilter``
     - Predicate via inline selection vector, skips empty outputs
     - Bounded
   * - ``BatchProjectIdx``
     - Column-index projection (reorder / prune)
     - Bounded
   * - ``BatchEvalProject``
     - Row-level expression transform
     - Bounded
   * - ``BatchAggregate``
     - GROUP BY + ``COUNT/SUM/AVG/MIN/MAX``; groups in a ``BTreeMap`` (key order)
     - Materializes input on first pull (spill deferred)
   * - ``BatchSort``
     - Materializing sort (ASC/DESC, NULLs last)
     - Materializes input on first pull (external sort deferred)
   * - ``BatchHashJoin``
     - INNER equality join; build right side, probe with left
     - Build side in RAM (spill deferred)
   * - ``BatchLimit``
     - Passes through at most N rows in total
     - Bounded

Projection, LIMIT, ORDER BY
---------------------------

* **Projection** — in the streaming engine path, projection is expression-based
  and applied per row against the evaluated batch (column-index projection is
  available as ``BatchProjectIdx`` for pure pruning/reordering).
* **LIMIT** — truncates each batch to the remaining budget and stops the scan
  early; the scan may stop reading from storage once the limit is reached.
* **ORDER BY** — performs a **materialized fallback**: all surviving rows are
  collected, sorted (stable, NULLs-last, PostgreSQL default), then emitted in
  batches. Memory is bounded by the result cardinality, not by batch width;
  the sort itself is the current memory ceiling (external merge sort is
  deferred and documented as such in the code).

Materialization fallback
------------------------

Queries that require full ordering (ORDER BY) take the materialized path inside
``stream_query``: collect → filter → sort → truncate by LIMIT → project in
batches. Streaming (non-ORDER-BY) SELECTs never hold more than one batch of
decoded rows plus the current page.

Aggregates, GROUP BY, and JOIN are also executed over materialized inputs:

* **Aggregate / GROUP BY** — ``stream_query`` scans persistent storage into a
  ``Vec<Row>`` (WHERE applied inline), feeds ``BatchAggregate``, then applies
  reorder / ORDER BY / LIMIT before streaming the output in batches. The
  aggregate materializes its input on first pull; spill is deferred.
* **INNER JOIN** — ``stream_query`` materializes **both** inputs from persistent
  storage (no streaming join), builds the right side in ``BatchHashJoin`` and
  probes with the left, then applies the post-join WHERE / ORDER BY / LIMIT and
  streams the output. Memory is O(left + right); spill is deferred.

The LIMIT bound is applied defensively at the sink: an ordered or grouped or
joined result is never larger than what the earlier operators produced, and a
plain streaming LIMIT stops reading storage as soon as the budget is reached.

Supported query shapes (R3-EXEC)
---------------------------------

``stream_query`` now executes the following SELECT shapes over persistent
storage (previously returned descriptive unsupported errors):

* full page-chain scans through ``StorageManager::scan_rows``,
* WHERE filtering (inline selection vector per batch),
* expression projection,
* LIMIT (early-terminating when there is no ORDER BY),
* ORDER BY through a materialized fallback,
* aggregate functions (``COUNT/SUM/AVG/MIN/MAX``) with and without GROUP BY,
* INNER equality JOIN (``… INNER JOIN … ON <left.col> = <right.col>``, one
  join pair spanning the two tables; self-joins and non-equality predicates
  are rejected with descriptive errors).

These are parity-checked against the Volcano executor in
``crates/qmind-sql/tests/sql_e2e.rs`` and are memory-materialized as described
above; distributed joins and cost-based optimization remain out of scope.

Memory considerations
---------------------

* Streams: at most ``DEFAULT_BATCH_SIZE`` rows per in-flight batch plus one
  page of raw rows.
* Aggregation and sort materialize their input; spill-to-disk is **deferred**.
* ``Batch.clear()`` reuses column buffers across pulls to avoid allocation
  churn.

Streaming result behavior
-------------------------

``QueryResult`` (``result_stream.rs``) wraps a batch operator and yields one
batch per ``next_batch()`` call until exhausted, or converts to a synchronous
``ExecResult`` via ``collect_all``. `stream_query` itself pushes batches into a
caller-supplied sink as they are produced.

Difference from the Volcano executor
------------------------------------

The existing Volcano executor (``execute()``) is row-at-a-time over the
in-memory MVCC row store and returns fully materialized results; the batch path
is column-oriented, reads persistent storage, and streams bounded batches.
Both share the parser and SQL semantics. The two are kept separate during R3
(see ADR-017).