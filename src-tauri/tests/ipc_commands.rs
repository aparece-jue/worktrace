//! P7 Task 1 fix round 1（评审 I6）：**30 条命令体逐条覆盖**。
//!
//! `#[tauri::command]` 生成的包装（`spawn_blocking` + `State`）要 Tauri 运行时才能调，
//! 所以本文件调的是 `commands::*_impl`——命令体本身：解析请求 → 调服务 → 返回响应。
//! 包装只剩一行转发，于是「走对服务、带对信封、返回对类型」都能在这里断言
//! （**`targets` 不在其中**，理由见下面「给对 targets」那一条）。
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
//! - **给对 targets**：**本文件不覆盖**（2026-10-04 订正）。`targets` 只活在
//!   `#[tauri::command]` 包装里（命令体拿不到它），要观测 `authority.records` 就得有
//!   Tauri 运行时——`tauri/test` 的 `mock_builder`，本轮没有启用。原先这里写「逐条断言在
//!   `tests/ipc_snapshots.rs` 与 `tests/error_contract.rs`」**不实**：那两份覆盖的是
//!   `capture_error_response` 这个**机制**（喂显式 targets）与一份**样例**
//!   `ErrorResponse`，都不是「逐条命令的 targets」。**登记为遗留**，见
//!   `docs/superpowers/plans/2026-10-03-p7-shell-and-ui.md` 的「遗留与边界」一节；
//! - 时钟是冻结的 `FakeClock`（`WALL`），所以写命令落库的 `created_at` / `updated_at`
//!   逐字可断言——`AppState::now_ms()` 走的就是这条时钟接缝。
//!
//! 采样节拍设成 1 小时：本文件只测命令，不让周期采样插进来写检查点。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use worktrace_lib::commands::{
    self, ArchiveProjectRequest, BackfillRequest, ClarifyReadyRequest, ConfirmedRangeRequest,
    CorrectRequest, CreateProjectRequest, CreateTagRequest, CreateTaskRequest,
    DiscardSessionRequest, EpochRequest, ListProjectsRequest, ListTagsRequest, PlanMutationRequest,
    ReconcileRequest, RenameProjectRequest, SetTaskProjectRequest, StartTimerRequest,
    TaskTagRequest, TaskTagsRequest, TransitionTaskRequest,
};
use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::domain::task::TaskStatus;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppGuard, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::catalog::{ProjectSelector, TaskQueryRequest};
use worktrace_lib::services::daily_plan::DailyPlanQuery;
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::stats::{
    Measure, MeasureColumn, StatsClass, StatsRange, TodayQuery, TodayView,
};
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

/// 照单全收的出口（`domain.changed` 发送侧的断言在文件末尾那一节）。
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

/// 永远失败的出口：证明**广播失败不影响命令结果**（00 §4）。
#[derive(Default)]
struct FailingSink {
    calls: AtomicUsize,
}

impl EventSink for FailingSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err("webview gone".to_string())
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
    let recorder = Arc::new(RecordingSink::default());
    let (dir, running) = launch_with(Arc::clone(&recorder) as Arc<dyn EventSink>);
    let epoch = running.data_epoch().to_string();
    Shell {
        _dir: dir,
        running,
        sink: recorder,
        epoch,
    }
}

/// 起一个真应用，用调用方给的出口（记录型 / 失败型都能起）。
fn launch_with(sink: Arc<dyn EventSink>) -> (tempfile::TempDir, Box<RunningApp>) {
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
    let running = match startup(
        config,
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };
    (dir, running)
}

impl Shell {
    /// 取串行边界。**同一个测试里只能取一次**（`Mutex` 不可重入）。
    fn state(&self) -> AppGuard<'_> {
        lock_app(self.running.app())
    }

    fn events(&self) -> Vec<EventEnvelope> {
        self.sink.events()
    }
}

