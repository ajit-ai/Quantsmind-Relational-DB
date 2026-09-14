# ADR-002 — Engine Language

- **Status**: Accepted (R1)
- **Date**: R1 architecture gate

## Context

The engine must be written in a systems language able to own disk I/O, a page
cache, MVCC, and a batch execution engine. Candidates evaluated: Rust, C++,
C, Go, and other systems languages (see `docs/architecture/TECHNOLOGY_DECISION.md`
for the full evidence-based comparison).

**Evidence from this repository**: the engine is already a working, well-tested
Rust codebase — 143 workspace tests pass, `cargo clippy --workspace
--all-targets -- -D warnings` is clean, `cargo fmt --all --check` passes, and
Rust-level features (snapshot reads via `RwLock` + `MvccStore::snapshot`,
generics over WAL sinks `Engine<W: Write>`) are used effectively
(`wire.rs:11`, `engine.rs:194`, `mvcc.rs:131`).

## Decision

**Keep Rust** for the database engine and all `qmind-*` crates.

Rust's ownership model is the strongest available defense against the two
bug classes that dominate storage-engine failure: use-after-free in buffer
management and data races in MVCC. Zero-cost abstractions and first-class
LLVM/SIMD tooling meet the execution requirements. Alternatives would require
a full rewrite of working, tested code while losing a safety property the
product needs.

## Consequences

- All R2–R10 engine work is in Rust; no foreign-language components introduced
  during R1–R2.
- Rust safety continues to be defensive: `unsafe` must remain justified and
  localized; clippy `-D warnings` stays an enforced gate.
- `src-tauri` (Tauri) is **not** a Rust engine dependency historically or going
  forward — the engine workspace excludes it.

## References

- `docs/architecture/TECHNOLOGY_DECISION.md` (criteria scoring)
- `Cargo.toml` (workspace, release profile)
- `.github/workflows/ci.yml` (gates)