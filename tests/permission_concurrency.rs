use discord_mcp::channels::{AuditActor, AuditContext, ChannelStore, StoreError};
use std::sync::{Arc, Barrier};

fn context() -> AuditContext<'static> {
    AuditContext {
        actor: AuditActor::Discord("123"),
        now: 1000,
        retention_days: 90,
    }
}

#[test]
fn permission_revisions_serialize_per_channel_and_detect_aba() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrency.db");
    let key = [79_u8; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store.put_bot("first", "123", "synthetic-token").unwrap();
    store.put_bot("second", "456", "synthetic-token").unwrap();
    let (version, revision) = store.permission_context("first").unwrap();
    let version = version.unwrap();
    let (other_version, other_revision) = store.permission_context("second").unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for (read, send) in [(true, false), (false, true)] {
        let barrier = barrier.clone();
        let path = path.clone();
        workers.push(std::thread::spawn(move || {
            let store = ChannelStore::open(&path, &key).unwrap();
            barrier.wait();
            store.set_permissions_audited_if_version(
                "101",
                "first",
                read,
                send,
                context(),
                (version, revision),
            )
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|value| value.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|value| matches!(value, Err(StoreError::AuthorizationDenied)))
            .count(),
        1
    );
    assert!(
        store
            .set_permissions_audited_if_version(
                "102",
                "second",
                true,
                false,
                context(),
                (other_version.unwrap(), other_revision)
            )
            .is_ok()
    );
    store.set_permissions("101", "first", true, false).unwrap();
    let (version, revision) = store.permission_context("first").unwrap();
    store.set_permissions("101", "first", false, false).unwrap();
    store.set_permissions("101", "first", true, false).unwrap();
    assert!(matches!(
        store.set_permissions_audited_if_version(
            "101",
            "first",
            false,
            true,
            context(),
            (version.unwrap(), revision)
        ),
        Err(StoreError::AuthorizationDenied)
    ));
}

#[test]
fn exhausted_revision_rejects_permission_changes_atomically() {
    use discord_mcp::channels::{ChannelAction, current_timestamp};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("exhausted.db");
    let key = [80_u8; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store.put_bot("alerts", "123", "synthetic-token").unwrap();
    store.put_bot("other", "456", "synthetic-token").unwrap();
    store.set_permissions("101", "alerts", true, false).unwrap();
    let external = rusqlite::Connection::open(&path).unwrap();
    external
        .execute(
            "UPDATE channel_permission_revisions SET revision=?1 WHERE channel_name='alerts'",
            [i64::MAX - 1],
        )
        .unwrap();
    let (version, revision) = store.permission_context("alerts").unwrap();
    assert_eq!(revision, i64::MAX - 1);
    let audit_context = || AuditContext {
        actor: AuditActor::Discord("123"),
        now: current_timestamp().unwrap(),
        retention_days: 90,
    };
    store
        .set_permissions_audited_if_version(
            "101",
            "alerts",
            true,
            false,
            audit_context(),
            (version.unwrap(), revision),
        )
        .unwrap();
    assert_eq!(store.permission_context("alerts").unwrap().1, i64::MAX);
    let audit_before = store
        .audit_events(None, Some("permission_change"), 100)
        .unwrap()
        .len();
    for (read, send) in [(false, true), (false, false)] {
        assert!(matches!(
            store.set_permissions("101", "alerts", read, send),
            Err(StoreError::Storage)
        ));
        assert!(matches!(
            store.set_permissions_audited_if_version(
                "101",
                "alerts",
                read,
                send,
                audit_context(),
                (version.unwrap(), i64::MAX)
            ),
            Err(StoreError::Storage)
        ));
    }
    assert!(
        store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("101", "alerts", ChannelAction::Send)
            .unwrap()
    );
    let revision: (i64, String) = external.query_row("SELECT revision, typeof(revision) FROM channel_permission_revisions WHERE channel_name='alerts'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(revision, (i64::MAX, "integer".to_owned()));
    assert_eq!(
        store
            .audit_events(None, Some("permission_change"), 100)
            .unwrap()
            .len(),
        audit_before
    );
    store.set_permissions("102", "other", true, false).unwrap();
    assert!(
        store
            .is_permitted("102", "other", ChannelAction::Read)
            .unwrap()
    );
    drop(store);
    let reopened = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(reopened.permission_context("alerts").unwrap().1, i64::MAX);
    assert!(
        reopened
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
}
