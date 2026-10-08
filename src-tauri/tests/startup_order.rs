//! P7 Task 0：启动顺序、单实例与恢复门禁。
//!
//! 计划原文要求：「副作用次序用注入的探针记录并在 `tests/startup_order.rs` 断言调用
//! 次序——不是断言"没崩"」。所以这里的断言落在**步骤序列**上，而不是「启动成功」。
//!
//! 另外三条：第二次启动**不打开库、不迁移、不建 run**；锁持有者被**强杀**后新进程
//! 能拿到锁；`run_id <> 当前 run` 的三件事决定恢复门禁。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::platform::single_instance::{self, InstanceLock};
use worktrace_lib::services::backup::BACKUP_FORMAT_VERSION;
use worktrace_lib::services::bootstrap::{
    lock_app, scan_recovery, startup, PreMigrationBackup, Startup, StartupConfig, StartupProbe,
    StartupStep,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::{current_version, migrate, user_tables, SCHEMA_VERSION};
use worktrace_lib::storage::run_repo;
use worktrace_lib::storage::session_repo::InvariantFault;

/// 假时钟的初始挂钟：`application_run.started_at` 必须等于它。
const WALL: i64 = 1_700_000_000_000;

/// 探针记到的**一条**事件：启动步骤，或迁移前备份的按需判据结果。
///
/// 两者记进**同一条流**：只记步骤的话，「备份完成 → `migrate` 开始」这个顺序
/// 就断言不出来（步骤里没有备份这一条，`StartupStep` 也不该为它加变体——
/// `ALL` 的条数由另一个用例的四重不变量钉着）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeEvent {
    Step(StartupStep),
    Backup(PreMigrationBackup),
}

#[derive(Default)]
struct RecordingProbe {
    events: Mutex<Vec<ProbeEvent>>,
}

impl StartupProbe for RecordingProbe {
    fn step(&self, step: StartupStep) {
        self.events.lock().unwrap().push(ProbeEvent::Step(step));
    }

    fn pre_migration_backup(&self, outcome: PreMigrationBackup) {
        self.events
            .lock()
            .unwrap()
            .push(ProbeEvent::Backup(outcome));
    }
}

impl RecordingProbe {
    fn events(&self) -> Vec<ProbeEvent> {
        self.events.lock().unwrap().clone()
    }

    /// 只看步骤（既有用例的断言口径）。
    fn steps(&self) -> Vec<StartupStep> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                ProbeEvent::Step(step) => Some(step),
                ProbeEvent::Backup(_) => None,
            })
            .collect()
    }

    /// 只看迁移前备份的按需判据结果。
    fn backups(&self) -> Vec<PreMigrationBackup> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                ProbeEvent::Backup(outcome) => Some(outcome),
                ProbeEvent::Step(_) => None,
            })
            .collect()
    }
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

impl Fixture {
    /// 临时目录本身：注入的备份目录、以及「让某个路径不可用」的注入都放在它下面。
    fn dir(&self) -> &Path {
        self._dir.path()
    }
}

