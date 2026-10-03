//! P2 Task 4 的心跳与检查点测试。
//!
//! 计划要求：正常心跳约每 30 秒写检查点且**不加 revision**；失败不推进持久化标记、
//! 后续可重试；**不可信样本不得写可信检查点**；内存里的可信点不能当恢复事实。

use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::timer::coordinator::{Coordinator, StartRequest};
use worktrace_lib::storage::checkpoint_repo;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;

struct H {
    _dir: tempfile::TempDir,
    db: Db,
    clock: Arc<Mutex<FakeClock>>,
    coord: Coordinator,
    epoch: String,
}

fn setup() -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    tx.execute(
        "INSERT INTO application_run(id, started_at) VALUES('run-1', 0)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Ready',0,0,0)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();

    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let coord = Coordinator::new(Box::new(Arc::clone(&clock)), "run-1");
    H {
        _dir: dir,
        db,
        clock,
        coord,
        epoch: meta.data_epoch,
    }
}

impl H {
    fn start(&mut self) {
        let req = StartRequest {
            expected_data_epoch: self.epoch.clone(),
            task_id: "t1".into(),
            task_expected_version: 0,
            mode: SessionMode::Foreground,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            expected_interval_ms: 30_000,
        };
        self.coord.start(&mut self.db, req).unwrap();
    }

    fn advance(&self, ms: i64) {
        self.clock.lock().unwrap().advance_both(ms);
    }

    fn advance_wall_only(&self, ms: i64) {
        self.clock.lock().unwrap().advance_wall(ms);
    }

    fn advance_mono_only(&self, ms: i64) {
        self.clock.lock().unwrap().advance_monotonic(ms);
    }

    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }

    fn checkpoints(&self, interval_id: &str) -> Option<checkpoint_repo::Checkpoint> {
        checkpoint_repo::latest(self.db.connection(), interval_id).unwrap()
    }
}

/// 心跳按 30 秒节奏写检查点，**不加 revision**。
#[test]
fn heartbeats_write_checkpoints_without_bumping_revision() {
    let mut h = setup();
    h.start();
    let interval_id = h
        .coord
        .live()
        .unwrap()
        .open_interval
        .as_ref()
        .unwrap()
        .0
        .clone();
    let rev_after_start = h.revision();

    // 首次心跳（还没有持久化标记）会写
    assert!(h.coord.heartbeat(&mut h.db).unwrap(), "首次心跳应当写");
    let cp = h.checkpoints(&interval_id).unwrap();
    assert_eq!(cp.elapsed_ms, 0);
    assert_eq!(h.revision(), rev_after_start, "心跳不得加 revision");

    // 不到 30 秒不再写
    h.advance(5_000);
    assert!(!h.coord.heartbeat(&mut h.db).unwrap(), "5 秒前不该再写");
    assert_eq!(
        h.checkpoints(&interval_id).unwrap().elapsed_ms,
        0,
        "检查点未变"
    );

    // 过了 30 秒再写，且 elapsed 与归属一致
    h.advance(30_000);
    assert!(h.coord.heartbeat(&mut h.db).unwrap(), "过了 30 秒应当写");
    let cp = h.checkpoints(&interval_id).unwrap();
    assert_eq!(cp.elapsed_ms, 35_000, "累计 35 秒");
    assert_eq!(
        cp.attribution_at,
        1_700_000_000_000 + 35_000,
        "attribution = 区间起点 + elapsed（checkpoint_repo 强制的那条）"
    );
    assert_eq!(h.revision(), rev_after_start, "心跳始终不加 revision");
}

/// **不可信样本不得写可信检查点**——否则会把可疑样本固化成恢复事实。
#[test]
fn an_untrusted_sample_never_becomes_a_trusted_checkpoint() {
    let mut h = setup();
    h.start();
    h.advance(30_000);
    assert!(h.coord.heartbeat(&mut h.db).unwrap());
    let before = h.coord.last_checkpoint().unwrap().clone();

    // 让下一次采样越界，然后查询（查询会把异常落库、会话转 recovering）
    h.advance_mono_only(1_000);
    h.advance_wall_only(31_000);
    let _ = h.coord.snapshot(&mut h.db);

    // 再心跳：状态已不是 running，且判定不可信 → 不写
    assert!(
        !h.coord.heartbeat(&mut h.db).unwrap(),
        "不可信时不得写检查点"
    );
    let interval_id = before.interval_id.clone();
    let after = h.checkpoints(&interval_id).unwrap();
    assert_eq!(
        after.elapsed_ms, before.elapsed_ms,
        "检查点不得被可疑样本推进"
    );
}

/// 心跳失败**不推进持久化标记**，所以下一次同一时刻还会重试。
#[test]
fn a_failed_heartbeat_does_not_advance_the_persisted_marker() {
    let mut h = setup();
    h.start();
    let interval_id = h
        .coord
        .live()
        .unwrap()
        .open_interval
        .as_ref()
        .unwrap()
        .0
        .clone();
    // start 本身会写一个 elapsed=0 的初始检查点；要验的是它**没被推进**
    let initial = h.checkpoints(&interval_id).unwrap();
    assert_eq!(initial.elapsed_ms, 0);

    // 让采样失败
    h.clock.lock().unwrap().fail_forever();
    h.advance(31_000);
    assert_eq!(
        h.coord.heartbeat(&mut h.db).unwrap_err().code(),
        "RECOVERY_REQUIRED"
    );
    assert!(
        h.coord.last_checkpoint().is_none(),
        "失败后不得记成已持久化"
    );
    assert_eq!(
        h.checkpoints(&interval_id).unwrap().elapsed_ms,
        0,
        "失败的心跳不得推进库里那条检查点"
    );

    // 恢复后立刻就能重试成功
    h.clock.lock().unwrap().recover();
    assert!(h.coord.heartbeat(&mut h.db).unwrap(), "恢复后应当重试成功");
    assert!(h.checkpoints(&interval_id).is_some());
}

/// 没有活动会话时心跳是空操作。
#[test]
fn a_heartbeat_without_a_session_is_a_no_op() {
    let mut h = setup();
    let rev = h.revision();
    assert!(!h.coord.heartbeat(&mut h.db).unwrap());
    assert_eq!(h.revision(), rev);
}
