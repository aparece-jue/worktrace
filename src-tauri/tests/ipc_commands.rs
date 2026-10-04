//! P7 Task 1 fix round 1（评审 I6）：**24 条命令体逐条覆盖**。
//!
//! `#[tauri::command]` 生成的包装（`spawn_blocking` + `State`）要 Tauri 运行时才能调，
//! 所以本文件调的是 `commands::*_impl`——命令体本身：解析请求 → 调服务 → 返回响应。
//! 包装只剩一行转发，于是「走对服务、带对信封、给对 targets、返回对类型」都能在这里断言。
//! 一个 `finish_timer` 里误调 `app.pause` 的复制粘贴错误，会在
//! [`finish_timer_impl_finishes_the_session_instead_of_pausing_it`] 当场失败。
//!
//! # 断言口径
//!
//! - **走对服务**：成功路径断言**业务事实**（返回的 DTO 字段 + 用 `SELECT` 回读那一行），
//!   不是只断言 `is_ok()`；
//! - **带对信封**：`expected_data_epoch` 给错 ⇒ `DATA_EPOCH_MISMATCH`；更新类命令的
//!   `expected_row_version` 给旧值 ⇒ `VERSION_CONFLICT`（说明请求里的 epoch/版本真的被
//!   当成了期望值，而不是「读出来再跟自己比」）；新建类命令不带版本也照样成功；
//! - **给对 targets**：`authority` 的逐条断言在 `tests/ipc_snapshots.rs` 与
//!   `tests/error_contract.rs`；这里只保证命令层把请求里的 id 交给了服务（看得到行为差别）；
//! - 时钟是冻结的 `FakeClock`（`WALL`），所以写命令落库的 `created_at` / `updated_at`
//!   逐字可断言——`AppState::now_ms()` 走的就是这条时钟接缝。
//!
//! 采样节拍设成 1 小时：本文件只测命令，不让周期采样插进来写检查点。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use worktrace_lib::commands::{
    self, ArchiveProjectRequest, ClarifyReadyRequest, CreateProjectRequest, CreateTagRequest,
    CreateTaskRequest, EpochRequest, ListProjectsRequest, ListTagsRequest, PlanMutationRequest,
    RenameProjectRequest, SetTaskProjectRequest, StartTimerRequest, TaskTagRequest,
    TaskTagsRequest,
};
use worktrace_lib::domain::session::SessionState;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::catalog::{ProjectSelector, TaskQueryRequest};
use worktrace_lib::services::daily_plan::DailyPlanQuery;
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::timer::coordinator::{ResumeRequest, SessionRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

const WALL: i64 = 1_700_000_000_000;
const TODAY: &str = "2026-10-04";
const TZ: &str = "Asia/Shanghai";

// ─────────────────────────────────────────────────────────────────────────────
// 出口与夹具
// ─────────────────────────────────────────────────────────────────────────────

/// 照单全收的出口（广播用例在下一轮的提交里加断言；这里先保证命令体不被出口干扰）。
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<EventEnvelope>>,
}

impl EventSink for RecordingSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        self.events.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

impl RecordingSink {
    fn events(&self) -> Vec<EventEnvelope> {
        self.events.lock().unwrap().clone()
    }
}

/// 起一个真应用（走 `services::bootstrap::startup`），前台放两个项目、三个任务、两个标签。
struct Shell {
    _dir: tempfile::TempDir,
    running: Box<RunningApp>,
    sink: Arc<RecordingSink>,
    epoch: String,
}

