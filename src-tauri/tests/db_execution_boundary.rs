//! 06 §4「DB 执行边界」实验（P1 Task 2 要求比较并记录选择）。
//!
//! 要回答的问题不是「哪个更快」——两种边界在**单连接**下必然串行，这是 SQLite
//! 写锁的固有性质。真正要定的是三件事：
//!
//! 1. 第二个写者该等还是该失败？（→ `busy_timeout` 的取值）
//! 2. 阻塞落在哪个线程上？（→ 决定能不能在 async 运行时/UI 回调里直接调）
//! 3. `Connection` 是 `!Sync`，这个约束该由谁吸收？
//!
//! 断言只写**确定性**性质（顺序、归属、错误码）；耗时只测量并打印，不做断言——
//! 时间断言在 CI 上必然 flaky，而它也不决定选择。

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::migrations::migrate;

/// 一个足够慢的语句：用递归 CTE 烧 CPU，不依赖 sleep，行为稳定。
const SLOW_SQL: &str =
    "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 300000)
                       SELECT count(*) FROM c";

fn fresh_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    (dir, db)
}

// ─────────────────────────────────────────────────────────────────────────────
// 问题 1：第二个写者该等还是该失败
// ─────────────────────────────────────────────────────────────────────────────

/// 本层配置了 `busy_timeout=5s`，所以并发的第二个写者**等待**而不是报错。
#[test]
fn a_second_writer_waits_instead_of_failing() {
    let (dir, db) = fresh_db();
    drop(db);

    // 两个独立连接指向同一个文件——比一个连接更能暴露锁行为。
    let a = Db::open(dir.path().join("w.db")).unwrap();
    let b = Db::open(dir.path().join("w.db")).unwrap();
    a.connection().execute_batch("BEGIN IMMEDIATE").unwrap();
    a.connection()
        .execute(
            "INSERT INTO project(id,name,row_version,status,created_at,updated_at)\n             VALUES('p1','held',0,'active',1,1)",
            [],
        )
        .unwrap();

    // b 在 a 持有写锁时发起写入：应当等，而不是立刻 SQLITE_BUSY。
    let started = Instant::now();
    let handle = {
        let b_conn = b.into_connection();
        thread::spawn(move || {
            let r = b_conn.execute(
                "INSERT INTO project(id,name,row_version,status,created_at,updated_at)\n                 VALUES('p2','waited',0,'active',1,1)",
                [],
            );
            (r, b_conn)
        })
    };
    thread::sleep(Duration::from_millis(150));
    a.connection().execute_batch("COMMIT").unwrap();

    let (result, _b_conn) = handle.join().unwrap();
    let waited = started.elapsed();

    result.expect("设了 busy_timeout，第二个写者应当等到锁释放后成功");
    println!(
        "[experiment] second writer waited {:?} for the lock",
        waited
    );
}

/// 对照组：把 timeout 设成 0，同一场景立刻失败并给出 SQLITE_BUSY。
///
/// 这条证明 `busy_timeout` 不是装饰——不设它，并发写会以用户可见的错误收场。
#[test]
fn without_busy_timeout_the_second_writer_fails_with_busy() {
    let (dir, db) = fresh_db();
    drop(db);

    let a = Db::open(dir.path().join("w.db")).unwrap();
    let b = Db::open(dir.path().join("w.db")).unwrap();
    b.connection()
        .busy_timeout(Duration::from_millis(0))
        .unwrap();

    a.connection().execute_batch("BEGIN IMMEDIATE").unwrap();
    let err = b
        .connection()
        .execute(
            "INSERT INTO project(id,name,row_version,status,created_at,updated_at)\n             VALUES('p2','x',0,'active',1,1)",
            [],
        )
        .unwrap_err();

    let text = err.to_string().to_lowercase();
    assert!(
        text.contains("locked") || text.contains("busy"),
        "应为锁冲突，实际：{err}"
    );
    a.connection().execute_batch("ROLLBACK").unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// 问题 2：阻塞落在哪个线程上
// ─────────────────────────────────────────────────────────────────────────────

/// **受控阻塞边界**：一个 `Mutex<Connection>`，调用方线程阻塞在锁上。
///
/// 观察到的性质：读请求**在调用方自己的线程上**等待，且必须等持锁者让出。
/// 这就是为什么不能在 async 运行时的工作线程或 UI 回调里直接调——会把它们占住。
#[test]
fn mutex_boundary_blocks_the_calling_thread() {
    let (_dir, db) = fresh_db();
    let shared = Arc::new(Mutex::new(db.into_connection()));
    let (id_tx, id_rx) = mpsc::channel();

    // 持锁者：占住锁一段时间，并把自己的线程 id 报出来。
    let holder = {
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            let conn = shared.lock().unwrap();
            conn.execute_batch(SLOW_SQL).unwrap();
            id_tx.send(thread::current().id()).unwrap();
            thread::sleep(Duration::from_millis(250)); // 持锁不放
        })
    };
    let holder_id = id_rx.recv().unwrap();

    // 调用方：**必须在此之前就已经起来**，否则锁早释放了，测出来是 0。
    let caller = {
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            let t0 = Instant::now();
            let conn = shared.lock().unwrap(); // ← 阻塞点
            let blocked = t0.elapsed();
            let r: i64 = conn
                .query_row("SELECT count(*) FROM project", [], |r| r.get(0))
                .unwrap();
            (blocked, r, thread::current().id())
        })
    };

    let (blocked, rows, caller_id) = caller.join().unwrap();
    holder.join().unwrap();

    assert_ne!(caller_id, holder_id, "两个线程应不同");
    assert!(rows >= 0);
    assert!(
        blocked >= Duration::from_millis(100),
        "调用方应当真的被锁挡住（实测 {blocked:?}）—— 这正是不能在 async 工作线程里直接调 SQL 的原因"
    );
    println!("[experiment] mutex boundary: caller blocked {blocked:?} on the lock");
}