fn started(fx: &Fixture) -> Box<worktrace_lib::services::bootstrap::RunningApp> {
    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    let outcome = startup(
        // 备份目录**一律注入**临时目录：即便某个用例的库已经存在、按需判据恰好为真，
        // 产物也只落在临时目录里——测试绝不写进开发机真实的数据目录。
        StartupConfig::new(&fx.db_path, &fx.lock_path).with_backup_dir(fx.dir().join("backups")),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
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
    let sink = Arc::new(RecordingSink::default());
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
        sink,
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
    let sink = Arc::new(RecordingSink::default());
    let outcome = startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
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

/// `StartupStep::ALL` 的**唯一消费者**：把「启动步骤有几条」钉成测试事实，
/// 而不是注释里的一句话。
///
/// 四条断言各防一种漂移：
///
/// 1. `ALL.len() == 9` —— 防从 `ALL` 里**删条目**（删一条立即红）；
/// 2. `ALL` 无重复 —— 防把同一条登记两遍冒充「九条」；
/// 3. `as_str()` 两两不同 —— 防两个变体共用同一个诊断名（日志与断言再也分不清步骤）；
/// 4. 两条路径**实测**步骤的并集 == `ALL` 集合（双向）—— 防「新步骤只接进
///    `startup()`、忘了登记进 `ALL`」，以及反向的「`ALL` 里躺着一条两条路径都
///    走不到的幽灵步骤」。正常启动 8 步、拿锁失败 2 步，并集正好 9 条。
///
/// 防不到的一种也写出来，别当它不存在：给枚举新增变体、却既没接进任何路径、
/// 也没登记进 `ALL`。Rust 稳定版无法枚举变体（`std::mem::variant_count` 仍是
/// nightly，本仓也不引派生宏），没有测试能替作者数变体个数。拦它的是编译器：
/// `as_str()` 的穷尽 `match` 会编译失败，逼作者回到 `bootstrap.rs` 动手；而只要
/// 他把新变体接进任一条路径，断言 4 就红。
#[test]
fn startup_step_all_is_the_union_of_what_both_paths_record() {
    // 1. 条数：计划里的六步拆成九条记录（①拆两条、②拆两条、⑤拆两条）。
    assert_eq!(
        StartupStep::ALL.len(),
        9,
        "StartupStep::ALL 必须是 9 条——正常启动 8 步 ∪ 拿锁失败 2 步"
    );

    // 2. 无重复：同一个变体不得在 ALL 里出现两次。
    let mut distinct: Vec<StartupStep> = Vec::new();
    for step in StartupStep::ALL {
        assert!(
            !distinct.contains(&step),
            "StartupStep::ALL 里有重复条目：{:?}",
            step
        );
        distinct.push(step);
    }

    // 3. 诊断名互不相同：`as_str()` 是日志与断言里的稳定名字，撞名就没法定位步骤。
    let mut names: Vec<&str> = Vec::new();
    for step in StartupStep::ALL {
        assert!(
            !names.contains(&step.as_str()),
            "两个变体映射到同一个诊断名：{}",
            step.as_str()
        );
        names.push(step.as_str());
    }

    // 4. 两条路径**实测**（不抄字面量）：拿锁失败 2 步、正常启动 8 步。
    let fx = fixture();

    // 拿锁失败路径：测试自己持锁，模拟既有实例。
    let held = InstanceLock::acquire(&fx.lock_path)
        .unwrap()
        .expect("测试自己先持锁，模拟既有实例");
    let blocked_probe = RecordingProbe::default();
    let blocked_sink = Arc::new(RecordingSink::default());
    let outcome = startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        blocked_sink,
        &blocked_probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("拿不到锁不是错误，是一条正常分支");
    assert!(
        matches!(outcome, Startup::AlreadyRunning { .. }),
        "持锁时第二次启动必须走 AlreadyRunning 分支"
    );
    let blocked_steps = blocked_probe.steps();
    assert_eq!(
        blocked_steps.len(),
        2,
        "拿锁失败路径记录 2 步：单实例检查 + 通知既有实例"
    );
    drop(held);

    // 正常启动路径：锁已放开；同一个 fixture（失败路径没碰过库）。
    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    let running = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("锁已放开，本进程应当是唯一实例"),
    };
    let ok_steps = probe.steps();
    assert_eq!(
        ok_steps.len(),
        8,
        "正常启动记录 8 步（不含拿锁失败路径的 ExistingInstanceNotified）"
    );
    drop(running);

    // 两条路径的并集（按变体去重）。
    let mut union = ok_steps.clone();
    for step in &blocked_steps {
        if !union.contains(step) {
            union.push(*step);
        }
    }

    // 方向一：路径记到的每一步都必须在 ALL 里。
    for step in &union {
        assert!(
            StartupStep::ALL.contains(step),
            "启动路径记录了 StartupStep::ALL 之外的步骤：{:?}",
            step
        );
    }
    // 方向二：ALL 里每一条都必须被某条路径记到——不是走不到的幽灵步骤。
    for step in StartupStep::ALL {
        assert!(
            union.contains(&step),
            "StartupStep::ALL 里的步骤两条路径都没记录：{:?}",
            step
        );
    }
}

/// 开窗口失败 ⇒ 启动失败（不留下一个还在跑的采样线程与一把没人放的锁）。
#[test]
fn a_failing_window_open_aborts_the_start_and_releases_the_lock() {
    let fx = fixture();
    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());

    let err = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
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
    let before = total_changes(state.db());
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
        total_changes(state.db()),
        before,
        "被门禁拒绝的请求不得写入任何东西（不采样、不建会话、不加 revision）"
    );
    assert_eq!(
        state
            .db()
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

// ─────────────────────────────────────────────────────────────────────────────
// 迁移前的按需一致备份（P6 Task 1 编排）
//
// 三条按需分支**按原因**分开断言，不许合并成一条「都没备份」：一个「备份函数恒返回
// Ok 且什么都不做」的实现会让「无需迁移」那条全绿，所以「需迁移 ⇒ 有产物」必须与它
// 成对存在。判据读 `PRAGMA user_version`（`current_version`），不是 `meta::read_meta`
// ——新库在 `migrate` 之前根本没有 `app_meta` 表。
// ─────────────────────────────────────────────────────────────────────────────

/// 迁移前的库会带上这张表；迁移后才有的 `app_meta` 则是「备份到底在迁移前还是后」
/// 的对照物。名字刻意不叫 `app_meta`/`application_run`，免得与 v1 schema 撞。
const PRE_MIGRATION_TABLE: &str = "pre_migration_marker";

/// 造一个**需要迁移**的既有库：库文件存在、`user_version == 0`，并留下一行事实。
fn database_needing_migration(path: &Path) {
    let db = Db::open(path).expect("建库文件");
    assert_eq!(
        current_version(db.connection()).unwrap(),
        0,
        "「需要迁移」的判据是 user_version < SCHEMA_VERSION"
    );
    db.connection()
        .execute_batch(&format!(
            "CREATE TABLE {PRE_MIGRATION_TABLE}(note TEXT NOT NULL);
             INSERT INTO {PRE_MIGRATION_TABLE}(note) VALUES('迁移前的事实');"
        ))
        .unwrap();
}

/// 造一个**已经是当前版本**的既有库（`migrate` 对它就是空操作）。返回库身份。
fn database_at_current_version(path: &Path) -> String {
    let mut db = Db::open(path).unwrap();
    migrate(db.connection()).unwrap();
    assert_eq!(current_version(db.connection()).unwrap(), SCHEMA_VERSION);
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    tx.commit().unwrap();
    meta.data_epoch
}

/// 注入的备份目录：与库、锁同在临时目录下——**绝不写进真实的 `%APPDATA%`**。
fn injected_backup_dir(fx: &Fixture) -> PathBuf {
    fx.dir().join("backups")
}

/// 目录里的条目名（升序）。目录不存在 ⇒ 空：这就是「零产物」的判据。
fn entry_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// 迁移前备份在注入目录里的产物名（与 `entry_names` 同一口径，只是过滤出备份）。
fn backup_names(dir: &Path) -> Vec<String> {
    entry_names(dir)
        .into_iter()
        .filter(|name| name.starts_with("worktrace-f") && name.ends_with(".db"))
        .collect()
}

fn marker_note(db: &Db) -> Option<String> {
    db.connection()
        .query_row(
            &format!("SELECT note FROM {PRE_MIGRATION_TABLE}"),
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
}

fn has_table(db: &Db, table: &str) -> bool {
    user_tables(db.connection())
        .unwrap()
        .iter()
        .any(|name| name == table)
}

fn foreign_key_violations(db: &Db) -> Vec<String> {
    let mut stmt = db.connection().prepare("PRAGMA foreign_key_check").unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn sqlite_version(db: &Db) -> String {
    db.connection()
        .query_row("SELECT sqlite_version()", [], |r| r.get::<_, String>(0))
        .unwrap()
}

/// ① **需迁移 ⇒ 确实写出一份备份**，而且它是**迁移前**的一致快照：
/// 顺序断言落在探针的同一条事件流上（拿到锁 → 打开库 → 备份完成 → `migrate` 开始），
/// 内容断言落在产物自己身上（迁移前的版本与事实都在，v1 的表一张都没有）。
#[test]
fn a_database_that_needs_migration_is_backed_up_before_it_is_migrated() {
    let fx = fixture();
    let backup_dir = injected_backup_dir(&fx);
    database_needing_migration(&fx.db_path);
    assert!(fx.db_path.exists(), "既有库：库文件在启动前就存在");

    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    let window_calls = Arc::new(AtomicUsize::new(0));
    let open_window = {
        let counter = Arc::clone(&window_calls);
        move || -> Result<(), AppError> {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    };

    let running = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path).with_backup_dir(&backup_dir),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &open_window,
    )
    .expect("迁移前备份成功，启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("首个进程应当是唯一实例"),
    };

    // 「拿到单实例锁 → 备份完成 → migrate 开始」：四条**相邻**事件，不多不少。
    assert_eq!(
        probe.events()[..4],
        [
            ProbeEvent::Step(StartupStep::SingleInstanceChecked),
            ProbeEvent::Step(StartupStep::DatabaseOpened),
            ProbeEvent::Backup(PreMigrationBackup::Taken),
            ProbeEvent::Step(StartupStep::Migrated),
        ],
        "备份必须夹在「打开库」与「迁移」之间"
    );

    // 恰好一份产物；名字里三个版号与 Unix 毫秒各就各位。
    let names = backup_names(&backup_dir);
    assert_eq!(names.len(), 1, "需迁移时恰好一份备份：{names:?}");
    assert_eq!(
        names[0],
        format!(
            "worktrace-f{BACKUP_FORMAT_VERSION}-s0-v{}-{WALL}.db",
            env!("CARGO_PKG_VERSION")
        ),
        "文件名：格式版号 / 数据库版号（迁移前）/ 应用版本 / Unix 毫秒"
    );

    // 产物**可独立打开**，并通过完整性与版本校验。
    let artifact = backup_dir.join(&names[0]);
    let backup = Db::open(&artifact).expect("备份产物必须能独立打开");
    let integrity: String = backup
        .connection()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok", "备份产物必须通过完整性校验");
    assert_eq!(foreign_key_violations(&backup), Vec::<String>::new());
    assert_eq!(
        current_version(backup.connection()).unwrap(),
        0,
        "备份的是**迁移前**的库版本——这正是「先备份后迁移」的顺序证据"
    );
    assert_eq!(
        marker_note(&backup).as_deref(),
        Some("迁移前的事实"),
        "备份必须带上迁移前的事实"
    );
    assert!(
        !has_table(&backup, "app_meta"),
        "迁移前备份里不该有 v1 的表（若备份发生在 migrate 之后，这条会红）"
    );
    // `VACUUM INTO` 的可用性证据：bundled SQLite 版本随用例输出（`--nocapture` 可见）。
    println!("bundled sqlite: {}", sqlite_version(&backup));

    // 真库确实迁移了，且事实没丢。
    let db = Db::open(&fx.db_path).unwrap();
    assert_eq!(current_version(db.connection()).unwrap(), SCHEMA_VERSION);
    assert!(has_table(&db, "app_meta"));
    assert_eq!(marker_note(&db).as_deref(), Some("迁移前的事实"));
    assert_eq!(window_calls.load(Ordering::SeqCst), 1, "成功启动照常开窗口");
    assert!(!running.recovery().requires_recovery());
}

/// ② **`user_version == SCHEMA_VERSION` ⇒ 零备份产物**，且 `migrate` 被调用（空操作）。
#[test]
fn a_database_already_at_the_current_version_produces_no_backup() {
    let fx = fixture();
    let backup_dir = injected_backup_dir(&fx);
    let epoch_before = database_at_current_version(&fx.db_path);
    let tables_before = {
        let db = Db::open(&fx.db_path).unwrap();
        user_tables(db.connection()).unwrap()
    };

    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    let running = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path).with_backup_dir(&backup_dir),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("无需迁移也是一种成功启动")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("首个进程应当是唯一实例"),
    };

    assert_eq!(
        probe.backups(),
        vec![PreMigrationBackup::NotNeeded],
        "按需判据走的是「无需迁移」这条分支"
    );
    assert!(
        probe.steps().contains(&StartupStep::Migrated),
        "版本相等时 migrate 仍被调用（幂等空操作），否则就是静默跳过迁移"
    );
    assert_eq!(
        backup_names(&backup_dir),
        Vec::<String>::new(),
        "无需迁移 ⇒ 零产物"
    );
    assert!(
        !backup_dir.exists(),
        "连备份目录都不该被建出来：解析只发生在「确实需要迁移」时"
    );

    // 空操作：版本、表集合、库身份一个都没动。
    let db = Db::open(&fx.db_path).unwrap();
    assert_eq!(current_version(db.connection()).unwrap(), SCHEMA_VERSION);
    assert_eq!(user_tables(db.connection()).unwrap(), tables_before);
    assert_eq!(
        running.data_epoch(),
        epoch_before,
        "migrate 是空操作，不得重建库身份"
    );
}

