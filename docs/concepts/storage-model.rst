Storage model
=============

This page explains what is persistent, what lives in memory, and how the pieces
interact. The page format itself is described in
:doc:`/developer-guide/page-format`.

Database root layout
--------------------

A database is a directory with a fixed, versioned skeleton
(``crates/qmind-sql/src/dbdir.rs``):

.. code-block:: text

   <root>/
     db.meta            versioned metadata (magic + 4 format versions)
     wal/wal.log        the write-ahead log (durable record of everything)
     catalog/           reserved (durable catalog files land here later)
     tables/            R3 page storage: segmented FilePageStore
     indexes/           reserved (persistent index structures)
     columnar/          reserved (persistent columnar segments)
     checkpoints/       reserved (checkpoint state)

``db.meta`` is 28 bytes: ``[QMDBMETA][meta u32][db u32][format u32][wal u32]
[catalog u32]``. Opening a database validates every version number against the
building binary; an unsupported version fails loudly instead of risking silent
corruption.

What is persistent
------------------

* ``db.meta`` — durable, synced at creation time.
* ``wal/wal.log`` — the write-ahead log. This is the **recovery authority**:
  committed state is whatever committed records the log contains.
* ``tables/seg_*.bin`` — page segment files written by ``FilePageStore``.
  Pages become durable when dirty frames are written back and the store is
  synced (on eviction or on ``close``).

What is in memory
-----------------

* The MVCC row store in ``Engine`` (``qmind-sql``) — a copy of committed rows,
  recovered from the WAL on open, used by the Volcano executor.
* The buffer pool frames — up to capacity (256 pages in the file-backed
  engine) 8 KiB pages cached in RAM; writes are write-back.
* The storage manager's table catalog and the page/table id counters.
* Secondary index trees, rebuilt from committed rows on open.

StorageManager and its responsibilities
---------------------------------------

``StorageManager<S: PageStore>`` (``crates/qmind-kernel/src/storage_manager.rs``)
owns:

* the ``BufferPool<S>``,
* the table catalog (``HashMap<String, TableMeta>``),
* a monotonic ``next_table_id`` (assigned from 1),
* a global ``next_page_id`` counter (assigned from 1; page 0 is reserved).

The global page counter guarantees page ids are unique across all tables, so a
table never aliases another table's pages. The SQL engine creates the manager,
registers tables, and appends committed rows; the manager never owns the WAL
writer.

Table storage model
-------------------

Each table is represented by ``TableMeta``: a stable table id, the table name,
the first and last page of an append-only row-chain, and a running row count.
Rows are packed into 8 KiB pages (see :doc:`/developer-guide/page-format`) as
length-prefixed byte records produced by ``encode_row`` (the row codec).

The buffer pool sits between the page chains and the file store:

* ``create_page`` allocates a fresh zeroed page (header sealed by the pool).
* ``pin`` loads a page from the store if it is not resident, validating the
  CRC on every load.
* ``payload_mut`` gives exclusive access to the payload region only, keeping
  the header checksum-consistent.
* ``flush_all`` seals and writes back all dirty frames, then calls the store's
  durability barrier.

WAL ordering (the write-ahead rule)
-----------------------------------

Persistent page data must **never** become durable before the WAL record for
the same commit:

.. code-block:: text

   WAL write
      ↓
   commit_group (flush + sync_data)
      ↓
   page-store update (buffer pool append; dirty frames)
      ↓
   (eventually) dirty page flush to FilePageStore

The engine's ``insert`` path collects the encoded rows first, commits them
through the WAL, and only then appends them to persistent storage. ``close``
flushes any pending WAL group before flushing dirty storage pages. Because
pages can therefore never lead the log, recovery never depends on the page
store being clean. See :doc:`/architecture/wal-storage-ordering`.

What happens during restart
---------------------------

``open_db`` (``crates/qmind-sql/src/engine.rs``):

1. validates ``db.meta`` version numbers;
2. replays the WAL clean prefix (recovering durable catalog + committed rows);
3. truncates a torn tail left by a crash mid-commit-group;
4. rebuilds the in-memory MVCC store, row-id counters, and secondary indexes;
5. **rebuilds the page store**: the existing ``tables/`` directory is wiped and
   re-created, tables are re-registered, and rows from the recovered state are
   re-inserted so a page scan is never missing a committed row.

Step 5 is why the current R3 page store is *derived from* the recovered WAL
state rather than an independent source of truth. It is correct for the current
single-writer design, but it is not the final billion-row architecture. This is
documented as transitional in :doc:`/architecture/wal-storage-ordering` and the
architecture decision records (ADR-016).

Fragment of what to expect: entities vs responsibilities
--------------------------------------------------------

.. list-table:: What handles what
   :header-rows: 1

   * - Concern
     - Owner
     - Lifecycle
   * - Row bytes
     - Row codec (``encode_row``/``decode_row``)
     - Created per insert, decoded per scan
   * - Row chains
     - ``TableStore`` helper
     - One chain per table, append-only
   * - Page allocation
     - ``BufferPool`` via global ``next_page_id``
     - Monotonic across the database lifetime
   * - Page cache
     - ``BufferPool`` (clock sweep, write-back)
     - Resides in RAM, loaded lazily
   * - Page files
     - ``FilePageStore``
     - Durable in ``tables/`` segments
   * - Durability order
     - WAL first, page flush second
     - Enforced by the engine
   * - Recovery authority
     - WAL
     - Replayed on every open