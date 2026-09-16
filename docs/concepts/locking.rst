Row-key locking (strict 2PL)
============================

QuantsMind integrates a strict two-phase locking protocol over the kernel
store's transactions. This chapter describes the lock manager, exactly how it
is wired into the runtime, and the precise conflict semantics — without
overstating what it does.

The one-sentence model:

    **Every row a transaction writes is exclusively locked from the moment it
    is first written until the transaction commits or aborts; a conflicting
    write is rejected immediately, never waited on.**

The lock manager
----------------

The kernel ships a strict-2PL lock manager (``qmind_kernel::lock``) holding
S/X locks keyed by arbitrary string resources::

   Resource(pub String)            // the lockable name (a row key)
   enum LockMode { Shared, Exclusive }
   enum LockError { Deadlock { cycle: Vec<u64> }, Busy }

Two acquisition paths exist on ``LockManager``:

* ``acquire`` — the classic scheme. Compatible requests grant immediately;
  incompatible ones join a FIFO wait queue and add waits-for edges. Every edge
  addition triggers a DFS cycle check; a cycle is rejected with
  ``Deadlock`` and the requester's queued request is rolled back (no-wait
  victim = the requester).
* ``try_lock`` — the deterministic no-wait path used by the runtime. Grants
  immediately when compatible with current holders, otherwise returns
  ``Busy`` without enqueueing and without touching the waits-for graph. A
  no-wait request can never wait, so it can never create a deadlock cycle.

Locks are released with ``release_all`` only at a transaction boundary —
strict 2PL: a transaction never releases a lock before commit/abort.

Integration boundary
--------------------

The lock manager lives inside ``MvccStore``, the kernel store that hosts
several coexisting transactions. That is the only layer where two live
writers can actually collide:

* ``MvccStore::set(txn, key, value)`` acquires the **exclusive** lock on the
  row key first, then buffers the write. It returns
  ``Result<(), LockError>``.
* ``MvccStore::lock_write(txn, key)`` reserves the exclusive lock without
  writing a value — the engine's tool for making a multi-row statement
  atomic at the lock level.
* Reads and snapshots take **no locks**: a snapshot is an immutable watermark
  and scans run lock-free, so readers never contend with writers.

The SQL layer hosts **one explicit transaction per session, with any number of
sessions concurrently** (see :doc:`transactions`). Statements serialize through
the engine's write guard, but the transactions — and their row locks — are
independent. In the current INSERT-only dialect each statement allocates fresh,
disjoint physical row ids from a shared counter, so two sessions' SQL writes
never collide on a key; row locks are the kernel's defense for the store's own
multi-writer API and are exercised and proven deterministically at the kernel
level, including row-local granularity (a writer to a held row is rejected
immediately while other rows of the same table stay writable).

Row keys map 1:1 to resources via ``String::from_utf8_lossy(key)``. Row keys
are UTF-8 (ASCII table name, ``\x01`` separator, decimal row id), so the
conversion is lossless; lock granularity is an individual row, meaning two
transactions writing disjoint rows never conflict.

Lock lifecycle
--------------

.. code-block:: text

   set / lock_write  ──►  exclusive lock acquired (no-wait, deterministic)
       │
       │   held for the whole transaction, including across statements
       │
       ├──► COMMIT  ──►  publish rows, THEN release every lock (strict 2PL)
       ├──► ROLLBACK ──► discard pending state and release every lock
       ├──► abort (explicit or on conflict) ──► release every lock
       └──► WAL-sink failure at COMMIT ──► txn reaped (id + locks) at once

Locks are released by:

* a successful ``COMMIT`` (after publication),
* ``ROLLBACK`` / abort (including the automatic rollback of a session that
  disconnects while owning a transaction),
* a WAL-sink failure at ``COMMIT`` — the store reaps the transaction id and
  releases its locks immediately, with no caller cleanup,
* a first-committer-wins conflict at commit, when the loser is aborted by its
  caller (until then the loser still holds its own locks — strict 2PL).

A failed *statement* does not release locks (statement failure ≠ rollback).
For a multi-row INSERT the engine reserves every row key up front and only
then writes, so a failed statement never leaves a partial written set; its
earlier reservations are released together at the next transaction boundary.

Conflict semantics
------------------

