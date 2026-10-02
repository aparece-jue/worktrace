# Worktrace — Architecture Decision Records

| Item | Value |
| --- | --- |
| Status | Design draft (under review) |
| Date | 2026-10-02 |
| Upstream | [`00-architecture.en.md`](00-architecture.en.md) |
| Chinese version | [`03-adr.zh.md`](03-adr.zh.md) |

Format: **Context → Decision → Rationale → Consequences → Rejected alternatives**. Status is `decided`, `open`, or `rejected`.

| ID | Decision | Status |
| --- | --- | --- |
| ADR-001 | Keep Ant Design as the sole component library; remove MUI / emotion | Decided, not yet applied |
| ADR-002 | Layered monolith with an internal event bus | Decided |
| ADR-003 | Access SQLite through `rusqlite` + `bundled` | Decided |
| ADR-004 | Whether dockview is needed | **Open** |
| ADR-005 | Windows-first with a single platform boundary | Decided |
| ADR-006 | Rust is the single source of truth | Decided |
| ADR-007 | Do not store `task.actual_duration` | Decided |
| ADR-008 | Store all timestamps as Unix milliseconds | Decided |
| ADR-009 | Separate Vite entries for HUD and Mini | Decided |
| ADR-010 | Event envelope carries a monotonic `revision` | Decided |

---

## ADR-001 — Ant Design only

**Status**: decided, not yet applied (first step once this design is approved)

**Context**: The scaffold depends on `antd@6`, `@mui/material@9`, `@emotion/react`, and `@emotion/styled` at once. Measured across `src/` and `src-tauri/src/`, MUI/emotion have **zero imports**; six files use antd.

**Decision**: Remove `@mui/material`, `@emotion/react`, and `@emotion/styled` from `package.json`. MUI remains a design/API reference only, consulted in documentation.

**Rationale**: Two component libraries mean double bundle size, two theming systems, and double the cognitive load — for zero current usage. The existing custom components (`FloatingInput`, `FloatingSelect`) depend only on antd and `theme.useToken()`.

**Consequences**: Regenerate `pnpm-lock.yaml`; run `pnpm build` and `pnpm tauri dev` to confirm no stale references. antd becomes the only component library, themed solely through `src/theme.ts`.

**Rejected**: Keeping MUI "in case it is useful later" — exactly the speculative stock SPEC §57 warns against.

---

## ADR-002 — Layered monolith with an internal event bus

**Status**: decided

**Context**: 13 modules must fit into one application. Options were a layered monolith, a microkernel with plugins, or a multi-crate workspace.

**Decision**: Layered monolith. Modules decouple through an internal event bus. No plugin system, and no crate split for now.

**Rationale**: SPEC §4's own diagram is layered; §5.4 states the event-driven design "does not need to become a complex workflow engine"; §50 lists `Plugin Marketplace` and `Generic Workflow Engine` as non-goals, and a microkernel is the first step toward exactly that. A multi-crate split buys boilerplate (`Cargo.toml` per crate, feature gates, circular-dependency juggling) that a single-person project does not need.

**Consequences**: Module boundaries are held by **discipline and review**, not by the compiler. The three hard rules in `00-architecture.en.md` §3.2 therefore belong in CI or at minimum in the code-review checklist.

**Rejected**: microkernel (pays for a non-existent requirement, conflicts with the non-goals). Multi-crate is not permanently rejected — when a module genuinely needs isolation (most likely `ai`, if network dependencies must be removable wholesale), carving out one crate is a local change.

---

## ADR-003 — `rusqlite` + `bundled`

**Status**: decided (with one assumption to verify)

**Decision**: `rusqlite` with the `bundled` feature. The database is accessed from Rust only.

**Rationale**:
- ADR-006 makes Rust the single source of truth, and `tauri-plugin-sql` is designed for frontend-issued SQL — a direct conflict, so it is excluded first.
- `bundled` compiles SQLite into the binary: no dependency on the user's system SQLite, and no extra packaging step.
- A local single-user file database has no connection-pool or multi-backend pressure, so a synchronous API keeps transactions and reasoning simpler.
- `sqlx`'s compile-time SQL checking needs a `DATABASE_URL` or a checked-in `.sqlx` cache, raising build and CI complexity for a benefit that does not materialize at this scale.
- Diesel requires `print-schema` and leans on macros that handle dynamic filtering plus aggregation poorly.

**Consequences**: SQL is a string with **no compile-time validation**. That is compensated with tests: every repository function in M01 gets one. This is the main cost of the decision and should be accepted rather than worked around.

**To verify**: how synchronous `rusqlite` interacts with Tauri commands — which thread runs a synchronous command, and whether heavy queries uniformly use `spawn_blocking`. See `00-architecture.en.md` §7, item 3.

**Rejected**: `sqlx`, Diesel, `tauri-plugin-sql` (see rationale).

---

## ADR-004 — Whether dockview is needed

**Status**: **open** (the only undecided item)

**Context**: The scaffold ships `dockview@8` and `dockview-react@8`. Measured across the repository, dockview has exactly **one** consumer: `src/components/DockviewDemo.tsx`, a 41-line demo. The two components actually in use (`FloatingInput`, `FloatingSelect`) do not depend on it.

**Question to answer**: does the main interface need **draggable, splittable multi-panel layouts**?

| Branch | Test | Consequence |
| --- | --- | --- |
| **Needed** | Pages such as Today / Inbox / Timer / Reports have a real need to observe several views side by side, and the user will rearrange layouts | Keep dockview and write it its own spec: panel registry, layout persistence, relationship to `features/` |
| **Not needed** | One view per page suffices; side-by-side observation is served by fixed columns or tabs | Remove dockview and `DockviewDemo`; `src/components` keeps only components in real use |

