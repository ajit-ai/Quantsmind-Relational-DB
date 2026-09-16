Concurrency
===========

This chapter describes QuantsMind's transaction concurrency model and the
bounds of what it does — and does not — provide.

The model in one sentence:

    **Snapshot Isolation with controlled / single-writer write serialization.**

The kernel's ``MvccStore`` natively hosts several concurrent transactions
(readers and writers) with snapshot isolation. The SQL engine adds a deliberate,
documented serialization boundary on top: **exactly one explicit transaction at
a time**, enforced deterministically at the server's session layer.

Snapshot lifetime
-----------------

A snapshot is established at ``BEGIN``:

* it pins the commit watermark *read at transaction start*;
* it never advances for the lifetime of the transaction;
* every read inside the transaction — scan, index equality, aggregate,
  GROUP BY, JOIN — pairs it with the transaction's own buffered writes.

A pure reader that never calls ``BEGIN`` takes a fresh snapshot per statement
(committed data only), which is how autocommit reads and foreign-session reads
behave. See :doc:`mvcc` for the full visibility contract.

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

Writer serialization and ownership
----------------------------------

The engine holds one explicit transaction at a time. At the server, every
connection is a session and ownership is deterministic:

1. the session whose ``BEGIN`` succeeds becomes the **owner**; every statement
   it sends — including ``SELECT`` — executes through the transaction-aware
   write path, so it sees its own uncommitted writes;
2. a **foreign session** that tries ``BEGIN`` while a transaction is open gets a
   deterministic error and does *not* acquire a transaction;
3. a **foreign write** while a transaction is open is rejected with a
   deterministic error:

   .. code-block:: text

      another explicit transaction is in progress on this database
      (single-writer constraint); commit or roll it back first

   The rejected writer never touches the engine: rejection happens before
   execution, so there is no partial storage mutation and no corrupted
   transaction state.
4. foreign reads (``SELECT`` / ``SHOW TABLES``) always work, on committed-only
   snapshots.

The boundary is architectural: writers serialize on the engine's exclusive
write guard per statement, and the engine holds at most one explicit
transaction at a time. Within a session, every row key written is additionally
protected by a strict-2PL **exclusive row lock** (see :doc:`locking`); at the
SQL layer the single-writer gate means those locks never collide, so row-lock
conflicts cannot surface through normal SQL use. The kernel hosts several live
writers at once, and there both the row locks and the first-committer-wins
validation do real work — both are proven deterministically at that layer.

Conflict behavior
-----------------

Three conflict surfaces exist, at different layers:

* **Kernel (strict-2PL row locks).** ``MvccStore::set`` takes an exclusive
  lock on the row key it writes (non-blocking, deterministic). A second live
  kernel transaction that targets the same row while it is held gets an
  immediate ``LockError::Busy`` and buffers nothing; the loser aborts and a
  fresh transaction retries. Locks are held until commit/abort — see
  :doc:`locking`.
* **Kernel (first-committer-wins).** A writer whose snapshot predates a
  concurrent commit on one of its keys loses at ``COMMIT`` with a
  deterministic ``Conflict`` and publishes nothing. This still applies once
  the lock is released: a stale-snapshot write that lands after the holder
  commits is rejected at commit time.
* **SQL session (single-writer gate).** A foreign writer is rejected up front,
  before it can affect any engine state.

The kernel conflict paths are proven deterministically (``concurrency_semantics``
and ``lock_2pl`` kernel tests); the SQL path is proven over real TCP sessions.

Writer ownership lifecycle
--------------------------

Ownership is released — deterministically — at every terminal point:

* **COMMIT** — rows are published and the transaction slot frees,
* **ROLLBACK** — buffered state is discarded and the slot frees,
* **failed COMMIT / ROLLBACK** — the engine has already taken (finished or
  discarded) the active transaction, so ownership frees even on error,
* **session termination** — a connection that closes (``Terminate`` packet or
  TCP disconnect) while owning an open transaction has it **rolled back
  automatically**, so a later session can always acquire the writer slot.

A failed *statement* (for example an arity error or a NOT NULL violation) does
**not** end the transaction — the transaction continues and the owner keeps
ownership until ``COMMIT`` or ``ROLLBACK``. This is the documented SQL semantic:
statement failure is not conflated with transaction rollback. After any release
the database is immediately usable by the next writer.

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
* the engine's single explicit transaction slot is owned by at most one session
  at a time, and the ownership flags are bookkept per connection.

Note: the server serializes operations through the engine lock — it does not
pretend to execute conflicting transactions in parallel. The determinism in the
tests comes from handshakes and barriers, never from sleeps; the correctness
proofs (two live transactions, snapshot stability, conflicts) live at the kernel
level where the interleavings are exact.

Explicitly unsupported semantics
--------------------------------

The current implementation does **not** provide:

* **multi-writer SQL sessions** (one writer at a time by design; the kernel
  store — where row locks and write-write conflicts live — already hosts
  several live writers, proven at the kernel layer);
* **serializable isolation** (SI is the isolation level; write skew is neither
  prevented nor claimed to be);
* **READ COMMITTED for explicit transactions** (a pinned SI snapshot never
  advances mid-transaction);
* **blocking write waits** — the SQL/MVCC write path always uses the
  deterministic no-wait row-lock path (``LockManager::try_lock``). The lock
  manager's blocking FIFO-queue ``acquire`` with deadlock detection remains an
  available API capability but is deliberately *not* used by the runtime,
  which chooses immediate rejection over waiting.

Progress on any of these is a separate R4 phase, not a property of this one.