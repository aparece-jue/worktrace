# Worktrace — Functional Spec and Acceptance Criteria

| Item | Value |
| --- | --- |
| Status | Revised draft for user review |
| Date | 2026-10-02 |
| Upstream | [`../../../PROJECT_SPEC.md`](../../../PROJECT_SPEC.md), [`01-module-breakdown.en.md`](01-module-breakdown.en.md) |
| Version split | Revised proposal in 05; approved R-01–R-08 in 06; this revision awaits review |
| Chinese version | [`04-functional-spec.zh.md`](04-functional-spec.zh.md) |

> Each item must have an observable acceptance criterion. If a criterion cannot be written, the requirement is not yet understood.

---

## 1. V0.1 — Recording loop (F-012/F-013 belong to V0.1b)

Revised proposal: recording/correction/recovery/Today/tray/minimal export and review first; HUD/global hotkeys move to V0.1b. Expose states by milestone, not a mandatory linear workflow.

### 1.1 Task core

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-001 | Inbox quick capture | One line plus Enter in the main window creates a visible task without project/tag/date; reject empty title. Global hotkey is V0.1b, not a V0.1 blocker | M01 M02 M12 | §3.1 §36 |
| F-002 | Clarify | Inbox may move directly to Ready; fields optional; start may clarify and start atomically | M01 M02 M05 M12 | §3.1 |
| F-003 | Task transitions | Follow the full table in 02 §5; Scheduled is not exposed in V0.1; illegal transitions write nothing; explicit reopen retains history | M01 M02 M05 M12 | §9 |
| F-004 | Projects | Create, rename, archive projects; tasks can belong to a project; archived projects disappear from the new-task picker | M01 M02 M12 | §8.2 |
| F-005 | Basic tags | Create and apply Domain / Activity / Context / Report tags; a task may carry several | M01 M02 | §16 |

### 1.2 Timing and effort

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-006 | Stopwatch/countdown | Pause freezes values; resume preserves budget; finish while paused works; expiry alerts only; persist effective work intervals | M04 M05 | §14 |
| F-007 | Clock/system events | Apply approved R-02 sleep/lock policy. Approved foreground pause excludes 30 minutes of suspension and never auto-resumes; clock changes in either direction require explainable reconciliation, never negative effort; normal-clock test error ≤1 second | M04 M05 M00 | §15 |
| F-008 | Sessions and intervals | Pause/resume retains a session, restart after finish creates another; intervals are the accounting source; pauses count in no daily total | M01 M05 | §11 |
| F-009 | Window-independent timing | Closing/hiding windows keeps the core alive; reopening queries immediately; explicit quit follows the data model | M04 M05 M11 M12 | §42 |

### 1.3 Interface and shell

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-010 | Today/minimal statistics | Daily selection, current task, confirmed human effort, provisional live effort and pending time shown separately; no time-block dependency; unpaused 23:50–00:10 splits into ten minutes each day | M06 M12 | §37 |
| F-011 | Tray | Offers current task, pause, complete, quick capture, quit; show HUD added in V0.1b. **With every window closed, the tray stays usable and timing continues** | M11 | §42 |
| F-012 | Basic HUD (V0.1b) | Topmost/transparent/click-through/no focus/no taskbar; verify DPI/multiple displays/reopen; failed properties require a reviewed alternative, not a false pass | M11 M00 M12 | §38 §40 |
| F-013 | HUD modes (V0.1b) | Locked/Edit switches immediately; edit supports move/resize, Locked does not steal input; capture-hotkey conflicts offer configurable settings | M11 M00 M12 | §40 |

### 1.4 Data and reliability

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-014 | Local-first | All data lives in local SQLite. **With the network cable unplugged, every V0.1 feature works** with no error and no degraded-mode notice | M01 | §5.1 |
| F-015 | Immediate-restart recovery | Restart within ten seconds: uncertain open intervals enter recovering; trusted paused sessions stay paused. Retain confirmed closed effort; distinguish discarding the uncertain interval from voiding the entire session; audit both | M01 M05 M12 | §46 §57 |
| F-016 | One core | Temporary second-launch process forwards activation and exits; no duplicate DB/timer initialization; raises existing window | M00 M11 | §42 |

---

