# Worktrace — Data Model

| Item | Value |
| --- | --- |
| Status | Design draft (under review) |
| Date | 2026-10-02 |
| Upstream | [`00-architecture.en.md`](00-architecture.en.md), [`01-module-breakdown.en.md`](01-module-breakdown.en.md) |
| Implemented by | M01 Storage, M02 Domain model |
| Chinese version | [`02-data-model.zh.md`](02-data-model.zh.md) |

> This document defines the entity relations, tables, and state machines. **Full DDL and migration scripts belong to M01's own spec.**

---

## 1. Entity relations

```
Goal 1──n Project 1──n Milestone 1──n Task
                                        │
        Task ──self──→ Task              │  parent_task_id (WBS hierarchy)
        Task n──n Task  (Dependency)     │  blocks / depends_on / related / parallel
        Task 1──n WorkSession ───────────┘
        WorkSession 1──n SessionSegment  (from V1)
        Task n──n Tag   (carries weight)
        Tag  ──self──→ Tag               (hierarchy only for kind = knowledge)
        Project 1──n ContextFact
        Project 1──n Decision
        Project 1──n Document ──n Task   (many-to-many)
        Tag(kind=knowledge) 1──1 KnowledgeStat
        Task n──n KnowledgeStat          (through TaskKnowledge)
```

---

## 2. Tables

### 2.0 Which tables belong to which milestone

This design covers the whole product lifecycle, but **not every table is created in V0.1**:

| Tables | Introduced in |
| --- | --- |
| `goal` `project` `milestone` `task` `work_session` `tag` `task_tag` | V0.1 |
| `knowledge_stat` `task_knowledge` | V0.2 |
| `context_fact` `decision` `document` `task_document` | V0.4 |

**Migration strategy**: V0.1 creates only the V0.1 tables. Later milestones **add** tables through migrations rather than creating everything up front.

Creating unused tables freezes structure that has not been thought through yet, and unused tables attract change requests — precisely the speculative design SPEC §57 warns against. The same applies to columns: `priority_json` and `estimated_json` on `task`, which serve V0.3's AI metadata, may start as plain columns in V0.1 and migrate to the envelope shape in V0.3.

### 2.1 Goals and projects

```sql
goal(
  id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT,
  status TEXT NOT NULL,                    -- active / done / dropped
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)

project(
  id TEXT PRIMARY KEY,
  goal_id TEXT REFERENCES goal(id),
  name TEXT NOT NULL, description TEXT,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)

milestone(
  id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id),
  title TEXT NOT NULL, status TEXT NOT NULL,
  due_at INTEGER, done_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)
```

### 2.2 Task

```sql
task(
  id TEXT PRIMARY KEY,
  project_id TEXT REFERENCES project(id),
  milestone_id TEXT REFERENCES milestone(id),
  parent_task_id TEXT REFERENCES task(id),   -- WBS hierarchy; an Action is simply a leaf Task
  title TEXT NOT NULL, description TEXT,
  status TEXT NOT NULL,                      -- see §3

  priority_json TEXT,                        -- {"value":"P1","source":"user","confidence":null,"confirmed_at":...}
  estimated_json TEXT,                       -- same envelope, carries SPEC §22 estimated_duration

  importance INTEGER, urgency INTEGER,       -- 0..3
  deadline INTEGER,
  scheduled_start INTEGER, scheduled_end INTEGER,
  planned_duration INTEGER,                  -- minutes; the user's own plan

  energy_required INTEGER, difficulty INTEGER,
  completion_criteria TEXT,
  quality TEXT,                              -- see §5
  needs_review INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)
```

**On `actual_duration`**: SPEC §8.4 lists it, and this design **deliberately omits it**. Actual effort is always aggregated from `work_session`:

```sql
SELECT SUM(ended_at - started_at - paused_total_ms) FROM work_session WHERE task_id = ?
```

A stored copy would be a second source of truth and would inevitably drift from the detail rows. `work_session(task_id)` is indexed, and at personal scale (a few hundred thousand rows over ten years) the aggregation cost is negligible. If reports ever become a bottleneck, add a **materialized summary table** maintained by M01 when a session ends — not a semantically vague column on `task`. The deviation is recorded in §7.

### 2.3 WorkSession

```sql
work_session(
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES task(id),
  mode TEXT NOT NULL,                        -- FOREGROUND / BACKGROUND / PASSIVE / WAITING
  started_at INTEGER NOT NULL,
  ended_at INTEGER,                          -- NULL = still running
  paused_total_ms INTEGER NOT NULL DEFAULT 0,
  target_end INTEGER,                        -- target instant for countdown/Pomodoro; NULL for stopwatch
  interruption_of TEXT REFERENCES work_session(id),
  quality TEXT,
  needs_review INTEGER NOT NULL DEFAULT 0,   -- set after crash recovery, see §6
  last_heartbeat_at INTEGER,                 -- refreshed every 30s while running
  created_at INTEGER NOT NULL
)
```

