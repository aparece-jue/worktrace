//! P6 Task 5：本地优先与端到端验收（**服务级那一半**；实机那一半归 P8）。
//!
//! ## 这条链路证明什么
//!
//! 计划 Task 5 的端到端是
//! 「启动 → 计时 → 关掉全部窗口（核心与周期采样继续）→ 重开窗口立即拉快照 →
//!  崩溃 → 重启（走 P3 的四类判定）→ 备份 → 恢复（新 epoch、旧请求被拒）」。
//! 本文件用**真入口**（`services::bootstrap::startup` + `AppState` + 真库）把它串成**一条**
//! 用例，每一步都断言**具体数值**：`revision` / `data_epoch` / `run_id` 的变化、
//! `interval_checkpoint` 的行与列、归一出来的待确认候选、旧 epoch 请求的错误码与
//! 「四件事」（`revision` 不变、无新行、无审计、既有记录字段一致）。
//!
//! ## 窗口不是一个能在服务层模拟的对象
//!
//! 本进程里没有任何 `WebviewWindow`（`tests/periodic_sampling.rs` 的模块头已经把这条口径
//! 写死：采样驱动只认进程，窗口回调在启动时返回一次 `Ok(())` 之后就不存在了）。所以：
//!
//! - **「关掉全部窗口」**在本层的含义是**核心与采样与窗口解耦**——进程里零窗口引用，
//!   `interval_checkpoint` 照样按 30 秒心跳前进（F-009 的可断言半边）；窗口生命周期
//!   本身（`platform::window` / `should_prevent_exit`）由 P7 的用例覆盖。
//! - **「重开窗口立即拉快照」**在本层的含义是**冷启动握手拿到的快照与库里的权威事实逐字
//!   一致**（`get_revision` + `timer_snapshot` 两个命令体，00 §5 规则 1 的「先监听后快照」
//!   在服务侧的落点）；窗口对象、前端闸门与 `RevisionGate` 归 P7/P8。
//!
//! ## 不能宣称的（全部归 P8）
//!
//! 真实拔网线跑一遍 V0.1 功能、手动触发备份/恢复、界面拿到旧请求被拒、强杀后重启核对单实例、
//! 锁屏 30 分钟 / 休眠唤醒 / 正反改时——按 `docs/validation/pre-p6-closure.md` 第 10 条
//! **全部登记为待 P8**，本文件只登记、不声称（`manual_platform_verified` 保持 false）。
//! F-014 在本任务能证的是「**不依赖网络/AI 的服务链路**」：`the_v01_...` 那条用例逐条
//! 核对依赖面与源码面（零网络/AI 客户端）并把导出路径真跑一遍。
//!
//! ## 不重复既有的单点用例
//!
//! - `tests/recovery_end_to_end.rs`：崩溃 → 重启扫描 → 对账 → 修正/补录/作废；
//! - `tests/recovery_scan.rs`：四类判定各自的字段级结果；
//! - `tests/backup_restore.rs`：备份/恢复的 16 条单点用例（并发写不撕裂、维护态四件事、
//!   旧 epoch 被拒、回滚不继承旧基线、提交/回滚两条路径拒绝造库）。
//!
//! 本文件的职责是**把链路串起来**，不是把它们的断言再写一遍。
//!
//! ## 清理口径（别把用户的副本当垃圾）
//!
//! 每次成功恢复都会在库旁边留下一份 `<db>.restore-rollback`——那是**恢复之前那个世界的
//! 唯一完整副本**，计划刻意保留它（`RestoreOutcome::rollback` 就是给它报路径的出口）。
//! 本文件**不删它**，反而断言它还在、且内容正是恢复前的事实。测试目录由
//! `tempfile::TempDir` 在夹具析构时整体删除，删的是**测试自己**的临时目录，
//! 不是用户的 `%APPDATA%`；生产侧「保留多久、怎么清」留给 P8。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::OptionalExtension;

use worktrace_lib::commands;
use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::{Clock, FakeClock};
use worktrace_lib::services::backup::{backup_consistent, restore_from_backup, ClockSource};
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::export::WeeklyQuery;
use worktrace_lib::services::stats::{Measure, StatsClass, StatsRangeQuery, TodayQuery};
use worktrace_lib::services::tasks::TransitionTaskRequest;
use worktrace_lib::services::timer::coordinator::{SessionRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta;
use worktrace_lib::storage::migrations::{current_version, migrate, SCHEMA_VERSION};
use worktrace_lib::storage::session_repo::InvariantFault;

/// 假时钟的两个原点：整份文件共用，用例之间不依赖任何真实时间。
const WALL: i64 = 1_700_000_000_000;
const MONO: i64 = 5_000;

/// 采样节拍压到 10ms：链路里等心跳不必等真实的 30 秒。
const TICK_MS: u64 = 10;

/// P2 的检查点节拍（`services::timer::coordinator::HEARTBEAT_INTERVAL_MS`）。
///
/// 它同时是「长时间不可信间隔」阈值的基数（`3 ×` 这个值，见 `anchor.rs` 的规则⑤），
/// 所以本文件一次只推进 30 秒——两次推进都不会被误判成挂起。
const HEARTBEAT_MS: i64 = 30_000;

/// 查询时区：显式给（02 §9 的「今日」「本周」都按用户时区算），与机器时区无关。
///
/// `WALL` = `2023-11-14T22:13:20Z` = `2023-11-15 06:13:20 +08:00`；链路结束在
/// `WALL + 60_000` ⇒ 本地日 `2023-11-15`（周三），所在周 `2023-11-13`（周一）起。
const TZ: &str = "Asia/Shanghai";

// ─────────────────────────────────────────────────────────────────────────────
// 样板：真启动入口 + 一份共享的假时钟 + 记录型广播出口
// ─────────────────────────────────────────────────────────────────────────────

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

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
    backup_dir: PathBuf,
    /// 时钟只在用例推进时前进：心跳、检查点与归属终点因此全是确定值。
    clock: Arc<Mutex<FakeClock>>,
    sink: Arc<RecordingSink>,
}

