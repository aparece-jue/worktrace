//! Task 仓储（P1 Task 4）。
//!
//! **不接受 `Connection` 做写**：所有写函数取调用方的 `&Transaction`，
//! 不自行 `begin`/`commit`，也不自行 `bump_revision`。一次业务写恰好加一次
//! revision 的责任在服务层（总纲 §9）。

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::domain::error::DomainError;
use crate::domain::project::ProjectStatus;
use crate::domain::task::{TaskStatus, TaskTransition, TransitionCause};
use crate::error::AppError;

use super::db::map_sqlite;
use super::guards::guard_row_version;
use super::WriteOutcome;

/// `task` 的一行。
///
/// `Serialize` 的来由与 [`crate::storage::project_repo::ProjectRow`] 逐字相同：
/// `TaskQueryResult.tasks` / `DailyPlanView.tasks` 直接装它交给 IPC。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskRow {
    pub id: String,
    pub project_id: Option<String>,
    pub title: String,
    pub status: TaskStatus,
    pub quality: Option<String>,
    pub row_version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 任务行的投影。`pub(crate)`：`daily_plan_repo` 要 `JOIN daily_plan` 出同样的行，
/// 列顺序必须与 [`read_row`] 一起复用，否则两处迟早会漂移。
pub(crate) const SELECT: &str =
    "SELECT id, project_id, title, status, quality, row_version, created_at, updated_at FROM task";

/// 把一行读成 [`TaskRow`]。`pub(crate)`：与 [`SELECT`] 成对复用，理由同上。
pub(crate) fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    let status: String = r.get(3)?;
    Ok(TaskRow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        title: r.get(2)?,
        // schema 的 CHECK 保证了取值合法，所以解析失败只有两种可能：库被绕过 CHECK
        // 写坏过，或更新版本写入的取值被旧版本读到。两种都要**说清是哪一列**——
        // `InvalidQuery` 的文案是 "Query is not read-only"，会把人指向完全错误的方向。
        status: TaskStatus::parse(&status).ok_or_else(|| enum_error(3, "task.status", &status))?,
        quality: r.get(4)?,
        row_version: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

/// 新建任务。新任务状态为 `Inbox`，版本从 0 起。
///
/// 归属到**已归档**的项目一律拒绝（F-004），且在事务内检查，不只依赖 UI 过滤。
///
/// 三条拒绝用与 [`set_task_project`] **同一套**领域变体（`UnknownProject` /
/// `ProjectArchived` / `NotInThisVersion`）：这是同一个判断的另一个入口，
/// 用户看到的理由必须逐字一致。状态判定同样走 `project_repo::get_project` 的领域枚举，
/// 库里出现**无法识别**的取值时由它按列报 `UnknownEnumValue`，不会被这里误说成
/// 「已完成」。（原先借用 `EmptyText{field:"project"}` 与 `IntervalOpenInWrongState`，
/// 用户会读到「「project」不能为空。」和一句关于计时区间的胡话——见 Task 5 fix round 1。）
pub fn create_task(
    tx: &Transaction<'_>,
    id: &str,
    title: &str,
    project_id: Option<&str>,
    now: i64,
) -> Result<TaskRow, AppError> {
    if title.trim().is_empty() {
        // 字段名是**面向用户**的：`EmptyText` 的 `Display` 会把它拼进
        // 「「…」不能为空。」，所以写「任务标题」而不是列名 `task.title`。
        return Err(DomainError::EmptyText {
            field: "任务标题"
        }
        .into());
    }
    if let Some(pid) = project_id {
        // 与 `set_task_project` 逐字同一条读路径：状态判定走领域枚举。库里出现
        // **无法识别**的取值时由 `project_repo` 按列报 `UnknownEnumValue`，
        // 而不是在这里被 `_` 兜底说成「本版本不支持」。
        let project = crate::storage::project_repo::get_project(tx, pid)?
            .ok_or(DomainError::UnknownProject)?;
        match project.status {
            ProjectStatus::Active => {}
            ProjectStatus::Archived => return Err(DomainError::ProjectArchived.into()),
            // `done` 在 V0.1 写不出来；读到它说明库来自更新的版本，不去动它。
            ProjectStatus::Done => {
                return Err(DomainError::NotInThisVersion {
                    what: "把任务关联到已完成的项目",
                }
                .into())
            }
        }
    }

    tx.execute(
        "INSERT INTO task(id, project_id, title, status, row_version, created_at, updated_at)
         VALUES(?1, ?2, ?3, 'Inbox', 0, ?4, ?4)",
        rusqlite::params![id, project_id, title.trim(), now],
    )
    .map_err(map_sqlite)?;

    record_change(
        tx,
        id,
        "{}",
        &format!(
            "{{\"status\":\"Inbox\",\"title\":{}}}",
            serde_json::to_string(title.trim()).expect("serializing a string cannot fail")
        ),
        now,
    )?;

    get_task(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "task vanished after insert".into(),
    })
}

