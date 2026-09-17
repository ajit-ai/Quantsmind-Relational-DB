StorageManager
==============

``StorageManager<S: PageStore>`` is the kernel-level coordinator for persistent
tables. It lives in ``crates/qmind-kernel/src/storage_manager.rs`` and is
parameterized over the :doc:`BufferPool/PageStore </developer-guide/page-format>`
backend. The file-backed engine uses ``StorageManager<FilePageStore>``.

Responsibilities
----------------

* owns the ``BufferPool<S>`` (page cache + durable store),
* owns the table catalog (``HashMap<String, TableMeta>``),
* owns the global allocators: ``next_table_id`` (from 1) and ``next_page_id``
  (from 1; page 0 is reserved),
* exposes insert / scan / metadata operations at row granularity,
* documents the WAL ordering contract (see below).

The WAL ordering contract
-------------------------

The storage manager does **not** own the WAL writer — that stays with the SQL
engine — but it documents the ordering that the engine must enforce:

    The caller **must** sync the WAL *before* calling ``flush``.

If dirty pages were ever written back and synced ahead of their WAL records, a
crash could leave page data visible that the log says never committed. The
combat is write-ahead ordering:

.. code-block:: text

   WAL write
      ↓
   commit_group (flush + sync_data)
      ↓
   page-store update
      ↓
   (later) dirty page flush

This contract is the subject of ADR-015 and is described in
:doc:`/architecture/wal-storage-ordering`.

Public operations
-----------------

``new(pool)``
    Wrap a buffer pool. Fresh manager: no tables, ``next_table_id = 1``,
    ``next_page_id = 1`` (page 0 reserved).

``create_table(name)``
    Registers a ``TableMeta`` with a fresh table id. Errors if the table
    already exists in storage.

``drop_table(name)``
    Removes a table's metadata (does not reclaim its pages yet).

``insert_row(table, row_bytes)``
    Appends one raw encoded row to the table's page chain via a ``TableStore``
    helper and saves the updated metadata.

``scan_rows(table) -> RowIter``
    Lazy pull-based cursor over the table's raw rows. At most one page of rows
    is buffered at a time. This is the primary scan interface used by the
    streaming query path.

``scan_all_rows / scan_batched``
    Push-based scans, also at row or batch granularity.

``row_count(table)``
    O(1) row count from table metadata.

``flush()``
    Writes back all dirty frames and syncs the store. Precondition: WAL synced.

``pool() / pool_mut() / table_meta()``
    Accessors for callers that need direct page or metadata access.

TableStore
----------

``TableStore<'a, S>` is a thin per-operation helper (owned pool borrow +
cloned ``TableMeta``) used while inserting or scanning. Relevant details:

* ``append_row`` appends to the last page of the chain, allocating and linking
  a fresh page when the current one is full. Page ids come from the storage
  manager's global counter, so ids stay unique across tables.
* ``scan_all`` / ``scan_batched`` walk the chain from first page to last.
* ``RowIter`` yields raw rows one at a time from page to page.

Engine integration points
-------------------------

The SQL engine wires the storage manager through ``Engine.storage``
(``Option<StorageManager<FilePageStore>>``, file-backed engines only):

``create_db``
    Creates the layout and metadata, opens ``tables/`` with a
    ``BufferPool::new(FilePageStore::open(...), 256)``.

``open_db``
    After WAL recovery, wipes the stale ``tables/`` directory (the WAL is the
    authority) and rebuilds the page store from the recovered state, registering
    every recovered table and inserting its committed rows.

``create_table``
    Appends the DDL to the WAL and syncs (autocommit), then registers the table
    in storage *after* the WAL durability point.

``insert``
    Evaluates rows, then: begin → set in MVCC → ``commit`` (appends + fsyncs a
    WAL group). Only after the successful commit are the committed rows
    appended to persistent storage (write-ahead order).

``close``
    ``commit_group`` (flush pending WAL group) then ``storage.flush()``.

``stream_query``
    Scans through ``StorageManager::scan_rows``, decodes raw rows into the
    :doc:`batch pipeline </developer-guide/batch-execution>`.

Notes and transitional behavior
-------------------------------

* **Stale pages are removed on open.** Because the WAL is the recovery
  authority, ``open_db`` deletes the ``tables/`` directory and re-creates it
  from recovered state. This avoids page-id / table-id collisions between a
  previous session and a fresh rebuild. It is correct today but is the
  transitional behavior documented in ADR-016.
* **WAL is the source of truth.** The page store is derived; a page scan can
  never be missing a committed row. The final checkpointed-storage lifecycle
  (where pages are authoritative and independently durable) is future work.
* **Single writer.** The storage manager and buffer pool assume a single
  writing thread. Concurrent writers are out of scope for R3 (R4 territory).