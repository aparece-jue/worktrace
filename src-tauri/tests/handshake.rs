use worktrace_lib::services::{catalog, handshake};
use worktrace_lib::storage::{
    db::Db,
    meta::{bump_revision, init_meta, require_meta, rotate_epoch},
    migrations::migrate,
    run_repo,
};

#[test]
fn initial_handshake_needs_no_epoch_and_restored_identity_rejects_old_queries() {
    let mut db = Db::open_in_memory().unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let original = init_meta(&tx).unwrap();
    tx.commit().unwrap();
    let first = handshake::get_revision(&db).unwrap();
    assert_eq!(first.data_epoch, original.data_epoch);
    assert_eq!(first.revision, 0);
    assert!(catalog::list_projects(&db, &first.data_epoch, None)
        .unwrap()
        .items
        .is_empty());
    // Model restore replacing the identity with a lower revision.
    db.connection()
        .execute("UPDATE app_meta SET revision=5", [])
        .unwrap();
    assert_eq!(handshake::get_revision(&db).unwrap().revision, 5);
    db.connection()
        .execute(
            "UPDATE app_meta SET data_epoch='replacement-epoch', revision=0",
            [],
        )
        .unwrap();
    let restored = handshake::get_revision(&db).unwrap();
    assert_eq!(restored.data_epoch, "replacement-epoch");
    assert_eq!(restored.revision, 0);
    assert_eq!(
        catalog::list_projects(&db, &first.data_epoch, None)
            .unwrap_err()
            .code(),
        "DATA_EPOCH_MISMATCH"
    );
    assert_eq!(handshake::get_revision(&db).unwrap(), restored);
}

/// 恢复/替换库的提交路径换库身份：`meta::rotate_epoch` **只动 `data_epoch`、不动 `revision`**，
/// 返回新值；它与 `run_repo::start_run` 在**同一个事务**里（P6 Task 4 的 ③-a）。
///
/// 分工写死：`init_meta` 只在建库时 INSERT 一次（重复 INSERT 会撞 `app_meta.singleton`
/// 的 PK）；`bump_revision` 只动 `revision`；`rotate_epoch` 只动 `data_epoch`。新 epoch 内
/// 不存在「旧响应」，所以它**不 bump revision**——那只会给「一次业务写恰好 +1」多造一条
/// 没有业务含义的例外。
#[test]
fn rotate_epoch_changes_only_the_epoch_and_returns_the_new_value() {
    let mut db = Db::open_in_memory().unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let original = init_meta(&tx).unwrap();
    tx.commit().unwrap();

    // 先让 revision 非零：否则「没动 revision」与「revision 本来就是 0」分不开。
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let revision = bump_revision(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(revision, 1);

    // 提交路径的事务形状：新 run + 新 epoch，一次提交。
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    run_repo::start_run(&tx, "restored-run", 1_700_000_000_000).unwrap();
    let rotated = rotate_epoch(&tx).unwrap();
    tx.commit().unwrap();

    let after = require_meta(db.connection()).unwrap();
    assert_ne!(rotated, original.data_epoch, "新 epoch 必须是全新的值");
    assert_eq!(after.data_epoch, rotated, "返回的就是库里那个新值");
    assert_eq!(after.revision, revision, "rotate_epoch 不得动 revision");
    assert!(
        run_repo::get_run(db.connection(), "restored-run")
            .unwrap()
            .is_some(),
        "与它同一事务的 start_run 一起提交"
    );

    // 旧 epoch 的请求从此被拒（与 `rotate_epoch` 是同一个机制，不在这里重写判断）。
    assert_eq!(
        catalog::list_projects(&db, &original.data_epoch, None)
            .unwrap_err()
            .code(),
        "DATA_EPOCH_MISMATCH"
    );
}

/// 回滚路径**不调** `rotate_epoch`；即便调了它也**随调用方的事务回滚**——
/// 它不自开事务、不自行提交（仓储的写口径与 `start_run` 相同）。
#[test]
fn rotate_epoch_rolls_back_with_its_transaction() {
    let mut db = Db::open_in_memory().unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let original = init_meta(&tx).unwrap();
    tx.commit().unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    run_repo::start_run(&tx, "rolled-back-run", 1_700_000_000_000).unwrap();
    let rotated = rotate_epoch(&tx).unwrap();
    assert_ne!(rotated, original.data_epoch, "函数本身返回新值");
    drop(tx); // 回滚

    let after = require_meta(db.connection()).unwrap();
    assert_eq!(
        after.data_epoch, original.data_epoch,
        "回滚后原库的 epoch 原样保留"
    );
    assert_eq!(after.revision, 0);
    assert!(
        run_repo::get_run(db.connection(), "rolled-back-run")
            .unwrap()
            .is_none(),
        "同一事务里的 run 一起回滚"
    );
}

#[test]
fn handshake_on_uninitialized_database_fails_without_creating_metadata() {
    let db = Db::open_in_memory().unwrap();
    migrate(db.connection()).unwrap();
    assert_eq!(
        handshake::get_revision(&db).unwrap_err().code(),
        "STORAGE_ERROR"
    );
    assert_eq!(
        db.connection()
            .query_row("SELECT count(*) FROM app_meta", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
