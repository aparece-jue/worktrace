# Worktrace — Data Model

Status: revised draft for user review. Date: 2026-10-03.
Upstream: [architecture](00-architecture.en.md); [Chinese](02-data-model.zh.md).
This replaces pause-total storage. The schema is logical, not an executable migration.

## 1. Versions and entities

| Version | Objects |
| --- | --- |
| V0.1 | app_meta, application_run, interval_checkpoint, project, task, work_session, work_interval, time_edit, task_change, daily_plan, tag, task_tag |
| V0.2 | goal, milestone, task_dependency, time_block, task_knowledge (actual-use metadata), phase_checkpoint (break-phase checkpoint), pomodoro_cycle (cycle budgets/interval ownership) |
| V0.3 | ai_suggestion, ai_feedback; per-action input/destination confirmation |
| V0.4 | context_fact, decision, document (references only), task_document, outcome, outcome_source, agent_import_batch/item |
| V0.5 | knowledge_stat (rebuildable cache), report_snapshot (confirmed reports) |

![ER (auxiliary and migration tables)](images/er-more-tables.svg)

Add Goal/Milestone foreign-key columns in V0.2, together with their tables. V0.1 exposes no unsupported filters. IDs are UUID strings, with explicit NOT NULL primary keys in full DDL. Timestamps are Unix milliseconds; durations are milliseconds, converted to minutes only for display.

Project owns tasks; tasks form a tree whose leaves are Actions. Each task has sessions; each session has effective work_interval rows. These minimal timing intervals are needed in V0.1 for pauses and range clipping; they are not the later activity-classification SessionSegment feature.

## 2. Logical schema and database constraints

