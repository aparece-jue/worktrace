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

use worktrace_lib::commands::{
    ArchiveProjectRequest, ClarifyReadyRequest, CreateProjectRequest, CreateTagRequest,
    CreateTaskRequest, EpochRequest, ListProjectsRequest, ListTagsRequest, PlanMutationRequest,
    RenameProjectRequest, SetTaskProjectRequest, StartTimerRequest, TaskTagRequest,
    TaskTagsRequest,
};
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::error::AppError;
use worktrace_lib::services::catalog::{self, ProjectSelector, ProjectTarget, TaskQueryRequest};
use worktrace_lib::services::daily_plan::DailyPlanQuery;
use worktrace_lib::services::timer::coordinator::{
    parse_session_mode, parse_timer_kind, ResumeRequest, SessionRequest,
};
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

/// `WriteOutcome::into_parts` 把载荷与「真的改了库吗」这一位一起交给命令层。
///
/// 命令层不能写出 `storage::` 的名字（分层门禁），所以它只能靠方法取值；
/// **两个分支都要钉住**：第二个返回值就是命令层的广播条件（评审 I1）——
/// `Changed` 才发 `domain.changed`，`Unchanged` 不发。
#[test]
fn write_outcome_hands_the_payload_and_the_changed_flag_to_the_command_layer() {
    assert_eq!(WriteOutcome::Changed(7).into_parts(), (7, true));
    assert_eq!(WriteOutcome::Unchanged(7).into_parts(), (7, false));
}

