//! P6 Task 4b：WAL 一致备份与恢复（新 `data_epoch`）。
//!
//! 覆盖计划 Task 4 列全的那份测试清单（逐条在下面各用例的文档里点名）：
//!
//! - 备份产物**可独立打开**并通过完整性 / 外键 / 版本校验；
//! - 恢复后 `data_epoch` 与恢复前不同、`run_id` 是**新的**；
//! - 旧 epoch 的写请求被拒（`DATA_EPOCH_MISMATCH`）且**不写入**；
//! - 恢复失败时原库可打开、原工时事实保留、重建状态符合恢复规则；
//! - 恢复期间写入返回 `DATA_RESTORE_IN_PROGRESS`，并按总纲 §5 第 8 条断言
//!   **被拒的用户命令四件事**（`revision` 不变、无新行、无审计、既有记录字段一致）；
//! - 在途心跳 / 异常分割与恢复**互斥**（同一把锁）；
//! - 维护态期间到达的写命令与**采样拍**都不能写新库；
//! - 成功后驱动仅绑定**新**运行态；失败后原库重建且**不继承旧计时基线**；
//! - 后台采样失败**不能绕过**维护态重试写入；
//! - 备份过程中有**并发写**时不产生撕裂（真实临时文件库，不用内存库）。
//!
//! 三段流程（① `begin_restore` → ② `prepare_and_swap` → ③ `commit_restore` /
//! `abort_restore`）在这里被**分段驱动**：只有这样才能把"维护态窗口"钉在测试手里
//! （计划原文的"堵在 `lock_app` 上的那一拍取锁之后被拒/被丢弃"就是那个窗口）。
//! 生产入口 `restore_from_backup` 把三段连在**一次调用**里，本文件也覆盖它。
//!
//! **权威身份只从库里读**：`RunningApp::data_epoch()/recovery()` 是启动快照，
//! 恢复之后就过期了（生产侧不读它们，命令层的握手读的是库）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::commands;
use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::{Clock, ClockSample, FakeClock, SampleError, SystemClock};
use worktrace_lib::platform::diagnostics::Diagnostics;
use worktrace_lib::services::backup::{
    abort_restore, backup_consistent, begin_restore, commit_restore, prepare_and_swap,
    restore_from_backup, ClockSource,
};
use worktrace_lib::services::bootstrap::{
    lock_app, startup, MaintenancePhase, NoProbe, RunningApp, SharedApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{Broadcaster, EventEnvelope, EventSink};
use worktrace_lib::services::recovery::DiscardSessionRequest;
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta;
use worktrace_lib::storage::migrations::{current_version, migrate, SCHEMA_VERSION};
use worktrace_lib::storage::session_repo;

/// 假时钟的挂钟与单调读数：整份用例共用同一个原点，测试之间不互相依赖真实时间。
const WALL: i64 = 1_700_000_000_000;
const MONO: i64 = 5_000;

/// 采样节拍：默认给一个**跑不起来**的值（用例自己控制"有没有采样拍"）；
/// 需要真的看到采样拍的用例用 [`launch_with_interval`] 显式给一个小节拍。
const IDLE_SAMPLING_MS: u64 = 3_600_000;

/// 可切换的假钟：启动时正常，之后可以让它**永远失败**（采样拍全部报错）。
///
/// 为什么需要它：`startup` 自己要先采一次样（`application_run.started_at` 与归属基线），
/// 而"后台采样一直失败"必须在**启动之后**才开始失败。
#[derive(Default)]
struct SwitchableClock {
    inner: Mutex<FakeClock>,
    fail: AtomicBool,
}

impl SwitchableClock {
    fn new() -> Self {
        Self {
            inner: Mutex::new(FakeClock::new(WALL, MONO)),
            fail: AtomicBool::new(false),
        }
    }

    fn fail_forever(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }

    /// 时钟恢复：③ 段要为新协调器采一次样，所以"采样失败窗口"必须能关掉。
    fn recover(&self) {
        self.fail.store(false, Ordering::SeqCst);
    }
}

impl Clock for SwitchableClock {
    fn sample(&self) -> Result<ClockSample, SampleError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(SampleError::Injected);
        }
        self.inner.lock().unwrap().sample()
    }
}

/// 交给 `startup` 的那一只（只借用共享状态，用例手里那份仍然能改它）。
struct SharedClock(Arc<SwitchableClock>);

impl Clock for SharedClock {
    fn sample(&self) -> Result<ClockSample, SampleError> {
        self.0.sample()
    }
}

/// 恢复的时钟来源（`services::backup::ClockSource`）。
///
/// 每次调用交出一只新的钟：提交与回滚互斥、都要建新协调器，而 `Coordinator::new`
/// 会拿走钟的所有权。**但交出的是同一个共享状态**（[`SharedClock`] 只是 `Arc` 克隆）——
/// 与生产侧"同一个 `SystemClock` 的克隆、共享 `origin`"是同一件事，也正是"时钟必须同源"
/// 的落地：恢复出来的协调器与组合根用的是**同一个**时钟来源。
fn clock_source(clock: &Arc<SwitchableClock>) -> ClockSource {
    let shared = Arc::clone(clock);
    Box::new(move || Box::new(SharedClock(Arc::clone(&shared))) as Box<dyn Clock + Send>)
}

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

/// 一个真应用（走 `services::bootstrap::startup`）+ 一个 Ready 任务 `t1`。
struct Rig {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    backup_dir: PathBuf,
    /// 正式诊断落点（注入临时目录：绝不写真实数据目录）。用例读它来断言
    /// "失败的原因有没有落盘"（P6 终审 I-2）。
    log_path: PathBuf,
    running: Box<RunningApp>,
    sink: Arc<RecordingSink>,
    clock: Arc<SwitchableClock>,
}

impl Rig {
    fn app(&self) -> SharedApp {
        Arc::clone(self.running.app())
    }

    fn broadcaster(&self) -> &Broadcaster {
        self.running.broadcaster()
    }

    /// 另开一条连接读库（**用完即弃**：恢复要改名主库文件，长期持有的第二条连接会让
    /// WAL 无法 checkpoint——那正是"不能覆盖仍打开的 WAL 数据库"要防的形态）。
    fn db(&self) -> Db {
        Db::open(&self.db_path).expect("第二条连接")
    }

