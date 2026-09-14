# QuantsMind Engine — Roadmap

> Status: **v0.1 Developer Preview (Experimental)** · updated 2026-09
> Exit criteria are enforced by tests/benches in CI. Companion: [ARCHITECTURE.md](./ARCHITECTURE.md).
> Honest status language used throughout: ✅ done / 🟡 partial / ⬜ planned.

## Milestone status (accurate as of 2026-09)

| Milestone | Theme | Status |
|---|---|---|
| **M0** | Foundations: workspace, CI, docs, bench harness | ✅ done |
| **M1** | Storage kernel: pages, buffer pool, B+Tree, WAL (group commit), file store | ✅ done |
| **M2** | Transactions: MVCC snapshots, recovery replay | ✅ done |
| **M3** | SQL core: parse → DDL/DML → filter/limit over MVCC | ✅ done |
| **M4** | Relational: GROUP BY, aggregates, hash INNER JOIN | ✅ done |
| **M5** | PG wire server + CLI shell (trust auth, simple Query) | 🟡 core done — auth/extended protocol pending |
| **M6** | Desktop Studio GUI (Tauri 2) | 🟡 shell done — rich UI not yet rewired to Rust engine |
| **M7** | Production hardening: fuzzing, soak, perf valley, packaging, v0.1.0 | ✅ done |
| **M8** | Persistent columnar replica (full HTAP storage split) | ✅ done |
| **M9** | Columnar integration into SQL engine (delta capture + routed reads) | ✅ done |

## Current state

