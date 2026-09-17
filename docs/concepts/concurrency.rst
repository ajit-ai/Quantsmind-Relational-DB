Concurrency
===========

This chapter describes QuantsMind's transaction concurrency model and the
bounds of what it does — and does not — provide.

The model in one sentence:

    **Snapshot Isolation with one explicit write transaction per session.**

The kernel's ``MvccStore`` natively hosts several concurrent transactions
(readers and writers) with snapshot isolation. The SQL layer exposes that
directly: **every session may hold its own explicit transaction, concurrently
with every other session** (R4-MULTIWRITER). Statement execution is still
serialized through the engine's write guard, but transactions are fully
independent — each has its own snapshot, pending writes and row locks, and any
subset may roll back or commit.

Snapshot lifetime
-----------------

A snapshot is established at ``BEGIN``:

* it pins the commit watermark *read at transaction start*;
* it never advances for the lifetime of the transaction;
* every read inside the transaction — scan, index equality, aggregate,
  GROUP BY, JOIN — pairs it with the transaction's own buffered writes.

A pure reader that never calls ``BEGIN`` takes a fresh snapshot per statement
(committed data only), which is how autocommit reads and sessions outside a
transaction behave. See :doc:`mvcc` for the full visibility contract.

Reader concurrency
------------------

Readers never block each other or the writer:

* a snapshot is an immutable ``u64`` watermark — capturing it is lock-free;
* after capture, scans run against committed versions without touching the
  writer's critical section;
* multiple live transactions can hold their own snapshots concurrently while a
  third transaction commits; each keeps seeing exactly its own point-in-time.

A reader never observes:

* another transaction's uncommitted writes,
* a partially committed state (``COMMIT`` publishes the whole record group, then
  every version atomically),
* a partially materialized index (index trees are rebuilt at commit, after the
  WAL durability point).

Writer transactions per session
-------------------------------

Each session holds at most **one** explicit transaction, and any number of
sessions may hold one at the same time. At the server, every connection is a
distinct session (a unique id):

1. a session whose ``BEGIN`` succeeds owns its own transaction; every
   statement it sends — including ``SELECT`` — executes through the
   transaction-aware path, so it sees its own uncommitted writes;
2. the same session cannot ``BEGIN`` twice (deterministic
   ``a transaction is already in progress`` error);
3. a **different session** may ``BEGIN`` at any time, even while another
   session's transaction is open, and runs its own independent transaction;
4. sessions outside a transaction always read on committed-only snapshots —
   they never observe any session's uncommitted writes.

Statements serialize on the engine's exclusive write guard per statement, but
the transactions themselves are independent: two sessions can write, roll back
and commit concurrently. Every row key written is additionally protected by a
strict-2PL **exclusive row lock** (see :doc:`locking`). Physical row ids are
append-only and allocated from a shared per-table counter at statement time,
so two sessions' ``INSERT`` statements always target **disjoint** rows: multi-writer
writes never collide on a key through the current dialect, and the kernel's
row locks and first-committer-wins validation — both proven deterministically
at that layer — are the defense if a future surface (UPDATE, DELETE, or a
UNIQUE/PK constraint) ever targets an existing key.

Conflict behavior
-----------------

Three conflict surfaces exist, at different layers:

* **Kernel (strict-2PL row locks).** ``MvccStore::set`` / ``lock_write`` takes
  an exclusive lock on the row key it writes (non-blocking, deterministic). A
  second live transaction that targets the same row while it is held gets an
  immediate ``LockError::Busy`` and buffers nothing; the loser aborts cleanly.
  Locks are held until commit/abort — see :doc:`locking`. The conflict is
  *row-local*: writers to disjoint rows of the same table never conflict.
* **Kernel (first-committer-wins).** A writer whose snapshot predates a
  concurrent commit on one of its keys loses at ``COMMIT`` with a
  deterministic ``Conflict`` and publishes nothing. This still applies once
  the lock is released: a stale-snapshot write that lands after the holder
  commits is rejected at commit time.
* **SQL mapping.** ``LockError::Busy`` surfaces as
  ``statement failed: row locked by another transaction`` and a ``Conflict``
  as ``transaction aborted: write-write conflict ... (first-committer-wins)``.
  With the current append-only, INSERT-only dialect two sessions always write
  disjoint physical rows, so both paths are structurally unreachable through
  SQL today; the kernel tests (``concurrency_semantics``, ``lock_2pl``) prove
  them exactly.

