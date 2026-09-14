# Technology Decision (R1.3)

> R1 deliverable — `docs/architecture/TECHNOLOGY_DECISION.md`
>
> Explicit, evidence-based language decision for the QuantsMind DB engine.
> Per R1 rules: Rust is not automatically preserved because it exists, and not
> automatically replaced because another language is theoretically possible.

---

## 1. Decision

```text
Decision: KEEP — Rust
```

Rationale in one sentence: the existing engine is written in Rust, the
code-quality signal (143 passing tests, clean `clippy -D warnings`, clean
`fmt`) shows the team can produce correct, idiomatic Rust, and Rust's
properties directly match every criterion that matters for a disk-backed
relational HTAP engine with embedded and server deployments.

---

## 2. Evaluation criteria

Scored Low / Medium / High / Very High, with explicit notes per criterion.

### 2.1 Execution performance — **High**

- Zero-cost abstractions, data-race freedom by construction, and no GC pauses
  fit a storage engine that must own every byte movement.
- `profile.release` already tunes for throughput: LTO `thin`,
  `codegen-units = 1`, `panic = "abort"` (`Cargo.toml`).
- Existing kernel bench harness (`crates/qmind-kernel/benches/kernel_bench.rs`)
  and CI's `cargo check --workspace --benches` gate keep perf work honest.
- C++/C marginally ahead on raw vectorization control in places, but the gap is
  engineering effort, not architecture.

### 2.2 Memory safety — **Very High**

- No raw-pointer page/catalog bugs, no use-after-free in an MVCC version
  store — the categorically most dangerous memory-corruption surface in a
  database (page cache + buffer manager + thread-interleaved MVCC).
- Balanced against unsafe Rust: current tree contains no unsafe block in the
  storage hot path (inspection), and remediation is localized when required.

### 2.3 Concurrency — **Very High**

- P5 already exploits `std::sync::RwLock` + snapshot reads
  (`wire.rs:107-124`, `engine.rs:194`) — safe by construction.
- Sync primitives, `parking_lot`-class libraries, and async runtimes are
  mainstream in the Rust DB ecosystem; a future multi-writer MVCC with
  crossbeam/tokio-style ordering maps cleanly onto Rust.

### 2.4 SIMD / vectorization ecosystem — **High**

- `std::simd` stabilization + crates (`wide`, `safe_arch`) plus LLVM auto-vec
  reach the throughput needed for batch columnar primitives (R3/R4).
- Equivalent case for C++ is marginally better only because of direct ISA
  intrinsics; Rust supports intrinsics too.

### 2.5 Storage-engine suitability — **Very High**

- Rust is the language of production storage engines today (sled, redb,
  forked open-source RDBMS work) — mature patterns for page/B+Tree/WAL do
  exist in the ecosystem to learn from; the codebase already ships page, WAL,
  MVCC and B+Tree primitives that compile cleanly.

### 2.6 Database ecosystem — **High**

- No PostgreSQL/SQLite-level C library is linked; the PGv3 listener is
  hand-implemented (`wire.rs`), and the SQL front end is a pure-Rust
  recursive-descent parser (`parser.rs`). Remaining ecosystem needs
  (parsing/interop) are Rust-first crates where relevant.

### 2.7 Portability — **High**

- CI already runs every gate on `ubuntu-latest` and `windows-latest`
  (`.github/workflows/ci.yml`); Tauri shell implies desktop targets; Rust's
  triples cover Linux/macOS/Windows with no GC runtime.
- `rust-version = "1.75"` (`Cargo.toml`) shows an intentional MSRV policy.

### 2.8 Tooling — **High**

- `cargo fmt`, `cargo clippy`, `cargo test --workspace` are wired as required
  CI gates; benchmark + static-analysis ecosystem is first-class.

### 2.9 Maintainability — **High**

- Module boundaries (`qmind-kernel`, `qmind-sql`, `qmind-server`,
  `qmind-embed`, `qmind-cli`) are clean; 143 tests pass; the current tree is
  small enough for the team to own wholly.
- Trait bounds (`Write`-generic WAL, `W: Write + Send + Sync + 'static`
  server) give testability without abstraction overhead.

### 2.10 Development velocity — **High**

- The codebase demonstrates rapid feature delivery (P3–P5 landed storage
  hardening, query surface, concurrency) without safety debt; the R2-R10 plan
  is additive on an existing, working foundation.

### 2.11 Long-term sustainability — **High**

- Rust is well-funded, stable, and increasingly the default for new
  infrastructure software; hiring and community support are durable.

### 2.12 Cross-platform support — **High**

- Engine is pure Rust with no GUI dependency (excluded `src-tauri` from the
  workspace); runs everywhere including headless servers, satisfying
  server + embed + desktop simultaneously.

---

## 3. Alternatives considered (evidence-based)

### 3.1 C++

- **Strengths**: mature SIMD/intrinsics, existing C++ DB lineage, no runtime.
- **Weaknesses**: memory safety for an MVCC/page-cache engine shifts to
  discipline (use-after-free and data races become latent), null-safety
  burdens, slow rebuild cycles, larger bug surface. Rewriting the entire
  existing, tested Rust tree to C++ has zero marginal product value.

### 3.2 C

- **Strengths**: absolute portability, tiny runtime.
- **Weaknesses**: everything C++ gives up, plus no ownership, manual memory
  lifecycle for the version store, and a much larger correctness tax.
  The engine would lose its best defensive property (memory safety).

### 3.3 Go

- **Strengths**: fast iteration, good concurrency primitives, GC removes
  ownership burden.
- **Weaknesses**: GC pauses are actively hostile to a buffer-manager/MVCC
  store that must repeatedly touch big buffers with predictable latency; the
  existing code has no Go surface to preserve. A Go engine is a full rewrite
  for a latency profile that does not suit the product.

### 3.4 Other systems candidates (Zig, D, Nim, OCaml, Java/kotlin native)

- None are as battle-tested at the infrastructure layer as Rust; each trades
  away safety or ecosystem maturity without a corresponding product win.

---

## 4. Is any subsystem a candidate for a non-Rust component?

- **No, not during R1-R2.** Every current subsystem is pure Rust and none
  forces a foreign language. Where specialized needs appear (e.g., full-text,
  SIMD kernels, native codecs) the plan is to use Rust crates, not language
  swaps, per "do not add document/KV/graph/vector capabilities during R1" and
  the minimal-change rule.
- The Studio frontend (TypeScript/React) is a **client**, not engine
  technology; its technology is decoupled from the engine by the
  `ENGINE_BOUNDARIES.md` contract and does not affect this decision.

---

## 5. Decision record

- **Decision**: `KEEP` Rust as the engine language.
- **Basis**: existing evidence (143 green tests, clean clippy, clean fmt,
  shipped P3-P5 features) plus the criterion scores above.
- **Rejected**: C++, C, Go, and other systems languages, because each would
  require a full rewrite of a working, tested engine strictly to lose a
  safety/ownership property the storage engine needs or to introduce a GC/Latency
  pattern that conflicts with the product.
- **Recorded in**: `ADR-002-engine-language.md`.

> No engine rewrite in another language occurs during R1 (per R1.3).