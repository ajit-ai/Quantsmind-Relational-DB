# Studio Architecture (R1.6)

> R1 deliverable — `docs/architecture/STUDIO_ARCHITECTURE.md`
>
> Future Studio direction. R1 scope: **do not redesign the GUI, add GUI
> features, or spend effort fixing Tauri.** Document the direction; implement
> at R8.

---

## 1. Status today (evidence-based)

- **Shell**: Tauri + React + TypeScript + Vite + Tailwind (`src/`,
  `src-tauri/`).
- **SQL execution is in-browser on PGlite** (PostgreSQL WASM), not the engine:
  - `src/lib/engine.ts:1` imports `PGlite` from `@electric-sql/pglite`;
  - `src/lib/engine.ts:79` constructs `new PGlite('idb://quantsmind')`;
  - UI advertises "IndexedDB (PGlite)" and "PGlite · PostgreSQL WASM"
    (`src/App.tsx:609,634`);
  - dialog copy: "powered by PGlite (PostgreSQL WASM)"
    (`src/components/Dialogs.tsx:98`).
- The embedded API target for Tauri commands exists (`qmind-embed/lib.rs:1-4`
  states it is "the contract the desktop GUI (Tauri commands) … calls") but is
  **not** the app's current SQL backend.

**Consequence**: the Studio today is a second database implementation
(browser-local SQLite) — a direct violation of the product rule "PGlite must
not remain as a second database implementation in the final Studio
architecture," and "the Studio must never become the database implementation."

---

## 2. Decisions for R1

| Item | Decision |
|---|---|
| GUI redesign | none (R1) |
| New GUI features | none (R1) |
| Tauri fixes | none (R1); Tauri is **non-core** |
| PGlite in final architecture | **REMOVE** |
| Studio today | preserved **only as a temporary development client** |
| Desktop packaging | undecided until engine/server architecture stabilizes |

---

## 3. Target Studio architecture (R8)

```text
Studio (React TS, replaceable shell)
   ↓
connector layer (one implementation):
   - embedded: qmind-embed (in-process)
   - remote:   PG wire protocol (or typed REST) → Server
   ↓
Database Engine  <—— the ONLY database implementation
```

Rules:

1. The Studio connects to the real QuantsMind DB (embed or server). It never
   owns SQL semantics, transaction state, or storage.
2. PGlite and any browser-side database are removed. Local persistence (if
   needed for offline UX later) is a **client cache**, explicitly not a
   database implementation, and must be dropped in favor of engine snapshots.
3. UI-renderable results come from engine result sets; grid operations map to
   engine statements (`SELECT`, `INSERT`, `UPDATE`, `DELETE` once supported in
   R4/R5).
4. The Studio is swappable: any future desktop/cloud client speaks the same
   connector contracts.

---

## 4. Desktop packaging status

- Tauri is recorded as **non-core** (`ADR-003`/`ADR-004`). The engine, server,
  embed, CLI have zero dependency on Tauri (workspace `exclude = ["src-tauri"]`).
- Packaging technology (Tauri, Electron, native, or browser-hosted) remains
  open until the engine/server architecture is stable. R1 explicitly spends no
  effort on it.

---

## 5. Migration path (R8, not R1)

1. Point the Studio connector at `qmind-embed` (fastest, in-process, JSON
   contract as today) while the server/studio protocol matures.
2. Replace PGlite-backed `engine.ts` with an embed/server-backed
   implementation; keep the UI shell and rendering.
3. Once PG-wire extended protocol + streaming land (`R6`), the Studio may also
   connect over the network for richer session behavior.
4. Delete PGlite dependency (`package.json`) and browser-storage paths.

Decision record: `ADR-004-studio-boundary.md`.