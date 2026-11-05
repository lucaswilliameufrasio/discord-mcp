use discord_mcp::channels::{ChannelStore, Overview};

#[test]
fn overview_counts_persisted_channels_grants_users_and_unexpired_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("overview.db");
    let key = [81; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(
        store.overview(1000).unwrap(),
        Overview {
            channels: 0,
            permitted_users: 0,
            read_grants: 0,
            send_grants: 0,
            unexpired_tokens: 0
        }
    );
    store.put_bot("one", "123", "synthetic-token").unwrap();
    store.put_bot("two", "456", "synthetic-token").unwrap();
    store.set_permissions("1", "one", true, true).unwrap();
    store.set_permissions("1", "two", false, true).unwrap();
    store.set_permissions("2", "one", true, false).unwrap();
    let valid = store
        .issue_access_token("1", "https://example.invalid/mcp", 1000, 100)
        .unwrap();
    store
        .issue_access_token("2", "https://example.invalid/mcp", 1000, 10)
        .unwrap();
    assert_eq!(
        store.overview(1050).unwrap(),
        Overview {
            channels: 2,
            permitted_users: 2,
            read_grants: 2,
            send_grants: 2,
            unexpired_tokens: 1
        }
    );
    store.revoke_access_token(&valid).unwrap();
    store.delete("one").unwrap();
    drop(store);
    let store = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(
        store.overview(1050).unwrap(),
        Overview {
            channels: 1,
            permitted_users: 1,
            read_grants: 0,
            send_grants: 1,
            unexpired_tokens: 0
        }
    );
}
