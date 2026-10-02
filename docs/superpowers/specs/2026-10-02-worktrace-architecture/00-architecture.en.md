# Worktrace — Overall Architecture

| Item | Value |
| --- | --- |
| Status | Design draft (under review) |
| Date | 2026-10-02 |
| Upstream | [`../../../PROJECT_SPEC.md`](../../../PROJECT_SPEC.md) (product design draft) |
| Applies to | V0.1 onward |
| Chinese version | [`00-architecture.zh.md`](00-architecture.zh.md) |

> This document describes **how**. **What and why** live in `PROJECT_SPEC.md`. Where they conflict, this document wins and the reason is recorded as an ADR.

---

## 1. Positioning and non-goals

Worktrace is a **desktop tool for AI-assisted personal work management, time tracking, and work analytics**, built for long-term single-user use.

Two constraints run through everything (SPEC §5.1, §5.2):

- **Local First** — core data lives only in a local SQLite file. The tool is fully usable offline.
- **AI Optional** — AI is an enhancement layer. Task, Project, Timer, WorkSession, Tag, Report, Review, Search, and Export must all work with no AI and no network.

**Explicitly out of scope** (SPEC §50): plugin marketplace, generic workflow engine, general agent platform, IDE replacement, Office replacement, CAD automation. Architecturally this reduces to one rule: **no plugin system** (see ADR-002).

---

## 2. Locked stack

| Layer | Choice | Note |
| --- | --- | --- |
| Shell | Tauri 2 | Single process, multiple webview windows |
| Frontend | React 19 + TypeScript 6 + Vite 8 | |
| Components | **Ant Design 6 (sole library)** | MUI / emotion removed — ADR-001 |
| Panels | dockview 8 | **Under evaluation** — ADR-004 |
| Packages | pnpm | |
| Backend | Rust, edition 2021 | |
| Database | SQLite via `rusqlite` + `bundled` | ADR-003 |
| Platform | Windows-first | All platform calls confined to `platform/` — ADR-005 |

---

## 3. Layers and dependency rules

### 3.1 Six layers

| Layer | Directory | Responsibility | May depend on |
| --- | --- | --- | --- |
| L4 Boundary | `commands/` | Tauri IPC: argument validation, DTO mapping, orchestration | `services`, `domain`, `events` |
| L3 Services | `services/` | timer / session / statistics / report / knowledge / context / search / ai | `domain`, `storage`, `platform`, `events` |
| L2 Persistence | `storage/` | Connections, transactions, schema, migrations, backup, repositories | `domain`, `events` |
| L1 Domain | `domain/` | Pure types and rules, **no IO** | `events` |
| L0 Platform | `platform/` | Win32: window styles, tray, hotkeys, sleep detection, single instance | none |
| Cross-cutting | `events/` | Event types and bus | none |

Dependencies always point L4 → L3 → L2 → L1. `platform/` and `events/` are leaves.

### 3.2 Three hard rules

1. **`domain/` must not import `storage/` or `platform/`.**
   Domain rules must be testable without a database or an operating system. Break this once and `domain/` degrades into "structs attached to the database", taking the SPEC §8–13 object model with it.
2. **`storage/` must not import `platform/`.**
   The database path is injected by the composition root; the storage layer should not know Windows exists.
3. **`commands/` must not import `storage/` directly.**
   All data access goes through `services/`. Otherwise business rules leak out of the service layer into the IPC boundary as ad-hoc logic inside command handlers.

### 3.3 Composition root

`src-tauri/src/lib.rs` is the **only assembly point**: build storage → build services → register commands → create windows and tray.

Whether the layering holds depends entirely on this being the only file that imports across all layers. Any second place that does so is a design smell.

---

## 4. Runtime topology

```
Rust main process (single source of truth)
├── Event bus ─── internal domain events + broadcast to every window
├── SQLite ────── single file, WAL
├── Windows (each = separate webview = separate JS context, no shared memory)
│     main   primary window, 1100×760 (min 800×600)
│     hud    always-on-top / transparent / click-through / no focus / no taskbar entry
│     mini   interactive: pause / complete / switch / quick capture
└── Tray ──────── native menu, independent of any window's lifetime
```

### 4.1 Windows are subscribers, not state holders

State exists in exactly one place: Rust. Consequently:

- Closing the HUD, closing the main window, or minimizing to tray does not stop the timer or statistics.
- Main, HUD, and Mini read the same state — they cannot disagree ("HUD shows 01:17:34, main shows 01:17:29" is impossible).
- Reopening a window requires no state reconstruction, only a fresh snapshot fetch.

### 4.2 Multiple build entries

HUD and Mini use **separate Vite HTML entries** (`hud.html` / `mini.html`), not a route parameter on the main entry.

Rationale: the HUD must open instantly, stay resident, and render minimally. Going through the main entry means a transparent overlay loads the entire main-window page bundle — slower startup, higher memory, and more fragile transparent rendering. The cost is a `build.rollupOptions.input` entry in the Vite config.

### 4.3 The tray is independent of windows

Tray items (current task / pause / complete / quick capture / show HUD / quit) read and write Rust state directly and emit events. SPEC §42 requires "hide the taskbar icon, keep the tray, keep running in the background" — achievable only if the tray does not depend on a live window.

