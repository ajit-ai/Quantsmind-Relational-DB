Deadlock detection
==================

QuantsMind's kernel ``LockManager`` implements blocking (queueing) lock
acquisition behind its deterministic no-wait ``try_lock`` path. This chapter
documents the blocking mechanism exactly as validated in R4-DEADLOCK: wait
queues, the waits-for graph, DFS cycle detection, the victim policy, cleanup,
and the strict boundary between the kernel mechanism and the SQL runtime —
which deliberately never uses it.

The one-sentence model:

    **A conflicting ``acquire`` request queues FIFO and waits for the holder;
    every edge addition runs a DFS cycle check; a cycle fails the requesting
    transaction immediately (no-wait victim = the requester); the runtime
    never uses `acquire` — it uses the deterministic `try_lock` no-wait path.**

Blocking acquisition vs the no-wait path
----------------------------------------

The lock manager exposes two acquisition APIs (see :doc:`locking` for the
S/X model):

* ``LockManager::try_lock`` — the SQL/MVCC write path. Incompatible requests
  fail immediately with ``LockError::Busy``; nothing is queued, nothing waits,
  a lock cycle is impossible by construction.
* ``LockManager::acquire`` — the blocking scheme. An incompatible request is
  queued and depends on the current holders; the grant happens when the
  blocker releases. `acquire` is a *cooperative* API: it returns ``Ok(())``
  once the request is granted or safely enqueued — it does not block the
  calling thread, and the caller re-checks ``holds`` or proceeds after the
  queue grant. Every scenario is therefore reproducible deterministically
  in a single thread.

Wait queues
-----------

A resource's lock entry keeps:

* ``holders`` — the transactions currently granted (with their modes), and
* a FIFO ``queue`` of ``(txn, mode)`` requests waiting to be granted.

Requests join the queue strictly in call order. On release the manager
promotes the queue head repeatedly while it is grantable (exclusive if no
other holder remains; shared if all remaining holders are shared), granting
compatible runs group-wise.

Waits-for graph
---------------

An enqueued waiter records a dependency edge toward every current holder of
the resource it is waiting for (self-edges for an S→X upgrade are excluded):

.. code-block:: text

   T2 queued on a resource held by T1   ⇒   Edge T2 → T1

Edges are added only at enqueue time, and a granted waiter loses the edges
that belonged to the resource it was granted; a waiter that still blocks on
other resources keeps those remaining dependencies so later cycle searches
stay sound.

Cycle detection
---------------

Every new edge runs a depth-first search from the requester through the
waits-for graph. If the requester is reachable from itself, a cycle exists and
the request is rejected:

.. code-block:: text

   T1 owns a, T2 owns b
   T1 requests b        ⇒  queues, Edge T1 → T2
   T2 requests a        ⇒  Edge T2 → T1; DFS finds T2→T1→T2   ⇒  DEADLOCK

The reported ``LockError::Deadlock { cycle }`` is closed
(first == last == requester) and names every transaction on the cycle. The
failing request is removed from its queue immediately; the rest of the
requester's state is untouched.

Victim policy
-------------

The current, documented policy is **no-wait victim = the requester**:

* the transaction whose request closes the cycle receives ``Deadlock`` and its
  request is rolled back (removed from the queue and from the graph);
* the requester keeps every lock it already holds (strict 2PL — nothing is
  silently released);
* **a ``Deadlock`` error does not auto-abort**: the caller decides. The victim
  resolves by calling ``release_all`` (abort / rollback), which then releases
  all of its locks and drains its queued requests;
* the other cycle members keep their locks and queued requests and proceed
  once the victim aborts — they are never erroneously victimized.

This is the smallest deterministic policy. Older-victim, youngest-victim, and
explicit-abort schemes are not implemented; the requester is an unambiguous,
reproducible choice.

First-committer-wins and first-claim
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

``acquire`` enforces *first-claim* order: the first transaction to queue for a
resource is granted before later waiters or later no-wait requests. This is
independent of the MVCC first-committer-wins validation at commit, which
decides *visibility* under shared snapshots — deadlock handling decides who
may hold the lock, MVCC decides when a commit is publishable.

Cleanup
-------

Terminating a transaction removes it from the lock table completely:

* ``release`` drops the transaction's granted hold **and** its own
  queued-but-ungranted requests on that resource, so an aborted waiter can
  never be promoted later (which would leak the lock) and never blocks future
  grant decisions;
* ``release_all`` (commit/abort/disconnect) releases every resource the
  transaction touches and sweeps the transaction out of **every other**
  waiter's dependency set — no stale wait edge survives a termination, so a
  later cycle search can never route through a dead transaction;
