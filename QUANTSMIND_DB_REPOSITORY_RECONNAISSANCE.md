# QUANTSMIND DB — REPOSITORY RECONNAISSANCE & ARCHITECTURE BASELINE

> Reconnaissance date: 2026-09-13
> Mode: inspection-only. No source files were modified, no commit/push was performed.
> This document is a baseline for the next implementation phase. It intentionally uses honest status language (Experimental / Partial / Not implemented / Planned).

---

## 1. Repository Inventory

### Location & VCS state

| Item | Value |
|---|---|
| Repository root | `F:\Codes\Git\Quantsmind-Relational-DB` |
| Current branch | `develop` (at `299f238`) |
| `main` branch | at `4f434bc` |
| Remote | `origin` → `https://github.com/ajit-ai/Quantsmind-Relational-DB.git` |
| Tags | `v0.1.0` |
| Working tree state | **2 files modified (uncommitted)** — `crates/qmind-sql/src/engine.rs`, `crates/qmind-sql/tests/sql_e2e.rs` (in-progress M9 columnar integration) |
| Stashes | none |

### Latest commits on `develop`

| Hash | Subject |
|---|---|
| `299f238` | docs: INSTALL.md + ARCHITECTURE_REPORT.md |
| `913fe8a` | feat(M8c): columnar reader — OLAP scan path over columnar segments |
| `83e9dd3` | feat(M8b): delta capture + async apply — WAL to columnar segments |
| `2132c50` | feat(M8a): columnar segment storage — persistent format with 3 encodings |
| `7a45cd0` | feat(M7b): cross-platform packaging — install/uninstall scripts for all platforms |

### Directory tree (tracked, 101 files)

```
.bolt/                          # Bolt tool config (vite-react-ts template) — tracked
.github/workflows/ci.yml        # CI gates (fmt, clippy, test, bench-compile)
.github/workflows/release.yml   # Tag-triggered multi-target build + sha256sums
docs/ARCHITECTURE.md            # Design doc + locked decisions (D-001..D-004)
docs/ROADMAP.md                 # Milestone list (STALE — see §18)
crates/
  qmind-kernel/    src/{page,buffer,btree,wal,mvcc,lock,recovery,fs_store,eviction,error,columnar,column_delta,column_reader}.rs
                   benches/kernel_bench.rs · tests/kernel_integration.rs
  qmind-sql/       src/{parser,executor,engine,codec}.rs · tests/{sql_e2e,parser_fuzz,soak}.rs · benches/sql_bench.rs
  qmind-server/    src/{wire,lib,main}.rs · tests/wire_e2e.rs
  qmind-cli/       src/main.rs
  qmind-embed/     src/lib.rs · tests/embed_api.rs
src-tauri/         Tauri 2 desktop shell (EXCLUDED from Cargo workspace)
src/               React + Tailwind frontend (legacy PGlite prototype + QmindStudio shell)
packaging/         linux/{install,uninstall,freebsd-install}.sh, macos/{install,uninstall}.sh + quantsmind.rb, windows/{install,uninstall}.ps1 + qmind.json
scripts/           build-all.ps1 / build-all.sh
dist/              (on disk, GITIGNORED) pglite WASM browser build output
```

### File counts

| Category | Count |
|---|---|
| Total tracked files | 101 |
| Rust source files (`.rs`) | 34 |
| Frontend source files (`.tsx`/`.ts`) | 20 |
| Config / metadata (`.toml`, `.json`, `.yml`, `.js`) | 25 |
| Shell/PowerShell/Ruby scripts | 9 |
| Markdown docs | 5 |
| Test/bench Rust files | 8 (~1,900 lines) |
| Rust source lines (crates, non-test) | ~7,200 |

### Suspicious / generated artifacts (present, nothing deleted)

| Path | Status | Verdict |
|---|---|---|
| `dist/` | Gitignored, on disk | Legacy PGlite WASM build output. Build artifact, not tracked. |
| `node_modules/` | Gitignored | Normal. |
| `src-tauri/target/`, `target/` | Gitignored | Normal build cache. |
| `src-tauri/qmind-data/desktop-wal.log` | Gitignored (`*.log`) | Runtime data artifact from running the desktop app. |
| `src-tauri/gen/schemas/*.json` | **Tracked** | Generated Tauri capability schemas; tracked by default. Debatable but harmless. |
| `.bolt/config.json`, `.bolt/prompt` | **Tracked** | Tool artifact from bolt.new; not part of the product. Candidate for gitignore. |
| `package.json` name = `vite-react-typescript-starter` | Tracked | Boilerplate name never updated (see §2). |

**Finding:** No `__pycache__`, `.egg-info`, `*.pyc`, or Python artifacts exist. This is a Rust workspace; the recon template's Python categories are not applicable.

---

## 2. Package / Project Identity

### Current identity (as claimed by the repo)

