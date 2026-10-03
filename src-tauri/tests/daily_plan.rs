//! P4 Task 4：今日选择列表（F-010 的「今日选择」半边）。
//!
//! 断言口径按总纲 §5 第 8 条：
//! - **被拒的命令**（未知任务、坏日期、坏时区、旧 epoch）：`revision` 与调用前相等、
//!   `daily_plan` / `task` 的行数与字段值逐字不变、审计（`task_change`）无新增；
//! - **幂等**（重复加入 / 重复移除）：第二次调用后 `revision` 不变，且返回值明确
//!   表示「集合没有变化」；
//! - **纯读**：什么都不写，返回的 `data_epoch` / `revision` 与库里的一致；
//! - **错误**：断言 `code()` 与**可辨的中文理由**（`detail()`），**不在 `message()`
//!   上找中文**——它的模板 `操作不被允许：{detail}` 自带中文，对 `DOMAIN_ERROR`
//!   恒真（Task 1 踩过的坑）。
//!
//! 快照一律**直连 SQL**（[`Fixture::task_snapshot`] / [`Fixture::plan_snapshot`]）：
//! 不拿被测的 `plan_for` 当 oracle，否则「计划行没变」这条断言会在它静默返回空集/
//! 错集时退化成恒真（Task 5 fix round 2 第 2 条）。
//!
//! 建库样板照 `tests/transaction_boundary.rs::bootstrap` 与 `tests/task_filters.rs`。

use worktrace_lib::domain::localdate::LocalDate;
use worktrace_lib::domain::task::TaskStatus;
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::services::daily_plan::{self, DailyPlanChange, DailyPlanQuery, DailyPlanView};
use worktrace_lib::storage::daily_plan_repo;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::task_repo::{self, TaskRow};
use worktrace_lib::storage::WriteOutcome;

/// 夹具里用到的日期与时区（时区一律是 `normalize_timezone` 的产物，即存储键）。
const DAY1: &str = "2026-10-03";
const DAY2: &str = "2026-10-04";
const UTC: &str = "UTC";
const SHANGHAI: &str = "Asia/Shanghai";
/// tzdb 里的两个名字，**不折叠**（R-T4-b）：`US/Eastern` 不是 `America/New_York` 的别名写法。
const EASTERN: &str = "US/Eastern";
const NEW_YORK: &str = "America/New_York";

/// 调用方给的墙钟毫秒（与 `ClockSample::wall_ms`、库里的 INTEGER 同单位）。
const NOW: i64 = 5_000;

// ─────────────────────────────────────────────────────────────────────────────
// 夹具
// ─────────────────────────────────────────────────────────────────────────────

struct Fixture {
    _dir: tempfile::TempDir,
    db: Db,
    /// 建库时定下的库身份，用来构造合法的请求。
    epoch: String,
}

/// 临时文件库 + 迁移 + 一个 run + 四个任务。
///
/// 任务的创建时刻**刻意有重复**，且有一个 id 排在前面、创建却更晚：
/// `t1` / `t2` 同为 1000（稳定排序的第二关键字 `id` 要靠它们证明），
/// `t3` = 2000，`a9` = 3000（只按 `id` 排序会把它排到最前面 ⇒ 排序用例会红）。
fn bootstrap() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    tx.execute(
        "INSERT INTO application_run(id, started_at) VALUES('run-1', 1000)",
        [],
    )
    .unwrap();
    // 走仓储建任务：审计行数与后面的断言口径一致（同 `tests/task_filters.rs`）。
    task_repo::create_task(&tx, "t1", "任务一", None, 1000).unwrap();
    task_repo::create_task(&tx, "t2", "任务二", None, 1000).unwrap();
    task_repo::create_task(&tx, "t3", "任务三", None, 2000).unwrap();
    task_repo::create_task(&tx, "a9", "任务四", None, 3000).unwrap();
    tx.commit().unwrap();

    Fixture {
        _dir: dir,
        db,
        epoch: meta.data_epoch,
    }
}