fn scalar(state: &AppState, sql: &str) -> i64 {
    state
        .db()
        .unwrap()
        .connection()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

fn text_of(state: &AppState, sql: &str) -> String {
    state
        .db()
        .unwrap()
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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
// 统计（F-010 的「今日工时」半边）
// ─────────────────────────────────────────────────────────────────────────────

/// `FakeClock` 冻结在 [`WALL`]（2023-11-14T22:13:20Z）⇒ 两个时区里的「今天」是常量。
/// 日期与日界**写死在这里**，不用服务自己的函数算期望值（那是同义反复）。
const UTC_DATE: &str = "2023-11-14";
const UTC_MID: i64 = 1_699_920_000_000;
const SH_DATE: &str = "2023-11-15";
const SH_MID: i64 = 1_699_977_600_000;
const DAY_MS: i64 = 86_400_000;

/// 三组工时各自固定四项、顺序与 [`Measure::ALL`] 一致，且**与视图同源**
/// （同一 `timezone`/`range`/`as_of`/`data_epoch`/`revision`）。
/// 把 `live` 组接到 `confirmed` 上、或让某组自带另一个水位，都会在这里红。
fn assert_four_measures(view: &TodayView, class: StatsClass, columns: &[MeasureColumn]) {
    let measures: Vec<Measure> = columns.iter().map(|column| column.measure).collect();
    assert_eq!(
        measures,
        Measure::ALL.to_vec(),
        "{class:?} 必须固定四项、顺序固定"
    );
    for column in columns {
        assert_eq!(column.class, class, "{class:?} 组里混进了别类的列");
        assert_eq!(column.timezone, view.timezone);
        assert_eq!(column.range, view.range);
        assert_eq!(column.as_of, view.as_of, "五项必须出自同一次查询");
        assert_eq!(column.data_epoch, view.data_epoch);
        assert_eq!(column.revision, view.revision);
    }
}

#[test]
fn stats_today_impl_returns_the_five_items_from_one_authoritative_read() {
    let shell = launch();
    let mut state = shell.state();

    // 先在「上海 2023-11-15」这一天放一个任务：① 列表不是空的，② revision 真的前进过，
    // 下面的权威版本断言就不会落在「0 == 0」这种空断言上。
    commands::add_to_plan_impl(
        &mut state,
        shell.running.broadcaster(),
        PlanMutationRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            date: SH_DATE.to_string(),
            timezone: TZ.to_string(),
        },
    )
    .unwrap();

    let view = commands::stats_today_impl(
        &mut state,
        TodayQuery {
            timezone: TZ.to_string(),
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .unwrap();

    // ① 今日选择列表：就是刚放进去的那条（与 `plan_for` 同一个读事务）。
    assert_eq!(view.tasks.len(), 1);
    assert_eq!(view.tasks[0].id, "t1");
    assert_eq!(view.tasks[0].title, "任务一");
    // ② 当前任务：本夹具没有任何会话被装载进协调器镜像。
    assert!(view.current.is_none(), "没有装载过任何会话");
    // ③④⑤ 三组各四项，且与视图同源。
    assert_four_measures(&view, StatsClass::Confirmed, &view.confirmed);
    assert_four_measures(&view, StatsClass::Live, &view.live);
    assert_four_measures(&view, StatsClass::Pending, &view.pending);

    // 库是空的（没有区间）：已确认与实时暂计是 0（不是缺失），待确认一条候选都没有
    // ⇒ 按条数判有无，毫秒按「不推算」给 `None`。
    assert_eq!(
        view.column(StatsClass::Confirmed, Measure::Human).ms,
        Some(0)
    );
    assert_eq!(view.column(StatsClass::Live, Measure::Human).ms, Some(0));
    assert_eq!(
        view.column(StatsClass::Pending, Measure::Human).intervals,
        0
    );
    assert_eq!(view.column(StatsClass::Pending, Measure::Human).ms, None);

    // 口径字段：`date`/`range`/`as_of` 同源（服务从同一次样本的归属终点算），
    // `revision`/`data_epoch` 与库的权威值一致 ⇒ 页面的本视图水位能用来判旧。
    assert_eq!(view.date, SH_DATE);
    assert_eq!(view.timezone, TZ);
    assert_eq!(
        view.range,
        StatsRange {
            from: SH_MID,
            to: SH_MID + DAY_MS
        },
        "日界是次日零点的换算结果，不是 start + 24h"
    );
    assert!(view.range.from < view.range.to);
    assert_eq!(view.as_of, WALL, "as_of 就是那一次样本的归属终点");
    assert_eq!(view.data_epoch, shell.epoch);
    assert_eq!(view.revision, revision_of(&state));

    // 时区归一：小写 `utc` 回显成 IANA 名字 `UTC`（原样回显就会在这里红）；「今天」
    // 也跟着时区走到 UTC 的那一天——计划是按 (日期, 时区) 存的，上海那天的不该在这里。
    let utc = commands::stats_today_impl(
        &mut state,
        TodayQuery {
            timezone: "utc".to_string(),
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .unwrap();
    assert_eq!(utc.timezone, "UTC");
    assert_eq!(utc.date, UTC_DATE);
    assert_eq!(
        utc.range,
        StatsRange {
            from: UTC_MID,
            to: UTC_MID + DAY_MS
        }
    );
    assert!(utc.tasks.is_empty(), "计划是按 (日期, 时区) 存的");

    // 读路径同样过 epoch 守卫：期望值不对 ⇒ `DATA_EPOCH_MISMATCH`。
    let stale = commands::stats_today_impl(
        &mut state,
        TodayQuery {
            timezone: TZ.to_string(),
            expected_data_epoch: "66666666-6666-4666-8666-666666666666".to_string(),
        },
    )
    .expect_err("陈旧 epoch 必须被拒");
    assert_code(&stale, "DATA_EPOCH_MISMATCH");

    // 坏时区在服务入口（归一那唯一一条）就被拒，不是回显一个坏名字。
    let bad_zone = commands::stats_today_impl(
        &mut state,
        TodayQuery {
            timezone: "Etc/Unknown".to_string(),
            expected_data_epoch: shell.epoch.clone(),
        },
    )
    .expect_err("拿不到 IANA 名称的时区必须被拒");
    assert_code(&bad_zone, "DOMAIN_ERROR");
}

// ─────────────────────────────────────────────────────────────────────────────
// 恢复与历史（P3 的服务，P8 的 IPC）
// ─────────────────────────────────────────────────────────────────────────────

/// 恢复会话的起点与可信前缀的终点（形状抄 `tests/reconcile.rs::crashed_recovering`）。
const T0: i64 = WALL - 20_000;
const PREFIX_END: i64 = WALL - 18_000;

/// 上一次运行留下的 run 行（`work_session.run_id` 有外键，所以先建它）。
///
/// 刻意用**另一个** run id：对账会把会话的 `run_id` 切到本次 run，那条断言才有内容
/// （两边同值时它恒真）。`clean_exit_at` 非空，免得「哪一行是当前 run」出现第二个答案。
fn seed_old_run(state: &AppState, run_id: &str, started_at: i64) {
    state
        .db()
        .unwrap()
        .connection()
        .execute(
            "INSERT INTO application_run(id,started_at,clean_exit_at) VALUES(?1,?2,?2)",
            rusqlite::params![run_id, started_at],
        )
        .unwrap();
}

/// 崩过、已被启动扫描归一的那条会话：可信前缀 + 零长度候选 + 终点未知段。
///
/// **直接插进运行中的库**，不进 `launch_with` 的前台夹具：夹具一旦带上一条别的 run 的
/// `recovering` 会话，恢复门禁会挡住 `start_timer`，本文件其余命令体用例会一起变红。
fn seed_recovering_session(state: &AppState, session_id: &str, run_id: &str) {
    let conn = state.db().unwrap().connection();
    conn.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,ended_at,
                                  needs_review,row_version)
         VALUES(?1,'t1',?2,'FOREGROUND','recovering','stopwatch',?3,NULL,1,0)",
        rusqlite::params![session_id, run_id, T0],
    )
    .unwrap();
    // (后缀, started_at, ended_at, duration_ms, needs_review)
    for (suffix, started_at, ended_at, duration_ms, needs_review) in [
        ("prefix", T0, Some(PREFIX_END), Some(PREFIX_END - T0), 0_i64),
        ("cand", PREFIX_END, Some(PREFIX_END), None, 1),
        ("unknown", WALL - 17_000, None, None, 1),
    ] {
        conn.execute(
            "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review,
                                       voided_at)
             VALUES(?1,?2,?3,?4,?5,?6,NULL)",
            rusqlite::params![
                format!("{session_id}-{suffix}"),
                session_id,
                started_at,
                ended_at,
                duration_ms,
                needs_review
            ],
        )
        .unwrap();
    }
}

/// 一条**没有任何区间**的会话：`discard_session` 的防御分支（报告要答「作废了哪一段」）。
fn seed_session_without_intervals(state: &AppState, session_id: &str, run_id: &str) {
    state
        .db()
        .unwrap()
        .connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,ended_at,
                                      needs_review,row_version)
             VALUES(?1,'t1',?2,'FOREGROUND','paused','stopwatch',?3,NULL,0,0)",
            rusqlite::params![session_id, run_id, WALL - 5_000],
        )
        .unwrap();
}