### 1.5 Corrections, export and protection (new)

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-017 | Manual correction | Add missed work, edit endpoints and void mistakes with audit; reject negative/overlapping human intervals; Today/export reconcile afterwards | M01 M05 M06 M12 | §11 §57 |
| F-018 | Minimal export/review | JSON detail export carries schema_version/units/timezone/generated time; Markdown weekly summary includes human effort/completed tasks/pending records; auditable without AI | M07 M06 M12 | §43 §45 |
| F-019 | Initial backup/restore | WAL-consistent backup; maintenance mode validates candidate, creates fresh data_epoch/run_id, cancels old requests and clears caches; lower-revision restore rejects old responses; failure retains original usable DB and re-handshakes | M01 M12 | §46 |
| F-020 | Multi-window convergence | Resolve snapshot/listener races, reordering and last-event loss; visible windows check epoch/revision within 30 seconds; reject old epochs and pre-pause session_version ticks; ticks never increment revision | M03 M01 M12 | §5.4 |

---

## 2. V0.2 — Knowledge and statistics

SPEC §52 scope: Knowledge Tag, Weighted Tag, Reports, TimeBlock, Pomodoro, Interruption, Multi Session.

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-101 | Concurrency | At most one running FOREGROUND, multiple paused allowed; direct conflicting start/resume rejected; explicit switch atomically pauses old and starts new | M01 M05 | §12 |
| F-102 | Execution mode | Each session is FOREGROUND / BACKGROUND / PASSIVE / WAITING | M05 | §12 |
| F-103 | Human-effort accounting | **1 h of foreground design plus 1 h of background AI generation must report 1 h of human effort, not 2 h** | M06 | §12 |
| F-104 | Pomodoro | 25/5, 50/10, 90/20, and custom; work and break spans are distinguishable | M04 | §14 |
| F-105 | Time blocks | Multiple blocks per task; unscheduling preserves effort; Scheduled introduced here; timezone and half-open ranges explicit | M01 M02 M12 | §14 §37 |
| F-106 | Interruptions | Explicit interruption atomically pauses old and starts new; show count and inserted-task human duration, mark an unresumed original as open; duration is not automatically productivity loss | M05 M06 | §23 |
| F-107 | Knowledge tags | Hierarchical knowledge tags (Electronics → Analog → ADC) | M01 M02 | §16.3 |
| F-108 | Tag weights | Per kind finite weights in 0..1; sum above 1 rejected, below 1 leaves Unallocated; Knowledge weights only in task_tag | M01 M02 M12 | §17 |
| F-109 | Two measures | Same weighted tag supports both: a 50% tag on 2h yields associated 2h and weighted 1h; associated sums non-additive, allocation never crosses kinds; history policy explicit | M06 | §18 |
| F-110 | Staged search | V0.2 Task/Project/Goal/Milestone/Tag/Session; V0.4 adds Document/Decision/ContextFact; test short Chinese terms, RS485/model IDs/paths; no categories for absent entities | M13 | §47 |
| F-111 | Reports | Daily / Weekly / Monthly / Quarterly / Custom, filterable by Project / Goal / Domain / Activity / Knowledge / Report Tag / Task / Milestone | M07 | §43 |
| F-112 | Export | JSON / CSV / Markdown; exported content matches what the UI shows | M07 | §45 |
| F-113 | Estimate error | Retain pre-start baseline and measure; zero estimates excluded from percentages; incomplete/pending/anomalous data separate; show samples/median error, never mix human/machine | M06 M08 | §22 |

---

## 3. V0.3 — AI assistance