/// 任务行的**直连 SQL** 快照（`created_at, id` 顺序）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskSnapshot {
    id: String,
    status: String,
    title: String,
    row_version: i64,
    created_at: i64,
    updated_at: i64,
}

/// 计划行的**直连 SQL** 快照。计划表只有三个键列，没有时间戳，也没有版本列。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanSnapshot {
    task_id: String,
    local_date: String,
    timezone: String,
}

/// 被拒 / 只读 / 幂等调用的「零变化」基线。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Baseline {
    revision: i64,
    tasks: Vec<TaskSnapshot>,
    /// 独立的行数断言：即使快照本身出了问题，「行数不变」这条也还站着。
    task_rows: i64,
    plan: Vec<PlanSnapshot>,
    plan_rows: i64,
    task_changes: i64,
}

impl Fixture {
    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }

    fn count(&self, table: &str) -> i64 {
        self.db
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn task(&self, id: &str) -> Option<TaskRow> {
        task_repo::get_task(self.db.connection(), id).unwrap()
    }

    fn task_snapshot(&self) -> Vec<TaskSnapshot> {
        let mut stmt = self
            .db
            .connection()
            .prepare(
                "SELECT id, status, title, row_version, created_at, updated_at
                 FROM task ORDER BY created_at, id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(TaskSnapshot {
                    id: r.get(0)?,
                    status: r.get(1)?,
                    title: r.get(2)?,
                    row_version: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            })
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    fn plan_snapshot(&self) -> Vec<PlanSnapshot> {
        let mut stmt = self
            .db
            .connection()
            .prepare(
                "SELECT task_id, local_date, timezone FROM daily_plan
                 ORDER BY task_id, local_date, timezone",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(PlanSnapshot {
                    task_id: r.get(0)?,
                    local_date: r.get(1)?,
                    timezone: r.get(2)?,
                })
            })
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    /// 最近一条 `task_change` 的 `(before_json, after_json)`，原样返回。
    fn last_change(&self) -> (String, String) {
        self.db
            .connection()
            .query_row(
                "SELECT before_json, after_json FROM task_change ORDER BY rowid DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 调用与断言助手
// ─────────────────────────────────────────────────────────────────────────────

/// 加入计划（服务入口，epoch-only 信封）。
fn add(
    f: &mut Fixture,
    task_id: &str,
    date: &str,
    timezone: &str,
) -> Result<WriteOutcome<DailyPlanChange>, AppError> {
    let env = WriteEnvelope::for_create(f.epoch.clone());
    daily_plan::add_to_plan(&mut f.db, env, task_id, date, timezone, NOW)
}

/// 移出计划（服务入口，epoch-only 信封）。
fn remove(
    f: &mut Fixture,
    task_id: &str,
    date: &str,
    timezone: &str,
) -> Result<WriteOutcome<DailyPlanChange>, AppError> {
    let env = WriteEnvelope::for_create(f.epoch.clone());
    daily_plan::remove_from_plan(&mut f.db, env, task_id, date, timezone, NOW)
}

/// 读某一天（某个时区）的计划（服务入口）。
fn read(f: &Fixture, date: &str, timezone: &str) -> Result<DailyPlanView, AppError> {
    daily_plan::plan_for(
        &f.db,
        DailyPlanQuery {
            date: date.to_string(),
            timezone: timezone.to_string(),
            expected_data_epoch: f.epoch.clone(),
        },
    )
}

fn ids(tasks: &[TaskRow]) -> Vec<&str> {
    tasks.iter().map(|t| t.id.as_str()).collect()
}

fn parse_day(raw: &str) -> LocalDate {
    LocalDate::parse(raw).unwrap()
}

fn baseline(f: &Fixture) -> Baseline {
    let tasks = f.task_snapshot();
    let task_rows = f.count("task");
    // 快照必须真的读到了行：否则「字段值不变」会退化成「空对空」。
    assert_eq!(
        tasks.len() as i64,
        task_rows,
        "快照条数必须与 task 表行数一致（快照坏了要当场发现，而不是让断言恒真）"
    );
    assert!(task_rows > 0, "基线快照不能是空的");
    let plan = f.plan_snapshot();
    let plan_rows = f.count("daily_plan");
    assert_eq!(plan.len() as i64, plan_rows, "计划快照条数必须与表行数一致");
    Baseline {
        revision: f.revision(),
        tasks,
        task_rows,
        plan,
        plan_rows,
        task_changes: f.count("task_change"),
    }
}

/// ① `revision` 不变 ② `task` 行数与字段值逐字不变 ③ `daily_plan` 行数与字段值逐字不变
/// ④ 审计无新增。
fn assert_unchanged(f: &Fixture, before: &Baseline) {
    assert_eq!(
        f.revision(),
        before.revision,
        "被拒 / 只读 / 幂等的调用不得改动 revision"
    );
    assert_eq!(f.count("task"), before.task_rows, "任务表行数不得变化");
    assert_eq!(
        f.task_snapshot(),
        before.tasks,
        "任务行不得有任何变化（含状态、版本、标题、时间）"
    );
    assert_eq!(
        f.count("daily_plan"),
        before.plan_rows,
        "计划表行数不得变化"
    );
    assert_eq!(
        f.plan_snapshot(),
        before.plan,
        "计划行不得有任何变化（含 task_id、日期、时区）"
    );
    assert_eq!(
        f.count("task_change"),
        before.task_changes,
        "审计表不得新增"
    );
}

/// 结果必须是「真的写了」。
fn expect_changed<T>(outcome: WriteOutcome<T>) -> T {
    match outcome {
        WriteOutcome::Changed(value) => value,
        WriteOutcome::Unchanged(_) => panic!("这次调用本该写库，却返回了「没有变化」"),
    }
}

/// 结果必须是「集合没有变化」（幂等重复）。
fn expect_unchanged<T>(outcome: WriteOutcome<T>) -> T {
    match outcome {
        WriteOutcome::Unchanged(value) => value,
        WriteOutcome::Changed(_) => panic!("幂等重复不得返回「写了」"),
    }
}

/// 领域拒绝：`code()` 对，且 `detail()` 是**具体的中文理由**（含全部期望片段）。
fn assert_domain_error(err: AppError, expected_detail_has: &[&str]) {
    assert_eq!(err.code(), "DOMAIN_ERROR", "应当是领域拒绝");
    let detail = err.detail().unwrap_or_default().to_string();
    assert!(
        detail
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "领域拒绝的 detail 必须是中文（空串、英文、SQL 片段都不许）：{detail:?}"
    );
    for needle in expected_detail_has {
        assert!(
            detail.contains(needle),
            "detail 应当说清原因：期望含 {needle:?}，实际 {detail:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 读：日期与时区隔离
// ─────────────────────────────────────────────────────────────────────────────

/// 计划按 `(task_id, local_date, timezone)` 隔离：别的日期、别的时区的行不会串进来。
#[test]
fn a_plan_read_lists_only_the_requested_day_and_timezone() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();
    add(&mut f, "t2", DAY2, UTC).unwrap();
    add(&mut f, "t3", DAY1, SHANGHAI).unwrap();

    assert_eq!(ids(&read(&f, DAY1, UTC).unwrap().tasks), ["t1"]);
    assert_eq!(ids(&read(&f, DAY2, UTC).unwrap().tasks), ["t2"]);
    assert_eq!(ids(&read(&f, DAY1, SHANGHAI).unwrap().tasks), ["t3"]);
    assert!(
        read(&f, DAY2, SHANGHAI).unwrap().tasks.is_empty(),
        "没有任何计划的一天读到空表"
    );
}

/// 跨日：给某一天加的任务不会出现在第二天；移除同样只作用于那一天（F-010）。
#[test]
fn a_task_selected_for_one_day_does_not_appear_on_the_next_day() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();
    add(&mut f, "t1", DAY2, UTC).unwrap();

    expect_changed(remove(&mut f, "t1", DAY1, UTC).unwrap());

    assert!(
        read(&f, DAY1, UTC).unwrap().tasks.is_empty(),
        "移除只作用于那一天"
    );
    assert_eq!(
        ids(&read(&f, DAY2, UTC).unwrap().tasks),
        ["t1"],
        "另一天的行不受影响"
    );
}

/// R-T4-b：`US/Eastern` 与 `America/New_York` 在 tzdb 里是**两个名字**，不折叠。
/// 所以「同一任务、同一天、不同时区键 ⇒ 两行」是**预期行为**——计划行记的是
/// 「当时那个时区的那一天」。这里把事实写成用例；实现里没有任何合并或迁移。
#[test]
fn the_same_task_on_the_same_day_under_another_timezone_key_is_a_separate_row() {
    // 前提：两个名字都是合法且**互不相同**的存储键（大小写与别名都不收敛）。
    assert_eq!(
        daily_plan::normalize_timezone(EASTERN).unwrap(),
        EASTERN,
        "时区键原样落库"
    );
    assert_eq!(daily_plan::normalize_timezone(NEW_YORK).unwrap(), NEW_YORK);

    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, EASTERN).unwrap();
    add(&mut f, "t1", DAY1, NEW_YORK).unwrap();

    let rows = f.plan_snapshot();
    assert_eq!(rows.len(), 2, "两个时区键就是两行（同一任务、同一天）");
    let zones: Vec<&str> = rows.iter().map(|r| r.timezone.as_str()).collect();
    assert_eq!(zones, [NEW_YORK, EASTERN], "按 timezone 排序的两行");
    assert_eq!(ids(&read(&f, DAY1, EASTERN).unwrap().tasks), ["t1"]);
    assert_eq!(ids(&read(&f, DAY1, NEW_YORK).unwrap().tasks), ["t1"]);
}

/// 稳定排序：`task.created_at, task.id`（F-010 的列表顺序）。
///
/// 加入顺序刻意打乱：顺序由查询决定，不由写入顺序决定。
#[test]
fn the_plan_is_ordered_by_task_creation_time_then_id() {
    let mut f = bootstrap();
    add(&mut f, "a9", DAY1, UTC).unwrap();
    add(&mut f, "t3", DAY1, UTC).unwrap();
    add(&mut f, "t2", DAY1, UTC).unwrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();

    assert_eq!(
        ids(&read(&f, DAY1, UTC).unwrap().tasks),
        ["t1", "t2", "t3", "a9"],
        "同创建时刻按 id 决出先后；创建晚但 id 靠前的排后面"
    );
}

/// 仓储的读可以直接接 `&Connection`（R-T4-c），排序与服务的读逐字一致。
#[test]
fn the_repository_read_takes_a_connection_and_keeps_the_order() {
    let mut f = bootstrap();
    add(&mut f, "t3", DAY1, UTC).unwrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();

    let rows = daily_plan_repo::plan_for(f.db.connection(), &parse_day(DAY1), UTC).unwrap();

    assert_eq!(ids(&rows), ["t1", "t3"]);
}

/// 读写同一口径（R-T4-h）：`utc` 与 `UTC` 是同一个存储键，写进去、读出来都是同一行。
#[test]
fn the_read_and_the_write_agree_on_the_normalized_timezone_key() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, "utc").unwrap();

    assert_eq!(
        f.plan_snapshot()[0].timezone,
        UTC,
        "落库的是规范化之后的存储键"
    );
    assert_eq!(ids(&read(&f, DAY1, "utc").unwrap().tasks), ["t1"]);
}

/// 纯读：不改 `revision`、不写审计、不动任何行；返回的 `data_epoch` / `revision`
/// 与库里一致（四项出自同一个读事务，与 T5 的筛选查询同一形状）。
#[test]
fn reading_the_plan_changes_nothing_and_reports_the_library_identity() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();
    let before = baseline(&f);
    let meta = require_meta(f.db.connection()).unwrap();

    let view = read(&f, DAY1, UTC).unwrap();

    assert_eq!(view.data_epoch, meta.data_epoch);
    assert_eq!(view.revision, meta.revision);
    assert_unchanged(&f, &before);
}