The kernel conflict paths are proven deterministically (``concurrency_semantics``,
``lock_2pl``, and ``deadlock`` kernel tests); multi-writer isolation, snapshot
stability and durability over real TCP sessions are proven in
``wire_e2e`` / ``multiwriter``. The kernel's blocking wait queues and DFS
deadlock detection are a validated but runtime-unused capability — see
:doc:`deadlocks`.

Writer ownership lifecycle
--------------------------

A session's transaction is released — deterministically — at every terminal
point:

* **COMMIT** — rows are published and the session's transaction slot frees,
* **ROLLBACK** — buffered state is discarded and the slot frees,
* **failed COMMIT / ROLLBACK** — the engine has already taken (finished or
  discarded) the session's transaction, so the slot frees even on error,
* **session termination** — a connection that closes (``Terminate`` packet or
  TCP disconnect) while holding an open transaction has it **rolled back
  automatically**, releasing its row locks; every other session's transaction
  is unaffected.

A failed *statement* (for example an arity error or a NOT NULL violation) does
**not** end the transaction — the transaction continues within that session,
and other sessions are unaffected either way. This is the documented SQL
semantic: statement failure is not conflated with transaction rollback. After
any release the session is immediately usable for a new transaction.

Failure cleanup
---------------

An aborted or failed transaction leaves nothing behind:

* buffered MVCC writes and any engine-side materialization buffers are dropped;
* no stale MVCC versions or secondary-index entries survive an abort;
* row-id gaps left by aborted transactions do not misalign index backfill
  (backfill reads real row keys);
* a WAL-sink failure during ``COMMIT`` surfaces as an error and publishes
  nothing; the transaction id, its row locks, and its pending writes are all
  released immediately (no caller cleanup needed), and the store stays usable
  for the next transaction.

Index visibility under concurrency
----------------------------------

Index trees are materialized only at commit:

* a foreign reader never finds an uncommitted row through an index;
* after a writer commits, the next reader can use the freshly materialized
  index;
* an open reader keeps snapshot semantics — its indexed lookups (which fall
  back to a transaction-aware scan inside an explicit transaction) never merge
  in post-snapshot rows.

Aggregates, GROUP BY and JOIN under concurrency
-----------------------------------------------

All of these execute through the same snapshot-aware scan path, so concurrency
cannot bypass the visibility rules:

* ``COUNT`` stays snapshot-stable over an open transaction;
* ``GROUP BY`` never groups another transaction's uncommitted rows;
* ``JOIN`` never exposes another transaction's buffered rows, on either side.

Server boundary
---------------

Each connection runs on its own OS thread against one
``Arc<RwLock<Engine>>``:

* concurrent sessions can read in parallel under the shared read guard;
* writes (autocommit or transaction-owner statements) take the exclusive write
  guard per statement;
* each session's transaction is tracked independently (one per session), and
  the per-connection flags are bookkept alongside the engine's authoritative
  session map.

Note: the server serializes *statement execution* through the engine lock — it
does not pretend to execute conflicting statements in parallel. Transactions
themselves are concurrent: two open transactions can both buffer writes and
commit independently. The determinism in the tests comes from handshakes and
barriers, never from sleeps; the correctness proofs (two live transactions,
snapshot stability, conflicts) live at the kernel level where the interleavings
are exact.

Explicitly unsupported semantics
--------------------------------

The current implementation does **not** provide:

* **serializable isolation** (SI is the isolation level; write skew is neither
  prevented nor claimed to be);
* **READ COMMITTED for explicit transactions** (a pinned SI snapshot never
  advances mid-transaction);
* **cross-session row-key write conflicts through SQL** — because the dialect
  is append-only INSERT (no UPDATE/DELETE, no UNIQUE/PK), two sessions always
  target disjoint physical rows; the no-wait conflict machinery is real and
  proven, but reachable only at the kernel layer until such a surface exists;
* **blocking write waits** — the SQL/MVCC write path always uses the
  deterministic no-wait row-lock path (``LockManager::try_lock``). The lock
  manager's blocking FIFO-queue ``acquire`` with deadlock detection is a
  validated kernel capability (see :doc:`deadlocks`) but is deliberately *not*
  used by the runtime, which chooses immediate rejection over waiting — so no
  SQL deadlock handling exists or is claimed.

Progress on any of these is a separate R4 phase, not a property of this one.