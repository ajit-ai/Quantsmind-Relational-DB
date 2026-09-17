R4-MULTIWRITER Completion Report
================================

Objective
---------

Remove the global "single explicit writer" constraint at the SQL/server layer
and enable **multi-writer sessions**: multiple connections, each holding its
own explicit write transaction concurrently, over one engine and one WAL.
Writers still go through the deterministic **no-wait** row-lock path
(``LockManager::try_lock``), so SQL conflicts are immediate errors and SQL
deadlock cycles cannot form. Explicitly preserved, unchanged, and re-proven:
Snapshot Isolation, per-session independent snapshots, one active transaction
*per session*, strict-2PL row locks, first-committer-wins, WAL-failure
auto-reap, disconnect rollback, index at-commit materialization (txn-aware
scan fallback), multi-row INSERT statement atomicity, and recovery. Explicitly
**not** introduced: SQL blocking/queueing, SQL deadlock handling, automatic
retry, Serializable isolation, last-writer-wins, or global lock serialization.

Starting baseline
-----------------

* 24 test suites, 282 tests, 0 failures
* ``fmt`` PASS, ``clippy -D warnings`` PASS, Sphinx (``-W``) PASS, 0 warnings
* Baseline commit on ``develop``: ``74f8482`` (feat: validate deadlock
  detection and lock cleanup)

What changed: per-session active transactions
---------------------------------------------

`Engine` previously held at most **one** explicit transaction
(``active: Option<ActiveTxn>``). It is now

.. code-block:: rust

   active: HashMap<SessionId, ActiveTxn>

with ``pub type SessionId = u64``. Each session's transaction carries its own
kernel txn id, pinned snapshot, strict-2PL row locks and buffered rows, so any
number of sessions can hold open transactions independently. Statement
execution is still serialized by ``&mut self`` / the server's engine write
guard, but transactions themselves are fully concurrent — any subset may roll
back or commit.

Engine API (``crates/qmind-sql/src/engine.rs``):

* ``execute(sql)`` — unchanged embedded entry point (session 0);
* ``execute_session(session, sql)`` — new; routes every statement through the
  calling session's transaction if it has one, else autocommit;
* ``session_in_transaction(session)`` — new (server routing), with
  ``in_transaction()`` kept as the session-0 convenience;
* ``txn_begin/txn_commit/txn_rollback(session)`` and the DDL-inside-txn guard
  are per session; ``insert(session, ...)`` joins the session's transaction.

Server wire (``crates/qmind-server/src/wire.rs``):

* ``serve`` assigns each connection a unique ``SessionId``
  (``AtomicU64``);
* routing replaces the old ``foreign_txn`` / ``single-writer constraint``
  rejection: a session in its own transaction runs every statement (including
  SELECT) on the write path for own-write visibility; sessions outside a
  transaction read committed-only snapshots and write in autocommit;
* disconnect handling rolls back **that session's** transaction only
  (``release_session_txn``), never touching other sessions.

Row-key disjointness (design note)
----------------------------------

Physical row ids are append-only and allocated from the engine's shared
per-table counter at statement time, and statements serialize through the
write guard. Two sessions' ``INSERTs`` therefore always target **disjoint**
physical rows — through the current INSERT-only dialect there is no way for two
SQL sessions to request the same row key. Consequences:

* cross-session row-lock ``Busy`` and first-committer-wins are *structurally
  unreachable through SQL today* (single-session retry/re-entrancy paths
  unchanged);
* the SQL mapping strings (``statement failed: row locked by another
  transaction``, ``transaction aborted: write-write conflict ...``) are
  preserved and correct, and become reachable the moment a surface that
  targets existing keys exists (UPDATE/DELETE or UNIQUE/PK);
* the conflict machinery is proven where it *does* run: at the kernel, with a
  new row-local race proof (below).

Tests added
-----------