/// 当前 run 的 id：从计时快照取（协调器那条接缝的公开出口），不猜 SQL。
fn current_run_id(state: &mut AppState) -> String {
    commands::timer_snapshot_impl(state).unwrap().run_id
}

/// 一条 `finished` 会话 + 它唯一的区间（走**真实**的 start / finish，不手工 SQL）：
/// 返回 `(session_id, interval_id, 提交后的会话版本)`。
fn finished_session(state: &mut AppState, shell: &Shell, task_id: &str) -> (String, String, i64) {
    let started = commands::start_timer_impl(
        state,
        shell.running.broadcaster(),
        start_request(shell, task_id),
    )
    .unwrap();
    let session_id = started.snapshot.session_id.clone().expect("应当有会话");
    let finished = commands::finish_timer_impl(
        state,
        shell.running.broadcaster(),
        SessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            session_expected_version: started.snapshot.session_version.unwrap(),
        },
    )
    .unwrap();
    let interval_id = text_of(
        state,
        &format!("SELECT id FROM work_interval WHERE session_id = '{session_id}'"),
    );
    (
        session_id,
        interval_id,
        finished.snapshot.session_version.unwrap(),
    )
}

/// 最近一条 `domain.changed` 的载荷（断言「载荷就是这次响应」）。
fn last_payload(shell: &Shell) -> serde_json::Value {
    shell
        .events()
        .last()
        .expect("应当已经有广播")
        .payload
        .clone()
}

/// `reconcile`：一次处理**全部**待确认区间，并给出权威 `data_epoch` / `revision`。
///
/// 第二次（会话已经不是 `recovering`）必须**被拒**而不是 `Unchanged`：这条命令没有
/// 「幂等重复」那一支——重复确认若被当成又一笔写，就会重复审计、重复加版本。
#[test]
fn reconcile_impl_confirms_every_pending_interval_and_rejects_a_second_pass() {
    let shell = launch();
    let mut state = shell.state();
    let run_id = current_run_id(&mut state);
    seed_old_run(&state, "run-old", T0 - 1_000);
    seed_recovering_session(&state, "s1", "run-old");

    // 先做一次会推进 revision 的写：下面的权威版本断言不会落在「0 == 0」上。
    let before = commands::create_project_impl(
        &mut state,
        shell.running.broadcaster(),
        CreateProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            name: "项目三".to_string(),
        },
    )
    .unwrap()
    .revision;
    let events_before = shell.events().len();

    let request = |epoch: &str, version: i64, action: &str| ReconcileRequest {
        expected_data_epoch: epoch.to_string(),
        session_id: "s1".to_string(),
        expected_row_version: version,
        action: action.to_string(),
        target_state: "finished".to_string(),
        ranges: vec![
            // 零长度候选：确认成零长度事实（`duration_ms = 0`）。
            ConfirmedRangeRequest {
                interval_id: "s1-cand".to_string(),
                started_at: PREFIX_END,
                ended_at: PREFIX_END,
            },
            // 终点未知的那段：用户给出终点，`ended_at <= now`。
            ConfirmedRangeRequest {
                interval_id: "s1-unknown".to_string(),
                started_at: WALL - 17_000,
                ended_at: WALL - 10_000,
            },
        ],
    };

    let report = commands::reconcile_impl(
        &mut state,
        shell.running.broadcaster(),
        request(&shell.epoch, 0, "confirm"),
    )
    .unwrap();

    // 响应自带权威版本（页面的本视图水位靠它判旧）。
    assert_eq!(report.data_epoch, shell.epoch);
    assert_eq!(report.revision, revision_of(&state));
    assert!(report.revision > before, "对账真的写了库");
    // 会话收尾：`finished` + `needs_review` 清假 + 版本 +1 + `run_id` 切到**本次** run。
    assert_eq!(report.session.id, "s1");
    assert_eq!(report.session.state, SessionState::Finished);
    assert!(!report.session.needs_review, "收尾必须清会话级待确认标记");
    assert_eq!(report.session.row_version, 1);
    assert_eq!(
        report.session.run_id, run_id,
        "对账把恢复归属切到本次 run（原始归属留在审计里）"
    );
    assert_eq!(
        report.session.ended_at,
        Some(WALL - 10_000),
        "finished 的终点取最后一条非作废区间的端点"
    );
    // 报告带该会话的**全部**区间（不只是被处理的两条）。
    assert_eq!(report.intervals.len(), 3);
    let confirmed = report
        .intervals
        .iter()
        .find(|interval| interval.id == "s1-unknown")
        .expect("终点未知的那段也在报告里");
    assert_eq!(confirmed.started_at, WALL - 17_000);
    assert_eq!(confirmed.ended_at, Some(WALL - 10_000));
    assert_eq!(
        confirmed.duration_ms,
        Some(7_000),
        "确认之后时长才成为事实（= 用户给的起止之差）"
    );
    assert!(!confirmed.needs_review);
    // 库里的权威事实与响应一致（不是把请求回显了一遍）。
    assert_eq!(
        text_of(&state, "SELECT state FROM work_session WHERE id = 's1'"),
        "finished"
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT COUNT(*) FROM work_interval WHERE session_id = 's1' AND needs_review = 1"
        ),
        0,
        "该会话不得再有待确认区间"
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT COUNT(*) FROM work_interval WHERE session_id = 's1'
               AND voided_at IS NULL AND ended_at IS NULL"
        ),
        0,
        "收尾后不得再留「未作废且终点未知」的段"
    );
    // 恰好一条广播，载荷就是这次响应。
    let events = shell.events();
    assert_eq!(events.len(), events_before + 1, "{events:?}");
    let event = events.last().unwrap();
    assert_eq!(event.event, "domain.changed");
    assert_eq!(event.revision, report.revision);
    assert_eq!(event.data_epoch, report.data_epoch);
    assert_eq!(last_payload(&shell), serde_json::to_value(&report).unwrap());

    // 第二次：会话已经是 `finished` ⇒ 拒绝（**不是** `Unchanged`）。
    let again = commands::reconcile_impl(
        &mut state,
        shell.running.broadcaster(),
        request(&shell.epoch, report.session.row_version, "confirm"),
    )
    .expect_err("非 recovering 的会话必须被拒");
    assert_code(&again, "DOMAIN_ERROR");
    assert_eq!(
        shell.events().len(),
        events_before + 1,
        "被拒的命令不广播、也不写库"
    );
    assert_eq!(revision_of(&state), report.revision, "被拒 ⇒ 版本不动");

    // epoch 不匹配 ⇒ `DATA_EPOCH_MISMATCH`（信封在写事务之前就校验）。
    let stale = commands::reconcile_impl(
        &mut state,
        shell.running.broadcaster(),
        request(
            "66666666-6666-4666-8666-666666666666",
            report.session.row_version,
            "confirm",
        ),
    )
    .expect_err("陈旧 epoch 必须被拒");
    assert_code(&stale, "DATA_EPOCH_MISMATCH");

    // 动作串非法 ⇒ `DOMAIN_ERROR`（命令层显式解析，不是 Tauri 的反序列化错误）。
    let bad_action = commands::reconcile_impl(
        &mut state,
        shell.running.broadcaster(),
        request(&shell.epoch, report.session.row_version, "approve"),
    )
    .expect_err("取值域外的动作必须被拒");
    assert_code(&bad_action, "DOMAIN_ERROR");
}

