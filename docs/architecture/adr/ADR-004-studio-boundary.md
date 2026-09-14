# ADR-004 — Studio Boundary

- **Status**: Accepted (R1)
- **Date**: R1 architecture gate

## Context

The Studio is a Tauri + React shell whose SQL layer executes in-browser on
**PGlite** (PostgreSQL WASM): `src/lib/engine.ts:1` imports PGlite,
`src/lib/engine.ts:79` instantiates `new PGlite('idb://quantsmind')`; UI
badges advertise "IndexedDB (PGlite)" (`src/App.tsx:609,634`). This makes the
Studio currently a **second database implementation**, independent of the
engine — a violation of the product rule that only one database implementation
may exist.

## Decision

- The Studio is a **pure, replaceable client**. It owns UI, result rendering,
  and client-side presentation only.
- The Studio must connect to the real QuantsMind DB (embedded API in-process,
  or the server over PG wire at R6+).
- **PGlite is removed** from the final Studio architecture (R8). Today it
  remains only as a temporary development client.
- The Studio must not maintain a separate SQL-semantics implementation; gaps
  are fixed in the engine, not reimplemented in the browser.
- Tauri is non-core; desktop packaging stays undecided until engine/server
  architecture is stable.

## Consequences

- R8 rewires the Studio connector to `qmind-embed`/server and deletes the
  PGlite code path and dependency.
- Until then, engine features are validated against `qmind-c*` triggers
  tests and the CLI, not the GUI.
- Removes the risk of diverging SQL behavior between the browser and the
  real engine.

## References

- `docs/architecture/STUDIO_ARCHITECTURE.md`
- `docs/architecture/ENGINE_BOUNDARIES.md`
- `src/lib/engine.ts`, `src/App.tsx`, `src/components/Dialogs.tsx`
- `docs/roadmap/PRODUCTION_ROADMAP.md` (R8 Studio)