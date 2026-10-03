//! V0.1 的 DDL（迁移起点，02 §2）。
//!
//! 这份 SQL 是**已发布迁移的正文**，一旦发布不得原地改写——后续变更走新的
//! `SCHEMA_VERSION` 与增量迁移。
//!
//! 相对计划附录补全的内容（Task 2 要求）：
//! - 枚举 CHECK：`task.status`、`work_session.{mode,state}`、`timer_kind`、`tag.kind`；
//! - 质量组合 CHECK：`quality` 只在 Review/Done/Cancelled 有值，`abandoned` 只随 Cancelled；
//! - 版本非负：所有 `row_version >= 0`；
//! - 外键显式 `ON DELETE RESTRICT`（02 §9「外键默认 RESTRICT」）。
//!
//! 明确**没有**做的事：
//! - 不枚举 `task.quality` / `work_session.quality` 的取值域——规格只规定了它与状态的
//!   *组合*规则，没给取值清单，凭空发明一套会与后续版本打架；取值合法性由服务校验。
//! - 不建 Goal/Milestone/番茄钟表，也不给 `project.goal_id`、`task.milestone_id` 留列
//!   （02 §2 原文：到 V0.2 迁移时才加）。
//! - `Scheduled`、`BACKGROUND`/`PASSIVE`/`WAITING` 在**结构上允许**，V0.1 由服务层拒绝写入。
//!   schema 不承担版本裁剪，否则 V0.2 要改已发布的 CHECK。

/// V0.1 的完整 DDL。12 张表 + 8 个索引。
pub const SCHEMA_V1_SQL: &str = r#"
CREATE TABLE app_meta(
  singleton   INTEGER PRIMARY KEY CHECK (singleton = 1),
  data_epoch  TEXT    NOT NULL,
  revision    INTEGER NOT NULL CHECK (revision >= 0)
);

CREATE TABLE application_run(
  id            TEXT PRIMARY KEY NOT NULL,
  started_at    INTEGER NOT NULL,
  clean_exit_at INTEGER
);