/// `reconcile` 的另一个动作：`discard_uncertain` —— **只丢不确定区间、保留可信前缀**。
///
/// 服务语义由 P3 覆盖；这里钉的是**命令层的动作映射**：把它错映射成 `Confirm`（两条动作
/// 的前置正好相反：`Confirm` 要求 `ranges` 恰好覆盖待确认集合，`DiscardUncertain` 要求
/// 它为空）会让本用例在 `.unwrap()` 处直接红。
#[test]
fn reconcile_impl_discard_uncertain_keeps_the_trusted_prefix() {
    let shell = launch();
    let mut state = shell.state();
    let run_id = current_run_id(&mut state);
    seed_old_run(&state, "run-old", T0 - 1_000);
    seed_recovering_session(&state, "s2", "run-old");
    let events_before = shell.events().len();

    let report = commands::reconcile_impl(
        &mut state,
        shell.running.broadcaster(),
        ReconcileRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: "s2".to_string(),
            expected_row_version: 0,
            action: "discard_uncertain".to_string(),
            target_state: "paused".to_string(),
            // `DiscardUncertain` 必须给空列表：作废集合由服务从库里取。
            ranges: Vec::new(),
        },
    )
    .unwrap();

    assert_eq!(report.session.state, SessionState::Paused);
    assert!(!report.session.needs_review);
    assert_eq!(report.session.run_id, run_id);
    assert_eq!(report.data_epoch, shell.epoch);
    assert_eq!(report.revision, revision_of(&state));
    // 可信前缀**原样保留**（这正是它与 `discard_session` 的区别）。
    let prefix = report
        .intervals
        .iter()
        .find(|interval| interval.id == "s2-prefix")
        .expect("可信前缀也在报告里");
    assert_eq!(prefix.voided_at, None, "可信前缀不许被作废");
    assert_eq!(prefix.duration_ms, Some(PREFIX_END - T0), "时长不动");
    assert!(!prefix.needs_review);
    // 两条不确定段全部作废，且不给它们补时长。
    for id in ["s2-cand", "s2-unknown"] {
        let interval = report
            .intervals
            .iter()
            .find(|interval| interval.id == id)
            .unwrap_or_else(|| panic!("{id} 也在报告里"));
        assert!(interval.voided_at.is_some(), "{id} 必须被作废");
        assert!(!interval.needs_review);
        assert_eq!(interval.duration_ms, None, "作废不编时长");
    }
    assert_eq!(
        scalar(
            &state,
            "SELECT COUNT(*) FROM work_interval WHERE session_id = 's2' AND voided_at IS NULL"
        ),
        1,
        "只剩可信前缀一条未作废"
    );
    // 恰好一条广播，载荷就是响应。
    assert_eq!(shell.events().len(), events_before + 1);
    assert_eq!(last_payload(&shell), serde_json::to_value(&report).unwrap());
}

