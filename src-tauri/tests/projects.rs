//! P4 Task 2：项目仓储与服务、任务归属（F-002 的「可选项目」）。
//!
//! 断言口径按总纲 §5 第 8 条：
//! - **被拒的用户命令**：`revision` 不变、目标表行数不变、审计无新增、涉及的既有记录
//!   字段值与调用前逐字相等（`baseline_of` / `assert_unchanged` 一次断言这四件事）；
//! - **幂等重复**（重命名同名、归档已归档、设成同值）：第二次调用 `revision` 不变，
//!   且返回值明确表达「没有变化」（`WriteOutcome::Unchanged`）；
//! - **错误**：断言 `code()` 字符串；文案口径用 `assert_domain_error`——CJK 判定落在
//!   `detail()` 上。**不在 `message()` 上找中文**：它的模板 `操作不被允许：{detail}`
//!   自带中文，对 `DOMAIN_ERROR` 恒真（Task 1 踩过的坑）。
//!
//! 建库样板照 `tests/transaction_boundary.rs::bootstrap`。

use std::sync::{Arc, Mutex};

use worktrace_lib::domain::project::ProjectStatus;
use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::catalog::{self, ProjectTarget};
use worktrace_lib::services::timer::coordinator::{Coordinator, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{bump_revision, init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::project_repo::{self, ProjectRow};
use worktrace_lib::storage::task_repo::{self, TaskRow};
use worktrace_lib::storage::WriteOutcome;

// ─────────────────────────────────────────────────────────────────────────────
// 夹具
// ─────────────────────────────────────────────────────────────────────────────

struct Fixture {
    _dir: tempfile::TempDir,
    db: Db,
    /// 建库时定下的库身份，用来构造合法的写请求。
    epoch: String,
}

/// 新建命令的信封。**写成自由函数而不是方法**：方法要借整个 `Fixture`，
/// 与 `&mut f.db` 撞借用（服务入口同时要 `&mut Db` 和一份信封）。
fn create_env(epoch: &str) -> WriteEnvelope {
    WriteEnvelope::for_create(epoch.to_string())
}

/// 更新命令的信封：带请求方看到的记录版本。
fn update_env(epoch: &str, expected_row_version: i64) -> WriteEnvelope {
    WriteEnvelope::for_update(epoch.to_string(), expected_row_version)
}

/// 临时文件库 + 迁移 + 一个 run + 一个 active 项目 `p1` + 项目内任务 `t1`（Inbox）。
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
    tx.execute(
        "INSERT INTO project(id,name,description,row_version,status,created_at,updated_at)
         VALUES('p1','项目一',NULL,0,'active',1000,1000)",
        [],
    )
    .unwrap();
    // 任务走仓储建：审计行数与后面的断言口径才一致（同 bootstrap 的理由）。
    task_repo::create_task(&tx, "t1", "任务一", Some("p1"), 1000).unwrap();
    tx.commit().unwrap();

    Fixture {
        _dir: dir,
        db,
        epoch: meta.data_epoch,
    }
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

    fn project(&self, id: &str) -> Option<ProjectRow> {
        project_repo::get_project(self.db.connection(), id).unwrap()
    }

    fn project_status(&self, id: &str) -> ProjectStatus {
        self.project(id).expect("project row exists").status
    }

    fn task(&self, id: &str) -> Option<TaskRow> {
        task_repo::get_task(self.db.connection(), id).unwrap()
    }

    /// 全部项目行的快照，**直连 SQL**（`created_at, id` 顺序，与仓储的稳定排序同一口径）。
    fn project_snapshot(&self) -> Vec<ProjectSnapshot> {
        let mut stmt = self
            .db
            .connection()
            .prepare(
                "SELECT id, name, description, row_version, status, created_at, updated_at
                 FROM project ORDER BY created_at, id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(ProjectSnapshot {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    description: r.get(2)?,
                    row_version: r.get(3)?,
                    status: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            })
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    /// 某个任务行的快照，**直连 SQL**（`None` = 库里没有这一行）。
    fn task_snapshot(&self, id: &str) -> Option<TaskSnapshot> {
        use rusqlite::OptionalExtension;
        self.db
            .connection()
            .query_row(
                "SELECT id, project_id, title, status, quality, row_version, created_at, updated_at
                 FROM task WHERE id = ?1",
                [id],
                |r| {
                    Ok(TaskSnapshot {
                        id: r.get(0)?,
                        project_id: r.get(1)?,
                        title: r.get(2)?,
                        status: r.get(3)?,
                        quality: r.get(4)?,
                        row_version: r.get(5)?,
                        created_at: r.get(6)?,
                        updated_at: r.get(7)?,
                    })
                },
            )
            .optional()
            .unwrap()
    }

    fn tx(&mut self) -> rusqlite::Transaction<'_> {
        self.db.connection_mut().unchecked_transaction().unwrap()
    }

    /// 直接插一个项目。**只用于布置前置状态**（已归档、`done` 这类正常写路径造不出的行）。
    fn insert_project(&self, id: &str, name: &str, status: &str, at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO project(id,name,description,row_version,status,created_at,updated_at)
                 VALUES(?1,?2,NULL,0,?3,?4,?4)",
                rusqlite::params![id, name, status, at],
            )
            .unwrap();
    }

    /// 直接插一个任务。只用于布置「已经离开理清阶段」这类前置状态。
    fn insert_task(&self, id: &str, status: TaskStatus, project_id: Option<&str>, at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO task(id,project_id,title,status,row_version,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,0,?5,?5)",
                rusqlite::params![id, project_id, format!("任务{id}"), status.as_str(), at],
            )
            .unwrap();
    }

    /// 直接插一条正在运行的会话，用来构造「有运行会话」这一非法前提。
    fn insert_running_session(&self, session_id: &str, task_id: &str, at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
                 VALUES(?1,?2,'run-1','FOREGROUND','running','stopwatch',?3,0)",
                rusqlite::params![session_id, task_id, at],
            )
            .unwrap();
    }

    /// 最近一条 `task_change` 的 `(before_json, after_json)`。
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