/// **串行工作线程**：连接被 move 进一个专属线程，调用方只发任务、等回执。
///
/// 观察到的性质：SQL 在**工作线程**上执行；调用方线程只阻塞在 channel 上。
/// 连接从不离开工作线程，`!Sync` 的约束被这一层吸收，调用点不必再关心它。
#[test]
fn worker_thread_runs_sql_on_its_own_thread() {
    let (_dir, db) = fresh_db();

    enum Job {
        Count(mpsc::Sender<(thread::ThreadId, i64)>),
        Stop,
    }

    let (tx, rx) = mpsc::channel::<Job>();
    let worker_id = {
        let conn = db.into_connection();
        let handle = thread::spawn(move || {
            let id = thread::current().id();
            while let Ok(job) = rx.recv() {
                match job {
                    Job::Count(reply) => {
                        let n: i64 = conn
                            .query_row("SELECT count(*) FROM project", [], |r| r.get(0))
                            .unwrap();
                        reply.send((id, n)).unwrap();
                    }
                    Job::Stop => break,
                }
            }
        });
        // 先问一次，拿到工作线程的 id
        let (rtx, rrx) = mpsc::channel();
        tx.send(Job::Count(rtx)).unwrap();
        let (id, _) = rrx.recv().unwrap();
        // 收尾
        tx.send(Job::Stop).unwrap();
        handle.join().unwrap();
        id
    };

    let caller_id = thread::current().id();
    assert_ne!(
        worker_id, caller_id,
        "SQL 必须在工作线程上执行，而不是调用方线程"
    );
    println!("[experiment] worker boundary: sql ran on worker thread {worker_id:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 问题 3：两种边界的吞吐对照（只测量，不断言）
// ─────────────────────────────────────────────────────────────────────────────

/// 20 个小读并发打进来，同时有一个慢写持锁。分别走两种边界，打印总耗时。
///
/// **不断言时间**：结论是「两者都串行、总耗时同量级」，这不需要阈值来证明；
/// 需要的是把实测值记进 `IMPLEMENTATION-NOTES.md`。
#[test]
fn both_boundaries_serialise_at_the_same_order_of_magnitude() {
    const READS: usize = 20;

    // A：Mutex 边界
    let (_dir_a, db_a) = fresh_db();
    let shared = Arc::new(Mutex::new(db_a.into_connection()));
    let t_a = Instant::now();
    {
        let s = Arc::clone(&shared);
        let writer = thread::spawn(move || {
            let conn = s.lock().unwrap();
            conn.execute_batch(SLOW_SQL).unwrap();
        });
        let mut readers = Vec::new();
        for _ in 0..READS {
            let s = Arc::clone(&shared);
            readers.push(thread::spawn(move || {
                let conn = s.lock().unwrap();
                let _: i64 = conn
                    .query_row("SELECT count(*) FROM project", [], |r| r.get(0))
                    .unwrap();
            }));
        }
        writer.join().unwrap();
        for r in readers {
            r.join().unwrap();
        }
    }
    let elapsed_a = t_a.elapsed();

    // B：工作线程边界。**必须先发同一个慢写**，否则两边做的不是同一件事，
    // 测出来的差值只是「有没有干那个慢活」，不是在比边界。
    let (_dir_b, db_b) = fresh_db();
    enum Job {
        Slow,
        Count(mpsc::Sender<()>),
    }
    let (tx, rx) = mpsc::channel::<Job>();
    let worker = {
        let conn = db_b.into_connection();
        thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                match job {
                    Job::Slow => {
                        conn.execute_batch(SLOW_SQL).unwrap();
                    }
                    Job::Count(reply) => {
                        let _: i64 = conn
                            .query_row("SELECT count(*) FROM project", [], |r| r.get(0))
                            .unwrap();
                        let _ = reply.send(());
                    }
                }
            }
        })
    };
    let t_b = Instant::now();
    tx.send(Job::Slow).unwrap();
    let mut replies = Vec::new();
    for _ in 0..READS {
        let (rtx, rrx) = mpsc::channel();
        tx.send(Job::Count(rtx)).unwrap();
        replies.push(rrx);
    }
    for r in replies {
        r.recv().unwrap();
    }
    let elapsed_b = t_b.elapsed();
    drop(tx);
    worker.join().unwrap();

    // 两边都做了「1 次慢写 + 20 次小读」，所以这两个数才可比。
    println!(
        "[experiment] 1 slow write + {READS} reads: mutex {elapsed_a:?} vs worker {elapsed_b:?}"
    );
    assert!(elapsed_a > Duration::ZERO && elapsed_b > Duration::ZERO);
}