/// `correct`：重定时一条可信区间；版本位、幂等重复与各条拒绝路径。
#[test]
fn correct_impl_retimes_a_finished_interval_and_guards_the_session_version() {
    let shell = launch();
    let mut state = shell.state();
    let (session_id, interval_id, version) = finished_session(&mut state, &shell, "t1");
    let events_before = shell.events().len();

    let request =
        |expected: i64, action: &str, from: Option<i64>, to: Option<i64>| CorrectRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            expected_row_version: expected,
            interval_id: interval_id.clone(),
            action: action.to_string(),
            started_at: from,
            ended_at: to,
            reason: Some("写错了".to_string()),
        };

    let report = commands::correct_impl(
        &mut state,
        shell.running.broadcaster(),
        request(version, "retime", Some(WALL - 60_000), Some(WALL)),
    )
    .unwrap();

    assert_eq!(report.session.id, session_id);
    assert_eq!(report.session.state, SessionState::Finished);
    assert_eq!(
        report.session.row_version,
        version + 1,
        "改区间事实会把所属会话的版本 +1（区间没有独立版本列）"
    );
    assert_eq!(report.interval.id, interval_id);
    assert_eq!(report.interval.started_at, WALL - 60_000);
    assert_eq!(report.interval.ended_at, Some(WALL));
    assert_eq!(
        report.interval.duration_ms,
        Some(60_000),
        "起止与时长必须一起改"
    );
    assert_eq!(report.data_epoch, shell.epoch);
    assert_eq!(report.revision, revision_of(&state));
    // 落库一致：不是只改内存。
    assert_eq!(
        scalar(
            &state,
            &format!("SELECT duration_ms FROM work_interval WHERE id = '{interval_id}'")
        ),
        60_000
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT COUNT(*) FROM time_edit WHERE reason = 'correct:retime'"
        ),
        1,
        "一次真实修正写一条审计"
    );
    // 一条广播，载荷就是响应。
    assert_eq!(shell.events().len(), events_before + 1);
    assert_eq!(last_payload(&shell), serde_json::to_value(&report).unwrap());

    // 幂等重复：同样的起止 ⇒ `Unchanged`（不写审计、不加版本、不广播）。
    let revision = revision_of(&state);
    let again = commands::correct_impl(
        &mut state,
        shell.running.broadcaster(),
        request(
            report.session.row_version,
            "retime",
            Some(WALL - 60_000),
            Some(WALL),
        ),
    )
    .unwrap();
    assert_eq!(again.revision, revision, "Unchanged 不前进版本");
    assert_eq!(again.interval.started_at, WALL - 60_000);
    assert_eq!(
        shell.events().len(),
        events_before + 1,
        "Unchanged 一条都不发：{:?}",
        shell.events()
    );

    // 旧版本 ⇒ `VERSION_CONFLICT`（请求里的版本真的被当成了期望值）。
    let stale = commands::correct_impl(
        &mut state,
        shell.running.broadcaster(),
        request(version, "retime", Some(WALL - 30_000), Some(WALL)),
    )
    .expect_err("旧会话版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");

    // 版本位**必填**：少了这个键就是传输层（serde）错误，不是一条可用的调用路径
    // （与其他四条命令、以及 `expected_data_epoch` 同一口径：必填标量缺了就拒绝）。
    // 服务层那条「缺少记录版本」守卫因此从命令层不可达——它的覆盖落在
    // `tests/correct.rs` 的缺版本用例上。
    let without_version = serde_json::json!({
        "expected_data_epoch": shell.epoch,
        "session_id": session_id,
        "interval_id": interval_id,
        "action": "delete",
    });
    assert!(
        serde_json::from_value::<CorrectRequest>(without_version).is_err(),
        "expected_row_version 是必填标量：省略它必须是反序列化错误"
    );

    // 三个可选字段**可以省略**（等价于 `null`）：这条 `delete` 只给必填键。
    // 它同时钉住 `delete` 这个动作词真的映射到服务的软删除——映射成 `retime` 就会
    // 因为缺起止而 `DOMAIN_ERROR`。
    let bare_delete: CorrectRequest = serde_json::from_value(serde_json::json!({
        "expected_data_epoch": shell.epoch,
        "session_id": session_id,
        "expected_row_version": report.session.row_version,
        "interval_id": interval_id,
        "action": "delete",
    }))
    .expect("可选字段省略必须能反序列化");
    assert_eq!(bare_delete.started_at, None);
    assert_eq!(bare_delete.ended_at, None);
    assert_eq!(bare_delete.reason, None);
    let deleted = commands::correct_impl(&mut state, shell.running.broadcaster(), bare_delete)
        .expect("软删除应当成功");
    assert_eq!(deleted.interval.id, interval_id);
    assert!(deleted.interval.voided_at.is_some(), "删除 = 软删除");
    assert!(!deleted.interval.needs_review);
    assert!(deleted.revision > report.revision);
    assert_eq!(
        scalar(
            &state,
            &format!("SELECT COUNT(*) FROM work_interval WHERE id = '{interval_id}'")
        ),
        1,
        "软删除不 DELETE 行"
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT COUNT(*) FROM time_edit WHERE reason = 'correct:delete'"
        ),
        1,
        "删除写一条审计"
    );
    assert_eq!(
        shell.events().len(),
        events_before + 2,
        "这一笔真的改了库 ⇒ 又一条广播：{:?}",
        shell.events()
    );

    // 非 `finished` 的会话 ⇒ 服务拒绝（命令层不加这条判断，也不放宽）。
    let running = commands::start_timer_impl(
        &mut state,
        shell.running.broadcaster(),
        start_request(&shell, "t3"),
    )
    .unwrap();
    let not_finished = commands::correct_impl(
        &mut state,
        shell.running.broadcaster(),
        CorrectRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: running.snapshot.session_id.clone().unwrap(),
            expected_row_version: running.snapshot.session_version.unwrap(),
            interval_id: "whatever".to_string(),
            action: "delete".to_string(),
            started_at: None,
            ended_at: None,
            reason: None,
        },
    )
    .expect_err("running 会话不能修正历史");
    assert_code(&not_finished, "DOMAIN_ERROR");

    // `retime` 缺一个时刻 ⇒ `DOMAIN_ERROR`（构造不出服务请求）。
    let half = commands::correct_impl(
        &mut state,
        shell.running.broadcaster(),
        request(
            report.session.row_version,
            "retime",
            Some(WALL - 30_000),
            None,
        ),
    )
    .expect_err("重定时必须同时给出起止");
    assert_code(&half, "DOMAIN_ERROR");

    // 取值域外的动作串 ⇒ `DOMAIN_ERROR`。
    let bad_action = commands::correct_impl(
        &mut state,
        shell.running.broadcaster(),
        request(report.session.row_version, "shift", Some(1), Some(2)),
    )
    .expect_err("取值域外的动作必须被拒");
    assert_code(&bad_action, "DOMAIN_ERROR");
}

