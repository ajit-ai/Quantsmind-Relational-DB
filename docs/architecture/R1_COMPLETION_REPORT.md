# R1 Completion Report

> R1 deliverable — `docs/architecture/R1_COMPLETION_REPORT.md`
>
> Final report for **R1 — Architecture Realignment**. Evidence-based;
> every classification cites the assessment (`CURRENT_ENGINE_ASSESSMENT.md`),
> which cites source files and lines. No code was rewritten during R1.

---

## 1. Product target

A production-grade, general-purpose relational **HTAP** database capable of
reliably operating on **billion-row** datasets on a single machine, with a
future path to larger-scale/distributed deployment (`ADR-001`). Priorities:
durability > correctness > measured performance > feature breadth.

## 2. Current architecture (what exists)

- A Rust workspace (`Cargo.toml`: `crates/*`, `src-tauri` excluded;
  `profile.release` = LTO thin, `panic = abort`, `codegen-units 1`).
- **`qmind-kernel`**: 8 KiB CRC pages (`page.rs`), buffer pool
  (`buffer.rs`, `eviction.rs`, `fs_store.rs`), in-memory arena B+Tree
  (`btree.rs`), in-memory MVCC store (`mvcc.rs:100`, `Snapshot{read_ts}`
  `mvcc.rs:20-22`), WAL record log with torn-tail handling (`wal.rs`),
  pure-function recovery (`recovery.rs:39`), experimental columnar segments +
  RLE + delta applier (`columnar.rs`, `column_delta.rs`, `column_reader.rs`).
- **`qmind-sql`**: tokenizer + recursive-descent parser (`parser.rs`),
  engine with `execute(&mut self)` / `execute_read(&self)` and single
  statement enforcement (`engine.rs:158-208`), Volcano row-at-a-time executor
  (`executor.rs`: `VecScan/Filter/Project/Limit/Sort/HashJoin/HashAggregate`),
  M9 columnar HTAP read path (`engine.rs:90-142,396-398`).
- **`qmind-server`**: minimal PGv3 listener, trust auth, simple `Q` only,
  TEXT OID 25, thread-per-connection, `Arc<RwLock<Engine>>` with SELECT/SHOW
  routed to the read guard (`wire.rs`).
- **`qmind-embed`**: one-method JSON facade over `Engine` (`lib.rs`).
- **`qmind-cli`**: psql-like REPL over the wire protocol (`main.rs`).
- **Studio**: Tauri + React + TS shell whose SQL runs **in-browser on PGlite**
  (`src/lib/engine.ts:1,79`), not the engine.
- Tests: 143 green across kernel/sql/embed/wire; CI gates on
  `ubuntu-latest` + `windows-latest` (`fmt`, `clippy -D warnings`,
  `test --workspace`, `check --benches`).

## 3. Major findings

1. **No working restart-persistence path.** Catalog, row store, and indexes
   are in-memory (`engine.rs:243-244`, `mvcc.rs:100`); the WAL is written but
   never replayed (`recovery.rs:39` is test-only), and the WAL commit path
   issues no `sync_all` (`wal.rs:237`). A restart returns an empty database.
2. **Single writer.** Server serializes writes behind a whole-engine write
   guard (`wire.rs:117`); no multi-writer MVCC/conflict detection yet. Reads
   are statement-scoped snapshots (`engine.rs:194`) — structurally sound.
3. **No binder/planner/optimizer.** SELECT is executed from a plan close to
   the AST; no cost model, no pushdown decisions (`executor.rs`).
4. **Studio is a second database implementation** via PGlite
   (`src/App.tsx:609,634`) — decoupled from the engine.
5. **Columnar HTAP exists but is off by default** (`engine.rs:90`
   `with_columnar` only).
6. **Documentation/source discrepancies recorded** (assessment §8): claimed
   persistence vs actual, claimed Studio connectivity vs actual, claimed
   HTAP availability vs actual.

## 4. KEEP decisions

`page format + CRC`, `column store (HTAP lane)`, `MVCC snapshot design`,
`torn-write/CRC approach`, `parser/tokenizer/AST`, `Volcano operator
framework` (HashJoin/HashAggregate/Sort/VecScan…), `SELECT/INSERT/JOIN/GROUP/
ORDER/LIMIT` surface, `expression evaluation` (surface), `embedded API
boundary`, `CLI`, `React shell as replaceable client`.

## 5. REWORK decisions

`page manager`, `buffer pool + eviction`, `row store (→ on-disk)`,
`catalog/metadata (→ durable + DDL journal)`, `indexes (→ on-disk + UNIQUE)`,
`txn manager lifecycle (durability/GC)`, `isolation (multi-writer)`,
`WAL (fsync + replay + segments)`, `fsync/flush discipline`, `recovery
pipeline (wire in)`, `checkpoints (R2)`, `PG wire protocol (auth/TLS/
extended)`, `batch execution + vectorized`, `NULL semantics`.