CREATE TABLE project(
  id          TEXT PRIMARY KEY NOT NULL,
  name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
  description TEXT,
  row_version INTEGER NOT NULL CHECK (row_version >= 0),
  status      TEXT NOT NULL CHECK (status IN ('active','archived','done')),
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE TABLE task(
  id                     TEXT PRIMARY KEY NOT NULL,
  project_id             TEXT REFERENCES project(id) ON DELETE RESTRICT,
  parent_task_id         TEXT REFERENCES task(id)    ON DELETE RESTRICT,
  title                  TEXT NOT NULL CHECK (length(trim(title)) > 0),
  description            TEXT,
  status                 TEXT NOT NULL CHECK (status IN (
                           'Inbox','Clarifying','Ready','Scheduled','Doing',
                           'Blocked','Waiting','Review','Done','Cancelled')),
  priority_json          TEXT,
  estimated_json         TEXT,
  planned_duration_ms    INTEGER CHECK (planned_duration_ms IS NULL OR planned_duration_ms >= 0),
  deadline               INTEGER,
  importance             INTEGER,
  urgency                INTEGER,
  energy_required        INTEGER,
  difficulty             INTEGER,
  completion_criteria    TEXT,
  quality                TEXT,
  baseline_estimate_json TEXT,
  row_version            INTEGER NOT NULL CHECK (row_version >= 0),
  created_at             INTEGER NOT NULL,
  updated_at             INTEGER NOT NULL,
  -- 非终结工作结果的状态质量为空；重开（回 Ready）时清当前质量。
  CONSTRAINT ck_task_quality_scope CHECK (
    quality IS NULL OR status IN ('Review','Done','Cancelled')
  ),
  -- abandoned 只对应 Cancelled，不能挂到 Review/Done 上。
  CONSTRAINT ck_task_quality_abandoned CHECK (
    quality IS NULL OR quality <> 'abandoned' OR status = 'Cancelled'
  )
);

CREATE TABLE work_session(
  id                 TEXT PRIMARY KEY NOT NULL,
  task_id            TEXT NOT NULL REFERENCES task(id)            ON DELETE RESTRICT,
  run_id             TEXT NOT NULL REFERENCES application_run(id) ON DELETE RESTRICT,
  mode               TEXT NOT NULL CHECK (mode IN ('FOREGROUND','BACKGROUND','PASSIVE','WAITING')),
  state              TEXT NOT NULL CHECK (state IN ('running','paused','recovering','finished','discarded')),
  timer_kind         TEXT NOT NULL CHECK (timer_kind IN ('stopwatch','countdown')),
  target_duration_ms INTEGER,
  started_at         INTEGER NOT NULL,
  ended_at           INTEGER,
  last_heartbeat_at  INTEGER,
  interruption_of    TEXT REFERENCES work_session(id) ON DELETE RESTRICT,
  quality            TEXT,
  needs_review       INTEGER NOT NULL DEFAULT 0 CHECK (needs_review IN (0,1)),
  row_version        INTEGER NOT NULL CHECK (row_version >= 0),
  -- 暂停后恢复仍是同一 session；结束后再次开始是新 session。
  CONSTRAINT ck_session_range CHECK (ended_at IS NULL OR ended_at >= started_at),
  CONSTRAINT ck_timer_budget CHECK (
    (timer_kind='stopwatch' AND target_duration_ms IS NULL) OR
    (timer_kind='countdown' AND target_duration_ms IS NOT NULL AND target_duration_ms > 0)
  ),
  CONSTRAINT ck_session_quality_scope CHECK (
    quality IS NULL OR state IN ('finished','discarded')
  )
);

CREATE TABLE work_interval(
  id                  TEXT PRIMARY KEY NOT NULL,
  session_id          TEXT NOT NULL REFERENCES work_session(id) ON DELETE RESTRICT,
  started_at          INTEGER NOT NULL,
  ended_at            INTEGER,
  voided_at           INTEGER,
  duration_ms         INTEGER,
  sampled_end_wall_at INTEGER,
  needs_review        INTEGER NOT NULL DEFAULT 0 CHECK (needs_review IN (0,1)),
  CONSTRAINT ck_interval_range CHECK (ended_at IS NULL OR ended_at >= started_at),
  -- 必须写成 CASE 而不是 OR：SQLite 把 CHECK 结果为 NULL 当作通过，
  -- 而 `duration_ms = ended_at - started_at` 在 duration_ms 为 NULL 时求值为 NULL，
  -- 用 OR 连接会让「可信闭合却没有 duration_ms」这条悄悄溜过去。
  CONSTRAINT ck_interval_duration CHECK (
    CASE
      WHEN duration_ms IS NOT NULL THEN
        ended_at IS NOT NULL AND duration_ms = ended_at - started_at AND duration_ms >= 0
      WHEN ended_at IS NOT NULL AND needs_review = 0 THEN
        0                                   -- 可信闭合必须有 duration_ms
      ELSE 1
    END
  ),
  -- 待确认与已作废是两类事实，不能同时成立。
  CONSTRAINT ck_interval_voided CHECK (voided_at IS NULL OR needs_review = 0)
);

CREATE TABLE interval_checkpoint(
  interval_id    TEXT PRIMARY KEY NOT NULL REFERENCES work_interval(id)    ON DELETE RESTRICT,
  run_id         TEXT NOT NULL    REFERENCES application_run(id)  ON DELETE RESTRICT,
  wall_at        INTEGER NOT NULL,
  attribution_at INTEGER NOT NULL,
  elapsed_ms     INTEGER NOT NULL CHECK (elapsed_ms >= 0)
);

CREATE TABLE time_edit(
  id          TEXT PRIMARY KEY NOT NULL,
  session_id  TEXT NOT NULL REFERENCES work_session(id) ON DELETE RESTRICT,
  before_json TEXT NOT NULL,
  after_json  TEXT NOT NULL,
  reason      TEXT,
  created_at  INTEGER NOT NULL
);

CREATE TABLE task_change(
  id          TEXT PRIMARY KEY NOT NULL,
  task_id     TEXT NOT NULL REFERENCES task(id) ON DELETE RESTRICT,
  before_json TEXT NOT NULL,
  after_json  TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE daily_plan(
  task_id    TEXT NOT NULL REFERENCES task(id) ON DELETE RESTRICT,
  local_date TEXT NOT NULL CHECK (local_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  timezone   TEXT NOT NULL CHECK (length(trim(timezone)) > 0),
  PRIMARY KEY (task_id, local_date, timezone)
);

CREATE TABLE tag(
  id          TEXT PRIMARY KEY NOT NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('Domain','Activity','Context','Report')),
  name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
  parent_id   TEXT REFERENCES tag(id) ON DELETE RESTRICT,
  row_version INTEGER NOT NULL CHECK (row_version >= 0),
  created_at  INTEGER NOT NULL,
  -- 层级属 V0.2（Knowledge）。V0.1 不接受非空 parent_id —— 这里用结构约束挡住，
  -- 因为「有 parent 却没有对应 kind」在任何版本都不是合法数据。
  CONSTRAINT ck_tag_parent_kind CHECK (parent_id IS NULL)
);

CREATE TABLE task_tag(
  task_id TEXT NOT NULL REFERENCES task(id) ON DELETE RESTRICT,
  tag_id  TEXT NOT NULL REFERENCES tag(id)  ON DELETE RESTRICT,
  -- V0.1 恒为 NULL：权重规则（每 kind 总和 ≤1、不自动归一化）属 V0.2（F-108）。
  weight  REAL CHECK (weight IS NULL OR (weight >= 0.0 AND weight <= 1.0)),
  PRIMARY KEY (task_id, tag_id)
);

-- 索引名与定义逐条抄自 02 §2 的索引块，不得改名或改列：
-- 名字是下游（迁移对比、诊断、测试）的稳定引用点。
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
"#;

#[cfg(test)]
mod tests {
    use super::SCHEMA_V1_SQL;

    /// 12 张表一个不少、一个不多。
    #[test]
    fn declares_exactly_the_twelve_v01_tables() {
        let mut tables: Vec<&str> = SCHEMA_V1_SQL
            .lines()
            .filter_map(|l| l.trim().strip_prefix("CREATE TABLE "))
            .map(|rest| rest.split(['(', ' ']).next().unwrap())
            .collect();
        tables.sort_unstable();
        assert_eq!(
            tables,
            vec![
                "app_meta",
                "application_run",
                "daily_plan",
                "interval_checkpoint",
                "project",
                "tag",
                "task",
                "task_change",
                "task_tag",
                "time_edit",
                "work_interval",
                "work_session",
            ]
        );
    }

    /// 八个索引，名字与 02 §2 逐字一致——这是下游（迁移对比、诊断）的稳定引用点。
    #[test]
    fn declares_exactly_the_eight_spec_indexes() {
        let mut idx: Vec<&str> = SCHEMA_V1_SQL
            .lines()
            .filter_map(|l| {
                let t = l.trim();
                t.strip_prefix("CREATE UNIQUE INDEX ")
                    .or_else(|| t.strip_prefix("CREATE INDEX "))
            })
            .map(|rest| rest.split(['(', ' ']).next().unwrap())
            .collect();
        idx.sort_unstable();
        assert_eq!(
            idx,
            vec![
                "idx_interval_session",
                "idx_session_task",
                "idx_task_project",
                "idx_task_status",
                "uq_open_interval",
                "uq_running_foreground",
                "uq_tag_child",
                "uq_tag_root",
            ]
        );
    }

    /// V0.2 的实体不得提前建表（02 §2：到 V0.2 迁移时才加）。
    #[test]
    fn no_v02_tables_leak_in() {
        for banned in [
            "goal",
            "milestone",
            "time_block",
            "pomodoro",
            "task_knowledge",
            "context_fact",
        ] {
            assert!(
                !SCHEMA_V1_SQL.contains(&format!("CREATE TABLE {banned}")),
                "V0.1 不得建 {banned} 表"
            );
        }
        assert!(
            !SCHEMA_V1_SQL.contains("goal_id"),
            "project.goal_id 属 V0.2"
        );
        assert!(
            !SCHEMA_V1_SQL.contains("milestone_id"),
            "task.milestone_id 属 V0.2"
        );
        assert!(
            !SCHEMA_V1_SQL.contains("cycle_index"),
            "work_interval.cycle_index 属 V0.2"
        );
    }
}