| Field | Value |
|---|---|
| Name (workspace) | `qmind-*` crates: `qmind-kernel`, `qmind-sql`, `qmind-server`, `qmind-cli`, `qmind-embed`; desktop `qmind-desktop` |
| Version | `0.1.0` (workspace-wide, `[workspace.package]`) |
| Edition / rust-version | Rust 2021 / 1.75 |
| License | None declared in any `Cargo.toml` |
| Runtime dependencies | `qmind-kernel`: **none**; `qmind-sql`: kernel only; `qmind-server`/`qmind-cli`: kernel+sql; `qmind-embed`: adds `serde_json`. Frontend adds `@electric-sql/pglite`, `@supabase/supabase-js` (legacy). |
| Package namespace | `qmind_*` (Rust crates), no importable Python namespace |
| CLI | `qmind-cli` (psql-like; opens TCP to server), `qmind-server` (wire listener), `qmind-desktop` (Tauri) |
| README identity | "A production-grade, embeddable relational database engine written in Rust, designed for hybrid transactional + analytical workloads (HTAP). Ships as a library, a Postgres-wire-compatible server, a CLI shell, and a native desktop GUI studio." |

### Inconsistencies found

1. **License:** no `license` field in any Cargo.toml, no LICENSE file. A public repo (GitHub remote) with no declared license.
2. **package.json brand:** still `vite-react-typescript-starter`, version `0.0.0` — not `quantsmind-studio` / `0.1.0`.
3. **Docs vs implementation:** `docs/ARCHITECTURE.md` claims `sqlparser-rs` for parsing ("do not hand-roll"); the shipped implementation is a **handwritten parser** (confirmed in `qmind-sql/Cargo.toml` description and E5 evolution). Docs stale.
4. **ROADMAP.md statuses stale** — see §18.

---

## 3. Complete Module-Level Inventory

| Module | Purpose | Impl. | Partial | Skeleton | Tests | Notes |
|---|---|---:|:---:|:---:|:---:|---|
| `qmind-kernel/src/page.rs` | 8KiB versioned pages, CRC header, checksum validate/seal | ✓ | | | (in 61 kernel) | D-003 versioned headers |
| `qmind-kernel/src/buffer.rs` | Clock-sweep buffer pool, fixed/replaceable frame, write-back eviction, validate-on-load | ✓ | | | ✓ | |
| `qmind-kernel/src/btree.rs` | B+Tree insert/get/get_all/range-scan, run-preserving splits, duplicate ordering, differential test | ✓ | | | ✓ | |
| `qmind-kernel/src/wal.rs` | CRC frame WAL, group commit, torn-tail replay, committed-prefix crash semantics | ✓ | | | ✓ | |
| `qmind-kernel/src/mvcc.rs` | Snapshot isolation, first-committer-wins, versioned rows, GC | ✓ | | | ✓ | |
| `qmind-kernel/src/lock.rs` | Lock table / row-version locks | ✓ | | | ✓ | |
| `qmind-kernel/src/recovery.rs` | Checkpoint + redo/undo replay from WAL | ✓ | | | ✓ | |
| `qmind-kernel/src/fs_store.rs` | Segment-file-backed page store | ✓ | | | ✓ | File-backed store (M1) |
| `qmind-kernel/src/eviction.rs` | Eviction policy helper | ✓ | | | ✓ | |
| `qmind-kernel/src/error.rs` | Kernel error enum | ✓ | | | | |
| `qmind-kernel/src/columnar.rs` | M8a persistent columnar segments: QMINDCOL header, ColumnType, Raw/Dict/RLE encodings, builder+dump | ✓ | | | ✓ | |
| `qmind-kernel/src/column_delta.rs` | M8b delta buffer + schema-aware DeltaApplier, LSN markers | ✓ | | | ✓ | |
| `qmind-kernel/src/column_reader.rs` | M8c ColumnarReader: all/filtered/projected scans | ✓ | | | ✓ | |
| `qmind-sql/src/parser.rs` | Handwritten recursive-descent SQL parser (DDL/DML/SELECT/JOIN/GROUP BY + fuzz harness) | ✓ | | | 23 unit ✓ | |
| `qmind-sql/src/executor.rs` | Volcano-style operators (scan/filter/project/join/aggregate) | ✓ | | | ✓ (via e2e) | |
| `qmind-sql/src/engine.rs` | Engine facade: catalog, execute(), MVCC + **new columnar routing (uncommitted M9)** | ✓ | | | ✓ | In-flight edits |
| `qmind-sql/src/codec.rs` | ColumnDef/ColumnType/SqlValue, row encode/decode | ✓ | | | ✓ | |
| `qmind-server/src/wire.rs` | Minimal PG wire v3: trust auth, simple Query, TEXT values (OID 25), threading per conn | | ✓ | | 3 e2e ✓ | No scram, no extended protocol, no TLS |
| `qmind-cli/src/main.rs` | psql-like REPL over TCP | | ✓ | | — | Reads only startup/Query/row messages |
| `qmind-embed/src/lib.rs` | `Database::execute` → JSON string API for GUI/FFI | ✓ | | | 2 ✓ | Target of Tauri command |
| `src/` React app | Legacy PGlite browser prototype + minimal QmindStudio eval shell | | ✓ | | — | See §4 |
| `src-tauri/` | Tauri 2 shell, `run_sql` command over embedded engine | ✓ (scaffold) | | ✓ | — | M6 shell only |

---

## 4. What "QuantsMind DB" Currently Means

From the actual implementation (not the README):

