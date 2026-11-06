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
        recovered.list_active(),
        Err(StoreError::RecoveryPending)
    ));
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
fn corrupt_channel_ciphertext_rejects_staged_restore_without_touching_source() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let destination = dir.path().join("prepared.db");
    let key = [148; 32];
    let store = ChannelStore::open(&source, &key).unwrap();
    store.put_bot("valid", "123", "synthetic-token").unwrap();
    store.put_bot("damaged", "456", "synthetic-token").unwrap();
    store.set_permissions("101", "valid", true, false).unwrap();
    drop(store);
    {
        let db = rusqlite::Connection::open(&source).unwrap();
        assert_eq!(
            db.execute(
                "UPDATE channels SET ciphertext=zeroblob(length(ciphertext)) WHERE name='damaged'",
                []
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    }
    let baseline = std::fs::read(&source).unwrap();
    assert!(matches!(
        prepare_restored_copy(&source, &destination, &key, 90),
        Err(discord_mcp::recovery::RecoveryError::InvalidKeyOrData)
    ));
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!dir.path().join(format!("prepared.db{suffix}")).exists());
    }
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".discord-mcp-recovery-")
    }));
    let source_store = ChannelStore::open(&source, &key).unwrap();
    assert!(
        source_store
            .is_permitted("101", "valid", ChannelAction::Read)
            .unwrap()
    );
    assert!(source_store.get_channel("valid").is_ok());
    assert!(matches!(
        source_store.get_channel("damaged"),
        Err(StoreError::Decryption)
    ));
}

#[test]
fn recovery_prepare_cli_wrong_key_cleans_staging_and_allows_safe_retry() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let destination = dir.path().join("prepared.db");
    let key = [149_u8; 32];
    let source_store = ChannelStore::open(&source, &key).unwrap();
    source_store
        .put_bot("alerts", "123", "synthetic-token")
        .unwrap();
    source_store
        .set_permissions("101", "alerts", true, false)
        .unwrap();
    drop(source_store);
    let baseline = std::fs::read(&source).unwrap();
    let run = |key: [u8; 32]| {
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .args([
                std::ffi::OsStr::new("recovery-prepare"),
                source.as_os_str(),
                destination.as_os_str(),
            ])
            .output()
            .unwrap()
    };
    let failure = run([150; 32]);
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stderr).contains("InvalidKeyOrData"));
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!dir.path().join(format!("prepared.db{suffix}")).exists());
    }
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".discord-mcp-recovery-")
    }));
    let success = run(key);
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    assert!(
        String::from_utf8_lossy(&success.stdout)
            .contains("Quarantined copy prepared; invalidated permissions=1")
    );
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    let prepared = ChannelStore::open(&destination, &key).unwrap();
    assert!(prepared.recovery_quarantined().unwrap());
    assert!(matches!(
        prepared.get_channel("alerts"),
        Err(StoreError::RecoveryPending)
    ));
    drop(prepared);
    let destination_baseline = std::fs::read(&destination).unwrap();
    let repeat = run(key);
    assert!(!repeat.status.success());
    assert!(String::from_utf8_lossy(&repeat.stderr).contains("DestinationExists"));
    assert_eq!(std::fs::read(&destination).unwrap(), destination_baseline);
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    let original = ChannelStore::open(&source, &key).unwrap();
    assert!(
        original
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
}

