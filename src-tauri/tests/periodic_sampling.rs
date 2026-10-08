//! P7 Task 0：周期采样驱动（F-009）。
//!
//! 计划原文：「**无窗口引用时周期采样仍被驱动**且空闲不写库」。
//! 这里的「无窗口」不是模拟出来的：采样驱动是一个只认进程的后台线程，
//! 测试进程里**根本没有窗口对象**，窗口回调在启动时返回一次 `Ok(())` 之后就不存在了。
//!
//! ## 证据口径（fix round 1 订正）
//!
//! - **写入探针必须取在 App 自己那条连接上**：`SELECT total_changes()` 是**连接级**
//!   计数，新开一条连接去问它恒为 0（评审实测：写入连接返回 1，新连接返回 0）。
//!   所以 [`Rig::app_total_changes`] 在锁内、在 App 的连接上取——它与「全表行数」
//!   互补：行数抓 INSERT/DELETE，`total_changes` 连 UPDATE 也抓。
//! - **广播的两条性质在真实路径上验**（`sampling_action`：同一临界区内「提交 → 广播」），
//!   不是在 `Mutex<Vec>` 上摆一条顺序序列。结构性用例另见 `tests/event_protocol.rs`。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, MaintenancePhase, NoProbe, RunningApp, SharedApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{bump_revision, init_meta};
use worktrace_lib::storage::migrations::migrate;

const WALL: i64 = 1_700_000_000_000;
/// 采样节拍：测试里压到 10ms，免得每个用例都等一秒。
const INTERVAL_MS: u64 = 10;

// ─────────────────────────────────────────────────────────────────────────────
// 三种出口
// ─────────────────────────────────────────────────────────────────────────────

/// 照单全收。
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

/// 永远失败：用来证明**广播失败不回滚已提交业务**。
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

/// 广播**当时**回读库：证明广播发生在**提交之后**。
///
/// 用**另一条连接**读，不是 `lock_app`：广播就发生在临界区内部，再用同一把锁会自锁；
/// 而另一条连接看不见未提交的数据——这正是「提交后可见」的判据。
struct PostCommitSink {
    db_path: PathBuf,
    seen: Mutex<Vec<SeenAtEmit>>,
}

#[derive(Debug, Clone)]
struct SeenAtEmit {
    revision: i64,
    observed_revision: i64,
    /// 广播那一刻，库里**已经提交**的检查点的最大 `elapsed_ms`。
    committed_checkpoint_elapsed_ms: Option<i64>,
    /// **这一拍自己**报出的进度（`payload.active_ms`）。
    ///
    /// 配对断言要的就是它：只有把「这一拍报的进度」与「同一拍广播时库里已提交的进度」
    /// 对起来，才能证明广播发生在本拍的提交之后（见下面那条用例）。
    active_ms: Option<i64>,
}

impl PostCommitSink {
    fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<SeenAtEmit> {
        self.seen.lock().unwrap().clone()
    }
}