- **It is a relational database storage engine + SQL layer**, embedded-first, with a small PG-wire server.
- The core is a **hand-written KV + B+Tree + MVCC + WAL storage kernel** (`qmind-kernel`, zero runtime deps) — classic OLTP row store.
- A **persistent columnar replica** (M8a/M8b/M8c) was added for the analytical side of HTAP; SQL-engine integration is mid-flight (uncommitted M9).
- Query layer is a **handwritten parser + Volcano executor** with scan/filter/project/limit plus hash-style INNER JOIN and GROUP BY aggregates.
- It is **NOT** yet a document DB, graph DB, vector DB, time-series DB, or multi-model DB. No such subsystems exist. The kernel layering (D-002) makes those additive later, but nothing is implemented.
- `qmind-embed` + `qmind-server` make it usable both embedded (library) and networked (server).
- The frontend still contains a legacy **PGlite (WASM Postgres) browser prototype** in `src/` plus a small Tauri shell that talks to the **real Rust engine** via `qmind-embed`. The rich UI components currently bind to PGlite, **not** to the Rust engine.

**Bottom line:** "QuantsMind DB" today = a self-contained, dependency-light OLTP relational engine with a newly introduced OLAP columnar path, a partial PG-wire server, and an unfinished desktop shell.

---

## 5. Data Model Analysis

### Core domain types

**Storage kernel layer**
- `Page` (8KiB, versioned, CRC) — physical unit of persistence.
- `BTree` record — key → value; duplicate `(key,value)` supported.
- MVCC `Version` / row version — snapshot identity + visibility.
- WAL `Frame` — CRC checksummed log record; group-commit batches.

**SQL layer**
- `ColumnDef` / `ColumnType` (`Int`, `Text`) in `codec.rs` — logical schema for `CREATE TABLE`.
- `SqlValue` (`Null | Int(i64) | Text(String)`) — **3-type value model**. No Float/Bool/Decimal/Date/JSON at the SQL layer yet.
- Row codec — `codec.rs` serializes rows to/from key/value.

**Columnar layer (M8)**
- `ColumnSegment` / `ColumnSegmentBuilder` — per-column segment files.
- `ColumnType` (`Null | Int | Text`); encodings `Raw | Dict | RLE`.
- `TableSchema` / `ColumnInfo` / `DeltaRow` / `DeltaBuffer` / `DeltaApplier` — HATP delta path.

### Ecosystem concepts check (Entity / State / Interaction / Observation / Knowledge)

Nothing in the current code models these. The schema is strictly relational columns+tables. **For a DB layer these concepts are not needed as core models** — they belong to an application/SDK layer or future JSONB-style document model, not the storage/query core. Do **not** import them into the DB engine.

---

## 6. Storage and Persistence Analysis

- **Where data lives:** `fs_store.rs` segment files for pages; WAL in `qmind-data/wal.log` (server default) or caller-provided sink (`Engine<W: Write>`); columnar segments written by M8 builders.
- **How written:** buffer pool write-back eviction → page store; WAL append with group commit (one syscall per batch); columnar at commit/apply time via DeltaApplier.
- **How read:** buffer pool validate-on-load (page CRC) → B+Tree; MVCC visibility; columnar via `ColumnarReader`.
- **Serialization:** `codec.rs` row codec; versioned page format (magic + version, D-003); QMINDCOL header for columnar.
- **Guarantees:** WAL-integrated commit, committed-prefix replay, torn-tail-safe; full-page CRC. Recovery test covers restart round-trips and committed-state reconstruction.
- **Concurrency:** MVCC snapshots (SI), first-committer-wins validation, lock table. Readers run concurrently; writers serialized via engine mutex at the SQL facade (single-writer today).
- **Durability:** real — file-backed store exists and is tested. Not mocked, not dependent on another database.
- **Corruption detection:** page CRC on load + WAL frame CRC + replay checks.

**Verdict: REAL, functional, file-backed, with tested crash semantics.** The main gap is that a single `Mutex<Engine>` serializes all SQL requests (no concurrent-writer support / latch crabbing yet).

---

## 7. Query Engine Analysis

| Capability | Status | Evidence |
|---|---|---|
| Lookup (point) | Implemented | B+Tree get; executor scan+filter; e2e tests |
| Filtering (predicates) | Implemented | executor filter ops |
| Projection | Implemented | executor project; columnar `read_projected` |
| Sorting | Absent | no ORDER BY operator found |
| Aggregation (GROUP BY / COUNT / SUM…) | Implemented | M4 aggregates; bench + e2e |
| Joins (hash INNER JOIN) | Implemented | M4; `select_join` — **plain column projections only** (no aggregates in join projections; no JOIN+GROUP BY) |
| Expressions | Partial | limited arithmetic/comparison in predicate path; not a full expression engine |
| Query planning | Absent | parser → direct plan; no optimizer rules, no cost model (documented as "plan-lite") |
| Execution plans | Partial | "plan-lite" IR inside executor |
| Indexes | Partial | B+Tree is the PK store; **no secondary indexes** (M4 exit criterion pending) |
| Optimization | Absent | none (threshold routing OLTP vs vectorized is planned, not shipped) |
| Vectorized/columnar OLAP path | Partial | M8c reader + uncommitted engine routing; batch constant `BATCH_ROWS = 2048` |

Documented known limitations (verified by tests failing when attempted): JOIN+GROUP BY and aggregate-over-JOIN are unsupported — the soak test was written to avoid them.

---

## 8. Indexing Analysis

