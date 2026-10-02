# Worktrace — Glossary

| Item | Value |
| --- | --- |
| Status | Design draft (under review) |
| Date | 2026-10-03 |
| Standing | Terminology reference; revised 00/02/04 own business rules. Resolve terminology conflicts together |
| Chinese version | [`99-glossary.zh.md`](99-glossary.zh.md) |

> The "Never confuse with" column is the main value here: it records the term pairs this project gets wrong most easily.

---

## 1. Time and effort (the most confusable group)

| English | 中文 | Definition | Never confuse with |
| --- | --- | --- | --- |
| Elapsed Time | 流逝时长 | Wall-clock time that passed. 09:00 to 10:00 is one hour regardless of what happened inside it | **Never say just "time" or "hours"** — always name which measure |
| **Human Effort** | **人工工时** | Time the user personally invested. Only FOREGROUND sessions count | Not a synonym for "actual duration" or "elapsed time" |
| Machine / AI Process Time | 机器时长 | Time consumed by background AI generation, a running simulation, etc. BACKGROUND and PASSIVE count here | Never counted as human effort |
| Associated Duration | 关联时长 | Measure one: if a task carries a tag, that tag counts the task's full effort | A **different measure** from weighted duration; neither substitutes for the other |
| Weighted Duration | 加权工时 | Measure two: effort allocated by tag weight (ADC 50% × 2h = 1h) | See above |
| Estimated Duration | 估时 | The pre-work estimate. May come from AI (`source = ai`) or the user | Not the same as planned duration |
| Planned Duration | 计划时长 | What the user actually scheduled | Not the same as estimated duration |
| Actual Duration | 实际时长 | The true elapsed cost, aggregated from sessions | **Not equal to human effort** when background work is involved |

> SPEC §12's hard rule: `1 h designing (foreground) + 1 h of AI generation (background)` **must** report 1 h of human effort, not 2 h.

---

## 2. Core objects

| English | 中文 | Definition | Never confuse with |
| --- | --- | --- | --- |
| Goal | 目标 | A long-term objective, e.g. "finish the industrial IO control board" | |
| Project | 项目 | A long-lived context container (goals + constraints + decisions + tasks + documents + knowledge) | Not merely a grouping of tasks |
| Milestone | 里程碑 | A project stage outcome, e.g. "schematic design frozen" | |
| Task | 任务 | The primary managed object. Every node from Task down to the leaves of the WBS tree is a Task | |
| Action | 行动 | **Simply a leaf Task**; no separate table (see 02-data-model §7) | Not a distinct entity type |
| WorkSession | 会话 | A start-to-finish work session which may contain pauses; effort aggregates effective intervals | Not the same level as a segment |
| SessionSegment | 分段 | Future activity classification, distinct from V0.1 timing work_interval | |
| Dependency | 依赖 | Task relations: `blocks` / `depends_on` / `related` / `parallel` | |

---

## 3. Task states

| English | 中文 | Definition | Never confuse with |
| --- | --- | --- | --- |
| Inbox | 收件箱 | Captured, not clarified | |
| Clarifying | 理清中 | Working out the next action | |
| Ready | 就绪 | Actionable, unscheduled | |
| Scheduled | 已排期 | Placed in a time block | |
| Doing | 进行中 | In progress | |
| **Blocked** | **受阻** | **I cannot proceed** (missing skill or decision, unfinished prerequisite) | **A different state from Waiting; never merge them** |
| **Waiting** | **等待** | **Waiting on something external** (a reply, a part, a test, an approval) | See above |
| Review | 待检查 | Finished, pending check | |
| Done | 完成 | Reopen explicitly to become Ready again | |
| Cancelled | 已取消 | Reopen explicitly to become Ready again | |

---

## 4. Execution modes

| English | 中文 | Counts as human effort | Definition |
| --- | --- | --- | --- |
| FOREGROUND | 前台 | ✅ | Current human work; one running session, several paused allowed |
| BACKGROUND | 后台 | ❌ | Runs in parallel, not driven by the user (e.g. AI writing a document) |
| PASSIVE | 被动 | ❌ | A machine process (e.g. an LTspice simulation running) |
| WAITING | 等待 | ❌ | Blocked on an external condition |

---

## 5. Tag kinds (five)

| English | 中文 | Definition |
| --- | --- | --- |
| Domain | 领域 | Which field the task belongs to: Hardware / Firmware / Software / Documentation / Management |
| Activity | 活动 | What is actually being done: Design / Research / Calculation / Coding / Debug / Review / Testing |
| Knowledge | 知识 | What knowledge is required. **The only kind with a hierarchy** (Electronics → Analog → ADC) |
| Context | 上下文 | What conditions are required: PC / Internet / OrCAD / Lab / High Focus |
| Report | 汇报 | For weekly reports and KPA: product development / technical research / problem analysis / verification testing |

> "Context" carries two meanings: a **tag kind** (this section) and **the Context Engine's context** (next section). Always qualify in writing — "Context-kind tag" versus "context bundle".

---

## 6. Capability model and quality

| English | 中文 | Definition | Never confuse with |
| --- | --- | --- | --- |
| Skill Level | 熟练度 | Experimental ability estimate; Unknown if disabled/insufficient evidence; default to usage facts | **Must coexist with confidence**; alone it produces wrong conclusions |
| Confidence | 置信度 | How much evidence backs that estimate | See above |
| Rework | 返工 | Work redone after being considered finished | |
| Completion Quality | 完成质量 | `normal` / `reworked` / `review_failed` / `partially_done` / `abandoned` | |
| Interruption | 打断 | A second task starts while timing, pausing the original session | |

