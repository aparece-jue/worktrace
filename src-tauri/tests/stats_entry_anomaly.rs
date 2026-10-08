//! **采样异常下的统计 / 导出入口**（2026-10-08 跨阶段复审的最后一项）。
//!
//! # 这个文件在补哪一层
//!
//! 协调器级的回归只钉得住 `Coordinator::stats_sample` **自己**：
//! `tests/timer_regressions.rs::old_running_session_is_isolated_before_sampling_with_or_without_new_anchor`
//! 与 `::nonrunning_monotonic_failure_cannot_return_statistics` 直接调协调器，断言
//! `RECOVERY_REQUIRED`、版本不动、审计不增。它们对 **`AppState` 的包装**一个字都没说
//! ——而 P8 的 IPC 层真正调的是包装（`stats_today` / `export_json` /
//! `export_weekly_markdown`）。包装里少一个 `?`、或者在失败后退化成 Ok、再采一次、
//! 拿旧样本凑一个视图，协调器级的绿全都挡不住。
//!
//! 复审把「统计 / 导出只读」订正为：**正常采样只读；采样发现异常时走 P2 的异常路径
//! ——可能提交恢复事务及其审计（幂等分支与硬故障回滚分支零写入），随后返回恢复错误。**
//! 本文件按这句的**两个半边**各写一条用例，都在**入口层**（真 `startup()` + 真
//! `AppState` + 假时钟）：
//!
//! | 用例 | 分支 | 库的可见结果 |
//! | --- | --- | --- |
//! | [`a_hard_fault_isolates_every_statistics_entry_with_zero_writes`] | 硬故障 / 隔离 | **零写入**（版本、`total_changes`、审计、区间全部不动） |
//! | [`a_wall_clock_jump_commits_exactly_one_recovery_transaction_for_every_entry`] | 墙钟异常且正在计时 | 恢复事务**落库一次**（版本恰好 +1），其后幂等零写入 |
//!
//! # 断言口径
//!
//! 每个断言写**具体数值**：毫秒、版本号、`total_changes` 的增量、行数、审计 JSON 的
//! 键值、区间的起止与 `duration_ms`。不用「非空」「有变化」代替。
//!
//! # 装置
//!
//! 与 `tests/stats_end_to_end.rs` 同一姿势：真 `startup()`（单实例 → 开库迁移 → 建本次
//! run → 恢复扫描 → 协调器），假时钟与协调器**共享**一个 `Arc`，于是「推进时间」始终
//! 走平台时钟接缝。采样节拍放到 1 分钟（与 `tests/today.rs` 的装置二同一手法）：周期
//! 线程就不会插在用例编排的两个时刻之间，红与绿都可复现。
//!
//! **不写生产代码、不改错误码**：这里只观察「入口有没有把协调器的恢复语义原样交出来」。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::export::WeeklyQuery;
use worktrace_lib::services::stats::{
    Measure, MeasureColumn, StatsClass, StatsRangeQuery, TodayQuery, TodayView,
};
use worktrace_lib::services::timer::anchor::SampleVerdict;
use worktrace_lib::services::timer::coordinator::{SessionRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::require_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::session_repo::{self, IntervalRow, SessionRow};

// ─────────────────────────────────────────────────────────────────────────────
// 时间的来历与判据常数
// ─────────────────────────────────────────────────────────────────────────────

/// 查询时区：与全部日界 / 周界同一套。
const TZ: &str = "Asia/Shanghai";
/// 用例的挂钟原点：2026-03-10 12:00（+08:00）。`FakeClock` 不自己走 ⇒ 全部采样都是它加偏移。
const WALL: i64 = 1_773_115_200_000;
/// 今天 2026-03-10 00:00 —— `export_json` 的半开起点（数字都落在今天）。
const TUE_MID: i64 = 1_773_072_000_000;
/// 明天 2026-03-11 00:00 —— 半开终点。
const WED_MID: i64 = 1_773_158_400_000;

/// 真起一次计时后走满 1 分钟：**可信前缀与已确认人工都是这个数**。
const ONE_MINUTE: i64 = 60_000;
/// 硬故障那一拍的单调钟倒刻度数（`AnchorState::observe` 只要求 `d_mono < 0`）。
const MONOTONIC_SETBACK: i64 = -100;
/// 拨给墙钟的越界量。判据是「两差任一绝对值**严格大于** 2000ms」
/// （`platform::clock::THRESHOLD_MS`）：2000 恰好**不**算跳变
/// （边界见 `tests/export_json.rs::the_generated_time_is_the_wall_clock_and_not_the_data_watermark`），
/// 所以这里用 5000。调用点同时推进单调钟 1000ms，于是 `delta_gap` 正好是它。
const WALL_JUMP: i64 = 5_000;
/// 每次撞异常时单调钟额外走的 1000ms：让「余段的候选终点」严格晚于可信前缀终点，
/// 于是待确认候选真是一条**有端点**的区间（否则零长度余段会被省略）。
const MONOTONIC_STEP: i64 = 1_000;
/// 一次「running + 墙钟异常」的恢复事务写下的行数：
/// ① 原区间在检查点处闭合（UPDATE `work_interval`）
/// ② 余段待确认候选（INSERT `work_interval`）
/// ③ 异常审计（INSERT `time_edit`）
/// ④ 会话置 `recovering` + `needs_review`（UPDATE `work_session`）
/// ⑤ 恰好一次版本（UPDATE `app_meta`）
const RECOVERY_TX_ROWS: i64 = 5;

// ─────────────────────────────────────────────────────────────────────────────
// 装置
// ─────────────────────────────────────────────────────────────────────────────

/// 忽略全部事件的出口：本文件不关心广播，但 `startup` 需要一个。
struct NoSink;

impl EventSink for NoSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
    /// 第二次启动用的锁文件：硬故障的唯一出口是**新 run**（同一个库、另一个锁），
    /// 第一条用例才拿得到「下一次成功的读」。
    rebuilt_lock_path: PathBuf,
    /// 与协调器**共享**的假时钟：`startup` 拿走一个 `Arc` 句柄，用例留一个。
    clock: Arc<Mutex<FakeClock>>,
}

