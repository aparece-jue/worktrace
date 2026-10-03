//! P1 Task 2 要求的迁移测试：空库初始化、重复启动、未来版本拒绝、
//! 故障中断后无半套表、备份失败不迁移。
//!
//! 尚无 UI，用临时文件库验证（内存库无法覆盖 WAL 与崩溃恢复语义）。

use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::migrations::{
    current_version, migrate, migrate_with_ddl, user_tables, SCHEMA_VERSION,
};

fn temp_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path().join("worktrace.db")).expect("open");
    (dir, db)
}

#[test]
fn empty_database_initialises_cleanly() {
    let (_dir, db) = temp_db();
    migrate(db.connection()).expect("空库应能迁移");

    assert_eq!(current_version(db.connection()).unwrap(), SCHEMA_VERSION);
    let tables = user_tables(db.connection()).unwrap();
    assert_eq!(tables.len(), 12, "实际：{tables:?}");
    assert!(tables.contains(&"app_meta".to_string()));
    assert!(tables.contains(&"task_tag".to_string()));
}

/// 「重复启动」：同一个库文件被两个进程依次打开并迁移，第二次必须无副作用。
#[test]
fn repeated_startup_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worktrace.db");

    let before = {
        let db = Db::open(&path).unwrap();
        migrate(db.connection()).unwrap();
        user_tables(db.connection()).unwrap()
    };

    let db2 = Db::open(&path).unwrap();
    migrate(db2.connection()).expect("第二次启动应成功");
    assert_eq!(user_tables(db2.connection()).unwrap(), before);
    assert_eq!(current_version(db2.connection()).unwrap(), SCHEMA_VERSION);
}

#[test]
fn a_future_database_version_is_refused_not_downgraded() {
    let (_dir, db) = temp_db();
    migrate(db.connection()).unwrap();
    db.connection()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();

    let err = migrate(db.connection()).unwrap_err();
    assert_eq!(err.code(), "STORAGE_ERROR");
    assert_eq!(
        current_version(db.connection()).unwrap(),
        SCHEMA_VERSION + 1,
        "拒绝未来版本后不得改动其版本号"
    );
}

/// 「故障中断后无半套表」：在真实磁盘库上注入一段中途失败的 DDL，
/// 断言表与版本**都**保持原样。SQLite 的 DDL 是事务性的，整批要么全成要么全无。
///
/// 注意必须从**空库**注入：`migrate_with_ddl` 会先看 `user_version`，
/// 已是当前版本就直接返回，坏 DDL 根本不会执行。
#[test]
fn interrupted_migration_leaves_no_partial_schema_on_disk() {
    let (_dir, db) = temp_db();
    assert_eq!(
        current_version(db.connection()).unwrap(),
        0,
        "本用例必须从空库开始"
    );

    // 假装这是「v1 迁移」：前半段合法、后半段语法错误。
    let broken = "CREATE TABLE v1_alpha(id TEXT PRIMARY KEY NOT NULL);\n\
                  CREATE TABLE v1_beta(id TEXT PRIMARY KEY NOT NULL);\n\
                  CREATE TABLE v1_gamma(%%% not sql %%%);\n";
    let err = migrate_with_ddl(db.connection(), broken).unwrap_err();
    assert_eq!(err.code(), "STORAGE_ERROR");

    assert!(
        user_tables(db.connection()).unwrap().is_empty(),
        "失败迁移不得留下 v1_alpha / v1_beta 这种半套表，实际：{:?}",
        user_tables(db.connection()).unwrap()
    );
    assert_eq!(
        current_version(db.connection()).unwrap(),
        0,
        "版本也不得前进"
    );

    // 坏迁移之后，正常迁移仍应能把库建起来。
    migrate(db.connection()).expect("故障之后正常迁移仍应成功");
    assert_eq!(user_tables(db.connection()).unwrap().len(), 12);
}

/// 「备份失败不迁移」：备份属于 P6 的初始化编排，这里验证**迁移自身不会
/// 在被拒绝时改动库**——即失败路径不产生副作用。
#[test]
fn a_refused_migration_changes_nothing() {
    let (_dir, db) = temp_db();
    migrate(db.connection()).unwrap();
    let before = user_tables(db.connection()).unwrap();
    let rev_before = current_version(db.connection()).unwrap();

    db.connection()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    let err = migrate(db.connection()).unwrap_err();
    assert_eq!(err.code(), "STORAGE_ERROR");
    assert_eq!(user_tables(db.connection()).unwrap(), before, "表不得变化");

    db.connection()
        .pragma_update(None, "user_version", rev_before)
        .unwrap();
    migrate(db.connection()).expect("恢复版本后仍可迁移");
}

/// 迁移后外键真的在生效——这是 `configure` 里最容易被漏掉、且漏了看不出来的一条。
#[test]
fn foreign_keys_are_enforced_after_migration() {
    let (_dir, db) = temp_db();
    migrate(db.connection()).unwrap();

    let err = db
        .connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
             VALUES('s1','no-such-task','no-such-run','FOREGROUND','running','stopwatch',1,0)",
            [],
        )
        .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("foreign key"),
        "外键应拒绝悬挂引用，实际：{err}"
    );
}
