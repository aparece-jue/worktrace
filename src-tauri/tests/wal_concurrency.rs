//! 第二连接 WAL 并发快照证据（P6 终审点名的无主计划条目）。
//!
//! 计划「P3 前遗留的消费门槛」第 1 条要的是：**为 P4 catalog 的四个读信封、以及 P2
//! 提交后的单一读事务，补第二连接 WAL 并发快照证据**——"无并发的返回值相等断言不能
//! 替代同事务验证"。本文件就是那份证据：真文件库（WAL）+ **第二条 `Db` 连接在另一条
//! 线程上持续提交**，主连接在每个入口上断言"数据与版本出自同一个快照"。
//!
//! 三类判据（都能被"把某一次读挪到事务外"这种一行级改动打红）：
//!
//! 1. **四个读信封**（`list_projects` / `list_tags` / `tags_of_task` / `list_tasks_filtered`）：
//!    并发写者每个事务恰好"加一行 + `revision` +1"，所以任何**同一个读事务**里取到的
//!    `(items/total, revision)` 必须满足 `Δ计数 == Δrevision`；把元数据读挪到读事务之外
//!    （两次独立快照）会让两边来自不同的提交，等式立刻不成立。
//! 2. **两个连接同时提交**（catalog 的写入口）：两边交回的 `revision` 必须构成**无重复、
//!    无空洞**的连续序列——"提交后再补读一次版本"会让两方拿到同一个值或跳过一格。
//! 3. **P2 提交后的单一读事务**（`Coordinator::rebuild_from_committed`）：命令交回的快照里
//!    `revision` 与 `task_row_version` 必须落在**同一个快照**上。并发写者每次改任务归属恰好
//!    `task_row_version +1` 且 `revision +1`，P2 每次会话写恰好 `revision +1`、不动任务行
//!    ⇒ `revision - task_row_version` 必须随 P2 写次数**逐次 +1**；元数据读与任务行读分成
//!    两次快照时，这个差会随并发写者的提交随机漂移。
//!
//! 不引入第二把生产锁、不改生产代码：写者用第二条连接（`tests/db_execution_boundary.rs`
//! 的既有模式），库由 `Db::open` 打开（WAL 是打开时的强制校验）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::catalog::{self, TaskQuery};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::timer::coordinator::{ResumeRequest, SessionRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::task_repo::{Page, ProjectFilter, TaskFilter};
use worktrace_lib::storage::WriteOutcome;

/// 并发窗口的下限：**两边都要真的动过**才算"并发"。
///
/// 不写死次数上限、不用固定 sleep：读循环一直跑到"读够多 + 写够多"或者硬超时。
/// 固定次数会让"读循环早就跑完了、写者才开始"这种假并发混进来。
const MIN_READS: u64 = 1_000;
const MIN_WRITES: usize = 40;
/// P2 那边的每次迭代更贵（取锁 + 采样 + 事务 + 提交后读事务），门槛相应低一些。
const MIN_P2_WRITES: usize = 20;
/// 硬超时：真跑不动就红，不挂死（正常机器上这一段是几十毫秒）。
const DEADLINE: Duration = Duration::from_secs(20);
/// 写者自己的步数上限（读循环比写循环快得多，正常远远到不了这个数）。
const MAX_WRITES: usize = 300;

// ─────────────────────────────────────────────────────────────────────────────
// 装置
// ─────────────────────────────────────────────────────────────────────────────

/// 真文件库 + `t1`（`Ready`）；`seed` 在并发开始**之前**跑完（所以它算基线的一部分）。
struct Rig {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    epoch: String,
}

fn rig<T>(seed: impl FnOnce(&mut Db, &str) -> T) -> (Rig, T) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let mut db = Db::open(&db_path).unwrap();
    migrate(db.connection()).unwrap();
    {
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
    let epoch = meta::read_meta(db.connection())
        .unwrap()
        .expect("app_meta 已初始化")
        .data_epoch;
    let seeded = seed(&mut db, &epoch);
    (
        Rig {
            _dir: dir,
            db_path,
            epoch,
        },
        seeded,
    )
}

