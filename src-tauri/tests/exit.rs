//! P7 Task 0：显式退出（02 §4）。
//!
//! 计划原文：「**一个事务**里结束 `running`/`paused` 会话、写 `clean_exit_at`、
//! 保存 revision、停定时器；**`recovering` 记录保留不清**」。
//! Task 4 的托盘「退出」与 P8 都复用这一条入口，所以这里的断言就是那条入口的契约。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{startup, NoProbe, RunningApp, Startup, StartupConfig};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

const WALL: i64 = 1_700_000_000_000;

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

struct Harness {
    running: Box<RunningApp>,
    db_path: PathBuf,
    _dir: tempfile::TempDir,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");

    {
        let mut db = Db::open(&db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        init_meta(&tx).unwrap();
        tx.commit().unwrap();
    }

    let mut config = StartupConfig::new(&db_path, &lock_path);
    config.sampling_interval_ms = 10;
    let running = match startup(
        config,
        Box::new(FakeClock::new(WALL, 0)),
        Arc::new(RecordingSink::default()) as Arc<dyn EventSink>,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };

    Harness {
        running,
        db_path,
        _dir: dir,
    }
}

impl Harness {
    fn db(&self) -> Db {
        Db::open(&self.db_path).unwrap()
    }

    fn scalar(&self, sql: &str) -> i64 {
        self.db()
            .connection()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }

    /// 可空整数列。
    fn int_opt(&self, sql: &str) -> Option<i64> {
        self.db()
            .connection()
            .query_row(sql, [], |r| r.get::<_, Option<i64>>(0))
            .unwrap()
    }

    /// 非空文本列。
    fn text(&self, sql: &str) -> String {
        self.db()
            .connection()
            .query_row(sql, [], |r| r.get::<_, String>(0))
            .unwrap()
    }
}

/// 预置：本次 run 的 running / paused / recovering 三个会话 + 上一代次的 paused 会话。
fn seed_sessions(h: &Harness) {
    let run_id = h.running.run_id().to_string();
    let db = h.db();
    let conn = db.connection();
    conn.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO application_run(id,started_at) VALUES('run-old',900)",
        [],
    )
    .unwrap();

    // running：有开放区间，退出时必须闭合。
    conn.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s-run','t1',?1,'FOREGROUND','running','stopwatch',1000,0)",
        [&run_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO work_interval(id,session_id,started_at) VALUES('iv-run','s-run',1000)",
        [],
    )
    .unwrap();

    // paused：没有开放区间，退出时直接置 finished。
    conn.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s-paused','t1',?1,'FOREGROUND','paused','stopwatch',1100,0)",
        [&run_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms)
         VALUES('iv-paused','s-paused',1100,1200,100)",
        [],
    )
    .unwrap();

    // recovering：待确认区间 — 退出**不得**动它。
    conn.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version,
                                  needs_review)
         VALUES('s-recovering','t1',?1,'FOREGROUND','recovering','stopwatch',1200,0,1)",
        [&run_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review)
         VALUES('iv-pending','s-recovering',1200,1500,NULL,1)",
        [],
    )
    .unwrap();

    // 上一代次留下的 paused 会话：恢复材料，退出不得碰（碰了就是「自动修复」）。
    conn.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s-old','t1','run-old','FOREGROUND','paused','stopwatch',1300,0)",
        [],
    )
    .unwrap();
}