* ``crates/qmind-sql/tests/multiwriter.rs`` (**new suite, 10 tests**, engine
  level, two sessions interleaved on one thread):
  (1) two independent write transactions begin, write, and commit — each sees
  only its own uncommitted rows; a foreign commit stays invisible to the
  open snapshot;
  (2) snapshot isolation holds with multiple writers (a foreign commit between
  two of a session's reads changes neither);
  (3) a failed statement in one session leaves the other's transaction and the
  first session's own earlier write fully intact;
  (4) concurrent indexed writers: both sessions' rows reachable through the
  materialized index after commit; own-write index fallback inside a txn;
  (5) interleaved multi-row INSERTs into one table allocate disjoint physical
  rows — both transactions commit, exactly once each (a live proof that the
  shared counter hands out disjoint ranges under interleaving);
  (6) autocommit writes proceed while an explicit transaction is open in
  another session (disjoint rows, snapshot-pinned reads);
  (7) a second session may ``BEGIN`` while the first transaction is open;
  re-``BEGIN`` on the same session still errors;
  (8) the DDL boundary is per session — DDL inside one's own transaction is
  rejected, foreign DDL/index backfill proceed concurrently;
  (9) rollback of one session preserves the other session's committed rows;
  (10) multiwriter commits survive reopen; an in-flight transaction at crash
  leaves no trace (recovery preserved).
* ``crates/qmind-kernel/tests/concurrency_semantics.rs`` (**+1, now 5**):
  ``row_local_conflicts_fail_fast_without_blocking_concurrent_rows`` — a
  third writer to a held row gets deterministic no-wait ``Busy`` while a
  writer to an untouched row of the same table proceeds (row-local
  granularity); after the winner commits, a stale-snapshot writer is rejected
  by first-committer-wins at commit even though the lock is free.
* ``crates/qmind-server/tests/wire_e2e.rs`` (**+2, now 13**, after rewriting
  the five tests that asserted the removed single-writer rejection):
  (a) ``wire_interleaved_multirow_explicit_writers_both_commit`` — two TCP
  sessions interleave multi-row INSERTs inside concurrent explicit
  transactions on one table; both commit; every physical row present once;
  (b) ``rollback_in_one_wire_session_does_not_disturb_the_other`` — a
  session's ROLLBACK leaves the other session's open transaction and its
  (later committed) rows intact.
  The rewritten tests document the new model: foreign writes are no longer
  blocked while a transaction is held (they commit on disjoint rows); the
  owner's pinned snapshot stays stable across a foreign commit; a
  disconnected session's transaction is rolled back automatically without
  blocking anyone; a failed statement keeps only its own session's
  transaction open; two sessions hold concurrent transactions and commit
  independently.

Final test count
----------------

.. list-table::
   :header-rows: 1

   * - Gate
     - Baseline
     - This phase
   * - Test suites
     - 24
     - 25 (new ``multiwriter`` suite)
   * - Tests passed
     - 282
     - 295 (13 added: +10 multiwriter, +1 kernel, +2 wire; 0 failures)

Exact per-suite counts (captured from the run):

* qmind-kernel: 93 (lib) + 5 (concurrency_semantics, +1) + 6 (correctness)
  + 12 (deadlock) + 3 (kernel_integration) + 9 (lock_2pl)
  + 9 (mvcc_visibility) + 6 (read_stress) = 143
* qmind-sql: 137 (50 lib + 4 concurrency + 9 crash_recovery + 10 multiwriter
  + 4 parser_fuzz + 12 persistence + 1 soak + 29 sql_e2e + 18 transactions)
* qmind-server: 13 (wire_e2e, +2); qmind-embed: 2 (embed_api)

Total accounted: 143 + 137 + 13 + 2 = 295

Documentation
-------------

* ``docs/concepts/concurrency.rst``: model sentence now reads "Snapshot
  Isolation with one explicit write transaction per session"; the
  single-writer ownership section is replaced by the per-session model; the
  conflict surfaces (kernel row locks, kernel first-committer-wins, SQL
  mapping) and the explicitly-unsupported list are updated; the row-key
  disjointness note is documented.
* ``docs/concepts/transactions.rst``: "Single-writer constraint" section
  replaced by "One transaction per session"; error/recovery bullets updated.
* ``docs/concepts/locking.rst``: SQL layer paragraph describes per-session
  transactions and the row-local, kernel-proven conflict granularity.
* ``docs/concepts/mvcc.rst``: concurrency and "Current limitation" sections
  now state multi-writer sessions are supported and explain why same-key SQL
  conflicts await an UPDATE/DELETE/PK surface.
* ``docs/concepts/deadlocks.rst``: "no SQL deadlock handling is claimed" now
  derives from the no-wait path under multi-writer, not from single-writer.
* ``docs/architecture/index.rst``: toctree gains this report.
* ``crates/qmind-kernel/tests/mvcc_visibility.rs``: stale
  "single-writer constraint" comment corrected to the multi-writer model.

Compatibility
-------------

All prior semantics preserved and re-proven: every baseline test still passes;
MVCC snapshot isolation, per-session stable snapshots, first-committer-wins,
strict-2PL no-wait locking, WAL-failure auto-reap, disconnect rollback, index
at-commit materialization, multi-row statement atomicity, and recovery are
unchanged. The removed ``single-writer constraint`` rejection is the one
intentional behavior change — its wire tests were converted to the new model.
No Serializable Isolation, no automatic SQL blocking, no SQL deadlock handling,
no implicit retry/restart introduced.

SQL blocking / deadlock status (exact)
--------------------------------------

* SQL write path: **no-wait only** — ``try_lock``, immediate ``Busy``; the SQL
  runtime never calls ``acquire``, never queues.
* SQL deadlock: **cannot form** — no SQL statement ever blocks on a lock, so
  there is no SQL wait cycle to detect; no SQL deadlock handling exists or is
  claimed at any level of multi-writer concurrency.
* Multi-writer SQL conflicts: unreachable through the current INSERT-only
  dialect (disjoint append-only row ids); the deterministic no-wait conflict
  delivery and first-committer-wins are proven at the kernel.

Known limitations
-----------------

* Same-row SQL conflicts wait for an UPDATE/DELETE or UNIQUE/PK surface; the
  kernel machinery (no-wait ``Busy``, FCW, row-local granularity) is proven
  and mapped, just not SQL-reachable today.
* Statement execution remains serialized by the engine write guard — this phase
  delivers concurrent *transactions*, not parallel statement execution.
* Kernel blocking ``acquire`` + scheduler, ``Deadlock``-error SQL plumbing, and
  SQL abort-and-retry semantics remain future (R4-DEADLOCK left them
  validated-but-unused).

Validation results
------------------

* Complete test suite: **25 suites, 295 tests, 0 failures** (exact per-suite
  counts captured from the actual run).
* ``cargo fmt --all -- --check``: PASS
* ``cargo clippy --workspace --all-targets --all-features -- -D warnings``: PASS
* Sphinx ``-W -b html``: PASS, 0 warnings

Revision
--------

* Branch: ``develop`` (no merge to ``main``, no push, no tag)
* Baseline (parent) commit: ``74f8482``
* Phase commit: ``feat: enable SQL multi-writer transactions`` — see
  ``git log -1 --oneline`` on ``develop``
* Working tree: clean after the phase commit

Next phase
----------

R4-MULTIWRITER leaves concurrent *transactions* proven. The remaining SQL
concurrency work is either (a) a row-targeting write surface (UPDATE/DELETE)
that makes the proven no-wait conflict delivery SQL-reachable, or (b)
integrating the kernel's validated blocking scheduler with SQL abort-and-retry
for genuine blocking writers. Recommendation: (a) first — it completes the
multi-writer conflict story through the public SQL dialect without touching
isolation or durability; (b) remains optional and kernel-only until a workload
actually needs blocking.