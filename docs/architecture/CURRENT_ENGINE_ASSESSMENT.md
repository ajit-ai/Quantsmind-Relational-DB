# Current Engine Assessment (R1)

> R1 deliverable — `docs/architecture/CURRENT_ENGINE_ASSESSMENT.md`
>
> Status: inspected against source, not README claims. Evidence cited as
> `file:line` throughout. This document classifies every major subsystem as
> `KEEP`, `REWORK`, `REPLACE`, `REMOVE`, or `DEFER` against the product target:
> a production-grade, general-purpose relational **HTAP** database for
> billion-row single-machine workloads with a later path to distribution.

---

## 0. Executive summary

The repository contains a coherent, well-tested **in-memory** relational
engine skeleton with many of the right primitives already present: 8 KiB
CRC-protected pages, a buffer pool, MVCC snapshots (`commit_ts`-based), a WAL
record format with torn-write detection, a B+Tree index structure, a
recursive-descent SQL front end, a Volcano executor, a minimal PGv3 wire
listener, a CLI, an embed JSON API, and an experimental on-disk columnar HTAP
path.

The single most important finding:

> **There is no working restart-persistence path today.** All catalog, index,
> and table data live in in-memory `HashMap`s. The WAL is written but never
> replayed on startup; `recovery::recover` is a pure function exercised only by
> tests (`recovery.rs:39`). A process restart returns to an empty database.

That is the decisive R1 fact: it does not invalidate the architecture — the
primitive set is the right skeleton — but it makes **R2 (durable storage +
recovery pipeline)** the mandatory prerequisite for every later R stage.

**Discrepancies between documentation and source** (recorded per spec):

1. `ARCHITECTURE_REPORT.md`/docs describe persistence and crash recovery as
   delivered behavior; the source shows persistence is write-only (WAL append
   with no fsync guarantee and no replay on open). See §4.
2. Docs/Studio copy describes the Studio as connected to the engine; the
   current React app executes SQL in-browser on **PGlite** and does not call the
   Rust engine at all (`src/lib/engine.ts:1,79`). See §8.
3. Docs describe HTAP as available; columnar reads only engage when
   `Engine::with_columnar` is configured (`engine.rs:90`), which the default
   runtime never does.

---

## 1. Storage subsystem

### 1.1 Page manager / page format

- **Component**: page manager and page format (`crates/qmind-kernel/src/page.rs`).
- **Current implementation**: fixed 8 KiB pages with an 18-byte header carrying
  a CRC32 over header + payload; page files persisted via `fs_store`.
- **Evidence from source**: `page.rs` (page + header layout, CRC); `fs_store.rs:112`
  uses `sync_all()` at a durability boundary; `buffer.rs:207-208` flush-all API.
- **Strengths**: fixed page size + checksums is the classic sound starting
  point; the buffer/store split exists.
- **Limitations**: no overflow/extended-value handling beyond a single page
  model; no page-level latching (whole store is serialized); no free-space
  management; no on-disk free-list.
- **Billion-row impact**: 1B rows require many millions of pages; page storage
  is intractable until eviction + free-space management + write-behind exist.
- **Production impact**: page format itself is acceptable; the management layer
  is incomplete.
- **Decision**: `REWORK`
- **Reason**: format is KEEP-worthy but the manager lacks eviction/write-behind/
  latching, which R2 must add.
- **Future target**: page store with real buffer management, page latching,
  free-space tracking (R2/R3 in `TARGET_ARCHITECTURE.md`).

### 1.2 Buffer pool

- **Component**: buffer pool (`crates/qmind-kernel/src/buffer.rs`),
  eviction (`crates/qmind-kernel/src/eviction.rs`).
- **Current implementation**: fixed-size pool with per-frame dirty tracking and
  `flush_all()` write-back; simple replacement policy in `eviction.rs`.
