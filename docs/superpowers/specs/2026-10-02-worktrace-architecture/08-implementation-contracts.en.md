# Worktrace implementation contract supplements

Status: 2026-10-03 proposal awaiting review; unimplemented. [中文](08-implementation-contracts.zh.md). Refines 02/04/07 without replacing platform probes.
[Architecture](00-architecture.en.md) · [Module breakdown](01-module-breakdown.en.md) · [Data model](02-data-model.en.md) · [ADR](03-adr.en.md) · [Functional spec](04-functional-spec.en.md) · [Roadmap](05-roadmap.en.md) · [Review notes](06-review-notes.en.md) · [Scope and import contract](07-scope-and-agent-import.en.md) · [Glossary](99-glossary.en.md) · [Other language](08-implementation-contracts.zh.md)

## 1. Interval duration and attribution (V0.1)

Separate sampled wall_at, run-local monotonic elapsed and accounting started_at/ended_at. Effective intervals remain the single statistical fact. Start samples W0/M0; close computes monotonic duration_ms and sets accounting ended_at=started_at+duration_ms. Trusted closed rows enforce nonnegative integer duration_ms=ended_at-started_at; the duration is a validation value, never independently editable. Preserve sampled closing wall time separately. Live snapshots use the coordinator’s elapsed/attribution endpoint, never independent Date.now(). Manual correction recomputes duration from confirmed endpoints with audit.

About every 30 seconds persist interval_id/run_id/trusted wall_at/attribution_at/elapsed_ms atomically; never serialize Instant. On every sample check both adjacent wall/monotonic delta differences and cumulative wall divergence from the trusted attribution baseline. Either check exceeding 2000ms, backward movement, suspend/clock events or invalid samples trigger reconciliation (same threshold as §7). The initial threshold is versioned after probes, not an accuracy promise.

Close the trusted prefix at the last checkpoint; mark only the uncertain remainder needs_review, retaining raw wall/candidate elapsed. Without a checkpoint the current interval is entirely uncertain. Split/audit atomically and never double-count the prefix. A trustworthy lock/suspend boundary pauses normally; late/untrusted notifications enter recovering. No continuing foreground work during recovery. A new run or explicit reconciliation establishes a new trusted attribution anchor; overlap with trusted history requires explicit attribution confirmation. Within a continuous run start each segment from the anchor rules below, never re-anchor each segment to sampled wall time or shift historical facts. Confirmed ranges must be legal/nonoverlapping; candidate duration is optional evidence. Clip at real timezone day boundaries, not fixed 24-hour days. Checkpoint writes do not increment revision; business interval/state changes do. Failed persistence cannot be treated as a recoverable checkpoint.

![Interval duration and attribution](images/interval-time-attribution.svg)

## 2. Pomodoro phases (V0.2)

Persist phase(work/break), phase_state(running/frozen) and cycle_index on the session; work_budget_ms/break_budget_ms live per cycle on pomodoro_cycle, not on the session; progress comes from phase_checkpoint (break) or is derived from that cycle's intervals (work). See §8 for the field matrix and state version. Work uses normal intervals; break keeps session paused with no open work_interval and an independent phase clock. Break neither occupies foreground nor counts effort. Work expiry alerts and continues overtime until explicit break; break expiry alerts until explicit next work cycle, rechecking occupancy. A conflict keeps the current phase state (§7 table: original state unchanged). Phase pause freezes elapsed; resuming break is not work resume. Lock/suspend freezes the phase, explicit continuation after wake; restart recovers work via intervals and break frozen at checkpoint, never backfills downtime. Task completion ends all phases. Per-cycle budget is separate from cumulative session effort.

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

## 7. Continuous attribution anchor and phase commands (revision)

Keep in-memory anchor_wall_at/anchor_monotonic for a continuous trusted run segment. Attribution endpoint A(M)=anchor_wall_at+(M-anchor_monotonic), including normal gaps/pauses between intervals; effort only counts open work intervals. Starts/resumes use the same sampled A, not fresh wall time; ends use start+interval monotonic work duration. Sub-threshold wall adjustments never reset the anchor/rewrite history, so a 500ms skew cannot overlap consecutive intervals.

Each sample checks abs(sampled_wall_at-A) and adjacent delta differences; either over 2000ms or a trusted system event triggers pause/recovery evaluation. Checkpoints never reset the anchor, preventing undetected cumulative drift. Sampling/detection/prefix confirmation/checkpoint writing is serialized; uncertain samples cannot be persisted as trusted first. New runs, wake and explicit reconciliation re-anchor without reusing Instant and validate attribution against history. Pending intervals are candidate ranges rather than confirmed overlap facts; later confirmation must validate against new work.

Persist independent phase_checkpoint(session_id/run_id/phase/cycle_index/session_version/phase_elapsed_ms/sampled_at), never on an old work interval. Writes match current phase/version; progress checkpoints do not increment revision, phase/state transitions increment session_version and business revision. Finished/discarded sessions clear active phase/state to null, retaining history in audit.

