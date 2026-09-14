# Workload: Filtered Scan

- **Workload**: `filtered_scan` — Selective (≤1% of rows) scan with predicate.
- **Primary scale**: S1–S4.

## SQL shape

```sql
SELECT * FROM ticks WHERE symbol = :target_sym
                      AND px BETWEEN :lo AND :hi
                      AND flag = true;
```

`target_sym` from the `dim.name` subset; range so ≤1% of rows qualify.
Run 100 iterations; report p95/p99 latency and rows/s.

## Measurement contract

### Procedure

1. Scale loaded; no secondary index used (full scan + predicate filter).
2. Execute each query independently; record latency.
3. Verify result count is consistent across iterations (same data =
   same count).

### Metrics

| Metric | Unit |
|---|---|
| rows matching predicate | rows (avg) |
| query latency (p95/p99) | µs |
| rows returned per second | rows/s |
| memory high-water mark (R3+) | bytes |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §2 (selective scan ≤1%).