/// `backfill`：新建一条 `finished` 的人工会话，**不启动计时、不伪造完成事件**。
#[test]
fn backfill_impl_records_a_finished_foreground_session_without_starting_a_timer() {
    let shell = launch();
    let mut state = shell.state();
    let events_before = shell.events().len();

    let report = commands::backfill_impl(
        &mut state,
        shell.running.broadcaster(),
        BackfillRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            started_at: WALL - 3_600_000,
            ended_at: WALL - 1_800_000,
        },
    )
    .unwrap();

    assert_eq!(report.session.task_id, "t1");
    assert_eq!(report.session.state, SessionState::Finished);
    assert_eq!(
        report.session.mode,
        SessionMode::Foreground,
        "人工只有 FOREGROUND"
    );
    assert_eq!(report.session.timer_kind, TimerKind::Stopwatch);
    assert_eq!(report.session.target_duration_ms, None);
    assert_eq!(report.session.started_at, WALL - 3_600_000);
    assert_eq!(report.session.ended_at, Some(WALL - 1_800_000));
    assert!(!report.session.needs_review);
    assert_eq!(report.interval.session_id, report.session.id);
    assert_eq!(report.interval.started_at, WALL - 3_600_000);
    assert_eq!(report.interval.duration_ms, Some(1_800_000));
    assert!(
        report.interval.sampled_end_wall_at.is_none(),
        "补录没有执行过程，不编采样点"
    );
    assert_eq!(report.interval.voided_at, None);
    assert!(!report.interval.needs_review);
    assert_eq!(report.data_epoch, shell.epoch);
    assert_eq!(report.revision, revision_of(&state));
    // 「不启动计时、不伪造完成事件」：任务没有被推成 `Doing`，也没有 `task_change` 行。
    assert_eq!(
        text_of(&state, "SELECT status FROM task WHERE id = 't1'"),
        "Ready"
    );
    assert_eq!(
        scalar(&state, "SELECT COUNT(*) FROM task_change"),
        0,
        "补录不是任务状态跃迁"
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT COUNT(*) FROM work_session WHERE state = 'running'"
        ),
        0
    );
    assert!(
        commands::timer_snapshot_impl(&mut state)
            .unwrap()
            .session_id
            .is_none(),
        "补录不装载协调器镜像"
    );
    assert_eq!(shell.events().len(), events_before + 1);
    assert_eq!(last_payload(&shell), serde_json::to_value(&report).unwrap());

    // 拒绝路径都零写入：结束时刻晚于现在 / 与已有人工区间重叠 / 任务不存在 / epoch 不匹配。
    let sessions = scalar(&state, "SELECT COUNT(*) FROM work_session");
    let future = commands::backfill_impl(
        &mut state,
        shell.running.broadcaster(),
        BackfillRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            started_at: WALL - 1_000,
            ended_at: WALL + 1_000,
        },
    )
    .expect_err("未来的「已发生工时」不是事实");
    assert_code(&future, "DOMAIN_ERROR");

    let overlap = commands::backfill_impl(
        &mut state,
        shell.running.broadcaster(),
        BackfillRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            // 起点落进刚补录的那一段里（端点相接不算重叠，所以必须真的相交）。
            started_at: WALL - 1_900_000,
            ended_at: WALL - 1_700_000,
        },
    )
    .expect_err("与已确认的人工时间重叠必须被拒");
    assert_code(&overlap, "DOMAIN_ERROR");

    let unknown = commands::backfill_impl(
        &mut state,
        shell.running.broadcaster(),
        BackfillRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "nope".to_string(),
            started_at: WALL - 7_200_000,
            ended_at: WALL - 7_100_000,
        },
    )
    .expect_err("不存在的任务必须被拒");
    assert_code(&unknown, "DOMAIN_ERROR");

    let stale = commands::backfill_impl(
        &mut state,
        shell.running.broadcaster(),
        BackfillRequest {
            expected_data_epoch: "66666666-6666-4666-8666-666666666666".to_string(),
            task_id: "t1".to_string(),
            started_at: WALL - 7_200_000,
            ended_at: WALL - 7_100_000,
        },
    )
    .expect_err("陈旧 epoch 必须被拒");
    assert_code(&stale, "DATA_EPOCH_MISMATCH");

    assert_eq!(
        scalar(&state, "SELECT COUNT(*) FROM work_session"),
        sessions,
        "被拒的补录一条都不写"
    );
    assert_eq!(shell.events().len(), events_before + 1);
}

/// `discard_session`：作废整次（全部区间软作废 + 会话 `discarded`），重复提交幂等。
#[test]
fn discard_session_impl_voids_the_whole_session_and_is_idempotent() {
    let shell = launch();
    let mut state = shell.state();
    let started = commands::start_timer_impl(
        &mut state,
        shell.running.broadcaster(),
        start_request(&shell, "t1"),
    )
    .unwrap();
    let session_id = started.snapshot.session_id.clone().unwrap();
    let version = started.snapshot.session_version.unwrap();
    let events_before = shell.events().len();

    let report = commands::discard_session_impl(
        &mut state,
        shell.running.broadcaster(),
        DiscardSessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            expected_row_version: version,
        },
    )
    .unwrap();

    assert_eq!(report.session.id, session_id);
    assert_eq!(report.session.state, SessionState::Discarded);
    assert!(!report.session.needs_review);
    assert_eq!(report.session.row_version, version + 1);
    assert_eq!(
        report.interval.session_id, session_id,
        "报告答「作废了哪一段」"
    );
    assert!(
        report.interval.voided_at.is_some(),
        "整次作废 = 全部区间软作废（不是 DELETE）"
    );
    assert_eq!(report.data_epoch, shell.epoch);
    assert_eq!(report.revision, revision_of(&state));
    assert_eq!(
        text_of(
            &state,
            &format!("SELECT state FROM work_session WHERE id = '{session_id}'")
        ),
        "discarded"
    );
    assert_eq!(
        scalar(
            &state,
            &format!(
                "SELECT COUNT(*) FROM work_interval WHERE session_id = '{session_id}'
                   AND voided_at IS NULL"
            )
        ),
        0,
        "没有留下未作废的区间"
    );
    // 协调器镜像停在 `discarded`（与 `finish` 停在 `finished` 同一口径）：
    // 计时区因此显示「已作废」，不会继续按 running 计暂计。
    let snapshot = commands::timer_snapshot_impl(&mut state).unwrap();
    assert_eq!(snapshot.session_id, Some(session_id.clone()));
    assert_eq!(snapshot.state, Some(SessionState::Discarded));
    // 一条广播。
    assert_eq!(shell.events().len(), events_before + 1);
    assert_eq!(last_payload(&shell), serde_json::to_value(&report).unwrap());

    // 幂等重复 ⇒ `Unchanged`：零写入、版本不动、不广播、不移动第一次的作废时刻。
    let revision = revision_of(&state);
    let voided_at = scalar(
        &state,
        &format!("SELECT voided_at FROM work_interval WHERE session_id = '{session_id}'"),
    );
    let again = commands::discard_session_impl(
        &mut state,
        shell.running.broadcaster(),
        DiscardSessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            expected_row_version: report.session.row_version,
        },
    )
    .unwrap();
    assert_eq!(again.revision, revision, "重复作废不前进版本");
    assert_eq!(again.session.state, SessionState::Discarded);
    assert_eq!(
        scalar(
            &state,
            &format!("SELECT voided_at FROM work_interval WHERE session_id = '{session_id}'")
        ),
        voided_at,
        "作废时刻是第一次那一刻"
    );
    assert_eq!(
        shell.events().len(),
        events_before + 1,
        "Unchanged 一条都不发：{:?}",
        shell.events()
    );

    // 旧版本 ⇒ `VERSION_CONFLICT`。
    let stale = commands::discard_session_impl(
        &mut state,
        shell.running.broadcaster(),
        DiscardSessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            expected_row_version: version,
        },
    )
    .expect_err("旧会话版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");

    // 没有任何区间的会话 ⇒ `DOMAIN_ERROR`（报告里的 `interval` 是必填的）。
    let run_id = current_run_id(&mut state);
    seed_session_without_intervals(&state, "s-empty", &run_id);
    let empty = commands::discard_session_impl(
        &mut state,
        shell.running.broadcaster(),
        DiscardSessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: "s-empty".to_string(),
            expected_row_version: 0,
        },
    )
    .expect_err("没有区间的会话无法作废");
    assert_code(&empty, "DOMAIN_ERROR");
    assert_eq!(
        text_of(
            &state,
            "SELECT state FROM work_session WHERE id = 's-empty'"
        ),
        "paused",
        "被拒 ⇒ 一行都不改"
    );
}