- **Evidence from source**: `buffer.rs:207-208` (`flush_all`, "Write back all
  dirty frames and sync the store"); `buffer.rs:274`, `fs_store.rs:184-245`
  (test paths relying on `flush_all`).
- **Strengths**: abstraction exists; dirty tracking present; unit tests exist.
- **Limitations**: no write-behind policy, no clock/LRU tuned for
  mixed OLTP/OLAP, no dynamic sizing, no background checkpointing, pool is not
  integrated with MVCC or WAL commit ordering.
- **Billion-row impact**: cannot serve a 1B-row dataset from a fixed pool with
  no eviction-driven read pattern.
- **Production impact**: acceptable for small datasets; performance collapses
  without managed memory.
- **Decision**: `REWORK`
- **Reason**: right abstraction, wrong life-cycle; production buffer management
  is a cornerstone of billion-row readiness.
- **Future target**: resizable pool, clean/dirty separation, background
  write-back coordinated with WAL checkpoints (R3).

### 1.3 Row store

- **Component**: row storage (`crates/qmind-sql/src/codec.rs`, the
  in-memory MVCC key/value backend in `crates/qmind-kernel/src/mvcc.rs`).
- **Current implementation**: rows are encoded (`encode_row`) and stored in an
  in-memory MVCC store keyed by `(table, row_id)`; the only on-disk artifact is
  an unreplayed WAL.
- **Evidence from source**: `engine.rs:335` `self.db.set(txn, &row_key(table, rid),
  encode_row(&row))`; `engine.rs:311` writes begin a transaction via `db.begin()`;
  `mvcc.rs:100` `MvccStore` is an in-memory structure.
- **Strengths**: row encoding exists; write path is transactional end-to-end.
- **Limitations**: **no disk-resident row store**; nothing survives restart;
  row layout is opaque to storage management (no slotted pages).
- **Billion-row impact**: impossible at 1B in current form (memory-bound).
- **Production impact**: correctness gap (data loss on restart) that must gate.
- **Decision**: `REWORK`
- **Reason**: must become a real on-disk slotted-page row store; today it is
  an in-memory prototype.
- **Future target**: disk-resident row store with slotted pages, MVCC-level
  versions, and checkpointing (R2/R3).

### 1.4 Column store (HTAP OLAP path)

- **Component**: column store (`crates/qmind-kernel/src/columnar.rs`),
  delta buffer/applier (`column_delta.rs`), reader (`column_reader.rs`),
  SQL integration (`engine.rs` M9).
- **Current implementation**: columnar segment files with RLE encoding;
  `DeltaApplier` accumulates rows and flushes segments by threshold;
  `ColumnarReader` serves reads; `Engine::with_columnar(dir)` enables the path.
- **Evidence from source**: `columnar.rs:547` segment flush `sync_all()`;
  `column_delta.rs:90-166` (`append_row`, `flush`, segment creation);
  `engine.rs:90-142` (`with_columnar`, `read_columnar` via `ColumnarReader::open`);
  `engine.rs:396-398` read path prefers columnar segments when present.
- **Strengths**: genuine on-disk columnar storage already exists and is wired
  into the SQL read path; RLE is a real compression primitive; has its own
  durability point.
- **Limitations**: off by default; no predicate/aggregation pushdown; possible
  RLE-null encoding quirk flagged during inspection (needs verification in R2);
  hard-coded flush thresholds; single-threaded flush.
- **Billion-row impact**: the only subsystem with a credible pattern for
  100M+ analytical scans; must be hardened, not discarded.
- **Production impact**: misleading if claimed as production HTAP today, but
  it is the strongest stone on the OLAP side.
- **Decision**: `KEEP`
- **Reason**: real on-disk columnar structure that matches the HTAP product
  target; worth investment.
- **Future target**: pushdown filters/aggregates, compression beyond RLE,
  multi-segment parallel scan, predicate pruning (R4).

### 1.5 Free-space management

- **Component**: free-space management (none found).
- **Current implementation**: absent.
- **Evidence**: no free-list / space-map module in `qmind-kernel/src`.
- **Strengths**: n/a.
- **Limitations**: page allocation is effectively append-only.
- **Billion-row impact**: churn-heavy workloads need free-space reuse.
- **Production impact**: low priority until R3.
- **Decision**: `DEFER`
- **Reason**: not required for the R2 persistence milestone; needed for stable
  heap reuse later.
- **Future target**: per-file space map or page free-list (R3).

### 1.6 Persistence + metadata (catalog)

- **Component**: persistence and metadata/catalog.
- **Current implementation**: schema lives in in-memory `HashMap`s
  (`engine.rs:243-244` `self.tables.insert(...)`); DDL (`CREATE TABLE`,
  `CREATE INDEX`) writes no WAL record; there is no system catalog module.
- **Evidence from source**: `engine.rs:219-244` (`create_table` mutates the
  in-memory map only); `engine.rs:280-292` (`create_index` builds in-memory B+Tree
  + `index_trees` map); dispatch `engine.rs:166-184`.
- **Strengths**: n/a.
- **Limitations**: **catalog is non-durable**; a restart forgets all tables.
- **Billion-row impact**: not sustainable.
- **Production impact**: mandatory fix in R2 (durable catalog + DDL journal).
- **Decision**: `REWORK`
- **Reason**: non-durable metadata violates the durability requirement.
- **Future target**: versioned system catalog stored through the WAL/checkpoint
  pipeline; DDL journaled (R2).

### 1.7 Indexes

- **Component**: index structures (`crates/qmind-kernel/src/btree.rs`,
  `engine.rs` index maintenance).
- **Current implementation**: in-memory arena B+Tree; secondary index trees kept
  in `index_trees` `HashMap`; maintained on insert
  (`engine.rs:344` `tree.insert(&index_key_encode(v)?, rid)`); unique/
  primary-key enforcement absent.
- **Evidence from source**: `btree.rs` (arena B+Tree); `engine.rs:277-292`.
- **Strengths**: working B+Tree with tests; index maintenance hooked into the
  write path.
- **Limitations**: in-memory only, not persisted, no UNIQUE/PK enforcement, no
  index statistics.
- **Billion-row impact**: 1B-row secondary indexes must be disk-resident.
- **Production impact**: indexes must be durable for correctness.
- **Decision**: `REWORK`
- **Reason**: format/structure is sound but lifecycle is not durable.
- **Future target**: disk-resident B+Tree or LSM for indexes, UNIQUE/PK
  enforcement + stats (R3/R5).

### 1.8 Partitioning

- **Component**: partitioning (none).
- **Current implementation**: absent.
- **Evidence**: no partition module.
- **Strengths**: n/a.
- **Limitations**: full tables scan as single units.
- **Billion-row impact**: partitioning (range/hash) is a major scaling lever.
- **Production impact**: deferred.
- **Decision**: `DEFER`
- **Reason**: architecturally reserved in `TARGET_ARCHITECTURE.md`, implemented
  at R5+.
- **Future target**: range/hash partitioning with partition pruning (R5).

---

## 2. Transactions

### 2.1 Transaction IDs / transaction manager

- **Component**: transaction manager (`crates/qmind-kernel/src/mvcc.rs`).
- **Current implementation**: in-memory manager tracking commit watermarks and
  pending transactions; commit timestamp assignment.
- **Evidence**: `mvcc.rs:100` `MvccStore`; `mvcc.rs:131` snapshot from
  `commit_watermark`; `mvcc.rs:152` version visibility `commit_ts <= snap.read_ts`;
  `mvcc.rs:189` pending filter.
- **Strengths**: commit-timestamp visibility is the right primitive for
  snapshot isolation; simple and testable.
- **Limitations**: entirely in-memory; no transaction log beyond the WAL
  group-write; no rollback set; no GC/vacuum for old versions; whole-store
  serialized writes.
- **Billion-row impact**: version GC is mandatory; a long-running read that
  pins old versions will exhaust memory at scale.
- **Production impact**: design sound, lifecycle incomplete.
- **Decision**: `KEEP` (core), with lifecycle `REWORK`
- **Reason**: commit-timestamp MVCC is the correct architecture; only its
  durability and GC need work.
- **Future target**: durable txn log, cluster-wide txn ids later, version GC
  at checkpoints (R4).

### 2.2 Snapshots

- **Component**: snapshots (`mvcc.rs:20-22` `Snapshot { read_ts: u64 }`).
- **Current implementation**: one monotonic `read_ts` per snapshot; readers
  capture one snapshot per statement (`engine.rs:174,202`).
- **Evidence**: `mvcc.rs:20-22`; `engine.rs:194` `execute_read` doc:
  "captures one snapshot for the whole statement".
- **Strengths**: per-statement point-in-time reads; P5 made reads concurrent
  and non-blocking.
- **Limitations**: no per-transaction snapshot spanning statements (no SQL
  `BEGIN`); no ANSI isolation level exposure.
- **Billion-row impact**: snapshot model is fine; GC must respect active
  `read_ts` (R4).
- **Production impact**: acceptable for MVP; long transactions future work.
- **Decision**: `KEEP` (design), `REWORK` (expose transaction-scoped snapshots)
- **Reason**: primitive correct.
- **Future target**: txn-scoped snapshots tied to MVCC GC watermark (R4).

### 2.3 Isolation / write conflicts / concurrency

- **Component**: isolation and write path.
- **Current implementation**: single writer under a whole-engine exclusive guard
  (`wire.rs:117` write guard) or a single engine instance; readers use
  snapshots. There is effectively one writer and no conflict detection because
  writes never interleave.
- **Evidence**: `wire.rs:107-124` (read/write dispatch); `wire.rs:11`
  `SharedEngine = Arc<RwLock<...>>`; P5 read stress tests.
- **Strengths**: deadlock-free today; snapshot isolation for reads is
  correct at the statement level.
- **Limitations**: no multi-writer concurrency, no row-level conflict
  detection, no SSI, no deadlock handling (none needed yet).
- **Billion-row impact**: concurrent OLTP writers are a core requirement;
  the single-writer model caps sustained write throughput.
- **Production impact**: must move past single-write-serialization.
- **Decision**: `REWORK`
- **Reason**: single-writer serialization is a scaling ceiling.
- **Future target**: multi-writer with row-level versioning and conflict
  detection (R4).

### 2.4 Locks / deadlock detection

- **Component**: lock manager (absent).
- **Current implementation**: only the server-level `RwLock` (not a storage lock
  manager).
- **Evidence**: `wire.rs:8` `std::sync::RwLock` import; no `lock.rs` usage in
  transaction path (agent inspection).
- **Strengths**: n/a.
- **Limitations**: none.
- **Billion-row impact**: negligible now.
- **Production impact**: deferred until multi-writer.
- **Decision**: `DEFER`
- **Reason**: no multi-writer concurrency yet.
- **Future target**: row/table lock manager + deadlock detection with SSI
  (R4 +).

### 2.5 Commit / rollback

- **Component**: commit/rollback path.
- **Current implementation**: commit writes a WAL group via
  `commit::<()>(txn, |recs| ...)` (`engine.rs:364`); no user-visible
  `ROLLBACK` statement exists in the parser surface.
- **Evidence**: `engine.rs:304-364` (`insert` → begin/set/commit);
  parser keyword surface (`parser.rs:395-399`).
- **Strengths**: grouped commit path exists.
- **Limitations**: rollback behavior not user-exposed; no statement-level
  rollback for multi-statement batches (server handles each `;`-split statement
  separately, `wire.rs:93-160`).
- **Billion-row impact**: R4 milestone.
- **Production impact**: polish item.
- **Decision**: `REWORK`
- **Reason**: commit durability requires the WAL rework; rollback needs SQL
  surface.
- **Future target**: SQL `BEGIN/COMMIT/ROLLBACK`, statement atomicity (R4).

---

## 3. Durability

### 3.1 WAL

- **Component**: write-ahead log (`crates/qmind-kernel/src/wal.rs`).
- **Current implementation**: append-only record stream written through a
  generic `Write` sink; `commit_group` does one bulk `write` + `flush`
  (`wal.rs:231-237`); torn-tail detection treats the tail as logical end
  (`wal.rs:255`, `wal.rs:450`); record structure includes a CRC.
- **Evidence from source**: `wal.rs:189-237` (group append semantics at
  `wal.rs:211` `append`), `wal.rs:450` torn-mid-record test.
- **Strengths**: record format with CRC + torn-tail handling is solid; grouped
  commit is a good base.
- **Limitations**: **not durable** — `sink.flush()` only flushes the in-memory
  buffer (`std::io::Write::flush`), not `fsync`/`sync_all`; **no replay on
  startup**; no checkpointing; no segment rotation; no WAL truncation.
- **Billion-row impact**: without fsync + replay, durability claims are void;
  without checkpoints, WAL growth is unbounded.
- **Production impact**: **the highest-priority rework item in the product**.
- **Decision**: `REWORK`
- **Reason**: durability is a hard requirement and the current WAL is
  write-mostly.
- **Future target**: fsync-disciplined WAL (group commit), startup replay,
  checkpoints with truncation, segment files (R2).

### 3.2 fsync / flush semantics

- **Component**: I/O durability semantics.
- **Current implementation**: `Write::flush()` on the sink; raw `sync_all()`
  exists in segment/store paths (`fs_store.rs:112`, `columnar.rs:547`) but the
  WAL commit path does not issue `sync_all`.
- **Evidence**: `wal.rs:237` `self.sink.flush()?;` vs `fs_store.rs:112`
  `f.sync_all()?;`.
- **Strengths**: some paths already sync.
- **Limitations**: WAL commit does not sync to the OS device.
- **Billion-row impact**: correctness first.
- **Production impact**: blocks honest durability claims.
- **Decision**: `REWORK`
- **Reason**: spec: "Correctness, durability and recoverability have priority
  over benchmark numbers."
- **Future target**: configurable durability (`sync`/`fsync`/group commit)
  (R2).

### 3.3 Recovery / crash recovery

- **Component**: recovery pipeline (`crates/qmind-kernel/src/recovery.rs`).
- **Current implementation**: `recover(log: &[u8]) -> RecoveredState`
  (`recovery.rs:39`) is a pure function over an in-memory byte slice; used by
  unit tests only; not invoked by `Engine::new`, the server binary
  (`wire.rs`/`main.rs`), or the embed API.
- **Evidence**: `recovery.rs:39`; `recovery.rs:101` builds `RecoveredState`;
  no call site outside tests (grep over `qmind-sql/src`, `qmind-server/src`,
  `qmind-embed/src` finds none).
- **Strengths**: logical replay logic exists and is tested in isolation.
- **Limitations**: not wired into any open/start path; no catalog replay; no
  replay of MVCC store frames.
- **Billion-row impact**: fundamental.
- **Production impact**: **critical**.
- **Decision**: `REWORK`
- **Reason**: recovery must be a first-class startup pipeline, not a test
  helper.
- **Future target**: startup recovery pipeline: open WAL → replay → catalog
  rebuild → checkpoint (R2).

### 3.4 Checkpoints

- **Component**: checkpointing (absent).
- **Current implementation**: none; no dirty-page checkpoint, no WAL
  truncation point.
- **Evidence**: buffer flush exists (`buffer.rs:208`) but no checkpoint/
  LSN-priority mechanism in the engine path.
- **Strengths**: n/a.
- **Limitations**: unbounded WAL growth; recovery must replay everything.
- **Billion-row impact**: checkpointing bounds recovery time.
- **Production impact**: long-run blocker.
- **Decision**: `DEFER`→`REWORK` (schedule in R2)
- **Reason**: needed for production but sequenced after WAL replay.
- **Future target**: LSN-based checkpoint with WAL truncation (R2).

### 3.5 Torn writes / corruption detection

- **Component**: integrity.
- **Current implementation**: WAL parsed defensively (torn tail ignored
  logically, `wal.rs:255,450`); pages carry CRC (`page.rs`); segment writes
  sync with checksum data.
- **Evidence**: `wal.rs:450` test name `torn_mid_record_tail_is_detected_...`;
  `page.rs` CRC header.
- **Strengths**: both WAL-tail and page-CRC protections exist.
- **Limitations**: no in-memory copy-on-write for torn page writes at the data
  file level; no checksums stored for row-store encoded values.
- **Billion-row impact**: integrity checks must scale with file size.
- **Production impact**: good foundation.
- **Decision**: `KEEP` (approach), extend coverage in R2
- **Reason**: correct primitives present.
- **Future target**: WAL/payload checksums everywhere; atomic page writes (R2).

---

## 4. Query engine

### 4.1 Parser / tokenizer / AST

- **Component**: SQL front end (`crates/qmind-sql/src/parser.rs`).
- **Current implementation**: hand-written tokenizer + recursive-descent parser;
  `Statement` AST includes `CreateTable`, `Insert`, `Select`, `CreateIndex`,
  `DropIndex`, `ShowTables` (`engine.rs:166-184`); expression tree built by
  precedence climbing (`parser.rs:763-781` multiplicative chain).
- **Evidence**: `parser.rs:395-399` keyword dispatch; `engine.rs:159-165`
  enforces exactly one statement.
- **Strengths**: readable, well-tested, no external parser deps; exact
  parse/execute single-statement contract is clean.
- **Limitations**: no `UPDATE`/`DELETE`/`BEGIN`/`COMMIT`/subqueries/CTE/window/
  prepared statements; error messages have limited position info.
- **Billion-row impact**: mid-term.
- **Production impact**: adequate for the R1-R2 surface; must expand at R5.
- **Decision**: `KEEP`
- **Reason**: no reason to replace a working front end.
- **Future target**: grow the grammar (binder-driven) rather than rewrite (R5).

### 4.2 Binder / semantic analysis

- **Component**: binder, semantic analysis (absent).
- **Current implementation**: the parser produces `Expr`/`Column` trees that
  execute directly; there is no intermediate bound representation; column
  resolution happens ad hoc during planning/execution.
- **Evidence**: no binder module in `qmind-sql/src` (agent inspection);
  executor consumes AST-shaped structures directly.
- **Strengths**: n/a.
- **Limitations**: no name/type resolution pass, no semantic validation layer.
- **Billion-row impact**: mid-term (unavoidable for a robust optimizer).
- **Production impact**: must be introduced before the optimizer.
- **Decision**: `REPLACE`
- **Reason**: there is nothing to preserve; a real binder must be built.
- **Future target**: typed `BoundQuery` with catalog lookups (R5).

### 4.3 Planner / optimizer

- **Component**: logical/physical planner and optimizer (absent; "plan" is
  close to the AST).
- **Current implementation**: selection is pushed to a fixed operator chain
  built inline by the executor (`executor.rs` operators below); no cost model,
  no join reordering, no predicate pushdown decisions.
- **Evidence**: executor operator list: `VecScan`, `Filter`, `Project`,
  `Limit`, `Sort`, `HashJoin`, `HashAggregate` (agent inspection,
  `executor.rs`); `engine.rs:396-398` has a special-case columnar read
  decision.
- **Strengths**: n/a.
- **Limitations**: no optimizer means query shape is fixed per statement.
- **Billion-row impact**: large joins/aggregations cannot be made efficient
  without planning.
- **Production impact**: R5.
- **Decision**: `REPLACE`
- **Reason**: don't retrofit an optimizer onto a plan-lite path; build the
  planner pipeline.
- **Future target**: logical plan → optimizer rules → physical plan mapped onto
  a batch engine (R5).

### 4.4 Executor

- **Component**: execution engine (`crates/qmind-sql/src/executor.rs`).
- **Current implementation**: Volcano-style row-at-a-time operators:
  `VecScan` (scan), `Filter`, `Project`, `Limit`, `Sort`, `HashJoin`,
  `HashAggregate`; expressions evaluated per row.
- **Evidence**: operator list above (agent inspection); M3 note in
  `lib.rs:5` ("columnar batches (~2048 rows)" as a milestone description).
- **Strengths**: clean operator model, fits the target Volcano→batch
  evolution, HashJoin + HashAggregate present.
- **Limitations**: row-at-a-time (no vectorization), no parallel operators, no
  spilling, no predicate/projection pruning into storage.
- **Billion-row impact**: row-at-a-time cannot reach analytical targets at 1B;
  batch execution is mandatory.
- **Production impact**: execution model is the second great lever after
  persistence.
- **Decision**: `KEEP` (operator layer), with batch execution `REWORK`
- **Reason**: the operator framework matches the target architecture; perf
  work is additive.
- **Future target**: columnar batch execution, vectorized exprs, parallel
  operators (R3).

### 4.5 Expression evaluation

- **Component**: expressions.
- **Current implementation**: AST expression tree evaluated recursively per
  row; supports arithmetic, comparison, string ops; three-valued NULL logic and
  casts are partial.
- **Evidence**: `parser.rs` expression grammar; `executor.rs`/`codec.rs`
  evaluation (agent inspection); SQL NULL semantics marked partial.
- **Strengths**: works for the supported surface.
- **Limitations**: per-row overhead, incomplete NULL semantics, no SQL
  functions beyond basics.
- **Billion-row impact**: vectorized expression chains needed at scale.
- **Production impact**: bounded now.
- **Decision**: `KEEP` (surface), `REWORK` (NULL/vectorization later)
- **Reason**: correct enough to keep; gains from vectorization fold into
  batch execution.
- **Future target**: vectorized expression evaluation with full SQL NULL
  semantics (R3/R5).

### 4.6 Performance repertoire

| Capability | Status | Class |
|---|---|---|
| Row-at-a-time execution | present | `REWORK` |
| Vectorized execution | absent | `DEFER` (R3) |
| Batch execution | columnar reader only (`engine.rs:396-398`) | `REWORK` |
| Parallel execution | absent | `DEFER` (R3/R7) |
| Predicate pushdown | absent into storage | `REWORK` (R4/R5) |
| Projection pruning | absent | `DEFER` (R5) |
| Partition pruning | absent (no partitions) | `DEFER` (R5) |
| Join algorithms | single HashJoin | `KEEP` (add merge join) |
| Aggregation | single HashAggregate | `KEEP` |
| Sorting | `Sort` operator | `KEEP` |
| Spilling | absent | `DEFER` (R3) |

---

## 5. SQL surface

Current statement support (evidence: `parser.rs:395-399`, `engine.rs:166-208`):
`CREATE TABLE [IF NOT EXISTS]`, `CREATE INDEX`, `DROP INDEX`, `INSERT INTO
… VALUES` (multi-row), `SELECT` (equi `JOIN`, `GROUP BY`, `ORDER BY`, `LIMIT`,
aggregates, expressions), `SHOW TABLES`. No `UPDATE`, `DELETE`, `BEGIN`,
`COMMIT`, `ROLLBACK`, subqueries, CTEs, window functions, or prepared
statements. `Engine::execute` and `execute_read` both reject multi-statement
input (`engine.rs:160-165`, `engine.rs:196-201`); the server splits on `;` and
executes each separately (`wire.rs:93-103`).

| SQL feature | Status | Class |
|---|---|---|
| SELECT | supported (scans, filters, joins, group, order, limit) | `KEEP` |
| INSERT | supported (multi-row) | `KEEP` |
| UPDATE | absent | `REPLACE` (build) |
| DELETE | absent | `REPLACE` (build) |
| transactions (SQL) | engine-level only; no `BEGIN/COMMIT` | `REWORK` (R4) |
| DDL | `CREATE TABLE/INDEX`, `DROP INDEX` only; non-durable | `REWORK` (R2/R4) |
| indexes | secondary only; in-memory | `REWORK` (R3) |
| constraints | `NOT NULL` (parser `Column`); no PK/UNIQUE/FK | `REPLACE` (R4/R5) |
| NULL semantics | partial three-valued logic | `REWORK` (R5) |
| expressions | arithmetic/comparison/string | `KEEP` |
| JOIN | equi only (HashJoin) | `KEEP` (extend) |
| GROUP BY / ORDER BY / LIMIT | supported | `KEEP` |
| subqueries / CTE / window | absent | `REPLACE` (R5+, not R1) |
| prepared statements | absent | `REPLACE` (R6) |

Per R1 rules, **no missing SQL feature is implemented in R1.**

---

## 6. Interfaces

### 6.1 Embedded API

- **Component**: `crates/qmind-embed/src/lib.rs`.
- **Current implementation**: `Database<W: Write>` wrapping `Engine<W>`;
  one-method JSON contract `{"ok":true,"columns":…,"rows":…}` /
  `{"ok":false,"error":…}`; intended for Tauri commands/FFI.
- **Evidence**: `lib.rs:11-28` (`Database::new`, `execute` JSON payloads).
- **Strengths**: minimal, FFI-friendly, no GUI coupling.
- **Limitations**: `&mut self` only (no concurrent reads through embed);
  strings-only cells; no streaming of large results.
- **Billion-row impact**: must add streaming/incremental result contracts.
- **Production impact**: fine for MVP contract.
- **Decision**: `KEEP`
- **Reason**: the boundary is correct; API shape can evolve.
- **Future target**: snapshot-read API (`&self`) mirroring `execute_read`,
  typed values, streaming results (R6).

### 6.2 PostgreSQL wire protocol

- **Component**: `crates/qmind-server/src/wire.rs`.
- **Current implementation**: minimal PGv3: trust auth (implicit)
  (`wire.rs:2`, `wire.rs:63` immediately sends auth-ok), `SSLRequest` answered
  `'N'` (`wire.rs:54-58`), startup accepting `8..=10000` bytes (`wire.rs:48`),
  simple-`Q` only (`wire.rs:73`), values as TEXT OID 25 (`wire.rs:2`),
  thread-per-connection (`wire.rs:20`), `SharedEngine = Arc<RwLock>`
  (`wire.rs:11`), `;`-split multi-statement execution with keyword routing
  (`wire.rs:93-124`).
- **Evidence**: cited above.
- **Strengths**: correct startup/-Shutdown framing; P5 read/write concurrency
  split is a genuine feature (readers never block writers on commit).
- **Limitations**: trust auth only, no TLS, no extended protocol, no prepared
  statements, single-format TEXT, no resource/timeout management, no
  observability/logging hooks, per-connection thread (no pool).
- **Billion-row impact**: protocol isn't the bottleneck, but extended protocol
  is needed for prepared analytics at R6+.
- **Production impact**: R6 milestone.
- **Decision**: `REWORK`
- **Reason**: correct base framing; production protocol needs auth, TLS,
  extended query, pooling.
- **Future target**: PGv3-compatible extended protocol, SCRAM/TLS, session
  management, observability (R6).

### 6.3 CLI

- **Component**: `crates/qmind-cli/src/main.rs`.
- **Current implementation**: psql-like REPL over the wire protocol,
  `qmind>` prompt, `\q`/`exit`, one-statement-per-line.
- **Evidence**: `main.rs:1-39`.
- **Strengths**: works, tiny, tests the wire server end-to-end.
- **Limitations**: no `\d`, no result formatting options, no history.
- **Billion-row impact**: none.
- **Production impact**: polish later.
- **Decision**: `KEEP`
- **Reason**: serves its purpose; no rework justified now.
- **Future target**: slow polish in R6+.

### 6.4 Tauri / React / PGlite / Studio

- **Component**: `src-tauri`, `src` (React+TS+Vite+Tailwind), PGlite, Studio.
- **Current implementation**: Tauri shell hosting the React app; the app's SQL
  layer is an **in-browser PGlite (PostgreSQL WASM)** instance
  (`src/lib/engine.ts:1` import, `:79` `new PGlite('idb://quantsmind')`);
  UI badges advertise "IndexedDB (PGlite)" / "PGlite · PostgreSQL WASM"
  (`src/App.tsx:609,634`); dialog copy calls it "powered by PGlite"
  (`src/components/Dialogs.tsx:98`). The Rust engine is not in this path; the
  embed JSON API target of Tauri commands (`qmind-embed/lib.rs:1-4`) is not the
  app's execution backend today.
- **Evidence**: cited above.
- **Strengths**: clean React/tailwind shell; qmind-embed exists as the intended
  command contract.
- **Limitations**: **second database implementation inside the Studio**
  (violates the product rule), disconnected from the engine, no offline sync
  story, GUI semantics (SQL behavior) duplicated in the browser.
- **Billion-row impact**: none, but architecture is wrong per R1 rules.
- **Production impact**: must be corrected at R8.
- **Decision**: Tauri `REMOVE` (from core dependency, see §7), PGlite `REMOVE`
  (from final Studio), React shell `KEEP` (as replaceable client), Studio
  `REWORK`.
- **Reason**: the Studio must be a pure client of the engine; PGlite must not
  remain a second DB implementation.
- **Future target**: Studio connects to the real engine over embed or PG wire;
  single SQL semantics implementation (R8).

---

## 7. Cross-cutting classifications (summary table)

| Subsystem | Decision |
|---|---|
| Page format + CRC | `KEEP` |
| Page manager | `REWORK` |
| Buffer pool + eviction | `REWORK` |
| Row store | `REWORK` |
| Column store (HTAP) | `KEEP` |
| Free-space management | `DEFER` |
| Catalog/metadata | `REWORK` |
| Indexes (B+Tree) | `REWORK` |
| Partitioning | `DEFER` |
| MVCC / snapshots (design) | `KEEP` |
| Txn manager lifecycle (durability/GC) | `REWORK` |
| Isolation / multi-writer | `REWORK` |
| Lock manager / deadlocks | `DEFER` |
| WAL | `REWORK` |
| fsync/flush discipline | `REWORK` |
| Recovery pipeline | `REWORK` |
| Checkpoints | `REWORK` (deferred to R2) |
| Torn write / CRC detection | `KEEP` |
| Parser / tokenizer / AST | `KEEP` |
| Binder / semantic analysis | `REPLACE` (build) |
| Planner / optimizer | `REPLACE` (build) |
| Executor (Volcano operators) | `KEEP` |
| Batch execution / vectorization | `REWORK` |
| Row-at-a-time perf | `REWORK` |
| SQL surface (SELECT/INSERT/JOIN/GROUP…) | `KEEP` |
| SQL surface (UPDATE/DELETE/subqueries/…absent) | `REPLACE` (later) |
| Constraints / NULL semantics | `REPLACE`/`REWORK` (R5) |
| Embedded API | `KEEP` |
| PG wire protocol | `REWORK` |
| CLI | `KEEP` |
| Tauri (engine dependency) | `REMOVE` |
| PGlite (final Studio) | `REMOVE` |
| React shell | `KEEP` (as replaceable client) |

Two decisions dominate R2: **WAL/recovery/catalog durability (REWORK)** and
**moving the row store + indexes from in-memory to disk (REWORK)**.

---

## 8. Documentation-vs-source discrepancy ledger (R1.1)

1. **Persistence claimed vs actual**: README/docs describe a persisted,
   crash-recoverable engine; no open/start path replays the WAL
   (`recovery.rs:39` test-only), and schema + data are in-memory
   (`engine.rs:243-244`, `mvcc.rs:100`).
2. **Studio connectivity claimed vs actual**: Studio badges/copy advertise
   PGlite as its database (`src/App.tsx:609,634`); the Rust engine is not used
   by the app; the embed contract (`qmind-embed/lib.rs`) is not the app's SQL
   backend.
3. **HTAP availability claimed vs actual**: HTAP reads exist only when
   `Engine::with_columnar` is configured (`engine.rs:90`); the default runtime
   and server never enable it, so out-of-the-box behavior is OLTP-only.
4. **"Columnar batches (~2048 rows)" milestone claim** (`qmind-sql/lib.rs:5`):
   batch execution exists only as a columnar-read concept, not as a general
   batch execution engine.
5. **WAL durability claim**: WAL commit path flushes the sink but issues no
   `sync_all` (`wal.rs:237`); OS-crash durability is not guaranteed.

No source file was altered during this assessment.