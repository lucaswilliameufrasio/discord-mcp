//! Synthetic SQLite transaction experiment; not an adoption verifier or application API.
use discord_mcp::channels::{ChannelStore, EmbeddedSecret};
use rusqlite::{Connection, Error, ErrorCode, TransactionBehavior, params};
use std::time::Duration;

#[test]
fn local_decision_is_atomic_with_epoch_check_but_can_become_stale_after_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.db");
    let store = ChannelStore::open(&path, &[116; 32]).unwrap();
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let operation = [71; 16];
    let instance = [72; 16];
    let epoch = [73; 16];
    let changed_epoch = [74; 16];
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            operation,
            &[instance],
        )
        .unwrap();
    store
        .begin_rotation_process(operation, instance, epoch, "synthetic-new")
        .unwrap();

    let mut verifier = Connection::open(&path).unwrap();
    let writer = Connection::open(&path).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    verifier
        .execute_batch("CREATE TABLE fixture_decision (epoch BLOB NOT NULL)")
        .unwrap();
    let tx = verifier
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let current: Vec<u8> = tx
        .query_row(
            "SELECT boot FROM rotation_process_epoch WHERE revision=?1 AND instance=?2",
            params![operation.as_slice(), instance.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(current, epoch);
    let conflict = writer
        .execute(
            "UPDATE rotation_process_epoch SET boot=?1 WHERE revision=?2 AND instance=?3",
            params![
                changed_epoch.as_slice(),
                operation.as_slice(),
                instance.as_slice()
            ],
        )
        .unwrap_err();
    assert!(
        matches!(conflict, Error::SqliteFailure(ref error, _) if error.code == ErrorCode::DatabaseBusy)
    );
    tx.execute(
        "INSERT INTO fixture_decision(epoch) VALUES (?1)",
        [epoch.as_slice()],
    )
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(
        writer
            .execute(
                "UPDATE rotation_process_epoch SET boot=?1 WHERE revision=?2 AND instance=?3",
                params![
                    changed_epoch.as_slice(),
                    operation.as_slice(),
                    instance.as_slice()
                ]
            )
            .unwrap(),
        1
    );
    let (recorded, current): (Vec<u8>, Vec<u8>) = verifier.query_row("SELECT d.epoch,e.boot FROM fixture_decision d JOIN rotation_process_epoch e ON e.revision=?1 AND e.instance=?2", params![operation.as_slice(), instance.as_slice()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(recorded, epoch);
    assert_eq!(current, changed_epoch);
    assert!(
        store
            .check_rotation_process_epoch(operation, instance, epoch)
            .is_err()
    );
}