/// `project` 一行的**直连 SQL** 快照。
///
/// 刻意不走 `project_repo::list_projects`：那是被测服务的下层，拿它当 oracle，
/// 「项目行字段值不变」会在它静默返回空集/少行时退化成恒真（终评 I2；写法照
/// `tests/task_filters.rs::task_snapshot` 与 `tests/daily_plan.rs::plan_snapshot`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectSnapshot {
    id: String,
    name: String,
    description: Option<String>,
    row_version: i64,
    status: String,
    created_at: i64,
    updated_at: i64,
}

/// `task` 一行的**直连 SQL** 快照（`status` / `quality` 保留库里的原值，不经仓储解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskSnapshot {
    id: String,
    project_id: Option<String>,
    title: String,
    status: String,
    quality: Option<String>,
    row_version: i64,
    created_at: i64,
    updated_at: i64,
}

/// 被拒 / 幂等命令的「零变化」基线：四件事一次抓齐（总纲 §5 第 8 条）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Baseline {
    revision: i64,
    projects: Vec<ProjectSnapshot>,
    /// 独立的行数断言：即使快照本身出了问题，「行数不变」这条也还站着。
    project_rows: i64,
    task: Option<TaskSnapshot>,
    task_changes: i64,
}

fn baseline_of(f: &Fixture, task_id: &str) -> Baseline {
    // 全部项目（含归档与 done）：行数与字段值一起钉住。
    let projects = f.project_snapshot();
    let project_rows = f.count("project");
    // 快照必须真的读到了行：否则「字段值不变」会退化成「空对空」（终评 I2）。
    assert_eq!(
        projects.len() as i64,
        project_rows,
        "项目快照条数必须与 project 表行数一致（快照坏了就当场发现，而不是让断言恒真）"
    );
    assert!(project_rows > 0, "基线快照不能是空的");
    let task = f.task_snapshot(task_id);
    assert!(
        task.is_some(),
        "基线里的目标任务 {task_id:?} 必须存在，否则任务行的「字段值不变」是空对空"
    );
    Baseline {
        revision: f.revision(),
        projects,
        project_rows,
        task,
        task_changes: f.count("task_change"),
    }
}

fn baseline(f: &Fixture) -> Baseline {
    baseline_of(f, "t1")
}

