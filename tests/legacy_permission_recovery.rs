use discord_mcp::{
    channels::{AuditActor, AuditContext, ChannelAction, ChannelStore, StoreError},
    recovery::prepare_restored_copy,
};

fn create_legacy_fixture(path: &std::path::Path, key: &[u8; 32]) {
    let store = ChannelStore::open(path, key).unwrap();
    store.put_bot("support", "123", "synthetic-token").unwrap();
    store.set_permissions("101", "support", true, true).unwrap();
    store
        .set_permissions("102", "support", false, true)
        .unwrap();
    drop(store);
    // This test-only database models the earlier schema; it is not a real backup.
    let database = rusqlite::Connection::open(path).unwrap();
    database
        .execute_batch("DROP TABLE channel_permission_revisions; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
}

#[test]
fn legacy_database_migration_preserves_grants_and_enables_revision_checks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let key = [139_u8; 32];
    create_legacy_fixture(&path, &key);
    let store = ChannelStore::open(&path, &key).unwrap();
    assert!(
        store
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        store
            .is_permitted("101", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        store
            .is_permitted("102", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("102", "support", ChannelAction::Read)
            .unwrap()
    );
    let (version, revision) = store.permission_context("support").unwrap();
    assert_eq!(revision, 0);
    store.set_permissions("102", "support", true, true).unwrap();
    let context = AuditContext {
        actor: AuditActor::Discord("123"),
        now: 1000,
        retention_days: 90,
    };
    assert!(matches!(
        store.set_permissions_audited_if_version(
            "101",
            "support",
            false,
            false,
            context,
            (version.unwrap(), revision)
        ),
        Err(StoreError::AuthorizationDenied)
    ));
    assert!(
        store
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    drop(store);
    let reopened = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(reopened.permission_context("support").unwrap().1, 1);
    assert!(
        reopened
            .is_permitted("102", "support", ChannelAction::Read)
            .unwrap()
    );
}

#[test]
fn simultaneous_legacy_opens_preserve_existing_permissions() {
    use std::sync::{Arc, Barrier};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-concurrent.db");
    let key = [141_u8; 32];
    create_legacy_fixture(&path, &key);
    let barrier = Arc::new(Barrier::new(5));
    let mut workers = Vec::new();
    for _ in 0..4 {
        let barrier = barrier.clone();
        let path = path.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            let store = ChannelStore::open(&path, &key).unwrap();
            assert!(
                store
                    .is_permitted("101", "support", ChannelAction::Read)
                    .unwrap()
            );
            assert!(
                store
                    .is_permitted("102", "support", ChannelAction::Send)
                    .unwrap()
            );
        }));
    }
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    let store = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(store.permission_context("support").unwrap().1, 0);
}

#[test]
fn legacy_restore_migrates_copy_without_mutating_source_or_reinstating_authority() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let output = dir.path().join("restored.db");
    let key = [140_u8; 32];
    create_legacy_fixture(&source, &key);
    let baseline = std::fs::read(&source).unwrap();
    let report = prepare_restored_copy(&source, &output, &key, 90).unwrap();
    assert_eq!(report.permissions, 2);
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    let restored = ChannelStore::open(&output, &key).unwrap();
    assert!(restored.recovery_quarantined().unwrap());
    assert!(matches!(
        restored.permission_context("support"),
        Err(StoreError::RecoveryPending)
    ));
    assert!(matches!(
        restored.set_permissions("101", "support", true, true),
        Err(StoreError::RecoveryPending)
    ));
    restored.activate_recovered_copy().unwrap();
    assert!(
        !restored
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !restored
            .is_permitted("102", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(restored.get_channel("support").is_ok());
    let (version, revision) = restored.permission_context("support").unwrap();
    assert_eq!(revision, 0);
    restored
        .set_permissions("101", "support", true, false)
        .unwrap();
    let context = AuditContext {
        actor: AuditActor::Discord("123"),
        now: 1000,
        retention_days: 90,
    };
    assert!(matches!(
        restored.set_permissions_audited_if_version(
            "101",
            "support",
            false,
            true,
            context,
            (version.unwrap(), revision)
        ),
        Err(StoreError::AuthorizationDenied)
    ));
    assert!(
        restored
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    drop(restored);
    let original = ChannelStore::open(&source, &key).unwrap();
    assert!(
        original
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        original
            .is_permitted("101", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        original
            .is_permitted("102", "support", ChannelAction::Send)
            .unwrap()
    );
}

#[test]
fn restore_rejects_incompatible_revision_table_without_publishing() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("bad-source.db");
    let output = dir.path().join("not-published.db");
    let key = [142_u8; 32];
    create_legacy_fixture(&source, &key);
    {
        let database = rusqlite::Connection::open(&source).unwrap();
        database.execute_batch("CREATE TABLE channel_permission_revisions(channel_name TEXT); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    }
    let baseline = std::fs::read(&source).unwrap();
    assert!(prepare_restored_copy(&source, &output, &key, 90).is_err());
    assert!(!output.exists());
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
}

#[test]
fn rejects_revision_schema_and_invalid_rows_on_open_and_restore() {
    let cases = [
        "CREATE TABLE channel_permission_revisions(channel_name TEXT NOT NULL REFERENCES channels(name) ON DELETE CASCADE, revision INTEGER NOT NULL)",
        "CREATE TABLE channel_permission_revisions(channel_name TEXT PRIMARY KEY NOT NULL, revision INTEGER NOT NULL)",
        "CREATE TABLE channel_permission_revisions(channel_name TEXT PRIMARY KEY NOT NULL REFERENCES channels(name) ON DELETE CASCADE, revision INTEGER NOT NULL); INSERT INTO channel_permission_revisions VALUES('support', -1)",
        "CREATE TABLE channel_permission_revisions(channel_name TEXT PRIMARY KEY NOT NULL REFERENCES channels(name) ON DELETE CASCADE, revision INTEGER NOT NULL); INSERT INTO channel_permission_revisions VALUES('support', 'invalid')",
        "CREATE TABLE channel_permission_revisions(channel_name TEXT PRIMARY KEY NOT NULL REFERENCES channels(name) ON DELETE CASCADE, revision INTEGER NOT NULL); INSERT INTO channel_permission_revisions VALUES('missing', 1)",
    ];
    for sql in cases {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let source = dir.path().join("source.db");
        let output = dir.path().join("output.db");
        let key = [143; 32];
        create_legacy_fixture(&source, &key);
        {
            let db = rusqlite::Connection::open(&source).unwrap();
            db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
            db.execute_batch(sql).unwrap();
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        }
        let baseline = std::fs::read(&source).unwrap();
        assert!(
            prepare_restored_copy(&source, &output, &key, 90).is_err(),
            "{sql}"
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(&source).unwrap(), baseline);
        assert!(ChannelStore::open(&source, &key).is_err(), "{sql}");
    }
}
