# ADR-003 — Engine/Server Boundary

- **Status**: Accepted (R1)
- **Date**: R1 architecture gate

## Context

`qmind-server` ships a minimal PGv3 listener that owns framing, connection
threads, and a `SharedEngine = Arc<RwLock<…>>` seam (`wire.rs:11`). The engine
(`qmind-kernel` + `qmind-sql`) must remain independently testable and free of
network/protocol concepts; the server must be free of engine internals. Today
the server already composes only the public `Engine` API
(`wire.rs:5` imports `qmind_sql::{Engine, SqlValue}`).

## Decision

The server is a **broker**: protocol framing, sessions, auth, TLS, resource
management, and observability. It may use only the public engine API
(`Engine::execute`, `Engine::execute_read`, snapshot semantics). It must never
reach into storage internals (page/WAL/MVCC types, encoded `(table, rid)`
rows). Concurrency ownership moves to a proper session/query API (R4) rather
than the server holding the engine-level lock.

## Consequences

- The engine stays embeddable and CLI-independent.
- Server correctness is testable against a protocol harness without touching
  engine internals.
- R4 replaces the `Arc<RwLock>` seam with an engine session/query API; the
  network thread never orchestrates engine locks.

## References

- `docs/architecture/ENGINE_BOUNDARIES.md`
- `crates/qmind-server/src/wire.rs`
- `docs/roadmap/PRODUCTION_ROADMAP.md` (R6 server stage)