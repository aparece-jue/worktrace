# Worktrace — Functional Spec and Acceptance Criteria

| Item | Value |
| --- | --- |
| Status | Design draft (under review) |
| Date | 2026-10-02 |
| Upstream | [`../../PROJECT_SPEC.md`](../../PROJECT_SPEC.md), [`01-module-breakdown.en.md`](01-module-breakdown.en.md) |
| Version split follows | SPEC §51–§56 |
| Chinese version | [`04-functional-spec.zh.md`](04-functional-spec.zh.md) |

> Each item must have an observable acceptance criterion. If a criterion cannot be written, the requirement is not yet understood.

---

## 1. V0.1 — Foundation

SPEC §51 scope: Task, Project, Basic Tag, Timer, WorkSession, SQLite, Today, Inbox, Tray, Basic HUD.

### 1.1 Task core

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-001 | Inbox quick capture | A global hotkey opens an input; one line plus Enter creates the task; it appears in Inbox. **No project, tag, or date is required at capture time.** Capture to visible ≤ 3 s | M02 M12 | §3.1 §36 |
| F-002 | Clarify | A task can be moved out of Inbox to Ready, with next action, project, and tags set. Clarifying forces no field | M02 M12 | §3.1 |
| F-003 | Task state machine | All ten states supported: Inbox / Clarifying / Ready / Scheduled / Doing / Blocked / Waiting / Review / Done / Cancelled. **Illegal transitions are rejected and write nothing.** `Blocked` and `Waiting` are separately selectable | M02 | §9 |
| F-004 | Projects | Create, rename, archive projects; tasks can belong to a project; archived projects disappear from the new-task picker | M01 M02 M12 | §8.2 |
| F-005 | Basic tags | Create and apply Domain / Activity / Context / Report tags; a task may carry several | M01 M02 | §16 |

### 1.2 Timing and effort

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-006 | Timer (stopwatch + countdown) | Start, pause, resume, finish; a countdown gives a perceptible alert at zero | M04 | §14 |
| F-007 | Timing accuracy | **After 30 minutes of system sleep, the displayed value is within 1 second.** Timing continues while the window is hidden or minimized to tray | M04 M00 | §15 |
| F-008 | WorkSession segments | One task may be timed in several sittings, each an independent session; task effort is the sum of sessions minus pauses | M05 | §11 |
| F-009 | Timing decoupled from windows | Closing the main window, closing the HUD, or minimizing to tray never interrupts timing; reopening shows the correct value | M04 M11 | §42 |

### 1.3 Interface and shell

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-010 | Today page | Shows today's plan, the current task, time worked today, and remaining tasks; the current task is visually unambiguous | M06 M12 | §37 |
| F-011 | Tray | Offers current task, pause, complete, quick capture, show HUD, quit. **With every window closed, the tray stays usable and timing continues** | M11 | §42 |
| F-012 | Basic HUD | A separate window with all five properties: always-on-top, transparent, click-through, never focused, no taskbar entry. Shows the current task name and timing progress | M11 M00 | §38 §40 |
| F-013 | HUD mode toggle | Switch between Locked (click-through, non-interactive) and Edit (draggable, resizable, configurable); the change takes effect immediately | M11 M00 | §40 |

### 1.4 Data and reliability

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-014 | Local-first | All data lives in local SQLite. **With the network cable unplugged, every V0.1 feature works** with no error and no degraded-mode notice | M01 | §5.1 |
| F-015 | Crash recovery | After the process is killed and restarted, unfinished sessions are flagged for confirmation and appear in the UI. **The system never backfills effort.** Only user confirmation counts it | M01 M05 | §46 §57 |
| F-016 | Single instance | A second launch raises the existing window rather than creating a second process, and does not corrupt the database | M00 | §42 |

---

## 2. V0.2 — Knowledge and statistics

SPEC §52 scope: Knowledge Tag, Weighted Tag, Reports, TimeBlock, Pomodoro, Interruption, Multi Session.

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-101 | Concurrent tasks | Several sessions may run at once; **at most one FOREGROUND** (a second attempt is rejected with a message) | M05 | §12 |
| F-102 | Execution mode | Each session is FOREGROUND / BACKGROUND / PASSIVE / WAITING | M05 | §12 |
| F-103 | Human-effort accounting | **1 h of foreground design plus 1 h of background AI generation must report 1 h of human effort, not 2 h** | M06 | §12 |
| F-104 | Pomodoro | 25/5, 50/10, 90/20, and custom; work and break spans are distinguishable | M04 | §14 |
| F-105 | Time blocks | Tasks can be placed in a time range (e.g. 14:00–15:30); Today renders by block | M04 M12 | §14 §37 |
| F-106 | Interruption handling | Starting another task while timing pauses the original and records the interruption; "interrupted N times this week, X lost" is queryable | M05 M06 | §23 |
| F-107 | Knowledge tags | Hierarchical knowledge tags (Electronics → Analog → ADC) | M01 M02 | §16.3 |
| F-108 | Tag weights | Per-task tag weights (Design 60% / Research 20% / Review 20%) | M01 M02 | §17 |
| F-109 | Two effort measures | **Associated duration** (tag counts in full) and **weighted duration** (allocated by weight) are separately queryable; the same task yields different, explainable results under each | M06 | §18 |
| F-110 | Search | Unified retrieval across task / project / document / decision / knowledge / context / session, grouped by type | M13 | §47 |
| F-111 | Reports | Daily / Weekly / Monthly / Quarterly / Custom, filterable by Project / Goal / Domain / Activity / Knowledge / Report Tag / Task / Milestone | M07 | §43 |
| F-112 | Export | JSON / CSV / Markdown; exported content matches what the UI shows | M07 | §45 |
| F-113 | Estimate-error statistics | Conclusions such as "this task class is underestimated by X% on average", computed from estimated versus actual | M06 M08 | §22 |

