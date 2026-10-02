# Worktrace — Data Model

Status: revised draft for user review. Date: 2026-10-02.
Upstream: [architecture](00-architecture.en.md); [Chinese](02-data-model.zh.md).
This replaces pause-total storage. The schema is logical, not an executable migration.

## 1. Versions and entities

| Version | Objects |
| --- | --- |
| V0.1 | app_meta, application_run, project, task, work_session, work_interval, time_edit, task_change, daily_plan, tag, task_tag |
| V0.2 | goal, milestone, task_dependency, time_block, task_knowledge (actual-use metadata) |
| V0.3 | ai_suggestion, ai_feedback; task/project security_level |
| V0.4 | context_fact, decision, document, task_document |
| V0.5 | knowledge_stat, a rebuildable derived cache |

Add Goal/Milestone foreign-key columns in V0.2, together with their tables. V0.1 exposes no unsupported filters. IDs are UUID strings, with explicit NOT NULL primary keys in full DDL. Timestamps are Unix milliseconds; durations are milliseconds, converted to minutes only for display.

Project owns tasks; tasks form a tree whose leaves are Actions. Each task has sessions; each session has effective work_interval rows. These minimal timing intervals are needed in V0.1 for pauses and range clipping; they are not the later activity-classification SessionSegment feature.

## 2. Logical schema and database constraints

```sql
-- Logical schema: full executable DDL and migrations belong to M01.
app_meta(singleton INTEGER PRIMARY KEY, revision INTEGER NOT NULL)
application_run(id TEXT PRIMARY KEY, started_at INTEGER NOT NULL, clean_exit_at INTEGER)
goal(id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT, status TEXT NOT NULL,
     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)
project(id TEXT PRIMARY KEY, goal_id TEXT REFERENCES goal(id), name TEXT NOT NULL,
        description TEXT, status TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)
milestone(id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id), title TEXT NOT NULL,
          status TEXT NOT NULL, due_at INTEGER, done_at INTEGER)
task(id TEXT PRIMARY KEY, project_id TEXT REFERENCES project(id), milestone_id TEXT REFERENCES milestone(id),
     parent_task_id TEXT REFERENCES task(id), title TEXT NOT NULL, description TEXT,
     status TEXT NOT NULL, priority_json TEXT, estimated_json TEXT,
     planned_duration_ms INTEGER, deadline INTEGER, importance INTEGER, urgency INTEGER,
     energy_required INTEGER, difficulty INTEGER, completion_criteria TEXT, quality TEXT,
     baseline_estimate_json TEXT, row_version INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)
work_session(id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES task(id),
             run_id TEXT NOT NULL REFERENCES application_run(id), mode TEXT NOT NULL,
             state TEXT NOT NULL, timer_kind TEXT NOT NULL, target_duration_ms INTEGER,
             started_at INTEGER NOT NULL, ended_at INTEGER, last_heartbeat_at INTEGER,
             interruption_of TEXT REFERENCES work_session(id), quality TEXT,
             needs_review INTEGER NOT NULL DEFAULT 0, row_version INTEGER NOT NULL)
work_interval(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES work_session(id),
              started_at INTEGER NOT NULL, ended_at INTEGER, voided_at INTEGER)
time_edit(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES work_session(id),
          before_json TEXT NOT NULL, after_json TEXT NOT NULL, reason TEXT,
          created_at INTEGER NOT NULL)
task_change(id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES task(id),
            before_json TEXT NOT NULL, after_json TEXT NOT NULL, created_at INTEGER NOT NULL)
daily_plan(task_id TEXT NOT NULL REFERENCES task(id), local_date TEXT NOT NULL,
           timezone TEXT NOT NULL, PRIMARY KEY(task_id,local_date,timezone))
tag(id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
    parent_id TEXT REFERENCES tag(id), created_at INTEGER NOT NULL)
task_tag(task_id TEXT NOT NULL REFERENCES task(id), tag_id TEXT NOT NULL REFERENCES tag(id),
         weight REAL, PRIMARY KEY(task_id,tag_id))
```

```sql
CREATE UNIQUE INDEX uq_running_foreground ON work_session(mode)
  WHERE mode='FOREGROUND' AND state='running';
CREATE UNIQUE INDEX uq_open_interval ON work_interval(session_id)
  WHERE ended_at IS NULL AND voided_at IS NULL;
CREATE UNIQUE INDEX uq_tag_root ON tag(kind,name) WHERE parent_id IS NULL;
CREATE UNIQUE INDEX uq_tag_child ON tag(kind,parent_id,name) WHERE parent_id IS NOT NULL;
CREATE INDEX idx_interval_session ON work_interval(session_id);
CREATE INDEX idx_task_project ON task(project_id);
CREATE INDEX idx_task_status ON task(status);
CREATE INDEX idx_session_task ON work_session(task_id);
```


![ER (V0.1 core tables)](images/er-core-tables.svg)

M01 supplies executable DDL, CHECKs, NOT NULLs, deletion policies and migrations. Enable foreign_keys on every connection.

