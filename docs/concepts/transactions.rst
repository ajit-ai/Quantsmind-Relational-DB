Transactions
============

QuantsMind exposes explicit transactions through SQL transaction-control
statements. This chapter describes the transaction lifecycle, the durability
model, and the current concurrency constraints.

SQL surface
-----------

Supported statements (standard spellings, case-insensitive):

.. code-block:: sql

   BEGIN;
   COMMIT;
   ROLLBACK;
   BEGIN TRANSACTION;   BEGIN WORK;
   COMMIT TRANSACTION; COMMIT WORK;
   ROLLBACK TRANSACTION; ROLLBACK WORK;

The parser accepts the optional ``TRANSACTION`` / ``WORK`` suffix on every
statement. A ``BEGIN`` while a transaction is already open, and a
``COMMIT`` / ``ROLLBACK`` with no transaction in progress, are rejected with
clear errors.

Lifecycle
---------

.. code-block:: text

   BEGIN  ──►  snapshot established (pinned for the transaction)
      │
      │   INSERT / DML buffered as MVCC writes (not yet durable)
      │   SELECT reads: own buffered writes + committed snapshot
      │
      ├──► COMMIT   ──►  write the [Begin, Put…, Commit] WAL group,
      │                   sync it to disk, THEN publish rows to
      │                   indexes / columnar deltas / page store
      │
      └──► ROLLBACK ──►  drop buffered state, append an Abort record
                         (audit marker), discard everything

Snapshot and visibility
-----------------------

The snapshot (and transaction id) are established at ``BEGIN`` and pinned for
the transaction's lifetime. Every read inside the transaction — full scan,
index equality, aggregate, GROUP BY, JOIN — observes:

* the transaction's own buffered writes, and
* committed rows at or before the snapshot watermark,

and never another transaction's uncommitted state, nor commits that land after
``BEGIN``. See :doc:`mvcc` for the full visibility contract.

Durability (deferred commit logging)
------------------------------------

An explicit transaction writes **nothing to the WAL at ``BEGIN``**. At
``COMMIT`` the engine emits the whole ``[Begin, Put…, Commit]`` record group as
one write batch and fsyncs it before publishing any row to the page store,
indexes, or columnar deltas. This makes ``COMMIT`` a single durability point:

* a transaction that is in flight (no ``COMMIT``) at a crash leaves **no WAL
  footprint** — recovery has nothing to undo;
* ``ROLLBACK`` appends a best-effort ``Abort`` record for an auditable
  boundary.

Materialization order
---------------------

Autocommit statements and explicit-transaction commits apply buffered rows to
the secondary indexes, columnar deltas, and persistent page store **only after
the WAL group is synced** (write-ahead ordering). This guarantees that a
rollback cannot leak phantom index or columnar state, and recovery rebuilds a
consistent committed world from the WAL authority.

DDL inside a transaction
------------------------

DDL (``CREATE TABLE``, ``CREATE INDEX``, ``DROP INDEX``) is rejected while an
explicit transaction is open:

.. code-block:: text

   DDL is not supported inside an explicit transaction; commit or roll back first

DDL remains autocommit-only by design.

One transaction per session
---------------------------

The engine allows **one open explicit transaction per session**, and any
number of sessions may hold one at the same time. At the server layer, each
connection is a distinct session:

* a session that issued ``BEGIN`` owns its own transaction; every statement
  it sends — including SELECT — executes through the transaction-aware write
  path so it observes its own uncommitted writes;
* that same session cannot ``BEGIN`` again until its transaction ends
  (deterministic ``a transaction is already in progress`` error);
* a **different session** may ``BEGIN`` at any time — even while another
  session's transaction is open — and runs an independent transaction with
  its own snapshot, pending writes and row locks;
* sessions outside a transaction read on committed-only snapshots: their
  SELECTs never observe any session's uncommitted state, and their writes run
  in autocommit.

A session's transaction is released — deterministically — by:

* ``COMMIT``,
* ``ROLLBACK``,
* a failed ``COMMIT`` / ``ROLLBACK`` (the engine has already taken or discarded
  the session's transaction),
* **session termination**: a connection that closes while holding an open
  transaction (Terminate packet or TCP disconnect) has it rolled back
  automatically, so its row locks are released and other sessions are never
  blocked.

A failed *statement* inside a transaction does **not** release the transaction:
it continues within that session, and other sessions' transactions are
unaffected either way. A session's transaction state never becomes permanently
stuck.

Readers are not blocked: the read snapshot path runs lock-free on committed
data. See :doc:`concurrency` for the full concurrency model, the conflict
surfaces, and the explicitly unsupported semantics.

Columnar (HTAP) interplay
-------------------------

Autocommit SELECTs and reads by sessions outside a transaction prefer the
columnar read path when columnar segments exist for a table. Inside an explicit
transaction, SELECT reads the transaction-aware MVCC row store instead, so the
transaction's own buffered rows are visible alongside all committed rows (the
row store retains committed rows even after columnar flushes).

Row-id gaps
-----------

Rolled-back transactions consume row ids (allocated at statement time) but
never publish them, leaving gaps in the row-id sequence. Gaps are harmless:
scans probe each id and skip absent rows, and index backfill maps each committed
row to its real row key rather than a compressed position, so later
lookups stay aligned.

Errors and recovery
-------------------

* Commit failures (WAL errors) abort the transaction before any row is
  published.
* Every row a transaction writes is exclusively locked from the first write
  until commit/abort (strict 2PL, see :doc:`locking`). In the current INSERT-only
  dialect two sessions write disjoint physical rows, so row locks effectively
  never collide between SQL sessions; at the kernel layer a live lock conflict
  rejects the requester immediately (``LockError::Busy``), and the losing
  transaction rolls back cleanly.
* Write-write conflicts fail with ``first-committer-wins`` semantics
  (``transaction aborted: write-write conflict ...``) and the losing
  transaction is rolled back cleanly.
* Deadlock detection is a **kernel** property of the lock manager's blocking
  ``acquire`` path (no-wait victim = the requester). Because the SQL runtime
  uses only the no-wait ``try_lock`` path — and multi-writer statements still
  serialize statement-by-statement through the engine write guard — a lock
  cycle can never form at the SQL layer: there is no SQL deadlock to handle.
  See :doc:`deadlocks`.
* After a crash, recovery rebuilds the committed world from the WAL; committed
  transactions stay visible, and in-flight / rolled-back / aborted state stays
  absent.