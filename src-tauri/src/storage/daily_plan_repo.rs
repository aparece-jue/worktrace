//! 今日计划仓储（P4 Task 4，F-010 的「今日选择」半边）。
//!
//! `daily_plan` 是**人工选择**的集合，不是排期：一行 = 「这个任务被选进了这一天
//! （某个时区的某一天）」。表里只有三个键列，没有时间戳，也没有版本列。
//!
//! - **复合键 `(task_id, local_date, timezone)`**：同一个任务同一天在不同时区键下
//!   是两行（`US/Eastern` 与 `America/New_York` 在 tzdb 里本来就是两个名字，
//!   裁决 R-T4-b）。这里**不做**任何合并或迁移——一行记的是「当时那个时区的那一天」
//!   这个事实，换时区只影响之后写入的行。
//! - **日期用 `&LocalDate`**：真实日历日由 `services::daily_plan::parse_local_date`
//!   校验，仓储只接受校验之后的类型（裁决 R-T4-a）。`timezone` 同理，只接受
//!   `normalize_timezone` 产出的**存储键**，仓储里不再解析一次时区。
//! - **写函数取调用方的 `&Transaction`**：不自行 `begin`/`commit`，也不自行
//!   `bump_revision`（总纲 §9）。幂等（重复加入 / 移除）在这里就断掉，返回
//!   [`WriteOutcome::Unchanged`]，**不发任何写语句**（与 `tag_repo` 同一口径）。
//! - **审计**：一次真的变化写一条 `task_change`，记的是**这个任务计划集合**的变化
//!   前后（与 `tag_repo::tag_task` 记标签集合同一形状）。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::domain::error::DomainError;
use crate::domain::localdate::LocalDate;
use crate::error::AppError;

use super::db::map_sqlite;
use super::task_repo::{self, record_change, TaskRow};
use super::WriteOutcome;

/// 一条计划行的键 `(local_date 落库字符串, timezone 存储键)`。
///
/// 审计要「变化前后的完整集合」，幂等判断要「这个键在不在」——两处读同一份形状。
type PlanKey = (String, String);

