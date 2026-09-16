# R3 Benchmark Report

- dataset rows: **100000**
- iterations per step: 3
- insert chunk: 500 rows/statement
- seed: fixed (deterministic LCG)
- engine: quantsmind v0.1.0

## Environment

| field | value |
|---|---|
| commit | bc6f4326847bf536f0b685f3c3b4a0ae4f8b0bc5 |
| rustc | rustc 1.98.0 (88d9e12ae 2026-08-18) |
| cpu | Intel64 Family 6 Model 142 Stepping 10, GenuineIntel |
| ram | 4199690240 bytes |
| os | windows (x86_64) |
| build mode | release |

## Measured workloads

| workload | rows processed | mean wall (ms) | rows/s | execution path |
|---|---|---|---|---|
| bulk_insert | 100000 | 2511 | 39.82K | fsync-per-statement |
| wf_full_scan | 100000 | 178 | 561.80K | streaming, bounded |
| wf_filtered_scan | 10000 | 200 | 50.00K | streaming, bounded |
| wf_projection | 100000 | 237 | 421.94K | streaming, bounded |
| wf_limit | 1000 | 1 | 1.00M | streaming, bounded |
| wf_order_small | 100 | 349 | 286.53 | materialized fallback |
| wf_order_full | 100000 | 610 | 163.93K | materialized fallback |
| wf_agg_count | 1 | 248 | 4.03 | streaming (batch aggregate) |
| wf_agg_sum | 1 | 189 | 5.29 | streaming (batch aggregate) |
| wf_agg_avg | 1 | 138 | 7.25 | streaming (batch aggregate) |
| wf_group_by | 4 | 194 | 20.62 | streaming (batch aggregate) |
| wf_join | 100000 | 855 | 116.96K | streaming (batch hash join, materialized build) |
| wf_close | 0 | 191 | 0.00 | close (WAL flush + page flush), wall-time only |
| wf_open | 0 | 1363 | 0.00 | open_db recovery + page rebuild, wall-time only |
| wf_scan_after_reopen | 100000 | 175 | 571.43K | streaming after reopen (sanity) |

## Honesty notes

- Aggregation and GROUP BY run through the **batch aggregate** pipeline
  (`R3-EXEC-1`): the scan is read from persistent storage and groups are
  materialized in memory (external spill is deferred).
- JOIN runs through the **batch hash join** pipeline (`R3-EXEC-2`): both
  inputs are materialized from persistent storage and the right (build) side
  is hashed; memory is O(left+right), spill is deferred.
- Full scan and filtered scan are streaming (batch size 2048) and bounded-memory.
- ORDER BY uses the materialized fallback (the streaming path sorts in memory).
- Insert throughput is fsync-per-commit (each statement commits its own WAL
  group); there is no batching across statements yet.
