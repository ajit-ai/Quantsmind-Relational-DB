# Workload: Sort

- **Workload**: `sort` — Full-table ORDER BY.
- **Primary scale**: S1–S4.

## SQL shape

```sql
SELECT id, px, symbol FROM ticks ORDER BY px DESC;
```

10 iterations; report p95/p99 latency. At S3/S4: measure with bounded memory
(R3+); report if spill occurred and its magnitude.

## Measurement contract

### Procedure

1. Scale loaded.
2. Execute query; record wall time and result size (all rows must be
   returned).
3. Repeat 10 times; report mean/median/mean+stddev.

### Metrics

| Metric | Unit |
|---|---|
| total rows sorted | rows |
| wall time (mean) | ms |
| sort rows/s | rows/s |
| memory high-water mark (R3+) | bytes |
| spill-to-disk (R3+) | bool/bytes |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §2 (sort), §5 (spill), §6 (bounded memory).