fn launch() -> Shell {
    let dir = tempfile::tempdir().unwrap();
    let db_path: PathBuf = dir.path().join("worktrace.db");
    let lock_path: PathBuf = dir.path().join("instance.lock");

    {
        let mut db = Db::open(&db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        init_meta(&tx).unwrap();
        for (id, name, status) in [("p1", "项目一", "active"), ("p2", "项目二", "archived")] {
            tx.execute(
                "INSERT INTO project(id,name,description,row_version,status,created_at,updated_at)
                 VALUES(?1,?2,NULL,0,?3,1000,1000)",
                rusqlite::params![id, name, status],
            )
            .unwrap();
        }
        for (id, title, status, project) in [
            ("t1", "任务一", "Ready", Some("p1")),
            ("t2", "任务二", "Inbox", None),
            ("t3", "任务三", "Ready", Some("p1")),
        ] {
            tx.execute(
                "INSERT INTO task(id,project_id,title,status,row_version,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,0,1000,1000)",
                rusqlite::params![id, project, title, status],
            )
            .unwrap();
        }
        for (id, kind, name) in [("tag1", "Context", "在家"), ("tag2", "Domain", "写作")] {
            tx.execute(
                "INSERT INTO tag(id,kind,name,parent_id,row_version,created_at)
                 VALUES(?1,?2,?3,NULL,0,1000)",
                rusqlite::params![id, kind, name],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    let mut config = StartupConfig::new(&db_path, &lock_path);
    config.sampling_interval_ms = 3_600_000;
    let sink = Arc::new(RecordingSink::default());
    let running = match startup(
        config,
        Box::new(FakeClock::new(WALL, 0)),
        Arc::clone(&sink) as Arc<dyn EventSink>,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };
    let epoch = running.data_epoch().to_string();
    Shell {
        _dir: dir,
        running,
        sink,
        epoch,
    }
}

impl Shell {
    /// 取串行边界。**同一个测试里只能取一次**（`Mutex` 不可重入）。
    fn state(&self) -> MutexGuard<'_, AppState> {
        lock_app(self.running.app())
    }

    fn events(&self) -> Vec<EventEnvelope> {
        self.sink.events()
    }
}

fn scalar(state: &AppState, sql: &str) -> i64 {
    state
        .db()
        .connection()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

fn text_of(state: &AppState, sql: &str) -> String {
    state
        .db()
        .connection()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

/// 当前权威 `revision`。
fn revision_of(state: &AppState) -> i64 {
    scalar(state, "SELECT revision FROM app_meta WHERE singleton = 1")
}

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 握手
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn get_revision_impl_returns_the_handshake_identity() {
    let shell = launch();
    let mut state = shell.state();

    let snapshot = commands::get_revision_impl(&mut state).unwrap();
    assert_eq!(snapshot.data_epoch, shell.epoch);
    assert_eq!(
        snapshot.revision,
        revision_of(&state),
        "握手不推进 revision"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 项目
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn list_projects_impl_reads_the_whole_history_and_filters_by_status() {
    let shell = launch();
    let mut state = shell.state();

    let all = commands::list_projects_impl(
        &mut state,
        ListProjectsRequest {
            expected_data_epoch: shell.epoch.clone(),
            status: None,
        },
    )
    .unwrap();
    assert_eq!(all.items.len(), 2, "不限制状态时归档项目也在里面");
    assert_eq!(all.data_epoch, shell.epoch);

    let archived = commands::list_projects_impl(
        &mut state,
        ListProjectsRequest {
            expected_data_epoch: shell.epoch.clone(),
            status: Some("archived".to_string()),
        },
    )
    .unwrap();
    assert_eq!(archived.items.len(), 1);
    assert_eq!(archived.items[0].id, "p2");

    // 读路径必须读得懂 `done`（V0.1 写不出来，只可能来自更新的版本）。
    let done = commands::list_projects_impl(
        &mut state,
        ListProjectsRequest {
            expected_data_epoch: shell.epoch.clone(),
            status: Some("done".to_string()),
        },
    )
    .unwrap();
    assert!(done.items.is_empty());

    let bad = commands::list_projects_impl(
        &mut state,
        ListProjectsRequest {
            expected_data_epoch: shell.epoch.clone(),
            status: Some("finished".to_string()),
        },
    )
    .expect_err("非法状态必须被拒");
    assert_code(&bad, "DOMAIN_ERROR");

    let stale = commands::list_projects_impl(
        &mut state,
        ListProjectsRequest {
            expected_data_epoch: "另一个库".to_string(),
            status: None,
        },
    )
    .expect_err("旧 epoch 必须被拒");
    assert_code(&stale, "DATA_EPOCH_MISMATCH");

    assert!(
        shell.events().is_empty(),
        "纯读命令不发任何通知：{:?}",
        shell.events()
    );
}

#[test]
fn list_selectable_projects_impl_only_lists_active_projects() {
    let shell = launch();
    let mut state = shell.state();

    let selectable = commands::list_selectable_projects_impl(
        &mut state,
        EpochRequest {
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .unwrap();
    assert_eq!(selectable.items.len(), 1);
    assert_eq!(selectable.items[0].id, "p1", "归档项目不出现在选择列表里");
}

#[test]
fn create_project_impl_trims_the_name_and_stamps_the_clock_sample() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let change = commands::create_project_impl(
        &mut state,
        CreateProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            name: "  项目三  ".to_string(),
        },
    )
    .unwrap();

    assert_eq!(change.project.name, "项目三");
    assert_eq!(change.project.created_at, WALL, "时间来自注入的时钟");
    assert_eq!(change.project.status.as_str(), "active");
    assert_eq!(change.revision, before + 1, "一次成功的业务写恰好加一次");
    assert_eq!(change.data_epoch, shell.epoch);
}

#[test]
fn rename_project_impl_carries_the_expected_version_and_is_idempotent_for_the_same_name() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let renamed = commands::rename_project_impl(
        &mut state,
        RenameProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            project_id: "p1".to_string(),
            expected_row_version: 0,
            name: "项目一（改名）".to_string(),
        },
    )
    .unwrap();
    assert_eq!(renamed.project.name, "项目一（改名）");
    assert_eq!(renamed.project.row_version, 1);
    assert_eq!(renamed.revision, before + 1);

    // 旧版本 ⇒ VERSION_CONFLICT：信封里的版本真的是期望值。
    let stale = commands::rename_project_impl(
        &mut state,
        RenameProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            project_id: "p1".to_string(),
            expected_row_version: 0,
            name: "又一次".to_string(),
        },
    )
    .expect_err("旧版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");

    // 改成同名 ⇒ 幂等：不写库、不加 revision、行版本不动。
    let revision = revision_of(&state);
    let same = commands::rename_project_impl(
        &mut state,
        RenameProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            project_id: "p1".to_string(),
            expected_row_version: 1,
            name: "项目一（改名）".to_string(),
        },
    )
    .unwrap();
    assert_eq!(same.project.row_version, 1);
    assert_eq!(same.revision, revision, "幂等重复不加 revision");
    assert_eq!(revision_of(&state), revision);
}

#[test]
fn archive_project_impl_archives_and_rejects_a_stale_epoch() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let archived = commands::archive_project_impl(
        &mut state,
        ArchiveProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            project_id: "p1".to_string(),
            expected_row_version: 0,
        },
    )
    .unwrap();
    assert_eq!(archived.project.status.as_str(), "archived");
    assert_eq!(archived.revision, before + 1);
    assert_eq!(
        text_of(&state, "SELECT status FROM project WHERE id = 'p1'"),
        "archived",
        "库里那一行也真的改了"
    );

    let stale = commands::archive_project_impl(
        &mut state,
        ArchiveProjectRequest {
            expected_data_epoch: "另一个库".to_string(),
            project_id: "p2".to_string(),
            expected_row_version: 0,
        },
    )
    .expect_err("旧 epoch 必须被拒");
    assert_code(&stale, "DATA_EPOCH_MISMATCH");
}

// ─────────────────────────────────────────────────────────────────────────────
// 标签
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn list_tags_impl_filters_by_kind_and_rejects_unknown_kinds() {
    let shell = launch();
    let mut state = shell.state();

    let all = commands::list_tags_impl(
        &mut state,
        ListTagsRequest {
            expected_data_epoch: shell.epoch.clone(),
            kind: None,
        },
    )
    .unwrap();
    assert_eq!(all.items.len(), 2);

    let contexts = commands::list_tags_impl(
        &mut state,
        ListTagsRequest {
            expected_data_epoch: shell.epoch.clone(),
            kind: Some("Context".to_string()),
        },
    )
    .unwrap();
    assert_eq!(contexts.items.len(), 1);
    assert_eq!(contexts.items[0].id, "tag1");

    let bad = commands::list_tags_impl(
        &mut state,
        ListTagsRequest {
            expected_data_epoch: shell.epoch.clone(),
            kind: Some("Knowledge".to_string()),
        },
    )
    .expect_err("V0.2 的标签类型必须被拒");
    assert_code(&bad, "DOMAIN_ERROR");
}

#[test]
fn create_tag_impl_creates_flat_tags_only() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let change = commands::create_tag_impl(
        &mut state,
        CreateTagRequest {
            expected_data_epoch: shell.epoch.clone(),
            kind: "Context".to_string(),
            name: "  办公室  ".to_string(),
            parent_id: None,
        },
    )
    .unwrap();
    assert_eq!(change.tag.name, "办公室");
    assert_eq!(change.tag.kind.as_str(), "Context");
    assert_eq!(change.revision, before + 1);

    let layered = commands::create_tag_impl(
        &mut state,
        CreateTagRequest {
            expected_data_epoch: shell.epoch.clone(),
            kind: "Context".to_string(),
            name: "子标签".to_string(),
            parent_id: Some("tag1".to_string()),
        },
    )
    .expect_err("V0.1 没有标签层级");
    assert_code(&layered, "DOMAIN_ERROR");
}

