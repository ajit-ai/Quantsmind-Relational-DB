# Workload: Join

- **Workload**: `join` — Equi hash join between `ticks` and `dim`.
- **Primary scale**: S1–S4.

## SQL shape

```sql
SELECT d.grp, COUNT(*), AVG(t.px)
FROM ticks t
JOIN dim d ON t.symbol = d.dim_id   -- dimension join
WHERE d.grp IN (0,1,2,3)
GROUP BY d.grp;
```

(`dim.dim_id` is the PK; join is a 1:N foreign key pattern.) Run 10
iterations; report p95/p99 latency and rows scanned.

## Measurement contract

### Procedure

1. Scale loaded; `dim` populated with fixed small cardinality (~10,000–50,000
   rows depending on S1–S4 setting — document exact size per scale).
2. Execute query; record wall time.
3. Verify deterministic aggregate counts across iterations.

### Metrics

| Metric | Unit |
|---|---|
| rows scanned (ticks) | rows |
| dim rows (small side) | rows |
| wall time (mean) | ms |
| join type chosen (HashJoin) | confirm |
| memory high-water mark (R3+) | bytes |
| spill (R3+) | bool/bytes |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §2 (join).