impl Fixture {
    /// 临时目录 + 迁移好的库 + 元数据 + 一条 `Ready` 任务 `t1`。
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("worktrace.db");
        let lock_path = dir.path().join("instance.lock");
        let backup_dir = dir.path().join("backups");
        {
            let mut db = Db::open(&db_path).unwrap();
            migrate(db.connection()).unwrap();
            let tx = db.connection_mut().unchecked_transaction().unwrap();
            meta::init_meta(&tx).unwrap();
            // 两条任务：`t1` 是本用例全程计时的那条；`t2` 只用来承载"更早几次崩溃"
            // 留下的残骸——一条 `running` 却没有开放区间的损坏记录挂在 `t1` 上会挡住
            // 它的**正常**状态跃迁（跨 run 的 running 会话整体拒绝），那是另一回事。
            tx.execute(
                "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
                 VALUES('t1','任务一','Ready',0,1000,1000)",
                [],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
                 VALUES('t2','任务二','Ready',0,1001,1001)",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        Self {
            _dir: dir,
            db_path,
            lock_path,
            backup_dir,
            clock: Arc::new(Mutex::new(FakeClock::new(WALL, MONO))),
            sink: Arc::new(RecordingSink::default()),
        }
    }

    /// 走**真启动入口**起一次应用。第二次调用 = 崩溃之后的重启（单实例锁由
    /// `RunningApp` 持有，`drop` 即释放——内核在句柄关闭时放锁，与强杀同一语义）。
    fn start(&self) -> Box<RunningApp> {
        let mut config =
            StartupConfig::new(&self.db_path, &self.lock_path).with_backup_dir(&self.backup_dir);
        config.sampling_interval_ms = TICK_MS;
        match startup(
            config,
            Box::new(Arc::clone(&self.clock)),
            Arc::clone(&self.sink) as Arc<dyn EventSink>,
            &NoProbe,
            &|| -> Result<(), AppError> { Ok(()) },
        )
        .expect("启动应当成功")
        {
            Startup::Running(running) => running,
            Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
        }
    }

    /// 恢复流程的时钟来源。每次调用交出一只新的钟，但**共享同一份假时钟状态**
    /// ——与生产侧「同一个 `SystemClock` 的克隆」是同一件事（C-C 的时钟同源）。
    fn clock_source(&self) -> ClockSource {
        let shared = Arc::clone(&self.clock);
        Box::new(move || Box::new(Arc::clone(&shared)) as Box<dyn Clock + Send>)
    }

    /// **另一条**连接：所有「库里的权威事实」都从这里读，不拿被测的服务入口当 oracle。
    /// 用完即弃——长期持有的第二条连接会让 WAL 无法 checkpoint，
    /// 而恢复的最后一步是**同目录改名**（Windows 上文件被打开就改不动）。
    fn db(&self) -> Db {
        Db::open(&self.db_path).expect("第二条连接")
    }

