# Worktrace — Roadmap and Milestones

Status: revised proposal for user review. Date: 2026-10-03. [Chinese](05-roadmap.zh.md).
Upstream: [acceptance](04-functional-spec.en.md), [modules](01-module-breakdown.en.md). [Pending choices](06-review-notes.en.md).

## 0. Review and incremental delivery

This revision changes documentation only, not SQLite/timer/HUD implementation. Review it, then probe DB blocking boundaries and clock policy. HUD/multi-entry probes are independent of the recording loop. Creating all empty directories or pushing documents is not a prerequisite for feature work; publishing/commits follow the user's workflow.

Apply dependency cleanup and decide production dockview after review; preserve copied components. Verify Rust→TS generator maintenance/Tauri 2 compatibility before selecting it.

## 1. Acceptance mapping

| Milestone | Goal | Required criteria |
| --- | --- | --- |
| V0.1 | Capture→time→correct/recover→Today→minimal export/review→backup | F-001…F-011, F-014…F-020 |
| V0.1b | Desktop enhancement: HUD/modes/global capture hotkey | F-012/F-013 plus F-001 hotkey extension |
| V0.2 | Concurrency/interruption/weights/scheduling/reports/existing-entity search | F-101…F-113 |
| V0.3 | AI/history/feedback/selected-input confirmation | F-201…F-208 |
| V0.4 | Brief facts/decisions, references, Agent imports and record search | F-301…F-305 plus F-110 extension |
| V0.5 | Extended review/KPA/knowledge facts/experimental capability model | F-401…F-405 |
| V1.0 | Automated backup/hardened migration/recovery/long-run verification | F-501…F-510 |

V0.1 Today is a daily selection, not a calendar; Goal/Milestone/WBS are not required. Scheduled begins in V0.2. Precise skill scores are experimental and never mandatory under sparse evidence.

## 2. V0.1 vertical delivery

1. M02/M01 minimal Project/Task/Tag CRUD, migrations/constraints/revision/errors; M00 ownership before DB initialization.
2. M04/M05 start/pause/resume/finish with durable work_interval. Test paused finish/restart, concurrent start/resume, clock changes and overlap.
3. M12/M03 Inbox/Today/timer, subscribe-before-snapshot, stale-reply rejection/light revision checks; M11 minimal tray uses the same services.
4. M05/M06 recovery confirmation, manual edits, range clipping and separate confirmed/live/pending figures.
5. M07/M01 JSON detail, Markdown weekly summary, manual backup/full restore; rehearse restore failure before trusting real data.
6. Pass V0.1 criteria, then use personally for two weeks, recording missed timers/forgotten stops/edit frequency/capture-switch effort/review usefulness.

Each step delivers commands/persistence/UI/tests/demo. Specify only the next slice. Estimate dates after probes and the real loop, not from module counts.

## 3. Later scope and dependencies

- V0.1b: HUD properties/DPI/displays/modes/hotkey conflicts; Mini scope awaits review. A failed HUD probe may defer delivery, never count degraded behaviour as a pass.
- V0.2: expand M06/M07, concurrency/interruption/time_block/Goal/Milestone/dependencies/WBS. M08 covers estimate errors and knowledge facts only. M13 searches existing objects and tests Chinese/model identifiers before choosing LIKE/FTS5.
- V0.3: M09 selected-input/preview/confirmation before M10 history/feedback. Use existing entities; advanced analyze_context/review_week interfaces follow later capabilities, not all eight at once.
- V0.4: brief ContextFact/Decision, references and reviewed Agent import under 07; M13 searches records/reference titles, never source-file bodies.
- V0.5: usage facts/sample counts before optional experimental ability scores; KPA/review use confirmed facts, acceptance rate does not establish saved time.
- V1.0: automate/harden existing protection and conduct fault/long-run tests; data protection is not first implemented here.

## 4. Risks and gates

Core chain: M02→M01→M04/M05→minimal M06→minimal M07. M12 shell can mock first. Search is independent of statistics; HUD is off the core chain; minimal M09 precedes M10.

| Risk | Verify before delivery |
| --- | --- |
| DB blocking/contention | Timer/query/backup concurrency, busy and disk-full paths |
| Wrong accounting | Pauses across midnight, clock changes, immediate restart, provisional/recovery confirmation |
| Stale windows | Snapshot races, lost final event, late old replies, hidden reopen |
| Data loss | Consistent backup, migration failures, future-schema write refusal, failed restore preserves original |
| Product overhead | Two weeks of real use before adding HUD/splits/complex categorization |

## 5. Slice specification

Each spec lists scope/F-IDs, commands/DTOs, transactions/invariants, errors/retries, tests, manual acceptance and deviations. Track implementation separately as not-started/in-progress/accepted. Meaningful invariant tests replace mechanical one-test-per-function rules.

Implementation checks for this revision: M01 validates epoch switching and ContextFact replacement; M04/M05 validate commit/baseline serialization and stale ticks; M06 retains trusted closed effort within recovering. R-01–R-08 are approved; this protocol/recovery revision awaits review.

Current scope (2026-10-03): personal work records and task management, including capability gaps and KPA evidence. External Agents and companion skills handle file reading, OCR, extraction and full-text search. See [scope and import contract](07-scope-and-agent-import.en.md).


Capability loop: evidence → user-confirmed gap → learning/practice task → work samples/self-assessment → review; optional proficiency retained. KPA collects traceable outcomes/dates, never grades people.
