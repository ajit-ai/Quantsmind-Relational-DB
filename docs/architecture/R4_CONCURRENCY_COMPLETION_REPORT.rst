R4-CONCURRENCY Completion Report
================================

Objective
---------

Implement and validate the next R4 phase: **controlled concurrent transaction
execution and conflict semantics**, built exactly on the completed R4-MVCC
baseline without redesigning its snapshot-isolation semantics. Concretely:
prove concurrent reads, snapshot stability across a concurrent commit,
deterministic writer conflicts under the existing single-writer constraint, a
complete writer-ownership lifecycle, failure cleanup, index/aggregate/JOIN
visibility under concurrency, and the server/session concurrency boundary —
then document the model precisely without overstating it.

Starting baseline
-----------------

* 21 test suites, 243 tests, 0 failures
* ``fmt`` PASS, ``clippy -D warnings`` PASS, Sphinx (``-W``) PASS, 0 warnings
* Baseline commit on ``develop``: ``c1fd850`` (R4 baseline — durable storage,
  transactions, snapshot isolation & MVCC visibility)

Concurrency model
-----------------

    **Snapshot Isolation with controlled / single-writer write serialization.**

* The kernel ``MvccStore`` natively hosts several coexisting transactions; each
  pins an immutable ``read_ts`` watermark at ``BEGIN``, reads its own buffered
  writes plus committed versions ``commit_ts <= read_ts``, and never sees later
  commits.
* The SQL engine adds a deliberate serialization boundary: exactly one explicit
  transaction at a time, enforced deterministically at the server's session
  layer. Writers serialize per statement on the engine's exclusive write guard;
  readers capture snapshots lock-free on the shared guard.
* No 2PL, no lock-free transactions, no serializable, no multi-writer MVCC at
  the SQL layer. The kernel's strict-2PL ``LockManager`` exists but is **not**
  wired into the SQL runtime in this phase.

Writer ownership model
----------------------

* ``BEGIN`` on a session makes it the transaction **owner**; all of its
  statements (including SELECT) run through the transaction-aware write path.
* A foreign session's ``BEGIN`` while a transaction is open is rejected with a
  deterministic error; the owner slot is never double-booked.
* Ownership is released deterministically by ``COMMIT``, by ``ROLLBACK``, by a
  failed ``COMMIT``/``ROLLBACK``, and by **session termination** (a connection
  that closes while owning an open transaction has it rolled back
  automatically). A failed *statement* inside a transaction does not release
  ownership (documented SQL semantic: statement failure ≠ rollback).

Reader semantics
----------------

* Readers never block each other or the writer; a snapshot is an immutable
  watermark and scans are lock-free after capture.
* An open reader keeps its point-in-time across other commits; a fresh
  transaction after a commit sees the newly committed rows. Proven
  deterministically with two live kernel transactions and a handshaken
  reader/writer at the session layer.

Conflict semantics
------------------

* **Kernel (first-committer-wins):** a second writer over a key already
  committed since its snapshot gets a deterministic ``Conflict``, publishes
  nothing, aborts cleanly; a fresh writer can immediately take the key over.
* **SQL/session (single-writer gate):** foreign writes while a transaction is
  open are rejected *before any execution* with the deterministic
  ``single-writer constraint`` error; the rejected session holds valid state,
  can read immediately, and retries successfully once ownership is released.
* Rejection was chosen over blocking queues to avoid deadlock-prone waits; no
  wait/blocking behavior was introduced.

Cleanup semantics
-----------------

* Aborted/failed transactions leave no buffered MVCC rows, no stale versions,
  no index entries, no leaked pages; a WAL-sink failure publishes nothing and
  the store stays usable; a failed statement inside a transaction buffers
  nothing (validation precedes state mutation).
* Ownership never becomes permanently stuck — including after a session
  abruptly closes mid-transaction (fixed in this phase).

Index behavior
--------------

Index trees are materialized only at commit. A foreign reader never finds an
uncommitted row through an index; after a writer commits, later readers use the
freshly materialized index; open transactions keep snapshot semantics (indexed
equality inside an explicit transaction uses the txn-aware scan fallback).

Server/session behavior
-----------------------

Each connection runs on its own thread against one ``Arc<RwLock<Engine>>``.
Concurrent sessions read in parallel; writes serialize on the exclusive guard;
the single transaction slot is owned by at most one session, bookkept
per-connection. The server does not pretend to execute conflicting
transactions in parallel — that boundary is documented rather than hidden.

Tests added
-----------

* ``crates/qmind-kernel/tests/concurrency_semantics.rs`` (**new suite, 4**):
  two concurrent readers stay stable across a writer's commit + fresh reader
  sees it; conflicting writer aborts and a fresh tx retries cleanly; abort after
  WAL-sink failure leaves nothing and the store stays usable; snapshot scans
  (the COUNT/GROUP BY/JOIN building blocks) stay stable across concurrent
  commits.
* ``crates/qmind-sql/tests/transactions.rs`` (**+2, now 16**):
  ownership released after COMMIT/ROLLBACK/failure with subsequent reuse;
  failed statement inside a transaction leaves no partial state.
* ``crates/qmind-server/tests/wire_e2e.rs`` (**+4, now 11**):
  disconnected owner releases writer ownership (fix proof); failed statement
  holds ownership until ROLLBACK then releases it; ownership flows between
  sessions only via terminal statements (concurrent BEGIN conflict); a
  handshaken reader never observes an open explicit transaction across scan,
  index, GROUP BY and JOIN, then sees everything after COMMIT.

Final test count
----------------

.. list-table::
   :header-rows: 1

   * - Gate
     - Baseline
     - This phase
   * - Test suites
     - 21
     - 22 (new ``concurrency_semantics`` suite)
   * - Tests passed
     - 243
     - 253 (10 added, 0 failures)

Known limitations
-----------------

* One explicit transaction at a time at the SQL layer (single-writer); no
  multi-writer MVCC, no concurrent-writer throughput claims.
* Kernels first-committer-wins conflict path is proven at the kernel level; at
  the SQL layer the session gate prevents overlapping writers before they can
  conflict.
* No strict 2PL / lock-table concurrency control is wired into the SQL runtime
  (the kernel ``LockManager`` remains a separate component for a later phase).
* No READ COMMITTED leak into explicit transactions; foreign/autocommit reads
  use a committed snapshot per statement by design.
* Aggregate-over-JOIN projections remain unsupported (pre-existing roadmap
  limitation); JOIN concurrency is proven with plain-column projections.

Validation results
------------------

* Complete test suite: **22 suites, 253 tests, 0 failures** (exact per-suite
  counts captured from the actual run).
* ``cargo fmt --all -- --check``: PASS
* ``cargo clippy --workspace --all-targets --all-features -- -D warnings``: PASS
* Sphinx ``-W -b html``: PASS, 0 warnings

Revision
--------

* Branch: ``develop`` (no merge to ``main``, no push, no tag)
* Baseline (parent) commit: ``c1fd850``
* Phase commit: this commit — see ``git log -1 --oneline`` on ``develop``
* Working tree: clean after the phase commit (pre-existing changes carried in
  the baseline commit, preserved)

Next R4 phase
-------------

R4-LOCK / R4-DEADLOCK — decide whether to wire the kernel strict-2PL
``LockManager`` into the SQL runtime, or continue without blocking waiters.