**Current lean**: the evidence for "needed" is thin. SPEC §36's core pages (Inbox / Today / Projects / Calendar / Review / Reports / Knowledge / Settings) are all single-view; §37's Today is a vertical list; §38–42's HUD / Mini / Tray are unrelated to dockview. **But this is a product question about working habits, not an architectural one**, so this document does not settle it.

**When to evaluate**: while designing the Today and Reports pages in M12. Until then, dockview stays installed but **unused**, and no code may be written against it.

**Note**: `DockviewDemo.tsx` is a copied demo file and **is not product code**. Whatever the outcome, it must not ship in V0.1.

---

## ADR-005 — Windows-first with a single platform boundary

**Status**: decided (with one assumption to verify)

**Context**: Tauri is cross-platform by nature, but SPEC §38–42's HUD (always-on-top, transparent, click-through, no taskbar entry, never focused) differs sharply per platform — `WS_EX_LAYERED` / `WS_EX_TRANSPARENT` on Windows, `NSPanel` + `ignoresMouseEvents` on macOS, and per-WM behaviour on Linux.

**Decision**: Windows-first. Every platform-specific call lives in `src-tauri/src/platform/`, with **no trait abstraction and no other platform implemented**.

**Rationale**: This is directory discipline and costs nearly nothing, yet it turns a port from "hunt every scattered `#[cfg(windows)]` across `src-tauri`" into "rewrite one directory". A trait with a single implementation is the speculative abstraction SPEC §57 warns against.

**Consequences**: `platform/` is a leaf, callable only from `services/` and above — a fourth boundary rule alongside the three hard rules.

**To verify**: dynamic click-through on Win32, and whether `SetWindowPos(..., SWP_FRAMECHANGED)` is required. See `00-architecture.en.md` §7, item 1.

**Rejected**: doing multi-platform now — high cost, and HUD, tray, and global hotkeys each need real hardware verification.

---

## ADR-006 — Rust as the single source of truth

**Status**: decided

**Decision**: Rust holds all domain state. React holds **UI-local state only** (form input, selection, expansion). Communication is command (frontend → Rust, request/response) plus event (Rust → frontend, broadcast).

**Rationale** (all three from SPEC itself):
- §7 states it directly: "Rust owns core state, timing, statistics, the database, Context, and AI calls."
- §15 requires timing immune to JS jank, hidden windows, and system sleep — satisfiable only with a Rust-side clock.
- §38–42 require four entry points (main, HUD, Mini, tray). In Tauri each webview is a **separate JS context** with no shared store, so React-held state would need hand-written cross-window synchronization.

**Consequences**: every UI action crosses IPC, so command granularity must follow user intent and avoid N+1 (`00-architecture.en.md` §5.1). A standardised frontend mirror (`src/services/domainState.ts`) is required, and **pages must never call `listen()` themselves**. The frontend becomes replaceable.

**Rejected**: React-owned state (three copies of cross-window sync, timing exposed to JS jank, tray unable to read React state); dual sources of truth synced by events (consistency costs, guaranteed bugs).

---

## ADR-007 — Do not store `task.actual_duration`

**Status**: decided

**Context**: SPEC §8.4's Task field list includes `actual_duration`.

**Decision**: Do not store it. Actual effort is always aggregated from `work_session`.

**Rationale**: A stored copy creates a second source of truth, and the cache will drift from the detail rows. `work_session(task_id)` is indexed, so aggregation is cheap at personal scale.

**Consequences**: every "actual effort for this task" query becomes a JOIN plus SUM. If reports become a bottleneck, add a **materialized summary table** maintained by M01 when a session ends — never a semantically vague column on `task`.

**Rejected**: storing the column and updating it on session end — too many invalidation paths (interruption, crash recovery, manual corrections); missing one breaks consistency.

---

## ADR-008 — Unix milliseconds for every timestamp

**Status**: decided

**Decision**: All time fields are `INTEGER` Unix milliseconds with no timezone. The presentation layer renders local time.

**Rationale**: avoids daylight-saving and relocation ambiguity, and keeps SQLite's date functions out of business calculations, which live in Rust.

**Consequences**: raw values are unreadable while debugging, so logs and tooling need consistent formatting. Range boundaries ("today") are computed in Rust using the local timezone and passed in; SQL performs no timezone reasoning.

---

## ADR-009 — Separate Vite entries for HUD and Mini

**Status**: decided (with one assumption to verify)

**Decision**: `hud.html` and `mini.html` are separate entries rather than one entry with a route parameter.

**Rationale**: the HUD must open instantly, stay resident, and render minimally. Going through the main entry loads the entire main-window bundle into a transparent overlay — slower startup, higher memory, more fragile rendering.

**Consequences**: Vite needs `build.rollupOptions.input` set up. The entries share `theme.ts` and `components/`, but **must not** import pages under `features/`.

**To verify**: how `WebviewWindowBuilder`'s url maps onto multi-entry output. See `00-architecture.en.md` §7, item 2.

---

## ADR-010 — Monotonic `revision` in the event envelope

**Status**: decided

**Context**: SPEC names events but defines no envelope. Windows may reopen, reload, or miss events.

**Decision**: every event carries `{ event, revision, at, payload }`, where `revision` is a globally monotonic counter maintained in Rust.

**Rationale**: on mount a window calls `get_snapshot()` to learn the current revision; if a later event's revision **skips**, the window knows it missed one and refetches. Without this, a window that missed an event displays stale data forever with no way to detect it.

**Consequences**: Rust maintains a global counter whose value must **persist across restarts**, or a restart would move the revision backwards and the frontend would misjudge. `get_snapshot()` becomes a required command.

**Rejected**: periodic full refresh — unpredictable latency, higher cost, and more expensive precisely in the high-frequency timing case.
