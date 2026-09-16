WAL-to-page-store ordering
==========================

The single most important invariant of the R3 storage integration is:

    **Page data must never become visible as committed durable state before the
    WAL durability point for the same commit.**

If pages were flushed and synced ahead of their WAL records, a crash could
leave page state on disk that the log says never committed — the engine would
then reconstruct a state that was never real.

The ordering contract
---------------------

.. code-block:: text

   WAL write
      ↓
   commit_group → flush → sync_data
      ↓
   page-store update (buffer pool append; page becomes dirty)
      ↓
   dirty page flush → FilePageStore → sync

Insert path (write-ahead order)
-------------------------------

In ``Engine::insert``:

1. rows are evaluated and validated;
2. the transaction is committed: WAL records are appended and the group is
   flushed and fsynced (``commit_group``);
3. only after that succeeds are the committed rows appended to persistent
   storage via ``StorageManager::insert_row``.

Because dirty frames may be written back at *any* time after step 3 (eviction,
flush), the WAL-first ordering is what keeps recovery correct.

Close path
----------

``Engine::close``:

1. ``wal.commit_group()`` — flush and sync any pending WAL group first;
2. ``storage.flush()`` — write back all dirty pages and sync the file store.

Shutdown never flushes pages ahead of their log records. Crash safety never
depends on ``close`` having run — the WAL alone can reconstruct committed state.

Open / recovery path
--------------------

``Engine::open_db``:

1. validate ``db.meta`` versions;
2. replay the WAL clean prefix (catalog + committed MVCC redo);
3. truncate a torn tail (crash inside a commit group) or fail loudly on any
   other corruption;
4. rebuild in-memory row-id counters and secondary indexes;
5. **rebuild the page store**: because the WAL is the authority and the page
   store is derived, the stale ``tables/`` directory is removed and re-created
   with the recovered tables and their committed rows.

Why page-store reconstruction on open is correct
------------------------------------------------

* The WAL always contains every committed record; it is the lowest common
  denominator of durable truth.
* The page store is a derived, denormalized scan-optimized copy. Rebuilding it
  from recovered state guarantees consistency between what SQL sees (the
  MVCC store from the WAL) and what a page scan returns.
* The cost is proportional to the recovered dataset and is paid at open time —
  acceptable for the current design and explicitly transitional (ADR-016).

The transitional nature
-----------------------

The current page store is **not** an independent durable authority. It is
rebuilt from the WAL on every open, and stale page files from a previous
session are discarded. The full checkpointed storage lifecycle — where pages
are authoritative and only the delta since the last checkpoint is replayed
from the WAL — is future work and is not claimed by R3.