- 132 tests green (71 kernel [62 unit + 6 property + 3 integration] + 58 sql [30 unit + 4 fuzz + 1 soak + 23 e2e] + 2 embed + 1 wire).
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` all green on CI (Ubuntu + Windows).
- Performance measured: B+Tree insert 3.56M elem/s, WAL 2.5M rec/s (release).
- **Status: Developer Preview / Experimental.** Honest gaps in ARCHITECTURE.md §1.1, §4, §5, §6.

## Known limitations (documented, not bugs)

- SQL subset: no PRIMARY KEY modifier, no INSERT column-lists, no subqueries,
  no UPDATE/DELETE.
- Secondary indexes are single-column and exclude NULL; the planner uses them
  for equality point lookups (range scans still go through the full scan).
- JOIN projections are plain columns only (no aggregates over joins, no JOIN+GROUP BY).
- Single-writer engine mutex; no mult-writer concurrency.
- Server: trust auth only, no TLS, simple Query only.
- GUI: rich components still bound to a PGlite prototype, not the Rust engine.

## Next phases (toward production-grade)

| Phase | Objective | Track | Exit criteria |
|---|---|---|---|
| **P2** | Docs truth reset + license + branding | Product | repo claims match reality; LICENSE present; honest status | ✅ |
| **P3** | Correctness hardening | Core | property/fuzz for B+Tree, MVCC, WAL; kill-9 chaos; zero committed-txn loss | ✅ |
| **P4** | Query surface | Core | secondary indexes, ORDER BY/sort, expression engine, richer predicates | ✅ |
| **P5** | Concurrency | Core | snapshot reads lock-free; multi-writer groundwork; read stress tests |
| **P6** | Server hardening | Product | SCRAM auth, TLS, extended protocol; psql/DBeaver compat tests |
| **P7** | HTAP perf contract | Perf | vectorized scans ≥50M rows/s; TPC-H Q1/Q6 ≤5× DuckDB; 72h soak |
| **P8** | Operational product | Ops | logging/metrics; CSV/JSONL import-export; backup/restore; resource limits |
| **P9** | Format & migration | Ops | versioned on-disk upgrade path; documented compatibility policy |
| **P10** | Developer Preview v0.2 | Product | everything above packaged and documented; honest preview label |
| **P11** | Production hardening | Product | conformance suite; security audit; semver/deprecation policy; signed releases |
| **P12** | 1.0 GA | Product | sustained evidence: 72h soak on CI, perf contracts enforced, upgrade path, docs complete |

## Multi-model outlook (post-1.0, additive per D-002)

- **Document model**: JSONB semantics as a model layer over the same kernel
  (reuses B+Tree/MVCC/WAL/columnar). ~2 phases after relational 1.0.
- **Key-Value model**: public KV API over the kernel. ~1–2 phases.
- No rewrite expected: the layered kernel is the extension mechanism.

## Milestone details (completed work, for the record)

### M0 — Foundations ✅
Workspace (4 crates + `src-tauri` excluded), architecture + roadmap docs,
CI gates (fmt, clippy `-D warnings`, test, bench-compile), criterion harness.

### M1 — Storage kernel ✅
Versioned CRC pages (8 KiB), clock-sweep buffer pool with write-back eviction,
B+Tree (insert/get/get_all/range-scan, duplicates, differential test), WAL
(CRC frames, group commit, torn-tail replay), file-backed page store.
B+Tree insert 3.56M elem/s · WAL 2.5M rec/s.

### M2 — Transactions & recovery ✅
Snapshot isolation, first-committer-wins, lock table, checkpoint + redo/undo
replay, restart round-trip tests.

### M3 — SQL core ✅
Handwritten tokenizer + recursive-descent parser (superseded the sqlparser-rs
plan — see ARCHITECTURE §4), DDL/DML/SELECT over MVCC, filter/project/limit.

### M4 — Relational ✅
GROUP BY + aggregates (COUNT/SUM/AVG/MIN/MAX), hash INNER JOIN. Remaining:
secondary indexes, JOIN+GROUP BY support.

### M5 — Server + CLI 🟡
PG wire v3 simple Query over blocking threads (trust auth, TEXT values);
minimal psql-like CLI REPL. Remaining: SCRAM/TLS, extended protocol.

### M6 — Desktop Studio 🟡
Tauri 2 shell with `run_sql` command over an embedded engine; minimal
QmindStudio eval shell wiring. Remaining: rewire the rich React UI from the
PGlite prototype to the Rust engine; icons/installers polish.

### M7 — Hardening + release ✅
Parser fuzz harness (8K inputs), 10K-row soak test, criterion benches,
cross-platform installers (linux/macos/windows/freebsd + brew + scoop),
SHA256SUMS on tag release, v0.1.0.

### M8 — Columnar replica ✅
QMINDCOL persistent segments (magic+version+CRC), Raw/Dict/RLE encodings,
delta buffer + schema-aware DeltaApplier with LSN markers, ColumnarReader
(all/filtered/projected scans).

### M9 — Columnar engine integration ✅
`Engine::with_columnar()`, insert capture to delta, threshold-based flush,
columnar read routing when columnar data exists, 3 new e2e tests.

## Phase notes (recent completed work)

### P2 — Docs truth reset + license + branding ✅
MIT LICENSE added (workspace + per-crate), README/ARCHITECTURE/ROADMAP rewritten
with honest status, test counts and feature claims corrected, studio package
renamed/versioned (`quantsmind-studio` 0.1.0).

### P3 — Correctness hardening ✅
Deterministic property harness `crates/qmind-kernel/tests/correctness.rs`
(zero external deps, splitmix64-seeded so every run is reproducible):
- **B+Tree differential**: 4K random upserts against an in-memory oracle —
  full-order scan, `get_all`, `len`, lower-bounded scans all agree.
- **WAL committed-prefix**: exhaustive truncation at *every* byte of a
  multi-group log — replay recovers exactly the frames fully contained, with
  `torn_tail` reported consistently at frame boundaries vs mid-frame tears.
- **WAL corruption**: 400 single-byte flips (plus multi-flip path) — every
  corruption is detected (torn tail or `WalCorrupt`), never a phantom record.
- **MVCC serial history**: 1200 interleaved beginning/commit/abort steps with
  overlapping writers, first-committer-wins cross-checked against a watermark
  model, plus a long-lived reader that must keep its frozen snapshot view.
- **Zero committed-txn loss**: 300 MVCC commits through the WAL, recovered
  state compared at every group durability point + 250 random tears — redo
  state equals the surviving committed writers, and a torn group can never
  report its txn as committed.

Found and fixed a real kernel bug: splitting a leaf that had filled with a
single duplicate key panicked (index out of bounds) and the raw-cut fallback
would have silently broken `get_all`. The split now lets such a leaf overfill
instead; a new unit regression test pins the behavior.

`cargo test --workspace` green (115), `cargo fmt --check` and
`cargo clippy -D warnings` green.

### P4 — Query surface ✅
Shipped as one commit covering three work-streams: **P4a** expression engine +
richer predicates, **P4b** ORDER BY/sort, **P4c** secondary indexes.

- **Expression engine**: nested arithmetic (`+ - * / %`) with overflow and
  division-by-zero errors, three-valued logic (SQL NULL: AND/OR/NOT/KNOWN, 3VL
  comparisons via `try_cmp`), complete precedence  (OR < AND < comparison <
  additive < multiplicative < unary, prefix `NOT`, parens).
- **Richer predicates**: col-vs-col comparisons, `LIKE` (`%`/`_`, case-sensitive),
  `IN (…)` list with NULL semantics, `BETWEEN … AND …`, case-insensitive
  keywords, negation forms `NOT a = 1`, `a NOT LIKE b`, `a NOT IN (…)`,
  `a NOT BETWEEN x AND y`.
- **Scalar functions**: `UPPER`, `LOWER`, `LENGTH`; expressions are legal in
  projections and ORDER BY (e.g. `SELECT a + 1 FROM t ORDER BY a * 2`).
- **ORDER BY/sort**: `Sort` executor operator (stable, materializing), PG null
  semantics (NULL largest → ASC nulls-last, DESC nulls-first), multiple keys,
  ORDER BY with LIMIT, on the row path, columnar path, JOINs, and behind GROUP BY
  (resolved against post-aggregation output columns).
- **Secondary indexes**: in-memory per-table index trees over the kernel B+Tree;
  `CREATE INDEX idx ON t (col)` / `DROP INDEX idx`; single-column, NULL-excluded;
  backfilled at CREATE from existing rows, maintained on every INSERT. The
  planner serves `col = literal` point lookups from the index (leaving the rest
  of the WHERE clause as a residual filter); range predicates still full-scan by
  design. Zero-runtime-dep: index keys use order-preserving encoding
  (sign-flipped big-endian INT / length-prefixed TEXT).
- Anecdote enforced by tests: 4 new e2e index tests (backfill + lookup, insert
  maintenance + NULL skipping, residual-predicate filtering, DDL errors/drop),
  6 new query-surface e2e tests, 1 new executor unit test (sort null ordering),
  6 new parser unit tests.

`cargo test --workspace` green (132), `cargo fmt --check` and
`cargo clippy -D warnings` green.