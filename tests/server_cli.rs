#![cfg(unix)]

use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use discord_mcp::channels::ChannelStore;
use rmcp::serde_json::{Value, json};

#[tokio::test]
async fn cli_starts_loopback_server_enforces_permissions_and_drains_on_term() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("server.db");
    let key = [61; 32];
    let encoded_key = hex::encode(key);
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-bot-token")
        .unwrap();
    let base = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        command
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", &encoded_key);
        command
    };
    let result = base()
        .args(["set-permission", "123", "alerts", "read"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let result = base()
        .args(["show-permissions", "alerts"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap().trim(),
        "123\tread=true\tsend=false"
    );
    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    let origin = format!("http://{addr}");
    let resource = format!("{origin}/mcp");
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let token = store.issue_access_token("123", &resource, now, 60).unwrap();
    let mut child = base()
        .arg("serve")
        .env("DISCORD_MCP_LISTEN", addr.to_string())
        .env("DISCORD_MCP_PUBLIC_ORIGIN", &origin)
        .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
        .env("DISCORD_MCP_ADMIN_ID", "123")
        .env(
            "DISCORD_MCP_DISCORD_CLIENT_SECRET",
            "synthetic-client-secret",
        )
        .env("DISCORD_MCP_CLIENT_ID", "test-client")
        .env("DISCORD_MCP_CLIENT_NAME", "Test client")
        .env(
            "DISCORD_MCP_CLIENT_REDIRECT",
            "http://127.0.0.1:8765/callback",
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    assert!(line.contains("loopback only"));
    assert!(!line.contains("synthetic-client-secret"));
    let client = reqwest::Client::new();
    let admin_page = client.get(format!("{origin}/admin")).send().await.unwrap();
    assert!(
        admin_page
            .text()
            .await
            .unwrap()
            .contains("Entrar com Discord")
    );
    let metadata: Value = client
        .get(format!("{origin}/.well-known/oauth-protected-resource"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["resource"], resource);
    let response: Value = client.post(&resource).bearer_auth(&token)
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_messages","arguments":{"channel":"alerts","limit":0}}}))
        .send().await.unwrap().json().await.unwrap();
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("limit must be")
    );
    let revoked = base()
        .args(["set-permission", "123", "alerts", "none"])
        .output()
        .unwrap();
    assert!(revoked.status.success());
    let response: Value = client.post(&resource).bearer_auth(&token)
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_messages","arguments":{"channel":"alerts","limit":0}}}))
        .send().await.unwrap().json().await.unwrap();
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("access denied")
    );
    let kill = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    assert!(child.wait().unwrap().success());
}

#[test]
fn cli_refuses_non_loopback_listener() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env("DISCORD_MCP_DB", dir.path().join("server.db"))
        .env("DISCORD_MCP_KEY", hex::encode([62; 32]))
        .env("DISCORD_MCP_LISTEN", "0.0.0.0:8000")
        .arg("serve")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("loopback address"));
}
