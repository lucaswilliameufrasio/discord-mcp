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