/// 第二条连接上的并发写者：反复跑 `step`，把每次写交回的 `revision` 收起来。
struct Hammer {
    revisions: Arc<Mutex<Vec<i64>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Hammer {
    /// `max_steps` = 写者自己的步数上限：读循环跑得慢时它不会无限写下去
    /// （`tags_of_task` 那条场景的标签语料是有限的）。
    ///
    /// `gap` = 两次提交之间的间隔。**只跟主连接的写者竞争时要给一个非零值**：
    /// 应用的写路径是「先读一大段、再升级写锁」（`guard_epoch` 在写之前），
    /// 而 SQLite 在这条路径上**不等待** `busy_timeout`——写者若以 µs 级周期连续提交，
    /// 主连接那一次升级几乎每次都撞上 ⇒ 活活饿死。读信封那些场景里主连接只读，
    /// 所以 `gap` 取 0 也无妨。
    fn spawn(
        mut db: Db,
        max_steps: usize,
        gap: Duration,
        mut step: impl FnMut(&mut Db, usize) -> i64 + Send + 'static,
    ) -> Self {
        let revisions = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let revisions = Arc::clone(&revisions);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                // **第二条连接**：由调用方在**读循环开始之前**打开并移进来
                // ——读者已经持有快照之后才 `Db::open` 会把"并发"变成"读完了再写"
                // （实测：写者的提交会整段挤到两次读之间）。
                let mut index = 0usize;
                while !stop.load(Ordering::SeqCst) && index < max_steps {
                    revisions.lock().unwrap().push(step(&mut db, index));
                    index += 1;
                    if !gap.is_zero() {
                        thread::sleep(gap);
                    }
                }
            })
        };
        Self {
            revisions,
            stop,
            handle: Some(handle),
        }
    }

    /// 已经提交了几次（读循环用它决定"并发窗口开够了没有"）。
    fn writes(&self) -> usize {
        self.revisions.lock().unwrap().len()
    }

    fn stop_and_join(&mut self) -> Vec<i64> {
        self.stop.store(true, Ordering::SeqCst);
        let handle = self.handle.take().expect("只收一次");
        handle.join().expect("写者线程");
        self.revisions.lock().unwrap().clone()
    }
}

/// 写者的一步：只有 `Changed` 才算数（每个场景的每一步都必然变化；`Unchanged` 会让
/// "每个写事务恰好 +1 revision"这条前提失效，所以直接红）。
fn changed<T>(outcome: WriteOutcome<T>, revision_of: impl Fn(&T) -> i64) -> i64 {
    match outcome {
        WriteOutcome::Changed(value) => revision_of(&value),
        WriteOutcome::Unchanged(_) => panic!("并发写者的每一步都必须真的写入"),
    }
}

/// 一次读信封的可断言部分：`(与写者同步前进的计数, revision, data_epoch)`。
type Envelope = (i64, i64, String);

/// 并发快照场景的主体：开第二条连接 → 并发写 → 反复读并断言 `Δ计数 == Δrevision`。
fn hold_one_snapshot(
    rig: &Rig,
    base: (i64, i64),
    min_reads: u64,
    step: impl FnMut(&mut Db, usize) -> i64 + Send + 'static,
    read: impl Fn(&Db) -> Result<Envelope, AppError>,
) -> (u64, usize) {
    let reader = Db::open(&rig.db_path).expect("读连接");
    let writer = Db::open(&rig.db_path).expect("第二条连接");
    let mut hammer = Hammer::spawn(writer, MAX_WRITES, Duration::ZERO, step);
    let deadline = Instant::now() + DEADLINE;
    let mut reads = 0u64;
    while (reads < min_reads || hammer.writes() < MIN_WRITES) && Instant::now() < deadline {
        let (count, revision, epoch) = read(&reader).expect("读信封应当成功");
        assert_eq!(epoch, rig.epoch, "并发写不改库身份");
        assert_eq!(
            count - base.0,
            revision - base.1,
            "读信封的数据与版本必须出自**同一个**读快照：count={count}（基线 {}）、\
             revision={revision}（基线 {}）",
            base.0,
            base.1
        );
        reads += 1;
    }
    let writes = hammer.stop_and_join();
    // 收尾：写者停了之后，终态也要自洽，而且写者确实提交了 `writes.len()` 次
    // （每一步只有一次业务写 ⇒ 恰好 +1 revision）。
    let (count, revision, _) = read(&reader).expect("收尾读");
    assert_eq!(count - base.0, revision - base.1, "终态仍然要自洽");
    assert_eq!(
        revision - base.1,
        writes.len() as i64,
        "每个写事务恰好加一次 revision（共 {} 次写）",
        writes.len()
    );
    drop(reader);
    assert!(
        reads >= min_reads && writes.len() >= MIN_WRITES,
        "并发窗口没能真的形成：reads={reads} writes={}",
        writes.len()
    );
    (reads, writes.len())
}

