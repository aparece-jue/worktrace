# Worktrace — Architecture Decision Records

Status: revised review draft; date: 2026-10-03. Corrects categorical reasoning, retains IDs and adds 011–013. Retained means continuity, not empirical verification. Product choices R-01…R-08 were approved on 2026-10-02; see [review notes](06-review-notes.en.md) §3.
[Architecture](00-architecture.en.md) · [Other language](03-adr.zh.md)

| ADR | Decision | Status |
| --- | --- | --- |
| ADR-001 | Ant Design only | Recommended, not applied |
| ADR-002 | Layered monolith | Retained |
| ADR-003 | rusqlite + bundled | Retained; threading probe open |
| ADR-004 | Production dockview use | **Decided** (R-04) |
| ADR-005 | Windows-first/platform boundary | Retained; HUD probes open |
| ADR-006 | Rust business authority | Retained |
| ADR-007 | Aggregate effective work intervals | Revised |
| ADR-008 | Units/timezones | Revised |
| ADR-009 | Lightweight HUD/Mini entries | Proposed; probe open |
| ADR-010 | Transactional revision/convergence | Revised |
| ADR-011 | Effective intervals/recovery | Revised proposal |
| ADR-012 | Milestones/product trade-offs | **Decided** |
| ADR-013 | Suggestion provenance, input confirmation and external Agents | Revised proposal |

## ADR-001 Ant Design only

Custom components use antd. Removing unused MUI/emotion reduces dependency maintenance, not necessarily bundle size: unused packages need not ship. Change dependencies and validate only after review.

## ADR-002 Layered monolith

No multi-crate split or workflow engine. Separate pure event types/runtime dispatch; perform required business writes atomically before notifications. Test real boundaries/failures.

## ADR-003 rusqlite + bundled

Access only through Rust services. Isolate synchronous IO in a blocking boundary; probe DB worker versus spawn_blocking. Define WAL/FKs/busy_timeout/backup races. Alternatives remain valid; choose lower setup cost here.

## ADR-004 Production dockview use

**Status**: decided (2026-10-02). Fixed layout first; keep the dockview dependency, but `DockviewDemo` does not ship in V0.1. Wire up dockview with layout persistence only once side-by-side panels are a confirmed need.

Preserve components/demo. Validate Today with fixed layout first; real side-by-side demand justifies persistence/panel contracts. Documentation advice does not authorize deleting copied files.

## ADR-005 Windows-first/platform boundary

Keep native implementations in platform; no premature other OS work. Single implementation does not forbid test clocks/boundary interfaces. HUD probes do not block recording.

## ADR-006 Rust business authority

React has view caches/UI state; cache libraries may be justified. Windows need convergence, not perpetual simultaneity. One core does not mean one OS process.

## ADR-007 Aggregate effective work intervals

No task.actual_duration; work_interval is the factual source. Any cache is rebuildable and invalidated on historical corrections, not only finish.

## ADR-008 Units/timezones

Unix-ms timestamps, ms durations, monotonic live measurement. Reports retain query timezone; Today uses current user timezone; history rebuckets by selected timezone; blocks retain creation timezone. UTC alone does not solve timing.

## ADR-009 Lightweight HUD/Mini entries

Separate entries proposed, lazy-loaded single entry valid. Compare measured startup/memory/package paths using current Vite APIs; do not assume every route loads the whole app.

## ADR-010 Transactional revision/convergence

Commit business writes/revision together and coalesce one invalidation per transaction. Subscribe before snapshot, reject stale replies and lightly verify visible-window versions. Separate run/tick sequence, no durable revision every second. Lost broadcast never rolls back committed work.

## ADR-011 Effective intervals/recovery

V0.1 work_interval is independent of V1 activity segments. Partial unique index protects running foreground occupancy. Recover uncertain intervals without a heartbeat-age threshold; trusted paused sessions stay paused and confirmed closed effort remains counted; corrections are versioned/audited.

## ADR-012 — Versions and product trade-offs

**Status**: decided (approved 2026-10-02)

Core recording, correction, recovery, minimal weekly review, and backup ship first; the HUD moves to V0.1b; M09's minimal policy lands in V0.3 and full context in V0.4.

These eight product trade-offs are approved (see [review notes](06-review-notes.en.md) §3):

| ID | Decision |
| --- | --- |
| R-01 | First release is the core loop only; HUD and the global capture hotkey move to V0.1b |
| R-02 | Foreground auto-pauses on lock/sleep; the user resumes explicitly afterwards |
| R-03 | Historical statistics recompute from **current** tags, projects, and weights; reports state the basis; exports freeze the result |
| R-04 | Fixed layout first; keep the dockview dependency, but `DockviewDemo` **must not ship in V0.1** |
| R-05 | AI off by default; preview selected records and confirm destination; no linked-file reading |
| R-06 | Ship knowledge usage and sample facts first; capability scoring is an **experiment that can be switched off** |
| R-07 | The Mini window is scheduled later, driven by real usage |
| R-08 | Per-kind tag weights sum to ≤1; the remainder stays unallocated and is **never auto-normalised** |

On R-04: keeping the demo component in `src/components/` as a reference is fine, but it must be excluded from the packaged artifact.


## ADR-013 Suggestion provenance, confirmation and external Agents

Retain suggestion/feedback/input versions and adoption. Confirm input and destination per action. Remove file classification and extraction caches; external tools parse files, reviewed imports retain provenance and never auto-send. Keep capability-gap analysis and evidence-focused KPA. The former classification scheme and its diagram were deleted; see 07 for current boundaries.

## Verification references

SQLite supports partial unique indexes; NULL-containing ordinary uniqueness does not protect root tags. Monotonic-clock suspension behaviour needs explicit platform policy. Distinguish Tauri core/WebView processes. References do not claim these features are implemented.

- [SQLite partial indexes](https://www.sqlite.org/partialindex.html)
- [SQLite NULL handling](https://www.sqlite.org/nulls.html)
- [Rust Instant](https://doc.rust-lang.org/std/time/struct.Instant.html)
- [Tauri process model](https://v2.tauri.app/concept/process-model/)

Current scope (2026-10-03): personal work records and task management, including capability gaps and KPA evidence. External Agents and companion skills handle file reading, OCR, extraction and full-text search. See [scope and import contract](07-scope-and-agent-import.en.md).
