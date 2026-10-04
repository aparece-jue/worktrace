//! P7 Task 0：周期采样驱动（F-009）。
//!
//! 计划原文：「**无窗口引用时周期采样仍被驱动**且空闲不写库」。
//! 这里的「无窗口」不是模拟出来的：采样驱动是一个只认进程的后台线程，
//! 测试进程里**根本没有窗口对象**，窗口回调在启动时返回一次 `Ok(())` 之后就不存在了。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, SharedApp, Startup, StartupConfig,
};
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

const WALL: i64 = 1_700_000_000_000;
/// 采样节拍：测试里压到 10ms，免得每个用例都等一秒。
const INTERVAL_MS: u64 = 10;

struct Harness {
    /// 先声明：Drop 时先停采样线程、再删临时目录。
    running: Box<RunningApp>,
    app: SharedApp,
    clock: Arc<Mutex<FakeClock>>,
    db_path: PathBuf,
    epoch: String,
    _dir: tempfile::TempDir,
}

/// 建库（元数据 + 给定任务），再走**真实启动入口**。
fn harness(tasks: &[&str]) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");

    let mut db = Db::open(&db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    for id in tasks {
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES(?1,'任务','Ready',0,1000,1000)",
            [id],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(db);

    let clock = Arc::new(Mutex::new(FakeClock::new(WALL, 0)));
    let mut config = StartupConfig::new(&db_path, &lock_path);
    config.sampling_interval_ms = INTERVAL_MS;

    let running = match startup(
        config,
        Box::new(Arc::clone(&clock)),
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };

    let app = Arc::clone(running.app());
    let epoch = running.data_epoch().to_string();

    Harness {
        running,
        app,
        clock,
        db_path,
        epoch,
        _dir: dir,
    }
}

impl Harness {
    fn start(&self, task_id: &str) -> Result<(), AppError> {
        let mut state = lock_app(&self.app);
        state
            .start(StartRequest {
                expected_data_epoch: self.epoch.clone(),
                task_id: task_id.to_string(),
                task_expected_version: 0,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 1_000,
            })
            .map(|_| ())
    }

    fn open_db(&self) -> Db {
        Db::open(&self.db_path).unwrap()
    }

    fn scalar(&self, sql: &str) -> i64 {
        self.open_db()
            .connection()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }

    /// 等采样驱动跑够 `n` 拍（10ms 一拍，给足 10 秒）。
    fn wait_for_ticks(&self, n: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.running.sampling_ticks() < n {
            assert!(
                Instant::now() < deadline,
                "采样驱动没有按周期触发：只跑了 {} 拍",
                self.running.sampling_ticks()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 等到谓词成立（或超时返回 false）。
    fn wait_until(&self, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

/// 窗口全关仍在跑：进程里没有任何窗口对象，采样照样被驱动。
#[test]
fn the_sampler_keeps_ticking_with_no_window_anywhere() {
    let h = harness(&[]);

    h.wait_for_ticks(5);
    let first = h.running.sampling_ticks();
    h.wait_for_ticks(first + 5);

    assert!(
        h.running.sampling_ticks() > first,
        "采样驱动必须在没有窗口的情况下继续跑"
    );
    assert_eq!(h.running.sampling_errors(), 0, "空闲采样不该报错");
}

/// 空闲（无活动会话）**不产生任何写入**。
#[test]
fn idle_sampling_writes_nothing_and_notifies_nothing() {
    let h = harness(&[]);
    h.wait_for_ticks(2);

    let before_changes = h.scalar("SELECT total_changes()");
    let before_rows = table_rows(&h);
    let before_revision = h.scalar("SELECT revision FROM app_meta WHERE singleton = 1");

    for _ in 0..5 {
        lock_app(&h.app).sample_tick().unwrap();
    }

    assert_eq!(
        h.scalar("SELECT total_changes()"),
        before_changes,
        "空闲采样一拍都不该写库（心跳、tick 都不加 revision）"
    );
    assert_eq!(table_rows(&h), before_rows, "任何表的行数都不该变");
    assert_eq!(
        h.scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        before_revision,
        "空闲不得制造 revision"
    );
}

/// 采样驱动也驱动**检查点心跳**：30 秒到期后写出带进度的检查点。
#[test]
fn the_scheduler_drives_the_checkpoint_heartbeat() {
    let h = harness(&["t1"]);
    h.start("t1").unwrap();
    let revision_after_start = h.scalar("SELECT revision FROM app_meta WHERE singleton = 1");

    // start 会写一个 elapsed=0 的初始检查点。
    let before = h.scalar("SELECT COUNT(*) FROM interval_checkpoint");

    // 推进 30 秒（挂钟与单调钟一起走，不构成异常）。
    h.clock.lock().unwrap().advance_both(30_000);

    let progressed = h.wait_until(|| {
        h.scalar("SELECT COALESCE(MAX(elapsed_ms), 0) FROM interval_checkpoint") >= 30_000
    });
    assert!(
        progressed,
        "心跳到期后应当由采样驱动写出带进度的检查点（初始检查点数 {before}）"
    );
    assert_eq!(
        h.scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        revision_after_start,
        "心跳不加 revision"
    );
}

/// 用户命令与周期采样走**同一把锁**：并发 start 只可能一个成功。
#[test]
fn commands_and_sampling_share_one_serial_boundary() {
    let h = harness(&["t1", "t2"]);
    // 先确认采样驱动真的在跑，否则「它还在跑」这条断言会变成计时竞态。
    h.wait_for_ticks(1);
    let ticks_before = h.running.sampling_ticks();
    let barrier = Arc::new(std::sync::Barrier::new(2));

    let results: Vec<Result<(), AppError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = ["t1", "t2"]
            .into_iter()
            .map(|task| {
                let app = Arc::clone(&h.app);
                let epoch = h.epoch.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    let mut state = lock_app(&app);
                    state
                        .start(StartRequest {
                            expected_data_epoch: epoch,
                            task_id: task.to_string(),
                            task_expected_version: 0,
                            mode: SessionMode::Foreground,
                            timer_kind: TimerKind::Stopwatch,
                            target_duration_ms: None,
                            expected_interval_ms: 1_000,
                        })
                        .map(|_| ())
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let succeeded = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(succeeded, 1, "前台槽位只有一个：{results:?}");

    let refused = results
        .iter()
        .find_map(|r| r.as_ref().err())
        .expect("另一个必须被拒");
    assert_eq!(
        refused.code(),
        "DOMAIN_ERROR",
        "被串行边界挡住的第二个 start 应当拿到可预期的领域冲突，而不是唯一索引的存储错误"
    );

    h.wait_for_ticks(ticks_before + 1);
    assert!(
        h.running.sampling_ticks() > ticks_before,
        "并发命令期间采样驱动仍在跑：{ticks_before} -> {}",
        h.running.sampling_ticks()
    );
}

/// 所有表的行数快照：空闲采样必须让它一动不动。
fn table_rows(h: &Harness) -> Vec<(String, i64)> {
    let db = h.open_db();
    let mut stmt = db
        .connection()
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    drop(stmt);

    names
        .into_iter()
        .map(|name| {
            let count: i64 = db
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {name}"), [], |r| r.get(0))
                .unwrap();
            (name, count)
        })
        .collect()
}