## 6. REPLACE decisions

`binder + semantic analysis` (absent → build), `planner/optimizer`
(absent → build), `UPDATE/DELETE/subqueries/CTE/window/prepared` (absent →
build in later stages).

## 7. REMOVE decisions

`Tauri as engine dependency` (engine never depends on it), `PGlite from the
final Studio architecture` (temporary dev client today), `second database
implementations` (per product rule: one engine only).

## 8. DEFER decisions

`free-space management` (R3), `partitioning` (R5), `lock manager/deadlock
detection` (R4), `parallel execution` (R3/R7), `vectorized execution`
(R3), `projection/partition pruning` (R5), `spill-to-disk` (R3).

## 9. Rust decision

**KEEP** — evidence-based (`TECHNOLOGY_DECISION.md`): safe ownership model for
a page-cache/MVCC engine, zero-cost abstractions, first-class concurrency and
SIMD ecosystem, mature storage-engine patterns, 143 green tests + clean
clippy/fmt, release profile already tuned; alternatives (C/C++/Go/others) 
require a full rewrite to give up safety or add GC latency. `ADR-002`.

## 10. Tauri decision

**CLIENT-ONLY (non-core)** — Tauri is excluded from the engine workspace
(`Cargo.toml` `exclude = ["src-tauri"]`); zero engine dependency. Desktop
packaging stays undecided until engine/server stabilize (R8). `ADR-004`,
`STUDIO_ARCHITECTURE.md`.

## 11. PGlite decision

**REMOVE** — must not remain a second DB implementation in the final Studio.
Today it is a temporary development client (`src/lib/engine.ts`); removed at
R8. `ADR-004`.

## 12. Storage strategy

Coordinated row + column architecture on a single MVCC timeline
(`ADR-005`): on-disk slotted row store + real buffer/write-behind (R3),
durable WAL/recovery/catalog (R2), columnar hardened with pushdown (R4).
Order matters: **durability (R2) before scale (R3)**.

## 13. Execution strategy

Keep Volcano operator framework; evolve to batch/vectorized `Chunk`
interface (R3), spill + memory budgets (R3), parallel operators (R3/R7),
planner-driven physical plans (R5). `ADR-006`.

## 14. Billion-row strategy

`BILLION_ROW_REQUIREMENTS.md` defines measurable acceptance criteria at
1M/10M/100M/1B across storage, query, OLTP, HTAP, reliability, and resource
management — with **no invented numbers**; `benchmarks/` provides the
workloads and results pipeline; R6 qualifies S4, R9 reruns, R10 publishes the
GA report.

## 15. Server strategy

Broker boundary (`ADR-003`): extended PGv3 protocol, SCRAM + TLS, prepared
statements, session/resource management, observability (R7); server stops
owning engine-level locks when a session/query API lands (R4).

## 16. Studio strategy

Pure replaceable client of the engine (embed or server); PGlite deleted;
no SQL semantics in the browser; packaging decided later. `ADR-004`,
`STUDIO_ARCHITECTURE.md`.

## 17. R2 implementation priorities

1. WAL durability (fsync discipline, segment rotation) + replay on open.
2. Durable catalog + DDL journaling.
3. Recovery pipeline (open → replay → rebuild → checkpoint) + checkpoints
   with WAL truncation.
4. Crash-restart end-to-end tests (`kill → restart → data intact`).
5. Begin moving row store/indexes to disk and buffer pool to managed eviction.

(`PRODUCTION_ROADMAP.md` R2 acceptance gate: kill-tests always restore last
committed state; DDL survives restart; recovery time bounded at S2.)

## 18. Risks

- **Durability gap is the dominant risk**: until R2 lands, any crash loses
  all data; this must be treated as the current product's hard stop for
  production claims.
- **WAL fsync semantics on Windows vs Linux** must be validated empirically.
- **Single-writer ceiling** caps OLTP throughput until R4 multi-writer.
- **Studio/PGlite divergence risk** — GUI may appear to "work" with semantics
  the engine lacks; mitigated by R8 migration and by tests on engine paths.
- **Columnar RLE-null quirk** (flag from inspection) needs a dedicated
  correctness test in R2 before HTAP is trusted.
- **Benchmark discipline**: no results until real runs; R6 hinges on the S4
  generation pipeline existing.

## 19. Tests executed

```text
cargo fmt --all -- --check                          PASS
cargo test --workspace                              PASS  (143 passed; 0 failed)
cargo clippy --workspace --all-targets              PASS  (repo CI variant)
cargo clippy --workspace --all-targets --all-features -- -D warnings   PASS  (R1 spec variant)
sphinx-build -W -b html docs docs\_build\html       PASS  (0 warnings)
```

Breakdown of the 143: `qmind-kernel` 77 (62 unit + 6 property + 3 integration
+ 6 read-stress), `qmind-sql` 62 (30 unit + 4 concurrency + 4 parser-fuzz + 1
soak + 23 e2e), `qmind-embed` 2, `qmind-server` 2 (wire e2e).