/// 读也要校验库身份：旧 epoch 的读请求被拒，且什么都没写（F-019）。
#[test]
fn a_stale_epoch_is_rejected_by_the_read() {
    let f = bootstrap();
    let before = baseline(&f);
    let stale = "epoch-from-another-db".to_string();

    let err = daily_plan::plan_for(
        &f.db,
        DailyPlanQuery {
            date: DAY1.to_string(),
            timezone: UTC.to_string(),
            expected_data_epoch: stale.clone(),
        },
    )
    .unwrap_err();

    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
    assert!(
        !err.message().contains(&stale),
        "message 不得带 epoch 字面量：{}",
        err.message()
    );
    assert_unchanged(&f, &before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 写：加入 / 移除
// ─────────────────────────────────────────────────────────────────────────────

/// 一次成功的加入：计划多一行、审计多一条、`revision` 恰好 +1，
/// 并把**这一天当前的计划**交回（与 T3 的打标返回当前标签集合同一形状）。
#[test]
fn adding_a_task_writes_one_row_one_audit_and_one_revision() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let change = expect_changed(add(&mut f, "t1", DAY1, UTC).unwrap());

    assert_eq!(ids(&change.tasks), ["t1"], "返回值里带着这一天当前的计划");
    assert_eq!(f.revision(), before.revision + 1, "一次业务写恰好 +1");
    assert_eq!(f.count("daily_plan"), before.plan_rows + 1);
    assert_eq!(
        f.count("task_change"),
        before.task_changes + 1,
        "审计与写入同一事务"
    );
}

/// 审计记的是**这个任务计划集合**的变化前后（与 T3 的标签集合同一形状），
/// 记完能脱离当时的库状态被读懂。
#[test]
fn the_audit_records_the_plan_set_before_and_after() {
    let mut f = bootstrap();
    expect_changed(add(&mut f, "t1", DAY1, UTC).unwrap());
    expect_changed(add(&mut f, "t1", DAY2, SHANGHAI).unwrap());

    let (before_json, after_json) = f.last_change();
    assert_eq!(
        before_json,
        r#"{"daily_plan":[{"local_date":"2026-10-03","timezone":"UTC"}]}"#
    );
    assert_eq!(
        after_json,
        r#"{"daily_plan":[{"local_date":"2026-10-03","timezone":"UTC"},{"local_date":"2026-10-04","timezone":"Asia/Shanghai"}]}"#
    );
}

/// 幂等（总纲 §5 第 8 条②）：重复加入不写审计、不加 revision、不动任何行，
/// 返回值明确表示「集合没有变化」。
#[test]
fn adding_the_same_task_twice_is_idempotent() {
    let mut f = bootstrap();
    expect_changed(add(&mut f, "t1", DAY1, UTC).unwrap());
    let after_first = baseline(&f);

    let second = expect_unchanged(add(&mut f, "t1", DAY1, UTC).unwrap());

    assert_eq!(f.revision(), after_first.revision, "第二次不得加 revision");
    assert_unchanged(&f, &after_first);
    assert_eq!(ids(&second.tasks), ["t1"], "返回值仍是这一天当前的计划");
    assert_eq!(second.revision, after_first.revision);
}

/// 幂等的另一半：移除一个本来就不在计划里的任务 ⇒ 不写、不加 revision。
#[test]
fn removing_a_task_that_is_not_in_the_plan_is_idempotent() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let outcome = expect_unchanged(remove(&mut f, "t1", DAY1, UTC).unwrap());

    assert_eq!(outcome.revision, before.revision);
    assert!(outcome.tasks.is_empty());
    assert_unchanged(&f, &before);
}