/// ① `revision` 不变 ② 项目行数与字段值一个不动 ③ 目标任务字段逐字不变 ④ 审计无新增。
fn assert_unchanged(f: &Fixture, task_id: &str, before: &Baseline) {
    assert_eq!(
        f.revision(),
        before.revision,
        "被拒/幂等的命令不得改动 revision"
    );
    assert_eq!(
        f.count("project"),
        before.project_rows,
        "项目表行数不得变化"
    );
    assert_eq!(
        f.project_snapshot(),
        before.projects,
        "项目行不得有任何变化（含行数、名字、状态、版本、时间）"
    );
    assert_eq!(
        f.task_snapshot(task_id),
        before.task,
        "任务行的字段值必须与调用前逐字相等（状态/版本/归属/时间）"
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

/// 结果必须是「没有变化」——幂等重复的返回值得能表达它（R-T2-e）。
fn expect_unchanged<T>(outcome: WriteOutcome<T>) -> T {
    match outcome {
        WriteOutcome::Unchanged(value) => value,
        WriteOutcome::Changed(_) => panic!("幂等重复不得返回「写了」"),
    }
}

/// 领域拒绝：`code()` 对，且 `detail()` 是**具体的中文理由**。
fn assert_domain_error(err: AppError, expected_detail_has: &str) {
    assert_eq!(err.code(), "DOMAIN_ERROR", "应当是领域拒绝");
    let detail = err.detail().unwrap_or_default().to_string();
    assert!(
        detail
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "领域拒绝的 detail 必须是中文（空串、英文、SQL 片段都不许）：{detail:?}"
    );
    assert!(
        detail.contains(expected_detail_has),
        "detail 应当说清原因：期望含 {expected_detail_has:?}，实际 {detail:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 创建项目
// ─────────────────────────────────────────────────────────────────────────────

/// 建项目：写入一条 active 行（名字已规范化），`revision` 恰好 +1，并出现在选择列表里。
#[test]
fn creating_a_project_writes_an_active_row_and_bumps_revision_once() {
    let mut f = bootstrap();
    let before = f.revision();

    let change = expect_changed(
        catalog::create_project(&mut f.db, create_env(&f.epoch), "  新项目  ", 2000).unwrap(),
    );

    assert_eq!(change.revision, before + 1, "一次业务写恰好加一次 revision");
    assert_eq!(change.project.name, "新项目", "落库的是规范化后的名字");
    assert_eq!(
        change.project.status,
        ProjectStatus::Active,
        "新建的项目是 active"
    );
    assert_eq!(change.project.row_version, 0, "新行版本从 0 起");
    assert_eq!(change.project.description, None, "V0.1 没有描述的写入口");
    assert_eq!(
        (change.project.created_at, change.project.updated_at),
        (2000, 2000)
    );
    assert_eq!(f.count("project"), 2);

    let selectable = catalog::list_selectable_projects(&f.db).unwrap();
    assert!(
        selectable.iter().any(|p| p.id == change.project.id),
        "新建的项目应当出现在新建任务的选择列表里"
    );
}

/// 空名字（或全空白）拒绝：`DOMAIN_ERROR`，一个项目行都不留、revision 不变。建与改名同规则。
#[test]
fn blank_project_names_are_refused_without_writing() {
    let mut f = bootstrap();

    for raw in ["", "   ", "\t"] {
        let before = baseline(&f);

        let err = catalog::create_project(&mut f.db, create_env(&f.epoch), raw, 2000).unwrap_err();
        assert_domain_error(err, "不能为空");
        assert_unchanged(&f, "t1", &before);

        let err = catalog::rename_project(&mut f.db, update_env(&f.epoch, 0), "p1", raw, 2000)
            .unwrap_err();
        assert_domain_error(err, "不能为空");
        assert_unchanged(&f, "t1", &before);
    }
}

/// 库身份对不上（旧 epoch）：`DATA_EPOCH_MISMATCH`，零变化，文案不带 epoch 字面量。
#[test]
fn a_stale_epoch_is_refused_before_anything_is_written() {
    let mut f = bootstrap();
    let stale = create_env("epoch-from-another-db");

    let before = baseline(&f);
    let err = catalog::create_project(&mut f.db, stale, "项目二", 2000).unwrap_err();

    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
    assert!(
        !err.message().contains("epoch-from-another-db"),
        "错误文案不得带库身份：{}",
        err.message()
    );
    assert_unchanged(&f, "t1", &before);
}

/// 项目重名是允许的：schema 没有唯一索引，规格（F-004）也没要求项目名唯一。
#[test]
fn duplicate_project_names_are_allowed() {
    let mut f = bootstrap();

    let first = expect_changed(
        catalog::create_project(&mut f.db, create_env(&f.epoch), "同名", 2000).unwrap(),
    );
    let second = expect_changed(
        catalog::create_project(&mut f.db, create_env(&f.epoch), "同名", 2100).unwrap(),
    );

    assert_ne!(
        first.project.id, second.project.id,
        "两次创建是两条独立的项目"
    );
    assert_eq!(second.revision, first.revision + 1);
    assert_eq!(f.count("project"), 3);
}

// ─────────────────────────────────────────────────────────────────────────────
// 重命名
// ─────────────────────────────────────────────────────────────────────────────

/// 重命名：写新名字，`row_version` / `updated_at` 前进，`created_at` 与状态不动，revision +1。
#[test]
fn renaming_a_project_writes_the_new_name_and_advances_the_version() {
    let mut f = bootstrap();
    let before = f.revision();

    let change = expect_changed(
        catalog::rename_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            "p1",
            "  改过的名字 ",
            3000,
        )
        .unwrap(),
    );

    assert_eq!(change.revision, before + 1);
    assert_eq!(change.project.name, "改过的名字");
    assert_eq!(change.project.row_version, 1);
    assert_eq!(change.project.created_at, 1000, "创建时间不得被改动");
    assert_eq!(change.project.updated_at, 3000);
    assert_eq!(
        change.project.status,
        ProjectStatus::Active,
        "重命名不改变状态"
    );
}

/// 重命名成同名（含只有首尾空白不同）：不写、不加版本、不加 revision（R-T2-e）。
#[test]
fn renaming_to_the_same_name_changes_nothing() {
    let mut f = bootstrap();

    for raw in ["项目一", "  项目一  "] {
        let before = baseline(&f);
        let change = expect_unchanged(
            catalog::rename_project(&mut f.db, update_env(&f.epoch, 0), "p1", raw, 3000).unwrap(),
        );
        assert_eq!(
            change.revision, before.revision,
            "没有变化 ⇒ revision 保持原值"
        );
        assert_eq!(change.project, f.project("p1").unwrap());
        assert_unchanged(&f, "t1", &before);
    }

    // 同值 + 过期版本 ⇒ 仍然是版本冲突：不能把过期请求当成幂等成功（它看的那一行已经旧了）。
    // 被拒的版本冲突同样要满足总纲 §5 第 8 条的零变化口径（终评 M4）。
    let before = baseline(&f);
    let err = catalog::rename_project(&mut f.db, update_env(&f.epoch, 7), "p1", "项目一", 3000)
        .unwrap_err();
    assert_eq!(err.code(), "VERSION_CONFLICT");
    assert_unchanged(&f, "t1", &before);
}

/// 版本过期：`VERSION_CONFLICT`，既有字段一个不动。
#[test]
fn renaming_rejects_a_stale_version_without_touching_the_row() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err = catalog::rename_project(&mut f.db, update_env(&f.epoch, 3), "p1", "新名字", 3000)
        .unwrap_err();

    assert_eq!(err.code(), "VERSION_CONFLICT");
    assert_unchanged(&f, "t1", &before);
}

/// 未知项目：领域拒绝（**不是**版本冲突——「记录不在」与「你手上的版本旧了」必须可区分）。
#[test]
fn renaming_an_unknown_project_is_a_domain_refusal() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err = catalog::rename_project(&mut f.db, update_env(&f.epoch, 0), "p404", "新名字", 3000)
        .unwrap_err();

    assert_domain_error(err, "找不到这个项目");
    assert_unchanged(&f, "t1", &before);
}

/// 更新类命令必须带记录版本：`for_create` 的信封（没有版本）一律拒绝，零写入。
///
/// 总纲 §9 禁止「读出当前值再跟自己比」——没有期望版本就没有并发保护，
/// 不能拿当前值冒充它。
#[test]
fn update_commands_refuse_an_envelope_without_a_row_version() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err =
        catalog::rename_project(&mut f.db, create_env(&f.epoch), "p1", "新名字", 3000).unwrap_err();
    assert_domain_error(err, "记录版本");
    assert_unchanged(&f, "t1", &before);

    let err = catalog::archive_project(&mut f.db, create_env(&f.epoch), "p1", 3000).unwrap_err();
    assert_domain_error(err, "记录版本");
    assert_unchanged(&f, "t1", &before);

    let err = catalog::set_task_project(
        &mut f.db,
        create_env(&f.epoch),
        "t1",
        ProjectTarget::Clear,
        3000,
    )
    .unwrap_err();
    assert_domain_error(err, "记录版本");
    assert_unchanged(&f, "t1", &before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 归档
// ─────────────────────────────────────────────────────────────────────────────

/// 归档**保留任务与历史**：任务行、归属与审计一条都不动，只把项目置为 archived。
#[test]
fn archiving_a_project_keeps_its_tasks_and_history() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let change = expect_changed(
        catalog::archive_project(&mut f.db, update_env(&f.epoch, 0), "p1", 4000).unwrap(),
    );

    assert_eq!(change.revision, before.revision + 1);
    assert_eq!(change.project.status, ProjectStatus::Archived);
    assert_eq!(change.project.row_version, 1);
    assert_eq!(change.project.name, "项目一", "归档不改名字");
    assert_eq!(change.project.created_at, 1000);
    assert_eq!(change.project.updated_at, 4000);

    assert_eq!(
        f.task_snapshot("t1").unwrap(),
        before.task.clone().unwrap(),
        "归档不删任务、不改归属"
    );
    assert_eq!(f.count("task"), 1);
    assert_eq!(
        f.count("task_change"),
        before.task_changes,
        "归档不写 task_change"
    );
    assert_eq!(f.count("project"), before.project_rows);
}

/// 重复归档：第二次调用 `revision` 不变，返回 `Unchanged`（R-T2-e）。
#[test]
fn archiving_an_archived_project_changes_nothing() {
    let mut f = bootstrap();
    expect_changed(
        catalog::archive_project(&mut f.db, update_env(&f.epoch, 0), "p1", 4000).unwrap(),
    );

    let before = baseline(&f);
    let change = expect_unchanged(
        catalog::archive_project(&mut f.db, update_env(&f.epoch, 1), "p1", 5000).unwrap(),
    );

    assert_eq!(
        change.revision, before.revision,
        "没有变化 ⇒ revision 保持原值"
    );
    assert_eq!(change.project.status, ProjectStatus::Archived);
    assert_eq!(change.project.updated_at, 4000, "重复归档不得刷新时间戳");
    assert_unchanged(&f, "t1", &before);
}

/// 归档也受版本保护：过期版本 ⇒ `VERSION_CONFLICT`，绝不覆盖已经变过的行。
#[test]
fn archiving_rejects_a_stale_version() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err = catalog::archive_project(&mut f.db, update_env(&f.epoch, 2), "p1", 4000).unwrap_err();

    assert_eq!(err.code(), "VERSION_CONFLICT");
    assert_unchanged(&f, "t1", &before);
}