#[test]
fn tag_and_untag_impl_hand_the_pair_to_the_service_and_are_idempotent() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let tagged = commands::tag_task_impl(
        &mut state,
        TaskTagRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            tag_id: "tag1".to_string(),
        },
    )
    .unwrap();
    assert_eq!(tagged.tags.len(), 1);
    assert_eq!(tagged.tags[0].id, "tag1");
    assert_eq!(tagged.revision, before + 1);

    // 幂等重复：不写库、不加 revision。
    let revision = revision_of(&state);
    let again = commands::tag_task_impl(
        &mut state,
        TaskTagRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            tag_id: "tag1".to_string(),
        },
    )
    .unwrap();
    assert_eq!(again.revision, revision);
    assert_eq!(revision_of(&state), revision);

    let untagged = commands::untag_task_impl(
        &mut state,
        TaskTagRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            tag_id: "tag1".to_string(),
        },
    )
    .unwrap();
    assert!(untagged.tags.is_empty());
    assert_eq!(untagged.revision, revision + 1);
}

#[test]
fn tags_of_task_impl_reads_the_current_set() {
    let shell = launch();
    let mut state = shell.state();

    let empty = commands::tags_of_task_impl(
        &mut state,
        TaskTagsRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
        },
    )
    .unwrap();
    assert!(empty.items.is_empty());

    commands::tag_task_impl(
        &mut state,
        TaskTagRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            tag_id: "tag2".to_string(),
        },
    )
    .unwrap();

    let after = commands::tags_of_task_impl(
        &mut state,
        TaskTagsRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
        },
    )
    .unwrap();
    assert_eq!(after.items.len(), 1);
    assert_eq!(after.items[0].id, "tag2");
}