/// `transition_task`：任务跃迁 + 同事务联动会话；`cause` 字符串真的决定 `reopen` 的合法性。
#[test]
fn transition_task_impl_moves_the_task_and_ends_its_running_session() {
    let shell = launch();
    let mut state = shell.state();
    let started = commands::start_timer_impl(
        &mut state,
        shell.running.broadcaster(),
        start_request(&shell, "t1"),
    )
    .unwrap();
    let session_id = started.snapshot.session_id.clone().unwrap();
    let task_version = started.task_version;
    let events_before = shell.events().len();
    // `start` 自己也会写任务变更审计（`Ready` → `Doing`），所以只比**增量**。
    let changes_before = scalar(
        &state,
        "SELECT COUNT(*) FROM task_change WHERE task_id = 't1'",
    );

    let report = commands::transition_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TransitionTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            expected_row_version: task_version,
            target: "Done".to_string(),
            cause: "user".to_string(),
        },
    )
    .unwrap();

    assert_eq!(report.task.id, "t1");
    assert_eq!(report.task.status, TaskStatus::Done);
    assert_eq!(report.task.row_version, task_version + 1);
    assert_eq!(
        report.ended_sessions,
        vec![session_id.clone()],
        "完成任务 = 同事务结束它正在跑的会话"
    );
    assert!(report.paused_sessions.is_empty());
    assert_eq!(report.data_epoch, shell.epoch);
    assert_eq!(report.revision, revision_of(&state));
    assert_eq!(
        text_of(
            &state,
            &format!("SELECT state FROM work_session WHERE id = '{session_id}'")
        ),
        "finished"
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT COUNT(*) FROM task_change WHERE task_id = 't1'"
        ),
        changes_before + 1,
        "跃迁写一条任务变更审计（`start` 的那条也在计数里，所以比增量）"
    );
    assert_eq!(shell.events().len(), events_before + 1);
    assert_eq!(last_payload(&shell), serde_json::to_value(&report).unwrap());

    // 幂等：已经是 `Done` 且没有未结束会话 ⇒ `Unchanged`（不写审计、不加版本、不广播）。
    let revision = revision_of(&state);
    let noop = commands::transition_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TransitionTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            expected_row_version: report.task.row_version,
            target: "Done".to_string(),
            cause: "user".to_string(),
        },
    )
    .unwrap();
    assert_eq!(noop.revision, revision, "Unchanged 不前进版本");
    assert_eq!(noop.task.status, TaskStatus::Done);
    assert!(noop.ended_sessions.is_empty());
    assert_eq!(
        shell.events().len(),
        events_before + 1,
        "Unchanged 一条都不发：{:?}",
        shell.events()
    );

    // 终结态回 `Ready` 必须显式 `reopen`：`user` ⇒ 服务拒绝。
    let implicit = commands::transition_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TransitionTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            expected_row_version: noop.task.row_version,
            target: "Ready".to_string(),
            cause: "user".to_string(),
        },
    )
    .expect_err("终结态回 Ready 必须显式 reopen");
    assert_code(&implicit, "DOMAIN_ERROR");

    // 同一个请求换成 `reopen` ⇒ 成功。这条断言把 `cause` 的字符串映射钉在服务语义上：
    // 把 "reopen" 解析成 `TransitionCause::User` 就会在这里红。
    let reopened = commands::transition_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TransitionTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            expected_row_version: noop.task.row_version,
            target: "Ready".to_string(),
            cause: "reopen".to_string(),
        },
    )
    .unwrap();
    assert_eq!(reopened.task.status, TaskStatus::Ready);
    assert!(reopened.revision > revision);
    assert_eq!(shell.events().len(), events_before + 2);

    // 跃迁表之外的组合（`Inbox` → `Doing`）⇒ 服务拒绝。
    let illegal = commands::transition_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TransitionTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t2".to_string(),
            expected_row_version: 0,
            target: "Doing".to_string(),
            cause: "user".to_string(),
        },
    )
    .expect_err("Inbox 不能直接 Doing");
    assert_code(&illegal, "DOMAIN_ERROR");

    // 取值域外的目标状态 / 原因 ⇒ `DOMAIN_ERROR`（命令层显式解析）。
    let bad_target = commands::transition_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TransitionTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t2".to_string(),
            expected_row_version: 0,
            target: "doing".to_string(),
            cause: "user".to_string(),
        },
    )
    .expect_err("状态串大小写不符就是取值域外");
    assert_code(&bad_target, "DOMAIN_ERROR");

    let bad_cause = commands::transition_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TransitionTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t2".to_string(),
            expected_row_version: 0,
            target: "Ready".to_string(),
            cause: "because".to_string(),
        },
    )
    .expect_err("取值域外的原因必须被拒");
    assert_code(&bad_cause, "DOMAIN_ERROR");

    // 旧任务版本 ⇒ `VERSION_CONFLICT`。
    let stale = commands::transition_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TransitionTaskRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            expected_row_version: task_version,
            target: "Cancelled".to_string(),
            cause: "user".to_string(),
        },
    )
    .expect_err("旧任务版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");
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

    let started = commands::start_timer_impl(
        &mut state,
        shell.running.broadcaster(),
        start_request(&shell, "t1"),
    )
    .unwrap();
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
        shell.running.broadcaster(),
        StartTimerRequest {
            mode: "FOCUS".to_string(),
            ..start_request(&shell, "t3")
        },
    )
    .expect_err("非法模式必须被拒");
    assert_code(&bad_mode, "DOMAIN_ERROR");

    let bad_kind = commands::start_timer_impl(
        &mut state,
        shell.running.broadcaster(),
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

    let started = commands::start_timer_impl(
        &mut state,
        shell.running.broadcaster(),
        start_request(&shell, "t1"),
    )
    .unwrap();
    let session_id = started.snapshot.session_id.clone().unwrap();
    let version = started.snapshot.session_version.unwrap();

    let paused = commands::pause_timer_impl(
        &mut state,
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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

    let started = commands::start_timer_impl(
        &mut state,
        shell.running.broadcaster(),
        start_request(&shell, "t1"),
    )
    .unwrap();
    let session_id = started.snapshot.session_id.clone().unwrap();
    let paused = commands::pause_timer_impl(
        &mut state,
        shell.running.broadcaster(),
        SessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            session_expected_version: started.snapshot.session_version.unwrap(),
        },
    )
    .unwrap();

    let resumed = commands::resume_timer_impl(
        &mut state,
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
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

    let started = commands::start_timer_impl(
        &mut state,
        shell.running.broadcaster(),
        start_request(&shell, "t1"),
    )
    .unwrap();
    let session_id = started.snapshot.session_id.clone().unwrap();

    let finished = commands::finish_timer_impl(
        &mut state,
        shell.running.broadcaster(),
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
        shell.running.broadcaster(),
        SessionRequest {
            expected_data_epoch: shell.epoch.clone(),
            session_id: session_id.clone(),
            session_expected_version: started.snapshot.session_version.unwrap(),
        },
    )
    .expect_err("旧会话版本必须被拒");
    assert_code(&stale, "VERSION_CONFLICT");
}

// ─────────────────────────────────────────────────────────────────────────────
// `domain.changed` 的发送侧（P7 Task 1 fix round 1）
// ─────────────────────────────────────────────────────────────────────────────

/// 一次真的改了库的写命令 ⇒ **恰好一条** `domain.changed`，载荷就是那次响应。
///
/// 信封里的 `data_epoch`/`revision` 必须与响应一致（前端据此使缓存失效），
/// `at` 来自这次命令的时钟采样。
#[test]
fn a_changed_write_broadcasts_exactly_one_domain_changed_with_the_response_as_payload() {
    let shell = launch();
    let mut state = shell.state();

    let change = commands::create_project_impl(
        &mut state,
        shell.running.broadcaster(),
        CreateProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            name: "项目三".to_string(),
        },
    )
    .unwrap();

    let events = shell.events();
    assert_eq!(events.len(), 1, "一次业务写恰好一条通知：{events:?}");
    let event = &events[0];
    assert_eq!(event.event, "domain.changed");
    assert_eq!(event.revision, change.revision);
    assert_eq!(event.data_epoch, change.data_epoch);
    assert_eq!(event.at, WALL, "at 来自这次命令的时钟采样");
    assert_eq!(
        event.payload,
        serde_json::to_value(&change).unwrap(),
        "载荷就是这次命令的响应 DTO"
    );
}

