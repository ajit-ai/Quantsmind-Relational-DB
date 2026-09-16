MVCC visibility
================

QuantsMind provides **Snapshot Isolation (SI)** at the kernel and SQL layer.
This chapter is the normative visibility contract: what a transaction can and
cannot observe, when its snapshot is established, and how each rule is
verified.

.. note::

   QuantsMind implements **Snapshot Isolation**, not Serializable. Everything
   below limits itself to SI guarantees. The single-writer constraint on the
   engine (see :doc:`transactions`) means multiple *simultaneous* writer
   sessions are not yet supported; the visibility rules here are still proven
   against live concurrent transactions at the kernel layer.

Isolation level
---------------

The engine and kernel provide **Snapshot Isolation**:

* every transaction observes a **stable snapshot** — a point-in-time view of
  the committed database — established when the transaction begins;
* subsequent commits from other transactions do **not** become visible to an
  existing snapshot;
* writes are validated by **first-committer-wins**: two overlapping
  transactions that both write the same key cannot both commit.

Read Committed (fresh snapshot per statement) is deliberately **not** used by
explicit transactions. Non-transactional SELECTs (autocommit and foreign
server sessions) capture a per-statement snapshot, which is their natural
transaction boundary.

Snapshot representation
-----------------------

A snapshot is an immutable, copyable value:

.. code-block:: rust

   pub struct Snapshot { pub read_ts: u64 }

``read_ts`` is the commit watermark captured when the transaction starts. A
row version committed with ``commit_ts <= read_ts`` is visible; a version
committed later is not. The snapshot is established once, at ``BEGIN``, and is
pinned for the lifetime of the transaction — it never advances.

Visibility rules
----------------

Rule 1 — Own writes are visible
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

A transaction observes its own uncommitted writes on every read path — full
scan, secondary-index equality lookup, aggregate, GROUP BY, and JOIN::

   BEGIN;
   INSERT INTO users VALUES (1, 'Alice');
   SELECT * FROM users;              -- Alice is visible to T

The engine reads with its transaction id (``MvccStore::read(Some(txn), ...)``)
so buffered (uncommitted) rows are merged with the committed snapshot.

Rule 2 — Foreign uncommitted writes are invisible
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

Another transaction's buffered writes never appear, even to a transaction that
started before they were written. At the kernel level, a second live
transaction (``MvccStore``) reads only its own pending version of a key, then
falls through to committed versions; another writer's pending state is never
consulted.

Rule 3 — Committed-before-snapshot rows are visible
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

Anything committed at or before the transaction's snapshot watermark is
visible::

   INSERT INTO t VALUES (1);  COMMIT;
   BEGIN;
   SELECT * FROM t;           -- sees the committed row

Rule 4 — Post-snapshot commits are invisible
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

The most important SI property: a commit that lands after ``BEGIN`` is not
exposed to the transaction's existing snapshot. This is the property that
distinguishes Snapshot Isolation from READ COMMITTED, and it is proven with
two live kernel transactions in the same store (see
``crates/qmind-kernel/tests/mvcc_visibility.rs``):

* ``T1`` snapshots the world ``{k=1}``;
* ``T2`` commits ``{k=2}`` afterwards;
* ``T1`` still reads ``{k=1}``.

Rule 5 — Repeated reads within a transaction are stable
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

Because the snapshot is pinned at ``BEGIN`` and never advances, any number of
SELECTs inside the same transaction observe the same committed world. Own
buffered writes grow the visible set monotonically; committed changes from
other transactions never appear mid-transaction.

Rule 6 — Own writes override snapshot visibility
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

A row written by the transaction itself is visible even though it was not part
of the BEGIN-time snapshot — read-your-own-writes.

Rule 7 — Rolled-back writes disappear
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

After ``ROLLBACK`` the buffered rows, their secondary-index entries (which are
materialized only at commit), columnar deltas, page-store state, and MVCC
versions all disappear. No structure leaks a rolled-back row. Rolled-back
transactions leave row-id gaps (their ids are not reused), which the index
backfill accounts for when mapping lookups back to row keys.

Rule 8 — Aborted writes never become visible
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

A transaction that writes and aborts contributes no committed version. Before
rollback only the owner sees the write; after rollback nobody does — both
immediately and after a reopen.

Index visibility
----------------

Secondary-index trees are materialized **at commit** (deferred
materialization): an index entry exists only for a committed row. Consequently:

* autocommit and foreign reads keep the index fast path — the tree references
  committed rows only;
* inside an explicit transaction, an ``col = literal`` SELECT falls back to the
  transaction-aware table scan, so the transaction's own uncommitted rows
  remain reachable through the equality predicate while the committed set is
  still indexed;
* an aborted insert never becomes discoverable through an index.

Execution paths
---------------

The transaction context (transaction id + pinned snapshot) is threaded through
the entire read pipeline — scan, filter, projection, aggregate, GROUP BY, and
JOIN — so every path enforces the same visibility rules. There is exactly one
MVCC implementation; operators consume the txn-aware scan rather than
implementing their own versioning.

Recovery interaction
--------------------

Recovery replays only committed transactions (deferred commit logging means an
in-flight transaction left a crash has no durable Begin/Put records; a rollback
writes an ``Abort`` record for audit). After ``open_db``:

* committed rows are visible to new transactions;
* in-flight, rolled-back, and aborted rows are absent.

Recovered MVCC state therefore preserves the same visibility contract.

Current limitation
------------------

Concurrent foreign writers remain restricted by the existing **single-writer
constraint** and are addressed in later R4 concurrency work. Within the SQL
layer, a transaction that must be observed concurrently by a second session can
only be observed by *readers* (a foreign session SELECTs the committed world
and never the owner's uncommitted state), and foreign writes are rejected while
a transaction is open. The SI rules that involve two overlapping writers are
verified directly against the kernel store, where multiple writers already
coexist.

Verification
------------

* `crates/qmind-kernel/tests/mvcc_visibility.rs` — Rules 1–8 with two live
  transactions in one store, plus recovery-consistent visibility after
  ``redo_from_records``.
* `crates/qmind-sql/tests/transactions.rs` — SQL-layer visibility: own-write
  scans/aggregates/GROUP BY/JOIN/index lookups, mixed committed + own +
  rolled-back sets, read-only transactions, reopen/rollback-gap index
  alignment.
* `crates/qmind-server/tests/wire_e2e.rs` — session-level visibility: a foreign
  session never observes the owner's uncommitted rows (scan and index reads),
  and the owner's snapshot view is stable while the transaction is held.