// ─────────────────────────────────────────────────────────────────────────────
// 任务
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn list_tasks_impl_goes_through_the_single_read_path() {
    let shell = launch();
    let mut state = shell.state();

    let ready = commands::list_tasks_impl(
        &mut state,
        TaskQueryRequest {
            statuses: vec!["Ready".to_string()],
            project: ProjectSelector::Any,
            context_tag_id: None,
            limit: 10,
            offset: 0,
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .unwrap();
    assert_eq!(ready.total, 2, "t1 与 t3");
    assert_eq!(ready.tasks.len(), 2);

    let without_project = commands::list_tasks_impl(
        &mut state,
        TaskQueryRequest {
            statuses: Vec::new(),
            project: ProjectSelector::None,
            context_tag_id: None,
            limit: 10,
            offset: 0,
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .unwrap();
    assert_eq!(without_project.total, 1);
    assert_eq!(without_project.tasks[0].id, "t2");

    let bad = commands::list_tasks_impl(
        &mut state,
        TaskQueryRequest {
            statuses: vec!["Nope".to_string()],
            project: ProjectSelector::Any,
            context_tag_id: None,
            limit: 10,
            offset: 0,
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .expect_err("非法状态串必须被拒");
    assert_code(&bad, "DOMAIN_ERROR");
}

#[test]
fn create_task_impl_captures_into_the_inbox_and_refuses_bad_input() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let change = commands::create_task_impl(
        &mut state,
        CreateTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            title: "  写报告  ".to_string(),
            project_id: Some("p1".to_string()),
        },
    )
    .unwrap();
    assert_eq!(change.task.title, "写报告");
    assert_eq!(change.task.status.as_str(), "Inbox", "捕获一律进 Inbox");
    assert_eq!(change.task.project_id.as_deref(), Some("p1"));
    assert_eq!(change.revision, before + 1);

    let empty = commands::create_task_impl(
        &mut state,
        CreateTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            title: "   ".to_string(),
            project_id: None,
        },
    )
    .expect_err("空标题必须被拒");
    assert_code(&empty, "DOMAIN_ERROR");

    let archived = commands::create_task_impl(
        &mut state,
        CreateTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            title: "挂到归档项目".to_string(),
            project_id: Some("p2".to_string()),
        },
    )
    .expect_err("归档项目不接受新归属");
    assert_code(&archived, "DOMAIN_ERROR");
}

#[test]
fn clarify_ready_impl_moves_inbox_to_ready_and_guards_the_version() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let clarified = commands::clarify_ready_impl(
        &mut state,
        ClarifyReadyRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t2".to_string(),
            expected_row_version: 0,
        },
    )
    .unwrap();
    assert_eq!(clarified.task.status.as_str(), "Ready");
    assert_eq!(clarified.revision, before + 1);
    assert_eq!(
        text_of(&state, "SELECT status FROM task WHERE id = 't2'"),
        "Ready"
    );

    let stale = commands::clarify_ready_impl(
        &mut state,
        ClarifyReadyRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t2".to_string(),
            expected_row_version: 0,
        },
    )
    .expect_err("旧版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");

    // 已经是 Ready 的任务不在这个入口的接受范围里（P3 的状态编排不从这里进来）。
    let not_clarifiable = commands::clarify_ready_impl(
        &mut state,
        ClarifyReadyRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            expected_row_version: 0,
        },
    )
    .expect_err("Ready 任务不能再被理清");
    assert_code(&not_clarifiable, "DOMAIN_ERROR");
}

