R4-LOCK Completion Report
=========================

Objective
---------

Wire the kernel's strict-2PL ``LockManager`` into the SQL/MVCC runtime as the
write-concurrency control of record: every row a transaction writes is
exclusively locked from its first write until commit/abort, with conflicts
resolved deterministically (no waits), complementing the existing
first-committer-wins validation. Do so without redesigning MVCC, WAL,
indexes, or the query engine, and preserve every R4-MVCC / R4-CONCURRENCY
guarantee — pinned snapshots, own-write visibility, first-committer-wins,
single-writer SQL boundary, deferred index materialization, and disconnect
rollback. Objectives:

* make ``MvccStore::set`` fallible with a dedicated ``LockError`` (busy /
  deadlock) surfaced from a non-blocking ``try_lock``;
* add a strict-2PL lifecycle: locks acquired on first write, released at
  COMMIT / ROLLBACK / abort, auto-released on WAL-sink failure and session
  disconnect;
* keep first-committer-wins: a stale-snapshot write over a key committed after
  its begin still loses at commit even after the lock is released;
* make multi-row INSERT statement-atomic at the lock level (reserve all rows
  before writing any);
* validate with a dedicated kernel lock suite + property-test update + SQL
  tests, update the concurrency/transactions/mvcc concept docs and add a
  locking concept chapter, and land one clean commit.

Starting baseline
-----------------

* 22 test suites, 253 tests, 0 failures
* ``fmt`` PASS, ``clippy -D warnings`` PASS, Sphinx (``-W``) PASS, 0 warnings
* Baseline commit on ``develop``: ``71b770a`` (R4-CONCURRENCY)

Locking model
-------------

    **Strict two-phase locking over row keys, with deterministic no-wait
    conflict rejection.**

* The kernel ``LockManager`` (existing S/X lock table, FIFO ``acquire`` queue,
  waits-for DFS deadlock detection — untouched) gains a non-blocking
  ``try_lock`` that grants immediately when compatible and otherwise returns
  ``LockError::Busy`` without enqueueing. A no-wait request can never deadlock.
* ``MvccStore`` owns a ``LockManager``. ``set`` now first takes the exclusive
  lock on the row key (``String::from_utf8_lossy(key)`` resource) and returns
  ``Result<(), LockError>``; a new ``lock_write`` reserves the exclusive lock
  without buffering a value. Locks are held to the transaction boundary
  (strict 2PL).
* The SQL engine's explicit INSERT reserves every row key of the statement
  (``lock_write``) before writing any value, then writes re-entrantly; a
  ``Busy`` mid-statement is a statement error, earlier reservations held until
  transaction end (no partial rows). The autocommit INSERT uses the fallible
  ``set`` loop and aborts the implicit transaction on failure.
* Reads and snapshots remain lock-free: a snapshot is still a pinned watermark
  and scans acquire nothing.
* At the SQL layer the single-writer gate serializes writers, so row locks
  never collide there; the locks defend the kernel store's own multi-writer
  API and are proven at that layer.

Conflict semantics
------------------

* **Kernel (row locks):** a second writer on a live row gets an immediate
  ``LockError::Busy`` and buffers nothing; the loser aborts and a fresh
  transaction retries. No queueing, no waiting, no timeouts.
* **Kernel (first-committer-wins, retained):** a writer whose snapshot predates
  a concurrent commit on one of its keys loses at ``COMMIT`` with a
  deterministic ``Conflict`` and publishes nothing — including when the set
  landed after the holder released the lock (stale-snapshot write). The two
  mechanisms are complementary: locks prevent concurrent holds, MVCC rejects
  stale overwrites.
* **SQL/session (single-writer gate):** foreign writes while a transaction is
  open are still rejected before any execution.
* The runtime never uses the blocking ``acquire`` path; the FIFO queue and
  deadlock detection remain API capabilities that the SQL/MVCC write path
  deliberately does not exercise.

Lifecycle and cleanup
---------------------

* Locks are released by success ``COMMIT`` (after publication), ``ROLLBACK`` /
  abort (including automatic rollback on session disconnect), and a
  first-committer-wins conflict once the loser aborts.
* A WAL-sink failure at ``COMMIT`` now reaps the transaction fully at once —
  id, pending writes, and row locks — with **no caller cleanup**; the previous
  contract required the caller to ``abort`` explicitly (idempotent no-op now).
* A failed statement does not release locks (statement failure ≠ rollback); a
  failed multi-row INSERT leaves no partial written set.

Code changes
------------

* ``crates/qmind-kernel/src/lock.rs``: ``LockError::Busy``; ``try_lock``
  (re-entrant same-or-exclusive-holder, sole-holder S→X upgrade, grant on
  compatible holders otherwise ``Busy``); +3 unit tests. FIFO ``acquire`` and
  DFS deadlock detection byte-for-byte unchanged.
* ``crates/qmind-kernel/src/mvcc.rs``: ``LockManager`` field; fallible ``set``;
  new ``lock_write``; commit success releases all locks; commit WAL-sink
  failure auto-reaps (``finish_abort`` + ``release_all``); conflict path keeps
  locks until caller aborts; ``abort`` releases all locks.
