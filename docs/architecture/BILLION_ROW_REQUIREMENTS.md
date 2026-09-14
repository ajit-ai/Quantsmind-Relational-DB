# Billion-Row Requirements (R1.7)

> R1 deliverable — `docs/architecture/BILLION_ROW_REQUIREMENTS.md`
>
> Technical requirements for 1M / 10M / 100M / 1B row datasets.
> **No performance numbers are invented here.** Every requirement is stated as
> a measurable acceptance criterion with a benchmark stem from `benchmarks/`
> that will fill in the numbers (`benchmarks/README.md`). Results are populated
> only by real runs (R6/R9 qualification).

Scales: **S1 = 1M**, **S2 = 10M**, **S3 = 100M**, **S4 = 1B**.

Table data set for all benchmark stems: `benchmarks/datasets/` (synthetic,
deterministic generator with realistic distributions; ~20-byte-wide dense row:
`id i64, sym char(8), ts ts, px f64, n i64, flag bool`).

---

## 1. Storage

### 1.1 Storage footprint

| Requirement | S1 | S2 | S3 | S4 | Measure |
|---|---|---|---|---|---|
| Dense-row storage bytes/row | ✓ | ✓ | ✓ | ✓ | `wf_full_scan`/dataset dataset size |
| Column-store bytes/row (R4+) | ✓ | ✓ | ✓ | ✓ | same |
| On-disk WAL growth bounded by checkpoints | ✓ | ✓ | ✓ | ✓ | `wf_recovery` after checkpoint |

Acceptance: bytes/row and total DB dir reported per scale in
`benchmarks/results/{scale}/`. No target number is asserted without a run.

### 1.2 Compression

- Column store compression must be **lossless** (RLE present today
  `columnar.rs`; extend with dictionary/bit-packing R4).
- Acceptable metric: compressed:raw ratio per column per scale in
  `benchmarks/results/{scale}/`. No invented ratio.

### 1.3 Metadata

- System catalog must be durable (R2). Requirement: opening a 1B-row database
  reads catalog/first-page metadata in **constant time** independent of scale.
- Acceptance: catalog load time measured at each scale.

### 1.4 Partitioning (R5+)

- Range/hash partitioning available so any single table can be physically
  split; partition pruning verified by `wf_filtered_scan` at S3/S4 scan widths.
- Acceptance: query plans (EXPLAIN output) show pruned partitions.

### 1.5 Indexing

- Secondary index maintenance must stay on the write path with bounded cost
  per index; index size reported per scale.
- Acceptance: `wf_point_lookup` (indexed) vs `wf_filtered_scan` (sequential)
  latency and index footprint per scale.

---

## 2. Querying

| Workload | Requirement | S1 | S2 | S3 | S4 |
|---|---|---|---|---|---|
| Point lookup (indexed) | p99 latency and throughput | ✓ | ✓ | ✓ | ✓ |
| Selective scan | p99 latency for ≤1% filter | ✓ | ✓ | ✓ | ✓ |
| Full scan | throughput (rows/s) over whole table | ✓ | ✓ | ✓ | ✓ |
| Aggregation | GROUP BY over full/percentage of table | ✓ | ✓ | ✓ | ✓ |
| Join | hash join, equi on 100M/1M pairs | ✓ | ✓ | ✓ | ✓ |
| Sort | ORDER BY full table | ✓ | ✓ | ✓ | ✓ (bounded memory) |

Requirements (behavioral, no invented values):

- All read workloads must produce correct results identical to a reference
  run; measured latency/throughput recorded per scale.
- At S4, every workload must respect a bounded-memory budget (see §5).

---

## 3. OLTP

- **INSERT throughput** (rows/s, batched) — `wf_bulk_insert`.
- **UPDATE / DELETE** — no support until R4 (`REPLACE` in assessment); when
  implemented, `wf_mixed_oltp` covers them on top of inserts.
- **Transaction throughput** (commits/s) — engine-level txn commits per second.
- **Concurrent writers** — at R4+: N writer threads (N ≥ 8) with early-abort
  conflict detection; `wf_mixed_oltp` measures combined throughput with
  readers.

Acceptance per scale: throughput + latency distributions recorded; no
fabricated target numbers.

---

## 4. HTAP

- **Concurrent OLTP writes + OLAP reads** with snapshot isolation:
  - Readers at any active snapshot observe a consistent single point-in-time
    even while writers commit (already structurally true post-P5:
    `engine.rs:194` statement-scoped snapshots, `wire.rs:107-124`).
  - Column-store freshness lags commits by an explicit, bounded, documented
    interval (`column_delta.rs` threshold flush).
- Workload: `wf_htap` — M writers + K OLAP scans reading the same table.
- Acceptance: analytical query correctness vs synchronous reference; lag
  histogram and OLTP interference recorded.

---

## 5. Reliability

| Requirement | Detail | Gate |
|---|---|---|
| Crash recovery | kill -9 / Ctrl-C (no graceful close) at arbitrary points; restart must reach a consistent prior committed state | `wf_recovery` |
| Checkpoint | scheduled checkpoint bounds recovery time; WAL truncated after successful checkpoint | `wf_recovery` |
| WAL | fsync-disciplined group commit (R2); commit_ts→LSN ordering preserved | `wf_recovery` |
| Restart | open-start pipeline replays committed txns and rebuilds catalog | `wf_recovery` |
| Corruption detection | CRC failure on page/WAL/segment detected, reported, and isolated | `wf_recovery` + kernel property tests |

No fabricated crash-recovery numbers; `wf_recovery` reports e.g.
"recover to last committed txn #, in T seconds at scale S".

---

## 6. Resource management

| Requirement | Detail |
|---|---|
| RAM limits | engine buffer pool + query budgets configurable; excess spills or aborts, never OOM |
| Disk usage | total on-disk footprint reported; WAL bounded by checkpoints |
| Query cancellation | cancelable queries at any scan/operator (R6 server) |
| Spill-to-disk | sort/aggregation spill beyond in-memory budget (R3). Acceptance: `wf_sort`/`wf_aggregation` at S4 complete within budget |
| Concurrency limits | max connections, per-query timeouts, per-session memory (R6) |

---

## 7. What must be true, explicitly

1. **Durability** trumps throughput: every committed transaction's effects are
   recoverable after restart (R2 gate).
2. **Memory-bounded**: a 1B-row database must not require 1B rows in RAM; the
   design target is a fixed-budget working set plus columnar scan.
3. **HTAP coherence**: one MVCC timeline, monotonic columnar refresh.
4. **Measured, not claimed**: every S1–S4 row in this document is a criterion
   awaiting `benchmarks/results/**` data — R1 establishes the structure, R6/R9
   perform and publish the runs.