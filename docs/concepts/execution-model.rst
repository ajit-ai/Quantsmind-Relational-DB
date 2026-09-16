Execution model
===============

The engine has two execution paths. Both share the same parser, expression
evaluator, and SQL semantics, but they differ in where they read data and how
they produce results.

.. code-block:: text

                 SQL
                  ↓
              Parser
                  ↓
           Execution selection
              ↓        ↓
       Volcano executor   Batch / streaming path
          (MVCC rows)     (persistent page storage)

The Volcano executor
--------------------

``Engine::execute`` runs a pull-based, row-at-a-time operator pipeline over the
in-memory MVCC row store (``crates/qmind-sql/src/executor.rs``):

.. code-block:: text

   VecScan / Scan → Filter → (Sort) → Scan(project) → (Limit)

Additional operators:

* ``HashAggregate`` — GROUP BY with ``COUNT / SUM / AVG / MIN / MAX``; emits
  one row per group ordered by group key.
* ``HashJoin`` — INNER equality join; materializes the right (build) side,
  probes with the left side.

Every operator implements ``next() -> Option<Row>``. This path fully supports
the current SQL subset, including joins, aggregates, and ``GROUP BY``, but it
materializes results in the returned ``ExecResult``.

The batch / streaming path
--------------------------

``Engine::stream_query`` executes a SELECT against persistent storage using the
batch pipeline (``crates/qmind-sql/src/batch.rs``, ``batch_ops.rs``,
``result_stream.rs``). It streams results to a sink in bounded batches of
``DEFAULT_BATCH_SIZE`` (2048) rows instead of returning one materialized
result. See :doc:`/developer-guide/batch-execution` for the full model.

Current streaming capabilities

* full page-chain scan through ``StorageManager::scan_rows``,
* WHERE filtering (inline selection vector per batch),
* expression projection,
* LIMIT (early-terminating without ORDER BY),
* ORDER BY through a materialized fallback (collect → sort by key expression →
  emit in batches),
* aggregate functions (``COUNT/SUM/AVG/MIN/MAX``) with and without GROUP BY,
  executed with ``BatchAggregate`` over a materialized input,
* INNER equality JOIN executed with ``BatchHashJoin`` (both inputs
  materialized from persistent storage; streaming/spilled joins are deferred).

The batch operators serve both the Volcano-free streaming pipeline and the
aggregation/join paths; memory behaviour for each operator is documented in the
:doc:`/developer-guide/batch-execution` operators table.

Why both paths exist
--------------------

The Volcano executor is the original, complete SQL engine: simple, correct, and
well tested, but row-at-a-time and fully materializing. The batch/streaming
path was introduced (R3) so that queries over large persistent tables can run
with bounded memory: at most one batch of results is live at a time, and a scan
cursor holds only one page of rows plus one batch.

The two paths are intentionally kept separate during R3 instead of rewriting
the Volcano executor, because:

* the durable/streaming architecture is still transitional (page storage is
  derived from the WAL), and
* a full switch of the executor is a larger, riskier change than adding a
  parallel streaming path behind ``stream_query``.

This decision is recorded in ADR-017 (batch execution).

Result streaming
----------------

``QueryResult`` (``result_stream.rs``) wraps any :doc:`batch operator
</developer-guide/batch-execution>` and yields one ``Batch`` at a time:

.. code-block:: text

   Query → ResultStream → Batch → Batch → … → None

Consumers pull a batch, flush it downstream, then pull the next one, so
client-visible result memory stays bounded even for millions of rows.