// ─────────────────────────────────────────────────────────────────────────────
// 判据 1：P4 catalog 的四个读信封
// ─────────────────────────────────────────────────────────────────────────────

/// 读信封 ①：`list_projects`（COMP-01 项目列表：`items` 与 `revision` 同一读事务）。
#[test]
fn the_project_list_envelope_holds_one_snapshot_while_a_second_connection_writes() {
    // **把读的窗口撑开**：这个入口一次返回**全量**项目行，所以先在并发开始之前放 2 万行
    // ——单次读要几毫秒。这不是为了"压力测试"，而是让"数据与版本来自两次独立快照"这种
    // 破坏**必然**能在并发窗口里被抓到（两张表都只有几十行时窗口只有几微秒，
    // 断言再对也只是碰运气）。
    let (rig, ()) = rig(|db, _| {
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        for i in 0..20_000 {
            tx.execute(
                "INSERT INTO project(id,name,row_version,status,created_at,updated_at)
                 VALUES(?1,?2,0,'active',1000,1000)",
                rusqlite::params![format!("既有项目 {i}"), format!("既有项目 {i}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    });
    let base = {
        let reader = Db::open(&rig.db_path).unwrap();
        let list = catalog::list_projects(&reader, &rig.epoch, None).unwrap();
        (list.items.len() as i64, list.revision)
    };
    assert_eq!(base.0, 20_000, "种子就是那两万行");

    let epoch = rig.epoch.clone();
    let (reads, writes) = hold_one_snapshot(
        &rig,
        base,
        20,
        {
            let epoch = epoch.clone();
            move |db, i| {
                changed(
                    catalog::create_project(
                        db,
                        WriteEnvelope::for_create(epoch.clone()),
                        &format!("并发项目 {i}"),
                        2_000 + i as i64,
                    )
                    .expect("并发建项目"),
                    |change| change.revision,
                )
            }
        },
        {
            let epoch = epoch.clone();
            move |db| {
                let list = catalog::list_projects(db, &epoch, None)?;
                Ok((list.items.len() as i64, list.revision, list.data_epoch))
            }
        },
    );
    println!("[wal] list_projects reads={reads} writes={writes}");
}

/// 读信封 ②：`list_tags`（COMP-01 标签列表）。
#[test]
fn the_tag_list_envelope_holds_one_snapshot_while_a_second_connection_writes() {
    let (rig, ()) = rig(|_, _| ());
    let base = {
        let reader = Db::open(&rig.db_path).unwrap();
        let list = catalog::list_tags(&reader, &rig.epoch, None).unwrap();
        (list.items.len() as i64, list.revision)
    };

    let epoch = rig.epoch.clone();
    let (reads, writes) = hold_one_snapshot(
        &rig,
        base,
        MIN_READS,
        {
            let epoch = epoch.clone();
            move |db, i| {
                changed(
                    catalog::create_tag(
                        db,
                        WriteEnvelope::for_create(epoch.clone()),
                        "Context",
                        &format!("并发标签 {i}"),
                        None,
                        2_000 + i as i64,
                    )
                    .expect("并发建标签"),
                    |change| change.revision,
                )
            }
        },
        {
            let epoch = epoch.clone();
            move |db| {
                let list = catalog::list_tags(db, &epoch, None)?;
                Ok((list.items.len() as i64, list.revision, list.data_epoch))
            }
        },
    );
    println!("[wal] list_tags reads={reads} writes={writes}");
}

/// 读信封 ③：`tags_of_task`（某个任务身上的标签）。
///
/// 标签本身在**并发开始之前**建好（算基线的一部分），并发写者只做 `tag_task`
/// ——于是"标签条数"与 `revision` 仍然一一对应。
#[test]
fn the_tags_of_task_envelope_holds_one_snapshot_while_a_second_connection_writes() {
    let mut tag_ids: Vec<String> = Vec::new();
    let (rig, ()) = rig(|db, epoch| {
        for i in 0..MAX_WRITES {
            let outcome = catalog::create_tag(
                db,
                WriteEnvelope::for_create(epoch),
                "Activity",
                &format!("受理标签 {i}"),
                None,
                1_000 + i as i64,
            )
            .expect("建标签");
            match outcome {
                WriteOutcome::Changed(change) => tag_ids.push(change.tag.id),
                WriteOutcome::Unchanged(_) => panic!("新标签必然是新写入"),
            }
        }
    });
    let base = {
        let reader = Db::open(&rig.db_path).unwrap();
        let list = catalog::tags_of_task(&reader, &rig.epoch, "t1").unwrap();
        (list.items.len() as i64, list.revision)
    };

    let epoch = rig.epoch.clone();
    let (reads, writes) = hold_one_snapshot(
        &rig,
        base,
        MIN_READS,
        {
            let epoch = epoch.clone();
            let tag_ids = tag_ids.clone();
            move |db, i| {
                let tag_id = &tag_ids[i];
                changed(
                    catalog::tag_task(
                        db,
                        WriteEnvelope::for_create(epoch.clone()),
                        "t1",
                        tag_id,
                        3_000 + i as i64,
                    )
                    .expect("并发打标"),
                    |change| change.revision,
                )
            }
        },
        {
            let epoch = epoch.clone();
            move |db| {
                let list = catalog::tags_of_task(db, &epoch, "t1")?;
                Ok((list.items.len() as i64, list.revision, list.data_epoch))
            }
        },
    );
    println!("[wal] tags_of_task reads={reads} writes={writes}");
}

/// 读信封 ④：`list_tasks_filtered`（筛选查询：`total` 与 `revision` 同一读事务）。
#[test]
fn the_task_query_envelope_holds_one_snapshot_while_a_second_connection_writes() {
    let (rig, ()) = rig(|_, _| ());
    let query = |epoch: &str| TaskQuery {
        filter: TaskFilter {
            statuses: Vec::new(),
            project: ProjectFilter::Any,
            context_tag_id: None,
        },
        page: Page {
            limit: 100,
            offset: 0,
        },
        expected_data_epoch: epoch.to_string(),
    };
    let base = {
        let reader = Db::open(&rig.db_path).unwrap();
        let result = catalog::list_tasks_filtered(&reader, query(&rig.epoch)).unwrap();
        (result.total, result.revision)
    };

    let epoch = rig.epoch.clone();
    let (reads, writes) = hold_one_snapshot(
        &rig,
        base,
        MIN_READS,
        {
            let epoch = epoch.clone();
            move |db, i| {
                changed(
                    catalog::create_task(
                        db,
                        WriteEnvelope::for_create(epoch.clone()),
                        &format!("并发任务 {i}"),
                        None,
                        4_000 + i as i64,
                    )
                    .expect("并发建任务"),
                    |change| change.revision,
                )
            }
        },
        {
            let epoch = epoch.clone();
            move |db| {
                let result = catalog::list_tasks_filtered(db, query(&epoch))?;
                // 并发写者可能把总数顶到 `limit` 之上：这一页最多 `limit` 条，
                // 但 `total` 与 `revision` 必须仍然同源。
                assert_eq!(
                    result.tasks.len() as i64,
                    result.total.min(100),
                    "这一页与 total 同源"
                );
                Ok((result.total, result.revision, result.data_epoch))
            }
        },
    );
    println!("[wal] list_tasks_filtered reads={reads} writes={writes}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 判据 2：两个连接同时提交 ⇒ 交回的 revision 无重复、无空洞
// ─────────────────────────────────────────────────────────────────────────────

/// `DEFERRED` 事务在**读过之后**再升级写锁时，SQLite 可能**立刻**返回 `SQLITE_BUSY`
/// （`busy_timeout` 在这条路径上不等待）——这是 SQLite 的性质，不是业务行为。
///
/// 本文件的写者把它当成"这次没写"：被拒的写不加 revision，重试即可。
/// 生产只有**一条**写连接（单实例 + 单连接），这条路径今天不可达；这里如实登记，
/// 不改生产代码（P6 终审修复波的边界）。
fn is_locked(error: &AppError) -> bool {
    error.code() == "STORAGE_ERROR"
        && error
            .detail()
            .is_some_and(|detail| detail.contains("database is locked"))
}

/// 重试到成功（或者因为别的原因失败 ⇒ 直接红）。
///
/// **必须退避**：两个 `DEFERRED` 事务都在"读过之后再升级写锁"，谁先提交就会让对方
/// 那一次升级**立刻**失败；不退避地原地重试会活锁。退避量按写序号错开相位
/// （`seed`），免得两个写者一直同步撞车。
fn retry_write<T>(seed: u32, mut body: impl FnMut() -> Result<T, AppError>) -> T {
    for attempt in 0..400u32 {
        match body() {
            Ok(value) => return value,
            Err(error) if is_locked(&error) && attempt < 399 => {
                let backoff_us = 50 * u64::from((attempt + seed) % 20 + 1);
                thread::sleep(Duration::from_micros(backoff_us));
                continue;
            }
            Err(error) => panic!("并发写失败：{error:?}"),
        }
    }
    unreachable!("重试次数用尽前必然返回")
}

/// 两个连接**同时**建项目：两边交回的 `revision` 合起来必须恰好是
/// `{基线+1, …, 基线+成功写次数}`。提交后**再补读**一次版本（而不是在写事务里读回）
/// 会让两方拿到同一个值、或让某个值没人交回——两种都会打红这条断言。
#[test]
fn two_connections_committing_at_once_return_unique_consecutive_revisions() {
    let (rig, ()) = rig(|_, _| ());
    let base_revision = meta::read_meta(Db::open(&rig.db_path).unwrap().connection())
        .unwrap()
        .expect("app_meta")
        .revision;

    let epoch = rig.epoch.clone();
    // 两边都留 200µs 的节拍：两个 `DEFERRED` 写者连续互撞会互相饿死
    // （见 `Hammer::spawn` 与 `retry_write` 的说明），而"真并发"不需要 µs 级对撞。
    let mut hammer = Hammer::spawn(
        Db::open(&rig.db_path).expect("第二条连接"),
        MAX_WRITES,
        Duration::from_micros(200),
        {
            let epoch = epoch.clone();
            move |db, i| {
                retry_write(i as u32, || {
                    catalog::create_project(
                        db,
                        WriteEnvelope::for_create(epoch.clone()),
                        &format!("写者 B {i}"),
                        5_000 + i as i64,
                    )
                    .map(|outcome| changed(outcome, |change| change.revision))
                })
            }
        },
    );

    // 主连接（写者 A）与写者 B 抢同一条 WAL 写锁。
    let mut main_db = Db::open(&rig.db_path).unwrap();
    let mut revisions = Vec::new();
    for i in 0..MIN_WRITES {
        revisions.push(retry_write(i as u32 + 7, || {
            catalog::create_project(
                &mut main_db,
                WriteEnvelope::for_create(epoch.clone()),
                &format!("写者 A {i}"),
                6_000 + i as i64,
            )
            .map(|outcome| changed(outcome, |change| change.revision))
        }));
        thread::sleep(Duration::from_micros(200));
    }
    revisions.extend(hammer.stop_and_join());
    drop(main_db);

    revisions.sort_unstable();
    let expected: Vec<i64> = (base_revision + 1..=base_revision + revisions.len() as i64).collect();
    assert_eq!(
        revisions, expected,
        "两个并发写者交回的 revision 必须无重复、无空洞（提交后补读会出现重复或空洞）"
    );
    println!(
        "[wal] two writers: {} commits, revisions {}..={}",
        revisions.len(),
        base_revision + 1,
        base_revision + revisions.len() as i64
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 判据 3：P2 提交后的快照（暂停 / 继续）
// ─────────────────────────────────────────────────────────────────────────────

/// **P2 提交后的单一读事务**（`Coordinator::rebuild_from_committed`）：暂停/继续命令交回的
/// 快照必须反映**这次提交**的会话事实，而且与同一次读事务取到的版本位自洽。
///
/// 并发侧是第二条连接上持续提交的业务写（建任务：每个事务恰好加一行 + `revision` +1）。
/// 断言四件事：
///
/// 1. 第 k 次会话写之后，快照里的 `session_version` 恰好是 `基线 + k`——**每个成功写只让
///    会话行前进一格**，快照读到的必须是这次提交之后的那一行（读到提交前那一行就红）；
/// 2. `state` 与这次写请求的意图一致（`pause` ⇒ `Paused`，`resume` ⇒ `Running`）；
/// 3. `revision` 逐次严格增长、`data_epoch` 不动；
/// 4. 终态账：`revision` 的总增量 == 并发写次数 + 会话写次数（**并发下每个写事务仍然恰好
///    加一次 revision**）。
///
/// ⚠️ 这里**没有**断言"`revision` 与 `task_row_version` 出自同一个读快照"那一版更锋利的
/// 等式：它的判别力要求第二条连接能改**正在计时的会话所属任务行**，而仓储明令禁止
/// （`task_repo::set_task_project` 的守卫：任务不在 `Inbox/Clarifying/Ready` 或已有运行会话
/// 时一律拒绝，V0.1 也没有改标题的入口）⇒ 那一版等式在任何实现下都恒真，写进来只是
/// 证据表演。四个读信封那一组（判据 1）承担"同一读快照"的锋利证据：并发写者改的正是
/// 被读的那些行。
#[test]
fn a_p2_commit_returns_a_snapshot_of_that_very_commit_under_a_second_connection() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");
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

    // 真启动：采样节拍调到一小时，且假钟不前进 ⇒ 采样线程全程只读。
    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let config =
        StartupConfig::new(&db_path, &lock_path).with_backup_dir(dir.path().join("backups"));
    let sink = Arc::new(NoSink);
    let running: Box<RunningApp> = match startup(
        config,
        Box::new(Arc::clone(&clock)),
        Arc::clone(&sink) as Arc<dyn EventSink>,
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

    // 起一次计时（任务 Ready → Doing 也在这一步完成），随后取基线。
    let (session_id, mut session_version, task_version) = {
        let mut state = lock_app(&app);
        let outcome = state
            .start(StartRequest {
                expected_data_epoch: epoch.clone(),
                task_id: "t1".to_string(),
                task_expected_version: 0,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            })
            .expect("开始计时");
        (
            outcome.snapshot.session_id.clone().expect("有会话"),
            outcome.snapshot.session_version.expect("有会话版本"),
            outcome.snapshot.task_row_version.expect("有任务版本"),
        )
    };
    let s0 = session_version;
    let r0 = meta::read_meta(Db::open(&db_path).unwrap().connection())
        .unwrap()
        .expect("app_meta")
        .revision;

    // 并发写者：第二条连接上持续建任务（合法业务写，每个事务恰好 +1 revision）。
    let epoch_for_writer = epoch.clone();
    // 3ms 的节拍：与主连接的写竞争时既保持"真并发"，又不把主连接饿死（见 `Hammer::spawn`）。
    let mut hammer = Hammer::spawn(
        Db::open(&db_path).expect("第二条连接"),
        MAX_WRITES,
        Duration::from_millis(3),
        move |db, i| {
            retry_write(i as u32, || {
                catalog::create_task(
                    db,
                    WriteEnvelope::for_create(epoch_for_writer.clone()),
                    &format!("并发任务 {i}"),
                    None,
                    8_000 + i as i64,
                )
                .map(|outcome| changed(outcome, |change| change.revision))
            })
        },
    );

    let mut writes = 0i64;
    let mut last_revision = 0i64;
    let mut paused = false;
    let deadline = Instant::now() + DEADLINE;
    while ((writes as usize) < MIN_P2_WRITES || hammer.writes() < MIN_WRITES)
        && Instant::now() < deadline
    {
        let attempt_index = writes;
        let mut attempts = 0u32;
        let outcome = loop {
            let mut state = lock_app(&app);
            let result = if paused {
                state.resume(ResumeRequest {
                    expected_data_epoch: epoch.clone(),
                    task_id: "t1".to_string(),
                    task_expected_version: task_version,
                    session_id: session_id.clone(),
                    session_expected_version: session_version,
                })
            } else {
                state.pause(SessionRequest {
                    expected_data_epoch: epoch.clone(),
                    session_id: session_id.clone(),
                    session_expected_version: session_version,
                })
            };
            match result {
                Ok(outcome) => break outcome,
                Err(error) if is_locked(&error) && attempts < 400 => {
                    // 与写者同样的退避理由（DEFERRED 升级冲突不等待 `busy_timeout`）。
                    attempts += 1;
                    drop(state);
                    thread::sleep(Duration::from_micros(
                        50 * u64::from((attempts + attempt_index as u32) % 20 + 1),
                    ));
                }
                Err(error) => panic!("第 {attempt_index} 次会话写失败：{error:?}"),
            }
        };
        paused = !paused;
        writes += 1;

        // ① 快照里的会话行必须是**这次提交之后**的那一行（每个成功写恰好 +1 行版本）。
        session_version = outcome.snapshot.session_version.expect("有会话版本");
        assert_eq!(
            session_version,
            s0 + writes,
            "第 {writes} 次写之后，快照的会话版本必须是「基线 + 成功写次数」"
        );
        assert_eq!(
            session_version,
            {
                // 会话版本的另一条读路径：库里的那一行（第二条连接）。
                Db::open(&db_path)
                    .unwrap()
                    .connection()
                    .query_row(
                        "SELECT row_version FROM work_session WHERE id = ?1",
                        [&session_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap()
            },
            "第 {writes} 次写之后，快照的会话版本必须等于库里那一行"
        );
        // ② 状态与这次请求的意图一致。
        assert_eq!(
            outcome.snapshot.state,
            Some(if paused {
                worktrace_lib::domain::session::SessionState::Paused
            } else {
                worktrace_lib::domain::session::SessionState::Running
            }),
            "第 {writes} 次写之后快照要反映这次提交的结果"
        );
        // ③ 版本位：每次成功写至少推进一格，库身份不动。
        assert!(
            outcome.snapshot.revision > last_revision,
            "第 {writes} 次写：快照 revision {} 没有前进（上一次 {last_revision}）",
            outcome.snapshot.revision
        );
        assert_eq!(outcome.snapshot.data_epoch, epoch, "并发写不改库身份");
        last_revision = outcome.snapshot.revision;
    }

    let hammer_writes = hammer.stop_and_join();
    assert!(
        writes as usize >= MIN_P2_WRITES && hammer_writes.len() >= MIN_WRITES,
        "并发窗口没能真的形成：p2_writes={writes} hammer_writes={}",
        hammer_writes.len()
    );

    // ④ 终态账：`revision` 的总增量 = 两边成功的写次数（每步恰好一次业务写）。
    let final_revision = meta::read_meta(Db::open(&db_path).unwrap().connection())
        .unwrap()
        .expect("app_meta")
        .revision;
    assert_eq!(
        final_revision - r0,
        hammer_writes.len() as i64 + writes,
        "并发下每个写事务仍然恰好加一次 revision"
    );
    println!(
        "[wal] p2 writes={writes} hammer writes={} revision {}..={final_revision}",
        hammer_writes.len(),
        r0
    );
}

/// 空的事件出口（本文件不关心广播）。
struct NoSink;

impl EventSink for NoSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        Ok(())
    }
}
