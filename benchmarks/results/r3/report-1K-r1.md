# R3 Benchmark Report

- dataset rows: **1000**
- iterations per step: 1
- insert chunk: 500 rows/statement
- seed: fixed (deterministic LCG)
- engine: quantsmind v0.1.0

## Environment

| field | value |
|---|---|
| commit | n/a |
| rustc | n/a |
| cpu | n/a |
| ram | n/a |
| os | windows (x86_64) |
| build mode | debug |

## Measured workloads

| workload | rows processed | mean wall (ms) | rows/s | execution path |
|---|---|---|---|---|
| bulk_insert | 1000 | 78 | 12.82K | fsync-per-statement |
| wf_full_scan | 1000 | 3 | 333.33K | streaming, bounded |
| wf_filtered_scan | 100 | 4 | 25.00K | streaming, bounded |
| wf_projection | 1000 | 5 | 200.00K | streaming, bounded |
| wf_limit | 1000 | 3 | 333.33K | streaming, bounded |
| wf_order_small | 100 | 7 | 14.29K | materialized fallback |
| wf_order_full | 1000 | 10 | 100.00K | materialized fallback |
| wf_agg_count | 1 | 3 | 333.33 | volcano/materialized |
| wf_agg_sum | 1 | 4 | 250.00 | volcano/materialized |
| wf_agg_avg | 1 | 5 | 200.00 | volcano/materialized |
| wf_group_by | 4 | 6 | 666.67 | volcano/materialized |
| wf_join | 1000 | 10 | 100.00K | volcano/materialized |
| wf_reopen | 1000 | 14 | 71.43K | close + open_db (includes page rebuild) |
| wf_open | 1000 | 75 | 13.33K | open_db recovery + page rebuild |
| wf_scan_after_reopen | 1000 | 5 | 200.00K | streaming after reopen (sanity) |

## Honesty notes

- Aggregation and JOIN run on the **volcano/materialized** executor; the
  bounded-memory streaming path still rejects them (R3-EXEC-1/2). Rows/s for
  those rows are not streaming measurements.
- Full scan and filtered scan are streaming (batch size 2048) and bounded-memory.
- ORDER BY uses the materialized fallback (the streaming path sorts in memory).
- Insert throughput is fsync-per-commit (each statement commits its own WAL
  group); there is no batching across statements yet.