#[test]
fn explicit_exit_ends_running_and_paused_and_writes_clean_exit_at() {
    let mut h = harness();
    seed_sessions(&h);
    let revision_before = h.scalar("SELECT revision FROM app_meta WHERE singleton = 1");
    let run_id = h.running.run_id().to_string();

    let report = h.running.shutdown().expect("显式退出应当成功");

    assert_eq!(report.run_id, run_id);
    assert_eq!(report.clean_exit_at, WALL, "退出时刻来自协调器的时钟采样");
    assert!(report.clean_exit_recorded);
    assert_eq!(
        report.sessions_ended,
        vec!["s-run".to_string(), "s-paused".to_string()],
        "running 与 paused 都在同一个事务里结束"
    );
    assert_eq!(report.recovering_kept, vec!["s-recovering".to_string()]);
    assert_eq!(
        report.revision,
        revision_before + 1,
        "两个会话在同一事务里结束 ⇒ revision 恰好 +1"
    );

    // clean_exit_at 落库（退出写的就是本次 run 那一行）。
    assert_eq!(
        h.int_opt(&format!(
            "SELECT clean_exit_at FROM application_run WHERE id = '{}'",
            h.running.run_id()
        )),
        Some(WALL)
    );

    // running → finished，开放区间被可信闭合（有 duration、无待确认）。
    assert_eq!(
        h.text("SELECT state FROM work_session WHERE id='s-run'"),
        "finished"
    );
    assert_eq!(
        h.scalar("SELECT ended_at FROM work_session WHERE id='s-run'"),
        WALL
    );
    assert_eq!(
        h.scalar("SELECT ended_at FROM work_interval WHERE id='iv-run'"),
        WALL
    );
    assert_eq!(
        h.scalar("SELECT duration_ms FROM work_interval WHERE id='iv-run'"),
        WALL - 1000
    );
    assert_eq!(
        h.scalar("SELECT needs_review FROM work_interval WHERE id='iv-run'"),
        0
    );

    // paused → finished（它本来就没有开放区间）。
    assert_eq!(
        h.text("SELECT state FROM work_session WHERE id='s-paused'"),
        "finished"
    );
    assert_eq!(
        h.scalar("SELECT ended_at FROM work_session WHERE id='s-paused'"),
        WALL
    );

    // recovering 记录**原样保留**（02 §4）。
    assert_eq!(
        h.text("SELECT state FROM work_session WHERE id='s-recovering'"),
        "recovering"
    );
    assert_eq!(
        h.int_opt("SELECT ended_at FROM work_session WHERE id='s-recovering'"),
        None
    );
    assert_eq!(
        h.scalar("SELECT needs_review FROM work_interval WHERE id='iv-pending'"),
        1
    );
    assert_eq!(
        h.scalar("SELECT ended_at FROM work_interval WHERE id='iv-pending'"),
        1500
    );
    assert_eq!(
        h.int_opt("SELECT duration_ms FROM work_interval WHERE id='iv-pending'"),
        None,
        "待确认区间仍然没有确定时长"
    );

    // 别的 run 的会话不受影响。
    assert_eq!(
        h.text("SELECT state FROM work_session WHERE id='s-old'"),
        "paused"
    );
    assert_eq!(
        h.int_opt("SELECT ended_at FROM work_session WHERE id='s-old'"),
        None
    );
}

/// 重复退出是幂等的：不重复结束、不重复加 revision、不覆盖第一次的时刻。
#[test]
fn explicit_exit_is_idempotent() {
    let mut h = harness();
    seed_sessions(&h);

    let first = h.running.shutdown().unwrap();
    let revision_after_first = h.scalar("SELECT revision FROM app_meta WHERE singleton = 1");

    let second = h.running.shutdown().unwrap();
    assert!(second.sessions_ended.is_empty());
    assert!(!second.revision_changed);
    assert!(
        !second.clean_exit_recorded,
        "clean_exit_at 已经写过，不再覆盖"
    );
    assert_eq!(second.revision, first.revision);
    assert_eq!(
        h.scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        revision_after_first
    );
    assert_eq!(second.recovering_kept, vec!["s-recovering".to_string()]);
    assert_eq!(
        h.scalar("SELECT ended_at FROM work_session WHERE id='s-run'"),
        WALL
    );
}

/// 一个事务：中途失败 ⇒ 前面已经结束的会话也要回滚，`clean_exit_at` 不写。
#[test]
fn a_failing_session_end_rolls_back_the_whole_exit() {
    let mut h = harness();
    seed_sessions(&h);

    // 故意造一条结束不了的会话：`needs_review=1` 的会话必须等恢复，
    // 结束原语会拒绝它。它排在 running 之后，所以前一步必须被回滚。
    let run_id = h.running.run_id().to_string();
    h.db()
        .connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                      row_version,needs_review)
             VALUES('s-stuck','t1',?1,'FOREGROUND','paused','stopwatch',2000,0,1)",
            [&run_id],
        )
        .unwrap();

    let revision_before = h.scalar("SELECT revision FROM app_meta WHERE singleton = 1");
    let err = h
        .running
        .shutdown()
        .expect_err("结束不了的会话必须让整个退出失败");
    assert_eq!(err.code(), "RECOVERY_REQUIRED");

    assert_eq!(
        h.text("SELECT state FROM work_session WHERE id='s-run'"),
        "running",
        "失败必须回滚已经结束的会话"
    );
    assert_eq!(
        h.int_opt("SELECT ended_at FROM work_session WHERE id='s-run'"),
        None
    );
    assert_eq!(
        h.int_opt("SELECT ended_at FROM work_interval WHERE id='iv-run'"),
        None
    );
    assert_eq!(
        h.int_opt(&format!(
            "SELECT clean_exit_at FROM application_run WHERE id = '{}'",
            h.running.run_id()
        )),
        None
    );
    assert_eq!(
        h.scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        revision_before,
        "回滚后 revision 不得前进"
    );
}

/// 退出先停定时器：之后不会再有一拍落进已经结束的事务里。
#[test]
fn shutdown_stops_the_sampler_first() {
    let mut h = harness();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while h.running.sampling_ticks() < 2 {
        assert!(std::time::Instant::now() < deadline, "采样驱动没有跑起来");
        std::thread::sleep(Duration::from_millis(5));
    }

    h.running.shutdown().unwrap();
    let after_shutdown = h.running.sampling_ticks();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        h.running.sampling_ticks(),
        after_shutdown,
        "退出之后不得再有采样触发"
    );
}
