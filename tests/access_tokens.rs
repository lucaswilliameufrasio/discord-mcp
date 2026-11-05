use discord_mcp::channels::{Channel, ChannelAction, ChannelStore, StoreError};

#[test]
fn opaque_tokens_are_resource_bound_expire_revoke_and_are_not_stored_as_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.db");
    let key = [21; 32];
    let resource = "https://example.invalid/mcp";
    let store = ChannelStore::open(&path, &key).unwrap();
    let token = store.issue_access_token("123", resource, 1000, 60).unwrap();
    assert_eq!(
        store
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .as_deref(),
        Some("123")
    );
    assert!(
        store
            .verify_access_token(&token, "https://other.invalid/mcp", 1001)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .verify_access_token(&token, resource, 1060)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .verify_access_token("invalid", resource, 1001)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.issue_access_token("123", "http://external.invalid/mcp", 1000, 60),
        Err(StoreError::InvalidInput)
    ));
    assert!(matches!(
        store.issue_access_token("123", resource, 1000, 3601),
        Err(StoreError::InvalidInput)
    ));
    drop(store);
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.windows(token.len()).any(|v| v == token.as_bytes()));
    let store = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(
        store
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .as_deref(),
        Some("123")
    );
    store.revoke_access_token(&token).unwrap();
    assert!(
        store
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .is_none()
    );
}

#[test]
fn verified_identity_and_channel_permission_are_both_required() {
    let dir = tempfile::tempdir().unwrap();
    let store = ChannelStore::open(&dir.path().join("access.db"), &[22; 32]).unwrap();
    let resource = "https://example.invalid/mcp";
    store.put_bot("general", "123", "synthetic-token").unwrap();
    let token = store.issue_access_token("321", resource, 1000, 60).unwrap();
    let identity = store
        .verify_access_token(&token, resource, 1001)
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.authorized_channel(&identity, "general", ChannelAction::Read),
        Err(StoreError::PermissionDenied)
    ));
    store
        .set_permissions("321", "general", true, false)
        .unwrap();
    assert!(matches!(
        store.authorized_channel(&identity, "general", ChannelAction::Read),
        Ok(Channel::Bot(_))
    ));
    assert!(matches!(
        store.authorized_channel(&identity, "general", ChannelAction::Send),
        Err(StoreError::PermissionDenied)
    ));
    store
        .set_permissions("321", "general", false, false)
        .unwrap();
    assert!(matches!(
        store.authorized_channel(&identity, "general", ChannelAction::Read),
        Err(StoreError::PermissionDenied)
    ));
}