/// ③ **库文件不存在（首启）⇒ 零产物**，走的是「没有可备份的事实」这条分支。
///
/// 注意新库的 `user_version == 0` **属于「需要迁移」**：它在这里被挡下的原因是
/// 库文件不存在，不是版本相等——两者的原因不同，所以断言也必须分开。
#[test]
fn a_first_launch_has_no_database_to_back_up() {
    let fx = fixture();
    let backup_dir = injected_backup_dir(&fx);
    assert!(!fx.db_path.exists(), "首启的前提：库文件不存在");

    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    let running = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path).with_backup_dir(&backup_dir),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("首启没有可备份的事实，不是降级")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("首个进程应当是唯一实例"),
    };

    assert_eq!(
        probe.backups(),
        vec![PreMigrationBackup::NothingToBackUp],
        "按需判据走的是「没有可备份的事实」这条分支"
    );
    assert!(
        probe.steps().contains(&StartupStep::Migrated),
        "首启仍然要迁移：新库 user_version == 0 属于「需要迁移」"
    );
    assert_eq!(
        backup_names(&backup_dir),
        Vec::<String>::new(),
        "首启零产物"
    );
    assert!(!backup_dir.exists(), "首启不该建出备份目录");

    let db = Db::open(&fx.db_path).unwrap();
    assert_eq!(current_version(db.connection()).unwrap(), SCHEMA_VERSION);
    assert!(has_table(&db, "app_meta"));
    assert_eq!(running.data_epoch().len(), 36, "新库在这里取得库身份");
}

