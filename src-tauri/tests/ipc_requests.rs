//! IPC **请求形状**与非法参数的稳定错误码（P7 Task 1）。
//!
//! 响应形状由 `tests/ipc_snapshots.rs` 逐字节钉住；这里钉的是**入参**这一半：
//!
//! 1. `mode` / `timer_kind` 是字符串，非法取值 ⇒ `DOMAIN_ERROR`
//!    （不依赖 serde 的枚举反序列化：那种失败拿不到 `ErrorResponse.code`）；
//! 2. `TaskQueryRequest` 的三值项目选择器与它文档里写的 JSON 形状一致，
//!    并且真的走到**唯一那条读路径** `catalog::list_tasks_filtered`；
//! 3. 非法状态串与越界分页 ⇒ 领域错误**且零写入**（`total_changes()` 不动）。
//!
//! 断言只认 `code()` 与 `detail()`：`message()` 的模板 `操作不被允许：{detail}`
//! 自带中文，对 `DOMAIN_ERROR` 恒真（`tests/task_filters.rs` 头注释记过这个坑）。
//!
//! 建库样板照 `tests/transaction_boundary.rs::bootstrap`。

use worktrace_lib::commands::StartTimerRequest;
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::error::AppError;
use worktrace_lib::services::catalog::{self, TaskQueryRequest};
use worktrace_lib::services::timer::coordinator::{parse_session_mode, parse_timer_kind};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::task_repo::{self, ProjectFilter};
use worktrace_lib::storage::WriteOutcome;

// ─────────────────────────────────────────────────────────────────────────────
// 夹具
// ─────────────────────────────────────────────────────────────────────────────

struct Fixture {
    _dir: tempfile::TempDir,
    db: Db,
    epoch: String,
}

/// 临时库：一个 run + 一个项目 + 一个 Ready 任务（走仓储入口，与生产同形）。
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
    task_repo::create_task(&tx, "t1", "任务一", Some("p1"), 1000).unwrap();
    task_repo::transition_task(&tx, "t1", 0, TaskStatus::Ready, TransitionCause::User, 1000)
        .unwrap();
    tx.commit().unwrap();

    Fixture {
        _dir: dir,
        db,
        epoch: meta.data_epoch,
    }
}

impl Fixture {
    /// App 那条连接上的累计写入行数（`total_changes()` 是连接级计数）。
    fn total_changes(&self) -> i64 {
        self.db
            .connection()
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap()
    }

