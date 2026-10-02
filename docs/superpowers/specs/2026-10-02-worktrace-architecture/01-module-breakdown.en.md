# Worktrace — Module Breakdown

| Item | Value |
| --- | --- |
| Status | Design draft (under review) |
| Date | 2026-10-02 |
| Upstream | [`00-architecture.en.md`](00-architecture.en.md) |
| Purpose | The **dispatch source for every later spec / plan**: each module gets its own spec → plan → implementation |
| Chinese version | [`01-module-breakdown.zh.md`](01-module-breakdown.zh.md) |

---

## 1. Overview

14 modules, each pinned to a layer. Dependencies always point downward; reverse dependencies are forbidden.

| Group | Module | Layer | Milestone |
| --- | --- | --- | --- |
| Infrastructure | M00 Platform | L0 `platform/` | V0.1 (single instance, tray) |
| | M01 Storage | L2 `storage/` | V0.1 |
| | M03 Event bus | cross-cutting `events/` | V0.1 |
| Domain | M02 Domain model | L1 `domain/` | V0.1 |
| Services | M04 Timer Engine | L3 `services/timer.rs` | V0.1 |
| | M05 WorkSession | L3 `services/session.rs` | V0.1 |
| | M06 Statistics | L3 `services/statistics.rs` | V0.2 |
| | M07 Reports & KPA | L3 `services/report.rs` | V0.2 |
| | M08 Knowledge / estimation | L3 `services/knowledge.rs` | V0.2 |
| | M09 Context Engine | L3 `services/context.rs` | V0.4 |
| | M10 AI Gateway | L3 `services/ai/` | V0.3 |
| | M13 Search | L3 `services/search.rs` | V0.2 |
| Shell | M11 Window system | L0 + `features/hud`·`features/mini` | V0.1 (tray + basic HUD) |
| | M12 Frontend skeleton | React `app/` + `features/` | V0.1 (Today + Inbox) |

---

## 2. Module contracts

Each row states: what it owns, what it must **not** own, its dependencies, the SPEC sections it implements, and its binding constraint.

### Infrastructure

| Module | Owns | Must not own | Depends on | SPEC | Binding constraint |
| --- | --- | --- | --- | --- | --- |
| **M00 Platform** | All Win32 and OS interaction: extended window styles (topmost / transparent / click-through / no focus / no taskbar entry), tray, global hotkeys, sleep and lock detection, single instance | Any business judgement; any domain type | — | §38–42 | Callable only from `services/` and above; `domain/` and `storage/` must not import it |
| **M01 Storage** | rusqlite connections and transactions, schema and migrations (`PRAGMA user_version` stepping), backup (`VACUUM INTO`), repository implementations, SQL aggregate queries | Business rules; orchestration beyond a transaction boundary | M02, M03 | §8–13, §45, §46 | Tables must contain **no** derived fields such as `remaining` or `elapsed` (see 02-data-model) |
| **M03 Event bus** | Domain event types, internal subscribe/dispatch, the global monotonic `revision`, broadcast to all windows, `get_snapshot()` support | Business logic; event persistence (no event sourcing in V0.1) | — | §5.4 | Carries **business state transitions only**; must never grow into a workflow engine (SPEC §50) |

### Domain

| Module | Owns | Must not own | Depends on | SPEC | Binding constraint |
| --- | --- | --- | --- | --- | --- |
| **M02 Domain model** | Pure types and rules: Goal / Project / Milestone / Task / Dependency / Tag / Knowledge / WorkSession / ContextFact / Decision; the Task state machine; WorkSession lifecycle invariants | Any IO; any database or platform call | M03 | §8–13, §16–22, §24 | Must be unit-testable with no database and no Windows |

### Services