- The B+Tree acts as the primary (row-id → row) heap index. Duplicate `(key,value)` ordering and range scans are tested.
- **No secondary indexes, no unique indexes beyond the primary structure, no composite indexes, no index maintenance for `CREATE INDEX`.**
- Index persistence is part of page storage (persistent). Performance characteristics are benchmarked (B+Tree insert ~3.56M elem/s release build).
- **Verdict:** primary indexing real; secondary indexing **NOT PRESENT** (ROADMAP lists it as M4 remainder).

---

## 9. Transaction / Concurrency Analysis

| Capability | Classification |
|---|---|
| Transactions (BEGIN/COMMIT/ROLLBACK in SQL) | PARTIAL — MVCC commit/rollback exists at kernel level; SQL-level statement model tested; multi-statement transactions/groups are exercised in e2e |
| Isolation (snapshot) | IMPLEMENTED — Snapshot Isolation + first-committer-wins |
| Locking | IMPLEMENTED — lock table / row-version locks |
| Concurrent readers | IMPLEMENTED (MVCC reads against snapshot) |
| Concurrent writers | NOT PRESENT — single `Mutex` serialization at `Engine` + `SharedEngine` (`Arc<Mutex<…>>`) in server and Tauri |
| Thread safety | PARTIAL — safe under one mutex; no latch crabbing, no multi-writer |
| Process safety | NOT PRESENT — single-process model; no multi-process open |
| Crash recovery | IMPLEMENTED — WAL replay + checkpoint; recovery module tests |

---

## 10. Serialization / Import / Export

| Capability | Status |
|---|---|
| JSON | Only at the `qmind-embed` boundary (`serde_json` for GUI responses) |
| CSV / JSONL / import / export | NOT PRESENT |
| Binary row codec | IMPLEMENTED (`codec.rs`) |
| Snapshots / backup / restore | NOT PRESENT (checkpoint exists internally, no user-facing backup tool) |
| Custom formats | QMINDCOL columnar format (M8a) + versioned pages + WAL frames |

---

## 11. API Analysis

### Public API (intended for consumers)
- `qmind-embed::Database::execute(sql) -> String` (JSON) — the GUI/FFI contract. Tested (2 tests). Stable shape.
- `qmind-sql::Engine::execute(sql) -> ExecResult` — core library API. Tested via e2e and embed.
- `qmind-sql` re-exports `Engine`, `ExecResult`, `ColumnDef`, `ColumnType`, `SqlValue`, `BATCH_ROWS`, `ENGINE`.
- Server: PG wire v3 simple-Query over TCP (trust auth). Tested e2e (2 clients).
- CLI shell. Not unit-tested.

### Internal API
- `qmind-kernel` modules (page/buffer/btree/wal/mvcc/…). Public crate surface is the whole module set; no `#[doc(hidden)]`, no sealed traits, no semver-gated API. Fuzz/internal structure is stable enough for benches.

### Experimental API
- Columnar M8 surface: `ColumnSegmentBuilder`, `DeltaApplier`, `ColumnarReader`, and the **uncommitted `with_columnar()` / `flush_to_columnar()` / `select_from_columnar()`** engine methods.

**Error behavior:** SQL errors are strings (`Err(String)`-ish) returned in JSON; no typed error taxonomy at the SQL layer (kernel has `error.rs`). Wire `E` messages are generic "ERROR".

---

## 12. Test and Quality Assessment

Command run (read-only): `cargo test --workspace`

**Result: 108 passed; 0 failed; 0 ignored.**

| Suite | Tests | Pass |
|---|---:|---:|
| kernel unit | 61 | 61 |
| kernel integration | 1 | 1 |
| sql parser | 23 | 23 |
| sql e2e (incl. 3 new M9 columnar tests) | 13 | 13 |
| parser fuzz | 4 | 4 |
| soak (10K-row lifecycle) | 1 | 1 |
| server wire e2e | 3 | 3 |
| embed api | 2 | 2 |

`cargo clippy --workspace --all-targets -- -D warnings` currently fails with **1 error: "redundant closure" at `engine.rs:243:64`** — inside the **uncommitted M9 columnar integration code**. Committed code was clippy-clean at release.

### Test-quality table

| Area | Tests | Confidence |
|---|---:|---|
| Storage (page/buffer/btree/wal/recovery/fs_store) | High (in 61 kernel) | High |
| Query (parse→scan/filter/project/join/aggregate) | Medium (e2e + bench) | Medium-High |
| Models/codec | Medium | Medium |
| Indexing (primary only) | Medium | Medium; secondary absent |
| Transactions/MVCC | Medium-High | High |
| Columnar M8 | 20 (7+8+5) | Medium; engine integration untested until M9 lands |
| Serialization | Medium | Medium |
| API (embed/server) | Low (2+3) | Medium |
| Error handling | Low | Low — negative-path tests scarce; wire error text minimal |
| Concurrency | Low | Low — no multi-writer stress; single mutex |
| Fuzz | 4 tests / 8K inputs | Medium; parser-only |
| Soak | 1 (10K rows, 10 phases) | Medium; 72h soak not yet run |

Tests verify real behavior (round-trips, restart recovery, crash semantics, parser fuzz), not just construction. Quality is generally good.

---

## 13. Dependency Analysis

