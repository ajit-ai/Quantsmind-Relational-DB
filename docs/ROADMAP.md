# QuantsMind Engine — Roadmap

> Each milestone ships an independently usable artifact. Exit criteria are
> enforced by benchmarks/tests in CI. See [ARCHITECTURE.md](./ARCHITECTURE.md).

| Milestone | Theme | Status | Exit criteria |
|---|---|---|---|
| **M0** | Foundations: workspace, CI, docs, bench harness | ✅ done | `cargo test` + clippy green in CI; workspace builds on Win/Linux |
| **M1** | Storage kernel: pages, buffer pool, B+Tree, WAL (group commit) | ✅ done — file-backed store included | 2.49M inserts/s (contract ≥500K); full-page CRC; restart roundtrip tests |
| **M2** | Transactions: MVCC snapshots, recovery replay | ✅ core done | FCW + SI tested; WAL-integrated commit; committed-state reconstruction from log |
| **M3** | SQL core: parse → DDL/DML → filter/limit over MVCC | ✅ core done | 6 e2e SQL tests green; vectorized executor + sqllogictest remain (perf phase) |
| **M4** | Relational: GROUP BY, aggregates, hash INNER JOIN | ✅ core done | e2e join+grouping tests; secondary indexes + TPC-H bench remain |
| **M5** | PG wire server + CLI shell | 🔄 a done — b pending | TCP e2e test passes (2 clients, shared engine); scram auth + extended protocol + soak remain |
| **M6** | Desktop Studio GUI (Tauri 2) | 🔄 a scaffold done | src-tauri shell + run_sql command + typed TS bridge; UI wiring, icons, installers remain |
| **M7** | Production hardening: fuzzing, perf valley, packaging | ⬜ | 72h soak clean; all §1.1 performance contract targets met |
| **M8** | Persistent columnar replica (full HTAP storage split) | ⬜ | delta-apply lag bounded; OLAP scans read replica without blocking OLTP |

## Milestone details

### M0 — Foundations (complete)
- [x] Cargo workspace with 4 crates (`kernel`, `sql`, `server`, `cli`)
- [x] Architecture doc + roadmap (this file)
- [x] CI gates: fmt, clippy `-D warnings`, test, bench compile
- [x] Criterion harness wired in kernel
- [x] Kernel module skeletons with real type signatures (`page`, `buffer`, `btree`, `wal`, `mvcc`)

### M1 — Storage kernel (core complete)
- [x] Versioned CRC page headers (D-003), 8KiB pages
- [x] Clock-sweep buffer pool with write-back eviction, validate-on-load
- [x] B+Tree: insert/get/get_all/range-scan, run-preserving splits,
      duplicate `(key,value)` ordering, differential test vs BTreeMap
- [x] WAL: CRC frames, group commit (one syscall per group),
      torn-tail-safe replay, committed-prefix crash semantics
- [x] Benchmarks: 2.49M inserts/s · 6.1M WAL rec/s · 123ns page seal
- [ ] File-backed PageStore with segment directory layout
- [ ] Latch crabbing groundwork for concurrent descent (M2 bridge)

### M2 — Transactions & recovery
Snapshot isolation, first-committer-wins validation, checkpointing, redo/undo
replay. Crash-injection test harness (SIGKILL at random instruction points).

### M3 — SQL core
sqlparser-rs AST → logical IR → rule rewrites → push-based vectorized executor.
OLTP point-plan fast path. sqllogictest files under `crates/qmind-sql/tests/`.

### M4 — Full relational
Hash join / sort-merge join, hash/streams aggregates, secondary index maintenance,
planner threshold routing (OLTP path vs vectorized path).

### M5 — Server + CLI
tokio listener, scram-sha-256 auth, simple + extended query protocol. CLI REPL.

### M6 — Desktop Studio
Tauri 2 shell embedding the engine as a library; React UI evolved from this
repo's existing prototype (SQL editor, schema browser, data grid patterns).

### M7 — Hardening
cargo-fuzz targets, nightly perf runs vs contract table (§1.1), release
packaging, docs site.

### M8 — Columnar replica
Async delta apply from row store to persistent columnar segments.

## Post-M8 candidates (market-driven)
Document model layer (JSONB semantics over kernel), KV model layer public API,
SSI isolation, compression (columnar dictionary/RLE), backup/export tooling.