    fn revision(&self) -> i64 {
        self.db
            .connection()
            .query_row(
                "SELECT revision FROM app_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}

/// 按文档里的形状构造一个请求：字段一个不少（IPC 侧就是这么发的）。
fn request(statuses: &[&str], project: &str, limit: i64, offset: i64, epoch: &str) -> String {
    format!(
        r#"{{"statuses":{},"project":{project},"context_tag_id":null,"limit":{limit},"offset":{offset},"expected_data_epoch":"{epoch}"}}"#,
        serde_json::to_string(statuses).unwrap()
    )
}

/// 领域错误：断言码 + 具体中文理由（`detail` 就是用户文案）。
fn assert_domain_error(error: &AppError, needle: &str) {
    assert_eq!(error.code(), "DOMAIN_ERROR", "实际：{error:?}");
    let detail = error.detail().unwrap_or_default();
    assert!(
        detail.contains(needle),
        "拒绝理由里应当说清是什么不合法（要找 {needle:?}）：{detail}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 字符串枚举：非法取值 ⇒ 稳定错误码
// ─────────────────────────────────────────────────────────────────────────────

/// `mode` / `timer_kind` 的非法取值拿到 `DOMAIN_ERROR`，而不是 panic 或
/// Tauri 的反序列化错误。
#[test]
fn unknown_mode_and_timer_kind_are_domain_errors() {
    for raw in ["FOCUS", "foreground", "pomodoro", ""] {
        let error = parse_session_mode(raw).expect_err("非法模式必须被拒");
        assert_domain_error(&error, "会话模式");
    }
    for raw in ["Pomodoro", "STOPWATCH", "", "计时"] {
        let error = parse_timer_kind(raw).expect_err("非法计时类型必须被拒");
        assert_domain_error(&error, "计时类型");
    }

    // 取值域本身：大小写敏感、允许首尾空白（与 schema 的 CHECK 同口径）。
    assert_eq!(
        parse_session_mode(" FOREGROUND ").unwrap().as_str(),
        "FOREGROUND"
    );
    assert_eq!(parse_timer_kind("countdown").unwrap().as_str(), "countdown");
    assert!(parse_session_mode("BACKGROUND").is_ok());
    assert!(parse_session_mode("PASSIVE").is_ok());
    assert!(parse_session_mode("WAITING").is_ok());
}

/// `start` 请求里 `expected_interval_ms` 省略时按本进程的采样节拍取值。
///
/// 它只用于识别挂起，不是「多久记一次工时」：客户端不该猜、也不该被迫传。
#[test]
fn start_timer_request_defaults_the_sampling_interval() {
    let raw = r#"{"expected_data_epoch":"e","task_id":"t1","task_expected_version":0,
                  "mode":"FOREGROUND","timer_kind":"stopwatch","target_duration_ms":null}"#;
    let request: StartTimerRequest = serde_json::from_str(raw).unwrap();
    assert_eq!(
        request.expected_interval_ms,
        worktrace_lib::services::bootstrap::DEFAULT_SAMPLING_INTERVAL_MS as i64
    );

    let explicit: StartTimerRequest =
        serde_json::from_str(&raw.replace("\"mode\"", "\"expected_interval_ms\":250,\"mode\""))
            .unwrap();
    assert_eq!(explicit.expected_interval_ms, 250);
}

// ─────────────────────────────────────────────────────────────────────────────
// TaskQueryRequest：三值选择器、非法状态串、越界分页
// ─────────────────────────────────────────────────────────────────────────────

/// 三值项目选择器的 JSON 形状就是文档里写的那三个，且真的传到了读路径。
#[test]
fn the_three_valued_project_selector_reaches_the_read_path() {
    let f = bootstrap();

    for (json, expected) in [
        ("\"any\"", ProjectFilter::Any),
        ("\"none\"", ProjectFilter::None),
        ("{\"id\":\"p1\"}", ProjectFilter::Id("p1".to_string())),
    ] {
        let raw = request(&["Ready"], json, 10, 0, &f.epoch);
        let query =
            catalog::TaskQuery::try_from(serde_json::from_str::<TaskQueryRequest>(&raw).unwrap())
                .unwrap();
        assert_eq!(query.filter.project, expected, "形状 {json}");

        let result = catalog::list_tasks_filtered(&f.db, query).unwrap();
        let expected_tasks = if expected == ProjectFilter::None {
            0
        } else {
            1
        };
        assert_eq!(result.total, expected_tasks, "形状 {json} 的命中数");
        assert_eq!(result.tasks.len(), expected_tasks as usize);
        assert_eq!(result.data_epoch, f.epoch);
    }
}

/// 非法状态串：在**开事务之前**就被拒，库里一行不动。
#[test]
fn an_unknown_status_string_is_rejected_without_touching_the_database() {
    let f = bootstrap();
    let changes = f.total_changes();
    let revision = f.revision();

    // 第二个值非法：前一个值已经解析成功也不该放行。
    let raw = request(&["Ready", "Nope"], "\"any\"", 10, 0, &f.epoch);
    let query =
        catalog::TaskQuery::try_from(serde_json::from_str::<TaskQueryRequest>(&raw).unwrap())
            .expect_err("非法状态串必须被拒");
    assert_domain_error(&query, "任务状态");

    assert_eq!(f.total_changes(), changes, "被拒的请求不写库");
    assert_eq!(f.revision(), revision, "被拒的请求不加 revision");
}

/// 越界分页：由读路径的**唯一**那处规则（`task_repo::require_page`）拒绝，
/// 同样是领域错误且零写入。
#[test]
fn out_of_range_paging_is_rejected_by_the_read_path_and_writes_nothing() {
    let f = bootstrap();
    let changes = f.total_changes();

    for (limit, offset, needle) in [
        (0_i64, 0_i64, "每页条数"),
        (101, 0, "每页条数"),
        (10, -1, "跳过条数"),
    ] {
        let raw = request(&["Ready"], "\"any\"", limit, offset, &f.epoch);
        let query =
            catalog::TaskQuery::try_from(serde_json::from_str::<TaskQueryRequest>(&raw).unwrap())
                .expect("分页越界不在这里拒绝：规则只有一处（require_page）");
        let error = catalog::list_tasks_filtered(&f.db, query).expect_err("越界分页必须被拒");
        assert_domain_error(&error, needle);
    }

    assert_eq!(f.total_changes(), changes, "被拒的查询不写库");
}

/// 空状态集合不限制状态（与 `TaskFilter` 同义），上限 100 是合法的。
#[test]
fn an_empty_status_set_means_no_status_filter() {
    let f = bootstrap();
    let raw = request(&[], "\"any\"", 100, 0, &f.epoch);
    let query =
        catalog::TaskQuery::try_from(serde_json::from_str::<TaskQueryRequest>(&raw).unwrap())
            .unwrap();
    assert!(query.filter.statuses.is_empty());

    let result = catalog::list_tasks_filtered(&f.db, query).unwrap();
    assert_eq!(result.total, 1, "空集合 = 整表");
}

/// 旧 epoch：读路径拒绝，且**不开写事务**（这条与写命令的信封守卫同码不同路）。
#[test]
fn a_stale_epoch_is_rejected_by_the_query_guard() {
    let f = bootstrap();
    let raw = request(&["Ready"], "\"any\"", 10, 0, "另一个库");
    let query =
        catalog::TaskQuery::try_from(serde_json::from_str::<TaskQueryRequest>(&raw).unwrap())
            .unwrap();
    let error = catalog::list_tasks_filtered(&f.db, query).expect_err("旧 epoch 必须被拒");
    assert_eq!(error.code(), "DATA_EPOCH_MISMATCH");
}

/// `WriteOutcome` 的两个分支给命令层同一个载荷（`into_value`）。
///
/// 命令层不能写出 `storage::` 的名字（分层门禁），所以它只能这样取值；
/// 这条顺带钉住「两个分支都不 panic、都给出那份值」。
#[test]
fn write_outcome_hands_the_payload_to_the_command_layer() {
    assert_eq!(WriteOutcome::Changed(7).into_value(), 7);
    assert_eq!(WriteOutcome::Unchanged(7).into_value(), 7);
}
