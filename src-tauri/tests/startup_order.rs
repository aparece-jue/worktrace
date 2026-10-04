//! P7 Task 0：启动顺序、单实例与恢复门禁。
//!
//! 计划原文要求：「副作用次序用注入的探针记录并在 `tests/startup_order.rs` 断言调用
//! 次序——不是断言"没崩"」。所以这里的断言落在**步骤序列**上，而不是「启动成功」。
//!
//! 另外三条：第二次启动**不打开库、不迁移、不建 run**；锁持有者被**强杀**后新进程
//! 能拿到锁；`run_id <> 当前 run` 的三件事决定恢复门禁。

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::platform::single_instance::{self, InstanceLock};
use worktrace_lib::services::bootstrap::{
    lock_app, scan_recovery, startup, Startup, StartupConfig, StartupProbe, StartupStep,
};
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;
use worktrace_lib::storage::session_repo::InvariantFault;

/// 假时钟的初始挂钟：`application_run.started_at` 必须等于它。
const WALL: i64 = 1_700_000_000_000;

#[derive(Default)]
struct RecordingProbe {
    steps: Mutex<Vec<StartupStep>>,
}

impl StartupProbe for RecordingProbe {
    fn step(&self, step: StartupStep) {
        self.steps.lock().unwrap().push(step);
    }
}

