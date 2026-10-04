use worktrace_lib::services::{catalog, handshake};
use worktrace_lib::storage::{db::Db, meta::init_meta, migrations::migrate};

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
