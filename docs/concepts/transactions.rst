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

Single-writer constraint
------------------------

The engine serializes all writers behind a single exclusive lock, and the
engine holds exactly **one** explicit transaction at a time. At the server
layer, each connection is a session:

* the session that issued ``BEGIN`` becomes the transaction **owner**; every
  statement it sends — including SELECT — executes through the transaction-
  aware write path so it observes its own uncommitted writes;
* a **foreign session** (one that does not own the transaction) may still read:
  its SELECTs run on committed-only snapshots and never observe the owner's
  uncommitted state;
* foreign **writes are rejected while a transaction is open** with a
  deterministic ``single-writer constraint`` error, delivered before any
  execution, so the rejected session never touches engine state and can simply
  retry once the owner finishes;
* a rejected writer's transaction state remains valid: it may read immediately,
  and take over the writer slot after the owner commits, rolls back, or
  disconnects.

Writer ownership is released — deterministically — by:

* ``COMMIT``,
* ``ROLLBACK``,
* a failed ``COMMIT`` / ``ROLLBACK`` (the engine has already taken or discarded
  the active transaction),
* **session termination**: a connection that closes while owning an open
  transaction (Terminate packet or TCP disconnect) has it rolled back
  automatically, so a later session can always acquire the writer slot.

A failed *statement* inside a transaction does **not** release ownership: the
transaction continues and the owner keeps it until ``COMMIT`` / ``ROLLBACK``.
Ownership never becomes permanently stuck.

Readers are not blocked: the read snapshot path runs lock-free on committed
data. See :doc:`concurrency` for the full concurrency model, the conflict
surfaces, and the explicitly unsupported semantics.

Columnar (HTAP) interplay
-------------------------

Autocommit and foreign SELECTs prefer the columnar read path when columnar
segments exist for a table. Inside an explicit transaction, SELECT reads the
transaction-aware MVCC row store instead, so the transaction's own buffered
rows are visible alongside all committed rows (the row store retains committed
rows even after columnar flushes).

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
* Write-write conflicts fail with ``first-committer-wins`` semantics
  (``transaction aborted: write-write conflict ...``) and the losing
  transaction is rolled back cleanly.
* After a crash, recovery rebuilds the committed world from the WAL; committed
  transactions stay visible, and in-flight / rolled-back / aborted state stays
  absent.