    fn scalar(&self, sql: &str) -> i64 {
        self.db()
            .connection()
            .query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    fn scalar_opt(&self, sql: &str) -> Option<i64> {
        self.db()
            .connection()
            .query_row(sql, [], |row| row.get::<_, Option<i64>>(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    fn text(&self, sql: &str) -> String {
        self.db()
            .connection()
            .query_row(sql, [], |row| row.get::<_, String>(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    fn epoch(&self) -> String {
        meta::read_meta(self.db().connection())
            .unwrap()
            .expect("app_meta 已初始化")
            .data_epoch
    }

    fn revision(&self) -> i64 {
        meta::read_meta(self.db().connection())
            .unwrap()
            .expect("app_meta 已初始化")
            .revision
    }

    /// `interval_checkpoint` 的**唯一**一行（`interval_id` 是主键：一次会话的一个区间
    /// 只有一行，每次心跳**覆盖**它——所以"关窗期间采样继续"的证据是这一行的列值前进，
    /// 不是行数增长）。
    fn checkpoint(&self) -> Option<CheckpointFacts> {
        self.db()
            .connection()
            .query_row(
                "SELECT interval_id, run_id, wall_at, attribution_at, elapsed_ms
                   FROM interval_checkpoint",
                [],
                |row| {
                    Ok(CheckpointFacts {
                        interval_id: row.get(0)?,
                        run_id: row.get(1)?,
                        wall_at: row.get(2)?,
                        attribution_at: row.get(3)?,
                        elapsed_ms: row.get(4)?,
                    })
                },
            )
            .optional()
            .unwrap_or_else(|error| panic!("读检查点：{error}"))
    }

    /// 可数的「四件事」（第四件——既有记录字段一致——由用例点名断言具体行）。
    fn facts(&self) -> Facts {
        let db = self.db();
        let connection = db.connection();
        Facts {
            revision: meta::read_meta(connection).unwrap().unwrap().revision,
            tasks: count(connection, "task"),
            sessions: count(connection, "work_session"),
            intervals: count(connection, "work_interval"),
            checkpoints: count(connection, "interval_checkpoint"),
            runs: count(connection, "application_run"),
            task_changes: count(connection, "task_change"),
            time_edits: count(connection, "time_edit"),
        }
    }

    /// 备份目录里的产物名（目录还不存在 = 空集）。
    fn artifacts(&self) -> Vec<String> {
        let mut names: Vec<String> = match std::fs::read_dir(&self.backup_dir) {
            Ok(entries) => entries
                .map(|entry| {
                    entry
                        .expect("目录项读得到")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }

    /// 现做一份**当前库**的一致备份（生产原语 `VACUUM INTO`），返回产物路径。
    fn take_backup(&self) -> PathBuf {
        let db = self.db();
        let version = current_version(db.connection()).unwrap();
        backup_consistent(
            Some(&self.backup_dir),
            db.connection(),
            version,
            &*self.clock,
            "task5 chain backup",
        )
        .expect("备份应当成功")
    }

    /// 走真命令体写一条业务事实（与用户命令同一条串行边界）。
    fn create_task(&self, running: &RunningApp, title: &str) -> Result<(), AppError> {
        let app = Arc::clone(running.app());
        let mut state = lock_app(&app);
        commands::create_task_impl(
            &mut state,
            running.broadcaster(),
            commands::CreateTaskRequest {
                expected_data_epoch: self.epoch(),
                title: title.to_string(),
                project_id: None,
            },
        )
        .map(|_| ())
    }

    /// 走真命令体开始计时（`t1`），返回会话 id。
    fn start_timer(&self, running: &RunningApp) -> String {
        let app = Arc::clone(running.app());
        let mut state = lock_app(&app);
        let outcome = commands::start_timer_impl(
            &mut state,
            running.broadcaster(),
            commands::StartTimerRequest {
                expected_data_epoch: self.epoch(),
                task_id: "t1".to_string(),
                task_expected_version: 0,
                mode: "FOREGROUND".to_string(),
                timer_kind: "stopwatch".to_string(),
                target_duration_ms: None,
                expected_interval_ms: 1_000,
            },
        )
        .expect("开始计时");
        outcome.snapshot.session_id.expect("有会话")
    }

    /// 推一次假时钟（挂钟与单调钟一起走：不构成任何异常判定）。
    fn advance(&self, delta_ms: i64) {
        self.clock.lock().unwrap().advance_both(delta_ms);
    }

    /// 等心跳把检查点推进到 `elapsed_ms`。
    fn wait_for_checkpoint(&self, elapsed_ms: i64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.checkpoint().map(|c| c.elapsed_ms).unwrap_or(-1) < elapsed_ms {
            assert!(
                Instant::now() < deadline,
                "心跳没有把检查点推进到 {elapsed_ms}ms：当前 {:?}",
                self.checkpoint()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn count(connection: &rusqlite::Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|error| panic!("数 {table}：{error}"))
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "采样驱动没有在 10 秒内推进");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckpointFacts {
    interval_id: String,
    run_id: String,
    wall_at: i64,
    attribution_at: i64,
    elapsed_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Facts {
    revision: i64,
    tasks: i64,
    sessions: i64,
    intervals: i64,
    checkpoints: i64,
    runs: i64,
    task_changes: i64,
    time_edits: i64,
}

/// `AppState::start` 的请求（门禁关着时用它断言"新计时被拒且零写入"）。
fn start_request(epoch: &str, task_version: i64) -> StartRequest {
    StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".to_string(),
        task_expected_version: task_version,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 1_000,
    }
}

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 端到端链路（服务级）
// ─────────────────────────────────────────────────────────────────────────────

/// 启动 → 计时 → 关掉全部窗口（核心与周期采样继续）→ 重开窗口立即拉快照 → 崩溃 →
/// 重启（走 P3 的四类判定）→ 备份 → 恢复（新 epoch、旧请求被拒）。
///
/// 每一步的断言都在下面用「① … ⑧」标出；数值全部是确定值（假时钟 + 30 秒心跳）。
#[test]
fn the_service_chain_survives_windows_crash_restart_backup_and_restore() {
    let fx = Fixture::new();

    // ── ① 启动：新库、新 run、零恢复材料 ──────────────────────────────────
    let running = fx.start();
    let app = Arc::clone(running.app());
    let run1 = running.run_id().to_string();
    let epoch1 = running.data_epoch().to_string();
    assert_eq!(epoch1, fx.epoch(), "启动快照的 epoch 就是库里的那个");
    assert_eq!(fx.revision(), 0, "新库的初始 revision 是 0");
    assert_eq!(fx.scalar("SELECT COUNT(*) FROM application_run"), 1);
    assert_eq!(
        fx.scalar(&format!(
            "SELECT started_at FROM application_run WHERE id = '{run1}'"
        )),
        WALL,
        "run 的起点来自可注入时钟的挂钟采样"
    );
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM application_run WHERE clean_exit_at IS NOT NULL"),
        0,
        "刚启动的 run 还没有退出时刻"
    );
    assert_eq!(fx.scalar("SELECT COUNT(*) FROM work_session"), 0);
    assert_eq!(fx.scalar("SELECT COUNT(*) FROM interval_checkpoint"), 0);
    assert!(
        !running.recovery().requires_recovery(),
        "干净启动没有恢复材料，门禁开着"
    );
    assert!(fx.artifacts().is_empty(), "无迁移 ⇒ 零备份产物（按需判据）");

    // ── ② 计时：一次业务写、一条开放区间、一个 elapsed=0 的初始检查点 ────
    let session = fx.start_timer(&running);
    let interval = fx.text(&format!(
        "SELECT id FROM work_interval WHERE session_id = '{session}' AND ended_at IS NULL"
    ));
    assert_eq!(
        fx.text("SELECT id FROM work_session WHERE state = 'running'"),
        session,
        "命令返回的会话就是库里那条唯一在跑的会话"
    );
    assert_eq!(
        fx.text(&format!(
            "SELECT run_id FROM work_session WHERE id = '{session}'"
        )),
        run1,
        "会话属于本次 run"
    );
    assert_eq!(
        fx.scalar(&format!(
            "SELECT started_at FROM work_session WHERE id = '{session}'"
        )),
        WALL
    );
    assert_eq!(
        fx.text("SELECT status FROM task WHERE id = 't1'"),
        "Doing",
        "start 同事务把任务推到 Doing"
    );
    assert_eq!(
        fx.scalar("SELECT row_version FROM task WHERE id = 't1'"),
        2,
        "首次 start 冻结估时基准（0→1）再把任务推到 Doing（1→2）：两跳都在同一个事务里"
    );
    assert_eq!(
        fx.scalar(&format!(
            "SELECT COUNT(*) FROM work_interval WHERE session_id = '{session}'"
        )),
        1
    );
    assert_eq!(
        fx.scalar_opt(&format!(
            "SELECT duration_ms FROM work_interval WHERE id = '{interval}'"
        )),
        None,
        "开放区间没有时长"
    );
    assert_eq!(
        fx.scalar(&format!(
            "SELECT needs_review FROM work_interval WHERE id = '{interval}'"
        )),
        0
    );
    assert_eq!(fx.revision(), 1, "start 恰好一次业务写");
    assert_eq!(
        fx.checkpoint(),
        Some(CheckpointFacts {
            interval_id: interval.clone(),
            run_id: run1.clone(),
            wall_at: WALL,
            attribution_at: WALL,
            elapsed_ms: 0,
        }),
        "start 落一个 elapsed=0 的初始检查点（归属起点 = 采样点的 A(M)）"
    );

    // ── ③ 关掉全部窗口：核心与周期采样继续 ────────────────────────────────
    //
    // 本进程里没有任何窗口（窗口生命周期归 P7/P8）；这一步的可断言半边是
    // 「采样驱动与窗口解耦」：心跳照样每 30 秒覆盖检查点，revision 一动不动。
    let ticks_before_close = running.sampling_ticks();
    for (beat, elapsed) in [(1_i64, HEARTBEAT_MS), (2, HEARTBEAT_MS * 2)] {
        fx.advance(HEARTBEAT_MS);
        fx.wait_for_checkpoint(elapsed);
        assert_eq!(
            fx.checkpoint(),
            Some(CheckpointFacts {
                interval_id: interval.clone(),
                run_id: run1.clone(),
                wall_at: WALL + elapsed,
                attribution_at: WALL + elapsed,
                elapsed_ms: elapsed,
            }),
            "第 {beat} 拍心跳：无窗口引用时检查点照样前进"
        );
        assert_eq!(fx.revision(), 1, "心跳不加 revision（00 §5）");
    }
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM interval_checkpoint"),
        1,
        "检查点是一行、每次心跳覆盖（不是每次心跳新增一行）"
    );
    assert!(
        running.sampling_ticks() > ticks_before_close,
        "采样触发次数在关窗期间继续增长：{} → {}",
        ticks_before_close,
        running.sampling_ticks()
    );
    assert_eq!(running.sampling_errors(), 0, "健康采样不报错");
    assert!(!running.sampling_died_unexpectedly(), "采样线程还活着");

    // ── ④ 重开窗口：立即拉快照（冷启动握手） ──────────────────────────────
    //
    // 00 §5 规则 1 的「先监听后快照」在服务侧就是这两条命令体：先拿库身份与版本，
    // 再拉与它同一读事务的一致视图。窗口拿到的每一个数字都必须与库里的权威事实相等。
    let (handshake, snapshot) = {
        let mut state = lock_app(&app);
        let handshake = commands::get_revision_impl(&mut state).expect("握手");
        let snapshot = commands::timer_snapshot_impl(&mut state).expect("快照");
        (handshake, snapshot)
    };
    assert_eq!(handshake.data_epoch, epoch1);
    assert_eq!(handshake.revision, 1);
    assert_eq!(snapshot.data_epoch, epoch1);
    assert_eq!(snapshot.revision, 1, "快照与握手同源");
    assert_eq!(snapshot.run_id, run1);
    assert_eq!(snapshot.session_id.as_deref(), Some(session.as_str()));
    assert_eq!(snapshot.state, Some(SessionState::Running));
    assert_eq!(snapshot.task_id.as_deref(), Some("t1"));
    assert_eq!(
        snapshot.task_title.as_deref(),
        Some("任务一"),
        "冷启动窗口没有第二条取任务标题的路径，标题必须在快照里"
    );
    assert_eq!(
        snapshot.task_row_version,
        Some(2),
        "快照带任务版本：前端据此构造 resume 请求（冷启动窗口没有第二条取版本的路径）"
    );
    assert_eq!(
        snapshot.as_of,
        WALL + HEARTBEAT_MS * 2,
        "暂计截至最近一次采样的归属终点 A(M)"
    );
    assert_eq!(
        snapshot.active_ms,
        HEARTBEAT_MS * 2,
        "关窗期间的 60 秒仍在计时（核心没有随窗口停下）"
    );
    assert_eq!(snapshot.pending_ms, None, "没有待确认段");
    assert!(
        snapshot.tick_seq >= 2,
        "两拍心跳已经发生，tick 序号至少前进两次：{}",
        snapshot.tick_seq
    );

    // ── ⑤ 崩溃：不调 shutdown，库里留下「崩在半路」的事实 ─────────────────
    //
    // `shutdown()` 是**显式退出**（写 `clean_exit_at`、结束 running/paused 会话）。
    // 崩溃是它的反面：直接 drop 整个运行态——采样线程停、连接关、单实例锁由内核释放。
    drop(running);
    // 旧 `AppState` 的句柄也必须放掉：留着它等于留着一条打开的连接，
    // 而恢复的最后一步是**同目录改名**（Windows 上被打开的文件改不动）。
    drop(app);

    assert_eq!(
        fx.text(&format!(
            "SELECT state FROM work_session WHERE id = '{session}'"
        )),
        "running",
        "崩溃现场就是一个 running 会话"
    );
    assert_eq!(
        fx.scalar(&format!(
            "SELECT COUNT(*) FROM work_interval WHERE session_id = '{session}' AND ended_at IS NULL"
        )),
        1,
        "它的开放区间还在"
    );
    assert_eq!(
        fx.checkpoint(),
        Some(CheckpointFacts {
            interval_id: interval.clone(),
            run_id: run1.clone(),
            wall_at: WALL + HEARTBEAT_MS * 2,
            attribution_at: WALL + HEARTBEAT_MS * 2,
            elapsed_ms: HEARTBEAT_MS * 2,
        }),
        "最后一个可信点原样留在库里"
    );
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM application_run WHERE clean_exit_at IS NOT NULL"),
        0,
        "崩溃不写 clean_exit_at（与显式退出可区分）"
    );
    assert_eq!(fx.scalar("SELECT COUNT(*) FROM time_edit"), 0);
    assert_eq!(fx.revision(), 1);

    // 崩溃现场**不止这一次崩溃**：一个长期使用的库还带着更早几次留下的残骸。
    // 用裸 SQL 注入（服务层不会产生损坏行），让 P3 的四类判定同台登场——
    // 这样"重启走四类判定"是在**同一条链路**上被看见的，而不是另开一个场景。
    {
        let db = fx.db();
        let connection = db.connection();
        // 第 1 类：running 却没有开放区间（判据之间不一致，只能诊断）。
        connection
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                          needs_review,row_version)
                 VALUES('s-fault','t2',?1,'BACKGROUND','running','stopwatch',?2,0,0)",
                rusqlite::params![run1, WALL - 5_000],
            )
            .unwrap();
        // 第 3 类：P2 留下的「终点未知」恢复记录（开放 + 待确认，是**合法**形态）。
        connection
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                          needs_review,row_version)
                 VALUES('s-rec','t2',?1,'BACKGROUND','recovering','stopwatch',?2,1,0)",
                rusqlite::params![run1, WALL - 4_000],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO work_interval(id,session_id,started_at,needs_review)
                 VALUES('iv-rec','s-rec',?1,1)",
                rusqlite::params![WALL - 4_000],
            )
            .unwrap();
        // 第 4 类：干净暂停（没有开放/待确认区间）。
        connection
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                          needs_review,row_version)
                 VALUES('s-pause','t2',?1,'BACKGROUND','paused','stopwatch',?2,0,0)",
                rusqlite::params![run1, WALL - 3_000],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review)
                 VALUES('iv-pause','s-pause',?1,?2,2000,0)",
                rusqlite::params![WALL - 3_000, WALL - 1_000],
            )
            .unwrap();
    }

    // ── ⑥ 重启：P3 的四类判定 ────────────────────────────────────────────
    let restarted = fx.start();
    let app = Arc::clone(restarted.app());
    let run2 = restarted.run_id().to_string();
    assert_ne!(run2, run1, "重启建新的 application_run");
    assert_eq!(fx.epoch(), epoch1, "重启不换库身份（epoch 只在恢复时换）");
    assert_eq!(fx.scalar("SELECT COUNT(*) FROM application_run"), 2);
    assert_eq!(
        fx.artifacts().len(),
        0,
        "已有库、版本相等 ⇒ 重启不产生备份产物（按需判据）"
    );

    // 第 2 类：归一成「可信前缀 + 终点未知的待确认段」，会话转 recovering。
    assert_eq!(
        fx.text(&format!(
            "SELECT state FROM work_session WHERE id = '{session}'"
        )),
        "recovering"
    );
    assert_eq!(
        fx.scalar(&format!(
            "SELECT needs_review FROM work_session WHERE id = '{session}'"
        )),
        1
    );
    assert_eq!(
        fx.scalar(&format!(
            "SELECT row_version FROM work_session WHERE id = '{session}'"
        )),
        1,
        "扫描改过一次状态 ⇒ 会话版本 +1"
    );
    assert_eq!(
        fx.text(&format!(
            "SELECT run_id FROM work_session WHERE id = '{session}'"
        )),
        run1,
        "第 2 类保持原归属，等 reconcile 才切到本次 run"
    );
    assert_eq!(
        fx.scalar_opt(&format!(
            "SELECT ended_at FROM work_interval WHERE id = '{interval}'"
        )),
        Some(WALL + HEARTBEAT_MS * 2),
        "可信前缀在**最后一个检查点**的归属时刻闭合（不是「到重启时刻」那段猜测）"
    );
    assert_eq!(
        fx.scalar_opt(&format!(
            "SELECT duration_ms FROM work_interval WHERE id = '{interval}'"
        )),
        Some(HEARTBEAT_MS * 2)
    );
    assert_eq!(
        fx.scalar_opt(&format!(
            "SELECT sampled_end_wall_at FROM work_interval WHERE id = '{interval}'"
        )),
        Some(WALL + HEARTBEAT_MS * 2)
    );
    let pending = fx.text(&format!(
        "SELECT id FROM work_interval WHERE session_id = '{session}' AND needs_review = 1"
    ));
    assert_eq!(
        fx.scalar(&format!(
            "SELECT COUNT(*) FROM work_interval WHERE session_id = '{session}'"
        )),
        2,
        "归一后是两段：可信前缀 + 待确认候选"
    );
    assert_eq!(
        fx.scalar(&format!(
            "SELECT started_at FROM work_interval WHERE id = '{pending}'"
        )),
        WALL + HEARTBEAT_MS * 2
    );
    assert_eq!(
        fx.scalar_opt(&format!(
            "SELECT ended_at FROM work_interval WHERE id = '{pending}'"
        )),
        Some(WALL + HEARTBEAT_MS * 2),
        "候选端点是零长度，不是事实"
    );
    assert_eq!(
        fx.scalar_opt(&format!(
            "SELECT duration_ms FROM work_interval WHERE id = '{pending}'"
        )),
        None,
        "待确认段没有时长 ⇒ 不计入任何「已确认」数字"
    );

    // 第 3 类：原样保持（row_version 一动不动）。
    assert_eq!(
        fx.text("SELECT state FROM work_session WHERE id = 's-rec'"),
        "recovering"
    );
    assert_eq!(
        fx.text("SELECT run_id FROM work_session WHERE id = 's-rec'"),
        run1
    );
    assert_eq!(
        fx.scalar("SELECT row_version FROM work_session WHERE id = 's-rec'"),
        0
    );
    assert_eq!(
        fx.scalar_opt("SELECT ended_at FROM work_interval WHERE id = 'iv-rec'"),
        None,
        "终点未知的开放段原样保留"
    );

    // 第 4 类：保持 paused，重绑本次 run，**不自动继续计时**。
    assert_eq!(
        fx.text("SELECT state FROM work_session WHERE id = 's-pause'"),
        "paused"
    );
    assert_eq!(
        fx.text("SELECT run_id FROM work_session WHERE id = 's-pause'"),
        run2
    );
    assert_eq!(
        fx.scalar("SELECT row_version FROM work_session WHERE id = 's-pause'"),
        1
    );
    assert_eq!(
        fx.scalar("SELECT duration_ms FROM work_interval WHERE id = 'iv-pause'"),
        2_000,
        "历史时长一个毫秒都不动"
    );

    // 第 1 类：只诊断，一个字都不写。
    assert_eq!(
        fx.text("SELECT state FROM work_session WHERE id = 's-fault'"),
        "running"
    );
    assert_eq!(
        fx.scalar("SELECT row_version FROM work_session WHERE id = 's-fault'"),
        0
    );
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM work_interval WHERE session_id = 's-fault'"),
        0,
        "不得替损坏记录造区间"
    );

    // 版本与审计：整批扫描**恰好一次** revision，只有真的变了的两个会话各一条审计。
    assert_eq!(fx.revision(), 2, "扫描事务整批 +1（不是每个会话一次）");
    assert_eq!(fx.scalar("SELECT COUNT(*) FROM time_edit"), 2);
    assert_eq!(
        fx.text("SELECT reason FROM time_edit ORDER BY reason LIMIT 1"),
        "crashed open interval normalized"
    );
    assert_eq!(
        fx.text("SELECT reason FROM time_edit ORDER BY reason DESC LIMIT 1"),
        "paused session rebound to this run"
    );

    // 门禁：三类事实都在，第 4 类重绑后不再是「别的 run 的残留」。
    assert!(restarted.recovery().requires_recovery());
    assert_eq!(
        restarted.recovery().unfinished_sessions,
        vec!["s-fault".to_string(), "s-rec".to_string(), session.clone()]
    );
    assert_eq!(
        restarted.recovery().pending_intervals,
        vec!["iv-rec".to_string(), pending.clone()]
    );
    assert_eq!(
        restarted.recovery().invariant_faults,
        vec![InvariantFault {
            session_id: "s-fault".to_string(),
            reason: "running 会话没有开放区间",
        }]
    );

    // 门禁关着 ⇒ 新计时被拒，且**四件事**成立（逐字段比对，不只比行数）。
    let before = fx.facts();
    let refused = {
        let mut state = lock_app(&app);
        state
            .start(start_request(&epoch1, 2))
            .expect_err("门禁关着时不得开始新计时")
    };
    assert_code(&refused, "RECOVERY_REQUIRED");
    assert_eq!(fx.facts(), before, "被门禁拒绝的命令不得写入任何东西");
    assert_eq!(fx.text("SELECT title FROM task WHERE id = 't1'"), "任务一");

    // ── ⑦ 备份：先写一条「只有这一时刻才有」的标记，再备份 ────────────────
    fx.create_task(&restarted, "恢复前独有的事实").unwrap();
    assert_eq!(fx.revision(), 3, "一条业务写恰好 +1");
    let artifact = fx.take_backup();
    assert_eq!(
        fx.artifacts().len(),
        1,
        "备份产物落在注入的目录里：{:?}",
        fx.artifacts()
    );

    // 产物可独立打开，且内容是**那一刻**的事实。
    {
        let backup = Db::open(&artifact).expect("备份产物必须能独立打开");
        let connection = backup.connection();
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        assert_eq!(
            current_version(connection).unwrap(),
            SCHEMA_VERSION,
            "产物库版号与被备份库一致"
        );
        assert_eq!(
            meta::read_meta(connection).unwrap().unwrap().revision,
            3,
            "产物里的 revision 就是备份那一刻的"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM task WHERE title = '恢复前独有的事实'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    // 备份**之后**再改一次库：恢复必须把它退回去。
    fx.create_task(&restarted, "备份之后的改动").unwrap();
    assert_eq!(fx.revision(), 4);
    let epoch_before_restore = fx.epoch();

    // ── ⑧ 恢复：新 epoch、新 run、旧请求被拒 ──────────────────────────────
    let ticks_before_restore = restarted.sampling_ticks();
    let outcome = restore_from_backup(
        &app,
        restarted.broadcaster(),
        &artifact,
        Some(&fx.backup_dir),
        &fx.clock_source(),
    )
    .expect("恢复应当成功");
    assert!(outcome.committed, "这条路径是提交");

    let new_epoch = fx.epoch();
    assert_ne!(new_epoch, epoch_before_restore, "必须生成全新 data_epoch");
    assert_eq!(outcome.data_epoch, new_epoch, "返回的 epoch 就是库里的那个");
    let run3 = outcome.run_id.clone();
    assert_ne!(run3, run1);
    assert_ne!(run3, run2, "恢复建的是**新** run");
    assert_eq!(fx.scalar("SELECT COUNT(*) FROM application_run"), 3);
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM application_run WHERE clean_exit_at IS NOT NULL"),
        0,
        "整条链路没有一次显式退出 ⇒ 没有任何 run 被写下 clean_exit_at"
    );
    // 事实退回到备份时刻。
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM task WHERE title = '备份之后的改动'"),
        0,
        "恢复丢弃备份之后的改动"
    );
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM task WHERE title = '恢复前独有的事实'"),
        1
    );
    assert_eq!(
        fx.revision(),
        4,
        "新 epoch 内重新起算：备份里是 3，恢复时的重扫把 s-pause 重绑到新 run ⇒ 恰好 +1"
    );

    // 驱动绑定**新**运行态：新协调器、无旧基线、维护态已退出。
    {
        let state = lock_app(&app);
        assert_eq!(state.coordinator().unwrap().run_id(), run3);
        assert!(
            state.coordinator().unwrap().live().is_none(),
            "新协调器不继承旧计时基线"
        );
        assert!(state.runtime_present());
        assert!(state.maintenance().is_none(), "维护态已退出");
        assert_eq!(state.recovery(), &outcome.recovery, "门禁快照已装机");
    }
    // 广播：一条带**新 epoch** 的 `domain.changed`（客户端据此重新握手）。
    let events = fx.sink.events();
    let last = events.last().expect("恢复完成必须广播一条");
    assert_eq!(last.event, "domain.changed");
    assert_eq!(last.data_epoch, new_epoch);
    assert_eq!(last.revision, fx.revision());

    // 旧 epoch 的写请求：一律拒绝且不写入。
    let before = fx.facts();
    let refused = {
        let mut state = lock_app(&app);
        commands::create_task_impl(
            &mut state,
            restarted.broadcaster(),
            commands::CreateTaskRequest {
                expected_data_epoch: epoch_before_restore.clone(),
                title: "旧 epoch 的写入".to_string(),
                project_id: None,
            },
        )
        .expect_err("旧 epoch 的写必须被拒")
    };
    assert_code(&refused, "DATA_EPOCH_MISMATCH");
    // 再拿一条**本来会写审计**的命令（任务跃迁）试同一件事：
    // 这样"审计表没有新增行"就不是恒真断言（`create_task` 本来就不写审计）。
    let refused = {
        let mut state = lock_app(&app);
        state
            .transition_task(
                WriteEnvelope::for_update(epoch_before_restore.clone(), 2),
                TransitionTaskRequest {
                    task_id: "t1".to_string(),
                    target: TaskStatus::Blocked,
                    cause: TransitionCause::User,
                },
            )
            .expect_err("旧 epoch 的跃迁必须被拒")
    };
    assert_code(&refused, "DATA_EPOCH_MISMATCH");

    assert_eq!(fx.facts(), before, "被拒的命令零写入（含审计表）");
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM task WHERE title = '旧 epoch 的写入'"),
        0
    );
    assert_eq!(
        fx.text("SELECT title FROM task WHERE id = 't1'"),
        "任务一",
        "既有记录字段一致"
    );
    assert_eq!(fx.text("SELECT status FROM task WHERE id = 't1'"), "Doing");
    assert_eq!(
        fx.scalar("SELECT row_version FROM task WHERE id = 't1'"),
        2,
        "既有记录字段一致（版本没被被拒的命令动过）"
    );