> The original numeric example is not an acceptance threshold; show facts and samples first. Scoring enablement is R-06.

---

## 7. Context engine

| English | 中文 | Definition |
| --- | --- | --- |
| Context Bundle | 上下文包 | Task context + project context + related documents + knowledge + history + decisions |
| ContextFact | 上下文事实 | A project-level fact (e.g. `Pt1000 current = 0.2mA`) |
| Superseded | 已取代 | The state of a fact replaced by a newer value. **The old value is kept, never overwritten** (versioning) |
| Decision Log | 决策日志 | Records decision, rationale, and date so "why was this chosen then" stays answerable |
| Context Completeness | 上下文完整度 | Action-specific required-input gaps, no global percentage |

---

## 8. Interface parts

| English | 中文 | Definition | Never confuse with |
| --- | --- | --- | --- |
| Main Window | 主窗 | The ordinary primary interface | |
| HUD / OSD | 抬头显示 | An always-on-top, transparent, click-through, never-focused window with no taskbar entry | |
| Mini Controller | 迷你控制器 | An **interactive** small window: pause / complete / switch / quick capture | **A different thing from the HUD**: the HUD is click-through and unclickable, the Mini is clickable |
| System Tray | 托盘 | Native tray menu, independent of any window's lifetime | |
| Locked | 锁定模式 | HUD state: click-through, unselectable, undraggable | |
| Edit | 编辑模式 | HUD state: movable, resizable, configurable | |

---

## 9. Architecture and implementation

| English | 中文 | Definition |
| --- | --- | --- |
| Single Source of Truth | 唯一真相源 | Domain state exists in exactly one place, on the Rust side (ADR-006) |
| Command | 命令 | The frontend → Rust request/response channel |
| Event | 事件 | The Rust → frontend broadcast channel |
| Envelope | 信封 | The common outer structure of an event: `{event, data_epoch, revision, at, payload} (business events; at is Unix milliseconds)` |
| revision | 修订号 | Durable business-transaction revision; ticks/heartbeats do not increment it (ADR-010) |
| Snapshot | 快照 | The full current state returned by `get_snapshot()`, used by a window to align on mount |
| State Mirror | 状态镜像 | The frontend cache layer `src/services/domainState.ts`, the only subscription entry point |
| Invalidation Signal | 失效信号 | The frontend role of a domain event — triggers a refetch rather than patching state |
| needs_review | 待确认 | Uncertain-interval flag mirrored by session; no running occupancy; only that interval awaits confirmation |
| Platform Layer | 平台层 | `src-tauri/src/platform/`, the only boundary for Win32 calls (ADR-005) |
| Composition Root | 组合根 | `lib.rs`, the only place that assembles all layers |
| Domain Layer | 领域层 | `domain/`, pure types and rules with no IO |

---

## 10. Methodology

| English | 中文 | Definition |
| --- | --- | --- |
| Capture | 捕获 | Write down what needs doing |
| Clarify | 理清 | Establish what it is and what the next action is |
| Plan | 计划 | Decompose, estimate, prioritise |
| Track | 记录 | Record real effort, switches, and interruptions |
| Review | 回顾 | Analyse results, produce reports, improve prediction accuracy |
| WBS | 工作分解结构 | Goal → Project → Milestone → Task → Action |

## 11. Revision additions

| Term | Definition |
| --- | --- |
| work_interval | Effective start/end facts; pause closes, resume opens; clipped to report ranges |
| source | user/rule/ai origin; adopting AI keeps its origin |
| confirmed_at | User-confirmed authority; no background overwrite of confirmed values |
| recovering | Recovery record with uncertain intervals; trusted closed effort retained; never advances |
| tick_seq | Per-run display sequence, not durable business revision |
| Provisional | Running interval as of snapshot time, separate from confirmed closed time |
| Unallocated | Remaining human effort when per-kind weights sum below 1 |

See 02/06 for origin/confirmation, sleep and historical classification defaults. All documents remain review drafts.

`data_epoch`: database UUID renewed on create/restore; revisions compare only within an epoch. `session_version`: timer state version rejecting late pre-pause ticks. `needs_review`: interval uncertainty mirrored by session summary; trusted closed intervals still count.

R-01–R-08 are approved; this protocol/recovery revision awaits review.

Current scope (2026-10-03): personal work records and task management, including capability gaps and KPA evidence. External Agents and companion skills handle file reading, OCR, extraction and full-text search. See [scope and import contract](07-scope-and-agent-import.en.md).


Reference: title/path/URL/source locator, no body. Agent Import: validated/reviewed per-item adoption of external skill JSON. Capability Profile: evidence, self-report and optional inference connected to actions. KPA Evidence: traceable work dates/outcomes/evidence.

See [08: timing, phases, AI and report snapshots](08-implementation-contracts.en.md).

duration_ms: trusted interval duration validated against accounting endpoints; Attribution Endpoint: start wall plus trusted monotonic elapsed; Checkpoint: successfully persisted trusted clock mapping; phase: Pomodoro work/break; report_snapshot: immutable confirmed report content/facts.