#[test]
fn set_task_project_impl_binds_clears_and_refuses_archived_projects() {
    let shell = launch();
    let mut state = shell.state();

    let bound = commands::set_task_project_impl(
        &mut state,
        SetTaskProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t2".to_string(),
            expected_row_version: 0,
            project: worktrace_lib::services::catalog::ProjectTarget::Bind("p1".to_string()),
        },
    )
    .unwrap();
    assert_eq!(bound.task.project_id.as_deref(), Some("p1"));

    let cleared = commands::set_task_project_impl(
        &mut state,
        SetTaskProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t2".to_string(),
            expected_row_version: bound.task.row_version,
            project: worktrace_lib::services::catalog::ProjectTarget::Clear,
        },
    )
    .unwrap();
    assert_eq!(cleared.task.project_id, None, "解除关联");

    let archived = commands::set_task_project_impl(
        &mut state,
        SetTaskProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t2".to_string(),
            expected_row_version: cleared.task.row_version,
            project: worktrace_lib::services::catalog::ProjectTarget::Bind("p2".to_string()),
        },
    )
    .expect_err("归档项目不接受新归属");
    assert_code(&archived, "DOMAIN_ERROR");
}

// ─────────────────────────────────────────────────────────────────────────────
// 今日计划
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn plan_for_impl_validates_the_date_and_timezone_before_the_epoch() {
    let shell = launch();
    let mut state = shell.state();

    let empty = commands::plan_for_impl(
        &mut state,
        DailyPlanQuery {
            date: TODAY.to_string(),
            timezone: TZ.to_string(),
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .unwrap();
    assert!(empty.tasks.is_empty());
    assert_eq!(empty.data_epoch, shell.epoch);

    let bad_date = commands::plan_for_impl(
        &mut state,
        DailyPlanQuery {
            date: "2026-13-01".to_string(),
            timezone: TZ.to_string(),
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .expect_err("坏日期必须被拒");
    assert_code(&bad_date, "DOMAIN_ERROR");

    let bad_zone = commands::plan_for_impl(
        &mut state,
        DailyPlanQuery {
            date: TODAY.to_string(),
            timezone: "Etc/Unknown".to_string(),
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .expect_err("拿不到 IANA 名称的时区必须被拒");
    assert_code(&bad_zone, "DOMAIN_ERROR");
}

#[test]
fn add_to_plan_impl_adds_and_is_idempotent_then_remove_takes_it_out() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let added = commands::add_to_plan_impl(
        &mut state,
        PlanMutationRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            date: TODAY.to_string(),
            timezone: TZ.to_string(),
        },
    )
    .unwrap();
    assert_eq!(added.tasks.len(), 1);
    assert_eq!(added.tasks[0].id, "t1");
    assert_eq!(added.revision, before + 1);

    let revision = revision_of(&state);
    let again = commands::add_to_plan_impl(
        &mut state,
        PlanMutationRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            date: TODAY.to_string(),
            timezone: TZ.to_string(),
        },
    )
    .unwrap();
    assert_eq!(again.revision, revision, "重复加入不加 revision");

    let removed = commands::remove_from_plan_impl(
        &mut state,
        PlanMutationRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            date: TODAY.to_string(),
            timezone: TZ.to_string(),
        },
    )
    .unwrap();
    assert!(removed.tasks.is_empty());
    assert_eq!(removed.revision, revision + 1);
}

#[test]
fn remove_from_plan_impl_is_idempotent_when_the_task_was_never_added() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let removed = commands::remove_from_plan_impl(
        &mut state,
        PlanMutationRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t3".to_string(),
            date: TODAY.to_string(),
            timezone: TZ.to_string(),
        },
    )
    .unwrap();
    assert!(removed.tasks.is_empty());
    assert_eq!(removed.revision, before, "本来就不在集合里 ⇒ 零写入");
}

