R4-DEADLOCK Completion Report
=============================

Objective
---------

Validate and prove the kernel's *blocking* lock path — the FIFO wait queues,
waits-for graph, and DFS deadlock detection — that R4-LOCK left defined but
unused by the SQL/MVCC runtime. Explicitly **not** converting the SQL runtime
to blocking multi-writer transactions: the phase establishes whether the
existing deadlock mechanism is correct and deterministic, fixes any defect it
exposes, proves the cleanup/cancellation semantics, and documents the exact
boundary between the kernel mechanism and the no-wait SQL path. Concretely:

* inspect the manager and answer precisely: when a txn enters the wait graph,
  when it leaves, how cycles are found, who is victimized, how waiting locks
  are released, whether a deadlock error auto-aborts, whether waiters are
  FIFO, and whether stale edges survive termination;
* deterministic (sleep-free) tests for two-txn and three-txn cycles, ordinary
  wait chains (never misclassified), waiter cancellation, victim cleanup, lock
  release after deadlock, lock-manager reuse, and MVCC snapshot stability
  across deadlock activity;
* fix any cleanup defect the tests expose;
* document blocking acquisition, wait queues, the wait-for graph, detection,
  victim policy, cleanup, the SQL no-wait boundary, and the readiness
  assessment for a future multi-writer phase.

Starting baseline
-----------------

* 23 test suites, 267 tests, 0 failures
* ``fmt`` PASS, ``clippy -D warnings`` PASS, Sphinx (``-W``) PASS, 0 warnings
* Baseline commit on ``develop``: ``3bd709e`` (feat: integrate strict two-phase
  locking)

Inspection findings
-------------------

The blocking API is *cooperative*, not thread-blocking: ``acquire`` returns
``Ok(())`` either when the request grants immediately or once it is safely
enqueued — the grant arrives when the blocker releases, so every scenario is
reproducible deterministically in a single thread (no sleeps, no channels).

1. **Waiting entry**: an incompatible request joins the resource's FIFO queue
   and records waits-for edges to every current holder (self-edges excluded).
2. **Leaving the graph**: on grant (edges for the granted resource removed), on
   deadlock (request rolled back), and on termination (``release_all``).
3. **Cycle detection**: a DFS from the requester runs on every edge addition;
   a closed path returns `Deadlock { cycle }` (first == last == requester).
4. **Victim**: the requester that closes the cycle (no-wait victim = requester);
   its request is rolled back; its other locks are untouched until its own
   commit/abort.
5. **Waiting locks on error**: `Deadlock` releases only the failed request;
   held locks stay held (strict 2PL) and drain via the victim's
   ``release_all``.
6. **Auto-abort**: `Deadlock` **does not** auto-abort the transaction — the
   caller resolves it.
7. **Waiters**: FIFO per resource; promotion grants compatible runs.
8. **Stale edges**: step 1 answered a bug — termination previously left a
   txn's *queued-but-ungranted* requests in the queue and left incoming
   dependency edges on terminated transactions. Fixed below.

Defect found and fixed
----------------------

*Queued-request residue on termination.* ``release``/``release_all`` earlier
removed only a txn's *granted* holds, not its queued-but-ungranted requests.
An aborted waiter could therefore stay queued and then be promoted after the
holder released — granting a lock to a transaction that is gone (a permanent
lock leak that would strand later waiters forever and pollute grant decisions
for the no-wait path). ``release_all`` also left incoming waits-for edges
pointing at terminated txns.

Fixed in ``crates/qmind-kernel/src/lock.rs``:

* ``release`` now purges the released txn's own queued requests on the
  resource before promotion, so a cancelled waiter can never be granted;
* promotion removes exactly the granted resource's blockers from a waiter's
  edge set (a multi-resource waiter keeps its other dependencies, keeping
  cycle detection sound);
* ``release_all`` sweeps the terminated txn out of every other waiter's
  dependency set — no stale wait edge survives a termination;
* new public ``is_idle()`` — the fully drained strict-2PL endpoint.

Victim policy (documented, not invented)
----------------------------------------

**No-wait victim = the requester** — the transaction whose request closes the
cycle receives `Deadlock` and its request is rolled back; it keeps the locks
it already holds until it aborts via ``release_all``; other cycle members are
never victimized and proceed once the victim aborts. Smallest deterministic
policy; unambiguous and reproducible. ``Deadlock`` never leaves the system
ambiguous: the failed request is gone, the resolution is explicit.

Deadlock model
--------------

    **Conflicting `acquire` requests queue FIFO and depend on the current
    holders; new edges run a DFS cycle check; a cycle fails the requester
    (victim) deterministically; termination completely removes a transaction
    — granted holds, queued requests, and everywhere it appears as a
    dependency.**

Locking serializes *holdership* only: deadlock activity does not advance the
commit watermark, does not touch pinned snapshots, and does not alter MVCC
visibility or first-committer-wins. No Serializable Isolation is introduced.

SQL runtime boundary
--------------------

* The SQL/MVCC write path is **no-wait only** (``try_lock``, immediate
  ``Busy``); the SQL runtime never calls ``acquire`` and cannot form a lock
  wait cycle at the SQL layer.
* Single-writer SQL means there is nothing to deadlock at the SQL surface; no
  SQL deadlock handling is claimed.
* This phase validates the kernel mechanism independently and leaves it unused
  by the runtime.

Tests added
-----------

