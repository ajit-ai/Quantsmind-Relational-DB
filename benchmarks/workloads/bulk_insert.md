# Workload: Bulk Insert

- **Workload**: `bulk_insert` — Batched row ingestion.
- **Primary scale**: S1–S4.

## SQL shape

```sql
INSERT INTO ticks (id, symbol, ts, px, n, flag) VALUES (:batch);
```

Batch sizes: 1,000; 10,000; 100,000 rows. Each batch is a separate
`INSERT` call (single multi-row statement per batch). Measure total rows/s
and per-batch latency.

## Measurement contract

### Procedure

1. Empty target table (no indexes beyond primary PK for R3+).
2. Insert rows in batches of the specified size until N rows total.
3. Record per-batch latency (µs) and aggregate rows/s.

### Metrics

| Metric | Unit |
|---|---|
| total rows inserted | rows |
| batch size used | rows/statement |
| overall rows/s | rows/s |
| mean batch latency | µs |
| p95 batch latency | µs |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §3 (insert throughput).