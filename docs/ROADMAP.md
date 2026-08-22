# QuantsMind Engine — Roadmap

> Each milestone ships an independently usable artifact. Exit criteria are
> enforced by benchmarks/tests in CI. See [ARCHITECTURE.md](./ARCHITECTURE.md).

| Milestone | Theme | Status | Exit criteria |
|---|---|---|---|
| **M0** | Foundations: workspace, CI, docs, bench harness | 🔄 **in progress** | `cargo test` + clippy green in CI; workspace builds on Win/Linux |
| **M1** | Storage kernel: pages, buffer pool, B+Tree, WAL (group commit) | ⬜ | ≥ 500K batched inserts/s single-thread; CRC page roundtrip tests; kill-mid-write leaves readable store |
| **M2** | Transactions: MVCC snapshots, RC/ISO, recovery replay | ⬜ | crash-injection suite passes (zero committed-txn loss); concurrent stress suite green |
| **M3** | SQL core: parser → logical plan → vectorized executor, single-table | ⬜ | ≥ 1M rows/s scan+filter; sqllogictest baseline green |
| **M4** | Full relational: joins, aggregates, secondary indexes, routing | ⬜ | TPC-H Q1/Q6 (SF 0.1) ≤ 5× DuckDB |
| **M5** | Server mode: PG wire protocol + CLI shell | ⬜ | psql connects & runs queries; 10K concurrent connections soak test |
| **M6** | Desktop Studio GUI (Tauri 2 + React reuse) | ⬜ | packaged installers (Win/Linux/macOS) run engine embedded |
| **M7** | Production hardening: cost hints, fuzzing, perf valley, packaging | ⬜ | 72h soak clean; all §1.1 performance contract targets met |
| **M8** | Persistent columnar replica (full HTAP storage split) | ⬜ | delta-apply lag bounded; OLAP scans read replica without blocking OLTP |

## Milestone details

### M0 — Foundations (current)
- [x] Cargo workspace with 4 crates (`kernel`, `sql`, `server`, `cli`)
- [x] Architecture doc + roadmap (this file)
- [x] CI gates: fmt, clippy `-D warnings`, test, bench compile
- [x] Criterion harness wired in kernel
- [ ] Kernel module skeletons with real type signatures (`page`, `buffer`, `btree`, `wal`, `mvcc`)

### M1 — Storage kernel
Deliverables: versioned file format headers (D-003), 8KiB pages w/ CRC,
clock-sweep buffer pool, B+Tree with latch crabbing, WAL segments with group
commit window. Benchmarks land in `crates/qmind-kernel/benches/`.

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
