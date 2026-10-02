# Worktrace — Roadmap and Milestones

| Item | Value |
| --- | --- |
| Status | Design draft (under review) |
| Date | 2026-10-02 |
| Upstream | [`01-module-breakdown.en.md`](01-module-breakdown.en.md), [`04-functional-spec.en.md`](04-functional-spec.en.md) |
| Chinese version | [`05-roadmap.zh.md`](05-roadmap.zh.md) |

> This document turns SPEC §51–§56's version split into an executable list of specs and plans. A milestone is done when the corresponding `F-xxx` items in `04-functional-spec.en.md` all pass.

---

## 0. Step zero — land this design (before any feature work)

| # | Action | Output | Test |
| --- | --- | --- | --- |
| 0.1 | Commit this design set | All 14 files under `docs/superpowers/specs/2026-10-02-worktrace-architecture/` | Pushed |
| 0.2 | Apply ADR-001: remove MUI / emotion | `package.json` plus a regenerated `pnpm-lock.yaml` | `pnpm build` passes; `pnpm tauri dev` starts |
| 0.3 | Verify the three open assumptions and write findings back | Updates to `00-architecture` §7 and to ADR-003 / ADR-005 | Each moves from "to verify" to "verified" or "replaced" |
| 0.4 | Lay down the directory skeleton (**empty dirs and stubs, no implementation**) | `src-tauri/src/{platform,storage,domain,services,events,commands}/` and `src/{app,features,components,hooks,services,types}/` | `cargo check` passes |
| 0.5 | Decide ADR-004: keep or drop dockview | ADR-004 becomes decided | If dropped, `DockviewDemo` goes with it |

**0.3 comes first.** If any of the three assumptions fails, it may change the `platform/` or storage design — the earlier that surfaces, the cheaper.

---

## 1. Milestone overview

| Milestone | Goal | Done when | Critical path |
| --- | --- | --- | --- |
| **V0.1** | The base loop works: capture, clarify, time, count effort, plus Today / tray / HUD | F-001 … F-016 pass | `M02 → M01 → M04 → M05` |
| **V0.2** | It can measure: concurrency, interruption, weights, two effort measures, reports, search | F-101 … F-113 pass | `M06 → M07` |
| **V0.3** | It has AI: clarify, decompose, estimate, tag, prioritise, quality stats | F-201 … F-208 pass | `M10` |
| **V0.4** | It understands context: project context, decision log, versioned facts, documents | F-301 … F-305 pass | `M09` |
| **V0.5** | It can review: weekly review, KPA, capability model, rework detection | F-401 … F-405 pass | `M07 M08` |
| **V1.0** | Dependable long-term: hardened migration / backup / crash recovery, 30-day reliability run | F-501 … F-510 pass | `M01` |

---

## 2. V0.1 — Foundation

**Goal**: the smallest loop that is genuinely useful. When it is done you can record real work with it, however few features it has.

### Specs and plans needed, in order

| Order | Module | Suggested spec file | Note |
| --- | --- | --- | --- |
| 1 | M00 Platform | `M00-platform-spec` | **Only** single instance and a minimal tray; HUD window styles wait for step 8 |
| 2 | M03 Event bus | `M03-events-spec` | Event type table, envelope, `revision` persistence, `get_snapshot()` |
| 3 | M02 Domain model | `M02-domain-spec` | Entities, Task state machine, WorkSession lifecycle and invariants |
| 4 | M01 Storage | `M01-storage-spec` | Full DDL, migration framework, repository interfaces, backup |
| 5 | M04 Timer Engine | `M04-timer-spec` | Instant-based clock, `timer.tick`, sleep correction |
| 6 | M05 WorkSession | `M05-session-spec` | Lifecycle, invariants, crash-recovery marking |
| 7 | M12 Frontend shell | `M12-frontend-shell-spec` | Theme, layout, `domainState.ts`, Today + Inbox |
| 8 | M11 Window system | `M11-windows-spec` | Tray, basic HUD including the Locked/Edit modes |

### Can run in parallel
- Step 7's **shell portion** (theme, layout, routing) can start against mock data before step 4 completes.
- Steps 1 and 2 are independent of each other.

### Risks

| Risk | Surfaces at | Response |
| --- | --- | --- |
| Win32 click-through does not behave as expected | Step 8 | Already probed in step 0.3; if unworkable, degrade the HUD to always-on-top plus no-focus (dropping click-through) and record an ADR |
| Synchronous rusqlite blocks IPC | Step 4 | Thread model probed in step 0.3; use `spawn_blocking` uniformly if needed |
| Multi-entry HUD build does not work | Step 8 | Probed in step 0.3; fallback is a single entry with a route parameter, sacrificing startup speed |