/// 选择列表只列 active；归档项目**从列表消失但历史还在**（F-004）。
#[test]
fn the_selection_list_hides_archived_projects_while_the_history_stays() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);
    f.insert_project("p3", "项目三", "archived", 1600);

    let ids =
        |projects: Vec<ProjectRow>| -> Vec<String> { projects.into_iter().map(|p| p.id).collect() };

    assert_eq!(
        ids(catalog::list_selectable_projects(&f.db).unwrap()),
        vec!["p1".to_string(), "p2".to_string()],
        "只列 active，按 created_at, id 稳定排序"
    );

    expect_changed(
        catalog::archive_project(&mut f.db, update_env(&f.epoch, 0), "p1", 4000).unwrap(),
    );

    assert_eq!(
        ids(catalog::list_selectable_projects(&f.db).unwrap()),
        vec!["p2".to_string()],
        "归档后立刻从选择列表消失"
    );
    assert_eq!(
        ids(project_repo::list_projects(f.db.connection(), None).unwrap()),
        vec!["p1".to_string(), "p2".to_string(), "p3".to_string()],
        "历史（含归档）仍然读得到：列表过滤不等于删除"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 任务归属（F-002 的「可选项目」；V0.1 边界：只改 project_id）
// ─────────────────────────────────────────────────────────────────────────────

/// 绑定：只改 `project_id`（连同 `updated_at` / `row_version`），记一条 `task_change`，
/// `revision` 恰好 +1；任务状态与其它属性一个字都不动。
#[test]
fn binding_a_task_to_a_project_updates_the_row_and_appends_one_audit_row() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);
    let before = baseline(&f);

    let change = expect_changed(
        catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            "t1",
            ProjectTarget::Bind("p2".into()),
            5000,
        )
        .unwrap(),
    );

    assert_eq!(change.revision, before.revision + 1);
    assert_eq!(change.task.project_id.as_deref(), Some("p2"));
    assert_eq!(change.task.row_version, 1);
    assert_eq!(change.task.updated_at, 5000);
    assert_eq!(change.task.created_at, 1000, "创建时间不动");
    assert_eq!(
        change.task.status,
        TaskStatus::Inbox,
        "关联项目不改变任务状态"
    );
    assert_eq!(change.task.title, "任务一", "不迁移任何属性");
    assert_eq!(
        f.count("task_change"),
        before.task_changes + 1,
        "恰好一条审计"
    );
    assert_eq!(
        f.last_change(),
        (
            r#"{"project_id":"p1"}"#.to_string(),
            r#"{"project_id":"p2"}"#.to_string()
        ),
        "审计记的是归属的前后值"
    );
}

