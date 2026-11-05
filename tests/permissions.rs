use discord_mcp::channels::{ChannelAction, ChannelStore, StoreError};

#[test]
fn grants_are_per_user_per_channel_and_per_action_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("permissions.db");
    let key = [13; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store.put_bot("support", "123", "synthetic-token").unwrap();
    store.put_bot("private", "456", "synthetic-token").unwrap();
    assert!(
        !store
            .is_permitted("1", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("1", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(matches!(
        store.set_permissions("1", "missing", true, false),
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        store.set_permissions("not-a-discord-id", "support", true, false),
        Err(StoreError::InvalidInput)
    ));
    store.set_permissions("1", "support", true, false).unwrap();
    store.set_permissions("2", "support", false, true).unwrap();
    drop(store);
    let store = ChannelStore::open(&path, &key).unwrap();
    assert!(
        store
            .is_permitted("1", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("1", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("2", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        store
            .is_permitted("2", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("1", "private", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("3", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("bad", "support", ChannelAction::Read)
            .unwrap()
    );
    store.set_permissions("1", "support", false, false).unwrap();
    assert!(
        !store
            .is_permitted("1", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        store
            .is_permitted("2", "support", ChannelAction::Send)
            .unwrap()
    );
}

#[test]
fn channel_replacement_and_deletion_revoke_existing_grants() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("permissions.db");
    let key = [14; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store.put_bot("channel", "123", "synthetic-token").unwrap();
    store.set_permissions("1", "channel", true, true).unwrap();
    store
        .put_webhook(
            "channel",
            "https://discord.com/api/webhooks/123/synthetic-token",
        )
        .unwrap();
    assert!(matches!(
        store.set_permissions("1", "channel", true, false),
        Err(StoreError::InvalidInput)
    ));
    assert!(
        !store
            .is_permitted("1", "channel", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("1", "channel", ChannelAction::Send)
            .unwrap()
    );
    store.set_permissions("1", "channel", false, true).unwrap();
    store.delete("channel").unwrap();
    store.put_bot("channel", "456", "synthetic-token").unwrap();
    assert!(
        !store
            .is_permitted("1", "channel", ChannelAction::Send)
            .unwrap()
    );
}

#[test]
fn existing_channel_database_is_upgraded_without_losing_channels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let key = [15; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store.put_bot("legacy", "123", "synthetic-token").unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("DROP TABLE channel_permissions", []).unwrap();
    drop(db);
    let store = ChannelStore::open(&path, &key).unwrap();
    assert!(matches!(
        store.get_channel("legacy"),
        Ok(discord_mcp::channels::Channel::Bot(_))
    ));
    assert!(
        !store
            .is_permitted("1", "legacy", ChannelAction::Send)
            .unwrap()
    );
}
