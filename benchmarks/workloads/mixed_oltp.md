# Workload: Mixed OLTP

- **Workload**: `mixed_oltp` — Concurrent OLTP write-heavy mix.
- **Primary scale**: S1–S4 (S4 deferred until R4 multi-writer exists).

## SQL shape (per client thread)

```sql
-- writer path (80%)
INSERT INTO ticks (id, symbol, ts, px, n, flag) VALUES (:dynamic_id, ...);

-- update path (10%, only when UPDATE implemented at R4)
UPDATE ticks SET n = n + 1 WHERE id = :random_existing_id;

-- read path (10%)
SELECT * FROM ticks WHERE id = :random_existing_id;
```

(Multi-statement `;`-split, or prepared statements at R7+.)

## Measurement contract

### Procedure

1. Scale loaded; N writer threads + M reader threads (document N, M per run).
2. Each writer thread does K batch inserts (batch size fixed, document it).
3. Readers do indexed lookups. Report combined: insert rows/s, reader QPS,
   p95 reader latency, conflict rate (R4+).
4. Report total run time.

### Metrics

| Metric | Unit |
|---|---|
| writer threads | count |
| reader threads | count |
| total inserts completed | rows |
| insert throughput | rows/s |
| reader QPS | queries/s |
| reader p95 latency | µs |
| conflict rate / restart rate (R4+) | % |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode, thread config.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §3 (insert throughput), §4 (HTAP interference).