This table references 02 §3 registry commands; identical names are one endpoint, with Pomodoro-only extensions here.

| Command | Preconditions | Result |
| --- | --- | --- |
| pause | work/running, session running | Close interval; work/frozen, session paused |
| resume | work/frozen, session paused | Check occupancy, open interval, work/running |
| start_break | work/running or work/frozen, no pending intervals | Close interval if running; no interval if paused; break/running, elapsed=0, session paused |
| pause_break | break/running | break/frozen, no work interval |
| continue_break | break/frozen | break/running, phase clock only |
| start_next_cycle | break/running or break/frozen | Check occupancy; success increments cycle, work/running, open interval, elapsed=0; failure unchanged |
| finish (same as 02 §3) | Session running/paused, no pending records | Shared finish plus active-phase cleanup; recovering yields RECOVERY_REQUIRED |
| reconcile (same as 02 §3, exists in V0.1) | Session recovering | Shared recovery confirmation/uncertain discard; retained pause additionally work/frozen, no automatic phase advance |

Public pause/resume serve ordinary timers and Pomodoro work; resume on break returns POMO_STATE_CONFLICT; use continue_break/start_next_cycle. Cancellation clears phases like finish but recovering still yields RECOVERY_REQUIRED. Restarted break is frozen, work follows interval recovery, never backfill phase time. All commands check expected_data_epoch/row_version; invalid states never write. Expiry only alerts. Diagram shows common paths; this table is normative.

V0.2 Pomodoro ticks/queries/results include phase/phase_state/cycle_index/phase_elapsed_ms/phase_remaining_ms/phase_overtime_ms. Remaining=max(0,phase budget-elapsed); human active_ms freezes during break. Ordinary timers have null phase fields; remaining_ms/overtime_ms are countdown-only, while Pomodoro uses phase fields for work and break budgets. All fields come from one coordinator snapshot and old session versions remain rejected.

## 8. Unified commands, cycle progress and timer fields (confirmed revision)

Choose (b): public pause/resume only, no second public work-command set. 02 §3 defines the base operation and 08 §7 extends Pomodoro preconditions/phase metadata. Ordinary timers retain their rules; Pomodoro work/running pause closes the interval and sets work/frozen, work/frozen resume checks occupancy and opens an interval/work/running. Both work commands reject break with POMO_STATE_CONFLICT and no writes; use pause_break/continue_break/start_next_cycle. Frontend/tray/platform events share the coordinator; UI dispatches by phase without command aliases.

V0.2 adds pomodoro_cycle(session_id/cycle_index/started_at/work_budget_ms/break_budget_ms), composite key session_id/cycle_index and per-cycle budget values. Pomodoro work_interval adds cycle_index referencing a valid same-session cycle; ordinary intervals use null. Initial start creates cycle 1; next-cycle transaction creates the cycle, updates current index and opens the interval atomically. Intervals bind cycles when opened; corrections/uncertain splits preserve that association, never reassociate old work to the current cycle.

Work phase_elapsed_ms sums current-cycle trusted effective durations plus trusted live interval elapsed, never an independent effort source. Break phase elapsed comes from phase_checkpoint/live baseline. Work pause/resume preserve cumulative phase progress: new interval checkpoint elapsed_ms=0 is not phase_elapsed_ms=0. start_break resets break progress; start_next_cycle resets new-cycle work progress. continue_break preserves spent break time.

Restart first reconciles intervals then aggregates current-cycle confirmed work. Ten minutes of work paused before kill still yields ten minutes spent after restart. Pending interval effort is separate, never advances budgets by guesses; confirm/discard recomputes its original cycle and retains work/frozen. Break restarts frozen at the last persisted checkpoint without downtime. Corrections recalculate progress and increment session_version to invalidate old ticks.

| timer_kind | Ordinary budget fields | Phase fields/remaining |
| --- | --- | --- |
| stopwatch | target_duration_ms/remaining_ms/overtime_ms all null | All phase fields null, active_ms counts upward |
| countdown | Required target; remaining=max(0,target-active), overtime=max(0,active-target) | Phase fields null; pause freezes effort |
| pomodoro | target_duration_ms/remaining_ms/overtime_ms all null | Current-cycle work/break budgets; phase_remaining=max(0,budget-elapsed), phase_overtime=max(0,elapsed-budget) |

Terminal sessions retain confirmed active_ms; Pomodoro phase/state/elapsed/remaining/overtime fields are null, historical cycles come from detail queries. Never infer phase freezing from session paused alone.

Acceptance: 25-minute work with ten spent remains fifteen after pause/resume, next cycle starts at twenty-five while total effort retains the prior cycle. Five-minute break with two spent remains three after freeze/continue. Restart preserves paused work progress; reconciliation uses original cycles; prior-cycle work never consumes the next budget; null fields match this table.

Command relationships: finish is shared across timer types, 08 only adds phase cleanup. reconcile exists in V0.1, not introduced in V0.2. correct edits finished history only; reconcile can internally reuse correction/finish primitives within its transaction, never alias those public commands. 02 §3 is the command/version/recovery-parameter registry.