pub fn get_task(conn: &Connection, id: &str) -> Result<Option<TaskRow>, AppError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], read_row)
        .optional()
        .map_err(map_sqlite)
}

/// 任务必须存在，否则 [`DomainError::UnknownTask`]。
///
/// `pub(crate)`：`tag_repo` 与 `daily_plan_repo` 的写入口问的是同一个问题
/// （「这个任务在不在」），拒绝理由必须**只有一处**——两处各写一份逐字同形的副本
/// 是 Task 4 付的代价，Task 6 收口（T4 Minor 4）。
pub(crate) fn require_task(conn: &Connection, id: &str) -> Result<(), AppError> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM task WHERE id = ?1", [id], |r| r.get(0))
        .optional()
        .map_err(map_sqlite)?;
    match found {
        Some(_) => Ok(()),
        None => Err(DomainError::UnknownTask.into()),
    }
}

/// 项目筛选的**三值**表达（裁决 R-T5-a）。
///
/// 不用 `Option<Option<String>>`：那样「不限制项目」与「只要没有项目的」在类型上
/// 长得一样，读代码的人得去翻调用点才知道收到的是哪一种。三者互不混同：
/// `Any` 连没有项目的任务一起列，`None` 恰恰只要那些。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProjectFilter {
    /// 不限制项目。
    #[default]
    Any,
    /// 只要**没有**项目的任务（`project_id IS NULL`）。
    None,
    /// 只要归属到指定项目的任务。
    Id(String),
}

/// 任务筛选条件（裁决 R-T5-a）：三个字段**取交集**，未选择的条件不限制。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskFilter {
    /// 状态集合；空集合 = 不限制状态。
    pub statuses: Vec<TaskStatus>,
    pub project: ProjectFilter,
    /// 情境（上下文）标签。**必须是 `Context` 类标签**——那条规则由服务入口
    /// 拒绝（04 F-005 的「非法情境 ID 拒绝」），仓储这里是纯过滤：判断「哪一类」
    /// 要读 `tag` 表，那是 `tag_repo` 的事，两边各管一段。
    pub context_tag_id: Option<String>,
}

/// 分页窗口（裁决 R-T5-a）：`limit` 1..=100、`offset` >= 0，越界**拒绝**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub limit: i64,
    pub offset: i64,
}

/// 一页查询结果：`tasks` 是这一页，`total` 是**满足条件的总数**（与窗口无关）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPage {
    pub tasks: Vec<TaskRow>,
    pub total: i64,
}

/// 按条件筛选任务（F-002 的轻量 GTD 列表）。
///
/// - 条件之间取交集；未选择的条件不限制。**空 `filter` 就是整表**——这正是被删掉的
///   `list_tasks` 的语义（裁决 R-T5-d：无过滤的整表列表在 V0.1 没有别的消费者）；
/// - 稳定排序 `created_at, id`，分页走 `limit` / `offset`；
/// - 情境标签用 `EXISTS` 而不是 JOIN：一个任务挂多个标签时不会被放大成多行，
///   `total` 因此数的是**任务**，不是 JOIN 的行数；
/// - SQL 里只拼占位符**个数**（状态集合的长度来自代码），值一律参数绑定。
pub fn list_tasks_filtered(
    conn: &Connection,
    filter: &TaskFilter,
    page: Page,
) -> Result<TaskPage, AppError> {
    // 分页越界在这里就断掉：`LIMIT -1` 在 SQLite 里等于**不限制**、负数 `OFFSET`
    // 等于 0——放过去不会报错，只会悄悄返回错误的一页（裁决 R-T5-a）。
    require_page(page)?;

    let (where_sql, params) = filter_clause(filter);
    let total: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM task{where_sql}"),
            rusqlite::params_from_iter(params.iter()),
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;

    let sql = format!("{SELECT}{where_sql} ORDER BY created_at, id LIMIT ? OFFSET ?");
    let mut window = params;
    window.push(Value::Integer(page.limit));
    window.push(Value::Integer(page.offset));
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(window.iter()), read_row)
        .map_err(map_sqlite)?;
    let tasks = rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)?;

    Ok(TaskPage { tasks, total })
}

