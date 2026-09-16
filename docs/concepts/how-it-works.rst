How it works
============

This page describes the engine from the outside in: where a SQL statement goes
once it is submitted, and how the result works its way back out.

System overview
---------------

.. code-block:: text

   SQL
    ↓
   Parser
    ↓
   Planner / execution selection
    ↓
   Execution operators
    ↓
   Batch execution
    ↓
   StorageManager
    ↓
   Buffer / page layer
    ↓
   FilePageStore
    ↓
   Persistent storage

A second, orthogonal path describes how data becomes durable and how it is
reconstructed after a restart:

.. code-block:: text

   Transaction / WAL
    ↓
   Durability
    ↓
   Recovery
    ↓
   Page-store reconstruction

Request path
------------

**Parser** (``qmind-sql``)
    The handwritten SQL parser turns a statement string into a typed AST
    (``Statement`` / ``Select`` sub-types). The supported SQL subset includes
    ``CREATE TABLE``, ``INSERT``, ``SELECT`` with expressions and predicates,
    ``ORDER BY`` / ``LIMIT``, ``GROUP BY`` + aggregates, hash ``INNER JOIN``,
    ``CREATE/DROP INDEX``, and ``SHOW TABLES``.

**Execution selection**
    Once parsed, the engine chooses one of two execution paths:

    * the **Volcano executor** operating over the in-memory MVCC row store
      (the ``execute`` entry point), or
    * the **batch/streaming path** operating over persistent page storage
      (the ``stream_query`` entry point).

    The streaming path exists so a query over a large persistent table does not
    require the whole result be resident in memory at once.

**Volcano executor (``execute``)**
    A pull-based, row-at-a-time iterator pipeline: ``VecScan/Scan → Filter →
    Sort → Scan(project) → Limit``, with ``HashAggregate`` and ``HashJoin``
    operators for ``GROUP BY`` and ``INNER JOIN`` respectively. Every operator
    implements ``next() -> Option<Row>``.

**Batch execution (``stream_query``)**
    A column-oriented pipeline over persistent storage. Rows in page chains are
    decoded straight into a ``Batch`` (``N`` rows × ``M`` columns stored
    column-wise), filtered with a selection vector, projected, and emitted in
    bounded batches of ``DEFAULT_BATCH_SIZE`` (2048) rows. Details are in
    :doc:`/developer-guide/batch-execution`.

**StorageManager**
    The kernel-level coordinator for persistent tables. It owns the buffer
    pool, the table catalog, the global page-id allocator, and the table-id
    allocator, and it documents the WAL-before-page-flush ordering contract.
    See :doc:`/developer-guide/storage-manager`.

**Buffer / page layer**
    A clock-sweep buffer pool caches 8 KiB pages in RAM. Pages are loaded
    through the ``PageStore`` trait only when first pinned and are CRC-validated
    on every load. Dirty frames are written back to the store either on
    eviction or on an explicit flush.

**FilePageStore**
    The file-backed ``PageStore``: segment files of 256 × 8 KiB page slots
    under the ``tables/`` directory. Pages are addressed by a single global
    64-bit ``PageId`` (page 0 is reserved).

**Persistent storage**
    The ``tables/`` segment files hold the page data. Together with ``db.meta``
    (versioned metadata) and ``wal/wal.log`` (the write-ahead log) they form
    the on-disk database root.

Durability and recovery path
----------------------------

* **Transaction / WAL** — writes go through a write-ahead log. DDL is
  autocommit; data writes are collected into a commit group and fsynced as a
  unit by ``commit_group``.
* **Durability** — a commit is durable once the WAL group has been flushed and
  synced. Page updates are only applied to the buffer pool *after* that
  durability point (write-ahead ordering).
* **Recovery** — on ``open_db`` the WAL clean prefix is replayed: the catalog
  (DDL) is rebuilt, committed MVCC state is re-derived, and a torn tail is
  truncated. Any other corruption fails the open loudly.
* **Page-store reconstruction** — because the WAL is the recovery authority,
  and the page store is a derived structure, ``open_db`` currently rebuilds the
  page store from the recovered state. This is intentional and transitional;
  see :doc:`/developer-guide/storage-manager` and
  :doc:`/architecture/wal-storage-ordering`.

What is implemented vs. future work
-----------------------------------

Implemented:

* Persistent single-writer page storage with global page-id allocation.
* WAL-first insert ordering and flush-on-close.
* Streaming batch scans with WHERE / projection / LIMIT, an ORDER BY
  materialized fallback, and aggregate / GROUP BY / INNER-JOIN query shapes
  (see :doc:`/developer-guide/batch-execution`).
* CRC-protected pages, versioned headers, and loud failure on corruption.

Future work (documented honestly, not claimed as done):

* The full checkpointed storage lifecycle where page storage is authoritative
  rather than derived from the WAL.
* Streaming and spill-to-disk execution for aggregate and join operators
  (currently both materialize their inputs in RAM).
* Multi-writer / concurrent transaction support (R4).