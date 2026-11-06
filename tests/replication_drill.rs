//! Opt-in synthetic snapshot/restore exercise using the sibling replication tool.
use std::{path::Path, process::Stdio, time::Duration};

use discord_mcp::{
    channels::{
        AuthorizationExchange, AuthorizationGrant, Channel, ChannelAction, ChannelStore,
        OperationalEvent,
    },
    recovery::prepare_restored_copy,
    webhook::WebhookUrl,
};
use rmcp::serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

async fn replicate(
    example: &Path,
    database: &Path,
    repository: &Path,
    destination: &Path,
) -> std::process::Output {
    let mut command = Command::new(example);
    command
        .args([database, repository, destination])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    timeout(Duration::from_secs(30), command.output())
        .await
        .expect("replication timeout")
        .expect("run replication tool")
}

async fn request(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    value: Value,
) -> Value {
    stdin
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
    stdin.flush().await.unwrap();
    let mut line = String::new();
    timeout(Duration::from_secs(5), stdout.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    rmcp::serde_json::from_str(&line).unwrap()
}

#[tokio::test]
#[ignore = "requires DISCORD_MCP_REPLICATION_EXAMPLE pointing to the independently built sibling example"]
async fn snapshot_and_restore_isolated_synthetic_wal_database() {
    let example =
        std::env::var_os("DISCORD_MCP_REPLICATION_EXAMPLE").expect("set replication example path");
    let example = Path::new(&example);
    assert!(example.is_file(), "replication example not found");
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = dir.path().join("source.db");
    let repo = dir.path().join("replica");
    let restored = dir.path().join("restored.db");
    let prepared = dir.path().join("prepared.db");
    let key = [111; 32];
    let store = ChannelStore::open(&source, &key).unwrap();
    let url = "https://discord.com/api/webhooks/123/synthetic-drill-token";
    store.put_webhook("alerts", url).unwrap();
    store
        .put_bot("support", "456", "synthetic-bot-token")
        .unwrap();
    store
        .set_permissions("101", "support", true, false)
        .unwrap();
    store.set_permissions("202", "alerts", false, true).unwrap();
    let resource = "https://example.invalid/mcp";
    let token = store.issue_access_token("101", resource, 1000, 60).unwrap();
    let code = store
        .issue_authorization_code(
            &AuthorizationGrant {
                user_id: "101",
                client_id: "client",
                redirect_uri: "http://127.0.0.1:8765/callback",
                resource,
                code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            },
            1000,
        )
        .unwrap();
    store
        .record_operational_event(1_000_000, 30, OperationalEvent::Read { failed: true })
        .unwrap();
    // Keep a writer connection open and a committed synthetic marker in the WAL.
    let writer = rusqlite::Connection::open(&source).unwrap();
    writer.execute_batch("PRAGMA wal_autocheckpoint=0; CREATE TABLE drill_marker(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO drill_marker VALUES (1, 'synthetic-marker');").unwrap();
    let wal = source.with_file_name("source.db-wal");
    assert!(std::fs::metadata(wal).unwrap().len() > 0);
    let baseline = store.list().unwrap();
    assert!(
        store
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        store
            .is_permitted("202", "alerts", ChannelAction::Send)
            .unwrap()
    );

    let output = replicate(example, &source, &repo, &restored).await;
    assert!(
        output.status.success(),
        "synthetic snapshot and restore failed"
    );
    let read_only = rusqlite::Connection::open_with_flags(
        &restored,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let integrity: String = read_only
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let marker: String = read_only
        .query_row("SELECT value FROM drill_marker WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(marker, "synthetic-marker");
    drop(read_only);
    assert!(ChannelStore::open(&restored, &[0; 32]).is_err());
    let recovered = ChannelStore::open(&restored, &key).unwrap();
    assert_eq!(recovered.list().unwrap(), baseline);
    assert_eq!(
        recovered.get_webhook("alerts").unwrap(),
        WebhookUrl::parse(url).unwrap()
    );
    assert!(matches!(
        recovered.get_channel("support"),
        Ok(Channel::Bot(_))
    ));
    assert!(
        recovered
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !recovered
            .is_permitted("101", "support", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        recovered
            .is_permitted("202", "alerts", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        !recovered
            .is_permitted("303", "alerts", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        !recovered
            .audit_events(None, Some("permission_change"), 50)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        recovered
            .operational_summary(999_000, 1_001_000)
            .unwrap()
            .read_failures,
        1
    );
    drop(recovered);

    let invalidated = prepare_restored_copy(&restored, &prepared, &key, 90).unwrap();
    assert_eq!(
        (
            invalidated.permissions,
            invalidated.tokens,
            invalidated.codes
        ),
        (2, 1, 1)
    );
    let quarantine = ChannelStore::open(&prepared, &key).unwrap();
    assert!(quarantine.recovery_quarantined().unwrap());
    assert!(quarantine.get_channel("alerts").is_err());
    assert!(
        quarantine
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .is_none()
    );
    let blocked = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &prepared)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .env_remove("DISCORD_WEBHOOK_URL")
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!blocked.status.success());
    drop(quarantine);
    let activation = timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .env("DISCORD_MCP_DB", &prepared)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .args(["recovery-activate", "--confirm"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(activation.status.success(), "synthetic activation failed");
    let quarantine = ChannelStore::open(&prepared, &key).unwrap();
    assert!(!quarantine.recovery_quarantined().unwrap());
    assert_eq!(
        quarantine
            .audit_events(None, Some("recovery_activated"), 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        !quarantine
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !quarantine
            .is_permitted("202", "alerts", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        quarantine
            .verify_access_token(&token, resource, 1001)
            .unwrap()
            .is_none()
    );
    let exchange = AuthorizationExchange {
        code: &code,
        client_id: "client",
        redirect_uri: "http://127.0.0.1:8765/callback",
        resource,
        code_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
    };
    assert!(
        quarantine
            .exchange_authorization_code(&exchange, 1001)
            .is_err()
    );
    assert_eq!(
        quarantine.get_webhook("alerts").unwrap(),
        WebhookUrl::parse(url).unwrap()
    );
    drop(quarantine);

    // Exercise the restored application without any valid outgoing message/read.
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &prepared)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .env_remove("DISCORD_WEBHOOK_URL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let init = request(&mut stdin, &mut stdout, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"synthetic-drill","version":"1"}}})).await;
    assert!(init.get("error").is_none());
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let read = request(&mut stdin, &mut stdout, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_messages","arguments":{"channel":"support","limit":0}}})).await;
    assert!(
        read["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("limit must be")
    );
    let send = request(&mut stdin, &mut stdout, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send_message","arguments":{"channel":"alerts","content":" "}}})).await;
    assert!(
        send["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("message must contain")
    );
    drop(stdin);
    assert!(
        timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );

    // Refuse a pre-existing destination and prove both it and the source survived.
    let protected = dir.path().join("do-not-overwrite.db");
    let sentinel = b"synthetic-protected-destination";
    std::fs::write(&protected, sentinel).unwrap();
    let rejected = replicate(example, &source, &dir.path().join("replica-2"), &protected).await;
    assert!(!rejected.status.success());
    assert_eq!(std::fs::read(&protected).unwrap(), sentinel);
    assert_eq!(store.list().unwrap(), baseline);
    assert!(
        store
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    let source_marker: String = writer
        .query_row("SELECT value FROM drill_marker WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(source_marker, "synthetic-marker");
}