Unavailable AI leaves the core unchanged. An explicitly requested AI action reports failure/retry non-blockingly rather than silently swallowing it or disrupting unrelated core work.

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-201 | AI clarification | Vague tasks ("look into it") receive concretisation suggestions, adoptable or dismissable in one action | M10 | §3.4 §32 |
| F-202 | AI decomposition | Complex tasks receive subtask suggestions; the user chooses which to adopt | M10 | §3.2 §32 |
| F-203 | AI estimate history | Retain origin/model/input version; adoption keeps source=ai and sets confirmed_at; no background overwrites of confirmed values; stale inputs invalidate results; unknown confidence is not fabricated | M10 M01 M02 | §22 §33 |
| F-204 | AI tag suggestions | Suggests Domain / Activity / Knowledge tags | M10 | §32 |
| F-205 | AI priority | No background overwrite of any confirmed value, including adopted AI; explicit edits/re-adoption allowed; show suggestion separately | M10 M02 | §5.3 |
| F-206 | Auditable AI metrics | Denominator includes valid accepted/edited/rejected outcomes; retain suggestion_id/model/prompt version; show acceptance and error separately, never claim equivalent saved time | M10 M01 M06 | §35 |
| F-207 | AI feedback | Quick feedback per suggestion: too fine / too coarse / estimate too long / too short / wrong tag / wrong category / unreasonable priority | M10 | §34 |
| F-208 | Minimal classification | V0.3 task/project default STRICT_LOCAL; minimal M09 filters before outbound; mixed levels/derived summaries/logs/provider switches cannot bypass; verify mock outbound payloads without logging sensitive text | M09 M10 M01 | §31 |

---

## 4. V0.4 — Context engine

SPEC §54.

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-301 | Project Context | A project stores goals, constraints, key parameters, architecture, decisions, documents, terminology | M09 | §26 |
| F-302 | Decision Log | Record decision, rationale, date; later review answers "why was this chosen then" | M09 | §29 |
| F-303 | Fact versioning | One current project/key fact; replacement atomic; new requests use current values and old-input suggestions become stale | M09 M01 | §28 |
| F-304 | Document association | Associate PDF / Word / Excel / Markdown / images / CSV / JSON / netlist / code / logs and extract context from them | M09 | §30 |
| F-305 | Explainable context check | Show present/missing critical information and sources; prompt only for missing requirements; percentages need public denominator/weights/unknown rules, otherwise no precise score | M09 | §27 |

---

## 5. V0.5 — Review and capability model

SPEC §55.

| ID | Feature | Acceptance criterion | Depends on | SPEC |
| --- | --- | --- | --- | --- |
| F-401 | Extended weekly review | Simple weekly review already exists in V0.1; extend with blockers/delays/error/interruptions/AI effects; insufficient evidence Unknown, no fabricated ability/benefit claims | M07 M06 | §3.6 |
| F-402 | KPA report | Produces composite material (time invested + tasks completed + milestones + outcomes + issues + improvements), not a bare total | M07 | §44 |
| F-403 | Experimental capability model | Default to usage facts/sample counts; enabled scores require algorithm version/confidence or Unknown/limitations; low evidence never yields definitive grades; enablement pending review | M08 | §19 §20 |
| F-404 | Rework detection | "Fast but reworked" is distinguished from "fast and clean" in the capability model, using `quality` | M08 | §24 |
| F-405 | Knowledge analytics | Knowledge usage frequency and recent movement are visible | M08 | §19 |

---

## 6. V1.0 — Stable and usable

SPEC §56's ten goals, restated as verifiable items.

| ID | Goal | Acceptance criterion | Depends on |
| --- | --- | --- | --- |
| F-501 | Stable structure | Migrate every published historical schema transactionally; refuse writes to future-version DBs; FK/integrity checks pass | M01 |
| F-502 | Stable migrations | Pre-migration backup exists from V0.1; injected failure leaves original usable and restart does not duplicate migration; failed backup prevents migration | M01 |
| F-503 | Reliable backup | Automatic backups (startup + daily) produce usable files; restoring from one yields complete data | M01 |
| F-504 | Hardened recovery | Power loss/kill/crash preserve committed data without partial transactions; uncertain time since heartbeat is visible and confirmable, not a promise of zero unpersisted loss | M01 M05 |
| F-505 | Mature task flow | Capture through completion needs no manual database edits | M02 M12 |
| F-506 | Reliable timing | Normal-clock 30-day run, simulated and real-clock checks; display/timing error ≤1 second/day; suspend/clock changes follow chosen policy, normal pauses are not drift | M04 M05 |
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

## 9. Acceptance environment and boundaries

Performance thresholds name hardware/data size/scope/baseline; capture latency excludes typing time. Classify backups/exports too. Integration cases cover migration failure, disk-full, duplicate submission, stale edits, pauses across midnight and corrections. Precise ability/completeness scores need explainable algorithms; otherwise Unknown. This revision awaits user review and product acceptance has not been executed.