/// 某一天（某个时区）的今日计划，按 `task.created_at, task.id` 稳定排序（R-T4-c）。
///
/// 这是**读**：任务是 `JOIN` 出来的完整行（UI 要显示标题与状态），计划行本身只有
/// 三个键列。`timezone` 必须是 [`crate::services::daily_plan::normalize_timezone`]
/// 的产物，否则读出来的是另一个键下的行——那是调用方的错，仓储不再兜底。
pub fn plan_for(
    conn: &Connection,
    date: &LocalDate,
    timezone: &str,
) -> Result<Vec<TaskRow>, AppError> {
    // 复用 `task_repo` 的投影与映射：任务行的列顺序与状态枚举解析只留一份，
    // 两处各写一份迟早会漂移。
    let sql = format!(
        "{} JOIN daily_plan ON daily_plan.task_id = task.id
         WHERE daily_plan.local_date = ?1 AND daily_plan.timezone = ?2
         ORDER BY task.created_at, task.id",
        task_repo::SELECT
    );
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt
        .query_map(
            rusqlite::params![date.to_string(), timezone],
            task_repo::read_row,
        )
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 把任务加入某一天的今日计划。这一行已经在集合里 ⇒ [`WriteOutcome::Unchanged`]：
/// 不写审计、不发任何写语句。
///
/// 只写 `daily_plan` 一行：**不碰任务行**（不设 `Scheduled`、不动 `updated_at` /
/// `row_version`）、不建 `time_block`、不启动计时（裁决 R-T4-e）。
pub fn add_to_plan(
    tx: &Transaction<'_>,
    task_id: &str,
    date: &LocalDate,
    timezone: &str,
    now: i64,
) -> Result<WriteOutcome<()>, AppError> {
    // 「找不到」用 `UnknownTask`：外键也会拦，但那会以 STORAGE_ERROR 的形式出现，
    // 用户看到的是「存储错误」而不是「找不到这个任务」（与 `tag_repo` 同一口径）。
    require_task(tx, task_id)?;

    let before = plan_keys_of_task(tx, task_id)?;
    let key: PlanKey = (date.to_string(), timezone.to_string());
    if before.contains(&key) {
        return Ok(WriteOutcome::Unchanged(()));
    }

    tx.execute(
        "INSERT INTO daily_plan(task_id, local_date, timezone) VALUES(?1, ?2, ?3)",
        rusqlite::params![task_id, key.0, key.1],
    )
    .map_err(map_sqlite)?;

    // 新行在集合里的位置由它自己的 `(local_date, timezone)` 决定，所以重读一次而不是
    // 猜着插进 `before` 里——顺序与 `plan_keys_of_task` 必须逐字一致（同 `tag_repo`）。
    let after = plan_keys_of_task(tx, task_id)?;
    record_change(tx, task_id, &plan_json(&before), &plan_json(&after), now)?;

    Ok(WriteOutcome::Changed(()))
}

/// 把任务从某一天的今日计划里移除。这一行本来就不在集合里 ⇒
/// [`WriteOutcome::Unchanged`]：不写审计、不发任何写语句。
///
/// 只删 `(task_id, local_date, timezone)` 那一行：别的日期、别的时区键下的计划行
/// 都不动（跨日与换时区都靠这条保持事实）。
pub fn remove_from_plan(
    tx: &Transaction<'_>,
    task_id: &str,
    date: &LocalDate,
    timezone: &str,
    now: i64,
) -> Result<WriteOutcome<()>, AppError> {
    require_task(tx, task_id)?;

    let before = plan_keys_of_task(tx, task_id)?;
    let key: PlanKey = (date.to_string(), timezone.to_string());
    if !before.contains(&key) {
        return Ok(WriteOutcome::Unchanged(()));
    }

    tx.execute(
        "DELETE FROM daily_plan WHERE task_id = ?1 AND local_date = ?2 AND timezone = ?3",
        rusqlite::params![task_id, key.0, key.1],
    )
    .map_err(map_sqlite)?;

    // 删除不动其余行的相对顺序，所以 after 直接从 before 推出来即可（省一次读，
    // 同 `tag_repo::untag_task`）。
    let after: Vec<PlanKey> = before.iter().filter(|k| *k != &key).cloned().collect();
    record_change(tx, task_id, &plan_json(&before), &plan_json(&after), now)?;

    Ok(WriteOutcome::Changed(()))
}

/// 这个任务当前的计划键，按 `(local_date, timezone)` 排序。
///
/// 幂等判断与审计共用这一次读：两处要的是同一份「集合」。
fn plan_keys_of_task(conn: &Connection, task_id: &str) -> Result<Vec<PlanKey>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT local_date, timezone FROM daily_plan WHERE task_id = ?1
             ORDER BY local_date, timezone",
        )
        .map_err(map_sqlite)?;
    let rows = stmt
        .query_map([task_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 审计里的计划集合形状：`{"daily_plan":[{"local_date":…,"timezone":…}, …]}`。
///
/// 记**变化前后的完整集合**（不是「加了一条」这种相对描述），这样一条审计可以脱离
/// 当时的库状态被读懂——与 `tag_repo` 记 `{"tags":[…]}` 同一理由。
fn plan_json(keys: &[PlanKey]) -> String {
    let entries: Vec<serde_json::Value> = keys
        .iter()
        .map(|(date, timezone)| serde_json::json!({ "local_date": date, "timezone": timezone }))
        .collect();
    serde_json::json!({ "daily_plan": entries }).to_string()
}

/// 任务必须存在，否则 [`DomainError::UnknownTask`]。
fn require_task(conn: &Connection, id: &str) -> Result<(), AppError> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM task WHERE id = ?1", [id], |r| r.get(0))
        .optional()
        .map_err(map_sqlite)?;
    match found {
        Some(_) => Ok(()),
        None => Err(DomainError::UnknownTask.into()),
    }
}
