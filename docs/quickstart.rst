.. _quickstart:

================
Quickstart
================

.. meta::
   :description: Build, run, and embed the QuantsMind engine — server, CLI shell,
      desktop studio, and the Rust embedding API.

Prerequisites
=============

.. list-table::
   :header-rows: 1

   * - Component
     - Version
   * - Rust
     - 1.75+ (stable)
   * - Node.js
     - 18+ (for web frontend / Tauri build)
   * - npm
     - 9+

Engine + server
===============

.. code-block:: bash

   # Clone
   git clone https://github.com/ajit-ai/Quantsmind-Relational-DB.git
   cd Quantsmind-Relational-DB

   # Build everything
   cargo build --release

   # Run the server (Postgres wire protocol on port 5432)
   cargo run --release -p qmind-server -- 5432

   # In another terminal — connect with the CLI
   cargo run --release -p qmind-cli -- 127.0.0.1:5432

You can also connect with any Postgres client (``psql``, DBeaver) — the server
speaks a Postgres wire v3 subset (trust auth, simple Query, values as TEXT).

Desktop studio
==============

.. code-block:: bash

   # Install Node dependencies
   npm install

   # Run in dev mode (opens a Tauri window with hot-reload)
   npx tauri dev

   # Build a production installer
   npx tauri build

The studio shell embeds the engine through ``qmind-embed``'s JSON API. Note:
the rich React components are still the legacy PGlite prototype and are not yet
rewired to the Rust engine (see :doc:`roadmap` M6).

Build from source
=================

All crates (library + server + CLI):

.. code-block:: bash

   cargo build --release

Binaries output to ``target/release/``:

- ``qmind-server`` — Postgres wire protocol server
- ``qmind-cli`` — interactive SQL shell

Workspace only (no binaries):

.. code-block:: bash

   cargo build -p qmind-kernel -p qmind-sql

Example session
===============

.. code-block:: sql

   CREATE TABLE users (id INTEGER NOT NULL, name TEXT, city TEXT);
   INSERT INTO users VALUES (1, 'Ada', 'London'), (2, 'Grace', 'New York');
   SELECT name FROM users WHERE city = 'London';
   --    name
   --   ------
   --   Ada

   CREATE INDEX idx_city ON users (city);
   SELECT COUNT(*) FROM users;
   --  4
   DROP INDEX idx_city;

Embedding the engine
====================

The engine is a plain Rust library — no runtime dependencies outside the
kernel. Point your ``Cargo.toml`` at the crate:

.. code-block:: toml

   [dependencies]
   qmind-sql = { path = "crates/qmind-sql" }
   qmind-kernel = { path = "crates/qmind-kernel" }

.. code-block:: rust

   use qmind_sql::Engine;

   fn main() -> Result<(), String> {
       let mut engine = Engine::new(Vec::new());        // WAL sink = in-memory
       engine.execute("CREATE TABLE t (a INTEGER NOT NULL, b TEXT)")?;
       engine.execute("INSERT INTO t VALUES (1, 'hello'), (2, 'world')")?;
       let res = engine.execute("SELECT a, b FROM t WHERE a = 1")?;
       for row in &res.rows {
           println!("{row:?}");
       }
       Ok(())
   }

Concurrency (current scope)
===========================

The engine is safe to share across threads behind a lock: reads (``SELECT``,
``SHOW TABLES``) and writes (``INSERT``, DDL) serialize on an exclusive mutex —
single-writer by design (see :doc:`architecture` §5 for honest status).

Testing
=======

.. code-block:: bash

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

.. _testing:

Test coverage
-------------

.. list-table::
   :header-rows: 1

   * - Suite
     - Count
     - Scope
   * - Kernel unit
     - 62
     - Pages, buffer pool, B+Tree, WAL, MVCC, locks, recovery, eviction, file store, columnar (M8)
   * - Kernel property/fuzz
     - 6
     - Differential B+Tree, WAL truncation/corruption, MVCC serial history, crash→recovery zero-loss
   * - Kernel integration
     - 3
     - Cross-store roundtrip
   * - SQL e2e
     - 23
     - DDL, DML, WHERE, expressions, ORDER BY, LIMIT, JOIN, GROUP BY, aggregates, columnar HTAP (M9), secondary indexes (P4)
   * - SQL unit (parser/codec/executor)
     - 30
     - Tokenizer, AST, case-insensitivity, strings, expression grammar, sort null-ordering, operators
   * - Parser fuzz
     - 4
     - 8K random inputs, no panics
   * - Soak test
     - 1
     - 10K row lifecycle across multiple tables
   * - Wire protocol
     - 1
     - TCP e2e (simple Query)
   * - Embedded API
     - 2
     - JSON API contract
   * - **Total**
     - **132**
     - **All green, clippy clean**

Tech stack
==========

.. list-table::
   :header-rows: 1

   * - Layer
     - Technology
   * - Language
     - Rust 2021 (MSRV 1.75)
   * - Parser
     - Handwritten tokenizer + recursive descent (0 external parser deps)
   * - Serialization
     - serde_json 1.x
   * - Desktop
     - Tauri 2 (Rust + React/TypeScript)
   * - Frontend
     - React 18, Vite 5, Tailwind CSS
   * - Docs
     - reStructuredText + Sphinx (this site)
   * - CI
     - GitHub Actions (Windows + Linux + docs deploy)