impl EventSink for PostCommitSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        let db = Db::open(&self.db_path).map_err(|e| format!("{e:?}"))?;
        let observed_revision: i64 = db
            .connection()
            .query_row(
                "SELECT revision FROM app_meta WHERE singleton = 1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let committed_checkpoint_elapsed_ms: Option<i64> = db
            .connection()
            .query_row("SELECT MAX(elapsed_ms) FROM interval_checkpoint", [], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;

        self.seen.lock().unwrap().push(SeenAtEmit {
            revision: envelope.revision,
            observed_revision,
            committed_checkpoint_elapsed_ms,
            active_ms: envelope.payload["active_ms"].as_i64(),
        });
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 样板
// ─────────────────────────────────────────────────────────────────────────────

/// 临时目录 + 库/锁路径，库里已经建好元数据与给定任务。
struct Fixture {
    dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
}

fn fixture(tasks: &[&str]) -> Fixture {
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

    Fixture {
        dir,
        db_path,
        lock_path,
    }
}

/// 跑起来的应用。**字段顺序即 Drop 顺序**：先停采样线程，再删临时目录。
struct Rig {
    running: Box<RunningApp>,
    clock: Arc<Mutex<FakeClock>>,
    db_path: PathBuf,
    epoch: String,
    run_id: String,
    _dir: tempfile::TempDir,
}

fn clock() -> Arc<Mutex<FakeClock>> {
    Arc::new(Mutex::new(FakeClock::new(WALL, 0)))
}

/// 走**真实启动入口**起一个应用。
fn launch(fx: Fixture, clock: Arc<Mutex<FakeClock>>, sink: Arc<dyn EventSink>) -> Rig {
    let mut config = StartupConfig::new(&fx.db_path, &fx.lock_path);
    config.sampling_interval_ms = INTERVAL_MS;

    let running = match startup(
        config,
        Box::new(Arc::clone(&clock)),
        sink,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };

    let db_path = fx.db_path.clone();
    let epoch = running.data_epoch().to_string();
    let run_id = running.run_id().to_string();
    Rig {
        running,
        clock,
        db_path,
        epoch,
        run_id,
        _dir: fx.dir,
    }
}

/// 记录型出口的样板（大多数用例用它）。
fn rig_recording(tasks: &[&str]) -> (Rig, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let rig = launch(
        fixture(tasks),
        clock(),
        Arc::clone(&sink) as Arc<dyn EventSink>,
    );
    (rig, sink)
}

impl Rig {
    fn app(&self) -> SharedApp {
        Arc::clone(self.running.app())
    }

    fn start(&self, task_id: &str) -> Result<(), AppError> {
        let mut state = lock_app(self.running.app());
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

    /// **另一条**连接：读全局事实（行数、已提交的检查点）。
    fn open_db(&self) -> Db {
        Db::open(&self.db_path).unwrap()
    }

    fn scalar(&self, sql: &str) -> i64 {
        self.open_db()
            .connection()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }

    /// 在锁内、在 **App 自己那条连接**上取标量。
    fn app_scalar(&self, sql: &str) -> i64 {
        lock_app(self.running.app())
            .db()
            .connection()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }

    /// App 连接上的累计写入行数。
    ///
    /// **必须在这条连接上取**：`total_changes()` 是连接级计数，新开的连接恒为 0
    /// （那等于没有断言）。它连 UPDATE 都算——行数快照抓不到的那一类。
    fn app_total_changes(&self) -> i64 {
        self.app_scalar("SELECT total_changes()")
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

// ─────────────────────────────────────────────────────────────────────────────
// 驱动本身
// ─────────────────────────────────────────────────────────────────────────────

/// 窗口全关仍在跑：进程里没有任何窗口对象，采样照样被驱动。
#[test]
fn the_sampler_keeps_ticking_with_no_window_anywhere() {
    let (rig, _sink) = rig_recording(&[]);

    rig.wait_for_ticks(5);
    let first = rig.running.sampling_ticks();
    rig.wait_for_ticks(first + 5);

    assert!(
        rig.running.sampling_ticks() > first,
        "采样驱动必须在没有窗口的情况下继续跑"
    );
    assert_eq!(rig.running.sampling_errors(), 0, "空闲采样不该报错");
}

/// 空闲（无活动会话）**不产生任何写入**，也不广播 tick。
#[test]
fn idle_sampling_writes_nothing_and_notifies_nothing() {
    let (rig, sink) = rig_recording(&[]);
    rig.wait_for_ticks(2);

    // 写入探针取在 **App 自己那条连接**上（新连接上 total_changes 恒为 0）。
    let before_changes = rig.app_total_changes();
    let before_rows = table_rows(&rig);
    let before_revision = rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1");

    for _ in 0..5 {
        lock_app(rig.running.app()).sample_tick().unwrap();
    }

    assert_eq!(
        rig.app_total_changes(),
        before_changes,
        "空闲采样一拍都不该写库：App 连接上的 total_changes 也必须一动不动"
    );
    assert_eq!(table_rows(&rig), before_rows, "任何表的行数都不该变");
    assert_eq!(
        rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        before_revision,
        "空闲不得制造 revision"
    );
    assert!(
        sink.events().is_empty(),
        "没有活动会话就没有 tick 通知：{:?}",
        sink.events()
    );
}

/// 有活动会话时，采样驱动自己跑出 tick 通知（走同一条串行边界）。
#[test]
fn an_active_session_is_ticked_and_broadcast_by_the_scheduler() {
    let (rig, sink) = rig_recording(&["t1"]);
    rig.start("t1").unwrap();
    let revision_after_start = rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1");
    assert_eq!(
        revision_after_start, 1,
        "start 是一次业务写，恰好加一次 revision"
    );

    let got_tick = rig.wait_until(|| {
        sink.events()
            .iter()
            .any(|e| e.event == "timer.tick" && e.payload["session_id"].is_string())
    });
    assert!(got_tick, "有活动会话时采样驱动应当广播 timer.tick");

    let tick = sink
        .events()
        .into_iter()
        .find(|e| e.event == "timer.tick" && e.payload["session_id"].is_string())
        .unwrap();
    assert_eq!(tick.data_epoch, rig.epoch);
    assert_eq!(tick.payload["run_id"], rig.run_id);
    assert!(
        tick.payload["tick_seq"].as_u64().unwrap() >= 1,
        "tick 的序号应当已经前进：{tick:?}"
    );
    assert_eq!(tick.revision, revision_after_start);
    assert_eq!(
        rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        revision_after_start,
        "tick 不加业务 revision"
    );
}

/// 采样驱动也驱动**检查点心跳**：30 秒到期后写出带进度的检查点。
///
/// 顺带钉住 `at` 的**来路**：它必须等于 `platform::clock` 采样到的挂钟，
/// 而不是某个回显调用方字面量的值。
#[test]
fn the_scheduler_drives_the_checkpoint_heartbeat() {
    let (rig, sink) = rig_recording(&["t1"]);
    rig.start("t1").unwrap();
    let revision_after_start = rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1");

    // start 会写一个 elapsed=0 的初始检查点。
    let before = rig.scalar("SELECT COUNT(*) FROM interval_checkpoint");

    // 推进 30 秒（挂钟与单调钟一起走，不构成异常）。
    rig.clock.lock().unwrap().advance_both(30_000);
    let expected_at = rig.clock.lock().unwrap().wall_ms();

    let progressed = rig.wait_until(|| {
        rig.scalar("SELECT COALESCE(MAX(elapsed_ms), 0) FROM interval_checkpoint") >= 30_000
    });
    assert!(
        progressed,
        "心跳到期后应当由采样驱动写出带进度的检查点（初始检查点数 {before}）"
    );
    assert_eq!(
        rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        revision_after_start,
        "心跳不加 revision"
    );

    // M1：`at` 来自时钟采样。
    let saw_expected_at = rig.wait_until(|| {
        sink.events()
            .iter()
            .any(|e| e.event == "timer.tick" && is_active_tick(e) && e.at == expected_at)
    });
    assert!(
        saw_expected_at,
        "tick 的 at 应当等于假时钟的挂钟采样 {expected_at}：{:?}",
        sink.events().iter().map(|e| e.at).collect::<Vec<_>>()
    );
}

/// 用户命令与周期采样走**同一把锁**：并发 start 只可能一个成功。
#[test]
fn commands_and_sampling_share_one_serial_boundary() {
    let (rig, _sink) = rig_recording(&["t1", "t2"]);
    // 先确认采样驱动真的在跑，否则「它还在跑」这条断言会变成计时竞态。
    rig.wait_for_ticks(1);
    let ticks_before = rig.running.sampling_ticks();
    let barrier = Arc::new(std::sync::Barrier::new(2));

    let results: Vec<Result<(), AppError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = ["t1", "t2"]
            .into_iter()
            .map(|task| {
                let app = rig.app();
                let epoch = rig.epoch.clone();
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

    rig.wait_for_ticks(ticks_before + 1);
    assert!(
        rig.running.sampling_ticks() > ticks_before,
        "并发命令期间采样驱动仍在跑：{ticks_before} -> {}",
        rig.running.sampling_ticks()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 广播的两条性质：走真实路径（sampling_action）
// ─────────────────────────────────────────────────────────────────────────────

/// **广播发生在本拍提交之后，而且与提交同处一条串行边界。**
///
/// 判据是**配对**的：出口在广播**当时**用另一条连接回读「那一刻库里已提交的检查点
/// 进度」（未提交的数据在它眼里不存在）。于是要求：
///
/// > 凡这一拍自己报出的 `active_ms` 已经越过 30 秒心跳线的观察，
/// > 同一拍读到的那份已提交进度也必须 ≥ 30 秒。
///
/// 即：客户端看到的那份进度，在它被广播出去的时候就已经落库了。
///
/// # 为什么必须配对（fix round 2 订正，评审 I2 遗留）
///
/// 旧写法只断言「**至少有一次**广播发生在带进度的检查点提交之后」。这条在实现被改坏
/// 之后**照样成立**：心跳一旦提交，`MAX(elapsed_ms)` 就停在 30_000，而采样每 10ms
/// 广播一次——之后随便哪一拍都能满足它。同理 `observed_revision >= envelope.revision`
/// 在本场景**恒真**（心跳不加 revision，提交前读到的也是同一个值），等于没断言。
/// 现在改成：那一拍自己的进度 ↔ 同一拍读到的已提交进度，并且 revision 要求**相等**
/// （本用例没有别的写者，同一临界区里读到的必须是同一个值）。
///
/// 把广播挪到本拍心跳提交之前（`sampling_action` 先 `tick` 并广播、再跑
/// `sample_tick`），`active_ms >= 30_000` 的第一条观察就会红——本轮实测见实施报告。
#[test]
fn a_tick_is_broadcast_after_the_commit_inside_the_same_boundary() {
    let fx = fixture(&["t1"]);
    let sink = Arc::new(PostCommitSink::new(fx.db_path.clone()));
    let rig = launch(fx, clock(), Arc::clone(&sink) as Arc<dyn EventSink>);
    rig.start("t1").unwrap();

    // 推进 30 秒：心跳到期。采样驱动会在**同一临界区**里「写检查点 → 提交 → 广播 tick」。
    rig.clock.lock().unwrap().advance_both(30_000);

    let reached = rig.wait_until(|| {
        sink.seen()
            .iter()
            .any(|seen| seen.active_ms.unwrap_or(0) >= 30_000)
    });
    assert!(
        reached,
        "推进 30 秒之后总该有一拍报出 >= 30_000 的 active_ms：{:?}",
        sink.seen()
    );

    let mut checked = 0;
    for seen in sink.seen() {
        // 配对：这一拍报出的进度不能领先于同一拍已提交的进度。
        if seen.active_ms.unwrap_or(0) >= 30_000 {
            checked += 1;
            assert!(
                seen.committed_checkpoint_elapsed_ms.unwrap_or(0) >= 30_000,
                "这一拍报出了 >= 30 秒的进度，广播那一刻库里却看不到对应的心跳检查点——\
                 广播跑到了本拍心跳提交之前（{seen:?}）"
            );
        }
        assert_eq!(
            seen.observed_revision, seen.revision,
            "广播必须发生在提交之后，且本用例没有别的写者：包里与库里应当读到同一个 \
             revision（{seen:?}）"
        );
    }
    assert!(checked > 0, "配对断言至少要真的检查到一条观察");
}

/// **两个写者 + 采样驱动同时广播，出口看到的 revision 序列单调非降。**
///
/// 每个写者都在**同一把锁**下完成「提交 + 广播」——这正是生产接线的形状
/// （`sampling_action` 与 Task 1 的命令都要这样）。把广播挪到锁外面，
/// 这里就会看到 revision 倒退。
///
/// fix round 2 订正：加上 `start` 之后会话是活动的，采样驱动**真的在广播 tick**
/// （在此之前夹具没有活动会话，采样一拍都不发声，这条用例其实只覆盖了两个写者）。
#[test]
fn two_writers_and_the_sampler_never_let_the_outlet_see_a_backwards_revision() {
    let (rig, sink) = rig_recording(&["t1"]);
    rig.wait_for_ticks(1);
    // 让采样驱动真的有东西可播：空闲采样不广播（F-009 的「不空转」那条）。
    rig.start("t1").unwrap();
    let broadcaster = Arc::clone(rig.running.broadcaster());

    std::thread::scope(|scope| {
        for writer in 0..2 {
            let app = rig.app();
            let broadcaster = Arc::clone(&broadcaster);
            let epoch = rig.epoch.clone();
            let db_path = rig.db_path.clone();
            scope.spawn(move || {
                // 自己的连接只用来写；**顺序**由 App 的那把锁保证。
                let mut own = Db::open(&db_path).unwrap();
                for i in 0..25 {
                    let guard = lock_app(&app);
                    let tx = own.connection_mut().unchecked_transaction().unwrap();
                    tx.execute(
                        "INSERT INTO project(id,name,row_version,status,created_at,updated_at)
                         VALUES(?1,'项目',0,'active',1,1)",
                        [format!("p-{writer}-{i}")],
                    )
                    .unwrap();
                    let revision = bump_revision(&tx).unwrap();
                    tx.commit().unwrap();

                    broadcaster.emit(EventEnvelope::domain_changed(
                        epoch.clone(),
                        revision,
                        WALL,
                        serde_json::json!({ "writer": writer, "i": i }),
                    ));
                    // guard 在这里才释放：提交与广播在同一个临界区里。
                    drop(guard);
                }
            });
        }
    });

    let revisions: Vec<i64> = sink.events().iter().map(|e| e.revision).collect();
    assert!(
        revisions.len() >= 50,
        "两个写者各 25 次提交+广播，出口至少该收到这么多条：{}",
        revisions.len()
    );
    for pair in revisions.windows(2) {
        assert!(
            pair[1] >= pair[0],
            "出口看到的 revision 序列必须单调非降，实际：{revisions:?}"
        );
    }
    assert_eq!(
        rig.running.broadcaster().diagnostics().out_of_order,
        0,
        "生产出口自己也不该记到任何一次倒退"
    );
}

/// **广播失败只记诊断，不回滚已提交业务。**
///
/// 用真实路径：活动会话 + 心跳到期 ⇒ 检查点已提交，随后 tick 的广播全部失败。
/// 断言「已提交的检查点还在、revision 没被回滚、采样本身没出错、失败被记成诊断」。
#[test]
fn a_failed_tick_broadcast_never_rolls_back_the_committed_heartbeat() {
    let fx = fixture(&["t1"]);
    let failing = Arc::new(FailingSink::default());
    let rig = launch(fx, clock(), Arc::clone(&failing) as Arc<dyn EventSink>);
    rig.start("t1").unwrap();
    let revision_after_start = rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1");

    rig.clock.lock().unwrap().advance_both(30_000);

    let committed = rig.wait_until(|| {
        rig.scalar("SELECT COALESCE(MAX(elapsed_ms), 0) FROM interval_checkpoint") >= 30_000
    });
    assert!(committed, "心跳应当已经提交");
    let failed = rig.wait_until(|| rig.running.broadcaster().diagnostics().failed >= 1);
    assert!(failed, "广播失败应当被记成诊断");
    assert!(failing.calls.load(Ordering::SeqCst) >= 1);

    // 已提交的事实没有被回滚，也没有被二次提交。
    assert!(
        rig.scalar("SELECT COALESCE(MAX(elapsed_ms), 0) FROM interval_checkpoint") >= 30_000,
        "广播失败不得回滚已经提交的检查点"
    );
    assert_eq!(
        rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        revision_after_start,
        "回滚会连 revision 一起退回去——它没有动"
    );
    assert_eq!(
        rig.app_scalar("SELECT COUNT(*) FROM work_session WHERE state = 'running'"),
        1,
        "会话仍在计时"
    );
    assert_eq!(
        rig.running.sampling_errors(),
        0,
        "广播失败不是采样失败：两者必须分开计数"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 维护态：整拍跳过（P6 Task 2a）
// ─────────────────────────────────────────────────────────────────────────────

/// **维护态期间采样整拍跳过**：零写入、零广播，而 `ticks` 照涨、`sampling_errors` 不涨。
///
/// 夹具是 armed 的：有活动会话，且在维护窗口里推进 30 秒——没有维护态时这一拍会写一个
/// 带进度的 `interval_checkpoint` 并广播 `timer.tick`（同文件的两条既有用例钉住了那条行为）。
/// 用例末尾的**正控**（维护结束之后同一个夹具立刻写出东西）证明"零写入"不是因为心跳没到期。
///
/// 维护态**不调** `Scheduler::stop()`：线程与 `ticks` 都照旧（`stop()` 不可逆，调了就再也
/// 回不来，而维护只是"这几拍不采样"）。
#[test]
fn a_maintenance_window_writes_nothing_broadcasts_nothing_and_keeps_ticking() {
    let (rig, sink) = rig_recording(&["t1"]);
    rig.start("t1").unwrap();

    // 进入维护态（进入时刻由调用方从时钟接缝取）。
    let entered_at = rig.clock.lock().unwrap().wall_ms();
    lock_app(rig.running.app())
        .begin_maintenance(MaintenancePhase::Restore, entered_at)
        .expect("进入维护态");

    // 基线全部在**置位之后**取：期间任何涨落都只能来自采样拍本身。
    let ticks_before = rig.running.sampling_ticks();
    let errors_before = rig.running.sampling_errors();
    let events_before = sink.events().len();
    let changes_before = rig.app_total_changes();
    let rows_before = table_rows(&rig);
    let revision_before = rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1");
    let checkpoints_before =
        rig.scalar("SELECT COALESCE(MAX(elapsed_ms), 0) FROM interval_checkpoint");

    // 推进 30 秒：心跳到期。维护态下这一拍必须整个消失。
    rig.clock.lock().unwrap().advance_both(30_000);
    rig.wait_for_ticks(ticks_before + 5);

    assert!(
        rig.running.sampling_ticks() > ticks_before,
        "维护态不停止调度器：ticks 照涨（{} -> {}）",
        ticks_before,
        rig.running.sampling_ticks()
    );
    assert_eq!(
        rig.scalar("SELECT COALESCE(MAX(elapsed_ms), 0) FROM interval_checkpoint"),
        checkpoints_before,
        "维护态不写检查点：整整 30 秒的进度都不许落库"
    );
    assert_eq!(table_rows(&rig), rows_before, "任何表的行数都不该变");
    assert_eq!(
        rig.app_total_changes(),
        changes_before,
        "App 连接上的 total_changes 也必须一动不动（UPDATE 也算）"
    );
    assert_eq!(
        rig.app_scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        revision_before,
        "维护态不得制造 revision"
    );
    assert_eq!(
        sink.events().len(),
        events_before,
        "维护态不广播：停止受理写入的同时广播一份旧世界的 tick 自相矛盾"
    );
    assert_eq!(
        rig.running.sampling_errors(),
        errors_before,
        "维护态不是错误：sampling_errors 不涨"
    );

    // 正控：维护结束之后，同一个夹具立刻恢复写入（否则上面那些"零"没有判别力）。
    lock_app(rig.running.app())
        .end_maintenance()
        .expect("退出维护态");
    let resumed = rig.wait_until(|| rig.app_total_changes() > changes_before);
    assert!(
        resumed,
        "维护结束之后采样必须恢复写入：到期的心跳（以及跨窗口长间隔的 P2 处置）都该落库"
    );
}

/// 维护态的拍是**整拍跳过（含读）**，不是"读了不写"。
///
/// 判据是行为性的：让时钟永久失败 ⇒ 只要那一拍真的去读样本，就必然拿到 `Err` 并涨
/// `sampling_errors`。维护态期间它**不涨**，而 `ticks` 照涨；维护一结束，同一个状态
/// 立刻把它涨上去（正控：夹具本来就是必然失败的）。
#[test]
fn a_maintenance_tick_is_skipped_before_any_read() {
    let (rig, _sink) = rig_recording(&["t1"]);
    rig.start("t1").unwrap();
    // 有活动会话 ⇒ 每一拍都要读时钟与库（`heartbeat` → `sample_and_detect`）。
    rig.clock.lock().unwrap().fail_forever();

    let entered_at = WALL;
    lock_app(rig.running.app())
        .begin_maintenance(MaintenancePhase::Restore, entered_at)
        .expect("进入维护态");
    let ticks_before = rig.running.sampling_ticks();
    let errors_before = rig.running.sampling_errors();

    rig.wait_for_ticks(ticks_before + 5);
    assert!(
        rig.running.sampling_ticks() > ticks_before,
        "维护态期间采样线程仍在被驱动"
    );
    assert_eq!(
        rig.running.sampling_errors(),
        errors_before,
        "整拍跳过：连读都没发生，所以这一刻的时钟故障一次都不该被记成采样失败"
    );

    lock_app(rig.running.app())
        .end_maintenance()
        .expect("退出维护态");
    let raised = rig.wait_until(|| rig.running.sampling_errors() > errors_before);
    assert!(
        raised,
        "维护结束之后的同一拍必须把时钟故障记进 sampling_errors（正控：夹具本来就是必然失败的）"
    );
}

/// 所有表的行数快照：空闲采样必须让它一动不动。
///
/// 行数抓 INSERT/DELETE；**UPDATE 要另外靠 `app_total_changes()`**（见上面那条用例）。
fn table_rows(rig: &Rig) -> Vec<(String, i64)> {
    let db = rig.open_db();
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

/// 这条通知是不是「有活动会话的 tick」。
fn is_active_tick(envelope: &EventEnvelope) -> bool {
    envelope.event == "timer.tick" && envelope.payload["session_id"].is_string()
}