#[test]
fn activation_cli_rejects_reintroduced_authority_and_preserves_source() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let key = [151_u8; 32];
    let store = ChannelStore::open(&source, &key).unwrap();
    store.put_bot("alerts", "123", "synthetic-token").unwrap();
    store.set_permissions("101", "alerts", true, false).unwrap();
    drop(store);
    let source_baseline = std::fs::read(&source).unwrap();
    let cases = [
        (
            "permission",
            "INSERT INTO channel_permissions VALUES ('102','alerts',1,0)",
            "channel_permissions",
        ),
        (
            "token",
            "INSERT INTO mcp_access_tokens VALUES (x'01','102','https://example.invalid/mcp',9999999999)",
            "mcp_access_tokens",
        ),
        (
            "code",
            "INSERT INTO mcp_authorization_codes VALUES (x'01','client','http://127.0.0.1/callback','https://example.invalid/mcp','102','synthetic',1,9999999999)",
            "mcp_authorization_codes",
        ),
    ];
    for (name, inject, table) in cases {
        let destination = dir.path().join(format!("{name}.db"));
        let prepare = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .args([
                std::ffi::OsStr::new("recovery-prepare"),
                source.as_os_str(),
                destination.as_os_str(),
            ])
            .output()
            .unwrap();
        assert!(
            prepare.status.success(),
            "{}",
            String::from_utf8_lossy(&prepare.stderr)
        );
        {
            // Simulate an external non-cooperating writer on this disposable copy.
            let db = rusqlite::Connection::open(&destination).unwrap();
            db.execute(inject, []).unwrap();
        }
        let activate = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_MCP_DB", &destination)
            .args(["recovery-activate", "--confirm"])
            .output()
            .unwrap();
        assert!(!activate.status.success(), "{name}");
        assert!(
            String::from_utf8_lossy(&activate.stderr).contains("InvalidInput"),
            "{name}"
        );
        let recovered = ChannelStore::open(&destination, &key).unwrap();
        assert!(recovered.recovery_quarantined().unwrap(), "{name}");
        assert!(matches!(
            recovered.get_channel("alerts"),
            Err(StoreError::RecoveryPending)
        ));
        assert!(
            recovered
                .audit_events(None, Some("recovery_activated"), 10)
                .unwrap()
                .is_empty()
        );
        drop(recovered);
        let db = rusqlite::Connection::open(&destination).unwrap();
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{name}");
        drop(db);
        assert_eq!(std::fs::read(&source).unwrap(), source_baseline, "{name}");
    }
    let original = ChannelStore::open(&source, &key).unwrap();
    assert!(
        original
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
}

#[test]
fn activation_audit_failure_rolls_back_quarantine_transition() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let destination = dir.path().join("quarantine.db");
    let key = [153_u8; 32];
    let source_store = ChannelStore::open(&source, &key).unwrap();
    source_store
        .put_bot("alerts", "123", "synthetic-token")
        .unwrap();
    source_store
        .set_permissions("101", "alerts", true, false)
        .unwrap();
    drop(source_store);
    let source_baseline = std::fs::read(&source).unwrap();
    prepare_restored_copy(&source, &destination, &key, 90).unwrap();
    {
        let db = rusqlite::Connection::open(&destination).unwrap();
        // Fault injection only after preparation: restored sources with triggers
        // are rejected, and this trigger exists solely in the synthetic copy.
        db.execute_batch("CREATE TRIGGER fail_activation_audit AFTER INSERT ON audit_events WHEN NEW.event_type='recovery_activated' BEGIN SELECT RAISE(ABORT, 'synthetic audit failure'); END;").unwrap();
    }
    let before = ChannelStore::open(&destination, &key).unwrap();
    let quarantine_audits = before
        .audit_events(None, Some("recovery_quarantined"), 10)
        .unwrap()
        .len();
    assert_eq!(quarantine_audits, 1);
    assert!(before.recovery_quarantined().unwrap());
    drop(before);
    let result = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &destination)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args(["recovery-activate", "--confirm"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Storage"));
    let after = ChannelStore::open(&destination, &key).unwrap();
    assert!(after.recovery_quarantined().unwrap());
    assert!(matches!(
        after.get_channel("alerts"),
        Err(StoreError::RecoveryPending)
    ));
    assert!(matches!(
        after.set_permissions("101", "alerts", true, false),
        Err(StoreError::RecoveryPending)
    ));
    assert!(
        after
            .audit_events(None, Some("recovery_activated"), 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        after
            .audit_events(None, Some("recovery_quarantined"), 10)
            .unwrap()
            .len(),
        quarantine_audits
    );
    drop(after);
    let db = rusqlite::Connection::open(&destination).unwrap();
    for table in [
        "channel_permissions",
        "mcp_access_tokens",
        "mcp_authorization_codes",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    drop(db);
    assert_eq!(std::fs::read(&source).unwrap(), source_baseline);
    let original = ChannelStore::open(&source, &key).unwrap();
    assert!(
        original
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
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