/// 分页窗口的输入校验。文案是面向用户的中文（与 `require_active_project` 同一口径）：
/// 调用方给的是页号与页大小时，「哪一项越界」就是用户唯一能采取行动的信息。
///
/// `pub(crate)`（P8 Task 2b）：[`super::session_repo::history_sessions`] 用同一个窗口
/// 类型与同一条规则——分页判据只有这一处实现，不复制第二份。
pub(crate) fn require_page(page: Page) -> Result<(), AppError> {
    if !(1..=100).contains(&page.limit) {
        return Err(AppError::Domain {
            detail: format!("「每页条数」只能是 1 到 100，收到的是 {}。", page.limit),
        });
    }
    if page.offset < 0 {
        return Err(AppError::Domain {
            detail: format!("「跳过条数」不能是负数，收到的是 {}。", page.offset),
        });
    }
    Ok(())
}

/// 筛选条件 → `WHERE` 片段 + 绑定参数。
///
/// 只有占位符是拼出来的，用户给的值（项目 ID、标签 ID）全部走参数绑定；
/// 状态取值来自 `TaskStatus::as_str()`，同样当参数传，不拼进 SQL 文本。
fn filter_clause(filter: &TaskFilter) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if !filter.statuses.is_empty() {
        let holders = vec!["?"; filter.statuses.len()].join(", ");
        clauses.push(format!("status IN ({holders})"));
        params.extend(
            filter
                .statuses
                .iter()
                .map(|s| Value::Text(s.as_str().to_string())),
        );
    }
    match &filter.project {
        ProjectFilter::Any => {}
        ProjectFilter::None => clauses.push("project_id IS NULL".to_string()),
        ProjectFilter::Id(id) => {
            clauses.push("project_id = ?".to_string());
            params.push(Value::Text(id.clone()));
        }
    }
    if let Some(tag_id) = &filter.context_tag_id {
        clauses.push(
            "EXISTS (SELECT 1 FROM task_tag
                     WHERE task_tag.task_id = task.id AND task_tag.tag_id = ?)"
                .to_string(),
        );
        params.push(Value::Text(tag_id.clone()));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, params)
}

/// 状态跃迁。与 `task_change` **同一事务**。
///
/// 只改状态与质量；「完成/取消时结束会话」是组合服务的事（P3 Task 6），
/// 仓储不越界去动 `work_session`。
pub fn transition_task(
    tx: &Transaction<'_>,
    id: &str,
    expected_version: i64,
    to: TaskStatus,
    cause: TransitionCause,
    now: i64,
) -> Result<TaskRow, AppError> {
    let before = get_task(tx, id)?.ok_or(DomainError::UnknownTask)?;
    guard_row_version(before.row_version, expected_version)?;

    let transition = TaskTransition::new(before.status, to, cause)?;

    // 重开要清当前质量（02 §5 末）。
    let quality = if transition.clears_quality()
        || !matches!(
            to,
            TaskStatus::Review | TaskStatus::Done | TaskStatus::Cancelled
        ) {
        None
    } else {
        before.quality.clone()
    };

    let n = tx
        .execute(
            "UPDATE task SET status = ?1, quality = ?2, row_version = row_version + 1,
                             updated_at = ?3
             WHERE id = ?4 AND row_version = ?5",
            rusqlite::params![to.as_str(), quality, now, id, expected_version],
        )
        .map_err(map_sqlite)?;
    if n == 0 {
        // 同事务内刚读过，n==0 只可能是并发写入——交给上层重试。
        return Err(AppError::VersionConflict {
            expected: expected_version,
            actual: -1,
        });
    }

    record_change(
        tx,
        id,
        &serde_json::json!({"status": before.status.as_str(), "quality": before.quality})
            .to_string(),
        &serde_json::json!({"status": to.as_str(), "quality": quality}).to_string(),
        now,
    )?;

    get_task(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "task vanished after update".into(),
    })
}

/// 写一条变更审计。与实体更新同事务——「操作失败不得出现半个审计记录」（02 §9）。
///
/// `pub(crate)`：任务行自己的变更（本模块）与标签关联的变更（`storage::tag_repo`）
/// 都是**这个任务**的历史，共用同一行形状——审计的写法只留一处。
pub(crate) fn record_change(
    tx: &Transaction<'_>,
    task_id: &str,
    before_json: &str,
    after_json: &str,
    now: i64,
) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO task_change(id, task_id, before_json, after_json, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            task_id,
            before_json,
            after_json,
            now
        ],
    )
    .map_err(map_sqlite)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 完成事件（P5 Task 4：周回顾的「完成任务」按**事件时刻**取）