// ─────────────────────────────────────────────────────────────────────────────
// 计时
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn timer_snapshot_impl_reports_idle_without_creating_a_session() {
    let shell = launch();
    let mut state = shell.state();

    let snapshot = commands::timer_snapshot_impl(&mut state).unwrap();
    assert_eq!(snapshot.data_epoch, shell.epoch);
    assert_eq!(snapshot.session_id, None);
    assert_eq!(snapshot.state, None);
    assert_eq!(snapshot.active_ms, 0);
    assert_eq!(snapshot.as_of, WALL, "as_of 来自注入的时钟");
    assert_eq!(
        scalar(&state, "SELECT COUNT(*) FROM work_session"),
        0,
        "查询不建会话"
    );
}

#[test]
fn timer_tick_impl_advances_the_display_sequence() {
    let shell = launch();
    let mut state = shell.state();

    let first = commands::timer_tick_impl(&mut state).unwrap();
    let second = commands::timer_tick_impl(&mut state).unwrap();
    assert_eq!(second.tick_seq, first.tick_seq + 1);
    assert_eq!(
        commands::timer_snapshot_impl(&mut state).unwrap().tick_seq,
        second.tick_seq,
        "快照不推进 tick_seq（只有 tick 推进）"
    );
}

fn start_request(shell: &Shell, task_id: &str) -> StartTimerRequest {
    StartTimerRequest {
        expected_data_epoch: shell.epoch.clone(),
        task_id: task_id.to_string(),
        task_expected_version: 0,
        mode: "FOREGROUND".to_string(),
        timer_kind: "stopwatch".to_string(),
        target_duration_ms: None,
        expected_interval_ms: 1_000,
    }
}

#[test]
fn start_timer_impl_starts_a_foreground_stopwatch_and_validates_the_strings() {
    let shell = launch();
    let mut state = shell.state();
    let before = revision_of(&state);

    let started = commands::start_timer_impl(&mut state, start_request(&shell, "t1")).unwrap();
    let session_id = started.snapshot.session_id.clone().expect("应当有会话");
    assert_eq!(started.snapshot.state, Some(SessionState::Running));
    assert_eq!(started.snapshot.timer_kind.unwrap().as_str(), "stopwatch");
    assert_eq!(started.revision, before + 1);
    assert_eq!(started.snapshot.revision, started.revision);
    assert_eq!(
        text_of(
            &state,
            &format!("SELECT state FROM work_session WHERE id = '{session_id}'")
        ),
        "running"
    );
    assert_eq!(
        text_of(
            &state,
            &format!("SELECT task_id FROM work_session WHERE id = '{session_id}'")
        ),
        "t1"
    );
    assert_eq!(
        text_of(&state, "SELECT status FROM task WHERE id = 't1'"),
        "Doing",
        "start 在同一条命令里把 Inbox/Ready 推到 Doing"
    );

    // 字符串枚举的非法取值：稳定错误码，而不是 Tauri 的反序列化错误。
    let bad_mode = commands::start_timer_impl(
        &mut state,
        StartTimerRequest {
            mode: "FOCUS".to_string(),
            ..start_request(&shell, "t3")
        },
    )
    .expect_err("非法模式必须被拒");
    assert_code(&bad_mode, "DOMAIN_ERROR");

    let bad_kind = commands::start_timer_impl(
        &mut state,
        StartTimerRequest {
            timer_kind: "pomodoro".to_string(),
            ..start_request(&shell, "t3")
        },
    )
    .expect_err("非法计时类型必须被拒");
    assert_code(&bad_kind, "DOMAIN_ERROR");
}

