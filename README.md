# QuantsMind Relational Database Engine

> **Status: Developer Preview / Experimental** — feature-complete for its SQL subset; not yet production-hardened (see [Architecture](docs/architecture.rst) for honest gaps).

An embeddable relational database engine written in Rust, designed for hybrid transactional + analytical workloads (HTAP). Ships as a library, a Postgres-wire-compatible server, a CLI shell, and a native desktop GUI studio.

**Version 0.1.0** · MIT License · Docs (RST sources live in [`docs/`](docs/index.rst); published site URLs below activate once GitHub Pages is enabled — see the [`docs.yml`](.github/workflows/docs.yml) workflow header for the opt-in publish step)

**Docs, offline:** build the site locally with `make -C docs html` (or `sphinx-build -b html docs docs/_build/html`, needs `pip install -r docs/requirements.txt`), then open `docs/_build/html/index.html`. Every docs change on `main` also ships the rendered HTML as the `docs-html` workflow artifact.

---

## Table of Contents

- [Design Pillars](#design-pillars)
- [Architecture Overview](#architecture-overview)
- [System Layers](#system-layers)
- [Performance Contract](#performance-contract)
- [Crate Structure](#crate-structure)
- [Getting Started](#getting-started)
- [Build from Source](#build-from-source)
- [Platform-Specific Instructions](#platform-specific-instructions)
- [Server Mode](#server-mode)
- [CLI Shell](#cli-shell)
- [Desktop Studio](#desktop-studio)
- [Embedding the Engine](#embedding-the-engine)
- [Packaging & Distribution](#packaging--distribution)
- [Testing](#testing)
- [Tech Stack](#tech-stack)
- [License](#license)

---

## Design Pillars

1. **Kernel-first layering** — the core is a generic KV + index + MVCC + WAL storage kernel. Relational, Document, and Key-Value are model layers on top. New data models are additive features, not rewrites.
2. **HTAP from day one** — row store serves OLTP; a persistent columnar replica (M8) serves OLAP reads. The OLAP path is batched, not yet SIMD-vectorized.
3. **Performance is a contract** — every milestone has numeric exit criteria enforced by benchmarks in CI. No aspirational numbers.
4. **Correctness over speed** — MVCC and recovery are fuzzed and property-tested. Silent corruption is the only unacceptable bug.

---

## Architecture Overview

```
┌───────────────────────────────────────────────────────────────────┐
│  Clients                                                          │
│    Desktop Studio (Tauri 2 + React)   CLI Shell   psql / drivers  │
├───────────────────────────────────────────────────────────────────┤
│  Server Layer                   [qmind-server]                     │
│    Postgres wire protocol v3 · TCP listener · session pool         │
├───────────────────────────────────────────────────────────────────┤
│  SQL Layer                      [qmind-sql]                        │
│    Handwritten parser → Volcano operator executor                  │
│    DDL: CREATE TABLE · DML: INSERT / SELECT                        │
│    Operators: SeqScan · Filter · Project · Limit                   │
│              HashJoin · HashAggregate · Columnar read path (M9)    │
├───────────────────────────────────────────────────────────────────┤
│  Embedded API                [qmind-embed]                         │
│    JSON contract for GUI / host integration                        │
├───────────────────────────────────────────────────────────────────┤
│  Kernel                       [qmind-kernel]   ← core IP          │
│    Buffer pool · B+Tree index · heap row pages                     │
│    MVCC snapshots (SI) · WAL group-commit · ARIES recovery         │
│    Strict 2PL · deadlock detection · LRU-K eviction                │
│    CRC32 page checksums · versioned on-disk format                 │
└───────────────────────────────────────────────────────────────────┘
```

### Transaction & Concurrency Control

| Component | Implementation |
|---|---|
| Isolation | Snapshot Isolation (readers never block writers) |
| Write conflicts | First-committer-wins validation |
| Locking | Strict 2PL — S/X locks, FIFO queues |
| Deadlock | Wait-for graph + DFS cycle detection |
| Recovery | ARIES-style/logical WAL recovery (redo-only, record-level replay) — no physical checkpoints yet |

### Storage Engine

| Component | Implementation |
|---|---|
| Page size | 8 KiB, CRC32 full-page checksums |
| Buffer pool | Clock-sweep + LRU-K (K=2) eviction |
| Index | B+Tree — split/merge, range scan, duplicate key support |
| WAL | Group-commit (1 ms window), CRC frames, torn-tail safe |
| Row format | Variable-length encoding, NULL bitmap per page |
| File format | `QMINDSEG` magic, versioned headers (D-003) |

### Volcano Execution Model (E4)

```
trait Operator {
    fn next(&mut self) -> Result<Option<Row>, String>;
}
```

Pull-based streaming operators: `VecScan`, `Scan<Closure>`, `Filter`, `Project`, `Limit`, `HashJoin`, `HashAggregate`. The engine's SELECT/JOIN/GROUP BY paths execute through these operators.

---

## Performance Contract

| Metric | Target | Status |
|---|---|---|
| Bulk insert | ≥ 500K rows/s | **Not re-measured in R2** |
| WAL write throughput | ≥ 1M rec/s | **Not re-measured in R2** |
| Page seal latency | < 1 μs | **Not re-measured in R2** |
| Recovery | zero committed-txn loss | **R2 PASS** -- 7 subprocess crash-recovery tests; committed data survives `std::process::exit` kills; uncommitted data rolled back |

> *Note: earlier R1 dev-build throughput numbers (2.49M rows/s, 6.1M rec/s, 123 ns) were not verified against the R2 codebase and are not claimed current. Only the recovery claim has R2 evidence.*

---

## Crate Structure

```
Quantsmind-Relational-DB/
├── Cargo.toml                 # workspace root (members = crates/*)
├── crates/
│   ├── qmind-kernel/          # storage kernel (pages, btree, wal, mvcc, lock, recovery)
│   ├── qmind-sql/             # SQL engine + Volcano executor
│   │   ├── src/engine.rs      # DDL/DML execution
│   │   ├── src/executor.rs    # Volcano operator framework
│   │   └── src/codec.rs       # row ↔ KV byte codec
│   ├── qmind-server/          # Postgres wire protocol server (TCP)
│   ├── qmind-embed/           # JSON API contract for GUI integration
│   └── qmind-cli/             # interactive REPL over TCP
├── src-tauri/                 # Tauri 2 desktop app (independent build)
│   ├── src/main.rs            # run_sql command wired to Engine
│   └── tauri.conf.json        # app config
├── src/                       # React/TypeScript frontend
│   ├── QmindStudio.tsx        # main studio UI
│   └── lib/desktop.ts         # typed Tauri bridge
├── docs/
│   ├── index.rst               # docs source of truth (RST, Sphinx)
│   ├── architecture.rst        # design decisions, kernel spec
│   ├── roadmap.rst             # milestones M0–M9, phases P2–P12
│   ├── quickstart.rst          # build/run/embed guide
│   └── conf.py                 # Sphinx config
└── .github/workflows/         # CI gates + docs build (Pages opt-in)
```

---

## Getting Started

### Prerequisites

| Component | Version |
|---|---|
| Rust | 1.75+ (stable) |
| Node.js | 18+ (for web frontend / Tauri build) |
| npm | 9+ |

### Quick Start — Engine + Server

```bash
# Clone
git clone https://github.com/ajit-ai/Quantsmind-Relational-DB.git
cd Quantsmind-Relational-DB

# Build everything
cargo build --release

# Run the server (Postgres wire protocol on port 5432)
cargo run --release -p qmind-server -- ./qmind-data 5432

# In another terminal — connect with the CLI
cargo run --release -p qmind-cli -- 127.0.0.1:5432
```

### Quick Start — Desktop Studio

```bash
# Install Node dependencies
npm install

# Run in dev mode (opens Tauri window with hot-reload)
npx tauri dev

# Build production installer
npx tauri build
```

---

## Build from Source

### All Crates (library + server + CLI)

```bash
cargo build --release
```

Binaries output to `target/release/`:
- `qmind-server` — Postgres wire protocol server
- `qmind-cli` — interactive SQL shell

### Workspace Only (no server/CLI binaries)

```bash
cargo build -p qmind-kernel -p qmind-sql
```

---

## Platform-Specific Instructions

### Windows

```powershell
# PowerShell or Command Prompt
git clone https://github.com/ajit-ai/Quantsmind-Relational-DB.git
cd Quantsmind-Relational-DB

# Install Rust (if not present)
winget install Rustlang.Rustup

# Build
cargo build --release

# Run server
.\target\release\qmind-server.exe .\qmind-data 5432

# Run CLI
.\target\release\qmind-cli.exe 127.0.0.1:5432

# Desktop Studio
npm install
npx tauri build
# Installer at: src-tauri/target/release/bundle/
```

### Linux (Ubuntu/Debian/Fedora/Arch)

```bash
# Install dependencies
# Ubuntu/Debian:
sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev

# Fedora:
sudo dnf groupinstall -y "Development Tools" && sudo dnf install -y openssl-devel

# Arch:
sudo pacman -S base-devel openssl

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# Clone and build
git clone https://github.com/ajit-ai/Quantsmind-Relational-DB.git
cd Quantsmind-Relational-DB
cargo build --release

# Run server
./target/release/qmind-server ./qmind-data 5432

# Run CLI
./target/release/qmind-cli 127.0.0.1:5432

# Desktop Studio (requires webkit2gtk for Tauri)
# Ubuntu/Debian:
sudo apt install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev
# Fedora:
sudo dnf install -y webkit2gtk4.1-devel libappindicator-gtk3-devel librsvg2-devel
# Arch:
sudo pacman -S webkit2gtk-4.1 libappindicator-gtk3 librsvg

npm install && npm run build
npx tauri build
# Binary at: src-tauri/target/release/qmind-studio
```

### macOS

```bash
# Install Xcode command line tools
xcode-select --install

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# Install Node.js (via Homebrew)
brew install node

# Clone and build
git clone https://github.com/ajit-ai/Quantsmind-Relational-DB.git
cd Quantsmind-Relational-DB
cargo build --release

# Run server
./target/release/qmind-server ./qmind-data 5432

# Run CLI
./target/release/qmind-cli 127.0.0.1:5432

# Desktop Studio
npm install
npx tauri build
# .app bundle at: src-tauri/target/release/bundle/macos/
# .dmg at: src-tauri/target/release/bundle/dmg/
```

### FreeBSD / OpenBSD

```bash
# Install dependencies
# FreeBSD:
pkg install -y git rust node npm pkgconf openssl

# OpenBSD:
doas pkg_add rust node npm

# Clone and build
git clone https://github.com/ajit-ai/Quantsmind-Relational-DB.git
cd Quantsmind-Relational-DB
cargo build --release

# Run
./target/release/qmind-server ./qmind-data 5432
./target/release/qmind-cli 127.0.0.1:5432
```

> **Note:** Desktop Studio (Tauri) requires GTK + WebKit2GTK on BSD. The engine and server work on all platforms without GUI dependencies.

---

## Server Mode

The server speaks Postgres wire protocol v3, so any Postgres client works.

```bash
# Start server
cargo run --release -p qmind-server -- ./qmind-data 5432

# Connect with psql (if available)
psql -h 127.0.0.1 -p 5432 -U qmind

# Connect with the built-in CLI
cargo run --release -p qmind-cli -- 127.0.0.1:5432
```

### Supported SQL Surface

```sql
-- DDL
CREATE TABLE users (id INTEGER NOT NULL, name TEXT, salary INTEGER);
CREATE TABLE IF NOT EXISTS users (id INTEGER NOT NULL, name TEXT);

-- DML
INSERT INTO users VALUES (1, 'Alice', 90000);
INSERT INTO users VALUES (2, 'Bob', 85000), (3, 'Carol', 95000);

-- Query
SELECT * FROM users WHERE salary > 80000 LIMIT 10;

-- Aggregates
SELECT COUNT(*), SUM(salary), AVG(salary) FROM users;
SELECT dept, COUNT(*), SUM(salary) FROM users GROUP BY dept;

-- Joins
SELECT name, amount FROM customers INNER JOIN orders ON id = cid;
SELECT name FROM customers INNER JOIN orders ON id = cid WHERE amount > 200;

-- System
SHOW TABLES;
```

---

## CLI Shell

```bash
cargo run --release -p qmind-cli -- [host:port]
```

Defaults to `127.0.0.1:5432`. Enter SQL statements interactively; results are printed in aligned text format. Supports multi-statement input separated by `;`.

---

## Desktop Studio

The Tauri 2 desktop app embeds the Rust engine directly (no TCP socket) and provides a web-based UI with:

- SQL editor with multi-statement support and Ctrl+Enter shortcut
- Schema browser (tables, columns, row counts)
- Data grid with results display
- Persistent storage (WAL survives app restarts)

```bash
# Development (hot-reload)
npx tauri dev

# Production build
npx tauri build
```

---

## Embedding the Engine

```rust
use qmind_sql::Engine;

let mut eng = Engine::new("my-data-dir").unwrap();

// Create table
eng.execute("CREATE TABLE t (id INTEGER NOT NULL, val TEXT)").unwrap();

// Insert data
eng.execute("INSERT INTO t VALUES (1, 'hello')").unwrap();

// Query
let result = eng.execute("SELECT * FROM t WHERE id = 1").unwrap();
for row in &result.rows {
    println!("{:?}", row);
}
```

The `qmind-embed` crate provides a JSON API wrapper for integration with any language that can call into Rust FFI or speak JSON over a transport.

---

## Packaging & Distribution

### Pre-built Binaries

Download from [GitHub Releases](https://github.com/ajit-ai/Quantsmind-Relational-DB/releases):

| Platform | Artifact | Installer |
|---|---|---|
| Windows x64 | `qmind-windows-x64-{ver}.zip` | `.msi`, `.exe` NSIS |
| Linux x64 | `qmind-linux-x64-{ver}.tar.gz` | `.deb`, `.AppImage` |
| Linux x64 static | `qmind-linux-x64-static-{ver}.tar.gz` | — (no glibc) |
| Linux ARM64 | `qmind-linux-arm64-{ver}.tar.gz` | — |
| macOS x64 | `qmind-macos-x64-{ver}.tar.gz` | `.dmg`, `.app` |
| macOS ARM64 | `qmind-macos-arm64-{ver}.tar.gz` | `.dmg`, `.app` |
| FreeBSD x64 | `qmind-freebsd-x64-{ver}.tar.gz` | — |

All releases include `SHA256SUMS.txt` for integrity verification.

### One-Line Install Scripts

```bash
# Linux (Debian/Ubuntu/Fedora/Arch/Alpine)
curl -sSL https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/linux/install.sh | bash

# macOS
curl -sSL https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/macos/install.sh | bash

# FreeBSD
fetch -qO- https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/linux/freebsd-install.sh | bash
```

### Windows (PowerShell)

```powershell
# Download and run
Invoke-WebRequest -Uri "https://raw.githubusercontent.com/ajit-ai/Quantsmind-Relational-DB/main/packaging/windows/install.ps1" -OutFile install.ps1
.\install.ps1

# Or with scoop
scoop bucket add quantsmind https://github.com/ajit-ai/Quantsmind-Relational-DB
scoop install qmind
```

### Desktop Installers

Built via `npx tauri build` on each platform:

| Platform | Format |
|---|---|
| Windows | `.msi` (WiX) and `.exe` (NSIS) installer |
| Linux | `.deb` (Debian/Ubuntu) and `.AppImage` (universal) |
| macOS | `.dmg` and `.app` bundle (notarized) |

### Uninstall

```bash
# Linux
packaging/linux/uninstall.sh

# macOS
packaging/macos/uninstall.sh

# Windows (PowerShell, as Admin)
.\packaging\windows\uninstall.ps1
```

### Cross-Compilation

```bash
# From Linux, build for all targets
./scripts/build-all.sh v0.1.0

# From Windows, build all targets
.\scripts\build-all.ps1 -Tag v0.1.0
```

### Static Build (musl)

```bash
# Linux static binary (no glibc dependency — runs anywhere)
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

### Automated Release CI

Pushing a tag `v*` triggers `.github/workflows/release.yml`:
- Builds 6 binary targets + 3 desktop platforms
- Generates `SHA256SUMS.txt` for all artifacts
- Creates draft GitHub Release with auto-generated notes

---

## Testing

```bash
# Run all tests
cargo test --workspace

# Run only kernel tests
cargo test -p qmind-kernel

# Run only SQL tests (engine + Volcano operators)
cargo test -p qmind-sql

# Run benchmarks (release mode)
cargo bench -p qmind-kernel

# Lint
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

### Test Coverage

| Suite | Count | Scope |
|---|---|---|
| Kernel unit | 70 | Pages, buffer pool, B+Tree, WAL, MVCC, locks, recovery, eviction, file store, columnar (M8); DDL record roundtrip, syncer durability, resume LSN continuation, Display stability |
| Kernel property/fuzz | 6 | Differential B+Tree, WAL truncation/corruption, MVCC serial history, crash→recovery zero-loss |
| Kernel integration | 3 | Cross-store roundtrip |
| Kernel read stress (P5) | 6 | Snapshot readers vs live writer: monotonicity, frozen snapshots, watermark advance |
| SQL e2e | 23 | DDL, DML, WHERE, expressions, ORDER BY, LIMIT, JOIN, GROUP BY, aggregates, columnar HTAP (M9), secondary indexes (P4) |
| SQL concurrency (P5) | 4 | Torn-batch detection, snapshot consistency, read/write gate, concurrent writers |
| SQL unit (parser/codec/executor) | 30 | Tokenizer, AST, case-insensitivity, strings, expression grammar, sort null-ordering, operators |
| Parser fuzz | 4 | 8K random inputs, no panics |
| Soak test | 1 | 10K row lifecycle across multiple tables |
| SQL persistence (R2) | 12 | In-process create/insert/close/reopen, torn-tail truncation, corruption loud failure, format version gate |
| SQL crash recovery (R2) | 7 | Real subprocess `std::process::exit` kills (R2.25 acceptance); uncommitted rollback; index rebuild; repeated restarts |
| Wire protocol | 2 | TCP e2e (simple Query) + concurrent-reader no-torn-read (P5) |
| Embedded API | 2 | JSON API contract |
| **Total** | **170** | **All green, clippy clean, fmt clean** |

---

## Tech Stack

| Layer | Technology |
|---|---|
| Language | Rust 2021 (MSRV 1.75) |
| Parser | Handwritten tokenizer + recursive descent (0 external parser deps) |
| Serialization | serde_json 1.x |
| Desktop | Tauri 2 (Rust + React/TypeScript) |
| Frontend | React 18, Vite 5, Tailwind CSS |
| CI | GitHub Actions (Windows + Linux) |
| Benchmarking | Criterion 0.5 |
| Wire protocol | Postgres v3 (binary-compatible) |

---

## Locked Design Decisions

| ID | Decision | Rationale |
|---|---|---|
| D-001 | **HTAP** — row store for OLTP; persistent columnar replica (M8) for OLAP reads | Hybrid workload without data duplication |
| D-002 | **Layered kernel** — KV + index + MVCC + WAL core; relational/doc/KV model layers on top | Extensibility without rewrites |
| D-003 | **Versioned on-disk format** — magic bytes + format version in every file header | Forward migration, no silent corruption |
| D-004 | **Postgres wire protocol** — ecosystem leverage (psql, DBeaver, drivers) | Zero-friction adoption |

---

## Roadmap Summary

| Phase | Theme | Status |
|---|---|---|
| M0 | Foundations (workspace, CI, docs) | Done |
| M1 | Storage kernel (pages, B+Tree, WAL, file store) | Done |
| M2 | Transactions (MVCC, ARIES recovery) | Done |
| M3 | SQL core (DDL, DML, filter, limit) | Done |
| M4 | Relational (JOIN, GROUP BY, aggregates) | Done |
| M5 | Server + CLI shell | Done (trust auth; SCRAM/TLS pending) |
| M6 | Desktop Studio (Tauri 2) | Shell done (rich UI rewiring pending) |
| M7 | Hardening (fuzzing, soak, packaging, v0.1.0) | Done |
| M8 | Persistent columnar replica (full HTAP) | Done |
| M9 | Columnar integration into SQL engine | Done |
| P2–P12 | Production arc (correctness, concurrency, perf, ops, 1.0 GA) | Planned — see [Roadmap](https://ajit-ai.github.io/Quantsmind-Relational-DB/roadmap.html) |

---

## License

QuantsMind Relational Database Engine is licensed under the [MIT License](LICENSE).