* the manager exposes ``is_idle()`` — the fully drained endpoint after all
  transactions have committed or aborted.

Non-deadlock waiting
--------------------

A plain wait chain is never misclassified as a deadlock. A linear dependency
chain (T1 holds a; T2 waits on a and holds d; T3 waits on d) is acyclic, so DFS
finds no cycle, all acquires return without error, and releasing in order
progresses every waiter — proven for same-resource FIFO queues and
cross-resource chains alike.

Deadlock resolution replay
--------------------------

After a cycle is detected and the victim aborts:

* the survivor keeps its locks and is granted the contested resource once the
  victim's release runs (no leaked lock remains);
* the manager is immediately reusable: fresh transactions acquire, release,
  and run a second deadlock cycle on the same manager with identical
  behavior — no stale internal wait-for state survives.

MVCC interaction
----------------

Deadlock handling operates purely on the lock table; it shares no state with
snapshot visibility:

* a deadlock cycle does not advance the commit watermark (``read_ts``);
* pinned snapshots are immutable before, during, and after deadlock activity;
* committed versions remain governed by MVCC; first-committer-wins is
  untouched;
* rolling back the deadlocked transaction removes its uncommitted state and
  leaves early snapshots stable;
* the surviving transaction retains its original snapshot.

No Serializable Isolation is introduced: locking serializes *holdership*;
MVCC still decides *visibility*.

SQL runtime boundary — no blocking in the SQL path
--------------------------------------------------

The SQL/MVCC write path is **no-wait only**:

* write conflicts use the deterministic ``try_lock`` path and surface
  ``LockError::Busy`` immediately;
* the SQL runtime never calls ``acquire``, never queues, and therefore cannot
  form a lock wait cycle at the SQL layer;
* the kernel's blocking ``acquire``, wait queues, waits-for graph, and
  deadlock detection are validated independently in this phase and left
  unused by the runtime.

Consequently **no SQL deadlock handling is claimed**: there is nothing to
handle at the SQL surface while it is single-writer (one explicit transaction
at a time). The kernel-level mechanism is proven and available for a future
multi-writer phase.

Unsupported semantics
---------------------

* Blocking SQL writers: not enabled (SQL stays single-writer, no-wait).
* Auto-restart of a deadlocked transaction: a ``Deadlock`` error is returned;
  the caller (future multi-writer runtime) decides to abort and retry. No
  hidden restart exists.
* Multiple victims of one cycle: exactly one (the requester) is reported and
  rolled back.
* Timeouts, priorities, or lock starvation prevention beyond FIFO order.
* Serializable isolation.

Future multi-writer integration (assessed, not implemented)
------------------------------------------------------------

Assessment of ``acquire`` for a future SQL multi-writer phase:

* what is already safe: FIFO waiters, deterministic requester-victim
  semantics, complete termination cleanup (queued requests purged, incoming
  edges swept), cycle evidence in the error, `is_idle` drain check, lock
  manager reuse after resolution;
* what remains: a real scheduler that blocks a worker thread until its queued
  request is granted (the current API is cooperative), integration between
  `Deadlock`-error delivery and the single-writer session boundary /
  ownership lifecycle, and SQL-level "abort victim + retry" semantics on top
  of the kernel no-wait/blocking paths;
* recommendation: keep the SQL write path on ``try_lock`` until multi-writer
  sessions and the blocking scheduler land together; the deadlock machinery is
  real, deterministic, and ready to be called by that scheduler.

Test coverage
-------------

* ``crates/qmind-kernel/tests/deadlock.rs`` (new, 12 tests): two-transaction
  cycle detection + resolution; symmetric cycle completed by the other side;
  three-transaction cycle (all external observations: victim's lock kept then
  released, survivor granted the contested resource); ordinary wait chains
  (same-resource FIFO and cross-resource, never misdetected); waiter
  cancellation by abort (no stale residue, fresh txn wins); deadlock-victim
  termination leaves no stale waiter; no-wait shared grants not poisoned by
  cancelled waiters; manager reuse after resolution (two full cycles on one
  manager); churn (cancellation + deadlock cycles) leaving a clean graph;
  snapshot/commit-watermark stability across deadlock activity; deadlocked-
  writer rollback removing uncommitted state and preserving SI.
* ``crates/qmind-kernel/src/lock.rs`` unit tests (+3): aborted waiter purged
  from the queue (no leak on later release); terminated txn swept from other
  waiters' dependency sets (live edges preserved); multi-resource waiter
  keeps its other dependencies when promoted on one resource.