#[test]
fn pause_timer_impl_pauses_the_session_instead_of_finishing_it() {
    let shell = launch();
    let mut state = shell.state();

    let started = commands::start_timer_impl(&mut state, start_request(&shell, "t1")).unwrap();
    let session_id = started.snapshot.session_id.clone().unwrap();
    let version = started.snapshot.session_version.unwrap();

    let paused = commands::pause_timer_impl(
        &mut state,
        SessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            session_expected_version: version,
        },
    )
    .unwrap();
    assert_eq!(
        paused.snapshot.state,
        Some(SessionState::Paused),
        "pause 必须暂停，不能顺手结束（finish/pause 复制粘贴就会在这里红）"
    );
    assert_eq!(
        text_of(
            &state,
            &format!("SELECT state FROM work_session WHERE id = '{session_id}'")
        ),
        "paused"
    );
    assert_eq!(
        scalar(
            &state,
            &format!("SELECT COALESCE(ended_at, -1) FROM work_session WHERE id = '{session_id}'")
        ),
        -1,
        "暂停不写 ended_at"
    );

    let stale = commands::pause_timer_impl(
        &mut state,
        SessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            session_expected_version: version,
        },
    )
    .expect_err("旧会话版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");
}

#[test]
fn resume_timer_impl_resumes_with_both_versions() {
    let shell = launch();
    let mut state = shell.state();

    let started = commands::start_timer_impl(&mut state, start_request(&shell, "t1")).unwrap();
    let session_id = started.snapshot.session_id.clone().unwrap();
    let paused = commands::pause_timer_impl(
        &mut state,
        SessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            session_expected_version: started.snapshot.session_version.unwrap(),
        },
    )
    .unwrap();

    let resumed = commands::resume_timer_impl(
        &mut state,
        ResumeRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            task_expected_version: paused.task_version,
            session_id: session_id.clone(),
            session_expected_version: paused.snapshot.session_version.unwrap(),
        },
    )
    .unwrap();
    assert_eq!(resumed.snapshot.state, Some(SessionState::Running));
    assert_eq!(
        text_of(
            &state,
            &format!("SELECT state FROM work_session WHERE id = '{session_id}'")
        ),
        "running"
    );

    let stale = commands::resume_timer_impl(
        &mut state,
        ResumeRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            task_expected_version: paused.task_version,
            session_id: session_id.clone(),
            session_expected_version: paused.snapshot.session_version.unwrap(),
        },
    )
    .expect_err("旧会话版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");
}

#[test]
fn finish_timer_impl_finishes_the_session_instead_of_pausing_it() {
    let shell = launch();
    let mut state = shell.state();

    let started = commands::start_timer_impl(&mut state, start_request(&shell, "t1")).unwrap();
    let session_id = started.snapshot.session_id.clone().unwrap();

    let finished = commands::finish_timer_impl(
        &mut state,
        SessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            session_expected_version: started.snapshot.session_version.unwrap(),
        },
    )
    .unwrap();
    assert_eq!(
        finished.snapshot.state,
        Some(SessionState::Finished),
        "finish 必须结束会话（误调 pause 会在这里红）"
    );
    assert_eq!(
        text_of(
            &state,
            &format!("SELECT state FROM work_session WHERE id = '{session_id}'")
        ),
        "finished"
    );
    assert_ne!(
        scalar(
            &state,
            &format!("SELECT COALESCE(ended_at, -1) FROM work_session WHERE id = '{session_id}'")
        ),
        -1,
        "结束会写 ended_at"
    );

    let stale = commands::finish_timer_impl(
        &mut state,
        SessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            session_expected_version: started.snapshot.session_version.unwrap(),
        },
    )
    .expect_err("旧会话版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");
}