| Module | Owns | Must not own | Depends on | SPEC | Binding constraint |
| --- | --- | --- | --- | --- | --- |
| **M04 Timer Engine** | Unified timing (Stopwatch / Countdown / Pomodoro / Time Block), the authoritative clock, ~1 Hz `timer.tick` broadcast, correction after system sleep | Effort attribution and statistics (M05 / M06) | M02, M03, M00 | §14, §15 | **No** `remaining -= 1` accumulation. Store only `started_at` / `target_end` / `paused_total_ms`; compute every displayed value on demand |
| **M05 WorkSession** | Session lifecycle (start / pause / resume / finish); concurrent tasks across FOREGROUND / BACKGROUND / PASSIVE / WAITING; interruption handling; completion quality; `needs_review` marking after a crash | Advancing time (M04); aggregating effort (M06) | M01, M02, M04, M03 | §11–13, §23, §24 | Invariant: at most one FOREGROUND session, enforced in the service layer (no schema unique constraint) |
| **M06 Statistics** | Both effort measures — associated duration (a tag counts in full) and weighted duration (allocated by weight); distribution across Project / Domain / Activity / Knowledge / Report tags; estimate-error statistics | Report layout and export (M07) | M01 (SQL aggregation), M02 | §17, §18, §22 | Aggregation is pushed down into SQL; never pull whole tables into memory |
| **M07 Reports & KPA** | Daily / Weekly / Monthly / Quarterly / Custom reports; KPA material (time invested + tasks done + milestones + issues + improvements); export to JSON / CSV / Markdown | The underlying statistical definitions (M06) | M06, M02 | §43, §44, §45 | — |
| **M08 Knowledge / estimation** | Proficiency and confidence model for knowledge tags; TaskKnowledge relations; estimate-error analysis and prediction from history | AI calls (M10); this module only produces structured features to feed it | M01, M02, M06 | §19–22 | — |
| **M09 Context Engine** | Project Context assembly; ContextFact (versioned facts with `superseded_by`); Decision Log; document association; classification (PUBLIC / INTERNAL / CONFIDENTIAL / STRICT_LOCAL); Context Completeness scoring | The actual AI call (M10) | M01, M02 | §25–31 | `STRICT_LOCAL` data must never enter any outbound path (contract with M10) |
| **M10 AI Gateway** | The unified interface (`clarify_task` / `decompose_task` / `estimate_duration` / `suggest_tags` / `suggest_priority` / `summarize_report` / `analyze_context` / `review_week`); AI output metadata (value / source / confidence / confirmed); user feedback capture; AI quality statistics | Letting a vendor SDK leak upward; never a hard dependency of the core | M09, M02 | §5.2, §5.3, §32–35 | Interface and implementation separated; every caller must degrade when AI is unavailable; AI suggestions must **not** overwrite fields whose `source` is `user` |
| **M13 Search** | Unified retrieval across Task / Project / Document / Decision / Knowledge / Context / Session | Semantic search (V0.1–V1.0 use SQL full-text / prefix matching only) | M01, M02 | §47 | Whether to enable SQLite FTS5 is decided in M13's own spec |

### Shell

| Module | Owns | Must not own | Depends on | SPEC | Binding constraint |
| --- | --- | --- | --- | --- | --- |
| **M11 Window system** | Creation and lifecycle of the main / HUD / Mini windows; the HUD's Locked and Edit modes; the tray; raising the existing instance on relaunch | What the windows display (M12) | M00, M03 | §38–42 | HUD and Mini use separate Vite entries; windows must never hold business state |
| **M12 Frontend skeleton** | Application shell (theme, routing, layout); the `features/` pages; the `src/services/domainState.ts` mirror; shared components under `components/`; auto-generated `types/` | Any business rule; any local persistence | All Rust modules (via IPC) | §6, §36, §37 | `features/` must not import each other; no page may call `listen()` itself |

---

## 3. Dependency graph and build order

```
M00 platform ─┐
M03 events   ─┼─→ M02 domain ─→ M01 storage ─→ M04 timer ─→ M05 session
              │                                      │           │
              │                                      └─────┬─────┘
              │                                            ▼
              │                            M06 statistics ─→ M07 report
              │                                   │      └─→ M08 knowledge
              │                                   └─→ M13 search
              └─→ M11 window system                     M09 context ─→ M10 ai
                                                         │
M12 frontend skeleton ←──────────────────────────────────┘ (via IPC; related to all)
```

### Recommended V0.1 order

1. **M00** — single instance plus a minimal tray
2. **M03** — event bus skeleton (types, revision, broadcast)
3. **M02** — domain model (Task / Project / Tag / WorkSession)
4. **M01** — storage (schema, migrations, repositories)
5. **M04** — Timer Engine
6. **M05** — WorkSession
7. **M12** — frontend skeleton (Today + Inbox)
8. **M11** — window system (tray + basic HUD)

### Work that can run in parallel

- **M09 → M10** (Context Engine and AI Gateway) depend only on M01/M02 and can start during V0.2.
- **M13 Search** depends on M01/M02 and is independent of M06/M07/M08.
- **M12's shell** (theme, layout, routing) can be built before M01 lands, against mock data.

### The critical path (strictly serial)

`M02 → M01 → M04 → M05 → M06 → M07` — a slip anywhere on this chain delays everything downstream. Protect it when scheduling.