**This table has no `remaining` and no `elapsed`.** That is SPEC §15's rule expressed in the schema: store instants only, compute every displayed value on demand.

```rust
elapsed_ms   = now - started_at - paused_total_ms
remaining_ms = target_end - now          // meaningless when target_end IS NULL
```

### 2.4 Tags

```sql
tag(
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,        -- domain / activity / knowledge / context / report
  name TEXT NOT NULL,
  parent_id TEXT REFERENCES tag(id),         -- used only by kind = knowledge
  created_at INTEGER NOT NULL,
  UNIQUE(kind, parent_id, name)
)

task_tag(
  task_id TEXT NOT NULL REFERENCES task(id),
  tag_id  TEXT NOT NULL REFERENCES tag(id),
  weight  REAL,                              -- NULL = associated-duration semantics; value = weighted-duration semantics
  PRIMARY KEY (task_id, tag_id)
)
```

A single `weight` column carries both of SPEC §18's measures; **no second table**:

| `weight` | Semantics | How it is counted |
| --- | --- | --- |
| `NULL` | Associated duration | The tag counts the task's full effort |
| value (e.g. 0.6) | Weighted duration | Effort allocated proportionally |

Weights are **not** forced to sum to 1. When they do not, M06 decides the normalization strategy; that decision belongs to M06's spec.

### 2.5 Knowledge

```sql
knowledge_stat(
  tag_id TEXT PRIMARY KEY REFERENCES tag(id),   -- kind must be knowledge
  experience_hours REAL NOT NULL DEFAULT 0,
  recent_hours REAL NOT NULL DEFAULT 0,
  application_count INTEGER NOT NULL DEFAULT 0,
  learning_count INTEGER NOT NULL DEFAULT 0,
  skill_level REAL, confidence REAL,            -- both required, SPEC §20
  estimate_accuracy REAL,
  last_used_at INTEGER
)

task_knowledge(
  task_id TEXT NOT NULL REFERENCES task(id),
  knowledge_tag_id TEXT NOT NULL REFERENCES tag(id),
  weight REAL, required_level REAL,
  used INTEGER NOT NULL DEFAULT 0,
  learning_gain REAL, confidence REAL,
  source TEXT,                                  -- user / ai
  PRIMARY KEY (task_id, knowledge_tag_id)
)
```

`skill_level` and `confidence` **must coexist** (SPEC §20): the first estimates ability, the second how much evidence backs it. Keeping only one produces "three tasks and you are an expert".

### 2.6 Context and decisions

```sql
context_fact(
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES project(id),
  key TEXT NOT NULL, value TEXT NOT NULL,
  source_type TEXT NOT NULL, source_id TEXT,
  confidence REAL,
  security_level TEXT NOT NULL DEFAULT 'INTERNAL',   -- PUBLIC/INTERNAL/CONFIDENTIAL/STRICT_LOCAL
  created_at INTEGER NOT NULL,
  superseded_by TEXT REFERENCES context_fact(id)     -- versioned, never overwritten
)

decision(
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES project(id),
  title TEXT NOT NULL, reason TEXT,
  decided_at INTEGER, created_at INTEGER NOT NULL
)

document(
  id TEXT PRIMARY KEY,
  project_id TEXT REFERENCES project(id),
  path TEXT NOT NULL, kind TEXT, security_level TEXT NOT NULL DEFAULT 'INTERNAL',
  created_at INTEGER NOT NULL
)

task_document(task_id TEXT NOT NULL, document_id TEXT NOT NULL, PRIMARY KEY(task_id, document_id))
```

`context_fact` **versions rather than overwrites** via `superseded_by` (SPEC §28): changing a parameter keeps the old value, marked superseded. The AI never reads stale data, and history survives.

---

## 3. Task state machine

```
Inbox ──→ Clarifying ──→ Ready ──→ Scheduled ──→ Doing ──→ Review ──→ Done
                            │           │           │
                            └───────────┴───────────┴──→ Blocked
                            └───────────┴───────────┴──→ Waiting
                                                        └──→ Cancelled
```

| State | Meaning |
| --- | --- |
| Inbox | Captured, not yet clarified |
| Clarifying | Working out what the next action is |
| Ready | Actionable but unscheduled |
| Scheduled | Placed in a time block |
| Doing | In progress |
| **Blocked** | **I cannot proceed** (missing skill, missing decision, unfinished prerequisite) |
| **Waiting** | **Waiting on someone or something else** (a reply, a part, a test, an approval) |
| Review | Finished, pending check |
| Done / Cancelled | Terminal |

`Blocked` and `Waiting` are distinct and must not be merged (SPEC §9). Transition rules live in `domain/task/state.rs` and are **enforced in the domain layer** — never scattered into services or commands. An illegal transition returns a domain error and writes nothing.

---

## 4. WorkSession lifecycle

