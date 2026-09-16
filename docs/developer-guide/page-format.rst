Page format
===========

This page documents the on-disk page format **as implemented** in
``crates/qmind-kernel/src/page.rs``, ``buffer.rs``, ``fs_store.rs`` and
``crates/qmind-sql/src/table_store.rs``. Nothing here is invented; the format is
still provisional and is labelled as such.

Page identity
-------------

* **Page size**: fixed at ``PAGE_SIZE = 8192`` bytes (8 KiB).
* **Page id**: ``PageId = u64``. Page id ``0`` is reserved and never
  allocated; real pages start at 1.
* **Global allocation**: page ids are handed out by the storage manager's
  single ``next_page_id`` counter, guaranteeing uniqueness across all tables
  in a database.

Page header (18 bytes)
----------------------

The header is serialized **little-endian** at *explicit* offsets (never
``repr(C)``, so alignment padding can never shift the checksum slot):

.. list-table:: PageHeader wire layout
   :header-rows: 1

   * - Field
     - Offset
     - Size
     - Type
   * - ``page_id``
     - 0
     - 8
     - ``u64`` little-endian
   * - ``format_version``
     - 8
     - 2
     - ``u16`` little-endian
   * - ``flags``
     - 10
     - 2
     - ``u16`` little-endian (reserved)
   * - ``used``
     - 12
     - 2
     - ``u16`` little-endian (occupied payload bytes)
   * - ``checksum``
     - 14
     - 4
     - ``u32`` little-endian CRC32
   * - **Total**
     - 0
     - 18
     - ``PageHeader::SIZE``

The header fields must never be reordered without bumping ``FORMAT_VERSION`` —
the on-disk format is a versioned contract (D-003).

Checksum
--------

* A **CRC32 (IEEE)** is computed over the entire 8 KiB page buffer — header
  fields *and* payload — **skipping the checksum slot itself**
  (``[14..18)``).
* The software implementation is a placeholder; the signature is designed so a
  hardware-accelerated streaming implementation (``crc32fast`` / PCLMULQDQ)
  can swap in later.
* Every page load through the buffer pool revalidates the header; a mismatch
  fails loudly with a ``ChecksumMismatch`` error. Silent payload corruption
  never propagates as garbage rows.

Page type and versioning
------------------------

* The ``flags`` field currently has no assigned page types (reserved).
* ``FORMAT_VERSION = 1`` is the current storage format version.
* ``PageHeader::decode`` rejects a checksum mismatch, and also rejects a
  payload ``format_version`` greater than the engine's (a future format is
  never silently downgraded or guessed).

Payload (row page layout)
-------------------------

The payload begins immediately after the 18-byte header. For the R3 table row
pages the payload layout defined in ``table_store.rs`` is:

.. code-block:: text

   [next_page_id : u64]    8 bytes @ payload offset 0  (0 = end of chain)
   [row_count    : u16]    2 bytes @ payload offset 8
   [row_0_len    : u32]    4 bytes little-endian
   [row_0_bytes  : [u8]]   row_0_len bytes
   [row_1_len    : u32]
   [row_1_bytes  : [u8]]
   …
   [free space]

Reserved per-page metadata: ``PAGE_META_SIZE = 8 + 2 = 10`` bytes
(``u64`` next pointer + ``u16`` row count). The maximum usable row area is:

.. code-block:: text

   ROW_AREA_SIZE = PAGE_SIZE - PageHeader::SIZE - PAGE_META_SIZE
                 = 8192 - 18 - 10
                 = 8164 bytes

Rows are length-prefixed so the scanner can skip forward without decoding.
When the current last page cannot fit another row, a fresh page is allocated
(global counter), linked via the ``next_page_id`` slot, and the row is appended
there. A page's ``row_count`` is a ``u16`` (max 65535 rows per page; in
practice the usable byte budget binds first).

Row serialization (codec)
-------------------------

Rows are encoded by ``encode_row`` / ``decode_row``
(``crates/qmind-sql/src/codec.rs``), one column in definition order:

* ``INT``: tag ``0`` + 8 little-endian bytes,
* ``TEXT``: tag ``1`` + ``u32`` UTF-8 length + bytes,
* ``NULL``: tag ``2``.

A truncated or mistagged stream decodes to ``None`` (a writer bug is surfaced
rather than silently producing columns).

Persistence location (FilePageStore)
------------------------------------

Pages are persisted by ``FilePageStore`` under the database's ``tables/``
directory in segment files:

* Segment file name: ``seg_{n:06}.bin``.
* Segment header: ``[magic "QMINDSEG"][u16 format_version][padding to 16
  bytes]``.
* Slots per segment: ``PAGES_PER_SEGMENT = 256`` → 2 MiB segments.
* A page with id ``p`` lives in segment ``p / 256`` at slot ``p % 256``;
  byte offset inside the file is ``16 + slot * 8192``.
* Opening/initializing a data directory validates the magic and format version
  of existing segments; a missing slot read reports ``PageNotFound``.

Buffer pool interaction
-----------------------

The ``BufferPool`` sits between table code and the store:

* ``create_page(id)`` — fresh zeroed page in a pinned, dirty frame (errors if
  the id already exists in pool *or* store).
* ``pin(id)`` — loads through the store if not resident and **revalidates the
  CRC** on every load; corrupted durable copies fail here.
* ``payload_mut(fid)`` — mutable view of the payload only; the pool owns header
  integrity and marks the frame dirty.
* ``seal()`` — recomputes the full-page checksum before any durable write.
* ``flush_all()`` — write back every dirty frame, then ``store.sync()``.
* Eviction is clock-sweep write-back: a dirty victim is sealed and written to
  the store before its frame is reused.

Corruption behavior
-------------------

* Checksum mismatch on load → ``Error::ChecksumMismatch`` (loud).
* Read beyond an existing segment → ``Error::PageNotFound``.
* Future payload format → loud rejection.
* The WAL side handles torn tails by truncation; **any other** corruption fails
  the open loudly rather than silently inventing state (R2.18).

Format/version handling and limitations
---------------------------------------

* **Provisional format.** The page format is versioned but has not reached a
  frozen milestone; it is documented from the current implementation.
* Single page size (8 KiB) — no variable-size pages.
* Append-only row chains — no in-place update, compaction, or free-space reuse
  on disk yet.
* ``row_count`` is a ``u16`` — an internal bound, documented above.
* Page types are not yet distinguished via ``flags``.
* The buffer pool and page store assume a single writer (R3 scope).