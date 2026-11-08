use discord_mcp::{
    channels::{ChannelStore, EmbeddedSecret},
    recovery::prepare_restored_copy,
};
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn compare(path: &Path, key: &[u8; 32], candidate: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", path)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args(["setup", "local", "vault", "compare-oauth", "--confirm"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(candidate.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn comparison_reconciles_observed_state_without_writing_or_exposing_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.db");
    let key = [88; 32];
    assert!(!compare(&path, &key, "candidate\n").status.success());
    assert!(!path.exists());
    let store = ChannelStore::open(&path, &key).unwrap();
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    let before = std::fs::read(&path).unwrap();
    let absent = compare(&path, &key, "candidate\n");
    assert!(absent.status.success());
    assert!(String::from_utf8_lossy(&absent.stdout).contains("no embedded OAuth secret"));
    assert_eq!(before, std::fs::read(&path).unwrap());
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    let current = compare(&path, &key, "synthetic-old\n");
    assert!(current.status.success());
    assert!(String::from_utf8_lossy(&current.stdout).contains("candidate matches"));
    assert!(!String::from_utf8_lossy(&current.stdout).contains("synthetic-old"));
    let different = compare(&path, &key, "synthetic-new\n");
    assert!(different.status.success());
    assert!(String::from_utf8_lossy(&different.stdout).contains("candidate differs"));
    store
        .rotate_embedded_secret(secret, "synthetic-old", "synthetic-new")
        .unwrap();
    drop(store);
    assert!(
        String::from_utf8_lossy(&compare(&path, &key, "synthetic-new\n").stdout)
            .contains("candidate matches")
    );
    assert!(
        String::from_utf8_lossy(&compare(&path, &key, "synthetic-old\n").stdout)
            .contains("candidate differs")
    );
    assert!(
        !compare(&path, &[89; 32], "synthetic-new\n")
            .status
            .success()
    );
    for invalid in ["", "synthetic-new", "synthetic-new\nextra\n"] {
        assert!(!compare(&path, &key, invalid).status.success());
    }
    let store = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(store.get_embedded_secret(secret).unwrap(), "synthetic-new");
}

#[test]
fn quarantined_and_corrupt_values_are_not_reported_as_absent_or_matching() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = dir.path().join("vault.db");
    let recovery = dir.path().join("recovery.db");
    let key = [90; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .insert_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret, "synthetic-value")
        .unwrap();
    drop(store);
    prepare_restored_copy(&path, &recovery, &key, 90).unwrap();
    assert!(
        !compare(&recovery, &key, "synthetic-value\n")
            .status
            .success()
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE embedded_secrets SET ciphertext=zeroblob(length(ciphertext))",
        [],
    )
    .unwrap();
    drop(db);
    assert!(!compare(&path, &key, "synthetic-value\n").status.success());
}
