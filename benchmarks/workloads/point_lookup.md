# Workload: Point Lookup

- **Workload**: `point_lookup` — Indexed lookup by primary key.
- **Primary scale**: S1–S4.

## SQL shape

```sql
SELECT * FROM ticks WHERE id = :target_id;
```

`target_id` chosen uniformly at random from `[0, N)`; 10,000 iterations per
report; reported p50/p95/p99 latency and throughput (QPS).

## Measurement contract

### Procedure

1. Scale loaded per `datasets/README.md` for the target scale.
2. Secondary index (if R3+) on `id` (primary key index always present).
3. Issue 10,000 single-row SELECT statements; record each latency.
4. Record overall QPS and latency percentiles.

### Metrics

| Metric | Unit |
|---|---|
| p50 latency | µs |
| p95 latency | µs |
| p99 latency | µs |
| QPS | queries/s |
| index size | bytes |
| dataset size (row store) | bytes |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §1.5 (index size), §2 (point lookup p99).