/// 移除只删 `(task_id, local_date, timezone)` 那一行。
#[test]
fn removing_deletes_only_that_task_on_that_day_in_that_timezone() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();
    add(&mut f, "t1", DAY2, UTC).unwrap();
    add(&mut f, "t1", DAY1, SHANGHAI).unwrap();
    add(&mut f, "t2", DAY1, UTC).unwrap();

    expect_changed(remove(&mut f, "t1", DAY1, UTC).unwrap());

    assert_eq!(ids(&read(&f, DAY1, UTC).unwrap().tasks), ["t2"]);
    assert_eq!(ids(&read(&f, DAY2, UTC).unwrap().tasks), ["t1"]);
    assert_eq!(ids(&read(&f, DAY1, SHANGHAI).unwrap().tasks), ["t1"]);
}

// ─────────────────────────────────────────────────────────────────────────────
// R-T4-e：今日计划只是人工选择
// ─────────────────────────────────────────────────────────────────────────────

/// 加入计划不改任务的任何字段：不设 `Scheduled`、不动 `row_version` / `updated_at`、
/// 不启动计时。（`time_block` 属 V0.2，V0.1 的 schema 里根本没有这张表。）
#[test]
fn adding_a_task_to_the_plan_does_not_touch_the_task_row() {
    let mut f = bootstrap();
    let before = baseline(&f);

    expect_changed(add(&mut f, "t1", DAY1, UTC).unwrap());

    assert_eq!(
        f.task_snapshot(),
        before.tasks,
        "任务行逐字不变（含状态、版本、更新时间）"
    );
    let t1 = f.task("t1").unwrap();
    assert_eq!(
        t1.status,
        TaskStatus::Inbox,
        "计划不推进状态：不自动设 Scheduled"
    );
    assert_eq!(f.count("work_session"), 0, "计划不启动计时");
}