impl RecordingProbe {
    fn steps(&self) -> Vec<StartupStep> {
        self.steps.lock().unwrap().clone()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    Fixture {
        db_path: dir.path().join("worktrace.db"),
        lock_path: dir.path().join("instance.lock"),
        _dir: dir,
    }
}

fn started(fx: &Fixture) -> Box<worktrace_lib::services::bootstrap::RunningApp> {
    let probe = RecordingProbe::default();
    let outcome = startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功");
    match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

/// 一次成功启动的副作用次序，逐条对上计划的六步（②⑤各拆成两条记录）。
#[test]
fn a_successful_start_records_the_fixed_side_effect_order() {
    let fx = fixture();
    let probe = RecordingProbe::default();
    let window_calls = Arc::new(AtomicUsize::new(0));
    let open_window = {
        let counter = Arc::clone(&window_calls);
        move || -> Result<(), AppError> {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    };

    let running = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        &probe,
        &open_window,
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("首个进程应当是唯一实例"),
    };

    assert_eq!(
        probe.steps(),
        vec![
            StartupStep::SingleInstanceChecked,
            StartupStep::DatabaseOpened,
            StartupStep::Migrated,
            StartupStep::RunCreated,
            StartupStep::RecoveryScanned,
            StartupStep::CoordinatorStarted,
            StartupStep::SamplingStarted,
            StartupStep::WindowOpened,
        ],
        "启动副作用次序固定，不得重排"
    );
    assert_eq!(window_calls.load(Ordering::SeqCst), 1, "窗口只开一次");

    // ③ 每次成功启动恰好一行 application_run，起点来自时钟采样。
    let db = Db::open(&fx.db_path).unwrap();
    let run = run_repo::get_run(db.connection(), running.run_id())
        .unwrap()
        .expect("本次启动应当留下 run 行");
    assert_eq!(run.started_at, WALL);
    assert_eq!(run.clean_exit_at, None, "还没退出，不该有 clean_exit_at");
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM application_run", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );

    // ④ 全新库没有任何历史，门禁应当是开的。
    assert!(!running.recovery().requires_recovery());
    assert_eq!(running.data_epoch().len(), 36, "库身份是 UUID");
}

/// 「第二次启动不打开库、不迁移、不建 run」。
#[test]
fn a_second_startup_notifies_the_existing_instance_and_touches_no_database() {
    let fx = fixture();
    let held = InstanceLock::acquire(&fx.lock_path)
        .unwrap()
        .expect("测试自己先持锁，模拟既有实例");

    let probe = RecordingProbe::default();
    let outcome = startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("拿不到锁不是错误，是一条正常分支");

    match outcome {
        Startup::AlreadyRunning { notified } => {
            assert!(notified, "拿不到锁的进程必须通知既有实例");
        }
        Startup::Running(_) => panic!("第二个实例不得启动"),
    }

    assert_eq!(
        probe.steps(),
        vec![
            StartupStep::SingleInstanceChecked,
            StartupStep::ExistingInstanceNotified,
        ],
        "第二个实例除了检查锁与发通知，不得做任何副作用"
    );
    assert!(
        !fx.db_path.exists(),
        "第二个实例不得打开库——连库文件都不该被创建"
    );
    assert!(
        single_instance::take_activation_request(&fx.lock_path).unwrap(),
        "既有实例应当能取到这次「唤起主窗」请求"
    );

    drop(held);
}

/// 开窗口失败 ⇒ 启动失败（不留下一个还在跑的采样线程与一把没人放的锁）。
#[test]
fn a_failing_window_open_aborts_the_start_and_releases_the_lock() {
    let fx = fixture();
    let probe = RecordingProbe::default();

    let err = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        &probe,
        &|| -> Result<(), AppError> {
            Err(AppError::Storage {
                detail: "window build failed".into(),
            })
        },
    ) {
        Err(e) => e,
        Ok(_) => panic!("窗口开不出来就不是一次成功启动"),
    };

    assert_eq!(err.code(), "STORAGE_ERROR");
    assert_eq!(
        probe.steps().last(),
        Some(&StartupStep::SamplingStarted),
        "失败路径不得记录 window_opened"
    );
    assert!(
        InstanceLock::acquire(&fx.lock_path).unwrap().is_some(),
        "启动失败必须放开锁"
    );
}

/// 每次成功启动一行 run；上一次没走显式退出就保持 `clean_exit_at IS NULL`。
#[test]
fn every_successful_start_creates_exactly_one_run_row() {
    let fx = fixture();

    let first = started(&fx);
    let first_id = first.run_id().to_string();
    drop(first); // 释放锁与采样线程，模拟进程退出（未走显式退出）

    let second = started(&fx);
    assert_ne!(second.run_id(), first_id, "每次启动换一个 run id");

    let db = Db::open(&fx.db_path).unwrap();
    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM application_run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "两次成功启动恰好两行");
    assert_eq!(
        run_repo::get_run(db.connection(), &first_id)
            .unwrap()
            .unwrap()
            .clean_exit_at,
        None,
        "没走显式退出就不该有 clean_exit_at"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 恢复门禁（第 8 条）：按 run_id <> 当前 run 查三件事
// ─────────────────────────────────────────────────────────────────────────────

/// 只统计**别的 run** 的会话：当前 run 的正常计时不能被当成恢复材料。
#[test]
fn the_recovery_scan_only_counts_sessions_of_other_runs() {
    let fx = fixture();
    let mut db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, "run-old", 1_000).unwrap();
    run_repo::start_run(&tx, "run-now", 2_000).unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    // 上个 run 的暂停会话：未结束，但没有待确认区间。
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s-old','t1','run-old','FOREGROUND','paused','stopwatch',1000,0)",
        [],
    )
    .unwrap();
    // 它的一个待确认区间（已闭合、无 duration、needs_review）。
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review)
         VALUES('iv-pending','s-old',1000,2000,NULL,1)",
        [],
    )
    .unwrap();
    // 不变量损坏 ①：running 却没有开放区间。
    // 用 BACKGROUND：`uq_running_foreground` 是**全局**唯一索引（不区分 run），
    // 而下面还要放一个属于当前 run 的正常 running 会话；V0.1 的服务层本来也不写
    // BACKGROUND，这里只是造一条事实坏掉的历史行。
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s-fault','t1','run-old','BACKGROUND','running','stopwatch',1500,0)",
        [],
    )
    .unwrap();
    // 不变量损坏 ②：paused 却残留开放区间。
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s-mixed','t1','run-old','FOREGROUND','paused','stopwatch',1600,0)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at)
         VALUES('iv-open','s-mixed',1600)",
        [],
    )
    .unwrap();
    // 当前 run 的正常会话：**一个字都不该进扫描结果**。
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s-now','t1','run-now','FOREGROUND','running','stopwatch',2000,0)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at)
         VALUES('iv-now','s-now',2000)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();

    let scan = scan_recovery(db.connection(), "run-now").unwrap();
    assert_eq!(
        scan.unfinished_sessions,
        vec![
            "s-old".to_string(),
            "s-fault".to_string(),
            "s-mixed".to_string()
        ],
        "只算别的 run 的未结束会话"
    );
    assert_eq!(scan.pending_intervals, vec!["iv-pending".to_string()]);
    assert_eq!(
        scan.invariant_faults,
        vec![
            InvariantFault {
                session_id: "s-fault".into(),
                reason: "running 会话没有开放区间",
            },
            InvariantFault {
                session_id: "s-mixed".into(),
                reason: "非 running 会话残留开放区间",
            },
        ]
    );
    assert!(scan.requires_recovery());
}

