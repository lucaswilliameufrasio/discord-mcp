use discord_mcp::channels::{ChannelStore, StoreError};
use discord_mcp::webhook::WebhookUrl;

#[test]
fn encrypted_channels_survive_restart_and_wrong_key_cannot_replace_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("channels.db");
    let key = [7_u8; 32];
    let secret_a = "https://discord.com/api/webhooks/123/test-secret-a";
    let secret_b = "https://discord.com/api/webhooks/456/test-secret-b";
    {
        let store = ChannelStore::open(&path, &key).unwrap();
        store.put_webhook("alpha", secret_a).unwrap();
        store.put_webhook("beta", secret_b).unwrap();
        assert_eq!(store.list().unwrap(), vec!["alpha", "beta"]);
    }
    let raw = std::fs::read(&path).unwrap();
    assert!(
        !raw.windows(secret_a.len())
            .any(|part| part == secret_a.as_bytes())
    );
    assert!(
        !raw.windows(secret_b.len())
            .any(|part| part == secret_b.as_bytes())
    );
    assert!(matches!(
        ChannelStore::open(&path, &[8_u8; 32]),
        Err(StoreError::Decryption)
    ));
    let store = ChannelStore::open(&path, &key).unwrap();
    assert_eq!(
        store.get_webhook("alpha").unwrap(),
        WebhookUrl::parse(secret_a).unwrap()
    );
    assert_eq!(
        store.get_webhook("beta").unwrap(),
        WebhookUrl::parse(secret_b).unwrap()
    );
    assert!(matches!(
        store.get_webhook("missing"),
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        store.put_webhook("invalid name", secret_a),
        Err(StoreError::InvalidInput)
    ));
    store
        .put_bot("robot", "123", "synthetic-bot-token")
        .unwrap();
    assert!(matches!(
        store.get_channel("robot"),
        Ok(discord_mcp::channels::Channel::Bot(_))
    ));
    assert!(matches!(
        store.get_webhook("robot"),
        Err(StoreError::InvalidInput)
    ));
    drop(store);
    let raw = std::fs::read(&path).unwrap();
    assert!(
        !raw.windows(b"synthetic-bot-token".len())
            .any(|v| v == b"synthetic-bot-token")
    );
    assert!(matches!(
        ChannelStore::open(&path, &key)
            .unwrap()
            .get_channel("robot"),
        Ok(discord_mcp::channels::Channel::Bot(_))
    ));
}

#[test]
fn cli_registers_and_lists_without_printing_webhook() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let key = hex::encode([9_u8; 32]);
    let secret = "https://discord.com/api/webhooks/123/synthetic-token";
    let mut add = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env("DISCORD_MCP_DB", &path)
        .env("DISCORD_MCP_KEY", &key)
        .args(["add-channel", "alerts"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    add.stdin
        .take()
        .unwrap()
        .write_all(secret.as_bytes())
        .unwrap();
    let output = add.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-token"));
    let output = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env("DISCORD_MCP_DB", &path)
        .env("DISCORD_MCP_KEY", &key)
        .arg("list-channels")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "alerts");
}