/// 任务自身的变化**不搬移**已有的计划行：状态改成 `Done`、`updated_at` 前进之后，
/// 那一行还在原来的 `(日期, 时区)` 上，而且完成项仍留在当天的列表里（状态由任务行
/// 提供，不由计划表清理）。
#[test]
fn a_task_state_change_does_not_move_its_plan_rows() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();
    let plan_before = f.plan_snapshot();

    // 直接改任务行：走 P3 的状态联动要好几步跃迁，这里只布置「任务变了」这一事实。
    f.db.connection()
        .execute(
            "UPDATE task SET status = 'Done', row_version = row_version + 1, updated_at = 9000
             WHERE id = 't1'",
            [],
        )
        .unwrap();

    assert_eq!(f.plan_snapshot(), plan_before, "计划行一个字段都不许动");
    let listed = read(&f, DAY1, UTC).unwrap().tasks;
    assert_eq!(ids(&listed), ["t1"], "完成项仍留在当天的列表里");
    assert_eq!(
        listed[0].status,
        TaskStatus::Done,
        "状态由任务行提供，计划表不清理已完成项"
    );
}

/// R-T4-b 的另一半：换时区**不搬移旧行**。旧键下的行还在，新键下另起一行。
#[test]
fn changing_the_timezone_keeps_the_rows_written_under_the_old_key() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();
    let old_row = f.plan_snapshot();

    add(&mut f, "t1", DAY1, SHANGHAI).unwrap();

    let rows = f.plan_snapshot();
    assert_eq!(rows.len(), 2, "旧行保留，新键另起一行（不做合并/迁移）");
    assert!(
        rows.contains(&old_row[0]),
        "UTC 那一行逐字未变：{old_row:?} vs {rows:?}"
    );
    assert_eq!(ids(&read(&f, DAY1, UTC).unwrap().tasks), ["t1"]);
    assert_eq!(ids(&read(&f, DAY1, SHANGHAI).unwrap().tasks), ["t1"]);
}

