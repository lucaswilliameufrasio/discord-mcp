#![cfg(feature = "encrypted-s3-backup")]

use discord_mcp::channels::ChannelStore;

#[test]
fn legacy_attempt_remains_unbound_and_binding_failure_rolls_back_intent() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("audit.db");
    let key = [7; 32];
    drop(ChannelStore::open(&database, &key).unwrap());
    let sql = rusqlite::Connection::open(&database).unwrap();
    sql.execute_batch("DROP TABLE backup_target_binding")
        .unwrap();
    let legacy = "a".repeat(64);
    sql.execute("INSERT INTO backup_audit(request_id,backup_id,actor_id,started_at,state) VALUES(?1,'legacy','123',1,'unknown')", [&legacy]).unwrap();
    let store = ChannelStore::open(&database, &key).unwrap();
    assert_eq!(
        store.inconclusive_backup_target(&legacy).unwrap(),
        Some(("legacy".to_owned(), None))
    );
    sql.execute_batch("CREATE TRIGGER synthetic_binding_failure BEFORE INSERT ON backup_target_binding BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;").unwrap();
    let attempted = "b".repeat(64);
    assert!(
        store
            .begin_backup_audit(&attempted, "new", "123", 1, &[9; 32])
            .is_err()
    );
    assert!(
        store
            .inconclusive_backup_target(&attempted)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.inconclusive_backup_target(&legacy).unwrap(),
        Some(("legacy".to_owned(), None))
    );
}