| Crate | Runtime deps | Dev deps | Why |
|---|---|---|---|
| `qmind-kernel` | **none** | criterion | Fully self-contained storage kernel — remarkable and valuable |
| `qmind-sql` | qmind-kernel | criterion | Handwritten parser/executor |
| `qmind-server` | qmind-kernel, qmind-sql | — | Wire listener |
| `qmind-cli` | qmind-kernel, qmind-sql | — | Shell |
| `qmind-embed` | qmind-sql, serde_json | — | JSON bridge |
| `src-tauri` (excluded) | tauri 2, qmind-embed, serde_json | — | Desktop shell |
| Frontend | `@electric-sql/pglite`, `@supabase/supabase-js`, react, lucide-react | vite, tailwind, typescript, tauri CLI | **pglite + supabase-js are LEGACY** from the browser prototype — only used by the unmounted `App.tsx`; `QmindStudio` uses the Rust bridge |

### Coupling assessment
- Coupled to NumPy/SciPy/cloud/external APIs: **No** (not a Python project).
- Coupled to SQL/NoSQL databases: **No** (pglite is in the frontend prototype, not the engine).
- Coupled to QuantsMind SDK / MicroQuantum / Karkain: **No imports anywhere.** This repo is fully standalone.

**Recommended cleanup (future, not done now):** prune `@electric-sql/pglite` and `@supabase/supabase-js` from the frontend once the legacy prototype is dropped or rewired.

---

## 14. QuantsMind Ecosystem Boundary Analysis

### QuantsMind SDK ↔ QuantsMind DB
- **DB owns:** storage, indexing, transactions, query execution, persistence formats, engine/embed/wire APIs.
- **SDK owns:** application-domain models and client-side helpers (Entity/State/Observation/knowledge concepts, analytics orchestration, workflows). The DB must store *data*; it must not *implement domain logic*.
- Do not duplicate: schema/query convenience wrappers that a thin adapter (`quantsmind.db` binding) can provide over `qmind-embed`. Build an *adapter*, not a fork of engine code.

### MicroQuantum ↔ QuantsMind DB
- No quantum-specific data structures exist or are justified by the engine today.
- **Do not introduce quantum functionality** into the DB merely because MicroQuantum exists. If ever needed, treat it as an application data type via the future document/JSONB model — not as a core engine feature.

### Karkain ↔ QuantsMind DB
- No compiler/DSL integration exists. SQL already serves as the query DSL.
- **Do not make Karkain a dependency.** The DB remains independently useful, per its standalone state today (zero-runtime-dep kernel).

---

## 15. Architecture Dependency Graph (actual)

```
Frontend (React, src/) ── Tauri 2 ──> qmind-desktop ──> qmind-embed
qmind-server (PG wire) ─────────────────────────────> qmind-sql ─┬─> qmind-kernel
qmind-cli (TCP) ────────────────────────────────────────────────> (wire protocol)
qmind-sql: parser ─> executor ─> engine ─> codec ────────────────> qmind-kernel
qmind-kernel: (no deps) page → buffer/fs_store → btree/wal → mvcc/lock → recovery
             columnar ── column_delta ── column_reader (M8, self-contained)
```

### Violations / risks found
- **Circular deps:** none.
- **Frontend ↔ engine leak:** the legacy `src/lib/engine.ts` still imports PGlite and *is* the data layer for `App.tsx`; the only Rust bridge (`desktop.ts`) serves the minimal `QmindStudio`. Two parallel data layers exist in the UI — a migration debt, not a kernel debt.
- **Generated artifacts tracked:** `src-tauri/gen/schemas/*`, `.bolt/*` are tooling artifacts inside the repo.
- **No layer violations in the Rust core:** storage does not touch UI; SQL does not touch storage internals.

---

## 16. Performance / Scalability Baseline

Measured facts already in the repo (from M7 release runs, not re-run): B+Tree insert **3.56M elem/s** (contract ≥500K), WAL **2.5M rec/s** (contract ≥1M). The repo contains criterion benches (`kernel_bench.rs`, `sql_bench.rs`) — **not run during recon to keep it read-only; numbers are documented in repo history/README**.

### Qualitative assessment
- **Serialization:** row codec is stack-simple; no reflection; good.
- **Disk I/O:** group-commit WAL minimizes syscalls; full-page CRC on load is a read amplification cost.
- **Algorithmic:** B+Tree log_access; hash join; aggregate passes; all sane.
- **Memory:** buffer pool with clock sweep; no out-of-core limits configured by default.
- **Concurrency limitation:** single mutex → writer-serialized, one process only. Reader parallelism is not exploited (no snapshot-aware read concurrency at the SQL facade).
- **OLAP:** columnar path built but not yet benchmarked; vectorized scan contract (≥50M rows/s) **not measured**; TPC-H numbers **not measured**.
- **Startup:** no measurement.

---

## 17. Security / Reliability Baseline

- **No `unsafe`** in the engine crates (safe Rust storage core) — good.
- **No deserialization of untrusted input** (no serde-based loaders; formats are hand-parsed with length/CRC checks).
- **Path traversal:** server/desktop read/write only `./qmind-data` (fixed relative dir) — low risk but no path sandboxing.
- **Auth on wire:** **trust auth only** (comment in `main.rs`). No username/password, no SCRAM, no TLS. Fine for dev; must not be labeled production-secure.
- **Injection:** SQL is parsed by the engine itself; no string-built queries reach a remote DB; injection surface minimal.
- **Secrets:** none handled.
- **Grant/permission model:** none — any connected client can do anything in this single-user local engine. Acceptable for embedded/desktop, needs a boundary statement for the server.