/// 解除关联：`project_id` 写 NULL，同样记审计、加版本、加一次 revision。
#[test]
fn clearing_the_project_link_is_recorded_once() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let change = expect_changed(
        catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            "t1",
            ProjectTarget::Clear,
            5000,
        )
        .unwrap(),
    );

    assert_eq!(change.revision, before.revision + 1);
    assert_eq!(change.task.project_id, None);
    assert_eq!(change.task.row_version, 1);
    assert_eq!(change.task.updated_at, 5000);
    assert_eq!(f.count("task_change"), before.task_changes + 1);
    assert_eq!(
        f.last_change(),
        (
            r#"{"project_id":"p1"}"#.to_string(),
            r#"{"project_id":null}"#.to_string()
        )
    );
}

/// 设成同一个归属（含本来就没有项目时的「解除」）：不写审计、不加版本、不加 revision。
#[test]
fn setting_the_same_project_changes_nothing() {
    let mut f = bootstrap();

    // t1 本来就在 p1 上。
    let before = baseline(&f);
    let change = expect_unchanged(
        catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            "t1",
            ProjectTarget::Bind("p1".into()),
            5000,
        )
        .unwrap(),
    );
    assert_eq!(change.revision, before.revision);
    assert_unchanged(&f, "t1", &before);

    // 先解除，再解除一次：同样是「没有变化」。
    expect_changed(
        catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            "t1",
            ProjectTarget::Clear,
            5000,
        )
        .unwrap(),
    );
    let before = baseline(&f);
    let change = expect_unchanged(
        catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 1),
            "t1",
            ProjectTarget::Clear,
            6000,
        )
        .unwrap(),
    );
    assert_eq!(change.revision, before.revision);
    assert_unchanged(&f, "t1", &before);

    // 同值 + 过期版本 ⇒ 版本冲突：不能把过期请求当成幂等成功。
    // 被拒的版本冲突同样要满足总纲 §5 第 8 条的零变化口径（终评 M4）。
    let before = baseline(&f);
    let err = catalog::set_task_project(
        &mut f.db,
        update_env(&f.epoch, 9),
        "t1",
        ProjectTarget::Clear,
        6000,
    )
    .unwrap_err();
    assert_eq!(err.code(), "VERSION_CONFLICT");
    assert_unchanged(&f, "t1", &before);
}