// ─────────────────────────────────────────────────────────────────────────────

/// 一条「任务被置为 `Done`」的变更事件。
///
/// 只承载周回顾要的那两件事实：**哪个任务**、**什么时候完成**。事件时刻就是
/// `task_change.created_at`——02 §10 明写「报告按该记录的时刻选完成项，不以
/// `updated_at` 或 session 结束时间代替」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoneEvent {
    pub task_id: String,
    /// 完成事件时刻（Unix 毫秒）。
    pub completed_at: i64,
}

/// 与半开范围 `[from, to)` 相交的**完成事件**，按 `(created_at, task_id, id)` 升序。
///
/// # 为什么必须按**形状**过滤
///
/// `task_change` 有**三种形状**（任务字段 / 标签集合 / 今日计划集合，见
/// [`crate::storage`] 的模块文档），三种都进同一张表。所以「有没有 `task_change` 行」
/// 回答不了任何业务问题：贴一个标签、加一次今日计划、建一个任务同样会写一行。
/// 完成事件的判据只有一条——`json_extract(after_json, '$.status') = 'Done'`
/// （`transition_task` 写的就是这个形状，见 [`transition_task`]）；路径不存在时
/// `json_extract` 返回 `NULL`、比较为假，别的形状因此天然落选。
///
/// # 为什么返回**全部**事件，而不是「最后一条」
///
/// 一个任务「完成 → 重开 → 再完成」有**两个** Done 事件，而 `task_change` 同一毫秒
/// 可以落多行、`ORDER BY created_at, id` 在它们之间没有时间先后的意义（P3 台账的
/// 携带项）。所以这里既不 `LIMIT 1`、也不做「取最新」的归并：**每个 Done 事件各归其周**，
/// 由调用方按事件时刻分组。排序只为了让输出稳定（同一份库给出同一个顺序）。
///
/// 范围是**半开**的：`created_at == from` 算在内、`created_at == to` 不算
/// （周一 00:00 那一刻完成的属于新的一周）；`from == to` 返回空。调用方保证 `from <= to`
/// ——服务层用 `IntervalRange::new` 校验，与
/// [`crate::storage::session_repo::intervals_overlapping`] 同一约定。
pub fn done_events_within(
    conn: &Connection,
    from: i64,
    to: i64,
) -> Result<Vec<DoneEvent>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT task_id, created_at FROM task_change
             WHERE json_extract(after_json, '$.status') = 'Done'
               AND created_at >= ?1 AND created_at < ?2
             ORDER BY created_at, task_id, id",
        )
        .map_err(map_sqlite)?;
    let rows = stmt
        .query_map(rusqlite::params![from, to], |r| {
            Ok(DoneEvent {
                task_id: r.get(0)?,
                completed_at: r.get(1)?,
            })
        })
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 把「列里的值不在取值域内」包成带列名的 SQLite 转换错误。
///
/// 用 `FromSqlConversionFailure` 而不是 `InvalidQuery`：后者的文案是
/// "Query is not read-only"，对「库里的枚举值非法」这种情况会把人指向
/// 完全错误的方向，而诊断恰恰是库损坏时最需要的东西。
pub(crate) fn enum_error(column: usize, field: &'static str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        Box::new(crate::domain::error::DomainError::UnknownEnumValue {
            field,
            value: value.to_string(),
        }),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 估时信封与基准冻结（P2 Task 3）
// ─────────────────────────────────────────────────────────────────────────────

/// 任务的估时信封。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimateEnvelope {
    /// 当前估时（用户/AI 给的）。可能为空。
    pub estimated_json: Option<String>,
    /// **首次 start 时冻结下来的基准**。一旦冻结就不再随 `estimated_json` 变化
    /// （02 §9：后续改估时不改基准；显式重新定基准须保留 task_change）。
    pub baseline_estimate_json: Option<String>,
}

/// 本次调用是否真的冻结了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreezeOutcome {
    /// 首次 start：写入了基准（`baseline` 为 `None` 表示当时本来就没有估时）。
    Frozen {
        baseline: Option<String>,
        new_version: i64,
    },
    /// 早已冻结过，本次**不动**。`baseline` 是当时冻结下来的值。
    AlreadyFrozen { baseline: Option<String> },
}

