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

#[test]
fn audit_preview_preserves_all_rows_and_excludes_inconclusive_and_invalid_times() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("preview.db");
    let store = ChannelStore::open(&database, &[8; 32]).unwrap();
    let sql = rusqlite::Connection::open(&database).unwrap();
    for (n, state, finished) in [
        (0, "confirmed", Some(99)),
        (1, "confirmed", Some(100)),
        (2, "confirmed", None),
        (3, "confirmed", Some(5)),
        (4, "started", None),
        (5, "unknown", Some(40)),
    ] {
        let request = format!("{n:064x}");
        store
            .begin_backup_audit(&request, "fixture", "123", 10, &[n as u8; 32])
            .unwrap();
        sql.execute(
            "UPDATE backup_audit SET state=?2,finished_at=?3 WHERE request_id=?1",
            rusqlite::params![request, state, finished],
        )
        .unwrap();
    }
    let before: Vec<(String, String, Option<i64>, Vec<u8>)> = {
        let mut statement = sql.prepare("SELECT a.request_id,a.state,a.finished_at,b.fingerprint FROM backup_audit a JOIN backup_target_binding b ON a.request_id=b.request_id ORDER BY a.request_id").unwrap();
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let preview = store.preview_backup_audit(100).unwrap();
    assert_eq!(
        (
            preview.total,
            preview.confirmed_before_cutoff,
            preview.started,
            preview.unknown,
            preview.other_records
        ),
        (6, 1, 1, 1, 3)
    );
    assert!(store.preview_backup_audit(-1).is_err());
    assert_eq!(store.preview_backup_audit(100).unwrap(), preview);
    let after: Vec<(String, String, Option<i64>, Vec<u8>)> = {
        let mut statement = sql.prepare("SELECT a.request_id,a.state,a.finished_at,b.fingerprint FROM backup_audit a JOIN backup_target_binding b ON a.request_id=b.request_id ORDER BY a.request_id").unwrap();
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(before, after);
}

#[test]
fn ciphertext_commitment_is_insert_once_and_old_attempts_remain_unbound() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("commitment.db");
    let store = ChannelStore::open(&database, &[8; 32]).unwrap();
    let legacy = "a".repeat(64);
    store
        .begin_backup_audit(&legacy, "legacy", "123", 10, &[1; 32])
        .unwrap();
    store.finish_backup_audit(&legacy, false, 11).unwrap();
    assert_eq!(store.backup_ciphertext_commitment(&legacy).unwrap(), None);
    assert!(
        store
            .commit_backup_ciphertext(&legacy, &[2; 32], 50)
            .is_err()
    );
    let current = "b".repeat(64);
    store
        .begin_backup_audit(&current, "new", "123", 12, &[3; 32])
        .unwrap();
    store
        .commit_backup_ciphertext(&current, &[4; 32], 100)
        .unwrap();
    assert!(
        store
            .commit_backup_ciphertext(&current, &[5; 32], 101)
            .is_err()
    );
    assert_eq!(
        store.backup_ciphertext_commitment(&current).unwrap(),
        Some(([4; 32], 100))
    );
    assert_eq!(store.backup_ciphertext_commitment(&legacy).unwrap(), None);
}

#[test]
fn reconciliation_evidence_requires_current_inconclusive_bound_commitment_and_is_insert_once() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("evidence.db");
    let store = ChannelStore::open(&database, &[8; 32]).unwrap();
    let id = "d".repeat(64);
    store
        .begin_backup_audit(&id, "fixture", "123", 10, &[1; 32])
        .unwrap();
    store.commit_backup_ciphertext(&id, &[2; 32], 100).unwrap();
    assert!(
        store
            .record_backup_match_evidence(&id, "123", 11, &[9; 32], &[2; 32], 100)
            .is_err()
    );
    assert!(
        store
            .record_backup_match_evidence(&id, "123", 11, &[1; 32], &[9; 32], 100)
            .is_err()
    );
    assert!(
        store
            .record_backup_match_evidence(&id, "123", 11, &[1; 32], &[2; 32], 101)
            .is_err()
    );
    assert!(
        store.recent_backup_audit().unwrap()[0]
            .evidence_at
            .is_none()
    );
    store
        .record_backup_match_evidence(&id, "123", 11, &[1; 32], &[2; 32], 100)
        .unwrap();
    assert!(
        store
            .record_backup_match_evidence(&id, "123", 12, &[1; 32], &[2; 32], 100)
            .is_err()
    );
    let rows = store.recent_backup_audit().unwrap();
    assert_eq!(rows[0].state, "started");
    assert_eq!(rows[0].evidence_at, Some(11));
    assert_eq!(rows[0].evidence_actor.as_deref(), Some("123"));
    let confirmed = "e".repeat(64);
    store
        .begin_backup_audit(&confirmed, "confirmed", "123", 13, &[1; 32])
        .unwrap();
    store
        .commit_backup_ciphertext(&confirmed, &[2; 32], 100)
        .unwrap();
    store.finish_backup_audit(&confirmed, true, 14).unwrap();
    assert!(
        store
            .record_backup_match_evidence(&confirmed, "123", 15, &[1; 32], &[2; 32], 100)
            .is_err()
    );
}