/// ④ **需迁移但备份失败 ⇒ 拒绝迁移**：`migrate` 零调用、`STORAGE_ERROR`、
/// 不建 `application_run`、不开窗口、库停在迁移前版本、锁被放开。
///
/// 这一条是三条分支里唯一「必须失败」的：不降级、不跳过、不先迁移后补。
/// 备份目录的父级被做成一个**普通文件**，所以失败发生在写任何东西之前。
#[test]
fn a_backup_that_cannot_be_written_refuses_to_migrate() {
    let fx = fixture();
    database_needing_migration(&fx.db_path);
    let blocked = fx.dir().join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let backup_dir = blocked.join("backups");

    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    let window_calls = Arc::new(AtomicUsize::new(0));
    let open_window = {
        let counter = Arc::clone(&window_calls);
        move || -> Result<(), AppError> {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    };

    let err = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path).with_backup_dir(&backup_dir),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &open_window,
    ) {
        Err(error) => error,
        Ok(_) => panic!("备份失败不得算一次成功启动"),
    };

    assert_eq!(err.code(), "STORAGE_ERROR", "复用既有码，不新增第六个");
    assert!(
        err.detail()
            .unwrap_or("")
            .starts_with("pre-migration backup"),
        "detail 必须带阶段标记：{:?}",
        err.detail()
    );
    assert_eq!(
        probe.steps(),
        vec![
            StartupStep::SingleInstanceChecked,
            StartupStep::DatabaseOpened
        ],
        "失败发生在第②步内部：migrate 零调用、后面每一步都没走"
    );
    assert!(
        probe.backups().is_empty(),
        "没走完的判据不报结果（原因由错误本身带）"
    );
    assert_eq!(window_calls.load(Ordering::SeqCst), 0, "不开窗口");

    // 库停在迁移前：只有那张迁移前的事实表，一张 v1 的表都没有。
    let db = Db::open(&fx.db_path).unwrap();
    assert_eq!(
        current_version(db.connection()).unwrap(),
        0,
        "库停在迁移前版本"
    );
    let tables = user_tables(db.connection()).unwrap();
    assert_eq!(
        tables,
        vec![PRE_MIGRATION_TABLE.to_string()],
        "不得留下半套 schema：{tables:?}"
    );
    assert!(
        !tables.iter().any(|table| table == "application_run"),
        "不得建 application_run"
    );
    assert!(
        InstanceLock::acquire(&fx.lock_path).unwrap().is_some(),
        "启动中途失败必须放开锁"
    );
}

