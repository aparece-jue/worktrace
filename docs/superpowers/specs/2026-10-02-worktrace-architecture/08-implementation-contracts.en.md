# Worktrace implementation contract supplements

Status: 2026-10-03 proposal awaiting review; unimplemented. [中文](08-implementation-contracts.zh.md). Refines 02/04/07 without replacing platform probes.
[Architecture](00-architecture.en.md) · [Module breakdown](01-module-breakdown.en.md) · [Data model](02-data-model.en.md) · [ADR](03-adr.en.md) · [Functional spec](04-functional-spec.en.md) · [Roadmap](05-roadmap.en.md) · [Review notes](06-review-notes.en.md) · [Scope and import contract](07-scope-and-agent-import.en.md) · [Glossary](99-glossary.en.md) · [Other language](08-implementation-contracts.zh.md)

## 1. Interval duration and attribution (V0.1)

Separate sampled wall_at, run-local monotonic elapsed and accounting started_at/ended_at. Effective intervals remain the single statistical fact. Start samples W0/M0; close computes monotonic duration_ms and sets accounting ended_at=started_at+duration_ms. Trusted closed rows enforce nonnegative integer duration_ms=ended_at-started_at; the duration is a validation value, never independently editable. Preserve sampled closing wall time separately. Live snapshots use the coordinator’s elapsed/attribution endpoint, never independent Date.now(). Manual correction recomputes duration from confirmed endpoints with audit.

About every 30 seconds persist interval_id/run_id/trusted wall_at/attribution_at/elapsed_ms atomically; never serialize Instant. Compare wall and monotonic deltas on sampling. Absolute divergence over 2000ms, backward movement, suspend/clock events or invalid samples trigger reconciliation. The initial threshold is versioned after probes, not an accuracy promise.

Close the trusted prefix at the last checkpoint; mark only the uncertain remainder needs_review, retaining raw wall/candidate elapsed. Without a checkpoint the current interval is entirely uncertain. Split/audit atomically and never double-count the prefix. A trustworthy lock/suspend boundary pauses normally; late/untrusted notifications enter recovering. No continuing foreground work during recovery. A backward clock creating overlap with trusted history blocks new work until explicit attribution correction; never shift existing history silently. Confirmed ranges must be legal/nonoverlapping; candidate duration is optional evidence. Clip at real timezone day boundaries, not fixed 24-hour days. Checkpoint writes do not increment revision; business interval/state changes do. Failed persistence cannot be treated as a recoverable checkpoint.

![Interval duration and attribution](images/interval-time-attribution.svg)

## 2. Pomodoro phases (V0.2)

Persist phase(work/break), cycle_index, work_budget_ms, break_budget_ms, phase_elapsed_ms checkpoint and state version. Work uses normal intervals; break keeps session paused with no open work_interval and an independent phase clock. Break neither occupies foreground nor counts effort. Work expiry alerts and continues overtime until explicit break; break expiry alerts until explicit next work cycle, rechecking occupancy. A conflict stays break/paused. Phase pause freezes elapsed; resuming break is not work resume. Lock/suspend freezes the phase, explicit continuation after wake; restart recovers work via intervals and break frozen at checkpoint, never backfills downtime. Task completion ends all phases. Per-cycle budget is separate from cumulative session effort.

![Pomodoro phases](images/pomodoro-phases.svg)

## 3. Outcomes and version dependencies

V0.4 adds minimal outcome(id/project_id/nullable task_id/title/body/nullable occurred_at/source/confirmed_at/row_version) and outcome_source so Agent outcome import has a target. Notes/decisions/facts/references have row_version, provenance and confirmation history. occurred_at is the declared/confirmed outcome date, never task completion; unknown=null. Adopted subtasks never implicitly change parent state. V0.5 extends categories/self-assessment/practice/action reviews using those entities. Import mappings validate target type/ID/existence, not unconstrained strings.

## 4. AI summary and schedule

V0.3 adds F-209 AI summary and F-210 schedule suggestions. Summary inputs include period/timezone/measure/epoch/revision and selected record IDs/versions. Code computes confirmed/live/pending figures. Output separates cited facts, user notes and inference; references must be whitelisted. No invented sources/time saved/ability claims. Invalid structure/citations cannot become confirmed reports.

Scheduling uses selected tasks, existing dependencies, confirmed estimates/deadlines and user-provided available windows. Missing inputs prompt, never assume all-day availability. AI proposes time_blocks; domain services revalidate ranges/state/dependencies/conflicts at adoption and commit selected items atomically. Stale versions conflict/recompute; never silently reschedule existing blocks or start timing, never guarantee on-time completion.

## 5. KPA reproducible snapshots (V0.5)

Drafts may change, confirmed report_snapshot is immutable. Persist period/timezone/filter/classification/measure, generation epoch/revision, referenced IDs/versions, necessary copies of source facts/outcomes/provenance, template/model/prompt versions and final text. IDs alone cannot reproduce corrected history. Corrections create revisions; regenerated reports create linked new snapshots. Old values stay readable with changed-source indication. References are historical locators, not promises of file availability/integrity; external hashes are declared evidence. Back up snapshots/provenance. Label unconfirmed AI output/ambiguous attribution/pending effort separately.

## 6. Implementation gates

Test clock changes both ways, late lock events, restart/checkpoint failure, timezone day clipping, break accounting/conflicts, V0.4 outcome provenance/import, stale AI input, invalid schedule/conflicts and old KPA reproduction after corrections. Implement V0.1 first; example checks never replace device validation.

Boundary details: start/resume atomically writes an initial elapsed_ms=0 checkpoint; phase changes increment session_version. Raw wall samples are user data, excluded from diagnostic bodies. Reports use the first confirmed interval per work episode as start and task_change completion event as finish; reopened episodes are distinct, never use updated_at as completion.