/// 归档项目不能接收新任务：在事务内拒绝（不只靠 UI 过滤），且零变化。
#[test]
fn binding_to_an_archived_project_is_refused() {
    let mut f = bootstrap();
    f.insert_project("p9", "已归档项目", "archived", 1500);
    let before = baseline(&f);

    let err = catalog::set_task_project(
        &mut f.db,
        update_env(&f.epoch, 0),
        "t1",
        ProjectTarget::Bind("p9".into()),
        5000,
    )
    .unwrap_err();

    assert_domain_error(err, "已归档");
    assert_unchanged(&f, "t1", &before);
}

/// 未知项目：拒绝，且任务行一个字段都不动。
#[test]
fn binding_to_an_unknown_project_is_refused() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err = catalog::set_task_project(
        &mut f.db,
        update_env(&f.epoch, 0),
        "t1",
        ProjectTarget::Bind("p404".into()),
        5000,
    )
    .unwrap_err();

    assert_domain_error(err, "找不到这个项目");
    assert_unchanged(&f, "t1", &before);
}

/// 未知任务：领域拒绝，且文案说的是「找不到」而不是「不能为空」。
#[test]
fn binding_an_unknown_task_is_a_domain_refusal() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);
    let before = baseline(&f);

    let err = catalog::set_task_project(
        &mut f.db,
        update_env(&f.epoch, 0),
        "t404",
        ProjectTarget::Bind("p2".into()),
        5000,
    )
    .unwrap_err();

    assert_domain_error(err, "找不到这个任务");
    assert_unchanged(&f, "t1", &before);
}

/// 任务版本过期：`VERSION_CONFLICT`，零变化。
#[test]
fn binding_rejects_a_stale_task_version() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);
    let before = baseline(&f);

    let err = catalog::set_task_project(
        &mut f.db,
        update_env(&f.epoch, 4),
        "t1",
        ProjectTarget::Bind("p2".into()),
        5000,
    )
    .unwrap_err();

    assert_eq!(err.code(), "VERSION_CONFLICT");
    assert_unchanged(&f, "t1", &before);
}