/// ④ 之二：`VACUUM INTO` 自己失败（同名产物已经在那里）**同样拒绝迁移**，
/// 且**不覆盖**既有产物——备份宁可失败也不静默盖掉上一份。
#[test]
fn a_backup_target_that_already_exists_refuses_to_migrate() {
    let fx = fixture();
    database_needing_migration(&fx.db_path);
    let backup_dir = injected_backup_dir(&fx);
    std::fs::create_dir_all(&backup_dir).unwrap();
    let taken = backup_dir.join(format!(
        "worktrace-f{BACKUP_FORMAT_VERSION}-s0-v{}-{WALL}.db",
        env!("CARGO_PKG_VERSION")
    ));
    std::fs::write(&taken, b"existing artifact").unwrap();

    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    let err = match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path).with_backup_dir(&backup_dir),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    ) {
        Err(error) => error,
        Ok(_) => panic!("同名产物已在时不得继续迁移"),
    };

    assert_eq!(err.code(), "STORAGE_ERROR");
    assert!(
        err.detail()
            .unwrap_or("")
            .starts_with("pre-migration backup"),
        "detail 必须带阶段标记：{:?}",
        err.detail()
    );
    assert!(
        !probe.steps().contains(&StartupStep::Migrated),
        "migrate 零调用"
    );
    assert_eq!(
        std::fs::read(&taken).unwrap(),
        b"existing artifact".to_vec(),
        "不得覆盖既有产物"
    );
    assert_eq!(backup_names(&backup_dir).len(), 1, "没有第二份产物");

    let db = Db::open(&fx.db_path).unwrap();
    assert_eq!(
        current_version(db.connection()).unwrap(),
        0,
        "库停在迁移前版本"
    );
}

