# Workload: HTAP

- **Workload**: `htap` — Concurrent OLTP writes + OLAP reads.
- **Primary scale**: S2–S4.

## SQL shape

**OLTP writer threads** (N threads):

```sql
INSERT INTO ticks (id, symbol, ts, px, n, flag) VALUES (...);
```

**OLAP reader threads** (M threads, running analytical scan):

```sql
SELECT symbol, AVG(px), COUNT(*) FROM ticks GROUP BY symbol;
```

## Measurement contract

### Procedure

1. Scale loaded.
2. N writer threads + M OLAP reader threads (document N, M).
3. Run until S3 rows total or for a fixed time window (document).
4. For each OLAP reader: record result set, wall time, and confirm no
   reader saw a torn/partial snapshot (R4+ writer concurrency).
5. Record: combined write throughput, reader QPS, per-reader p95 latency.
6. Record columnar refresh lag if delta-applier flushes happen concurrently.

### Metrics

| Metric | Unit |
|---|---|
| writer threads / OLAP reader threads | counts |
| total inserts (duration) | rows |
| writer throughput | rows/s |
| OLAP reader QPS | queries/s |
| OLAP reader p95 latency | ms |
| snapshot consistency check | pass/fail |
| delta flush lag observed | µs or bool |

### Environment (record per run)

- commit hash, OS, CPU, RAM, disk, build mode, thread config.

## Acceptance linkage

`BILLION_ROW_REQUIREMENTS.md` §4 (HTAP: concurrent writes + reads with
snapshot consistency, bounded freshness lag).