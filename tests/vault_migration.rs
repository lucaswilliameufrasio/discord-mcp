use std::process::Command;

use discord_mcp::{
    channels::{ChannelStore, EmbeddedSecret, StoreError},
    recovery::prepare_restored_copy,
};

fn command(path: &std::path::Path, key: &[u8; 32]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
    cmd.env_clear()
        .env("DISCORD_MCP_DB", path)
        .env("DISCORD_MCP_KEY", hex::encode(key));
    cmd
}

#[test]
fn explicit_env_migration_preserves_source_and_rejects_existing_or_invalid_destination() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.db");
    let key = [77; 32];
    let args = ["setup", "local", "vault", "migrate-oauth-from-env"];
    let preview = command(&path, &key)
        .args(args)
        .arg("--preview")
        .env(
            "DISCORD_MCP_DISCORD_CLIENT_SECRET",
            "synthetic-source-marker",
        )
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(!String::from_utf8_lossy(&preview.stdout).contains("synthetic-source-marker"));
    assert!(!path.exists());
    let run = |db: &std::path::Path, key: &[u8; 32], source: Option<&str>| {
        let mut cmd = command(db, key);
        cmd.args(args).arg("--confirm");
        if let Some(value) = source {
            cmd.env("DISCORD_MCP_DISCORD_CLIENT_SECRET", value);
        }
        cmd.output().unwrap()
    };
    assert!(
        !run(&path, &key, Some("synthetic-source-marker"))
            .status
            .success()
    );
    assert!(!path.exists());
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-channel-token")
        .unwrap();
    drop(store);
    assert!(!run(&path, &key, None).status.success());
    assert!(!run(&path, &key, Some("invalid value")).status.success());
    assert!(
        !run(&path, &[78; 32], Some("synthetic-source-marker"))
            .status
            .success()
    );
    let result = run(&path, &key, Some("synthetic-source-marker"));
    assert!(result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-source-marker"));
    assert!(
        !run(&path, &key, Some("different-secret-marker"))
            .status
            .success()
    );
    let store = ChannelStore::open(&path, &key).unwrap();
    assert!(store.get_channel("alerts").is_ok());
    assert_eq!(
        store
            .get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret)
            .unwrap(),
        "synthetic-source-marker"
    );
}

#[test]
fn migration_rejects_quarantined_database_and_keeps_source_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = dir.path().join("source.db");
    let recovered = dir.path().join("recovered.db");
    let key = [79; 32];
    drop(ChannelStore::open(&path, &key).unwrap());
    let baseline = std::fs::read(&path).unwrap();
    prepare_restored_copy(&path, &recovered, &key, 90).unwrap();
    let output = command(&recovered, &key)
        .args([
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-env",
            "--confirm",
        ])
        .env(
            "DISCORD_MCP_DISCORD_CLIENT_SECRET",
            "synthetic-source-marker",
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let store = ChannelStore::open_recovery_copy(&recovered, &key).unwrap();
    assert!(matches!(
        store.get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret),
        Err(StoreError::RecoveryPending)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), baseline);
}

#[test]
fn concurrent_migration_has_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let key = [80; 32];
    drop(ChannelStore::open(&path, &key).unwrap());
    let spawn = |value| {
        command(&path, &key)
            .args([
                "setup",
                "local",
                "vault",
                "migrate-oauth-from-env",
                "--confirm",
            ])
            .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", value)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut first = spawn("synthetic-first");
    let mut second = spawn("synthetic-second");
    assert_eq!(
        usize::from(first.wait().unwrap().success())
            + usize::from(second.wait().unwrap().success()),
        1
    );
    let store = ChannelStore::open(&path, &key).unwrap();
    let value = store
        .get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret)
        .unwrap();
    assert!(value == "synthetic-first" || value == "synthetic-second");
}