/// 保留策略：**只留最新 5 份**（按文件名里的 Unix 毫秒），
/// **清理失败只记诊断**（最老的那份是个删不掉的目录，启动照样成功），
/// 且认不出的名字一律不碰。
#[test]
fn the_retention_keeps_the_newest_five_and_survives_an_undeletable_entry() {
    let fx = fixture();
    database_needing_migration(&fx.db_path);
    let backup_dir = injected_backup_dir(&fx);
    std::fs::create_dir_all(&backup_dir).unwrap();

    let old = |at_ms: i64| backup_dir.join(format!("worktrace-f1-s1-v0.0.9-{at_ms}.db"));
    // 最老的那份是**目录**：`remove_file` 删不掉它（两个平台都不行）。
    std::fs::create_dir(old(1_000)).unwrap();
    for at_ms in [2_000, 3_000, 4_000, 5_000, 6_000] {
        std::fs::write(old(at_ms), b"old artifact").unwrap();
    }
    // 保留策略只认自己写出来的名字：这两份必须原样留着。
    std::fs::write(backup_dir.join("notes.txt"), b"keep me").unwrap();
    std::fs::write(
        backup_dir.join("worktrace-f1-s1-v0.0.9-notatimestamp.db"),
        b"keep me too",
    )
    .unwrap();

    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    // 本轮写出第 7 份（时间戳最新）⇒ 该删掉最老的两份：目录（失败）与 2000（成功）。
    match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path).with_backup_dir(&backup_dir),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("清理失败不得把一次成功启动变成失败")
    {
        Startup::Running(_) => {}
        Startup::AlreadyRunning { .. } => panic!("首个进程应当是唯一实例"),
    };

    let mut expected = vec![
        "notes.txt".to_string(),
        "worktrace-f1-s1-v0.0.9-1000.db".to_string(),
        "worktrace-f1-s1-v0.0.9-3000.db".to_string(),
        "worktrace-f1-s1-v0.0.9-4000.db".to_string(),
        "worktrace-f1-s1-v0.0.9-5000.db".to_string(),
        "worktrace-f1-s1-v0.0.9-6000.db".to_string(),
        "worktrace-f1-s1-v0.0.9-notatimestamp.db".to_string(),
        format!(
            "worktrace-f{BACKUP_FORMAT_VERSION}-s0-v{}-{WALL}.db",
            env!("CARGO_PKG_VERSION")
        ),
    ];
    expected.sort();
    assert_eq!(
        entry_names(&backup_dir),
        expected,
        "只留最新 5 份；删不掉的目录与认不出的名字原样保留"
    );
    assert!(old(1_000).is_dir(), "删不掉的那份仍在（清理失败只记诊断）");
    assert!(!old(2_000).exists(), "最老的**文件**被清掉了");
}