* A live row is held exclusively; a second writer targeting it gets
  ``LockError::Busy`` **immediately** and buffers nothing. No queueing, no
  waiting, no timeouts — the loser aborts and a fresh transaction retries.
* The kernel's first-committer-wins validation still runs at ``COMMIT``: a
  writer whose snapshot predates a concurrent commit on one of its keys loses
  even if the lock was already released (a stale-snapshot write). The two
  mechanisms are complementary — locks prevent concurrent holds, MVCC rejects
  stale overwrites.
* The runtime never uses the blocking ``acquire`` path; the blocking FIFO
  wait queues, waits-for graph, and DFS deadlock detection are validated and
  documented in :doc:`deadlocks`, and stay an unused API capability of the
  SQL/MVCC write path.

Blocking acquisition and deadlock detection
-------------------------------------------

``acquire`` — the classic queue-based path — coexists with ``try_lock`` but is
never called by the runtime. A blocking request that cannot grant immediately
queues FIFO, records waits-for edges, and runs a DFS cycle check per edge
addition; a cycle fails the requester with ``LockError::Deadlock { cycle }``
(no-wait victim = the requester), and full termination cleanup purges the
victim's queued requests and sweeps its incoming wait edges. The semantics,
victim policy, cleanup guarantees, MVCC independence, and the strict SQL
no-wait boundary are specified in :doc:`deadlocks`.

MVCC interaction
----------------

Locking does not change the SI visibility contract (see :doc:`mvcc`): readers
keep stable snapshots, own writes stay visible to their transaction, foreign
uncommitted writes stay invisible, and post-snapshot commits stay invisible.
The locks only serialize writes to the same row and keep the store's pending
writes mutually exclusive.

Index interaction
-----------------

Secondary-index trees are materialized at ``COMMIT`` exactly as before. Locking
adds nothing at the index level: locks are per row key and released before the
next transaction begins, so a fresh reader can always use a freshly built
index. Within an open transaction, indexed equality keeps the txn-aware scan
fallback, and its own rows are read through the transaction context.

Failure cleanup
---------------

* A WAL-sink failure at ``COMMIT``: nothing published, transaction id +
  row locks + pending writes released immediately.
* Autocommit statement failure: the implicit transaction is aborted, dropping
  its pending writes and locks — the statement leaves no trace.
* Explicit-transaction statement failure: no partial rows (validation runs
  before the reserve-all lock pass); the transaction stays open and keeps its
  locks until the caller commits or rolls back.

Test coverage
-------------

* ``crates/qmind-kernel/tests/lock_2pl.rs`` (new, 9 tests): lock held
  set→commit, release on abort, no queue residue + same-txn retry,
  first-committer-wins after lock release, WAL-failure auto-reap,
  reserve-all-then-write, distinct keys never conflict, same-row re-entrant
  writes, lock-free snapshot reads beside a locked writer.
* ``crates/qmind-kernel/tests/deadlock.rs`` (new, 12 tests): deadlock
  detection, wait chains, cancellation, and cleanup — see :doc:`deadlocks`.
* ``crates/qmind-kernel/src/lock.rs`` unit tests: ``try_lock`` conflict
  rejection without queueing, shared coexistence + re-entry, sole-holder
  upgrade (+3, R4-LOCK); aborted-waiter queue purge, terminated-txn wait-set
  sweep, multi-resource waiter edge retention (+3, R4-DEADLOCK).
* ``crates/qmind-kernel/tests/correctness.rs`` (updated property test): a
  randomized serial-history model now treats a ``Busy`` set as a deterministic
  immediate abort.
* ``crates/qmind-sql/tests/transactions.rs`` (+2): multi-row INSERT reserving
  all rows then committing with a secondary index; multi-row statement failure
  leaves no partial rows and the transaction stays usable.

Design assumptions
------------------

* No ``UPDATE``/``DELETE`` exists in the SQL surface yet; the only row keys
  locked are INSERT writes.
* DDL is autocommit-only (rejected inside explicit transactions) and takes no
  persistent locks.
* Recovery replays the WAL directly (``redo_from_records``) and takes no
  locks — locks are a runtime construction, rebuilt per transaction on demand.
* No serializable isolation is claimed; SI plus per-row write locks is the
  delivered behavior.