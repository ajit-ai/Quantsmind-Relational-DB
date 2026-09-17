Benchmark harness
=================

The R3 benchmark harness is a small, dependency-light harness that exercises
the engine against persistent file-backed databases. It prefers the repository's
existing benchmark conventions (criterion benches in the crates plus a scripts
directory under ``benchmarks/``).

You can read the current harness documentation and workload descriptions in the
repository's ``benchmarks/README.md`` and ``benchmarks/workloads/*.md``.

Dataset sizes
-------------

Sizes are configurable; the standard set is:

.. code-block:: text

   1_000
   10_000
   100_000
   1_000_000
   10_000_000

Large datasets are optional and are not required for normal development or CI.
Generated datasets are not committed to the repository.

Categories measured
-------------------

* **Storage / write** — bulk insert, sustained insert, page-store write
  throughput.
* **Scan** — full table scan, filtered scan, projection, LIMIT.
* **Ordering** — ORDER BY over small and large results; streaming vs
  materialized behavior.
* **Aggregation** — COUNT, SUM, AVG, GROUP BY.
* **JOIN** — small, larger, and representative equality joins.
* **Persistence** — database creation, close/reopen, recovery, query after
  reopen.
* **Streaming** — large-result streaming measured separately from fully
  materialized execution where both are available.

Metrics captured
----------------

Where practical the harness records elapsed time, rows/sec, operations/sec,
query latency, database size, WAL size, page-store size, recovery duration, and
peak memory/RSS if it can be measured reliably. Rows/sec is derived from real
elapsed time and row counts.

Environment recording
---------------------

Every result file records the environment so runs are reproducible: OS, CPU,
RAM, Rust version, repository commit, build profile, dataset size, and relevant
configuration.

Honesty rule
------------

Benchmarks report **real measured values only**. No result is asserted to prove
billion-row capability — R3 explicitly does not certify billion-row
qualification.