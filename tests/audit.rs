use discord_mcp::channels::{
    AuditActor, AuditContext, Channel, ChannelAction, ChannelStore, LoginKind,
};

#[test]
fn audit_persists_structured_events_filters_and_cursor_without_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.db");
    let key = [101; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-secret-token")
        .unwrap();
    store
        .record_discord_login(LoginKind::Admin, AuditActor::Discord("123"), true, 1000, 90)
        .unwrap();
    store
        .record_discord_login(LoginKind::Admin, AuditActor::Unknown, false, 1001, 90)
        .unwrap();
    store
        .set_permissions_audited(
            "456",
            "alerts",
            true,
            false,
            AuditContext {
                actor: AuditActor::Discord("123"),
                now: 1002,
                retention_days: 90,
            },
        )
        .unwrap();
    let permissions = store
        .audit_events(None, Some("permission_change"), 50)
        .unwrap();
    assert_eq!(permissions.len(), 1);
    assert_eq!(permissions[0].actor_id.as_deref(), Some("123"));
    assert_eq!(permissions[0].subject_user_id.as_deref(), Some("456"));
    assert_eq!(permissions[0].channel.as_deref(), Some("alerts"));
    assert_eq!(permissions[0].can_read, Some(true));
    assert_eq!(permissions[0].can_send, Some(false));
    let rows = store.audit_events(None, None, 2).unwrap();
    assert_eq!(rows.len(), 2);
    let previous = store.audit_events(Some(rows[1].id), None, 2).unwrap();
    assert_eq!(previous.len(), 2);
    assert_eq!(previous[0].event_type, "admin_login");
    assert!(store.audit_events(None, Some("unknown"), 50).is_err());
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    let raw: String = db.query_row("SELECT group_concat(COALESCE(actor_id,'') || COALESCE(channel_name,'') || COALESCE(subject_user_id,'')) FROM audit_events", [], |row| row.get(0)).unwrap();
    assert!(!raw.contains("synthetic-secret-token"));
    drop(db);
    assert_eq!(
        ChannelStore::open(&path, &key)
            .unwrap()
            .audit_events(None, None, 50)
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn audit_failure_rolls_back_permission_change_and_channel_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.db");
    let key = [102; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store.put_bot("alerts", "123", "synthetic-token").unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT, 'test audit unavailable'); END;").unwrap();
    assert!(store.set_permissions("123", "alerts", true, false).is_err());
    assert!(
        !store
            .is_permitted("123", "alerts", ChannelAction::Read)
            .unwrap()
    );
    db.execute_batch("DROP TRIGGER fail_audit;").unwrap();
    store.set_permissions("123", "alerts", true, false).unwrap();
    db.execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT, 'test audit unavailable'); END;").unwrap();
    assert!(
        store
            .put_webhook(
                "alerts",
                "https://discord.com/api/webhooks/123/synthetic-token"
            )
            .is_err()
    );
    assert!(matches!(store.get_channel("alerts"), Ok(Channel::Bot(_))));
    assert!(
        store
            .is_permitted("123", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert!(store.delete("alerts").is_err());
    assert!(
        store
            .is_permitted("123", "alerts", ChannelAction::Read)
            .unwrap()
    );
}

#[test]
fn retention_prunes_old_events_and_keeps_current() {
    let dir = tempfile::tempdir().unwrap();
    let store = ChannelStore::open(&dir.path().join("audit.db"), &[103; 32]).unwrap();
    store
        .record_discord_login(LoginKind::Mcp, AuditActor::Unknown, false, 1000, 1)
        .unwrap();
    store
        .record_discord_login(
            LoginKind::Admin,
            AuditActor::Discord("123"),
            true,
            100_000,
            1,
        )
        .unwrap();
    let events = store.audit_events(None, None, 50).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "admin_login");
    assert!(store.prune_audit(100_000, 0).is_err());
}