    // **正控**：同一条跃迁命令换上新 epoch 就成功，而且真的写了审计
    // ⇒ 上面那条"零审计"是被 epoch 守卫挡住的，不是这条命令本来就不写。
    let audits_before = fx.scalar("SELECT COUNT(*) FROM task_change");
    {
        let mut state = lock_app(&app);
        state
            .transition_task(
                WriteEnvelope::for_update(new_epoch.clone(), 2),
                TransitionTaskRequest {
                    task_id: "t1".to_string(),
                    target: TaskStatus::Blocked,
                    cause: TransitionCause::User,
                },
            )
            .expect("新 epoch 的跃迁应当成功");
    }
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM task_change"),
        audits_before + 1
    );
    assert_eq!(fx.revision(), 5, "一条业务写恰好 +1");
    assert_eq!(
        fx.text("SELECT status FROM task WHERE id = 't1'"),
        "Blocked"
    );

    // 回滚副本：**恢复之前那个世界**的唯一完整副本，刻意保留（本用例不删它）。
    let rollback = outcome
        .rollback
        .clone()
        .expect("提交路径必须报出回滚副本的路径");
    assert!(rollback.exists(), "回滚副本必须真的在磁盘上");
    {
        let old = Db::open(&rollback).expect("回滚副本必须能独立打开");
        let connection = old.connection();
        assert_eq!(
            meta::read_meta(connection).unwrap().unwrap().data_epoch,
            epoch_before_restore,
            "副本保留了恢复前的库身份"
        );
        assert_eq!(meta::read_meta(connection).unwrap().unwrap().revision, 4);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM task WHERE title = '备份之后的改动'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1,
            "副本里有恢复**之前**才存在的那条改动"
        );
    }

    // 采样驱动仍然活着：同一个 `Scheduler` 换库后自然对新运行态工作。
    assert!(!restarted.sampling_died_unexpectedly());
    assert_eq!(
        restarted.sampling_errors(),
        0,
        "维护态那几拍是整拍跳过，不是失败"
    );
    wait_until(|| restarted.sampling_ticks() >= ticks_before_restore + 3);

    // 链路终点：门禁**仍然关着**——恢复出来的世界带着未处理的第 1/2/3 类事实，
    // 必须走 F-015 的用户确认才能开始新计时（这是正确结果，不是缺陷）。
    assert!(outcome.recovery.requires_recovery());
    assert_eq!(
        outcome.recovery.unfinished_sessions,
        vec!["s-fault".to_string(), "s-rec".to_string(), session.clone()]
    );
    assert_eq!(
        outcome.recovery.pending_intervals,
        vec!["iv-rec".to_string(), pending.clone()]
    );
    let refused = {
        let mut state = lock_app(&app);
        state
            .start(start_request(&new_epoch, 3))
            .expect_err("恢复后门禁仍关着")
    };
    assert_code(&refused, "RECOVERY_REQUIRED");
}