```sql
-- Logical schema: full executable DDL and migrations belong to M01.
app_meta(singleton INTEGER PRIMARY KEY, data_epoch TEXT NOT NULL, revision INTEGER NOT NULL)
application_run(id TEXT PRIMARY KEY, started_at INTEGER NOT NULL, clean_exit_at INTEGER)
goal(id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT, status TEXT NOT NULL,
     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)
project(id TEXT PRIMARY KEY, goal_id TEXT REFERENCES goal(id), name TEXT NOT NULL,
        description TEXT, status TEXT NOT NULL, row_version INTEGER NOT NULL,
        created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)
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
              started_at INTEGER NOT NULL, ended_at INTEGER, voided_at INTEGER,
              duration_ms INTEGER, sampled_end_wall_at INTEGER,
              needs_review INTEGER NOT NULL DEFAULT 0)
interval_checkpoint(interval_id TEXT PRIMARY KEY REFERENCES work_interval(id),
                    run_id TEXT NOT NULL REFERENCES application_run(id),
                    wall_at INTEGER NOT NULL, attribution_at INTEGER NOT NULL, elapsed_ms INTEGER NOT NULL)

time_edit(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES work_session(id),
          before_json TEXT NOT NULL, after_json TEXT NOT NULL, reason TEXT,
          created_at INTEGER NOT NULL)
task_change(id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES task(id),
            before_json TEXT NOT NULL, after_json TEXT NOT NULL, created_at INTEGER NOT NULL)
daily_plan(task_id TEXT NOT NULL REFERENCES task(id), local_date TEXT NOT NULL,
           timezone TEXT NOT NULL, PRIMARY KEY(task_id,local_date,timezone))
tag(id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
    parent_id TEXT REFERENCES tag(id), row_version INTEGER NOT NULL, created_at INTEGER NOT NULL)
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

02 §3 is the sole public session-command registry; 08 §7 adds Pomodoro preconditions to the same commands, never duplicate endpoints. These business-intent names use the serial coordinator/DB transaction; edits include expected_data_epoch/row_version and never partially commit.

| Command | Preconditions and atomic changes |
| --- | --- |
| start | Executable task/occupancy checks; create running session/open interval/initial checkpoint; Pomodoro also creates cycle 1, work/running |
| pause | Running without pending intervals; close interval, paused; Pomodoro work/running only, additionally work/frozen |
| resume | Paused without pending intervals; check occupancy/open interval/running; Pomodoro work/frozen only, additionally work/running while retaining cycle progress |
| finish | Running/paused without pending records; close interval if running, finished/ended_at; clear active Pomodoro phase, including break/running or break/frozen. Recovering yields RECOVERY_REQUIRED |
| switch (V0.2) | Validate old work/target; atomically pause old and start new; reason=user_switch/interrupt, interrupt reason retains interruption_of. No separate public interrupt alias; not for Pomodoro break |
| correct | Finished only; check version/ranges/overlap, edit trusted history/audit/recompute cycles. Recovering uses reconcile; running/paused must finish first |
| reconcile | Recovering only; action=confirm/discard_uncertain, target_state=paused/finished; atomically resolve all pending intervals, validate ranges/overlap, audit, clear review flags/update run_id. Paused never auto-times; Pomodoro work/frozen. Never void the entire session |
| discard_session | Explicit user intent to void entire session; close open interval if any, void all/clear review flags, discarded/ended_at, clear phase and audit; never delete audit or implicitly change task state |
| backfill | Manual history: validate task/ranges/overlap, create finished session/trusted intervals/audit; never start timing or fabricate task completion |

start_break/pause_break/continue_break/start_next_cycle are V0.2-only phase commands under 08 §7. finish ends a session, not a task. Task completion/cancellation uses the same finish primitive in its task transaction, rejecting the whole action on recovering. reconcile may share internal finish logic but is one transaction, never client-side correct then finish.

![Session and interval lifecycle](images/session-interval-lifecycle.svg)

Pause/resume keeps the session; starting again after finish creates a new one. A paused session may finish directly. Completing/cancelling a task finishes its running/paused sessions. Unresolved recovering records return RECOVERY_REQUIRED rather than silently confirming history.

active_ms sums trusted closed interval.duration_ms plus coordinator monotonic live elapsed; duration equals accounting endpoint difference; anomalies and manual correction follow 08. Paused values freeze because there is no open interval. Only countdown uses remaining_ms=max(0,target_duration_ms-active_ms); overtime is shown separately. Expiry alerts but does not complete the task. Pauses consume no countdown budget.

Use a monotonic clock for live durations and wall time for persisted attribution. Wall-clock changes, suspend and discontinuities cannot be hidden with now-started_at: stop the uncertain interval at the last trusted checkpoint and require reconciliation. Monotonic clock state is not reused across restarts.

Confirmed R-02: pause foreground work on lock/suspend, resume explicitly. Specify machine/background suspend behaviour in their implementation spec; the foreground choice is approved in [review notes](06-review-notes.en.md). Accuracy tests must name the chosen sleep policy and include clock changes in both directions.

## 4. Recovery and exit (behaviour revision awaiting review)

After single-instance ownership, create application_run and scan unfinished previous-run sessions without heartbeat-age thresholds. Recover based on interval facts:

| Previous record | Startup action | Accounting |
| --- | --- | --- |
| paused with no open/pending intervals | Keep paused, update run_id, never auto-resume | Confirmed closed intervals still count |
| running with an open interval | Set recovering, flag only that interval needs_review=1 | Closed confirmed intervals count, open interval pending |
| recovering | Retain pending status, never add known effort | Same |
| Broken state/interval invariant | Isolate and diagnose; no invented repairs | Suspect intervals excluded and visible |

![The four recovery classifications](images/recovery-classification.svg)

Keep session.needs_review and interval flags consistent in transactions; never independently modify them. Running has no pending interval; recovering has a pending interval or explicit invariant-fault marker. About every 30 seconds persist a trusted checkpoint with interval_id/trusted wall time/live baseline. It is a candidate cutoff, never automatic effort backfill. M04/M05 specify checkpoint persistence; last_heartbeat_at alone is not a complete clock mapping.

Recovering does not occupy running foreground. Show confirmed effort and pending interval separately; unknown endpoints are unknown ranges, not precise fabricated duration. Use reconcile(action=confirm,target_state=finished/paused) to confirm/edit uncertain intervals atomically; retained pause requires explicit resume.

Discard uncertain interval only sets its voided_at and clears pending status, preserving earlier valid intervals; session becomes finished or paused. Voiding the whole session is a separate explicit intent, voids every interval and sets discarded. Audit/version/revision updates are atomic; do not share an ambiguous Discard button. Confirmation cannot overlap subsequently recorded human work.

Window closure does not exit. Explicit quit finishes running/paused and saves revision/clean_exit_at atomically; recovering remains pending. After resolution or preserved pause, update run_id to current run with original attribution audited. Repeated restarts cannot double-void or alter confirmed effort.

![Crash recovery flow](images/crash-recovery-flow.svg)

> This diagram expands only the uncertain-interval branch; the full four-way classification is in the previous figure.

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

Use half-open [from,to) ranges. Clip every effective interval: max(0,min(end,to)-max(start,from)). Use the same coordinator snapshot monotonic attribution endpoint for open running intervals; never sample wall time independently. Separate confirmed closed intervals (including trusted parts of recovering sessions) from provisional live time. Exclude pending/voided intervals and discarded sessions; pending time is separate. DTOs carry measure, timezone, range, as_of and revision.

Human effort includes FOREGROUND only. Sum BACKGROUND/PASSIVE as separate machine measures; WAITING is separate. Never add concurrent machine durations to human totals.

![Interval clipping and the human/machine split](images/interval-clip-split.svg)

Associated duration always counts each associated tag in full, regardless of weight; sums across tags may exceed total effort and must be labelled non-additive. Weighted effort allocates within each kind: finite weight in 0..1, NULL means unallocated. Totals below 1 leave an Unallocated remainder; totals above 1 are rejected. No silent normalization. Knowledge ancestor reports deduplicate intervals rather than summing parent/child associations.

Confirmed R-03: recompute history using current tags/project assignments/weights and label reports accordingly. Exports retain the generated result and its accounting policy. Historical classification snapshots are a future change, not an open R-03 decision. Knowledge weights live only in task_tag; task_knowledge stores required_level/used/learning_gain rather than duplicating weights.

## 7. AI and context migrations

Use stable priority_json/estimated_json envelopes from V0.1: value, source(user/rule/ai), confirmed_at, updated_at; estimates use milliseconds. Add suggestion_id in V0.3. Source is origin, confirmation is authority: accepting AI does not relabel origin as user. Background jobs cannot overwrite confirmed values; explicit editing/re-adoption can.

ai_suggestion retains task/kind/input version, provider/model/prompt version, suggested value, original estimate, timestamps and pending/accepted/edited/rejected/stale outcomes. ai_feedback links suggestion_id. Keep rejected suggestions and post-adoption edits for valid acceptance/error statistics. Stale inputs invalidate suggestions. Self-reported model confidence differs from empirically calibrated confidence; insufficient evidence is Unknown.

V0.3 persists selected input entity versions and destination; no default sends or four-level security_level. V0.4 context_fact holds user-confirmed brief facts, one current value per project/key with replacement history. document stores only supplied title/path/URL/source locator, never bodies/OCR/extraction caches; association neither verifies content nor authorizes reading. Agent batches/items retain schema version, payload hash, provenance, adoption results and mappings; see 07. Deduplication is not path-based. V0.1 captures outcomes/evidence in completion notes; V0.4 adds minimal outcome/outcome_source for imports; V0.5 extends self-assessments/practice and report_snapshot.

knowledge_stat is rebuildable and carries algorithm_version/sample_count/computed_at. V0.5 ability scores are experimental, never inferred from total hours alone.

## 8. Required M01/M05 cases

Pause/restart, finish while paused, conflicting resume, concurrent starts, pauses across midnight, empty ranges, duplicate root tags, historical overlap, kill/restart within ten seconds, quit while running/paused, clock changes both ways, stale edits, report recomputation after corrections, pending exclusion and explicit confirmation.

## 9. History, estimate baseline and database restore

V0.1 task_change records state/quality/assignment edits in the same transaction, supporting reopen history and completion by date. daily_plan records selected local date/timezone; updated_at does not mean planned today. First start freezes the estimate envelope to baseline_estimate_json; later estimates do not overwrite it. Explicit rebasing is audited; estimates after work began are labelled non-pre-start. Default error compares confirmed human duration; incomplete tasks do not enter completed samples.

M01 full DDL maps every invariant to CHECK/index or transactional service validation. Interval edits target finished sessions only; finish/confirm running or paused records first. Failed edits never leave partial audit records.

Restore: stop timers/pause writes/close connections, consistently back up current DB, validate candidate in a temporary path (integrity/FKs/schema; reject future versions, back up and migrate old versions), switch recoverably in the same directory, reopen/validate. Roll back paths and reopen the original on failure. Never overwrite an open WAL database. Backups contain all required data; export/schema/application versions are distinct.

## 10. Service contract additions

- Manual priority/estimate inputs use source=user and confirmed_at=operation time. Adopted AI retains origin with equal overwrite protection. Envelopes contain known fields, not arbitrary EAV attributes.
- Explicit resume after recovery updates session.run_id to the current run and refreshes heartbeat; time_edit preserves original recovery attribution. Otherwise later startup may misclassify a resumed session as previous-run work.
- Project states active/archived/done; archived projects cannot start new sessions but history remains editable. Goal active/done/dropped; Milestone open/done/cancelled. Full DDL defines importance/urgency/energy/difficulty ranges; V0.1 need not expose energy/difficulty inputs.
- Task completion writes task_change; reports select completions by its timestamp, not updated_at/session end. Reopened work is labelled reopened, not falsely presented as still completed.
- Pausable countdown budgets are execution timing; fixed time_block calendar endpoints do not move on pause. Never conflate them.

## 11. Restore epochs and fact replacement order (revision)

Enter maintenance on DB restore/replacement; cancel old queued work/queries/AI requests. Validate a temporary candidate, generate a new data_epoch, switch recoverably and reopen. Failure returns to the original DB/epoch and forces a handshake. Create a current run and scan using §4; never reuse backed-up runtime memory. Every old-epoch mutation returns DATA_EPOCH_MISMATCH. Restored revision may be lower and is comparable only inside its new epoch.

Keep the superseded_by model for ContextFact with this atomic order: insert new fact temporarily superseded_by=old ID (not current); set old.superseded_by=new ID; clear new.superseded_by to NULL. On commit the chain is acyclic and exactly one current row exists; failure rolls back everything. Intermediate states are not visible to other readers. Reject cross-project/key links, self-references and arbitrary historical pointer edits. Do not insert a second current row first or assume UNIQUE checks wait until commit.

![ContextFact three-step supersede](images/contextfact-supersede.svg)

Time queries require interval.needs_review=0 and voided_at IS NULL. Whole-session void never affects other sessions. Test epochs/snapshots, state-version/ticks, trusted paused recovery, partial discard and fact replacement with fault injection.

Current scope (2026-10-03): personal work records and task management, including capability gaps and KPA evidence. External Agents and companion skills handle file reading, OCR, extraction and full-text search. See [scope and import contract](07-scope-and-agent-import.en.md).

See [08: timing, phases, AI and report snapshots](08-implementation-contracts.en.md).

V0.1 adds interval_checkpoint(interval_id/run_id/wall_at/attribution_at/elapsed_ms); recovery uses only the last persisted checkpoint. Trusted closed duration_ms is nonnull and equals ended_at-started_at; uncertain rows may be null. Add CHECK/FKs. V0.2 phases and V0.4/V0.5 outcomes/report migrations follow 08. Core ER is a summary omitting sampling/checkpoint fields; text and 08 are normative.

V0.2 phase/phase_state/cycle_index and independent phase_checkpoint reference session; phase_state distinguishes active/frozen break. V0.1 starts sample the continuous trusted attribution anchor rather than re-anchoring raw wall time; see 08 §7.

Public pause/resume serve ordinary timing and Pomodoro work; break uses explicit phase commands. V0.2 migrates pomodoro_cycle and interval.cycle_index together, deriving work phase elapsed from that cycle and preserving ownership during recovery. Ordinary budget fields are null for stopwatch/pomodoro; see 08 §8.

Recovering edits/confirmation use reconcile, never correct; uncertain discard is reconcile(action=discard_uncertain), whole-session void is discard_session. Broken invariants require diagnosis, never guessed auto-repair.

## Startup scan versions and audit

Read-only scanning does not increment revision. If a scan transaction changes session state, run_id or interval facts, increment each changed session.row_version once, increment revision once for that transaction, and record time_edit. No-op scans add neither versions nor audit. This includes rebinding paused sessions to the current run. Recovering sessions retain their original recovery attribution until reconcile updates run_id with audit. Isolate corrupt records for diagnosis without guessing repairs. P7 establishes the startup entry point, P3 connects the actual scan after implementation, and P6 hardens that same entry point.

**Why time_edit rather than a new table:** it is the session-scoped before/after audit already written by `reconcile` (02 §3) and by an explicit `resume` that moves `run_id` to the current run (§10). A scan changing `run_id` or state is the same kind of fact, so reusing the table keeps "who changed this attribution, and from what" answerable through a single query instead of a second audit surface built just for the startup scan.