/// 旧 epoch：`DATA_EPOCH_MISMATCH`，零变化，文案不带库身份。
#[test]
fn binding_rejects_a_stale_epoch() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);
    let before = baseline(&f);
    let stale = update_env("epoch-from-another-db", 0);

    let err = catalog::set_task_project(
        &mut f.db,
        stale,
        "t1",
        ProjectTarget::Bind("p2".into()),
        5000,
    )
    .unwrap_err();

    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
    assert!(
        !err.message().contains("epoch-from-another-db"),
        "错误文案不得带库身份：{}",
        err.message()
    );
    assert_unchanged(&f, "t1", &before);
}

/// 只有**理清阶段**（Inbox / Clarifying / Ready）能改归属；一旦开始（Doing 等）拒绝。
#[test]
fn binding_is_allowed_only_before_the_task_starts() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);

    for (id, status) in [
        ("tinbox", TaskStatus::Inbox),
        ("tclarifying", TaskStatus::Clarifying),
        ("tready", TaskStatus::Ready),
    ] {
        f.insert_task(id, status, None, 2000);
        let change = expect_changed(
            catalog::set_task_project(
                &mut f.db,
                update_env(&f.epoch, 0),
                id,
                ProjectTarget::Bind("p2".into()),
                5000,
            )
            .unwrap(),
        );
        assert_eq!(change.task.status, status, "改归属不改状态");
        assert_eq!(change.task.project_id.as_deref(), Some("p2"));
    }

    for (id, status) in [
        ("tdoing", TaskStatus::Doing),
        ("twaiting", TaskStatus::Waiting),
        ("treview", TaskStatus::Review),
        ("tdone", TaskStatus::Done),
    ] {
        f.insert_task(id, status, Some("p1"), 2000);
        let before = baseline_of(&f, id);

        let err = catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            id,
            ProjectTarget::Bind("p2".into()),
            5000,
        )
        .unwrap_err();

        assert_domain_error(err, "不能改归属");
        assert_unchanged(&f, id, &before);
    }
}

/// 有会话正在跑的任务不能改归属——这是状态检查之外的**独立**守卫（也在事务内）。
///
/// 正常路径造不出「Ready 但仍有运行会话」，所以直接写库布置：只有这样才证明守卫
/// 是自己在拦，而不是靠任务状态顺手挡住。
#[test]
fn binding_is_refused_while_a_session_is_running() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);
    f.insert_task("t9", TaskStatus::Ready, Some("p1"), 2000);
    f.insert_running_session("s9", "t9", 3000);

    let before = baseline_of(&f, "t9");
    let err = catalog::set_task_project(
        &mut f.db,
        update_env(&f.epoch, 0),
        "t9",
        ProjectTarget::Clear,
        5000,
    )
    .unwrap_err();

    assert_domain_error(err, "正在计时");
    assert_unchanged(&f, "t9", &before);
    assert_eq!(f.count("work_session"), 1, "会话事实不得被这次拒绝改动");
}

/// 归档与关联的竞态：项目在**另一个连接**上被归档；请求方手上的任务版本仍然有效，
/// 但事务内的项目检查必须看到归档并拒绝——不靠 UI 过滤，也不靠调用前的读快照。
#[test]
fn a_project_archived_by_another_writer_is_refused_inside_the_transaction() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);

    {
        let path = f.db.path().unwrap().to_path_buf();
        let mut other = Db::open(path).unwrap();
        let tx = other.connection_mut().unchecked_transaction().unwrap();
        project_repo::archive_project(&tx, "p2", 0, 4000).unwrap();
        bump_revision(&tx).unwrap();
        tx.commit().unwrap();
    }

    let before = baseline(&f);
    let err = catalog::set_task_project(
        &mut f.db,
        update_env(&f.epoch, 0),
        "t1",
        ProjectTarget::Bind("p2".into()),
        5000,
    )
    .unwrap_err();

    assert_domain_error(err, "已归档");
    assert_unchanged(&f, "t1", &before);
}

/// 末步骤故障（审计写不进去）⇒ 整个事务回滚：任务行、审计、revision 不留半条痕迹。
#[test]
fn a_failed_audit_write_rolls_the_whole_assignment_back() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);
    // 任务行先改、审计后写：让审计插入必然失败，正是「半个事务」的经典形状。
    f.db.connection()
        .execute_batch(
            "CREATE TRIGGER p4_fail_audit BEFORE INSERT ON task_change
             BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;",
        )
        .unwrap();

    let before = baseline(&f);
    let err = catalog::set_task_project(
        &mut f.db,
        update_env(&f.epoch, 0),
        "t1",
        ProjectTarget::Bind("p2".into()),
        5000,
    )
    .unwrap_err();

    assert_eq!(err.code(), "STORAGE_ERROR", "基础设施失败走存储错误");
    assert_unchanged(&f, "t1", &before);
}