### 4.4 Single instance

A second launch must raise the existing instance's main window rather than start a second process (two processes would write the same SQLite file). This lives in `platform/single_instance.rs`.

### 4.5 HUD Locked / Edit

The HUD's two modes (SPEC §40) are **held in Rust**, because toggling them changes Win32 extended window styles; the frontend only sends a command.

---

## 5. IPC contract

| Channel | Direction | Purpose | Mechanism |
| --- | --- | --- | --- |
| **command** | Frontend → Rust | Every **intent** (start a task, complete a task, query a view) | `invoke()` |
| **event** | Rust → Frontend | Every **state-change notification**, delivered to all windows | `emit()` |

### 5.1 Command granularity

| Principle | Good | Bad |
| --- | --- | --- |
| One call completes one business transaction | `complete_task(task_id, quality)` internally ends the session → writes effort → updates statistics → emits events | `end_session()` + `update_task_status()` + `refresh_stats()` as three IPC calls |
| Fetch a view in one shot | `get_today_view()` returns everything the Today page needs | `list_tasks()` + `list_sessions()` + `get_stats()` |
| Name by intent | `start_task` / `pause_session` / `quick_capture` | `insert_session` / `update_row` |
| No N+1 | Lists carry the fields they need | One `get_task_detail` per row |

**DTO strategy**: reuse domain types via `#[derive(Serialize)]`, but return **dedicated aggregate DTOs** for view commands (`TodayView`, `ReportView`). Do not build a mirrored DTO per domain type.

### 5.2 Event envelope

Names follow SPEC §5.4's `entity.action` dotted form: `task.completed`, `session.started`, `session.finished`, `task.updated`, `project.updated`, `report.generated`.

```jsonc
{
  "event": "task.updated",
  "revision": 1042,                          // monotonically increasing global version
  "at": "2026-10-02T21:30:00+08:00",
  "payload": { }
}
```

`revision` is **new in this document** (SPEC does not mention it). Rationale: a window may reopen, reload, or miss an event. On mount it calls `get_snapshot()` to obtain the current revision; if a later event's revision skips a number, the window knows it missed something and refetches. Without it, a window that missed an event shows stale data forever.

### 5.3 Error contract

Every command returns `Result<T, AppError>`, serialized as:

```jsonc
{ "code": "TASK_NOT_FOUND", "message": "...", "detail": { } }
```

**Rust panics must never cross the IPC boundary**; they are converted to `AppError`.

---

## 6. Frontend state mirror

Whether Q3=A (Rust as single source of truth) actually holds depends on this layer.

**Single entry point**: `src/services/domainState.ts` owns all subscription and caching. **No page may call `listen()` itself** — otherwise duplicate subscriptions, leaks, and inconsistent state appear together.

| Event kind | Frontend behaviour | Rationale |
| --- | --- | --- |
| Domain events (`task.created` / `updated` / `completed`, `project.updated`, …) | **Cache-invalidation signal only** → refetch affected views | The frontend must never re-implement business rules. Patching state from events duplicates those rules, and two copies drift |
| High-frequency timer (`timer.tick`, ~1 Hz) | Replace displayed values directly: `{ session_id, elapsed_ms, remaining_ms }` | Refetching everything once per second is wasteful; this is display-only data the frontend performs no business logic on |

**Shape**: a very thin store (`useSyncExternalStore` + one `Map`). **No Redux / Zustand / Jotai** — the source of truth is Rust and the frontend is only a cache; those libraries solve "the frontend owns the truth", a problem Q3=A already removed.

**Type sync**: Rust DTOs → TS types must not be hand-written. Candidates are `ts-rs` and `tauri-specta`; pick one after confirming maintenance status and Tauri v2 compatibility as the first implementation step (see §7).

---

## 7. Assumptions still to be verified

These three are **not yet verified** and must not be treated as settled during implementation:

1. **Dynamic click-through on Windows** — the `WS_EX_LAYERED | WS_EX_TRANSPARENT` combination, and whether toggling it requires `SetWindowPos(..., SWP_FRAMECHANGED)` to take effect.
2. **Tauri v2 multi-window × Vite multi-entry** — how `WebviewWindowBuilder`'s url maps onto multi-entry build output.
3. **Synchronous `rusqlite` with Tauri commands** — which thread executes a synchronous command, and whether heavy queries should uniformly use `spawn_blocking`.

Findings must be written back into this document and into ADR-003 / ADR-005.

---

## 8. ADR index

| ID | Decision | Status |
| --- | --- | --- |
| ADR-001 | Keep Ant Design as the only component library; remove MUI / emotion | Decided, not yet applied |
| ADR-002 | Layered monolith with an internal event bus (rejects microkernel and multi-crate) | Decided |
| ADR-003 | Access SQLite through `rusqlite` + `bundled` | Decided |
| ADR-004 | Evaluate whether dockview is needed | **Open** |
| ADR-005 | Windows-first with a single platform boundary at `platform/` | Decided |
| ADR-006 | Rust is the single source of truth; React holds UI-local state only | Decided |

See [`03-adr.en.md`](03-adr.en.md).