// ─────────────────────────────────────────────────────────────────────────────
// F-014：本地优先 / 离线
// ─────────────────────────────────────────────────────────────────────────────

/// 生产源码里一旦出现这些符号，就说明多了一条出网路径。
///
/// 判据是**源码面**（`src/**/*.rs`）+ **依赖面**（`Cargo.toml` 的依赖段），
/// 不是"跑一遍看看报不报错"：后者在联网的机器上永远绿。
/// 注释里出现的 URL **不**算（文档链接不是出网路径），所以这里只认客户端符号。
const NETWORK_SYMBOLS: [&str; 17] = [
    "std::net",
    "tcpstream",
    "tcplistener",
    "udpsocket",
    "socketaddr",
    "tosocketaddrs",
    "reqwest",
    "hyper::",
    "ureq::",
    "isahc",
    "tungstenite",
    "tokio::net",
    "tonic::",
    "h2::",
    "openai",
    "anthropic",
    "api_key",
];

/// 直接依赖里一旦出现这些名字，依赖面就不再是"离线自足"的。
const NETWORK_CRATES: [&str; 19] = [
    "reqwest",
    "hyper",
    "ureq",
    "surf",
    "isahc",
    "curl",
    "attohttpc",
    "awc",
    "tower-http",
    "tokio-tungstenite",
    "tungstenite",
    "async-tungstenite",
    "tonic",
    "h2",
    "rustls",
    "native-tls",
    "openssl",
    "async-openai",
    "openai-api-rs",
];