No existing test was weakened. No known failures.

## 20. Benchmark framework created

`benchmarks/` with `README.md`, `workloads/` (10 specifications:
`point_lookup`, `bulk_insert`, `filtered_scan`, `full_scan`, `aggregation`,
`join`, `sort`, `mixed_oltp`, `htap`, `recovery`), `datasets/` (canonical
synthetic schema + generator contract), `results/` (report template; **empty
until real runs**). Scales S1–S4 (1M/10M/100M/1B). No results fabricated;
S4 generation deferred to R3+ per spec.

## 21. Acceptance status

R1 Definition of Done — all items satisfied:

- repository inspected ✅ (workspace, all crates, `src`, `src-tauri`, docs,
  workflows, tests, benches)
- documentation vs implementation compared ✅ (verified against source;
  discrepancies ledgered, assessment §8)
- KEEP/REWORK/REPLACE/REMOVE/DEFER decisions ✅ (assessment §7)
- Rust explicit decision ✅ (`ADR-002`, Tech Decision)
- Tauri separated from core ✅ (`ADR-004`, boundaries)
- PGlite final status defined ✅ (REMOVE)
- engine/server/client boundaries documented ✅ (`ENGINE_BOUNDARIES.md`)
- target storage architecture ✅ (`TARGET_ARCHITECTURE.md`, `ADR-005`)
- target execution architecture ✅ (`TARGET_ARCHITECTURE.md`, `ADR-006`)
- billion-row requirements ✅ (`BILLION_ROW_REQUIREMENTS.md`)
- benchmark structure ✅ (`benchmarks/`)
- R2–R10 roadmap ✅ (`PRODUCTION_ROADMAP.md`)
- ADRs for major decisions ✅ (7 ADRs in `docs/architecture/adr/`)
- workspace tests pass ✅ (143/143)
- formatting passes ✅
- clippy passes ✅ (both CI and `--all-features` variants)
- no major engine rewrite ✅ (zero code changes; docs + scaffolding only)
- R1 completion report ✅ (this file)

---

## The R1 answer

> **Can the current architecture realistically evolve into a billion-row
> production database?**

**Yes — with a mandatory, well-scoped R2.** The current architecture has the
correct skeleton (CRC pages, buffer pool, commit_ts MVCC snapshots, WAL record
format with torn-tail protection, Volcano operators, a real columnar on-disk
path, PGv3 framing, snapshot reads). None of these primitives fundamentally
conflict with billion-row operation; they are incomplete, not wrong.

What must change for the claim to become defensible:

1. **R2 durability**: real fsync-disciplined WAL with replay + checkpoints;
   durable catalog + DDL journal; recovery wired into startup. Without this
   the product cannot honestly claim persistence at any size.
2. **R3 storage**: row store and indexes move from `HashMap`s to on-disk
   structures behind a real buffer manager with eviction and free-space
   management; batch (vectorized) execution with bounded memory + spill.
3. **R4 transactions**: multi-writer MVCC with conflict detection, GC,
   SQL `UPDATE/DELETE/BEGIN/COMMIT`; columnar predicate pushdown.
4. **R5 planning**: binder → optimizer → physical plans over catalog stats;
   partitioning.
5. **R6 qualification**: run the S4 envelope with the real workloads and
   publish results.

If that program were rejected (no willingness to fund R2's durability work),
the honest answer would instead be "**No** — the engine is an in-memory
prototype with a write-only WAL, and no amount of R5+/performance work makes
1B durable rows without it." The R1 recommendation is therefore to proceed
to R2 with durability as the first milestone and to treat the definitions of
done exactly as `PRODUCTION_ROADMAP.md` states.

```text
R1 STATUS: PASS WITH RISKS

Rust: KEEP
Tauri: CLIENT-ONLY (non-core; packaging undecided until R8)
PGlite: REMOVE (final Studio architecture; temporary dev client today)
Storage: REWORK (durable WAL/recovery/catalog R2 → on-disk row/index R3;
         column store KEEP as HTAP lane, harden R4)
Execution: KEEP (Volcano framework) + REWORK to batch/vectorized (R3)
SQL: KEEP (surface) / REPLACE (absent binder/planner/optimizer, UPDATE/DELETE,
     subqueries, prepared statements — later stages)
Server: REWORK (auth/TLS/extended protocol/sessions, R7)
Billion-row readiness: FOUNDATION LAID, NOT EARNED — requires R2 durability,
     R3 on-disk + batch, R4 multi-writer/HTAP, R5 planner, R6 qualification
     runs with published (never fabricated) benchmark results
R2 recommendation: proceed — durability first (WAL fsync + replay, durable
     catalog, recovery pipeline, crash-restart end-to-end tests), bounded
     recovery time at S2; keep R2 scoped exactly as PRODUCTION_ROADMAP.md
```

**R1 is an architecture gate and stops here. R2 implementation does not begin
automatically.**