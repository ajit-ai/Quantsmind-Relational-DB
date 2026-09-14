# Workload: Aggregation

- **Workload**: `aggregation` — GROUP BY over full or partial dataset.
- **Primary scale**: S1–S4.

## SQL shape

Variant A (partial, ≤10% rows):

```sql
SELECT symbol, COUNT(*), AVG(px), SUM(n)
FROM ticks
WHERE flag = true
GROUP BY symbol;
```

Variant B (full):

```sql
SELECT symbol, COUNT(*), AVG(px), SUM(n)
FROM ticks
GROUP BY symbol;
```

Run variant A at all scales; variant B from S3+ (measure bounded-memory
handling). 10 iterations; report mean latency and rows scanned.

## Measurement contract

### Procedure

1. Scale loaded; execute query; record wall time and result set size.
2. Verify deterministic result counts across iterations.
3. At S4: report memory high-water mark (bounded/possible with spill).

### Metrics

| Metric | Unit |
|---|---|
| rows scanned | rows |
| distinct groups produced | count |
| wall time (mean) | ms |
| memory high-water mark (R3+) | bytes |
| spill-to-disk (R3+) | bool/bytes |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §2 (aggregation), §5 (spill), §6 (bounded
memory).