* ``crates/qmind-sql/src/engine.rs``: explicit INSERT = ``lock_write`` all rows
  → ``set`` all rows (re-entrant) → extend buffered; autocommit INSERT =
  fallible ``set`` loop, ``db.abort(txn)`` on failure.
* Tests updated across ``mvcc_visibility.rs`` (16 unwraps), ``concurrency_semantics.rs``
  (test 2 rewritten: 3 kernel writers — loser ``Busy``-aborts, then wins the
  released row but loses FCW at commit, fresh retry commits),
  ``correctness.rs`` (property test: ``Busy`` set ⇒ deterministic immediate
  abort, no inflight push), ``kernel_integration.rs`` (3), ``read_stress.rs`` (3),
  ``transactions.rs`` (+2).

Tests added
-----------

* ``crates/qmind-kernel/tests/lock_2pl.rs`` (**new suite, 9**): exclusive lock
  held set→commit and released on abort; no queue residue after ``Busy`` +
  same-txn retry; first-committer-wins still rejects a stale-snapshot write
  after the lock is released; WAL-sink failure reaps transaction + releases
  locks and the store stays usable; ``lock_write`` reserve-all is
  all-or-nothing, re-entrant, and released at the boundary; distinct keys never
  conflict; repeated same-row writes in one transaction; a snapshot reader
  proceeds lock-free while a writer holds the row.
* ``crates/qmind-kernel/src/lock.rs`` (**+3**): ``try_lock`` grants/frees and
  rejects without queueing; shared coexistence + re-entry; sole-holder upgrade.
* ``crates/qmind-sql/tests/transactions.rs`` (**+2, now 18**): multi-row INSERT
  reserves all rows and commits with a secondary index materialized; multi-row
  statement failure (NOT NULL mid-list) leaves no partial rows and the
  transaction continues cleanly.

Final test count
----------------

.. list-table::
   :header-rows: 1

   * - Gate
     - Baseline
     - This phase
   * - Test suites
     - 22
     - 23 (new ``lock_2pl`` suite)
   * - Tests passed
     - 253
     - 267 (14 added, 0 failures)

Exact per-suite counts (captured from the run):

* qmind-kernel: 90 (lib) + 4 (concurrency_semantics) + 6 (correctness)
  + 3 (kernel_integration) + 9 (lock_2pl, new) + 9 (mvcc_visibility)
  + 6 (read_stress) = 127
* qmind-sql: 50 (lib) + 4 (concurrency) + 9 (crash_recovery) + 4 (parser_fuzz)
  + 12 (persistence) + 1 (soak) + 29 (sql_e2e) + 18 (transactions, +2) = 127
* qmind-server: 11 (wire_e2e); qmind-embed: 2 (embed_api)

Total accounted: 127 + 127 + 11 + 2 = 267

Documentation
-------------

* ``docs/concepts/locking.rst`` (**new**): the strict-2PL model, lock manager
  API, integration boundary, lock lifecycle, conflict semantics, MVCC / index
  interplay, failure cleanup, and design assumptions.
* ``docs/concepts/concurrency.rst``: conflict surfaces now three (row locks +
  FCW + single-writer gate); unsupported-semantics list now excludes lock
  wiring and blocking waits explicitly; WAL-failure cleanup note updated.
* ``docs/concepts/transactions.rst`` / ``docs/concepts/mvcc.rst``:
  cross-references and the row-lock lifecycle added to the error/concurrency
  chapters.
* Toctrees updated (``docs/concepts/index.rst``, ``docs/architecture/index.rst``).

Known limitations
-----------------

* One explicit transaction at a time at the SQL layer (single-writer); the
  kernel store hosts several live writers where the locks and conflicts are
  proven, but there is no SQL multi-writer throughput claim.
* No serializable isolation; SI + per-row write locks is the delivered
  behavior (write skew not prevented).
* No SQL ``UPDATE``/``DELETE`` yet, so the only locked keys are INSERT rows;
  DDL is autocommit-only and takes no persistent locks.
* Recovery replays the WAL and takes no locks (locks are a runtime
  construction rebuilt per transaction).
* The blocking ``acquire`` path and deadlock detection stay an unused API
  capability in the runtime path by design.
* Aggregate-over-JOIN projections remain unsupported (pre-existing roadmap
  limitation).

Validation results
------------------

* Complete test suite: **23 suites, 267 tests, 0 failures** (exact per-suite
  counts captured from the actual run; qmind-kernel 127, qmind-sql 127,
  qmind-server 11).
* ``cargo fmt --all -- --check``: PASS
* ``cargo clippy --workspace --all-targets --all-features -- -D warnings``: PASS
* Sphinx ``-W -b html``: PASS, 0 warnings

Revision
--------

* Branch: ``develop`` (no merge to ``main``, no push, no tag)
* Baseline (parent) commit: ``71b770a``
* Phase commit: this commit — see ``git log -1 --oneline`` on ``develop``
* Working tree: clean after the phase commit

Next R4 phase
-------------

R4-DEADLOCK — decide whether to exercise the lock manager's blocking FIFO
``acquire`` + DFS deadlock rejection in the runtime, or keep the deterministic
no-wait row-lock policy; also candidate: SQL multi-writer sessions with
row-lock-based conflict delivery at the SQL surface.