// ─────────────────────────────────────────────────────────────────────────────
// 被拒：零变化
// ─────────────────────────────────────────────────────────────────────────────

/// 任务不存在 ⇒ 领域拒绝（不是空标题、不是存储错误），且零变化。
#[test]
fn an_unknown_task_is_rejected_without_writing_anything() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();
    let before = baseline(&f);

    assert_domain_error(
        add(&mut f, "nope", DAY1, UTC).unwrap_err(),
        &["找不到这个任务"],
    );
    assert_unchanged(&f, &before);

    assert_domain_error(
        remove(&mut f, "nope", DAY1, UTC).unwrap_err(),
        &["找不到这个任务"],
    );
    assert_unchanged(&f, &before);
}

/// 日期走唯一入口（R-T4-h）：形状不对的（`2024-2-29`）与不存在的日历日
/// （`2023-02-30`——GLOB 只管形状，管不了日历）都在开事务之前被拒。
#[test]
fn a_malformed_or_nonexistent_date_is_rejected_without_writing_anything() {
    let mut f = bootstrap();
    let before = baseline(&f);

    for bad in [
        "2024-2-29",
        "2023-02-30",
        "2026-13-01",
        "2026-10-03T00:00:00",
    ] {
        let err = add(&mut f, "t1", bad, UTC).unwrap_err();
        assert_domain_error(err, &["本地日期"]);
        let err = remove(&mut f, "t1", bad, UTC).unwrap_err();
        assert_domain_error(err, &["本地日期"]);
    }
    assert_domain_error(
        add(&mut f, "t1", "   ", UTC).unwrap_err(),
        &["本地日期", "不能为空"],
    );

    assert_unchanged(&f, &before);
}