- Session state: running / paused / recovering / finished / discarded. A running session has exactly one open effective interval; paused/finished/discarded have none. Recovering may retain one uncertain endpoint and never advances normally.
- Timer kind: stopwatch / countdown, extended with pomodoro in V0.2. target_duration_ms is an input budget, NULL for stopwatch.
- Modes: FOREGROUND / BACKGROUND / PASSIVE / WAITING; only FOREGROUND is exposed in V0.1. Several foreground sessions may be paused, but only one may run. Resume uses the same occupancy check.
- An interval cannot end before its start; effective intervals within a session cannot overlap. Confirmed historical human intervals across sessions cannot overlap either; validate in a write transaction.
- task.quality is allowed in Review, Done or Cancelled (abandoned may accompany Cancelled). Reopen clears current quality and preserves the change history. Add a CHECK for allowed state/quality combinations.
- Trim tag names, reject empty strings and compare case-sensitively for now. Root and child uniqueness are separate. Only Knowledge has parents; parent and child kinds match, and cycles are rejected.
- Default FK policy is RESTRICT. Archive/soft-delete tasks with timing history; archive projects without removing history. Reject deleting used tags until associations are removed.
- Do not store task.actual_duration, elapsed, remaining or paused_total_ms. Effective intervals are the timing source; time_edit is audit history, not an alternative aggregate source.

## 3. Sessions and timers

| Intent | Atomic changes |
| --- | --- |
| start | Check executability/foreground occupancy; create running session and open interval |
| pause | Close open interval; set paused |
| resume | Check occupancy; create open interval; set running |
| finish | Close interval if running; set finished and ended_at |
| switch/interrupt (V0.2) | Pause old session; start new session with interruption_of; roll back both on failure |
| correct | Check row_version, ranges and overlaps; edit intervals; append time_edit; increase version |

![Session and interval lifecycle](images/session-interval-lifecycle.svg)

Pause/resume keeps the session; starting again after finish creates a new one. A paused session may finish directly. Completing/cancelling a task finishes its running/paused sessions. Unresolved recovering records return RECOVERY_REQUIRED rather than silently confirming history.

active_ms is the sum of closed effective intervals plus the running interval's current duration. Paused values freeze because there is no open interval. Countdown remaining_ms=max(0,target_duration_ms-active_ms); overtime is shown separately. Expiry alerts but does not complete the task. Pauses consume no countdown budget.

Use a monotonic clock for live durations and wall time for persisted attribution. Wall-clock changes, suspend and discontinuities cannot be hidden with now-started_at: stop the uncertain interval at the last trusted checkpoint and require reconciliation. Monotonic clock state is not reused across restarts.

Proposed default: pause foreground work on lock/suspend; resume only by explicit action. Machine/background suspension is reconciled separately. This remains a product-review item in [review notes](06-review-notes.en.md). Accuracy tests must name the chosen sleep policy and include clock changes in both directions.

## 4. Recovery and exit

Acquire single-instance ownership before initializing a run. Each startup creates application_run and moves all unfinished sessions from previous runs, including paused sessions, to recovering with needs_review=1. No two-minute heartbeat threshold: immediate restart after a kill is covered. Persist a heartbeat about every 30 seconds; it supplies a last trusted time, not a fabricated endpoint.

Recovering sessions neither run nor occupy the running foreground slot. Exclude the whole unconfirmed session from confirmed totals; show pending time separately. The user can confirm an endpoint, edit intervals, save as paused then explicitly resume, or discard. Close uncertain intervals, validate overlaps and audit the decision atomically.

Closing a window does not terminate the core. Explicit quit atomically finishes running/paused sessions and saves revision/clean_exit_at. Recovering records remain unresolved. Startup scanning and transactions handle crashes before/after commit; delivered shutdown events are not a reliability prerequisite.

## 5. Task states and hierarchy

| From | Allowed targets |
| --- | --- |
| Inbox | Clarifying, Ready, Cancelled |
| Clarifying | Inbox, Ready, Cancelled |
| Ready | Doing, Scheduled (V0.2), Blocked, Waiting, Review, Done, Cancelled |
| Scheduled (V0.2) | Ready, Doing, Blocked, Waiting, Review, Done, Cancelled |
| Doing | Ready, Blocked, Waiting, Review, Done, Cancelled |
| Blocked / Waiting | Ready, Cancelled |
| Review | Ready (failed check), Done, Cancelled |
| Done / Cancelled | Ready only through explicit reopen, preserving history |

![Task state machine (main chain)](images/task-states-main.svg)

![Task state machine (branches and terminal states)](images/task-states-branch.svg)

V0.1 has no Scheduled UI. Today's plan is a simple daily selection; scheduling is a V0.2 time_block. start from Inbox may clarify to Ready in the same command. Doing describes work in progress, not a running timer; pause need not change it. Blocked/Waiting pauses running sessions; completion/cancellation finishes them. A background session finishing never automatically completes its task.