---

## 18. Documentation Reality Check

| Documented claim | Implemented | Tested | Accurate? |
|---|---|---|---|
| "sqlparser-rs, do not hand-roll parser" (ARCHITECTURE.md:115, ROADMAP:43) | **No — handwritten parser ships** | ✓ | **Stale — should say handwritten parser (decision superceded by E5)** |
| M5: "a done — b pending" (server + CLI) | Server+CLI functional; SCRAM/extended protocol not built | 3 e2e | Partially stale — core done; auth/extended pending |
| M6: "a scaffold done" (Desktop Studio) | Tauri shell + run_sql works | 2 embed tests | Accurate — shell only |
| M7: ⬜ (hardening) | **Done** — fuzz, 72h-ish soak test, benchmarks, packaging, v0.1.0 release | ✓ | **Stale — mark done** |
| M8: ⬜ (columnar replica) | **Done** — columnar.rs, column_delta.rs, column_reader.rs; engine integration in-flight (uncommitted M9) | 20 tests | **Stale — mark ~done (integration pending)** |
| "Persistent columnar replica arrives later (M8)" | Exists | ✓ | Accurate |
| Performance contract §1.1 (bulk insert/point scan/vectorized scan/TPC-H) | B+Tree & WAL met; **vectorized scan and TPC-H targets not measured** | part | **Overclaim risks: §1.1 is a target table; enforcement incomplete** |
| README "production-grade" / HTAP | Architecture real; maturity = developer/preview | — | **Overclaim — mark Experimental/Developer Preview** |
| INSTALL.md / ARCHITECTURE_REPORT.md | Match actual installers/schema | — | Accurate |

**Definition adopted for the project going forward (honest status language):** the `develop` branch is a **Developer Preview / Experimental** engine. `v0.1.0` was an early release tag.

---

## 19. Existing Strengths

- **Zero-dependency, safe-Rust storage kernel** — rare and architecturally clean; easy to reason about and port.
- **Real, tested crash semantics** — WAL CRC, group commit, torn-tail replay, committed-prefix guarantees, restart round-trip tests.
- **Handwritten parser + Volcano executor with fuzz harness** — 8K-input parser fuzzing already wired.
- **CI gates** — fmt + clippy -D warnings + tests + bench-compile on Linux and Windows.
- **Package/tag discipline** — milestone-per-commit history, `v0.1.0` tag, cross-platform install scripts with SHA256SUMS.
- **Columnar M8 design** — format versioning (D-003), three encodings, delta applier pattern aligns with the HTAP direction.
- **Layered kernel (D-002)** — future Document/KV models are real options, not rewrites.
- **Small, reviewable codebase** (~7.2K source lines).

Do not rewrite these.

---

## 20. Problems / Technical Debt

### P0 — Blocking
1. **`cargo clippy -D warnings` fails** — `crates/qmind-sql/src/engine.rs:243:64` "redundant closure" in the uncommitted M9 code. CI would fail today on this branch.
   - Impact: breaks the CI gate; M9 cannot merge cleanly.
   - Action: replace `map(|sv| sql_value_to_col_value(sv))` with the fn pointer; re-run clippy before committing M9.
2. **(Procedural) uncommitted milestone work** — `engine.rs` + `sql_e2e.rs` modified on `develop`.

   - Impact: working-tree/branch drift; no checkpoint exists.
   - Action: land M9 (after clippy fix) as its own commit.

