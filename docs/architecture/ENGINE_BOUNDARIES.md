# Engine Boundaries (R1.5)

> R1 deliverable — `docs/architecture/ENGINE_BOUNDARIES.md`
>
> Strict dependency and information-flow boundaries between Engine, Server,
> Embedded API, CLI, and Studio. These are enforced contract rules, not
> suggestions.

---

## 1. Boundary diagram (sanctioned shapes)

Shape A — remote / network client:

```text
Studio ──► API/PostgreSQL Protocol ──► Server ──► Database Engine
```

Shape B — embedded host:

```text
Application ──► Embedded API ──► Database Engine
```

Shape C — CLI (same boundary as A):

```text
CLI ──► PostgreSQL Protocol ──► Server ──► Database Engine
```

Precisely one database implementation may exist: **the engine**. Every other
box is allowed to hold only client/broker semantics.

---

## 2. Module ownership

| Box | Where it may live | Must not contain |
|---|---|---|
| Database Engine | `qmind-kernel`, `qmind-sql` | React, Tauri, browser storage, SQL front-end leakage, GUI state, PGlite, server framing |
| Server | `qmind-server` | Engine internals (page/WAL/MVCC types), GUI |
| Embedded API | `qmind-embed` | Engine internals beyond its contract facade |
| CLI | `qmind-cli` | Engine internals |
| Studio | `src` (+ optional `src-tauri` shell) | SQL semantics, database state, PGlite, any second storage engine |

---

## 3. The Engine must not know about…

- **React / Studio UI** — no engine path imports `src/**`, no UI constructs
  in engine types.
- **Tauri** — `src-tauri` is excluded from the cargo workspace
  (root `Cargo.toml` `exclude = ["src-tauri"]`). Nothing in `qmind-*` depends
  on Tauri; this stays true.
- **browser storage / PGlite** — the engine never talks to IndexedDB,
  WebSockets-on-WASM, or PGlite. Today the browser path is PGlite-bound
  (`src/lib/engine.ts`); that is a **temporary dev client only** and must be
  removed from the final architecture (`STUDIO_ARCHITECTURE.md`).
- **GUI state** — no view model, selection, or layout state reaches the
  engine.

## 4. The Studio must never become the database implementation

- All CREATE/INSERT/SELECT semantics must be executed by the engine. If a
  Studio feature needs behavior the engine lacks, the gap is fixed in the
  engine — not by reimplementing behavior in the browser.
- The Studio may hold: connection config, result rendering, UI state,
  local presentation caches. It may not hold: a second database engine,
  SQL execution semantics, MVCC/transaction state.
- PGlite is the current violation and is scheduled for removal at R8
  (`PRODUCTION_ROADMAP.md`).

---

## 5. Server ⇄ Engine boundary

- The server owns protocol framing, sessions, auth, TLS, resource management,
  observability (`wire.rs` today: framing + connection threads).
- It composes the engine through the engine's public API
  (`Engine::execute`, `Engine::execute_read`, snapshots) — it already does not
  import engine internals (`qmind-server` imports only `qmind_sql::{Engine,
  SqlValue}`).
- The engine API is the **sole** contract; server must never reach into
  `(table, rid)` encodings, WAL record bytes, or `MvccStore`.
- Concurrency ownership: `SharedEngine = Arc<RwLock<…>>` (`wire.rs:11`) is an
  acceptable present-day seam; R4 introduces a proper session/query API and
  the server stops owning engine-level locks.

## 6. Embed ⇄ Engine boundary

- `qmind-embed` is a thin JSON facade over the engine API
  (`qmind-embed/lib.rs:11` `Database<W: Write>` wraps `Engine<W>`). It exposes
  no GUI coupling and gets snapshot-read support mirroring `execute_read`
  (R6).
- FFI/embed hosting (e.g., Tauri commands) must go through `qmind-embed` or the
  server — never directly through engine internals.

## 7. CLI ⇄ Server boundary

- CLI speaks the wire protocol only (`qmind-cli/main.rs` connects a TCP
  stream, sends a PG startup packet). No CLI dependency on engine types.

## 8. Enforcement

- Workspace layout already encodes the boundaries: `qmind-kernel` ←
  `qmind-sql` ← `{qmind-server, qmind-embed, qmind-cli}`, `src-tauri`
  excluded from workspace.
- CI runs `cargo clippy --workspace` and tests; boundary regressions are
  caught by (a) dependency direction and (b) `cargo tree` inspections during
  R-stage gates.
- Rule of thumb: **a change is architecture-correct if removing the Studio
  entirely leaves the engine, server, embed, and CLI fully functional.**

Decision record: `ADR-003-engine-server-boundary.md`,
`ADR-004-studio-boundary.md`.