use discord_mcp::{
    channels::{ChannelStore, EmbeddedSecret, StoreError},
    recovery::prepare_restored_copy,
};

#[cfg(unix)]
#[test]
fn cli_enrollment_and_explicit_embedded_server_fail_closed() {
    use std::{
        io::Write,
        net::TcpListener,
        process::{Command, Stdio},
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let key = [73; 32];
    let binary = env!("CARGO_BIN_EXE_discord-mcp");
    let enrollment = || {
        let mut command = Command::new(binary);
        command
            .env_clear()
            .env("DISCORD_MCP_DB", &path)
            .env("DISCORD_MCP_KEY", hex::encode(key));
        command.args(["setup", "local", "vault", "discord-oauth", "--confirm"]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    };
    let run_enroll = |value: &str| {
        let mut child = enrollment().spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(value.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    assert!(!run_enroll("synthetic-oauth-marker\n").status.success());
    assert!(!path.exists());
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-channel-token")
        .unwrap();
    drop(store);
    let output = run_enroll("synthetic-oauth-marker\n");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-oauth-marker"));
    assert!(!run_enroll("different-value\n").status.success());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut command = Command::new(binary);
    command
        .env_clear()
        .arg("serve")
        .env("DISCORD_MCP_DB", &path)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .env("DISCORD_MCP_SECRET_PROVIDER", "embedded")
        .env("DISCORD_MCP_LISTEN", addr.to_string())
        .env("DISCORD_MCP_PUBLIC_ORIGIN", format!("http://{addr}"))
        .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
        .env("DISCORD_MCP_ADMIN_ID", "1")
        .env("DISCORD_MCP_CLIENT_ID", "client")
        .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
        .env(
            "DISCORD_MCP_CLIENT_REDIRECT",
            "http://127.0.0.1:8765/callback",
        );
    let rejected = command
        .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "conflict")
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    command.env_remove("DISCORD_MCP_DISCORD_CLIENT_SECRET");
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    struct Cleanup<'a>(&'a mut std::process::Child);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    use std::io::{BufRead, BufReader};
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let guard = Cleanup(&mut child);
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    assert!(line.contains("loopback only"));
    assert!(!line.contains("synthetic-oauth-marker"));
    drop(guard);
    let store = ChannelStore::open(&path, &key).unwrap();
    assert!(store.get_channel("alerts").is_ok());
    assert_eq!(
        store
            .get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret)
            .unwrap(),
        "synthetic-oauth-marker"
    );
}

#[test]
fn encrypted_secret_survives_restart_and_never_implicitly_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let key = [73; 32];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let store = ChannelStore::open(&path, &key).unwrap();
    assert!(matches!(
        store.get_embedded_secret(secret),
        Err(StoreError::NotFound)
    ));
    for invalid in ["", "contains whitespace", "invalid\nline"] {
        assert!(matches!(
            store.insert_embedded_secret(secret, invalid),
            Err(StoreError::InvalidInput)
        ));
    }
    assert!(
        store
            .insert_embedded_secret(secret, &"a".repeat(513))
            .is_err()
    );
    store
        .put_bot("alerts", "123", "synthetic-channel-token")
        .unwrap();
    store
        .insert_embedded_secret(secret, "synthetic-oauth-marker")
        .unwrap();
    assert!(store.insert_embedded_secret(secret, "replacement").is_err());
    drop(store);
    assert!(ChannelStore::open(&path, &[74; 32]).is_err());
    let store = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(
        store.get_embedded_secret(secret).unwrap(),
        "synthetic-oauth-marker"
    );
    assert!(store.get_channel("alerts").is_ok());
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    let (nonce, ciphertext): (Vec<u8>, Vec<u8>) = db
        .query_row(
            "SELECT nonce, ciphertext FROM embedded_secrets",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(nonce.len(), 24);
    assert!(
        !ciphertext
            .windows(b"synthetic-oauth-marker".len())
            .any(|window| window == b"synthetic-oauth-marker")
    );
    db.execute(
        "UPDATE embedded_secrets SET ciphertext=zeroblob(length(ciphertext))",
        [],
    )
    .unwrap();
    drop(db);
    let store = ChannelStore::open(&path, &key).unwrap();
    assert!(matches!(
        store.get_embedded_secret(secret),
        Err(StoreError::Decryption)
    ));
}

#[test]
fn embedded_secret_cannot_be_accessed_or_created_in_quarantine() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let destination = dir.path().join("recovery.db");
    let key = [75; 32];
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let store = ChannelStore::open(&source, &key).unwrap();
    store
        .insert_embedded_secret(secret, "synthetic-value")
        .unwrap();
    drop(store);
    prepare_restored_copy(&source, &destination, &key, 90).unwrap();
    let recovered = ChannelStore::open_recovery_copy(&destination, &key).unwrap();
    assert!(matches!(
        recovered.get_embedded_secret(secret),
        Err(StoreError::RecoveryPending)
    ));
    assert!(matches!(
        recovered.insert_embedded_secret(secret, "other"),
        Err(StoreError::RecoveryPending)
    ));
    drop(recovered);
    let db = rusqlite::Connection::open(&destination).unwrap();
    db.execute("INSERT INTO embedded_secrets(name, nonce, ciphertext) VALUES ('discord_oauth_client_secret', zeroblob(24), zeroblob(32))", []).unwrap();
    drop(db);
    let recovered = ChannelStore::open_recovery_copy(&destination, &key).unwrap();
    assert!(recovered.activate_recovered_copy().is_err());
    drop(recovered);
    let db = rusqlite::Connection::open(&destination).unwrap();
    db.execute("DELETE FROM embedded_secrets", []).unwrap();
    drop(db);
    let recovered = ChannelStore::open_recovery_copy(&destination, &key).unwrap();
    recovered.activate_recovered_copy().unwrap();
    assert!(matches!(
        recovered.get_embedded_secret(secret),
        Err(StoreError::NotFound)
    ));
    let original = ChannelStore::open(&source, &key).unwrap();
    assert_eq!(
        original.get_embedded_secret(secret).unwrap(),
        "synthetic-value"
    );
}
