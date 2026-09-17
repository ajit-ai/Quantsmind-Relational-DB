# ADR-017 -- Batch Execution (Alongside the Volcano Executor)

- **Status**: Accepted (R3)
- **Date**: R3 architecture gate

## Context

The SQL engine's only execution path is a Volcano-style, row-at-a-time
operator pipeline over the in-memory MVCC row store (`executor.rs`), which
fully materializes results in an `ExecResult`. R3 introduces a persistent page
store; queries over large persistent tables need bounded-memory execution and
a streaming output shape. Rewriting the Volcano executor entirely is a large,
higher-risk change and is deferred.

## Decision

Introduce a parallel **batch/streaming** execution path instead of replacing
the Volcano executor:

- A column-oriented `Batch` (`N` rows × `M` columns) with a
  `SelectionVector`, `NullBitmap`, and row adapters
  (`crates/qmind-sql/src/batch.rs`).
- A set of batch-to-batch operators implementing
  `next_batch(batch_size) -> Option<Batch>` (`batch_ops.rs`): raw/row/vec
  scans, filter, index and expression projection, aggregate, sort, hash join,
  and limit. Large-scale spilling for aggregation/sort is deferred.
- A streaming result wrapper (`result_stream.rs`, `QueryResult`) that yields
  one batch at a time.
- An engine entry point `stream_query` that runs SELECTs against persistent
  storage, streaming bounded batches to a caller-supplied sink, with an
  ORDER BY materialized fallback.

The two paths coexist: `execute()` keeps the complete SQL surface (including
joins, aggregates, GROUP BY) over the MVCC store; `stream_query` incrementally
gains the same capabilities (aggregation and join are wired in later R3-EXEC
phases). Shared code (parser, expression evaluation, codec) remains unified.

## Consequences

- Large persistent scans stream with bounded memory.
- The Volcano executor is retained and unaffected, reducing risk.
- Two execution paths must be kept semantically consistent; the R3 completion
  report tracks their SQL capability differences explicitly.
- Spill-to-disk, vectorized kernels, and a single execution engine are future
  work.

## References

- `crates/qmind-sql/src/batch.rs`
- `crates/qmind-sql/src/batch_ops.rs`
- `crates/qmind-sql/src/result_stream.rs`
- `crates/qmind-sql/src/engine.rs` (`stream_query`)
- `docs/developer-guide/batch-execution.rst`