    fn scalar(&self, sql: &str) -> i64 {
        self.db()
            .connection()
            .query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    fn text(&self, sql: &str) -> String {
        self.db()
            .connection()
            .query_row(sql, [], |row| row.get::<_, String>(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// 库里的**权威**身份（不是 `RunningApp` 的启动快照：恢复之后那份快照会过期，
    /// 权威读取口是库与 `AppState`）。
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

    /// 当前 run —— **只**从运行态读（`application_run` 里按 `started_at` 排序在假钟下
    /// 分不出先后：所有 run 的 `started_at` 都等于 `WALL`）。
    fn run_id(&self) -> String {
        let app = self.app();
        let state = lock_app(&app);
        state
            .coordinator()
            .expect("运行态在手")
            .run_id()
            .to_string()
    }

    fn events(&self) -> Vec<EventEnvelope> {
        self.sink.events.lock().unwrap().clone()
    }

    /// 诊断落点（与生产同一个 `Diagnostics`，只是路径在临时目录）。
    fn diagnostics(&self) -> Diagnostics {
        Diagnostics::to_file(&self.log_path)
    }

    /// 诊断日志里**最后一条** `event=<名字>` 记录（没有就是 `None`）。
    fn last_log_line(&self, event: &str) -> Option<String> {
        let needle = format!("event={event} ");
        std::fs::read_to_string(&self.log_path)
            .ok()?
            .lines()
            .rev()
            .find(|line| line.starts_with(&needle))
            .map(str::to_string)
    }

    fn clock_source(&self) -> ClockSource {
        clock_source(&self.clock)
    }

    /// 库里可数的"四件事"（第四件——既有记录字段一致——由各用例点名断言具体行）。
    fn facts(&self) -> Facts {
        let db = self.db();
        let connection = db.connection();
        Facts {
            revision: meta::read_meta(connection).unwrap().unwrap().revision,
            tasks: connection
                .query_row("SELECT COUNT(*) FROM task", [], |row| row.get(0))
                .unwrap(),
            sessions: connection
                .query_row("SELECT COUNT(*) FROM work_session", [], |row| row.get(0))
                .unwrap(),
            audit: connection
                .query_row("SELECT COUNT(*) FROM time_edit", [], |row| row.get(0))
                .unwrap(),
            checkpoints: connection
                .query_row("SELECT COUNT(*) FROM interval_checkpoint", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            runs: connection
                .query_row("SELECT COUNT(*) FROM application_run", [], |row| row.get(0))
                .unwrap(),
        }
    }
}

/// 被拒请求要断言的"四件事"里的可数部分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Facts {
    revision: i64,
    tasks: i64,
    sessions: i64,
    audit: i64,
    checkpoints: i64,
    runs: i64,
}

fn launch() -> Rig {
    launch_with_interval(IDLE_SAMPLING_MS)
}

fn launch_with_interval(sampling_interval_ms: u64) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");
    let backup_dir = dir.path().join("backups");
    let log_path = dir.path().join("worktrace.log");

    {
        let mut db = Db::open(&db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        meta::init_meta(&tx).unwrap();
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务一','Ready',0,1000,1000)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let clock = Arc::new(SwitchableClock::new());
    let mut config = StartupConfig::new(&db_path, &lock_path).with_diagnostic_log(&log_path);
    config.sampling_interval_ms = sampling_interval_ms;
    let sink = Arc::new(RecordingSink::default());
    let running = match startup(
        config,
        Box::new(SharedClock(Arc::clone(&clock))),
        Arc::clone(&sink) as Arc<dyn EventSink>,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };

    Rig {
        _dir: dir,
        db_path,
        backup_dir,
        log_path,
        running,
        sink,
        clock,
    }
}

/// 现做一份**当前库**的一致备份（走生产原语 `VACUUM INTO`），返回产物路径。
fn take_backup(rig: &Rig) -> PathBuf {
    let db = rig.db();
    let version = current_version(db.connection()).unwrap();
    backup_consistent(
        Some(&rig.backup_dir),
        db.connection(),
        version,
        &FakeClock::new(WALL, MONO),
        "test backup",
        &rig.diagnostics(),
    )
    .expect("备份应当成功")
}

/// 写一条可观察的业务事实（走真命令体：与用户命令同一条串行边界）。
fn create_task(rig: &Rig, title: &str) -> Result<(), AppError> {
    let app = rig.app();
    let mut state = lock_app(&app);
    commands::create_task_impl(
        &mut state,
        rig.broadcaster(),
        commands::CreateTaskRequest {
            expected_data_epoch: rig.epoch(),
            title: title.to_string(),
            project_id: None,
        },
    )
    .map(|_| ())
}

/// 启动一次计时（`t1`），返回会话 id。
fn start_timer(rig: &Rig) -> String {
    let app = rig.app();
    let mut state = lock_app(&app);
    let outcome = commands::start_timer_impl(
        &mut state,
        rig.broadcaster(),
        commands::StartTimerRequest {
            expected_data_epoch: rig.epoch(),
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

fn wait_for_ticks(running: &RunningApp, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while running.sampling_ticks() < n {
        assert!(
            Instant::now() < deadline,
            "采样驱动没有按周期触发：只跑了 {} 拍",
            running.sampling_ticks()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_restore_refusal(error: &AppError) {
    assert_eq!(error.code(), "DATA_RESTORE_IN_PROGRESS", "实际：{error:?}");
    assert_eq!(error.detail(), None, "维护态拒绝不带内部 detail：{error:?}");
}

/// 造一份**结构合法、但没有库身份**的候选库：`migrate` 建了 schema，而 `app_meta`
/// 那一行没插（`init_meta` 只在建库时调）。
///
/// 它正好穿过 ② 段的三条验证（完整性 / 外键 / 版本），而在 ③-a 的 `rotate_epoch`
/// 上失败——这就是"提交失败 ⇒ 回滚"那条路径的**确定性**入口。
fn candidate_without_identity(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let db = Db::open(&path).unwrap();
    migrate(db.connection()).unwrap();
    path
}

// ─────────────────────────────────────────────────────────────────────────────
// 备份产物本身
// ─────────────────────────────────────────────────────────────────────────────

/// 计划："备份文件可独立打开并通过完整性/外键/版本校验"。
#[test]
fn a_backup_opens_standalone_and_passes_integrity_foreign_keys_and_version() {
    let rig = launch();
    create_task(&rig, "备份前的事实").unwrap();
    let session = start_timer(&rig);
    let revision_before = rig.revision();

    let artifact = take_backup(&rig);

    let db = Db::open(&artifact).expect("备份产物必须能独立打开");
    let integrity: String = db
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let violations: i64 = db
        .connection()
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0, "备份产物不得有外键违规");
    assert_eq!(
        current_version(db.connection()).unwrap(),
        SCHEMA_VERSION,
        "备份产物的库版号与被备份库一致"
    );

    // 内容是**那一时刻**的事实：revision、会话、还没结束的开放区间都在。
    let meta = meta::read_meta(db.connection()).unwrap().unwrap();
    assert_eq!(meta.revision, revision_before);
    assert_eq!(
        db.connection()
            .query_row(
                "SELECT state FROM work_session WHERE id = ?1",
                [&session],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "running"
    );
    assert_eq!(
        db.connection()
            .query_row(
                "SELECT COUNT(*) FROM task WHERE title = '备份前的事实'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

/// 计划："备份过程中有并发写时不产生撕裂（用真实的临时文件库，不用内存库）"。
///
/// 判别力：**只拷主库文件**（不拷 `-wal`）的实现会丢掉最近一次提交——最后那条
/// `并发写期间的标记` 就在 WAL 里。所以这里既断言结构（完整性/外键），
/// 也断言那条标记**在产物里**。
#[test]
fn a_concurrent_write_during_backup_does_not_tear_the_artifact() {
    let rig = launch();
    create_task(&rig, "备份前的事实").unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let app = rig.app();
        let epoch = rig.epoch();
        let broadcaster = Arc::clone(rig.running.broadcaster());
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut n = 0;
            while !stop.load(Ordering::SeqCst) {
                let mut state = lock_app(&app);
                let _ = commands::create_task_impl(
                    &mut state,
                    &broadcaster,
                    commands::CreateTaskRequest {
                        expected_data_epoch: epoch.clone(),
                        title: format!("并发写 {n}"),
                        project_id: None,
                    },
                );
                n += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
            n
        })
    };

    // 让写线程先跑起来，然后**在并发写之下**备份。
    std::thread::sleep(Duration::from_millis(30));
    create_task(&rig, "并发写期间的标记").unwrap();
    let artifact = take_backup(&rig);
    stop.store(true, Ordering::SeqCst);
    let writes = writer.join().unwrap();
    assert!(writes > 0, "并发写线程必须真的写过东西");

    let db = Db::open(&artifact).expect("并发写之下写出的产物必须能独立打开");
    let integrity: String = db
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let violations: i64 = db
        .connection()
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);

    // **WAL 里的事实必须在产物里**（"只拷主库文件"的实现会在这里红）。
    let marked: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task WHERE title = '并发写期间的标记'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(marked, 1, "备份必须是 WAL 一致的：最近一次提交不能丢");

    // 结构上也不能出现"半个事实"：没有挂空的区间、没有缺开放区间的会话。
    let orphan_intervals: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM work_interval i
             WHERE NOT EXISTS (SELECT 1 FROM work_session s WHERE s.id = i.session_id)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphan_intervals, 0, "产物里不得有孤儿区间");
    let broken_running: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM work_session s
             WHERE s.state IN ('running','paused')
               AND NOT EXISTS (SELECT 1 FROM work_interval i WHERE i.session_id = s.id)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(broken_running, 0, "产物里不得有缺区间的会话");
}

// ─────────────────────────────────────────────────────────────────────────────
// 维护态窗口：运行态不在手
// ─────────────────────────────────────────────────────────────────────────────

/// ① 段之后：**三个访问器一律可失败**、运行态不在手、写命令拿到
/// `DATA_RESTORE_IN_PROGRESS` 且**四件事**成立。
#[test]
fn during_maintenance_the_runtime_is_absent_and_writes_are_refused() {
    let rig = launch();
    create_task(&rig, "维护前的事实").unwrap();
    let app = rig.app();
    // 一条**正在跑**的会话：下面那条被拒的命令（作废整次）在正常路径上会写一条
    // `time_edit` 审计 —— 这样"无审计"这条断言才有判别力（在 `create_task` 上断等于恒真，
    // 它本来就不写审计：评审 Minor 4）。
    let session = start_timer(&rig);
    let session_version: i64 = rig
        .db()
        .connection()
        .query_row(
            "SELECT row_version FROM work_session WHERE id = ?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    let before = rig.facts();
    let epoch = rig.epoch();

    // ① 进入维护态 + 取走运行态（锁内、短）。
    let runtime = begin_restore(&app).expect("进入维护态并取走运行态");

    {
        let mut state = lock_app(&app);
        assert!(state.maintenance().is_some(), "维护态已置位");
        assert_eq!(
            state.maintenance().unwrap().phase(),
            MaintenancePhase::Restore
        );
        assert!(!state.runtime_present(), "运行态不在手");
        // **访问器契约**（本任务的破坏性签名变更）：缺运行态 ⇒ 可失败；不是 panic，
        // 也不是默认值。（`Db`/`Coordinator` 不实现 `Debug`，所以用 `.err()` 取错误，
        // 不用 `unwrap_err()`。）
        assert_eq!(
            state
                .db()
                .err()
                .expect("缺运行态时 db() 必须返回 Err")
                .code(),
            "DATA_RESTORE_IN_PROGRESS"
        );
        assert_eq!(
            state
                .coordinator()
                .err()
                .expect("缺运行态时 coordinator() 必须返回 Err")
                .code(),
            "DATA_RESTORE_IN_PROGRESS"
        );
        assert_eq!(
            state
                .db_mut()
                .err()
                .expect("缺运行态时 db_mut() 必须返回 Err")
                .code(),
            "DATA_RESTORE_IN_PROGRESS"
        );

        // 维护期间到达的写命令：取锁之后立刻被拒（`run_command` 的 `guard_writable`
        // 与这三个访问器给的是**同一个码**，这里走的是命令体这一侧）。
        let refused = commands::create_task_impl(
            &mut state,
            rig.broadcaster(),
            commands::CreateTaskRequest {
                expected_data_epoch: epoch.clone(),
                title: "维护期间的写入".to_string(),
                project_id: None,
            },
        )
        .expect_err("维护态期间不得受理写入");
        assert_restore_refusal(&refused);

        // 再来一条**会写审计**的写命令（作废整次）：它被拒 ⇒ `time_edit` 一行都不许多。
        let refused = state
            .discard_session(
                WriteEnvelope::for_update(epoch.clone(), session_version),
                DiscardSessionRequest {
                    session_id: session.clone(),
                },
            )
            .expect_err("维护态期间不得受理会写审计的写入");
        assert_restore_refusal(&refused);
    }

    // 被拒的用户命令**四件事**：revision 不变、无新行、无审计、既有记录字段一致。
    assert_eq!(rig.facts(), before, "维护态拒绝必须零写入");
    assert_eq!(
        rig.text("SELECT title FROM task WHERE id = 't1'"),
        "任务一",
        "既有记录字段一致"
    );
    assert_eq!(rig.epoch(), epoch, "维护态期间库身份不变");

    // 收尾：把运行态装回去（三段流程的后两段）。
    let artifact = take_backup(&rig);
    let swap = prepare_and_swap(
        runtime,
        &artifact,
        Some(&rig.backup_dir),
        &rig.clock_source(),
        &rig.diagnostics(),
    )
    .unwrap_or_else(|failure| panic!("② 段应当成功：{:?}", failure.error));
    let outcome =
        commit_restore(&app, rig.broadcaster(), &swap, &rig.clock_source()).expect("③-a 提交");
    assert!(outcome.committed);

    // **正控**：出了维护态，同一条命令成功并且**真的写了审计** ⇒ 上面那条
    // "维护态零审计"是被判据挡住的，而不是这条命令本来就不写审计。
    let audit_before = rig.facts().audit;
    let (version_now, epoch_now) = {
        let state = lock_app(&app);
        let db = state.db().unwrap();
        (
            db.connection()
                .query_row(
                    "SELECT row_version FROM work_session WHERE id = ?1",
                    [&session],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            meta::read_meta(db.connection())
                .unwrap()
                .unwrap()
                .data_epoch,
        )
    };
    {
        // 恢复之后那条会话是外来事实（`recovering`）——作废整次本来就接它。
        let mut state = lock_app(&app);
        state
            .discard_session(
                WriteEnvelope::for_update(epoch_now, version_now),
                DiscardSessionRequest {
                    session_id: session.clone(),
                },
            )
            .expect("维护结束之后同一条命令应当成功");
    }
    assert_eq!(
        rig.facts().audit,
        audit_before + 1,
        "这条命令在正常路径上写审计（正控）"
    );
}

/// 计划："维护态期间到达的写命令与采样拍都不能写新库"。
///
/// 采样拍由**真的采样线程**产生（10ms 节拍）：维护态期间整拍跳过——不 heartbeat
/// （不写 `interval_checkpoint`）、不 tick、不广播，`sampling_errors` 也不涨。
#[test]
fn sampling_ticks_during_maintenance_write_nothing() {
    let rig = launch_with_interval(10);
    let app = rig.app();
    // 有一个**正在跑**的会话：健康路径下每一拍都会 tick，30 秒到点还会写检查点。
    start_timer(&rig);

    let runtime = begin_restore(&app).expect("进入维护态并取走运行态");
    // **快照取在进入维护态之后**：从"记起点"到"置上维护态"之间，采样线程还可以**合法地**
    // 跑完一拍并广播（那一刻还没进维护态）。把它算成"维护态期间的写入/广播"是假红
    // ——这条竞态在 10ms 节拍下真的咬过一次（P6 终审修复波的门禁首跑）。
    let before = rig.facts();
    let events_before = rig.events().len();

    // 等真的过了若干拍（`ticks` 是"触发了几次"，维护态**照涨**）。
    let ticks_at_entry = rig.running.sampling_ticks();
    wait_for_ticks(&rig.running, ticks_at_entry + 5);
    std::thread::sleep(Duration::from_millis(50));

    assert_eq!(
        rig.facts(),
        before,
        "维护态的采样拍不得写任何东西（含 interval_checkpoint）"
    );
    assert_eq!(
        rig.running.sampling_errors(),
        0,
        "维护态不是错误：sampling_errors 不得因此增长"
    );
    assert_eq!(
        rig.events().len(),
        events_before,
        "维护态期间不得广播（不广播旧世界的 tick）"
    );

    let artifact = take_backup(&rig);
    let swap = prepare_and_swap(
        runtime,
        &artifact,
        Some(&rig.backup_dir),
        &rig.clock_source(),
        &rig.diagnostics(),
    )
    .unwrap_or_else(|failure| panic!("② 段应当成功：{:?}", failure.error));
    commit_restore(&app, rig.broadcaster(), &swap, &rig.clock_source()).expect("③-a 提交");
}

/// 计划："后台采样失败不能绕过维护态重试写入"。
///
/// 两半：**维护窗口之内**（时钟坏掉）采样一次都没读到——`sampling_errors` 停在 0、
/// 库一个字节没动、写入被拒；**窗口之外**同一只坏钟必须立刻被采样拍观察到（正控，
/// 否则"0 次失败"只是"钟其实没坏"）。
#[test]
fn a_failing_sampler_cannot_bypass_maintenance() {
    let rig = launch_with_interval(10);
    let app = rig.app();
    let before = rig.facts();
    let epoch = rig.epoch();
    // 产物先做出来：它是一份**没有会话**的库，所以恢复之后门禁是开的，
    // 才可能在窗口之外起一次真计时去观察坏钟。
    let artifact = take_backup(&rig);

    // ① 正常进入维护态（进入时刻取自同一条时钟接缝，坏钟下连恢复都开不了头——
    //    这是口径的一部分：③ 段本来也要为新协调器采样）。
    let runtime = begin_restore(&app).expect("进入维护态并取走运行态");

    // 窗口之内让钟坏掉：维护态**整拍跳过**（判据在取锁之后第一句），所以连一次读钟都
    // 不会发生——`sampling_errors` 必须停在 0，而不是"每拍一条失败"。这正是
    // "后台采样失败不能绕过维护态重试写入"的落点：失败/重试那条路根本进不来。
    rig.clock.fail_forever();
    let ticks_at_entry = rig.running.sampling_ticks();
    wait_for_ticks(&rig.running, ticks_at_entry + 5);
    std::thread::sleep(Duration::from_millis(50));

    assert_eq!(
        rig.running.sampling_errors(),
        0,
        "维护态整拍跳过：坏钟不会在维护窗口里被读到，更不会变成失败重试"
    );
    assert_eq!(rig.facts(), before, "采样失败重试不得写进任何库");

    {
        let mut state = lock_app(&app);
        let refused = commands::create_project_impl(
            &mut state,
            rig.broadcaster(),
            commands::CreateProjectRequest {
                expected_data_epoch: epoch.clone(),
                name: "维护期间的写入".to_string(),
            },
        )
        .expect_err("维护态期间不得受理写入");
        assert_restore_refusal(&refused);
    }
    assert_eq!(rig.facts(), before, "被拒的写命令必须零写入");

    // 窗口关上之前让钟恢复：③ 段要为**新协调器**采一次样（判据没为坏钟开后门）。
    rig.clock.recover();
    let swap = prepare_and_swap(
        runtime,
        &artifact,
        Some(&rig.backup_dir),
        &rig.clock_source(),
        &rig.diagnostics(),
    )
    .unwrap_or_else(|failure| panic!("② 段应当成功：{:?}", failure.error));
    commit_restore(&app, rig.broadcaster(), &swap, &rig.clock_source()).expect("③-a 提交");

    // **正控**：恢复出来的库没有未完成会话 ⇒ 门禁是开的 ⇒ 起一次真计时，此后每一拍
    // 都会读钟；再把钟弄坏，采样错误必须真的涨。
    start_timer(&rig);
    rig.clock.fail_forever();
    let errors_before = rig.running.sampling_errors();
    let ticks_before = rig.running.sampling_ticks();
    wait_for_ticks(&rig.running, ticks_before + 3);
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.running.sampling_errors() == errors_before {
        assert!(
            Instant::now() < deadline,
            "坏钟在维护态之外必须被采样拍观察到（否则这条用例没有判别力）"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // 窗口之外坏钟走的是 **P2 既有的异常路径**（`read_sample` 拿不到样本 ⇒
    // `handle_anomaly(Unavailable)`：会话转 `recovering`、写一条审计、revision +1）——
    // 那是 P2 的规则，本任务不碰；这里只核对它**确实**发生了（与窗口内的"零写入"对照）。
    let after = rig.facts();
    assert_eq!(after.revision, before.revision + 2, "计时 +1、异常事务 +1");
    assert_eq!(after.audit, before.audit + 1, "异常事务写一条审计");
    assert_eq!(
        rig.text("SELECT state FROM work_session WHERE id = (SELECT id FROM work_session)"),
        "recovering",
        "坏钟把会话推向 recovering（P2 既有语义）"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ③-a 提交：新 epoch、新 run、旧 epoch 的写入被拒
// ─────────────────────────────────────────────────────────────────────────────

/// 计划："恢复后 `data_epoch` 与恢复前不同、`run_id` 是新的" + "旧 epoch 的写入被拒
/// （`DATA_EPOCH_MISMATCH`）且不写入" + "成功后驱动仅绑定新运行态"。
#[test]
fn a_successful_restore_commits_a_new_epoch_and_a_new_run() {
    let rig = launch();
    create_task(&rig, "备份时刻的事实").unwrap();
    let session = start_timer(&rig);
    let artifact = take_backup(&rig);
    let backup_revision = rig.revision();

    // 备份之后继续改库：恢复必须把它**退回去**（这才说明恢复真的换了库）。
    create_task(&rig, "备份之后的改动").unwrap();
    let old_epoch = rig.epoch();
    let old_run = rig.run_id();
    let old_revision = rig.revision();
    let old_runs = rig.scalar("SELECT COUNT(*) FROM application_run");

    let outcome = restore_from_backup(
        &rig.app(),
        rig.broadcaster(),
        &artifact,
        Some(&rig.backup_dir),
        &rig.clock_source(),
    )
    .expect("恢复应当成功");
    assert!(outcome.committed, "这条路径是提交");

    // 全新身份：epoch 与 run_id 都换了。
    let new_epoch = rig.epoch();
    assert_ne!(new_epoch, old_epoch, "必须生成全新的 data_epoch");
    assert_eq!(outcome.data_epoch, new_epoch, "返回的 epoch 就是库里的那个");
    assert_ne!(outcome.run_id, old_run, "必须建新的 run");
    assert_eq!(rig.run_id(), outcome.run_id);
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM application_run"),
        old_runs + 1,
        "新 run 是**新增**的一行，旧 run 的行保留"
    );

    // 事实退回到备份时刻：备份之后的改动不在了，备份前的还在。
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM task WHERE title = '备份之后的改动'"),
        0,
        "恢复必须丢弃备份之后的改动"
    );
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM task WHERE title = '备份时刻的事实'"),
        1
    );
    // revision 只在新 epoch 内比较（00 §5），而且**不继承**备份之后那次业务写的 +1。
    // 它可能比备份时刻多 1：备份里那条 `running` 会话被 P3 的扫描归一了，而
    // 「扫描事务若实际改变会话状态 ⇒ revision 增加一次并记录 time_edit」
    // （02 §"启动扫描的版本与审计补充"）——那是恢复路径**应当**有的那一次。
    assert!(
        rig.revision() >= backup_revision,
        "恢复后的 revision 至少包含备份里的事实：{} < {backup_revision}",
        rig.revision()
    );
    assert!(
        rig.revision() <= old_revision,
        "恢复后的 revision 不得高于原库（新 epoch 内重新起算）"
    );

    // 备份里那条 `running` 会话**不得**还占着全局的 `uq_running_foreground`
    // （两步重扫的第一步：P3 的四类归一）。
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM work_session WHERE state = 'running'"),
        0,
        "旧 run 的 running 会话必须被归一，不能继续占着全局前台唯一索引"
    );
    assert!(
        session_repo::running_foreground(rig.db().connection())
            .unwrap()
            .is_none(),
        "全局前台唯一索引必须已经空出来"
    );
    assert_eq!(
        rig.db()
            .connection()
            .query_row(
                "SELECT state FROM work_session WHERE id = ?1",
                [&session],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "recovering",
        "它变成外来事实，等用户确认（F-015）"
    );

    // 驱动绑定**新**运行态：`AppState` 里的协调器就是新 run 的那个。
    let app = rig.app();
    {
        let state = lock_app(&app);
        assert_eq!(state.coordinator().unwrap().run_id(), outcome.run_id);
        assert!(
            state.coordinator().unwrap().live().is_none(),
            "新协调器不继承旧计时基线"
        );
        assert!(state.runtime_present());
        assert!(state.maintenance().is_none(), "维护态已退出");
        assert_eq!(
            state.recovery(),
            &outcome.recovery,
            "门禁快照就是返回的那一份"
        );
    }

    // 广播：一条带**新 epoch** 的 `domain.changed`（客户端据此重新握手）。
    // **回滚副本可被发现**（fix round 1 的 Minor 1）：提交成功后它是"恢复前那个世界"
    // 的唯一完整副本，路径必须从结果里拿得到，而且要真的在磁盘上。
    let rollback = outcome
        .rollback
        .clone()
        .expect("提交路径必须报出回滚副本的路径");
    assert!(
        rollback.exists(),
        "回滚副本必须真的在磁盘上：{}",
        rollback.display()
    );
    assert_eq!(
        rollback,
        rig.db_path.with_file_name(format!(
            "{}.restore-rollback",
            rig.db_path.file_name().unwrap().to_string_lossy()
        ))
    );

    let broadcast = rig.events();
    let last = broadcast.last().expect("恢复完成必须广播一条");
    assert_eq!(last.event, "domain.changed");
    assert_eq!(last.data_epoch, new_epoch, "广播的必须是新 epoch");
    assert_eq!(
        last.revision,
        rig.revision(),
        "广播的 revision 就是提交后的那个"
    );

    // **旧 epoch 的写请求**：拒绝且不写入（四件事里的三条 + 那一行不存在）。
    let before = rig.facts();
    let refused = {
        let mut state = lock_app(&app);
        commands::create_task_impl(
            &mut state,
            rig.broadcaster(),
            commands::CreateTaskRequest {
                expected_data_epoch: old_epoch.clone(),
                title: "旧 epoch 的写入".to_string(),
                project_id: None,
            },
        )
        .expect_err("旧 epoch 的写入必须被拒")
    };
    assert_eq!(refused.code(), "DATA_EPOCH_MISMATCH");
    assert_eq!(rig.facts(), before, "被拒的写入必须零写入");
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM task WHERE title = '旧 epoch 的写入'"),
        0,
        "不得落进任何一行"
    );
    assert_eq!(rig.text("SELECT title FROM task WHERE id = 't1'"), "任务一");
}

// ─────────────────────────────────────────────────────────────────────────────
// ③-b 回滚：候选库不合法 / 提交失败
// ─────────────────────────────────────────────────────────────────────────────

/// 计划："恢复失败时原库可打开、原工时事实保留且重建状态符合恢复规则"。
///
/// 候选库**通过了** ② 段的三条验证、在 ③-a 的 `rotate_epoch` 上失败（没有 `app_meta`
/// 行 ⇒ 影响行数 0）⇒ 回滚：原库改回来、重开、新 run、两步重扫、新协调器。
#[test]
fn a_failed_commit_rolls_back_to_the_original_library_without_the_old_baseline() {
    let rig = launch();
    let session = start_timer(&rig);
    let old_epoch = rig.epoch();
    let old_run = rig.run_id();
    let started_at: i64 = rig
        .db()
        .connection()
        .query_row(
            "SELECT started_at FROM work_session WHERE id = ?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    let candidate = candidate_without_identity(rig._dir.path(), "candidate.db");

    let error = restore_from_backup(
        &rig.app(),
        rig.broadcaster(),
        &candidate,
        Some(&rig.backup_dir),
        &rig.clock_source(),
    )
    .expect_err("没有库身份的候选库必须让提交失败");
    assert_eq!(error.code(), "STORAGE_ERROR");
    assert!(
        format!("{error:?}").contains("rotate_epoch"),
        "失败原因必须点明是 rotate_epoch 的影响行数：{error:?}"
    );

    // 原库回来了：路径上还是它，身份没变。
    assert_eq!(rig.epoch(), old_epoch, "回滚必须保留原 epoch");
    assert_eq!(
        rig.db()
            .connection()
            .query_row(
                "SELECT started_at FROM work_session WHERE id = ?1",
                [&session],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        started_at,
        "原工时事实一条都不能丢"
    );

    // 重建状态符合恢复规则：新 run、旧会话成了外来事实（recovering）、门禁关着。
    let new_run = rig.run_id();
    assert_ne!(new_run, old_run, "回滚路径也要建新 run");
    assert_eq!(
        rig.db()
            .connection()
            .query_row(
                "SELECT state FROM work_session WHERE id = ?1",
                [&session],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "recovering",
        "原 running 会话被归一成 recovering（符合恢复规则，不是自动修复）"
    );
    let app = rig.app();
    {
        let state = lock_app(&app);
        assert_eq!(state.coordinator().unwrap().run_id(), new_run);
        assert!(
            state.coordinator().unwrap().live().is_none(),
            "**不得**继承旧计时基线（新协调器、新 run）"
        );
        assert!(
            state.recovery().requires_recovery(),
            "门禁必须关着：有未处理的恢复事实"
        );
        assert!(state.maintenance().is_none(), "维护态已退出");
        // 门禁挡住新计时（判据与 P3 一致，不另写一套）。
        assert_eq!(
            state.guard_business_timing().unwrap_err().code(),
            "RECOVERY_REQUIRED"
        );
    }

    // 广播的是**原 epoch**：客户端据此知道恢复没发生。
    let last = rig.events().last().cloned().expect("回滚也要广播一条");
    assert_eq!(last.event, "domain.changed");
    assert_eq!(last.data_epoch, old_epoch);
    assert_eq!(rig.epoch(), old_epoch, "AppState 上的身份与库一致");
}

/// 候选库本身不合法（不是 SQLite 文件）⇒ ② 段就拒绝，**原库一个字节都没动**。
#[test]
fn a_candidate_that_fails_validation_leaves_the_original_untouched() {
    let rig = launch();
    create_task(&rig, "原库的事实").unwrap();
    let old_epoch = rig.epoch();
    let before = rig.facts();
    let old_run = rig.run_id();

    let corrupt = rig._dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"this is not a sqlite database at all").unwrap();

    let error = restore_from_backup(
        &rig.app(),
        rig.broadcaster(),
        &corrupt,
        Some(&rig.backup_dir),
        &rig.clock_source(),
    )
    .expect_err("坏候选库必须被拒绝");
    assert_eq!(error.code(), "STORAGE_ERROR");

    // 原库可打开、身份未变、业务事实未变。
    assert_eq!(rig.epoch(), old_epoch);
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM task WHERE title = '原库的事实'"),
        1
    );
    assert_ne!(rig.run_id(), old_run, "运行态照样重建（新 run）");
    let after = rig.facts();
    assert_eq!(
        Facts {
            runs: before.runs,
            ..after
        },
        before,
        "除了新 run 那一行，原库的业务事实必须一字不差"
    );
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM work_session WHERE state = 'recovering'"),
        0,
        "没有未完成的会话时，重建不该凭空造出 recovering 记录"
    );
}

/// 未来版本的候选库 ⇒ ② 段拒绝（02 §9："未来版本拒绝"）。
#[test]
fn a_future_schema_candidate_is_rejected() {
    let rig = launch();
    let old_epoch = rig.epoch();
    let before = rig.facts();

    let future = rig._dir.path().join("future.db");
    {
        let db = Db::open(&future).unwrap();
        migrate(db.connection()).unwrap();
        db.connection()
            .execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
            .unwrap();
    }

    let error = restore_from_backup(
        &rig.app(),
        rig.broadcaster(),
        &future,
        Some(&rig.backup_dir),
        &rig.clock_source(),
    )
    .expect_err("未来版本的库必须被拒绝");
    assert_eq!(error.code(), "STORAGE_ERROR");
    assert!(
        format!("{error:?}").contains("newer than this build"),
        "失败原因要点明版本方向：{error:?}"
    );
    assert_eq!(rig.epoch(), old_epoch);
    assert_eq!(
        Facts {
            runs: before.runs,
            ..rig.facts()
        },
        before,
        "被拒绝的候选库不得留下任何业务痕迹"
    );
}

/// 计划 / 02 §9："schema 版本（未来版本拒绝，**旧版本先备份再迁移**）"。
///
/// 候选库是 `user_version = 0` 的空库 ⇒ ② 段先写出一份**迁移前**的一致备份（阶段标记
/// `pre-restore backup`），再 `migrate` 到当前版本。它随后在 ③-a 上失败（迁移不写
/// `app_meta` 行 ⇒ `rotate_epoch` 影响 0 行）⇒ 回滚，原库一字不动。
///
/// 这条同时是 `services::backup::backup_consistent` 返回值"由谁消费"的第二半证据：
/// 恢复路径把产物路径记进结果/诊断，而原库不受影响。
#[test]
fn an_older_schema_candidate_is_backed_up_before_it_is_migrated() {
    let rig = launch();
    create_task(&rig, "原库的事实").unwrap();
    let old_epoch = rig.epoch();
    let before = rig.facts();

    // 空库文件：`Db::open` 建出来时 `user_version = 0`（< SCHEMA_VERSION），
    // 但**不** `migrate`——它就是"旧版本"。
    let candidate = rig._dir.path().join("v0.db");
    {
        let db = Db::open(&candidate).unwrap();
        assert_eq!(current_version(db.connection()).unwrap(), 0);
    }

    let error = restore_from_backup(
        &rig.app(),
        rig.broadcaster(),
        &candidate,
        Some(&rig.backup_dir),
        &rig.clock_source(),
    )
    .expect_err("迁移后仍没有库身份的候选库必须让提交失败");
    assert_eq!(error.code(), "STORAGE_ERROR");

    // 迁移前那份备份**确实写出来了**：文件名里的 `s0` 就是被备份库的 user_version。
    let mut artifacts: Vec<String> = std::fs::read_dir(&rig.backup_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("worktrace-f") && name.ends_with(".db"))
        .collect();
    artifacts.sort();
    assert_eq!(artifacts.len(), 1, "恰好一份产物：{artifacts:?}");
    assert!(
        artifacts[0].contains("-s0-"),
        "产物名必须记下**被备份库**的 version（0）：{}",
        artifacts[0]
    );
    // 它可独立打开并通过完整性校验（与迁移前备份同一条原语、同一套判据）。
    let artifact = rig.backup_dir.join(&artifacts[0]);
    let db = Db::open(&artifact).expect("迁移前备份必须能独立打开");
    let integrity: String = db
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");

    // 原库一字不动（只有那一行新 run）。
    assert_eq!(rig.epoch(), old_epoch);
    assert_eq!(
        Facts {
            runs: before.runs,
            ..rig.facts()
        },
        before,
        "被拒绝的候选库不得改动原库的业务事实"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 与在途采样/心跳互斥
// ─────────────────────────────────────────────────────────────────────────────

/// 计划："在途心跳/异常分割与恢复互斥"。
///
/// 有**正在跑**的会话（采样每拍都在 tick，30 秒到点会 heartbeat），10ms 节拍之下
/// 连续恢复三次：每次都必须是"要么整拍被丢弃、要么看到的是完整的某一侧"，
/// 库的一致性不带伤、事实一条不丢。
#[test]
fn an_in_flight_heartbeat_cannot_race_the_restore() {
    let rig = launch_with_interval(10);
    let session = start_timer(&rig);
    let artifact = take_backup(&rig);

    for round in 0..3 {
        let outcome = restore_from_backup(
            &rig.app(),
            rig.broadcaster(),
            &artifact,
            Some(&rig.backup_dir),
            &rig.clock_source(),
        )
        .unwrap_or_else(|error| panic!("第 {round} 轮恢复失败：{error:?}"));
        assert!(outcome.committed);

        let db = rig.db();
        let integrity: String = db
            .connection()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok", "第 {round} 轮之后库必须完好");
        let violations: i64 = db
            .connection()
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0, "第 {round} 轮之后不得有外键违规");
        // 归一之后**没有** running 会话占着全局索引（异常分割那条路不越过恢复）。
        assert_eq!(
            db.connection()
                .query_row(
                    "SELECT COUNT(*) FROM work_session WHERE state = 'running'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0,
            "第 {round} 轮之后不得留下 running 会话"
        );
        assert_eq!(
            db.connection()
                .query_row(
                    "SELECT COUNT(*) FROM work_session WHERE id = ?1",
                    [&session],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1,
            "第 {round} 轮之后那条会话仍在（事实不丢）"
        );
    }
    assert!(rig.running.sampling_ticks() > 0, "采样线程全程在跑");
}

/// 计划："成功后驱动仅绑定新运行态"。
///
/// 恢复之后 `ticks` 照涨（调度器**没有**被 stop/重启），而且健康的新运行态不报错：
/// 采样拍照常工作、没有任何错误累积。
#[test]
fn after_a_restore_the_driver_keeps_running_against_the_new_runtime() {
    let rig = launch_with_interval(10);
    let artifact = take_backup(&rig);
    let outcome = restore_from_backup(
        &rig.app(),
        rig.broadcaster(),
        &artifact,
        Some(&rig.backup_dir),
        &rig.clock_source(),
    )
    .expect("恢复应当成功");

    let ticks_before = rig.running.sampling_ticks();
    wait_for_ticks(&rig.running, ticks_before + 3);
    assert!(
        !rig.running.sampling_died_unexpectedly(),
        "采样线程必须还活着"
    );
    assert_eq!(
        rig.running.sampling_errors(),
        0,
        "健康的新运行态下采样不该出错"
    );

    // 新的运行态就是新 run。
    let app = rig.app();
    let state = lock_app(&app);
    assert_eq!(state.coordinator().unwrap().run_id(), outcome.run_id);
    assert!(
        state.coordinator().unwrap().tick_seq() > 0,
        "tick 计数在新运行态上继续前进"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// `abort_restore` 的独立入口（P8 的恢复失败路径会走它）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn abort_restore_reinstalls_the_original_library() {
    let rig = launch();
    create_task(&rig, "原库独有的事实").unwrap();
    let old_epoch = rig.epoch();
    let old_run = rig.run_id();
    let app = rig.app();

    // 候选库 = 当前库的拷贝，再塞一条**只有候选库才有**的事实。
    // 没有这条标记，"装回原库"与"留着候选库"在断言上不可区分
    // （候选是原库的 `VACUUM INTO` 拷贝，epoch/行数全同：评审 Minor 3）。
    let artifact = take_backup(&rig);
    {
        let candidate = Db::open(&artifact).unwrap();
        candidate
            .connection()
            .execute(
                "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
                 VALUES('t-candidate','候选库独有的事实','Ready',0,1000,1000)",
                [],
            )
            .unwrap();
    }

    let runtime = begin_restore(&app).expect("进入维护态");
    let swap = prepare_and_swap(
        runtime,
        &artifact,
        Some(&rig.backup_dir),
        &rig.clock_source(),
        &rig.diagnostics(),
    )
    .unwrap_or_else(|failure| panic!("② 段应当成功：{:?}", failure.error));
    assert!(swap.is_swapped(), "② 段成功之后文件已经切换过");
    // 切换之后主库路径上**就是候选库**：那条独有事实在。
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM task WHERE id = 't-candidate'"),
        1,
        "切换之后主库路径上应当是候选库"
    );

    let outcome =
        abort_restore(&app, rig.broadcaster(), swap, &rig.clock_source()).expect("③-b 回滚");
    assert!(!outcome.committed);
    assert_eq!(outcome.data_epoch, old_epoch, "回滚保留原 epoch");
    assert_ne!(outcome.run_id, old_run, "回滚也要建新 run");
    assert_eq!(rig.epoch(), old_epoch);
    assert_eq!(
        outcome.rollback, None,
        "回滚路径上副本被消耗掉（改名回主库路径），所以没有可发现的副本"
    );
    // **判别力**：候选库那条独有事实必须不在（把 `rollback_files` 改成空操作，
    // 这条断言会红），而原库自己的事实必须在。
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM task WHERE id = 't-candidate'"),
        0,
        "回滚之后主库路径上必须**不是**候选库"
    );
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM task WHERE title = '原库独有的事实'"),
        1,
        "原库的事实必须还在"
    );
    assert!(
        rig.db().connection().is_autocommit(),
        "原库可以正常打开（连接不是挂在半个事务上）"
    );
    let last = rig.events().last().cloned().expect("回滚要广播");
    assert_eq!(last.data_epoch, old_epoch);
}

/// **回滚必须回到原来那间库**（fix round 2 的 Minor 1）：身份判据在 `install_runtime`
/// **之前**判，所以它报错时运行态还没有装回——进程留在维护态（一切写入被拒），
/// 而不是"装上了半个运行态、再回头去把库文件换掉"。
///
/// 形态：把回滚副本（"恢复之前那个世界"）的 `data_epoch` 改掉，模拟"改回来的那份不是
/// 原来那间库"⇒ ③-b 必须拒绝上线：不装运行态、不广播、留在维护态。
#[test]
fn abort_restore_refuses_a_library_whose_identity_changed() {
    let rig = launch();
    create_task(&rig, "原库的事实").unwrap();
    let app = rig.app();
    let events_before = rig.events().len();

    let runtime = begin_restore(&app).expect("进入维护态");
    let artifact = take_backup(&rig);
    let swap = prepare_and_swap(
        runtime,
        &artifact,
        Some(&rig.backup_dir),
        &rig.clock_source(),
        &rig.diagnostics(),
    )
    .unwrap_or_else(|failure| panic!("② 段应当成功：{:?}", failure.error));
    assert!(swap.is_swapped(), "② 段成功之后文件已经切换过");

    // 把回滚副本的身份改掉：它已经不是"恢复之前那间库"了。
    {
        let rollback = Db::open(swap.rollback_path()).unwrap();
        rollback
            .connection()
            .execute("UPDATE app_meta SET data_epoch = 'not-the-original'", [])
            .unwrap();
    }

    let error = abort_restore(&app, rig.broadcaster(), swap, &rig.clock_source())
        .expect_err("回滚回来的库身份不对时必须拒绝上线");
    assert_eq!(error.code(), "STORAGE_ERROR");
    assert!(
        format!("{error:?}").contains("wrong library identity"),
        "错误必须点明身份不对：{error:?}"
    );
    {
        let state = lock_app(&app);
        assert!(
            state.maintenance().is_some(),
            "判据在装卸之前 ⇒ 仍留在维护态（不是先上线、再回头换库文件）"
        );
        assert!(!state.runtime_present(), "没有装上半个运行态");
    }
    assert_eq!(rig.events().len(), events_before, "被拒绝的回滚不得广播");
}

/// **恢复流程的逐入口判据：主库路径上没有库时，回滚必须拒绝打开它**
/// （fix round 1 的 Important；口径是**逐入口**而不是"永不造库"——见
/// `services::backup` 里 `RestoreSwap::require_library_in_place` 的注释：
/// 本模块封不住既有的 `Db::open`，所以 [commit_restore] 与 [abort_restore] 各判一次）。
///
/// 为什么这是必需的：`Db::open` 走 `Connection::open`，文件不存在时它会**创建一个空文件**；
/// 而空文件的 `user_version = 0` 会被下一次启动当成"需要迁移的旧库"⇒ 备份它（空的）→
/// 迁移 → `init_meta` ⇒ 用户的数据被静默换成一间空库。
///
/// 形态是可达的：② 段失败之后（切换从未发生、`swapped = false`）原库文件若因为任何
/// 外部原因不在了（杀毒隔离、同步工具、用户手动删），`abort_restore` 就是最后一道门。
#[test]
fn abort_restore_refuses_to_create_a_missing_library() {
    let rig = launch();
    create_task(&rig, "原库的事实").unwrap();
    let app = rig.app();

    let runtime = begin_restore(&app).expect("进入维护态");
    let corrupt = rig._dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"this is not a sqlite database at all").unwrap();
    let failure = prepare_and_swap(
        runtime,
        &corrupt,
        Some(&rig.backup_dir),
        &rig.clock_source(),
        &rig.diagnostics(),
    )
    .expect_err("坏候选库必须被拒绝");
    assert!(!failure.swap.is_swapped(), "切换从未发生");

    // 外部原因让原库文件不见了（用例直接删掉它来模拟）。
    std::fs::remove_file(&rig.db_path).unwrap();

    let error = abort_restore(&app, rig.broadcaster(), failure.swap, &rig.clock_source())
        .expect_err("主库路径上没有库时，回滚不得凭空造一个");

    // 三件事一起断言，**先断最要命的那条**：
    // ② `Db::open` 没有替我们创建一个空文件（`Connection::open` 的默认行为）；
    assert!(
        !rig.db_path.exists(),
        "恢复流程不得创建数据库文件（空库会被下一次启动当成待迁移的旧库 ⇒ 静默换库）"
    );
    // ① 错误点明回滚副本的路径，用户才找得到数据；
    assert_eq!(error.code(), "STORAGE_ERROR");
    let detail = format!("{error:?}");
    assert!(
        detail.contains("restore-rollback"),
        "错误必须点明回滚副本的路径，用户才找得到数据：{detail}"
    );
    // ③ 进程**不留在"以为库是好的"状态**（仍在维护态、运行态不在手 ⇒
    //    任何写入都被 `DATA_RESTORE_IN_PROGRESS` 拒）。
    {
        // 块作用域：guard 必须在 `RunningApp` 之前释放（`Scheduler::Drop` 要 join 采样线程，
        // 而采样线程每一拍都要取这把锁）。
        let state = lock_app(&app);
        assert!(
            state.maintenance().is_some(),
            "拿不到原库 ⇒ 留在维护态（拒绝一切写入，而不是打开一间空库）"
        );
        assert!(!state.runtime_present());
    }
}

/// **提交路径同样不许造库**（fix round 2 的 Important）。
///
/// 形态与上一条对称，但发生在 ② 段**成功之后**：候选库已经在主库路径上，若它在 ③ 之前
/// 被外部抹掉，`commit_restore` 的 `Db::open` 会创建一个**空文件**——`start_run`/
/// `rotate_epoch` 随即失败，而**空库留在主库路径上**；下一次启动看到
/// "文件在 + `user_version = 0`"照样会把它当成待迁移的旧库 ⇒ 用户数据被静默换掉。
///
/// 断言：commit 返回 Err、**主库文件没有被创建**、进程留在维护态（不谎报成功），
/// 且原库完好地躺在回滚副本里（把它改名回来就能读到全部事实）。
#[test]
fn commit_restore_refuses_to_create_a_missing_library() {
    let rig = launch();
    create_task(&rig, "原库的事实").unwrap();
    let old_epoch = rig.epoch();
    let app = rig.app();

    let runtime = begin_restore(&app).expect("进入维护态");
    let artifact = take_backup(&rig);
    let swap = prepare_and_swap(
        runtime,
        &artifact,
        Some(&rig.backup_dir),
        &rig.clock_source(),
        &rig.diagnostics(),
    )
    .unwrap_or_else(|failure| panic!("② 段应当成功：{:?}", failure.error));
    assert!(swap.is_swapped(), "② 段成功之后文件已经切换过");

    // 外部原因让刚就位的候选库文件不见了（用例直接删掉它来模拟）。
    std::fs::remove_file(&rig.db_path).unwrap();
    // 这条用例之前已经有过业务写（`create_task`）⇒ 广播流不是空的，
    // 断言"没有**新的**广播"才是对的。
    let events_before = rig.events().len();

    let error = commit_restore(&app, rig.broadcaster(), &swap, &rig.clock_source())
        .expect_err("目标文件不在时，提交不得凭空造一个");
    assert!(
        !rig.db_path.exists(),
        "提交路径不得创建数据库文件（空库会被下一次启动当成待迁移的旧库 ⇒ 静默换库）"
    );
    assert_eq!(error.code(), "STORAGE_ERROR");
    let detail = format!("{error:?}");
    assert!(
        detail.contains("restore-rollback"),
        "错误必须点明原库（回滚副本）的路径：{detail}"
    );
    {
        let state = lock_app(&app);
        assert!(
            state.maintenance().is_some(),
            "拒绝之后仍留在维护态：不谎报成功、也不把半个运行态装上"
        );
        assert!(!state.runtime_present());
    }
    assert_eq!(
        rig.events().len(),
        events_before,
        "被拒绝的提交不得广播任何东西"
    );

    // **原库完好**：它躺在回滚副本里，改名回来就能读到全部事实（拒绝没有破坏任何东西）。
    let rollback = swap.rollback_path().to_path_buf();
    assert!(
        rollback.exists(),
        "回滚副本必须还在：{}",
        rollback.display()
    );
    std::fs::rename(&rollback, &rig.db_path).unwrap();
    assert_eq!(rig.epoch(), old_epoch);
    assert_eq!(
        rig.scalar("SELECT COUNT(*) FROM task WHERE title = '原库的事实'"),
        1,
        "原库的事实一条都不少"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// P6 终审修复波：锁重入防线（M-2）、失败落盘（I-2）、恢复之后的同源时钟（I-1）
// ─────────────────────────────────────────────────────────────────────────────

/// **恢复不得在持锁线程上开始**（P6 终审 M-2）。
///
/// 防线原先是 `debug_assert!`，而 release 会把它整条编译掉 ⇒ P8 一旦把恢复放进
/// `run_command` 闭包（正持锁）里调用，现象是**静默死锁**（① 段的 `lock_app` 永远等不到）。
/// 判据换成与 `RunningApp::shutdown` 同一条标准的硬判据。
///
/// 断言：返回 `STORAGE_ERROR`（既不 panic 也不卡住），**拒绝时不碰任何东西**——
/// 维护态没置位、运行态还在手、库身份没变、业务事实一条没动。
#[test]
fn restore_refuses_to_start_on_the_thread_that_holds_the_serial_boundary() {
    let rig = launch();
    create_task(&rig, "原库的事实").unwrap();
    let app = rig.app();
    let epoch_before = rig.epoch();
    let revision_before = rig.revision();
    let artifact = take_backup(&rig);

    let error = {
        // 本线程**正持着**串行边界：恢复内部要再取一次锁，走进去就是自死锁。
        let _guard = lock_app(&app);
        restore_from_backup(
            &app,
            rig.broadcaster(),
            &artifact,
            Some(&rig.backup_dir),
            &rig.clock_source(),
        )
        .expect_err("持锁线程上开始恢复必须被拒绝")
    };
    assert_eq!(error.code(), "STORAGE_ERROR");
    assert!(
        format!("{error:?}").contains("锁外"),
        "拒绝文案要说清正确姿势（在锁外调用）：{error:?}"
    );

    {
        let state = lock_app(&app);
        assert!(state.maintenance().is_none(), "拒绝时不进维护态");
        assert!(state.runtime_present(), "运行态没有被取走");
    }
    assert_eq!(rig.epoch(), epoch_before, "库身份没被动过");
    assert_eq!(rig.revision(), revision_before, "一次被拒的调用不得改版本");
}

/// **恢复失败必须落盘**（P6 终审 I-2）：两条失败臂各写一条 `event=restore.failed`，
/// 带 `stage=`（哪一段失败）、`rolled_back=`（原库有没有重建回来）与 `rollback=`（副本路径）。
///
/// 为什么这是必需的：原先只有成功路径写 `restore.finished` ⇒「恢复失败**且回滚也失败**」
/// （原库没重建回来、进程永久停在维护态、托盘拒绝退出）在日志里与"正在恢复"不可区分。
#[test]
fn a_failed_restore_records_its_stage_and_rollback_copy() {
    let rig = launch();
    create_task(&rig, "原库的事实").unwrap();

    // ① ② 段失败：候选库根本不是 SQLite 文件（验证就拒，原库一个字节都没动）。
    let corrupt = rig._dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"this is not a sqlite database at all").unwrap();
    let error = restore_from_backup(
        &rig.app(),
        rig.broadcaster(),
        &corrupt,
        Some(&rig.backup_dir),
        &rig.clock_source(),
    )
    .expect_err("坏候选库必须被拒绝");
    assert_eq!(error.code(), "STORAGE_ERROR");
    let line = rig
        .last_log_line("restore.failed")
        .expect("② 段失败必须写 restore.failed");
    assert!(
        line.contains("stage=prepare_and_swap"),
        "哪一段失败：{line}"
    );
    assert!(line.contains("rolled_back=true"), "原库重建回来了：{line}");
    assert!(line.contains("code=STORAGE_ERROR"), "code 要落盘：{line}");
    assert!(
        line.contains("rollback=") && line.contains(".restore-rollback"),
        "回滚副本路径要落盘：{line}"
    );

    // ③-a 失败：候选库结构合法但没有库身份（`rotate_epoch` 影响行数不对）⇒ 提交失败、回滚。
    let candidate = candidate_without_identity(rig._dir.path(), "candidate.db");
    let error = restore_from_backup(
        &rig.app(),
        rig.broadcaster(),
        &candidate,
        Some(&rig.backup_dir),
        &rig.clock_source(),
    )
    .expect_err("没有库身份的候选库必须让提交失败");
    assert_eq!(error.code(), "STORAGE_ERROR");
    let line = rig
        .last_log_line("restore.failed")
        .expect("③ 段失败必须写 restore.failed");
    assert!(line.contains("stage=commit_restore"), "哪一段失败：{line}");
    assert!(line.contains("rolled_back=true"), "原库重建回来了：{line}");
    assert!(line.contains("code=STORAGE_ERROR"), "code 要落盘：{line}");
    assert!(
        line.contains(".restore-rollback"),
        "回滚副本路径要落盘：{line}"
    );
}

/// 恢复用的那只钟从哪来（[`boundary_state_after_restore`] 的两种形态）。
enum RestoreClock {
    /// **生产形态**：组合根那一份（`RunningApp::clock_source()`），与 OS 事件源同源。
    FromRunningApp,
    /// **I-1 描述的故障形态**：另建一只 `SystemClock`（两个 `Instant` 原点）。
    FreshInstance,
}

/// 走一遍「真启动 → 备份 → 恢复 → 起计时 → 锁屏边界」，返回边界之后那条会话的状态。
///
/// 时钟形状按**生产**摆：组合根建一只手 `SystemClock`，克隆一份交给 `startup`（协调器），
/// 原件留给 `RunningApp`；OS 事件源在生产里拿的是**同一个实例**的又一份克隆，所以这里
/// 直接用那只钟取边界样本（`lib.rs::setup` 的 `event_clock` 就是它）。
fn boundary_state_after_restore(which: RestoreClock) -> String {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");
    let backup_dir = dir.path().join("backups");
    {
        let mut db = Db::open(&db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        meta::init_meta(&tx).unwrap();
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务一','Ready',0,1000,1000)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let clock = SystemClock::new();
    let mut config = StartupConfig::new(&db_path, &lock_path).with_backup_dir(&backup_dir);
    config.sampling_interval_ms = IDLE_SAMPLING_MS;
    let sink = Arc::new(RecordingSink::default());
    let mut running = match startup(
        config,
        Box::new(clock.clone()),
        Arc::clone(&sink) as Arc<dyn EventSink>,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };
    // 组合根那一句（`lib.rs::setup`）：启动成功后把同源句柄挂给 P8。
    running.attach_clock(clock.clone());

    let source: ClockSource = match which {
        RestoreClock::FromRunningApp => running.clock_source().expect("组合根已挂同源时钟"),
        RestoreClock::FreshInstance => {
            Box::new(|| Box::new(SystemClock::new()) as Box<dyn Clock + Send>)
        }
    };

    // 备份（恢复的输入）：此刻**没有任何会话**，所以恢复之后门禁是开的。
    let artifact = {
        let db = Db::open(&db_path).unwrap();
        let version = current_version(db.connection()).unwrap();
        let naming_clock = source();
        backup_consistent(
            Some(&backup_dir),
            db.connection(),
            version,
            &*naming_clock,
            "i1 backup",
            &Diagnostics::disabled(),
        )
        .expect("备份应当成功")
    };

    // **先让真实时间走一段**：组合根那只钟的 `monotonic` 必须明显大于恢复时新建的钟
    // （负控的判别力就来自这个差；生产里这个差就是"进程已经跑了多久"）。
    std::thread::sleep(Duration::from_millis(50));

    let app = Arc::clone(running.app());
    let outcome = restore_from_backup(&app, running.broadcaster(), &artifact, None, &source)
        .expect("恢复应当成功");
    assert!(outcome.committed, "这条路径是提交");

    // 新 epoch 从库里读（`RunningApp::data_epoch()` 是启动快照，恢复之后过期）。
    let epoch = meta::read_meta(Db::open(&db_path).unwrap().connection())
        .unwrap()
        .expect("app_meta 已初始化")
        .data_epoch;
    let session_id = {
        let mut state = lock_app(&app);
        state
            .start(StartRequest {
                expected_data_epoch: epoch,
                task_id: "t1".to_string(),
                task_expected_version: 0,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            })
            .expect("恢复之后应当能起计时（备份里没有未完成会话）")
            .snapshot
            .session_id
            .expect("开始之后有会话")
    };

    // 锁屏：边界样本取自**组合根那只钟**（生产里 OS 事件源拿的就是它的克隆）。
    std::thread::sleep(Duration::from_millis(20));
    let boundary = clock.sample().expect("真实时钟可用");
    {
        let mut state = lock_app(&app);
        state
            .system_boundary(Some(boundary))
            .expect("边界处理本身是一次成功调用（接受或转入待确认）");
    }

    Db::open(&db_path)
        .unwrap()
        .connection()
        .query_row(
            "SELECT state FROM work_session WHERE id = ?1",
            [&session_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
}

/// **恢复之后 OS 边界仍然可信**（P6 终审 I-1）。
///
/// 组合根把唯一那份 `SystemClock` 再克隆一份挂在 `RunningApp` 上，恢复建的新协调器从
/// `RunningApp::clock_source()` 取钟 ⇒ 与 OS 事件源**同源**，锁屏边界被 `system_pause`
/// **接受**（会话转 `paused`，没有待确认区间）。
///
/// 同一条用例里的负控是**I-1 描述的生产故障形态**：恢复的时钟来源另建一只
/// `SystemClock`（"全仓 `ClockSource` 只在测试里被构造"时 P8 唯一写得出来的形状）
/// ⇒ 两个 `Instant` 原点，同一条边界被拒，会话掉进 `recovering`。两条一起断言，
/// 这条用例才不是恒真断言（sabotage：把 `clock_source` 换成新建一只钟，第一条立即红）。
#[test]
fn a_restore_keeps_the_os_boundary_trusted_because_the_clock_is_shared() {
    assert_eq!(
        boundary_state_after_restore(RestoreClock::FromRunningApp),
        "paused",
        "同源时钟 ⇒ 锁屏边界被接受（R-02 成立）"
    );
    assert_eq!(
        boundary_state_after_restore(RestoreClock::FreshInstance),
        "recovering",
        "另建一只 SystemClock ⇒ 边界被拒（这正是 I-1 的生产故障形态）"
    );
}