/// 门禁命中 ⇒ **拒绝业务计时**，且拒绝不写入任何东西。
#[test]
fn an_unfinished_previous_run_closes_the_gate_on_business_timing() {
    let fx = fixture();
    {
        let mut db = Db::open(&fx.db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        init_meta(&tx).unwrap();
        run_repo::start_run(&tx, "run-old", 1_000).unwrap();
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务','Ready',0,1000,1000)",
            [],
        )
        .unwrap();
        // 上一代次崩在运行中：running + 开放区间。
        tx.execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
             VALUES('s-old','t1','run-old','FOREGROUND','running','stopwatch',1000,0)",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO work_interval(id,session_id,started_at) VALUES('iv-old','s-old',1000)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let running = started(&fx);
    assert!(
        running.recovery().requires_recovery(),
        "上一代次还有未结束会话，门禁必须关着"
    );
    assert_eq!(running.recovery().unfinished_sessions, vec!["s-old"]);

    let mut state = lock_app(running.app());
    let before = total_changes(&state.db);
    let err = state
        .start(StartRequest {
            expected_data_epoch: running.data_epoch().to_string(),
            task_id: "t1".into(),
            task_expected_version: 0,
            mode: SessionMode::Foreground,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            expected_interval_ms: 1_000,
        })
        .expect_err("门禁关着时不得开始新计时");

    assert_eq!(err.code(), "RECOVERY_REQUIRED");
    assert_eq!(
        total_changes(&state.db),
        before,
        "被门禁拒绝的请求不得写入任何东西（不采样、不建会话、不加 revision）"
    );
    assert_eq!(
        state
            .db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM work_session WHERE run_id <> 'run-old'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0,
        "不得为本次 run 建会话"
    );
}

/// 读连接上的累计写入行数。空闲采样必须让它一动不动。
fn total_changes(db: &Db) -> i64 {
    db.connection()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// 锁持有者被强杀
// ─────────────────────────────────────────────────────────────────────────────

const LOCK_ENV: &str = "WORKTRACE_TEST_LOCK_FILE";

/// 辅助进程：拿到锁、报一声、然后挂在那儿等被杀。
///
/// 只有 `a_killed_lock_holder_releases_the_lock` 用 `current_exe` 显式拉起它。
#[test]
#[ignore = "helper process: 由 a_killed_lock_holder_releases_the_lock 拉起"]
fn lock_holder_helper() {
    let path = std::env::var(LOCK_ENV).expect("需要 WORKTRACE_TEST_LOCK_FILE");
    let lock = InstanceLock::acquire(&path)
        .expect("辅助进程应能访问锁文件")
        .expect("辅助进程应拿到锁");
    println!("LOCKED {}", lock.path().display());
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
fn a_killed_lock_holder_releases_the_lock() {
    let fx = fixture();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_holder_helper", "--ignored", "--nocapture"])
        .env(LOCK_ENV, &fx.lock_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("应能拉起辅助进程");

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if line.starts_with("LOCKED") {
                let _ = tx.send(());
                return;
            }
        }
    });

    let locked = rx.recv_timeout(Duration::from_secs(60));
    if locked.is_err() {
        // 辅助进程没起来也要收尸，别留下孤儿。
        let _ = child.kill();
        let _ = child.wait();
    }
    locked.expect("辅助进程应当报告自己已持锁");

    assert!(
        InstanceLock::acquire(&fx.lock_path).unwrap().is_none(),
        "持锁者活着时，别的进程拿不到锁"
    );

    child.kill().expect("强杀辅助进程");
    child.wait().expect("回收辅助进程");

    // 句柄由内核关闭，锁的释放是**异步**于子进程退出的，给一点重试窗口。
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut acquired = false;
    while Instant::now() < deadline {
        if let Some(lock) = InstanceLock::acquire(&fx.lock_path).unwrap() {
            drop(lock);
            acquired = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        acquired,
        "锁持有者被强杀后，新进程必须能拿到锁（不依赖任何清理逻辑）"
    );
}