/// F-002 的落地形状：Inbox 任务可以先归到项目、再理清成 Ready，此后按项目筛选能找到它。
///
/// 这里用 P1 的跃迁原语（T5 的 `clarify_ready` 就是包一层事务 + epoch 校验）；
/// T5 的 `list_tasks_filtered` 落地后，筛选那一条由它接管。
#[test]
fn a_clarified_ready_task_is_findable_by_its_project() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);

    let change = expect_changed(
        catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            "t1",
            ProjectTarget::Bind("p2".into()),
            5000,
        )
        .unwrap(),
    );
    assert_eq!(
        change.task.status,
        TaskStatus::Inbox,
        "关联项目不代替理清（状态仍由理清入口推进）"
    );

    let tx = f.tx();
    task_repo::transition_task(
        &tx,
        "t1",
        change.task.row_version,
        TaskStatus::Ready,
        TransitionCause::User,
        6000,
    )
    .unwrap();
    let found: Vec<String> = {
        let mut stmt = tx
            .prepare(
                "SELECT id FROM task WHERE project_id = ?1 AND status = 'Ready'
                 ORDER BY created_at, id",
            )
            .unwrap();
        let rows = stmt.query_map(["p2"], |r| r.get(0)).unwrap();
        rows.collect::<Result<Vec<String>, _>>().unwrap()
    };
    tx.commit().unwrap();

    assert_eq!(
        found,
        vec!["t1".to_string()],
        "理清为 Ready 之后按项目筛选能找到它"
    );
}

/// R-T2-h：P1 的「归档项目不能开始/继续计时」在**新关联路径**下依然成立。
///
/// 关联成功 → 项目被归档 → 再启动计时：事务内拒绝，且不留会话行、不加 revision、
/// 不把任务推到 Doing。
#[test]
fn starting_a_timer_on_a_task_whose_project_was_archived_is_refused() {
    let mut f = bootstrap();
    f.insert_project("p2", "项目二", "active", 1500);

    let bound = expect_changed(
        catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            "t1",
            ProjectTarget::Bind("p2".into()),
            5000,
        )
        .unwrap(),
    );
    expect_changed(
        catalog::archive_project(&mut f.db, update_env(&f.epoch, 0), "p2", 5500).unwrap(),
    );

    let epoch = f.epoch.clone();
    let task_version = bound.task.row_version;
    let revision_before = f.revision();
    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let mut coord = Coordinator::new(Box::new(Arc::clone(&clock)), "run-1");

    let err = coord
        .start(
            &mut f.db,
            StartRequest {
                expected_data_epoch: epoch,
                task_id: "t1".into(),
                task_expected_version: task_version,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            },
        )
        .unwrap_err();

    assert_eq!(err.code(), "DOMAIN_ERROR");
    assert_eq!(f.revision(), revision_before, "被拒的启动不得加 revision");
    assert_eq!(f.count("work_session"), 0, "不得留下会话行");
    assert_eq!(
        f.task("t1").unwrap().status,
        TaskStatus::Inbox,
        "任务状态不得被推到 Doing"
    );
}

/// `done` 是后续版本的状态（R-T2-c）：读路径**读得懂**，V0.1 的写路径一律不动它。
#[test]
fn done_projects_are_readable_but_not_writable_in_v01() {
    let mut f = bootstrap();
    f.insert_project("pdone", "已完成项目", "done", 1500);

    assert_eq!(
        f.project_status("pdone"),
        ProjectStatus::Done,
        "读路径必须读得懂 done"
    );
    assert!(
        !catalog::list_selectable_projects(&f.db)
            .unwrap()
            .iter()
            .any(|p| p.id == "pdone"),
        "done 项目不该出现在新建任务的选择列表里"
    );

    let before = baseline(&f);
    let errors = [
        catalog::rename_project(&mut f.db, update_env(&f.epoch, 0), "pdone", "改名", 3000)
            .unwrap_err(),
        catalog::archive_project(&mut f.db, update_env(&f.epoch, 0), "pdone", 3000).unwrap_err(),
        catalog::set_task_project(
            &mut f.db,
            update_env(&f.epoch, 0),
            "t1",
            ProjectTarget::Bind("pdone".into()),
            3000,
        )
        .unwrap_err(),
    ];

    for err in errors {
        assert_domain_error(err, "当前版本");
        assert_unchanged(&f, "t1", &before);
    }
    assert_eq!(
        f.project_status("pdone"),
        ProjectStatus::Done,
        "done 行一个字没动"
    );
}