/// 幂等重复（零写入、revision 不动）⇒ **一条都不发**。
///
/// 这是「仅 `Changed` 时广播」的对手断言：把条件写成「总是广播」，本用例必红。
#[test]
fn an_idempotent_write_broadcasts_nothing() {
    let shell = launch();
    let mut state = shell.state();

    // 第一次打标：真的改了库 ⇒ 一条。
    commands::tag_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TaskTagRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            tag_id: "tag1".to_string(),
        },
    )
    .unwrap();
    assert_eq!(shell.events().len(), 1);

    // 第二次打标（同任务、同标签）⇒ Unchanged：不写库、不加 revision、不广播。
    let revision = revision_of(&state);
    let again = commands::tag_task_impl(
        &mut state,
        shell.running.broadcaster(),
        TaskTagRequest {
            expected_data_epoch: shell.epoch.clone(),
            task_id: "t1".to_string(),
            tag_id: "tag1".to_string(),
        },
    )
    .unwrap();
    assert_eq!(again.revision, revision);
    assert_eq!(
        shell.events().len(),
        1,
        "幂等重复不得再发通知：{:?}",
        shell.events()
    );

    // 改成同名的项目也是幂等重复。
    let same = commands::rename_project_impl(
        &mut state,
        shell.running.broadcaster(),
        RenameProjectRequest {
            expected_data_epoch: shell.epoch.clone(),
            project_id: "p1".to_string(),
            expected_row_version: 0,
            name: "项目一".to_string(),
        },
    )
    .unwrap();
    assert_eq!(same.revision, revision);
    assert_eq!(
        shell.events().len(),
        1,
        "改成同名同样不广播：{:?}",
        shell.events()
    );
}

/// 广播失败 ⇒ 命令仍然成功、业务已提交、诊断计数 +1（00 §4）。
#[test]
fn a_failed_broadcast_keeps_the_command_successful_and_counts_a_diagnostic() {
    let failing = Arc::new(FailingSink::default());
    let (_dir, running) = launch_with(Arc::clone(&failing) as Arc<dyn EventSink>);
    let epoch = running.data_epoch().to_string();
    let broadcaster = Arc::clone(running.broadcaster());
    let mut state = lock_app(running.app());
    let before = revision_of(&state);

    let change = commands::create_project_impl(
        &mut state,
        running.broadcaster(),
        CreateProjectRequest {
            expected_data_epoch: epoch,
            name: "项目三".to_string(),
        },
    )
    .expect("广播失败不得让命令失败");

    assert_eq!(change.revision, before + 1, "业务已经提交");
    assert_eq!(
        scalar(&state, "SELECT COUNT(*) FROM project WHERE name = '项目三'"),
        1,
        "已提交的业务不会被广播失败回滚"
    );
    assert!(failing.calls.load(Ordering::SeqCst) >= 1, "出口确实被调过");
    assert!(
        broadcaster.diagnostics().failed >= 1,
        "失败被记成诊断：{:?}",
        broadcaster.diagnostics()
    );
    assert_eq!(broadcaster.diagnostics().sent, 0, "这条出口一次都没成功过");
}