/// F-014 在**服务层**能证的那一半：不依赖网络/AI。
///
/// 三条证据各自可失败：
/// 1. **依赖面**：`Cargo.toml` 的依赖段里没有任何 HTTP/网络/AI 客户端（解析器先自证有效：
///    必须解析出已知的几条依赖）；
/// 2. **源码面**：`src/**/*.rs` 里零网络客户端符号（改坏判据或加一条出网调用立即红）；
/// 3. **行为面**：整条本地功能链（计时 → Today → JSON 明细导出 → Markdown 周回顾 →
///    备份）在一台**真库**上跑通，且给出确定数字——导出只读本机库，不联网。
///
/// **不能据此宣称**「拔网线跑通了 V0.1」：真实拔网线、发布产物与界面链路归 P8。
#[test]
fn the_v01_service_surface_is_offline_and_exports_stay_local() {
    // ① 依赖面。
    let dependencies = declared_dependencies(&source("Cargo.toml"));
    for known in ["tauri", "rusqlite", "serde_json", "jiff", "uuid"] {
        assert!(
            dependencies.iter().any(|name| name == known),
            "依赖解析器必须真的解析出 `{known}`（否则下面的「零命中」是空断言）：{dependencies:?}"
        );
    }
    let offenders: Vec<&String> = dependencies
        .iter()
        .filter(|name| NETWORK_CRATES.contains(&name.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "V0.1 的服务链路不得依赖任何网络/AI 客户端：{offenders:?}"
    );

    // ② 源码面。
    let mut files = Vec::new();
    collect_rs_files(&manifest().join("src"), &mut files);
    assert!(
        files.len() >= 30,
        "源码扫描必须真的扫到文件（否则「零命中」是空断言）：{}",
        files.len()
    );
    let mut hits = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("读不到 {}：{error}", path.display()))
            .to_lowercase();
        for needle in NETWORK_SYMBOLS {
            if text.contains(needle) {
                hits.push(format!("{}: {needle}", relative(path)));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "生产源码里出现了网络/AI 客户端符号（F-014 的「无网可用」不再是可自动验证的性质）：{hits:?}"
    );

    // ③ 行为面：整条本地功能链在真库上跑，给出确定数字。
    let fx = Fixture::new();
    let running = fx.start();
    let app = Arc::clone(running.app());
    let epoch = fx.epoch();
    let session = fx.start_timer(&running);
    fx.advance(HEARTBEAT_MS * 2);
    fx.wait_for_checkpoint(HEARTBEAT_MS * 2);
    {
        let mut state = lock_app(&app);
        commands::finish_timer_impl(
            &mut state,
            running.broadcaster(),
            SessionRequest {
                expected_data_epoch: epoch.clone(),
                session_id: session.clone(),
                session_expected_version: 0,
            },
        )
        .expect("结束计时");
    }
    assert_eq!(fx.revision(), 2, "start + finish 各恰好一次业务写");
    assert_eq!(
        fx.scalar("SELECT duration_ms FROM work_interval"),
        HEARTBEAT_MS * 2,
        "区间时长 = 归属终点 − 起点：全程本机计算"
    );

    // Today：人工那一列就是已确认的 60 秒。
    let today = {
        let mut state = lock_app(&app);
        state
            .stats_today(&TodayQuery {
                timezone: TZ.to_string(),
                expected_data_epoch: epoch.clone(),
            })
            .expect("Today")
    };
    assert_eq!(today.date, "2023-11-15");
    assert_eq!(today.timezone, TZ);
    assert_eq!(
        today.column(StatsClass::Confirmed, Measure::Human).ms,
        Some(HEARTBEAT_MS * 2)
    );
    assert_eq!(
        today
            .column(StatsClass::Confirmed, Measure::Human)
            .intervals,
        1
    );
    assert_eq!(
        today.column(StatsClass::Pending, Measure::Human).intervals,
        0
    );
    assert_eq!(
        today.column(StatsClass::Live, Measure::Human).ms,
        Some(0),
        "没有开放区间就没有运行暂计"
    );
    // `current` 仍指向**刚提交结束**的那条会话，只是状态是 `finished`：
    // 协调器的镜像（`Coordinator::live`）在提交后照实装载那一行，P2/P5 就是这条语义，
    // 今天没有任何用例把它钉住。**这是本任务发现的既有行为，不是本任务改的**——
    // 界面（P8）必须按 `state` 分支，不能把「有 current」当成「正在计时」。
    let current = today
        .current
        .as_ref()
        .expect("刚结束的会话仍在协调器镜像里");
    assert_eq!(current.session_id, session);
    assert_eq!(current.task_id, "t1");
    assert_eq!(current.state, SessionState::Finished);
    assert_eq!(today.revision, 2);
    assert_eq!(today.data_epoch, epoch);

    // JSON 明细导出：结构化结果里的事实与库一致。
    let json = {
        let mut state = lock_app(&app);
        state
            .export_json(&StatsRangeQuery {
                from: WALL - 1_000,
                to: WALL + HEARTBEAT_MS * 4,
                timezone: TZ.to_string(),
                expected_data_epoch: epoch.clone(),
            })
            .expect("JSON 导出")
    };
    assert_eq!(json.data_epoch, epoch);
    assert_eq!(json.revision, 2);
    let document: serde_json::Value =
        serde_json::from_str(&json.text).expect("导出的就是 JSON 文本");
    assert_eq!(document["revision"], 2);
    assert_eq!(document["data_epoch"], epoch.as_str());
    assert_eq!(document["as_of"], WALL + HEARTBEAT_MS * 2);
    let intervals = document["intervals"].as_array().expect("明细是数组");
    assert_eq!(intervals.len(), 1);
    assert_eq!(intervals[0]["duration_ms"], HEARTBEAT_MS * 2);
    assert_eq!(intervals[0]["clipped_ms"], HEARTBEAT_MS * 2);
    assert_eq!(intervals[0]["measure"], "human");
    assert_eq!(intervals[0]["class"], "confirmed");

    // Markdown 周回顾：周界与合计都是确定的（周一 → 下周一，半开）。
    let weekly = {
        let mut state = lock_app(&app);
        state
            .export_weekly_markdown(&WeeklyQuery {
                timezone: TZ.to_string(),
                anchor: None,
                expected_data_epoch: epoch.clone(),
            })
            .expect("周回顾")
    };
    assert_eq!(weekly.data_epoch, epoch);
    assert_eq!(weekly.revision, 2);
    assert_eq!(weekly.week_start, "2023-11-13");
    assert_eq!(weekly.week_end, "2023-11-20");
    assert_eq!(weekly.range.from, 1_699_804_800_000);
    assert_eq!(weekly.range.to, 1_700_409_600_000);
    assert!(
        weekly
            .text
            .contains("本周合计：1 分钟（60000 毫秒），共 1 条已确认区间。"),
        "周回顾的合计与明细一致：{}",
        weekly.text
    );
    assert!(weekly
        .text
        .contains("| 2023-11-15 | 1 分钟（60000 毫秒） |"));

    // 备份：同一套原语在本机把库复制成单文件快照，产物可独立打开。
    let artifact = fx.take_backup();
    let backup = Db::open(&artifact).expect("产物可独立打开");
    let integrity: String = backup
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert_eq!(
        backup
            .connection()
            .query_row("SELECT COUNT(*) FROM work_interval", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 源码扫描的样板（照 `tests/dev_injections.rs` 的写法）
// ─────────────────────────────────────────────────────────────────────────────

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn source(rel: &str) -> String {
    let path = manifest().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读不到 {}：{error}", path.display()))
}

fn relative(path: &Path) -> String {
    path.strip_prefix(manifest())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("读不到 {}：{e}", dir.display()))
    {
        let path = entry.expect("目录项读得到").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// `Cargo.toml` 里**依赖段**的 crate 名（`[dependencies]`、`[dev-dependencies]`、
/// `[build-dependencies]` 与 `[target.'…'.dependencies]`）。
///
/// 只取等号左边、跳过注释与空行：文档注释里出现的 crate 名（例如解释"为什么**不**引入
/// `fs2`"）不是依赖，不能算命中。
fn declared_dependencies(manifest: &str) -> Vec<String> {
    let mut in_dependencies = false;
    let mut names = Vec::new();
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_dependencies = line.ends_with("dependencies]");
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
    names
}
