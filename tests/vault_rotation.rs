use discord_mcp::{
    channels::{ChannelStore, EmbeddedSecret, StoreError},
    recovery::prepare_restored_copy,
};
use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

fn rotate(path: &std::path::Path, key: &[u8; 32], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", path)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args(["setup", "local", "vault", "rotate-oauth", "--confirm"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn explicit_rotation_rejects_stale_values_and_preserves_data_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let key = [83; 32];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let preview = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", "invalid-private-marker")
        .args(["setup", "local", "vault", "rotate-oauth", "--preview"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(!db.exists());
    assert!(!String::from_utf8_lossy(&preview.stdout).contains("invalid-private-marker"));
    assert!(
        !rotate(&db, &key, "old-synthetic\nnew-synthetic\n")
            .status
            .success()
    );
    assert!(!db.exists());
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-channel-token")
        .unwrap();
    store
        .insert_embedded_secret(secret, "old-synthetic")
        .unwrap();
    drop(store);
    assert!(
        !rotate(&db, &[86; 32], "old-synthetic\nnew-synthetic\n")
            .status
            .success()
    );
    for input in [
        "wrong\nnew-synthetic\n",
        "old-synthetic\nold-synthetic\n",
        "old-synthetic\nnew-synthetic",
        "old-synthetic\nnew-synthetic\nthird\n",
    ] {
        let output = rotate(&db, &key, input);
        assert!(!output.status.success());
        let output = String::from_utf8_lossy(&output.stderr);
        assert!(!output.contains("old-synthetic"));
        assert!(!output.contains("new-synthetic"));
    }
    let sql = rusqlite::Connection::open(&db).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_vault_rotation BEFORE UPDATE ON embedded_secrets BEGIN SELECT RAISE(ABORT, 'failure'); END;").unwrap();
    drop(sql);
    assert!(
        !rotate(&db, &key, "old-synthetic\nnew-synthetic\n")
            .status
            .success()
    );
    let sql = rusqlite::Connection::open(&db).unwrap();
    sql.execute_batch("DROP TRIGGER reject_vault_rotation;")
        .unwrap();
    drop(sql);
    assert!(
        rotate(&db, &key, "old-synthetic\nnew-synthetic\n")
            .status
            .success()
    );
    assert!(
        !rotate(&db, &key, "old-synthetic\nother-synthetic\n")
            .status
            .success()
    );
    let store = ChannelStore::open(&db, &key).unwrap();
    assert_eq!(store.get_embedded_secret(secret).unwrap(), "new-synthetic");
    assert!(store.get_channel("alerts").is_ok());
}

#[test]
fn stale_concurrent_writers_have_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let key = [84; 32];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .insert_embedded_secret(secret, "old-synthetic")
        .unwrap();
    drop(store);
    let spawn = |value: &'static str| {
        let path = db.clone();
        std::thread::spawn(move || rotate(&path, &key, value).status.success())
    };
    let first = spawn("old-synthetic\nfirst-new\n");
    let second = spawn("old-synthetic\nsecond-new\n");
    assert_eq!(
        usize::from(first.join().unwrap()) + usize::from(second.join().unwrap()),
        1
    );
    let store = ChannelStore::open(&db, &key).unwrap();
    assert!(matches!(
        store.get_embedded_secret(secret).unwrap().as_str(),
        "first-new" | "second-new"
    ));
}

#[test]
fn quarantine_refuses_rotation_without_reintroducing_a_secret() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let recovery = dir.path().join("recovery.db");
    let key = [85; 32];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let store = ChannelStore::open(&source, &key).unwrap();
    store
        .insert_embedded_secret(secret, "old-synthetic")
        .unwrap();
    drop(store);
    prepare_restored_copy(&source, &recovery, &key, 90).unwrap();
    assert!(
        !rotate(&recovery, &key, "old-synthetic\nnew-synthetic\n")
            .status
            .success()
    );
    let recovered = ChannelStore::open_recovery_copy(&recovery, &key).unwrap();
    assert!(matches!(
        recovered.get_embedded_secret(secret),
        Err(StoreError::RecoveryPending)
    ));
    recovered.activate_recovered_copy().unwrap();
    assert!(matches!(
        recovered.get_embedded_secret(secret),
        Err(StoreError::NotFound)
    ));
}