```
created ──start──→ running ──pause──→ paused ──resume──→ running
                     │                                     │
                     ├──interrupt──→ running (new session) │
                     │               original → paused      │
                     └──finish───────────────────────────→ finished
```

| State | Test |
| --- | --- |
| running | `ended_at IS NULL` and not paused |
| paused | `ended_at IS NULL` and `paused_total_ms` already includes the current pause span |
| finished | `ended_at IS NOT NULL` |

Pausing **does not insert a row**; it accumulates `paused_total_ms` and records the pause start (in a companion field M05 defines). A task worked in three sittings therefore yields **three sessions**, matching SPEC §11's example (09:00–09:35 / 14:20–15:10 / 16:30–17:00).

**Execution mode affects statistics, not storage**:

| mode | Statistical meaning |
| --- | --- |
| FOREGROUND | Counts as human effort |
| BACKGROUND | Runs in parallel; human effort accounted separately |
| PASSIVE | Machine/passive process (e.g. a simulation); not human effort |
| WAITING | Waiting on something external; not human effort |

This is what guarantees SPEC §12's rule that "1h designing + 1h of AI generation ≠ 2h of human effort".

---

## 5. Completion quality

`quality` values (SPEC §24): `normal` / `reworked` / `review_failed` / `partially_done` / `abandoned`.

Its purpose is to stop the system drawing a wrong conclusion: "finished fast = highly skilled". With it, M08 can separate "fast and clean" from "fast but reworked". It exists in two places — `task.quality` (task-level verdict) and `work_session.quality` (per-sitting verdict) — with different purposes; neither is derived from the other.

---

## 6. Invariants the service layer must enforce

The schema can express only part of the truth. These four belong to `services/` and need tests:

| # | Invariant | Why the schema cannot express it |
| --- | --- | --- |
| 1 | **At most one FOREGROUND session** | A unique index cannot say "only where mode = FOREGROUND and ended_at IS NULL" |
| 2 | **`task.quality` is set only when status ⊆ {Done}** | Cross-table, cross-state conditional |
| 3 | **`knowledge_stat.tag_id` must reference a tag of kind = knowledge** | SQLite has no conditional cross-table foreign key |
| 4 | **Crash recovery: sessions with `ended_at IS NULL` past their heartbeat get `needs_review = 1`** | Requires a business-chosen time threshold |

The full meaning of (4) (SPEC §46, crash recovery):

- On startup, scan for sessions where `ended_at IS NULL` and `last_heartbeat_at` is older than two minutes.
- **Do not backfill `ended_at`.** Fabricated effort is worse than missing effort: it corrupts statistics, estimate prediction, and the proficiency model.
- Set `needs_review = 1` and ask the user to confirm how that span should be attributed.
- Basis: SPEC §5.3 `User > Rule > AI`, §57 "data must be explainable".

---

## 7. Deviations from SPEC

| Deviation | SPEC | This design | Rationale |
| --- | --- | --- | --- |
| No `task.actual_duration` | §8.4 lists the field | Aggregated from `work_session` | Avoids a second source of truth; see §2.2 |
| No `Action` table | §3.2's WBS tree has an Action level | Task self-reference; an Action is a leaf Task | Two near-identical tables mean two repositories and two state machines |
| AI metadata in JSON columns | §33 requires value/source/confidence/confirmed | Inline JSON columns, no EAV table | The field set is known at compile time and always read/written whole; EAV is for run-time-unknown attribute sets |
| New `last_heartbeat_at` | Not mentioned | Added | SPEC §46 requires crash recovery; without a heartbeat the crashed span is unknowable |
| New event-level `revision` | Not mentioned | Added | See `00-architecture.en.md` §5.2 |

---

## 8. Indexes

```sql
CREATE INDEX idx_task_status        ON task(status);
CREATE INDEX idx_task_project       ON task(project_id);
CREATE INDEX idx_task_parent        ON task(parent_task_id);
CREATE INDEX idx_task_deadline      ON task(deadline) WHERE deadline IS NOT NULL;
CREATE INDEX idx_session_task       ON work_session(task_id);
CREATE INDEX idx_session_started    ON work_session(started_at);
CREATE INDEX idx_session_open       ON work_session(ended_at) WHERE ended_at IS NULL;
CREATE INDEX idx_task_tag_tag       ON task_tag(tag_id);
CREATE INDEX idx_context_fact_lookup ON context_fact(project_id, key) WHERE superseded_by IS NULL;
CREATE INDEX idx_document_project   ON document(project_id);
```

`idx_session_open` is a **partial index** serving the two hottest queries: find the running session, and scan for crash recovery. At personal scale it holds single-digit rows.

All timestamps are stored as **Unix milliseconds (INTEGER)** with no timezone attached; the presentation layer renders them in local time. This avoids daylight-saving and relocation ambiguity, and keeps SQLite's date functions out of business calculations.