/// 读估时信封。
pub fn read_estimate(
    conn: &Connection,
    task_id: &str,
) -> Result<Option<EstimateEnvelope>, AppError> {
    conn.query_row(
        "SELECT estimated_json, baseline_estimate_json FROM task WHERE id = ?1",
        [task_id],
        |r| {
            Ok(EstimateEnvelope {
                estimated_json: r.get(0)?,
                baseline_estimate_json: r.get(1)?,
            })
        },
    )
    .optional()
    .map_err(map_sqlite)
}

/// 首次 `start` 时冻结估时基准。
///
/// **判据是「这个任务还没有任何会话」，不是「`baseline_estimate_json` 是不是空」**——
/// 否则一个本来就没估时的任务会在每次 start 时反复「冻结」（每次都是 `NULL`），
/// 而 02 §9 要的是「第一次 start 时冻结，后续不改」。用会话存在与否做标记，
/// 第一次之后无论基准是不是 `NULL` 都不再动它。
pub fn freeze_baseline_estimate(
    tx: &Transaction<'_>,
    task_id: &str,
    expected_version: i64,
    now: i64,
) -> Result<FreezeOutcome, AppError> {
    let before = get_task(tx, task_id)?.ok_or(DomainError::UnknownTask)?;
    guard_row_version(before.row_version, expected_version)?;

    let existing: i64 = tx
        .query_row(
            "SELECT count(*) FROM work_session WHERE task_id = ?1",
            [task_id],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;
    let current: Option<String> = tx
        .query_row(
            "SELECT baseline_estimate_json FROM task WHERE id = ?1",
            [task_id],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;

    if existing > 0 {
        return Ok(FreezeOutcome::AlreadyFrozen { baseline: current });
    }

    let estimated: Option<String> = tx
        .query_row(
            "SELECT estimated_json FROM task WHERE id = ?1",
            [task_id],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;

    let n = tx
        .execute(
            "UPDATE task SET baseline_estimate_json = ?1, row_version = row_version + 1,
                             updated_at = ?2
             WHERE id = ?3 AND row_version = ?4",
            rusqlite::params![estimated, now, task_id, expected_version],
        )
        .map_err(map_sqlite)?;
    if n == 0 {
        return Err(AppError::VersionConflict {
            expected: expected_version,
            actual: -1,
        });
    }

    record_change(
        tx,
        task_id,
        &format!(
            "{{\"baseline_estimate_json\":{}}}",
            json_or_null(current.as_deref())
        ),
        &format!(
            "{{\"baseline_estimate_json\":{}}}",
            json_or_null(estimated.as_deref())
        ),
        now,
    )?;

    Ok(FreezeOutcome::Frozen {
        baseline: estimated,
        new_version: expected_version + 1,
    })
}

/// 把可选的 JSON 片段拼进审计文本：`None` 写成 `null`。
fn json_or_null(v: Option<&str>) -> String {
    v.unwrap_or("null").to_string()
}

/// 开始/继续计时必须在调用方事务内检查归属项目。
///
/// 状态判定走 `project_repo::get_project` 的领域枚举（与 [`set_task_project`] 同一条
/// 读路径）：`done` / `archived` 的对外文案一字不变；库里出现**无法识别**的取值时，
/// 由 `project_repo` 按列报 `UnknownEnumValue`，不会再被这里说成「已完成」。
pub fn require_active_project(conn: &Connection, task_id: &str) -> Result<(), AppError> {
    let task = get_task(conn, task_id)?.ok_or_else(|| AppError::Domain {
        detail: "任务不存在。".into(),
    })?;
    if let Some(project_id) = task.project_id {
        let project = crate::storage::project_repo::get_project(conn, &project_id)?
            // 项目行不存在只可能来自数据被绕过外键改坏（`task.project_id` 有 FK）。
            // 保持原先 `query_row` 未加 `optional()` 时的失败类别：存储层失败，
            // 不在这里新造一句领域文案。
            .ok_or_else(|| map_sqlite(rusqlite::Error::QueryReturnedNoRows))?;
        match project.status {
            ProjectStatus::Active => {}
            ProjectStatus::Archived => {
                return Err(AppError::Domain {
                    detail: "项目已归档，不能开始或继续计时。".into(),
                })
            }
            ProjectStatus::Done => {
                return Err(AppError::Domain {
                    detail: "项目已完成，不能开始或继续计时。".into(),
                })
            }
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 任务归属（P4 Task 2：F-002 的「可选项目」）
// ─────────────────────────────────────────────────────────────────────────────

/// 给任务指定项目，或解除关联。`project_id` 为 `None` 就是解除。
///
/// # V0.1 的边界（写清以免被读成越界）
///
/// **只改 `task.project_id`**（连同 `updated_at`、`row_version` 与一条 `task_change`）。
/// 不创建项目、不迁移任何属性、**不改变任务状态**——规格里「不做 Inbox 转项目及属性
/// 迁移」指的是把任务转成独立 Project 实体并搬运属性，那是另一件事。
///
/// # 为什么只接受理清阶段
///
/// 只允许**没有运行会话**的 `Inbox`/`Clarifying`/`Ready`。任务一旦开始，归属就是
/// 正在被记录的事实的一部分，改它要经 P3 的状态联动，不能从这个最小入口绕过去。
///
/// # 幂等
///
/// 目标归属与现状相同 ⇒ [`WriteOutcome::Unchanged`]，**不写审计、不加版本**。
/// 版本守卫排在幂等判断之前：请求方手上的版本旧了就得刷新，不能报「没有变化」。
pub fn set_task_project(
    tx: &Transaction<'_>,
    task_id: &str,
    expected_version: i64,
    project_id: Option<&str>,
    now: i64,
) -> Result<WriteOutcome<TaskRow>, AppError> {
    // 「找不到」用 `UnknownTask`，不是 `EmptyText`：空文本说的是「给的值是空的」，
    // 与「这条记录不在」是两回事（用户看到的文案也不同）。
    let before = get_task(tx, task_id)?.ok_or(DomainError::UnknownTask)?;
    guard_row_version(before.row_version, expected_version)?;

    if !matches!(
        before.status,
        TaskStatus::Inbox | TaskStatus::Clarifying | TaskStatus::Ready
    ) {
        return Err(DomainError::TaskNotInClarifying {
            status: before.status.as_str(),
        }
        .into());
    }
    if has_running_session(tx, task_id)? {
        return Err(DomainError::TaskHasRunningSession.into());
    }

    if let Some(pid) = project_id {
        // 与 `create_task` 的归档检查同一口径：**在事务内**判定，不只依赖 UI 过滤。
        // 这里用领域变体而不是手写文案，前端/测试能按 detail 分辨「不存在」与「已归档」。
        let project = crate::storage::project_repo::get_project(tx, pid)?
            .ok_or(DomainError::UnknownProject)?;
        match project.status {
            ProjectStatus::Active => {}
            ProjectStatus::Archived => return Err(DomainError::ProjectArchived.into()),
            // `done` 在 V0.1 写不出来；读到它说明库来自更新的版本，不去动它。
            ProjectStatus::Done => {
                return Err(DomainError::NotInThisVersion {
                    what: "把任务关联到已完成的项目",
                }
                .into())
            }
        }
    }

    if before.project_id.as_deref() == project_id {
        return Ok(WriteOutcome::Unchanged(before));
    }

    let n = tx
        .execute(
            "UPDATE task SET project_id = ?1, row_version = row_version + 1, updated_at = ?2
             WHERE id = ?3 AND row_version = ?4",
            rusqlite::params![project_id, now, task_id, expected_version],
        )
        .map_err(map_sqlite)?;
    if n == 0 {
        return Err(AppError::VersionConflict {
            expected: expected_version,
            actual: -1,
        });
    }

    record_change(
        tx,
        task_id,
        &serde_json::json!({ "project_id": before.project_id }).to_string(),
        &serde_json::json!({ "project_id": project_id }).to_string(),
        now,
    )?;

    get_task(tx, task_id)?
        .ok_or_else(|| AppError::Storage {
            detail: "task vanished after update".into(),
        })
        .map(WriteOutcome::Changed)
}

/// 这个任务有没有正在运行的会话。
///
/// 只看 `state='running'`：暂停/已结束的会话属于历史，不阻止后续的归属整理。
/// （paused/finished 的任务本来也不在允许的状态集合里，这道守卫守的是
/// 「状态与事实对不上」的数据。）
///
/// `pub(crate)`：理清入口（`services::catalog::clarify_ready`）要在**同一个事务**里
/// 问同一个问题，判据只留这一处，别处不再写一份「正在计时」的 SQL。
pub(crate) fn has_running_session(conn: &Connection, task_id: &str) -> Result<bool, AppError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM work_session WHERE task_id = ?1 AND state = 'running')",
        [task_id],
        |r| r.get(0),
    )
    .map_err(map_sqlite)
}
