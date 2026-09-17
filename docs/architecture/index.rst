Architecture
============

The R3 architecture documentation covers the transitional
storage/execution architecture, the WAL-to-page-store ordering rules, and the
architecture decision records behind the R3 choices.

.. toctree::
   :maxdepth: 2
   :caption: R3 architecture

   r3-architecture
   wal-storage-ordering
   R3_COMPLETION_REPORT

.. toctree::
   :maxdepth: 2
   :caption: R4 milestones

   R4_MVCC_COMPLETION_REPORT
   R4_CONCURRENCY_COMPLETION_REPORT
   R4_LOCK_COMPLETION_REPORT
   R4_DEADLOCK_COMPLETION_REPORT
   R4_MULTIWRITER_COMPLETION_REPORT

Architecture decision records (ADRs)
------------------------------------

New ADRs follow the repository convention: Markdown files in
``docs/architecture/adr/`` numbered sequentially after ADR-013.

* :download:`ADR-014 StorageManager integration <adr/ADR-014-storage-manager-integration.md>`
* :download:`ADR-015 WAL-to-page-store ordering <adr/ADR-015-wal-page-store-ordering.md>`
* :download:`ADR-016 Transitional page reconstruction <adr/ADR-016-transitional-page-reconstruction.md>`
* :download:`ADR-017 Batch execution <adr/ADR-017-batch-execution.md>`

Legacy R2/R1 architecture notes (kept in place, not migrated)
-------------------------------------------------------------

* :download:`R2 completion report <R2_COMPLETION_REPORT.md>`
* :download:`R1 completion report <R1_COMPLETION_REPORT.md>`
* :download:`Recovery invariants <RECOVERY_INVARIANTS.md>`
* :download:`Durability contract <DURABILITY_CONTRACT.md>`
* :download:`Storage layout <STORAGE_LAYOUT.md>`