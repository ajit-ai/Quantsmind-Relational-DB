R4-MVCC Completion Report
=========================

1. Objective
------------

Establish and verify the **SQL-layer MVCC visibility contract** for explicit
transactions and concurrent sessions during R4 (Transactions & Reliability).
This phase proves Snapshot Isolation semantics through tests at three layers —
kernel, SQL engine, and wire (server session) — and documents the contract.

2. Starting State
-----------------

R4-TXN and R4-SQL-TXN were complete: explicit ``BEGIN`` / ``COMMIT`` /
``ROLLBACK`` lifecycle, transaction-aware MVCC reads, deferred commit logging
(``[Begin, Put…, Commit]`` WAL group synced at COMMIT), deferred
materialization of indexes/pages/columnar deltas, and session-aware server
routing with the single-writer constraint.

Baseline before this phase: 20 test suites, 0 failures; ``fmt`` and ``clippy
-D warnings`` clean.

3. Implemented MVCC Behavior
----------------------------

Snapshot Isolation at the kernel and SQL layer:

* a snapshot is an immutable watermark ``read_ts`` captured at ``BEGIN``;
* versions with ``commit_ts <= read_ts`` are visible, later commits are not;
* transactions read their own buffered writes first (read-your-own-writes),
  then the committed chain under the snapshot;
* foreign uncommitted writes are never consulted;
* first-committer-wins rejects a commit that conflicts with a post-snapshot
  commit on the same key.

4. Snapshot Lifecycle
---------------------

Established once at ``BEGIN`` and pinned for the transaction's lifetime. It
never advances: later commits remain invisible, and repeated SELECTs inside one
transaction observe the same committed world. Non-transactional SELECTs
(autocommit and foreign sessions) take a per-statement snapshot, which is the
correct "transaction boundary" for a single statement.

5. Own-Write Visibility
-----------------------

Verified on every read path: full scan, secondary-index equality lookup,
aggregate (``COUNT``), GROUP BY, and JOIN with the transaction's own buffered
row on one side.

Phase fix: secondary-index trees are materialized only at commit, so an index
equality lookup inside an explicit transaction now falls back to the
transaction-aware table scan (autocommit/foreign reads keep the index fast
path). This keeps own uncommitted rows reachable through indexed predicates.

6. Foreign-Write Visibility
---------------------------

A second live transaction never observes another transaction's uncommitted
writes (kernel rule test with two concurrent ``MvccStore`` transactions). At the
server level a foreign session's scan and index reads return committed state
only while the owner holds an open transaction.

7. Commit/Rollback Visibility
-----------------------------

* Committed-before-snapshot rows are visible.
* Post-snapshot commits are invisible to an existing snapshot (the SI/RC
  discriminator, proven with two live kernel transactions).
* Rollback removes buffered rows, index entries, columnar deltas, page state,
  and MVCC versions; rolled-back rids become gaps that index backfill handles.

8. Index Visibility
-------------------

Index reads preserve MVCC visibility: owner reads through an index see own
uncommitted rows; foreign readers do not; aborted inserts are never
discoverable. Phase fix: ``CREATE INDEX`` backfill now maps each committed row
to its real row key instead of enumerating present rows, so row-id gaps left by
rolled-back transactions no longer misalign lookups.

9. Execution-Path Visibility
----------------------------

The transaction context (id + pinned snapshot) threads through scan → filter →
projection → aggregate → GROUP BY → JOIN. One MVCC implementation is shared;
operators consume the transaction-aware scan. Phase fix: SELECT inside an
explicit transaction reads the MVCC row store rather than the committed-only
columnar projection, so committed + own buffered rows are both visible.

10. Recovery Interaction
------------------------

Recovery replays committed transactions only; in-flight transactions leave no
WAL footprint (deferred logging). After reopen, committed rows are visible and
in-flight/rolled-back/aborted rows are absent, for both the row store and
secondary indexes.

11. Server Session Tests
------------------------

* Owner sees own uncommitted rows; foreign scan and index reads stay empty
  until COMMIT; a fresh transaction on the foreign session then sees the row.
* The owner's snapshot view is stable across the transaction (foreign writes
  are rejected under the single-writer constraint, so no interleaved commit can
  perturb the pinned snapshot).
* Rolled-back owner rows are invisible to a foreign session.

12. Test Results
----------------

Run with ``cargo test --workspace --all-targets``:

.. list-table::
   :header-rows: 1

   * - Suite
     - Tests
     - Notes
   * - qmind-kernel ``mvcc_visibility``
     - 9
     - new; SI rules 1–8 + recovered visibility, two live transactions
   * - qmind-sql ``transactions``
     - 14
     - new R4-MVCC cases added (13 prior R4-TXN cases + additions below)
   * - qmind-server ``wire_e2e``
     - 4
     - R4-MVCC session tests added (3 existing R4-TXN/R4-SQL-TXN + additions)

Previous R4 baseline: 20 suites, 0 failures. Final count is taken from the
actual ``--all-targets`` run reported in phase 13/14 below.

13. Documentation
-----------------

* ``docs/concepts/mvcc.rst`` — the normative visibility contract (rules 1–8,
  snapshot lifecycle, index/execution-path/recovery interaction, limitation).
* ``docs/concepts/transactions.rst`` — SQL transaction control, lifecycle,
  deferred commit logging, materialization order, single-writer constraint,
  columnar interplay, row-id gaps.
* This report.
* ``docs/concepts/index.rst`` toctree updated.

14. Known Limitations
---------------------

* Single-writer constraint persists: foreign writes are rejected while an
  explicit transaction is open; multiple simultaneous writer sessions are a
  later R4 phase.
* Snapshot-isolation rules that require two overlapping writers are proven at
  the kernel layer; the SQL layer remains single-writer by design.
* Columnar segments are a committed-only projection; the transaction-aware row
  store is the read path inside explicit transactions.
* SQL JOIN projections use unqualified column names (parser dialect limit,
  unchanged).
* DDL is autocommit-only (rejected inside explicit transactions).

15. Remaining R4 Work
---------------------

R4-MVCC does **not** complete R4.

Concurrent writer support, locking/deadlock hardening, checkpointing, and WAL
lifecycle (truncation/rotation), failure injection, persistence hardening, and
reliability benchmarks remain subsequent R4 work.