use std::process::Command;

use discord_mcp::{
    channels::{AuthorizationGrant, ChannelAction, ChannelStore, StoreError},
    recovery::prepare_restored_copy,
};

#[test]
fn token_validation_checks_quarantine_with_token_in_same_snapshot() {
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    let resource = "https://example.invalid/mcp";
    let store = ChannelStore::open(&path, &[132; 32]).unwrap();
    let token = store.issue_access_token("101", resource, 1000, 60).unwrap();
    assert_eq!(
        store
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .as_deref(),
        Some("101")
    );

    // Model the old split read with two independently committed statements.
    let prior = store.recovery_quarantined().unwrap();
    assert!(!prior);
    let writer = rusqlite::Connection::open(&path).unwrap();
    // Deliberately retain the token to model inconsistent/untrusted restored
    // state; normal quarantine also deletes credentials in the same transaction.
    writer
        .execute(
            "INSERT INTO recovery_control(id,quarantined) VALUES(1,1)",
            [],
        )
        .unwrap();
    let hash = Sha256::digest(token.as_bytes());
    let stale_lookup: String = writer.query_row(
        "SELECT user_id FROM mcp_access_tokens WHERE token_hash=?1 AND resource=?2 AND expires_at>?3",
        rusqlite::params![hash.as_slice(), resource, 1001], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        stale_lookup, "101",
        "split reads would combine incompatible states"
    );
    assert_eq!(
        store.verify_access_token(&token, resource, 1001).unwrap(),
        None
    );
    assert_eq!(
        store
            .verify_access_token(&token, "https://other.invalid/mcp", 1001)
            .unwrap(),
        None
    );
    assert_eq!(
        store.verify_access_token(&token, resource, 1060).unwrap(),
        None
    );
}

#[test]
fn preparation_quarantines_and_invalidates_restored_authority_before_activation() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("restored.db");
    let output = dir.path().join("quarantined.db");
    let key = [131; 32];
    let resource = "https://example.invalid/mcp";
    let store = ChannelStore::open(&source, &key).unwrap();
    store.put_bot("alerts", "123", "synthetic-token").unwrap();
    store.set_permissions("101", "alerts", true, true).unwrap();
    let token = store.issue_access_token("101", resource, 1000, 60).unwrap();
    let grant = AuthorizationGrant {
        user_id: "101",
        client_id: "client",
        redirect_uri: "http://127.0.0.1:8765/callback",
        resource,
        code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
    };
    store.issue_authorization_code(&grant, 1000).unwrap();
    drop(store);
    let baseline = std::fs::read(&source).unwrap();
    let report = prepare_restored_copy(&source, &output, &key, 90).unwrap();
    assert_eq!((report.permissions, report.tokens, report.codes), (1, 1, 1));
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    let recovered = ChannelStore::open(&output, &key).unwrap();
    assert!(recovered.recovery_quarantined().unwrap());
    assert!(matches!(
        recovered.get_channel("alerts"),
        Err(StoreError::RecoveryPending)
    ));
    assert!(matches!(
        recovered.set_permissions("101", "alerts", true, false),
        Err(StoreError::RecoveryPending)
    ));
    assert!(
        recovered
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        recovered.issue_access_token("101", resource, 1001, 60),
        Err(StoreError::RecoveryPending)
    ));
    let no_confirm = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env("DISCORD_MCP_DB", &output)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .arg("recovery-activate")
        .output()
        .unwrap();
    assert!(!no_confirm.status.success());
    for args in [vec![], vec!["web"], vec!["list-channels"]] {
        let result = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env("DISCORD_MCP_DB", &output)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("quarantined"));
    }
    assert!(recovered.recovery_quarantined().unwrap());
    drop(recovered);
    let activate = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env("DISCORD_MCP_DB", &output)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args(["recovery-activate", "--confirm"])
        .output()
        .unwrap();
    assert!(activate.status.success());
    let recovered = ChannelStore::open(&output, &key).unwrap();
    assert!(!recovered.recovery_quarantined().unwrap());
    assert!(recovered.get_channel("alerts").is_ok());
    assert!(
        !recovered
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        recovered
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        recovered
            .audit_events(None, Some("recovery_quarantined"), 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        recovered
            .audit_events(None, Some("recovery_activated"), 10)
            .unwrap()
            .len(),
        1
    );
    let original = ChannelStore::open(&source, &key).unwrap();
    assert!(
        original
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert_eq!(
        original
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .as_deref(),
        Some("101")
    );
}

#[test]
fn failures_never_publish_or_overwrite_the_destination() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let output = dir.path().join("destination.db");
    let key = [132; 32];
    let store = ChannelStore::open(&source, &key).unwrap();
    store.put_bot("alerts", "123", "synthetic-token").unwrap();
    drop(store);
    let baseline = std::fs::read(&source).unwrap();
    assert!(prepare_restored_copy(&source, &output, &[0; 32], 90).is_err());
    assert!(!output.exists());
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    std::fs::write(&output, b"protected-synthetic").unwrap();
    assert!(prepare_restored_copy(&source, &output, &key, 90).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"protected-synthetic");
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    let corrupt = dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"invalid synthetic sqlite").unwrap();
    let new_output = dir.path().join("new.db");
    assert!(prepare_restored_copy(&corrupt, &new_output, &key, 90).is_err());
    assert!(!new_output.exists());
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
}

#[test]
fn refuses_missing_key_marker_triggers_and_source_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let destination = dir.path().join("prepared.db");
    let key = [133; 32];
    drop(ChannelStore::open(&source, &key).unwrap());
    let db = rusqlite::Connection::open(&source).unwrap();
    db.execute("DELETE FROM key_check", []).unwrap();
    drop(db);
    assert!(prepare_restored_copy(&source, &destination, &key, 90).is_err());
    assert!(!destination.exists());
    let db = rusqlite::Connection::open(&source).unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM key_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    drop(db);
    drop(ChannelStore::open(&source, &key).unwrap());
    let db = rusqlite::Connection::open(&source).unwrap();
    db.execute_batch("CREATE TRIGGER malicious AFTER DELETE ON channel_permissions BEGIN DELETE FROM channels; END;").unwrap();
    drop(db);
    assert!(prepare_restored_copy(&source, &destination, &key, 90).is_err());
    assert!(!destination.exists());
    let sidecar = dir.path().join("source.db-wal");
    std::fs::write(&sidecar, b"synthetic sidecar").unwrap();
    assert!(prepare_restored_copy(&source, &destination, &key, 90).is_err());
    assert_eq!(std::fs::read(&sidecar).unwrap(), b"synthetic sidecar");
    assert!(!destination.exists());
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".discord-mcp-recovery-")
    }));
}
