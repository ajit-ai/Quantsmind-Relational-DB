# Workload: Full Scan

- **Workload**: `full_scan` — Full-table sequential scan (no predicate).
- **Primary scale**: S2–S4.

## SQL shape

```sql
SELECT * FROM ticks;
```

(Or, to measure scan throughput more cleanly at scale: `SELECT id FROM ticks` —
a narrow column scan if supported by column store. Document which variant is
run.)

10 iterations; report total time, scan throughput (rows/s, GB/s if possible),
and memory high-water mark if bounded-memory (R3+) scan is used.

## Measurement contract

### Procedure

1. Scale loaded; full table in row store and/or column store; document which.
2. Execute full scan; record wall time and rows returned.
3. Repeat 10 times; report mean/median/mean+stddev.

### Metrics

| Metric | Unit |
|---|---|
| total rows scanned | rows |
| wall time (mean) | ms |
| scan throughput | rows/s |
| data volume scanned | GB |
| memory high-water mark (R3+) | bytes |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §2 (full scan throughput).