/// 改归属的二值 IPC 形状就是文档里写的那两个（P7 Task 1 fix round 1，评审 M4）。
///
/// 它与筛选用的三值 `ProjectSelector` 是**两个**枚举：那边有 `"any"`（不限制），
/// 这边没有——「这次不改归属」的调用方不该调这个命令。
#[test]
fn the_project_target_has_its_two_documented_json_shapes() {
    let bind: ProjectTarget = serde_json::from_str(r#"{"bind":"p1"}"#).unwrap();
    assert_eq!(bind, ProjectTarget::Bind("p1".to_string()));

    let clear: ProjectTarget = serde_json::from_str(r#""clear""#).unwrap();
    assert_eq!(clear, ProjectTarget::Clear);

    assert!(
        serde_json::from_str::<ProjectTarget>(r#""any""#).is_err(),
        "筛选用的三值形状不属于改归属"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 18 个请求 DTO 的 JSON 形状（P7 Task 1b fix round 1，评审 I1）
// ─────────────────────────────────────────────────────────────────────────────
//
// 响应那一半由 `tests/ipc_snapshots.rs` 逐字节钉住；**请求这一半原先没有任何机械
// 联系**：改一个 Rust 请求字段名，Rust 431 条绿、前端 17 条绿、`tsc` 也绿，
// 生产里表现为那条命令**每次调用**都退化成 `TRANSPORT_ERROR`（Tauri 的参数反序列化
// 失败拿不到 `ErrorResponse.code`）。下面按域分组，给**每个**请求 DTO 一份
// 「字段一个不少」的 JSON 样本。
//
// **为什么每条都要断言取值**（不是 `from_str(...).unwrap()` 就完事）：
// serde 默认**忽略未知字段**。一个 `Option<T>` 字段被改名之后，样本里那个旧键会被
// 静静丢掉、新键缺失回落成 `None`，`unwrap()` 照样成功——只有当测试真的去读
// `request.<字段>` 时，改名才会变成编译错误。必填字段缺失则由 `unwrap()` 当场炸。
//
// 另一侧的缺口（TS 写错、Rust 不知道）**没有**机械联系，只能人工对齐——
// 登记在 `docs/superpowers/plans/2026-10-03-p7-shell-and-ui.md` 的「遗留与边界」一节。

/// 握手与项目：`EpochRequest` / `ListProjectsRequest` / `CreateProjectRequest` /
/// `RenameProjectRequest` / `ArchiveProjectRequest`。
#[test]
fn handshake_and_project_request_dtos_take_the_documented_json() {
    let epoch: EpochRequest = serde_json::from_str(r#"{"expected_data_epoch":"e1"}"#).unwrap();
    assert_eq!(epoch.expected_data_epoch, "e1");

    let list: ListProjectsRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","status":"archived"}"#).unwrap();
    assert_eq!(list.expected_data_epoch, "e1");
    assert_eq!(
        list.status.as_deref(),
        Some("archived"),
        "读路径的取值域含 done"
    );
    let all: ListProjectsRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","status":null}"#).unwrap();
    assert_eq!(all.status, None, "null = 不限制状态");

    let create: CreateProjectRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","name":"项目一"}"#).unwrap();
    assert_eq!(create.expected_data_epoch, "e1");
    assert_eq!(create.name, "项目一");

    let rename: RenameProjectRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","project_id":"p1","expected_row_version":3,"name":"项目二"}"#,
    )
    .unwrap();
    assert_eq!(rename.expected_data_epoch, "e1");
    assert_eq!(rename.project_id, "p1");
    assert_eq!(rename.expected_row_version, 3);
    assert_eq!(rename.name, "项目二");

    let archive: ArchiveProjectRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","project_id":"p1","expected_row_version":3}"#,
    )
    .unwrap();
    assert_eq!(archive.expected_data_epoch, "e1");
    assert_eq!(archive.project_id, "p1");
    assert_eq!(archive.expected_row_version, 3);
}

/// 标签：`ListTagsRequest` / `CreateTagRequest` / `TaskTagRequest` / `TaskTagsRequest`。
#[test]
fn tag_request_dtos_take_the_documented_json() {
    let list: ListTagsRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","kind":"Context"}"#).unwrap();
    assert_eq!(list.expected_data_epoch, "e1");
    assert_eq!(
        list.kind.as_deref(),
        Some("Context"),
        "四类是大小写敏感的大写串"
    );
    let all: ListTagsRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","kind":null}"#).unwrap();
    assert_eq!(all.kind, None, "null = 全部四类");

    let create: CreateTagRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","kind":"Context","name":"在家","parent_id":null}"#,
    )
    .unwrap();
    assert_eq!(create.expected_data_epoch, "e1");
    assert_eq!(create.kind, "Context");
    assert_eq!(create.name, "在家");
    assert_eq!(create.parent_id, None, "V0.1 没有层级");

    let tag: TaskTagRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","task_id":"t1","tag_id":"tag1"}"#)
            .unwrap();
    assert_eq!(tag.expected_data_epoch, "e1");
    assert_eq!(tag.task_id, "t1");
    assert_eq!(tag.tag_id, "tag1");

    let of_task: TaskTagsRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","task_id":"t1"}"#).unwrap();
    assert_eq!(of_task.expected_data_epoch, "e1");
    assert_eq!(of_task.task_id, "t1");
}

/// 任务：`CreateTaskRequest` / `ClarifyReadyRequest` / `SetTaskProjectRequest` /
/// `TaskQueryRequest`。
#[test]
fn task_request_dtos_take_the_documented_json() {
    let create: CreateTaskRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","title":"任务一","project_id":"p1"}"#)
            .unwrap();
    assert_eq!(create.expected_data_epoch, "e1");
    assert_eq!(create.title, "任务一");
    assert_eq!(create.project_id.as_deref(), Some("p1"));

    let clarify: ClarifyReadyRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","task_id":"t1","expected_row_version":0}"#,
    )
    .unwrap();
    assert_eq!(clarify.expected_data_epoch, "e1");
    assert_eq!(clarify.task_id, "t1");
    assert_eq!(clarify.expected_row_version, 0);

    let project: SetTaskProjectRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","task_id":"t1","expected_row_version":0,"project":{"bind":"p1"}}"#,
    )
    .unwrap();
    assert_eq!(project.expected_data_epoch, "e1");
    assert_eq!(project.task_id, "t1");
    assert_eq!(project.expected_row_version, 0);
    assert_eq!(project.project, ProjectTarget::Bind("p1".to_string()));

    let query: TaskQueryRequest = serde_json::from_str(
        r#"{"statuses":["Ready","Doing"],"project":{"id":"p1"},"context_tag_id":"tag1",
            "limit":10,"offset":20,"expected_data_epoch":"e1"}"#,
    )
    .unwrap();
    assert_eq!(
        query.statuses,
        vec!["Ready".to_string(), "Doing".to_string()]
    );
    assert_eq!(query.project, ProjectSelector::Id("p1".to_string()));
    assert_eq!(query.context_tag_id.as_deref(), Some("tag1"));
    assert_eq!(query.limit, 10);
    assert_eq!(query.offset, 20);
    assert_eq!(query.expected_data_epoch, "e1");
}