/// 保留策略**永不删掉刚写出的那一份**（Task 1 评审留下的那条）：
/// 纯按文件名里的挂钟毫秒排序时，系统时间被回拨、且已有 ≥5 份 ⇒ 刚写出的那份时间戳
/// 最小，会被当成「最老」删掉——现象是「启动成功、日志说已备份、产物却没了」。
///
/// 本条构造的正是那个形状：5 份既有产物的文件名时间戳**全都比本轮更晚**。
#[test]
fn the_retention_never_removes_the_artifact_it_just_wrote() {
    let fx = fixture();
    database_needing_migration(&fx.db_path);
    let backup_dir = injected_backup_dir(&fx);
    std::fs::create_dir_all(&backup_dir).unwrap();

    let named = |at_ms: i64| backup_dir.join(format!("worktrace-f1-s1-v0.0.9-{at_ms}.db"));
    let existing: Vec<i64> = (1..=5).map(|i| WALL + i * 1_000).collect();
    for at_ms in &existing {
        std::fs::write(named(*at_ms), b"previous artifact").unwrap();
    }

    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    match startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path).with_backup_dir(&backup_dir),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("保留策略不得把一次成功启动变成失败")
    {
        Startup::Running(_) => {}
        Startup::AlreadyRunning { .. } => panic!("首个进程应当是唯一实例"),
    }

    let just_written = backup_dir.join(format!(
        "worktrace-f{BACKUP_FORMAT_VERSION}-s0-v{}-{WALL}.db",
        env!("CARGO_PKG_VERSION")
    ));
    assert!(
        just_written.exists(),
        "本次产物不得被保留策略删掉：{}",
        just_written.display()
    );
    assert_eq!(backup_names(&backup_dir).len(), 5, "仍然只留 5 份");
    assert!(
        !named(existing[0]).exists(),
        "该被清掉的是最老的既有产物，而不是刚写出的那份"
    );
}
