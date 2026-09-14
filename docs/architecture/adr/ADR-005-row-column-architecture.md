# ADR-005 — Row/Column Architecture (HTAP)

- **Status**: Accepted (R1)
- **Date**: R1 architecture gate

## Context

The product is HTAP: OLTP writes must coexist with OLAP reads on the same
logical tables at billion-row scale. Current state: a row-oriented in-memory
MVCC store is the OLTP path (`engine.rs:335`, `mvcc.rs:100`); an experimental
on-disk columnar segment path exists in the kernel and is wired into SQL reads
when enabled (`engine.rs:90` `with_columnar`, `engine.rs:396-398` read
preference; `columnar.rs`, `column_delta.rs`, `column_reader.rs`).

## Decision

A **coordinated row + column architecture with a single MVCC timeline**:

- OLTP reads/writes use the row store (slotted pages at R3).
- OLAP scans use the column store (segments, R4+ features).
- Both derive from one commit-timestamped timeline; the column store is
  refreshed from delta buffers (`column_delta.rs`) monotonically behind
  commit, so snapshot reads are always consistent.
- The existing columnar subsystem is retained as the OLAP lane, hardened
  rather than rewritten.

## Consequences

- The column store may lag commits by a bounded, documented interval (delta
  flush threshold) — acceptable for OLAP freshness guarantees.
- Predicate/aggregation pushdown (R4/R5) must be planned against both lanes.
- No second MVCC implementation may appear in the column lane; one GC
  watermark governs both.

## References

- `docs/architecture/TARGET_ARCHITECTURE.md` (§3 target storage path)
- `crates/qmind-kernel/src/{columnar,column_delta,column_reader,mvcc}.rs`
- `docs/architecture/BILLION_ROW_REQUIREMENTS.md` (HTAP §4)