---

## 3. V0.2 — Measurement

**Goal**: from "it records" to "it explains".

| Order | Module | Suggested spec file | Note |
| --- | --- | --- | --- |
| 1 | M06 Statistics | `M06-statistics-spec` | Both measures; the weight-normalisation strategy (an open point in SPEC §17/§18) is settled here |
| 2 | M04 extension | `M04-timer-spec` (addendum) | Pomodoro and time blocks |
| 3 | M05 extension | `M05-session-spec` (addendum) | Concurrent tasks, four execution modes, interruption |
| 4 | M13 Search | `M13-search-spec` | **Includes the FTS5 decision** |
| 5 | M08 Knowledge | `M08-knowledge-spec` | Proficiency and confidence model, estimate error |
| 6 | M07 Reports | `M07-report-spec` | Five ranges, filters, export |

### Can run in parallel
M13 Search is independent of M07 and M08.

### Watch out
**F-103 (human-effort accounting) decides this release.** It is the only SPEC §12 requirement written as a counter-example (1 h design + 1 h AI ≠ 2 h human). M06's spec must give it a dedicated test.

---

## 4. V0.3 — AI

**Goal**: AI joins as an enhancement layer, and **offline behaviour is unchanged**.

| Order | Module | Suggested spec file | Note |
| --- | --- | --- | --- |
| 1 | M10 AI Gateway | `M10-ai-gateway-spec` | Eight interfaces, provider abstraction, degradation paths, classification blocking |
| 2 | M10 feedback and quality | same (addendum) | `F-206`, `F-207` |

### Boundaries that must hold
- AI is never a hard dependency of the core (SPEC §5.2).
- AI suggestions never overwrite a field whose `source = user` (SPEC §5.3 / `F-205`).
- `STRICT_LOCAL` data never enters an outbound path (SPEC §31 / `F-208`) — **this needs a verifiable mechanism, not a promise**.

---

## 5. V0.4 — Context engine

| Order | Module | Suggested spec file |
| --- | --- | --- |
| 1 | M09 Context Engine | `M09-context-spec` (project context / ContextFact / decision log / documents / completeness) |

Depends on a settled M01 and M02; can overlap with parts of V0.5.

---

## 6. V0.5 — Review and capability model

| Order | Module | Suggested spec file | Note |
| --- | --- | --- | --- |
| 1 | M07 extension | `M07-report-spec` (addendum) | Weekly review |
| 2 | M07 KPA | same | Composite reporting material |
| 3 | M08 extension | `M08-knowledge-spec` (addendum) | Skill model, rework detection, knowledge analytics |

---

## 7. V1.0 — Stable and usable

**Goal**: from "works" to "safe to depend on for years".

| Order | Content | Note |
| --- | --- | --- |
| 1 | Harden M01 | Migration framework, automatic backup, and a complete restore path |
| 2 | Full crash-recovery scenarios | Drill power loss, kill, and system crash |
| 3 | Long-run verification | 30 continuous days of timing, 7 days of resident HUD (`F-506`, `F-508`) |
| 4 | Report reconciliability | Any range's report reconciles line by line with detail rows (`F-509`) |

---

## 8. Critical path and scheduling discipline

**Strictly serial critical path**:

```
M02 → M01 → M04 → M05 → M06 → M07
```

A slip anywhere here delays everything downstream. Protect it and let other work yield.

**Parallel pool** (off the critical path):
- M13 Search (needs M01/M02)
- M09 Context Engine (needs M01/M02, V0.4)
- M12's shell portion (mock data first)
- M00 / M03 (completable early in V0.1)

---

## 9. Convention for writing each spec

Every module spec uses the same skeleton so cross-references stay mechanical:

```markdown
# Mxx <module> design spec

| Item | Value |
| Status | draft / reviewed |
| Upstream | 00-architecture / 01-module-breakdown |
| Covers | F-xxx, F-yyy |
| Related ADRs | ADR-xxx |

## 1. Responsibility and boundary (including what it does NOT own)
## 2. Interface contract (commands / events / data structures)
## 3. Internal design
## 4. Errors and degradation
## 5. Test strategy (including invariants that must be covered)
## 6. Deviations from upstream documents
```

**Hard requirement**: section 5 must list the **invariants** the module guards and the tests for them. Without that, the spec is not finished. This is the only thing keeping ADR-002 (layering by discipline) from eroding.

---

## 10. On scheduling

This document **gives no calendar estimates**. The three unverified assumptions (see `00-architecture.en.md` §7) are unresolved, and any one of them failing changes the scale of the work. Fill in estimates once step 0.3 is complete and each module spec has been written.