---

## 3. V0.3 — AI assistance

SPEC §53. **Shared precondition for every item: with AI unavailable, V0.1 and V0.2 behaviour is unaffected and no error dialog appears.**

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-201 | AI clarification | Vague tasks ("look into it") receive concretisation suggestions, adoptable or dismissable in one action | M10 | §3.4 §32 |
| F-202 | AI decomposition | Complex tasks receive subtask suggestions; the user chooses which to adopt | M10 | §3.2 §32 |
| F-203 | AI estimation | An estimate **carrying a confidence value**; on adoption it is written to `estimated_json` with `source = ai` | M10 M02 | §22 §33 |
| F-204 | AI tag suggestions | Suggests Domain / Activity / Knowledge tags | M10 | §32 |
| F-205 | AI priority suggestion | Suggests priority and **must never overwrite any value whose `source = user`**; on conflict the user value stands and a notice appears | M10 M02 | §5.3 |
| F-206 | AI quality statistics | Estimate error, tag acceptance rate, decomposition acceptance rate, priority edit rate | M10 | §35 |
| F-207 | AI feedback | Quick feedback per suggestion: too fine / too coarse / estimate too long / too short / wrong tag / wrong category / unreasonable priority | M10 | §34 |
| F-208 | Classification blocking | Context marked `STRICT_LOCAL` **never appears in any outbound request**; verifiable by packet capture or logs | M09 M10 | §31 |

---

## 4. V0.4 — Context engine

SPEC §54.

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-301 | Project Context | A project stores goals, constraints, key parameters, architecture, decisions, documents, terminology | M09 | §26 |
| F-302 | Decision Log | Record decision, rationale, date; later review answers "why was this chosen then" | M09 | §29 |
| F-303 | Versioned ContextFact | Changing a parameter keeps the old value marked `superseded`; **the AI can only read the currently effective value** | M09 | §28 |
| F-304 | Document association | Associate PDF / Word / Excel / Markdown / images / CSV / JSON / netlist / code / logs and extract context from them | M09 | §30 |
| F-305 | Context Completeness | Reports a percentage (e.g. 88%) and **asks the user only when genuinely critical information is missing** | M09 | §27 |

---

## 5. V0.5 — Review and capability model

SPEC §55.

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-401 | Weekly review | Reports completion rate, project progress, blocked tasks, delay reasons, effort distribution, estimate error, interruptions, AI suggestion accuracy | M07 M06 | §3.6 |
| F-402 | KPA report | Produces composite material (time invested + tasks completed + milestones + outcomes + issues + improvements), not a bare total | M07 | §44 |
| F-403 | Skill model | Each knowledge item carries both `skill_level` and `confidence`; confidence is visibly low when evidence is thin | M08 | §19 §20 |
| F-404 | Rework detection | "Fast but reworked" is distinguished from "fast and clean" in the capability model, using `quality` | M08 | §24 |
| F-405 | Knowledge analytics | Knowledge usage frequency and recent movement are visible | M08 | §19 |

---

## 6. V1.0 — Stable and usable

SPEC §56's ten goals, restated as verifiable items.

| ID | Goal | Acceptance criterion | Depends on |
| --- | --- | --- | --- |
| F-501 | Stable data structure | Migration steps forward from any historical version, each step inside a transaction | M01 |
| F-502 | Stable migration | A backup is taken before migrating; a failed migration does not damage the original database | M01 |
| F-503 | Reliable backup | Automatic backups (startup + daily) produce usable files; restoring from one yields complete data | M01 |
| F-504 | Crash recovery | Power loss, kill, and system crash all leave consistent data with no silent loss | M01 M05 |
| F-505 | Mature task flow | Capture through completion needs no manual database edits | M02 M12 |
| F-506 | Reliable timing | Over 30 continuous days, cumulative error ≤ 1 second per day | M04 |
| F-507 | Complete review | Daily and weekly review form a closed loop (SPEC §3.6) | M07 |
| F-508 | Stable HUD | Resident for 7 continuous days without crashing, leaking memory, or stealing focus | M11 |
| F-509 | Reliable reports | Any time range's report figures reconcile line by line against the detail rows | M07 |
| F-510 | Usable AI assistance | Offline, every core feature works with no degraded-mode notice | M10 |

---

## 7. Global non-functional requirements

| Item | Requirement |
| --- | --- |
| Works offline | Everything except V0.3+ AI features works with no network and no error |
| Data is portable | At least JSON / CSV / Markdown export; the user is never locked inside the database (SPEC §45) |
| Data is explainable | AI output carries source, confidence, and change history where possible (SPEC §57-3) |
| Management cost | Recording a task must not take more steps than writing it on paper (SPEC §57-1) |
| Long-term first | Any design that trades long-term maintainability for looking smart on day one is rejected (SPEC §57-5) |

---

## 8. Not accepted (non-goals)

No acceptance criteria are written for these, because SPEC §50 excludes them: plugin marketplace, generic workflow engine, general agent platform, full CAD automation, IDE replacement, Office replacement, universal automation platform.