No hierarchy UI in V0.1. V0.2 times leaves by default; parents aggregate descendant intervals without duplicate counting. Children finishing do not automatically finish the parent. Parent/child and milestone assignments must share the project, reject cycles, and validate subtree moves transactionally.

task_dependency stores one predecessor→successor direction; blocks/depends_on are views of it. Reject self-dependencies and dependency cycles; related/parallel do not block. time_block stores task_id/start_at/end_at/timezone independently and supports repeated scheduling.

## 6. Statistics and tags

Use half-open [from,to) ranges. Clip every effective interval: max(0,min(end,to)-max(start,from)). Use one snapshot now for open running intervals. Distinguish confirmed closed time from provisional live time. Exclude recovering/discarded sessions. DTOs carry measure, timezone, range, as_of and revision.

Human effort includes FOREGROUND only. Sum BACKGROUND/PASSIVE as separate machine measures; WAITING is separate. Never add concurrent machine durations to human totals.

Associated duration always counts each associated tag in full, regardless of weight; sums across tags may exceed total effort and must be labelled non-additive. Weighted effort allocates within each kind: finite weight in 0..1, NULL means unallocated. Totals below 1 leave an Unallocated remainder; totals above 1 are rejected. No silent normalization. Knowledge ancestor reports deduplicate intervals rather than summing parent/child associations.

Proposed default: recompute history using current tags/project assignments/weights and label reports accordingly. Exports retain the generated result and its accounting policy. Historical classification snapshots remain a user-review option. Knowledge weights live only in task_tag; task_knowledge stores required_level/used/learning_gain rather than duplicating weights.

## 7. AI and context migrations

Use stable priority_json/estimated_json envelopes from V0.1: value, source(user/rule/ai), confirmed_at, updated_at; estimates use milliseconds. Add suggestion_id in V0.3. Source is origin, confirmation is authority: accepting AI does not relabel origin as user. Background jobs cannot overwrite confirmed values; explicit editing/re-adoption can.

ai_suggestion retains task/kind/input version, provider/model/prompt version, suggested value, original estimate, timestamps and pending/accepted/edited/rejected/stale outcomes. ai_feedback links suggestion_id. Keep rejected suggestions and post-adoption edits for valid acceptance/error statistics. Stale inputs invalidate suggestions. Self-reported model confidence differs from empirically calibrated confidence; insufficient evidence is Unknown.

V0.3 task/project security_level defaults to STRICT_LOCAL; M09/M10 whitelist outbound payloads. In V0.4, context_fact allows only one current value per project/key through a partial unique index WHERE superseded_by IS NULL; replacement inserts and links atomically. Documents retain content hash/modified time, and extraction caches inherit classification. task_document has real FKs. Reclassification never strips origin security metadata.

knowledge_stat is rebuildable and carries algorithm_version/sample_count/computed_at. V0.5 ability scores are experimental, never inferred from total hours alone.

## 8. Required M01/M05 cases

Pause/restart, finish while paused, conflicting resume, concurrent starts, pauses across midnight, empty ranges, duplicate root tags, historical overlap, kill/restart within ten seconds, quit while running/paused, clock changes both ways, stale edits, report recomputation after corrections, pending exclusion and explicit confirmation.

## 9. History, estimate baseline and database restore

V0.1 task_change records state/quality/assignment edits in the same transaction, supporting reopen history and completion by date. daily_plan records selected local date/timezone; updated_at does not mean planned today. First start freezes the estimate envelope to baseline_estimate_json; later estimates do not overwrite it. Explicit rebasing is audited; estimates after work began are labelled non-pre-start. Default error compares confirmed human duration; incomplete tasks do not enter completed samples.

M01 full DDL maps every invariant to CHECK/index or transactional service validation. Interval edits target finished/recovering sessions only; finish/confirm running or paused records first. Failed edits never leave partial audit records.

Restore: stop timers/pause writes/close connections, consistently back up current DB, validate candidate in a temporary path (integrity/FKs/schema; reject future versions, back up and migrate old versions), switch recoverably in the same directory, reopen/validate. Roll back paths and reopen the original on failure. Never overwrite an open WAL database. Backups contain all required data; export/schema/application versions are distinct.

## 10. Service contract additions

- Manual priority/estimate inputs use source=user and confirmed_at=operation time. Adopted AI retains origin with equal overwrite protection. Envelopes contain known fields, not arbitrary EAV attributes.
- Explicit resume after recovery updates session.run_id to the current run and refreshes heartbeat; time_edit preserves original recovery attribution. Otherwise later startup may misclassify a resumed session as previous-run work.
- Project states active/archived/done; archived projects cannot start new sessions but history remains editable. Goal active/done/dropped; Milestone open/done/cancelled. Full DDL defines importance/urgency/energy/difficulty ranges; V0.1 need not expose energy/difficulty inputs.
- Task completion writes task_change; reports select completions by its timestamp, not updated_at/session end. Reopened work is labelled reopened, not falsely presented as still completed.
- Pausable countdown budgets are execution timing; fixed time_block calendar endpoints do not move on pause. Never conflate them.