* ``crates/qmind-kernel/tests/deadlock.rs`` (**new suite, 12**): two-transaction
  cycle detected and resolved; symmetric cycle victimizing the other requester;
  three-transaction cycle (external observations only: victim keeps then
  releases its lock, survivor granted the contested resource, no leaked lock);
  ordinary wait chain (same-resource FIFO and cross-resource, never
  misclassified); waiter cancellation by abort leaves no stale residue and a
  fresh txn wins; deadlock-victim termination leaves no stale waiter; no-wait
  shared grants not poisoned by cancelled waiters; lock-manager reuse (two full
  cycles on one manager); repeated cancellation + deadlock churn drains clean;
  snapshot/commit-watermark stability across deadlock activity; deadlocked-
  writer rollback removes uncommitted state and preserves SI for early readers.
* ``crates/qmind-kernel/src/lock.rs`` unit tests (**+3, now 13**):
  aborted-waiter queue purge (no grant leak on later release); terminated txn
  swept from other waiters' dependency sets (live edges preserved); promoted
  multi-resource waiter keeps its other-waiter dependencies.

Final test count
----------------

.. list-table::
   :header-rows: 1

   * - Gate
     - Baseline
     - This phase
   * - Test suites
     - 23
     - 24 (new ``deadlock`` suite)
   * - Tests passed
     - 267
     - 282 (15 added: +3 kernel lib, +12 deadlock suite, 0 failures)

Exact per-suite counts (captured from the run):

* qmind-kernel: 93 (lib, +3) + 4 (concurrency_semantics) + 6 (correctness)
  + 12 (deadlock, new) + 3 (kernel_integration) + 9 (lock_2pl)
  + 9 (mvcc_visibility) + 6 (read_stress) = 142
* qmind-sql: 127 (unchanged: 50 lib + 4 + 9 + 4 + 12 + 1 + 29 + 18)
* qmind-server: 11 (wire_e2e); qmind-embed: 2 (embed_api)

Total accounted: 142 + 127 + 11 + 2 = 282

Documentation
-------------

* ``docs/concepts/deadlocks.rst`` (**new**): blocking acquisition, wait
  queues, waits-for graph, DFS cycle detection, victim policy, cleanup,
  non-deadlock waiting, replay after resolution, MVCC independence, the SQL
  no-wait boundary, unsupported semantics, and the multi-writer readiness
  assessment.
* ``docs/concepts/locking.rst``: blocking-vs-no-wait distinction sharpened,
  conflict-semantics bullet now cross-references the validated deadlock
  mechanism; test-coverage updated with the new suite.
* ``docs/concepts/concurrency.rst``: conflict-surface paragraph and
  unsupported-semantics list now state that the blocking path is validated
  but runtime-unused, so no SQL deadlock handling exists or is claimed.
* ``docs/concepts/transactions.rst``: deadlock is documented as a kernel-only
  property unreachable at the SQL layer.
* Toctrees updated (``docs/concepts/index.rst``, ``docs/architecture/index.rst``).

Compatibility
-------------

All prior semantics preserved and re-proven: 267 baseline tests still pass;
MVCC snapshot isolation, first-committer-wins, strict-2PL no-wait SQL
locking, the single-writer constraint, WAL-failure auto-reap, disconnect
rollback, index at-commit materialization, and recovery are unchanged. No
Serializable Isolation, no automatic SQL blocking, no SQL multi-writer, no
implicit retry/restart introduced.

Known limitations
-----------------

* SQL remains single-writer and no-wait; the blocking path is kernel-only and
  clocked by the *caller* (cooperative `acquire` — no thread-blocking
  scheduler yet).
* A transaction that waits on several resources at once keeps correct edges
  only through the promotion fix; the runtime cannot create that shape today.
* No starvation/priority/timeout schemes beyond FIFO.
* Aggregate-over-JOIN projections remain unsupported (pre-existing roadmap
  limitation).

Validation results
------------------

* Complete test suite: **24 suites, 282 tests, 0 failures** (exact per-suite
  counts captured from the actual run).
* ``cargo fmt --all -- --check``: PASS
* ``cargo clippy --workspace --all-targets --all-features -- -D warnings``: PASS
* Sphinx ``-W -b html``: PASS, 0 warnings

Revision
--------

* Branch: ``develop`` (no merge to ``main``, no push, no tag)
* Baseline (parent) commit: ``3bd709e``
* Phase commit: this commit — see ``git log -1 --oneline`` on ``develop``
* Working tree: clean after the phase commit

Readiness assessment for multi-writer SQL
-----------------------------------------

* Already safe: deterministic requester-victim policy, complete termination
  cleanup (queued requests purged, incoming edges swept), cycle evidence in
  the error, FIFO waiters, reuse after resolution.
* Remains: a real scheduler to block a worker until its queued grant arrives,
  `Deadlock`-error plumbing through the session/ownership layer, and explicit
  SQL abort-and-retry semantics for the loser.
* Recommendation: keep the SQL write path on ``try_lock`` until multi-writer
  sessions and the blocking scheduler are implemented together; the deadlock
  machinery is real, deterministic, and ready to be called by that scheduler.

Next phase
----------

**R4-MULTIWRITER** — SQL multi-writer sessions with row-lock conflict delivery
(Busy now, queueing+deadlock later) and connection-owned transaction slots —
rather than another kernel-only correctness phase; the kernel deadlock story
is now complete and independently proven.