### P1 — Important
3. **Docs drift** — ARCHITECTURE.md/ROADMAP.md still claim `sqlparser-rs`; M5/M6/M7/M8 statuses stale; performance-contract table incomplete (vectorized scan, TPC-H unmeasured). Impact: wrong guidance for the next phase. Action: update to honest status language (Experimental/Partial), document the handwritten-parser decision, mark measured vs planned targets.
4. **Single-writer mutex** (`Arc<Mutex<Engine>>` everywhere). Impact: no read-scale; blocks later HTAP story. Action: snapshot-aware read concurrency (MVCC reads don't need the writer lock) in a later phase; document today.
5. **Secondary indexes absent.** ROADMAP M4 remainder; joins/point queries rely on full scans/PK. Impact: subset of real OLTP. Action: schedule `CREATE INDEX` + maintenance.
6. **No license field / LICENSE file.** Impact: public repo legal ambiguity. Action: add a license (project-level decision).

### P2 — Improvement
7. **No package.json brand/version** (still `vite-react-typescript-starter`). Impact: branding debt in the Desktop Studio. Action: rename to `quantsmind-studio` 0.1.0.
8. **Legacy PGlite prototype still mounted in `src/`** with `@electric-sql/pglite` + `@supabase/supabase-js` deps. Impact: two engines behind the UI; confused data layer. Action: decide migrate-vs-drop; then prune deps.
9. **Tracked tooling artifacts** (`.bolt/*`, `src-tauri/gen/schemas/*`). Impact: noise. Action: add to `.gitignore`/`.gitattributes` as appropriate (decision needed).
10. **Server is trust-auth only, no TLS.** Impact: not production-safe. Action: at minimum document; SCRAM/TLS behind a "server hardening" phase.

### P3 — Future
11. **No ORDER BY / sort operator; no expression engine; optimizer absent.** Planned phases.
12. **No import/export/snapshot tooling** — needed for a usable release.
13. **Columnar performance unbenchmarked** and **non-vectorized** (batched by rows, not SIMD). M8 contract (≥50M rows/s) pending.
14. **Multi-process / multi-database-instance model** absent by design (single data dir currently).

---

## 21. What Should NOT Be Built

Justified by the current architecture and ecosystem boundary (§14):

- **Quantum computing engine** — belongs to MicroQuantum; a DB must not host it.
- **Financial-domain logic / fraud detection / medical logic / trading logic** — domain logic belongs in the SDK/application layer, not the DB.
- **Hosted SaaS / cloud control plane** — this is an embeddable-ish engine; cloud is a packaging/service concern, not engine scope.
- **Karkain compiler/runtime integration** — SQL is the query DSL; keep Karkain non-dependent.
- **An AI/ML training layer** — out of scope; the DB serves data.
- **A second storage engine** — the layered kernel (D-002) is the extension mechanism; do not fork it.

---

## 22. Proposed QuantsMind DB Boundary

> **"QuantsMind DB is a self-contained, embeddable relational database engine in Rust — a safe, dependency-light storage kernel (KV + B+Tree + MVCC + WAL) with a handwritten SQL layer, an OLAP columnar read path, and optional Postgres-wire server / desktop-shell frontends."**

- **Core responsibility:** durable, transactional relational storage with correctness guarantees (ACID at single-process scope), SQL, and HTAP read paths.
- **Non-responsibilities:** domain/business logic, quantum computing, machine learning, cloud hosting, authentication infrastructure beyond basic wire auth.
- **Primary users:** the QuantsMind Desktop Studio, embedding applications (via `qmind-embed`), and wire clients using psql semantics.
- **Primary use cases:** local/embedded OLTP + OLAP data management; embeddable DB for desktop/edge tools.
- **Storage model:** versioned pages + B+Tree row store; WAL; persistent columnar segments (M8) for analytics.
- **Query model:** SQL subset (DDL/DML/SELECT/filter/project/join/aggregate); plan-lite execution; columnar path for scans.
- **API model:** Rust library (`Engine`), JSON embedding API (`qmind-embed`), PG wire v3 (simple query), CLI shell.
- **Extension model:** kernel-layered (D-002) — future Document/KV models plug in as new model layers; encodings are additive in the columnar format.
- **Relationship with QuantsMind SDK:** **independent.** Any SDK integration is a thin adapter over `qmind-embed`/wire — never duplicate engine code in the SDK.

---

## 23. Proposed Target Architecture

Proposed module map (Rust, from repo evidence — **not implemented**):

```
crates/
  qmind-kernel/          storage kernel (KEEP, near-frozen)
    page, buffer, fs_store, btree, wal, mvcc, lock, recovery, eviction, error
    columnar, column_delta, column_reader        # M8 stable
  qmind-sql/             SQL layer (KEEP + finish M9)
    parser (handwritten), executor (Volcano + plan-lite), engine (route OLTP/OLAP),
    codec, [future: sort/expr, secondary-index]
  qmind-server/          wire listener (HARDEN later: SCRAM, extended protocol)
  qmind-cli / qmind-embed/  today's facades (KEEP)
  [proposed] qmind-adapters/  SDK/foreign bindings (thin, NOT engine forks)
  [proposed] qmind-io/        import/export/snapshot (CSV/JSONL/backup)
```

Design rules:
- **Dependency direction:** frontends → `qmind-sql` → `qmind-kernel` → nothing. Columnar stays inside kernel or becomes its own `qmind-columnar` crate *if* it grows; currently fine inside kernel.
- **Public/private:** the stable public surface is `qmind-embed` + `qmind-sql::Engine`; kernel internals promoted to public API only when needed by consumers.

Even so: **do not restructure for its own sake** — the current layout already realizes most of this. The next phase is about finishing in-flight work and hardening, not re-layering.

---

## 24. Recommended Development Phases

Proposed (adapted to actual repo state; roadmapped from the existing milestone numbering):

| Phase | Objective | Modules affected | Tests required | Acceptance criteria | Depends on | Risks |
|---|---|---|---|---|---|---|
| DB-001 | Land M9 columnar integration | `qmind-sql/src/engine.rs`, `sql_e2e.rs` | existing + 3 columnar e2e | clippy clean, 108+ tests green, commit+merge | M8 stable | clippy error today (P0-1) |
| DB-002 | Docs truth reset | `ARCHITECTURE.md`, `ROADMAP.md`, `README.md`, `LICENSE` | — | honest status language; measured vs planned split | — | low |
| DB-003 | Hardening sweep | `engine.rs` routing, `wire.rs` | wire auth test, multi-conn | soak + fuzz green on new code | DB-001 | medium |
| DB-004 | Server hardening (auth/TLS, extended protocol) | `wire.rs`, `main.rs` | auth tests | SCRAM at minimum; documented | DB-003 | high (wire is intricate) |
| DB-005 | Secondary indexes | `btree.rs`/executor catalog | index e2e + perf | point lookups use index; no scan fallback | M4 core | medium |
| DB-006 | Sort + expression engine | `executor.rs`, new expr module | sort/expr e2e | ORDER BY + richer predicates | — | medium |
| DB-007 | Columnar perf (vec scans, TPC-H Q1/Q6) | `column_reader.rs`, engine batch | criterion targets | ≥50M rows/s contract; TPC-H ≤5× DuckDB | DB-001 | high (perf engineering) |
| DB-008 | Import/export + snapshots | new `qmind-io` | CSV/JSONL round-trip, backup/restore | backup → restore equality | DB-003 | medium |
| DB-009 | Read concurrency (snapshot reads lock-free) | `engine.rs`, `mvcc.rs` | multi-reader stress | concurrent reads scale; writers remain safe | DB-006 | high |
| DB-010 | Developer Preview release | packaging, version bump | full gates | tag + installers on all platforms; honest "Developer Preview" labeling | DB-001..3 | low |

Most of DB-001 and DB-002 are <1 day each given the current state. DB-003–DB-010 are the substantive arc toward a reviewable preview release.

---

## 25. MVP Definition

**Smallest useful QuantsMind DB release (Developer Preview v0.2.0):**

### MUST work
- `Engine` DDL/DML + SELECT (filter/project/join/aggregate) with ACID single-process guarantees and restart recovery.
- Columnar OLAP read path end-to-end from SQL (M9 landed, routed by threshold).
- PG-wire simple-query for psql-type clients (documented as trust-auth/dev only).
- Install via `packaging/` on Windows/Linux/macOS; `qmind-server` + `qmind-cli` runnable.
- Full gate: fmt, clippy -D warnings, 108+ tests, bench-compile on CI.
- Honest documentation (Experimental / Developer Preview), LICENSE present, ROADMAP current.

### MAY be experimental
- Columnar flush scheduling / apply lag; raw RLE/dict encodings.
- Desktop Studio UI wiring (only `QmindStudio` eval shell guarantees a working loop).
- Server without auth/TLS (explicitly "dev only").

### MUST NOT be included
- SCRAM/TLS (unless DB-004 lands first), secondary indexes, sort, import/export, read-concurrency — all phase-locked after MVP.
- Domain/quantum/ML features (they're out of scope permanently, §21).

### What makes the first release genuinely useful
- **A single-file "install → `qmind-server` → psql" demo** plus embedded use via `qmind-embed` from the Desktop Studio: realistic local OLTP+OLAP, recoverable, packaged.

---

## 26. Final Recommendation

## Current State
`develop` @ `299f238` with **uncommitted M9 columnar-engine integration**. 108 tests pass; **1 clippy violation** in the uncommitted code blocks the CI gate. `main` @ `4f434bc`, tag `v0.1.0`.

## What We Have
- A real, safe-Rust, zero-dependency storage kernel (pages/buffer/B+Tree/WAL/MVCC/recovery) with tested crash semantics.
- A handwritten SQL parser + Volcano executor (DDL/DML/filter/project/join/aggregate) with fuzz and soak suites.
- M8 persistent columnar segments (Raw/Dict/RLE) + reader, integration mid-flight.
- PG-wire server (simple query, trust auth), CLI, embed JSON API, Tauri shell, cross-platform installers, v0.1.0 release history.

## What Is Missing
- Clean landing of M9 (clippy-green merge). Docs truth reset. Secondary indexes. Sort/expressions. Auth/TLS. Columnar *and* vectorized performance proofs (TPC-H, ≥50M rows/s). Import/export/backup. Read concurrency. A declared license. Honest status labeling.

## What Should Be Reused
- The whole `qmind-kernel`; the handwritten parser; Volcano executor; MVCC/recovery; columnar M8 design; CI gates; packaging.

## What Should Be Redesigned
- Nothing structural. **Finish, don't rewrite.** The two changes to make are *communication-level*: (a) single-writer → snapshot-aware read concurrency, (b) server auth model + explicit dev/prod boundary. Both are phased, not blocking MVP.

## What Should Be Removed/Deferred
- Legacy PGlite frontend data layer (or rewire) + `@electric-sql/pglite`/`@supabase/supabase-js` deps; `.bolt/*` tracking; boilerplate package.json naming; stale doc claims; un-claimed license gap.

## Recommended Architecture
Keep the 4-crate layered workspace + excluded `src-tauri`. Extend only where evidence demands: an `io`/adapter surface for import/export and a clean OLTP/OLAP routing seam in `engine.rs` (already started by M9).

## Recommended MVP
Developer Preview v0.2.0 per §25: M9 landed cleanly → docs truth reset + LICENSE → full gate green → packaged install with an honest "Experimental / Developer Preview" label. That is a genuinely useful, honest first release.

## Recommended First Implementation Phase
**DB-001 (Land M9):** fix the redundant-closure clippy error at `engine.rs:243`, re-run `cargo test` + `cargo clippy -D warnings`, commit the M9 columnar integration, merge to `main`, push. One focused PR-sized step; everything later builds on it.

## Estimated Complexity
- DB-001 (land M9): **~low** (minutes of work; it's ready).
- DB-002 (docs reset): **~low**.
- DB-003..DB-005 (hardening, auth, secondary indexes): **medium** each.
- DB-007 (columnar perf to contract): **high** — genuine systems engineering the repo has not attempted yet.
- Overall to a marketed "Production" label: **high** and depends on evidence (TPC-H, soak, scale), not just code.

---

<p align="center"><i>END OF RECONNAISSANCE REPORT — inspection-only; working tree left as found (M9 uncommitted).</i></p>