/// 时区走唯一入口：不存在的时区与「没有 IANA 名称」的名字都被拒；空白另有理由。
#[test]
fn an_unknown_timezone_is_rejected_without_writing_anything() {
    let mut f = bootstrap();
    let before = baseline(&f);

    for bad in ["Mars/Olympus", "GMT+8", "+08:00", "Etc/Unknown"] {
        assert_domain_error(add(&mut f, "t1", DAY1, bad).unwrap_err(), &["时区"]);
        assert_domain_error(remove(&mut f, "t1", DAY1, bad).unwrap_err(), &["时区"]);
    }
    assert_domain_error(
        add(&mut f, "t1", DAY1, "  ").unwrap_err(),
        &["时区", "不能为空"],
    );

    assert_unchanged(&f, &before);
}

/// 旧 epoch：`DATA_EPOCH_MISMATCH`、零变化，且 `message()` 不带 epoch 字面量与 SQL
/// 片段（总纲 §5 第 8 条）。
#[test]
fn a_stale_epoch_is_rejected_before_anything_is_written() {
    let mut f = bootstrap();
    add(&mut f, "t1", DAY1, UTC).unwrap();
    let before = baseline(&f);
    let stale = "epoch-from-another-db".to_string();

    let env = WriteEnvelope::for_create(stale.clone());
    let err = daily_plan::add_to_plan(&mut f.db, env, "t2", DAY1, UTC, NOW).unwrap_err();

    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
    assert!(
        !err.message().contains(&stale),
        "message 不得带 epoch 字面量：{}",
        err.message()
    );
    assert!(
        !err.message().contains("INSERT") && !err.message().contains("SELECT"),
        "message 不得带 SQL 片段：{}",
        err.message()
    );
    assert_unchanged(&f, &before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 事务边界
// ─────────────────────────────────────────────────────────────────────────────

/// R-T4-c：仓储只写**调用方的事务**——调用方回滚，计划行与审计都不得留下
/// （仓储自己不 `begin`、不 `commit`）。
#[test]
fn a_rolled_back_callers_transaction_leaves_nothing_behind() {
    let mut f = bootstrap();
    let before = baseline(&f);
    let day = parse_day(DAY1);

    let tx = f.db.connection_mut().unchecked_transaction().unwrap();
    let outcome = daily_plan_repo::add_to_plan(&tx, "t1", &day, UTC, NOW).unwrap();
    assert!(
        matches!(outcome, WriteOutcome::Changed(())),
        "写原语要报告「真的写了」"
    );
    drop(tx); // 不 commit ⇒ 整段回滚

    assert_unchanged(&f, &before);
    assert!(f.plan_snapshot().is_empty(), "回滚之后不许留下半行");
}