/// 建库 + 迁移 + 一个可开始计时的任务（`start` 需要它）。
///
/// 夹具里的裸 SQL 是**装置**，不是被测路径——与 `tests/today.rs` 的 `app_fixture`、
/// `tests/export_markdown.rs` 的 `app_fixture` 同一写法。
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let db = Db::open(&db_path).unwrap();
    migrate(db.connection()).unwrap();
    db.connection()
        .execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','写文档','Doing',0,1000,1000)",
            [],
        )
        .unwrap();
    drop(db);

    Fixture {
        db_path,
        lock_path: dir.path().join("instance.lock"),
        rebuilt_lock_path: dir.path().join("instance-rebuilt.lock"),
        clock: Arc::new(Mutex::new(FakeClock::new(WALL, 0))),
        _dir: dir,
    }
}

/// 真启动：单实例 → 开库/迁移 → 建本次 run → 恢复扫描 → 协调器 → 采样线程。
///
/// **采样节拍 1 分钟**：周期线程先睡后跑（`Scheduler::spawn`），1 分钟的间隔让它在用例
/// 期间一拍都不落，时刻编排完全由用例自己用 `sample_tick` 决定。
fn started(fx: &Fixture, lock_path: &Path) -> Box<RunningApp> {
    let config = StartupConfig {
        db_path: fx.db_path.clone(),
        lock_path: lock_path.to_path_buf(),
        sampling_interval_ms: 60_000,
    };
    let outcome = startup(
        config,
        Box::new(Arc::clone(&fx.clock)),
        Arc::new(NoSink),
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功");
    match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 三个（P8 会调的）包装入口：同一姿势各来一次
// ─────────────────────────────────────────────────────────────────────────────

/// 统计 / 导出的三个 `AppState` 包装。它们的成功类型各不相同（`TodayView` /
/// `ExportJson` / `ExportMarkdown`），这里统一抹成 `Result<(), AppError>`——本文件只问
/// 一件事：**恢复语义有没有被原样交出来**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Today,
    Json,
    Weekly,
}

const ENTRIES: [Entry; 3] = [Entry::Today, Entry::Json, Entry::Weekly];

impl Entry {
    fn name(self) -> &'static str {
        match self {
            Entry::Today => "stats_today",
            Entry::Json => "export_json",
            Entry::Weekly => "export_weekly_markdown",
        }
    }

    fn call(self, state: &mut AppState, epoch: &str) -> Result<(), AppError> {
        match self {
            Entry::Today => state.stats_today(&today_query(epoch)).map(|_| ()),
            // 范围给「今天」整天：统计异常在取数之前就发生，范围值与断言无关。
            Entry::Json => state.export_json(&range_query(epoch)).map(|_| ()),
            // `anchor` 省略 = 「本周」由同一次样本的水位决定（与 Today 的「今天」同一口径）。
            Entry::Weekly => state
                .export_weekly_markdown(&weekly_query(epoch))
                .map(|_| ()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求与只读探针
// ─────────────────────────────────────────────────────────────────────────────

fn today_query(epoch: &str) -> TodayQuery {
    TodayQuery {
        timezone: TZ.to_string(),
        expected_data_epoch: epoch.to_string(),
    }
}

fn range_query(epoch: &str) -> StatsRangeQuery {
    StatsRangeQuery {
        from: TUE_MID,
        to: WED_MID,
        timezone: TZ.to_string(),
        expected_data_epoch: epoch.to_string(),
    }
}

fn weekly_query(epoch: &str) -> WeeklyQuery {
    WeeklyQuery {
        timezone: TZ.to_string(),
        anchor: None,
        expected_data_epoch: epoch.to_string(),
    }
}

fn start_request(epoch: &str) -> StartRequest {
    StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".into(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    }
}

fn session_request(state: &mut AppState, epoch: &str) -> SessionRequest {
    let snapshot = state.snapshot().expect("查询快照");
    SessionRequest {
        expected_data_epoch: epoch.to_string(),
        session_id: snapshot.session_id.clone().expect("有活动会话"),
        session_expected_version: snapshot.session_version.expect("会话版本"),
    }
}

fn revision(db: &Db) -> i64 {
    require_meta(db.connection()).unwrap().revision
}

/// **连接级**累计写过的行数：抓得住 UPDATE，也抓得住回滚掉的写入尝试。
/// 探针必须取在 App **自己那条连接**上（新连接上恒为 0）。
fn total_changes(db: &Db) -> i64 {
    db.connection()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap()
}

fn audit_count(db: &Db) -> i64 {
    db.connection()
        .query_row("SELECT COUNT(*) FROM time_edit", [], |r| r.get(0))
        .unwrap()
}

fn audit_reason(db: &Db) -> String {
    db.connection()
        .query_row(
            "SELECT reason FROM time_edit ORDER BY created_at, id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn audit_after(db: &Db) -> Value {
    let raw: String = db
        .connection()
        .query_row(
            "SELECT after_json FROM time_edit ORDER BY created_at, id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).expect("审计的 after_json 必须是合法 JSON")
}

fn session_row(db: &Db, session_id: &str) -> SessionRow {
    session_repo::get_session(db.connection(), session_id)
        .unwrap()
        .expect("会话必须存在")
}

fn intervals_of(db: &Db, session_id: &str) -> Vec<IntervalRow> {
    session_repo::intervals_of_session(db.connection(), session_id).unwrap()
}

/// Today 里某一类某一 measure 的列（三类都固定四项，缺项以 `Some(0)` 出现）。
/// `Human` 那一列就是 F-010 的「确认人工工时」。
fn cell(view: &TodayView, class: StatsClass, measure: Measure) -> &MeasureColumn {
    view.column(class, measure)
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 硬故障 / 隔离路径：零写入
// ─────────────────────────────────────────────────────────────────────────────

/// **硬故障那一拍，三个入口都必须零写入地拒绝，且拒绝之后事实仍然自洽。**
///
/// 形状与协调器级的 `nonrunning_monotonic_failure_cannot_return_statistics` 同源：
/// 会话已经结束（`live` 停在 `finished`，不是 `running`），随后单调钟倒退 100ms。
/// `try_handle_anomaly` 在「非 running + 硬故障」这条路上**一个事务都不开**，只把协调器
/// 锁进故障态；`sample_and_detect` 随后用 `refuse_if_faulted` 把这一拍（以及之后每一拍）
/// 挡回去。**这一支没有任何恢复事务**——所以版本、`total_changes`、审计、区间全部不许动。
///
/// 三个入口都试：第一次调用正是「判出硬故障」的那一拍，后两次在 `read_sample` 的故障
/// 守卫上被挡住——两条路都不许写一行，也都不许把 `Err` 变成 Ok。
///
/// 收尾用**新 run**（同一个库、另一个锁文件）再读一次：那是硬故障唯一的出口
/// （「只能新 run 安全重建」，见 `Coordinator::refuse_if_faulted` 的文档），也是本用例
/// 「下一次成功的读仍然一致」的判据——读出来的必须还是那 1 分钟，而不是 0 或待确认。
#[test]
fn a_hard_fault_isolates_every_statistics_entry_with_zero_writes() {
    let fx = fixture();
    let running = started(&fx, &fx.lock_path);
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    // ① 真起一次计时：开始 → 走满 1 分钟（心跳落一枚检查点）→ 结束。
    {
        let mut state = lock_app(&app);
        state.start(start_request(&epoch)).expect("开始计时");
    }
    fx.clock.lock().unwrap().advance_both(ONE_MINUTE);
    {
        let mut state = lock_app(&app);
        state.sample_tick().expect("周期采样应当成功");
    }
    let sid = {
        let mut state = lock_app(&app);
        let request = session_request(&mut state, &epoch);
        state.finish(request).expect("结束计时");
        let live = state.coordinator().live().expect("镜像还在");
        assert_eq!(live.state, SessionState::Finished);
        assert!(
            live.open_interval.is_none(),
            "结束后没有开放区间：这正是「非 running」那一半"
        );
        live.id.clone()
    };
    let interval_id = {
        let state = lock_app(&app);
        let rows = intervals_of(state.db(), &sid);
        assert_eq!(rows.len(), 1, "结束后只有一条闭合区间");
        rows[0].id.clone()
    };

    let before_revision = {
        let state = lock_app(&app);
        revision(state.db())
    };
    let before_changes = {
        let state = lock_app(&app);
        total_changes(state.db())
    };

    // ② 硬故障：单调钟倒退 100ms。下一个采样入口会判 `MonotonicBackwards`。
    fx.clock
        .lock()
        .unwrap()
        .advance_monotonic(MONOTONIC_SETBACK);

    // ③ 三个入口各来一次，每次单独量写入。
    let mut deltas = Vec::new();
    for entry in ENTRIES {
        let call_revision = {
            let state = lock_app(&app);
            revision(state.db())
        };
        let call_changes = {
            let state = lock_app(&app);
            total_changes(state.db())
        };
        let result = {
            let mut state = lock_app(&app);
            entry.call(&mut state, &epoch)
        };
        assert_eq!(
            result.unwrap_err().code(),
            "RECOVERY_REQUIRED",
            "{} 不得把恢复语义咽下去（也不得退化成 Ok）",
            entry.name()
        );
        let after_revision = {
            let state = lock_app(&app);
            revision(state.db())
        };
        let after_changes = {
            let state = lock_app(&app);
            total_changes(state.db())
        };
        assert_eq!(
            after_revision,
            call_revision,
            "{} 在硬故障分支上不得加版本",
            entry.name()
        );
        assert_eq!(
            after_changes,
            call_changes,
            "{} 在硬故障分支上不得写任何一行",
            entry.name()
        );
        deltas.push(after_changes - call_changes);
    }
    assert_eq!(deltas, vec![0, 0, 0], "三个入口合计零写入");

    // ④ 隔离只在内存里；库里的既成事实一个字没变。
    {
        let state = lock_app(&app);
        assert!(
            state.coordinator().is_faulted(),
            "硬故障必须把协调器锁进故障态"
        );
        assert_eq!(
            state.coordinator().last_verdict(),
            SampleVerdict::MonotonicBackwards {
                d_mono_ms: MONOTONIC_SETBACK
            }
        );
        assert_eq!(
            state.coordinator().live().unwrap().state,
            SessionState::Finished,
            "镜像被换成了别的会话就是另一回事了"
        );

        assert_eq!(revision(state.db()), before_revision, "版本一动不动");
        assert_eq!(
            total_changes(state.db()),
            before_changes,
            "整个硬故障分支（含三次失败调用）一行都不写"
        );
        assert_eq!(audit_count(state.db()), 0, "硬故障分支不写审计");

        let row = session_row(state.db(), &sid);
        assert_eq!(row.state, SessionState::Finished);
        assert!(!row.needs_review, "没有待确认候选被造出来");
        assert_eq!(row.row_version, 1, "结束计时的版本位保持原样");

        let rows = intervals_of(state.db(), &sid);
        assert_eq!(rows.len(), 1, "既没有分割，也没有新的候选段");
        assert_eq!(rows[0].id, interval_id);
        assert_eq!(rows[0].started_at, WALL);
        assert_eq!(rows[0].ended_at, Some(WALL + ONE_MINUTE));
        assert_eq!(rows[0].duration_ms, Some(ONE_MINUTE));
        assert!(!rows[0].needs_review);
        assert!(rows[0].voided_at.is_none());
    }

    // ⑤ 新 run（硬故障唯一的出口）：同一个库、另一个锁文件。三个入口一行都没写，
    //    所以这一次成功的读必须报出同一分钟——不是 0，也不是待确认。
    drop(running);
    drop(app);
    let rebuilt = started(&fx, &fx.rebuilt_lock_path);
    let rebuilt_epoch = rebuilt.data_epoch().to_string();
    let rebuilt_app = Arc::clone(rebuilt.app());
    let view = {
        let mut state = lock_app(&rebuilt_app);
        state
            .stats_today(&today_query(&rebuilt_epoch))
            .expect("新 run 必须能报出既成事实")
    };
    assert_eq!(view.date, "2026-03-10");
    assert_eq!(
        cell(&view, StatsClass::Confirmed, Measure::Human).ms,
        Some(ONE_MINUTE),
        "已确认人工 = 那 1 分钟，一个毫秒都不能少"
    );
    assert_eq!(
        cell(&view, StatsClass::Confirmed, Measure::Human).intervals,
        1
    );
    assert_eq!(
        cell(&view, StatsClass::Confirmed, Measure::MachineBackground).ms,
        Some(0)
    );
    assert_eq!(
        cell(&view, StatsClass::Confirmed, Measure::Waiting).ms,
        Some(0)
    );
    assert_eq!(
        cell(&view, StatsClass::Live, Measure::Human).ms,
        Some(0),
        "没有在计时的区间"
    );
    assert_eq!(
        cell(&view, StatsClass::Pending, Measure::Human).intervals,
        0,
        "失败的三次调用没有留下待确认候选"
    );
    assert_eq!(
        view.revision, before_revision,
        "三次失败调用 + 一次新启动都没动业务版本"
    );
    assert_eq!(view.data_epoch, epoch, "同一个库身份");
    drop(rebuilt);
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 墙钟异常且正在计时：恢复事务真的落库，恰好一次
// ─────────────────────────────────────────────────────────────────────────────

/// **正在计时时撞上墙钟跳变：恢复事务必须落库一次、可见，且随后的调用幂等。**
///
/// 三次调用各自**再拨一次墙钟**，所以每一次都自己撞上异常：
/// - 第一次是「`running` + 墙钟异常」：`handle_anomaly` 走主路径——可信前缀在最后成功
///   检查点处闭合、余段成为待确认候选、写审计、会话置 `recovering`、`revision` 恰好 +1；
/// - 后两次是「`recovering` + 墙钟异常」：协调器的**幂等分支**，不重复分割、不重复写审计、
///   不加版本，仍然返回 `RECOVERY_REQUIRED`。
///
/// 于是「恰好一次版本 + 5 行写入」与「后两次零写入」是同一次运行里的两个观察：
/// 入口既没有藏住那笔写，也没有交出一个用异常前样本拼的视图（它返回的是 `Err`）。
///
/// 最后再各叫一次、**不拨钟**：异常已被检测但**未被接受**（`unaccepted_clock_correction`），
/// 统计入口继续按恢复语义拒绝，仍然一行都不写、版本不动。
#[test]
fn a_wall_clock_jump_commits_exactly_one_recovery_transaction_for_every_entry() {
    let fx = fixture();
    let running = started(&fx, &fx.lock_path);
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    // ① 开始计时，走满 1 分钟：心跳写下的检查点就是异常分割的**可信前缀终点**。
    {
        let mut state = lock_app(&app);
        state.start(start_request(&epoch)).expect("开始计时");
    }
    fx.clock.lock().unwrap().advance_both(ONE_MINUTE);
    {
        let mut state = lock_app(&app);
        state.sample_tick().expect("周期采样应当成功");
    }
    let (sid, open_interval_id) = {
        let state = lock_app(&app);
        let live = state.coordinator().live().expect("镜像还在");
        assert_eq!(live.state, SessionState::Running);
        let open = live.open_interval.clone().expect("running 必有开放区间");
        (live.id.clone(), open.0)
    };

    let before_revision = {
        let state = lock_app(&app);
        revision(state.db())
    };
    let before_changes = {
        let state = lock_app(&app);
        total_changes(state.db())
    };

    // ② 三个入口各来一次，每次之前都把墙钟拨过阈值。
    let mut deltas = Vec::new();
    for entry in ENTRIES {
        {
            let mut clock = fx.clock.lock().unwrap();
            clock.advance_monotonic(MONOTONIC_STEP);
            clock.advance_wall(WALL_JUMP + MONOTONIC_STEP);
        }
        let call_changes = {
            let state = lock_app(&app);
            total_changes(state.db())
        };
        let result = {
            let mut state = lock_app(&app);
            entry.call(&mut state, &epoch)
        };
        assert_eq!(
            result.unwrap_err().code(),
            "RECOVERY_REQUIRED",
            "{} 不得把恢复语义咽下去（也不得退化成 Ok）",
            entry.name()
        );
        let after_changes = {
            let state = lock_app(&app);
            total_changes(state.db())
        };
        deltas.push(after_changes - call_changes);
    }
    assert_eq!(
        deltas,
        vec![RECOVERY_TX_ROWS, 0, 0],
        "恢复事务只落一次（第一次入口），后两次是幂等分支：零写入"
    );

    // ③ 那笔恢复事务是**可见的**：版本恰好 +1，会话 `recovering`，余段是待确认候选。
    let after_revision = {
        let state = lock_app(&app);
        revision(state.db())
    };
    assert_eq!(
        after_revision,
        before_revision + 1,
        "一次异常恰好一次版本（不是 0，也不是 3）"
    );
    let written_total = {
        let state = lock_app(&app);
        total_changes(state.db()) - before_changes
    };
    assert_eq!(
        written_total, RECOVERY_TX_ROWS,
        "三次调用合计只写了那一笔恢复事务的 5 行"
    );

    {
        let state = lock_app(&app);

        // 审计：恰好一条，形状与 P2 的墙钟异常同源。
        assert_eq!(audit_count(state.db()), 1, "恰好一条异常审计");
        assert_eq!(audit_reason(state.db()), "clock jumped");
        let after = audit_after(state.db());
        assert_eq!(
            after["trusted_interval"].as_str(),
            Some(open_interval_id.as_str()),
            "可信前缀保留原区间 id"
        );
        assert_eq!(
            after["candidate_end"].as_i64(),
            Some(WALL + ONE_MINUTE + MONOTONIC_STEP),
            "候选终点取同一次样本的归属终点 A(M)，不是被拨过的墙钟"
        );
        assert_eq!(
            after["sampled_wall_at"].as_i64(),
            Some(WALL + ONE_MINUTE + WALL_JUMP + MONOTONIC_STEP),
            "原始墙钟读数照实记下来"
        );
        assert_eq!(
            after["sampled_monotonic_ms"].as_i64(),
            Some(ONE_MINUTE + MONOTONIC_STEP)
        );
        assert!(
            after["clock_correction_accepted"].as_bool().unwrap(),
            "墙钟异常那一笔审计同时记下「校正已被接受」"
        );

        // 会话：进入 recovering 并带待确认标记（版本位 0 → 1）。
        let row = session_row(state.db(), &sid);
        assert_eq!(row.state, SessionState::Recovering);
        assert!(row.needs_review, "会话带待确认事实");
        assert_eq!(row.row_version, 1);

        // 区间：可信前缀 + 待确认候选，两段端点相接、不重叠。
        let rows = intervals_of(state.db(), &sid);
        assert_eq!(rows.len(), 2, "一段可信前缀 + 一段待确认候选");
        assert_eq!(rows[0].id, open_interval_id);
        assert_eq!(rows[0].started_at, WALL);
        assert_eq!(rows[0].ended_at, Some(WALL + ONE_MINUTE));
        assert_eq!(
            rows[0].duration_ms,
            Some(ONE_MINUTE),
            "可信前缀是已确认的 1 分钟"
        );
        assert!(!rows[0].needs_review);
        assert_eq!(rows[1].started_at, WALL + ONE_MINUTE);
        assert_eq!(
            rows[1].ended_at,
            Some(WALL + ONE_MINUTE + MONOTONIC_STEP),
            "候选终点是归属终点"
        );
        assert_eq!(
            rows[1].duration_ms, None,
            "待确认候选没有 duration：不推算成工时"
        );
        assert!(rows[1].needs_review);
        assert_eq!(
            rows[1].sampled_end_wall_at,
            Some(WALL + ONE_MINUTE + WALL_JUMP + MONOTONIC_STEP)
        );

        // 内存镜像跟着事实走（不然快照会继续按异常前的状态出数）。
        assert_eq!(
            state.coordinator().live().unwrap().state,
            SessionState::Recovering
        );
        assert_eq!(
            state.coordinator().last_verdict(),
            SampleVerdict::Jumped {
                delta_gap_ms: WALL_JUMP
            }
        );
    }

    // ④ 再各叫一次、**不拨钟**：校正已被检测但未被接受，统计入口继续拒绝，仍然零写入。
    for entry in ENTRIES {
        let call_changes = {
            let state = lock_app(&app);
            total_changes(state.db())
        };
        let result = {
            let mut state = lock_app(&app);
            entry.call(&mut state, &epoch)
        };
        assert_eq!(
            result.unwrap_err().code(),
            "RECOVERY_REQUIRED",
            "{} 在「已检测未接受」的校正下也必须继续拒绝",
            entry.name()
        );
        let state = lock_app(&app);
        assert_eq!(
            total_changes(state.db()),
            call_changes,
            "{} 不得为未接受的校正写一行",
            entry.name()
        );
        assert_eq!(
            revision(state.db()),
            after_revision,
            "{} 不得为未接受的校正加版本",
            entry.name()
        );
    }

    drop(running);
}