/// 今日计划：`PlanMutationRequest` / `DailyPlanQuery`。
#[test]
fn daily_plan_request_dtos_take_the_documented_json() {
    let mutate: PlanMutationRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","task_id":"t1","date":"2026-10-04","timezone":"Asia/Shanghai"}"#,
    )
    .unwrap();
    assert_eq!(mutate.expected_data_epoch, "e1");
    assert_eq!(mutate.task_id, "t1");
    assert_eq!(mutate.date, "2026-10-04");
    assert_eq!(mutate.timezone, "Asia/Shanghai");

    let read: DailyPlanQuery = serde_json::from_str(
        r#"{"date":"2026-10-04","timezone":"Asia/Shanghai","expected_data_epoch":"e1"}"#,
    )
    .unwrap();
    assert_eq!(read.date, "2026-10-04");
    assert_eq!(read.timezone, "Asia/Shanghai");
    assert_eq!(read.expected_data_epoch, "e1");
}

/// 计时：`StartTimerRequest` / `SessionRequest` / `ResumeRequest`。
#[test]
fn timer_request_dtos_take_the_documented_json() {
    let start: StartTimerRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","task_id":"t1","task_expected_version":0,
            "mode":"FOREGROUND","timer_kind":"countdown","target_duration_ms":1500000,
            "expected_interval_ms":250}"#,
    )
    .unwrap();
    assert_eq!(start.expected_data_epoch, "e1");
    assert_eq!(start.task_id, "t1");
    assert_eq!(start.task_expected_version, 0);
    assert_eq!(start.mode, "FOREGROUND", "mode 是字符串，由命令体显式解析");
    assert_eq!(start.timer_kind, "countdown", "timer_kind 同理");
    assert_eq!(start.target_duration_ms, Some(1_500_000));
    assert_eq!(start.expected_interval_ms, 250);

    let session: SessionRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","session_id":"s1","session_expected_version":2}"#,
    )
    .unwrap();
    assert_eq!(session.expected_data_epoch, "e1");
    assert_eq!(session.session_id, "s1");
    assert_eq!(session.session_expected_version, 2);

    let resume: ResumeRequest = serde_json::from_str(
        r#"{"expected_data_epoch":"e1","task_id":"t1","task_expected_version":0,
            "session_id":"s1","session_expected_version":2}"#,
    )
    .unwrap();
    assert_eq!(resume.expected_data_epoch, "e1");
    assert_eq!(resume.task_id, "t1");
    assert_eq!(resume.task_expected_version, 0);
    assert_eq!(resume.session_id, "s1");
    assert_eq!(resume.session_expected_version, 2);
}

/// 必填字段缺失**必须当场被拒**（这条是上面那些样本的对手面）。
///
/// 同时把「serde 默认忽略未知字段」这个事实钉住：它正是上面每条都要断言取值、
/// 而不能只 `unwrap()` 的原因。
#[test]
fn a_request_missing_a_required_field_is_rejected() {
    /// 报错必须说清缺的是哪个字段。
    fn assert_missing(error: serde_json::Error, field: &str) {
        let text = error.to_string();
        assert!(
            text.contains("missing field") && text.contains(field),
            "报错要说清缺的是哪个字段（要找 {field:?}）：{text}"
        );
    }

    // 新建项目没有 name。
    let error = serde_json::from_str::<CreateProjectRequest>(r#"{"expected_data_epoch":"e1"}"#)
        .expect_err("缺 name 必须被拒");
    assert_missing(error, "name");

    // 改既有对象没有带版本。
    let error = serde_json::from_str::<RenameProjectRequest>(
        r#"{"expected_data_epoch":"e1","project_id":"p1","name":"项目二"}"#,
    )
    .expect_err("缺 expected_row_version 必须被拒");
    assert_missing(error, "expected_row_version");

    // 会话命令没有会话 id。
    let error = serde_json::from_str::<SessionRequest>(
        r#"{"expected_data_epoch":"e1","session_expected_version":2}"#,
    )
    .expect_err("缺 session_id 必须被拒");
    assert_missing(error, "session_id");

    // 未知字段被**忽略**（不是报错）⇒ 只 `unwrap()` 挡不住「字段改名」。
    let with_extra: ListProjectsRequest =
        serde_json::from_str(r#"{"expected_data_epoch":"e1","status":"active","typo":1}"#).unwrap();
    assert_eq!(with_extra.status.as_deref(), Some("active"));
}
