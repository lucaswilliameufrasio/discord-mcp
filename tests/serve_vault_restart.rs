#![cfg(target_os = "linux")]

use discord_mcp::channels::{ChannelStore, EmbeddedSecret};
use rmcp::serde_json::{Value, json};
use std::{
    io::Write,
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn real_serve_requires_shutdown_for_rotation_and_preserves_mcp_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let key = [94; 32];
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .insert_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret, "synthetic-old")
        .unwrap();
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let origin = format!("http://{address}");
    let resource = format!("{origin}/mcp");
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let token = store
        .issue_access_token("123", &resource, now, 300)
        .unwrap();
    drop(store);
    let binary = env!("CARGO_BIN_EXE_discord-mcp");
    let base = || {
        let mut cmd = Command::new(binary);
        cmd.env_clear()
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", hex::encode(key));
        cmd
    };
    let start = || {
        Server(
            base()
                .arg("serve")
                .env("DISCORD_MCP_SECRET_PROVIDER", "embedded")
                .env("DISCORD_MCP_LISTEN", address.to_string())
                .env("DISCORD_MCP_PUBLIC_ORIGIN", &origin)
                .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
                .env("DISCORD_MCP_ADMIN_ID", "123")
                .env("DISCORD_MCP_CLIENT_ID", "fixture")
                .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
                .env(
                    "DISCORD_MCP_CLIENT_REDIRECT",
                    "http://127.0.0.1:8765/callback",
                )
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let rotate = || {
        let mut child = base()
            .args(["setup", "local", "vault", "rotate-oauth", "--confirm"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child
            .stdin
            .take()
            .unwrap()
            .write_all(b"synthetic-old\nsynthetic-new\n");
        child.wait_with_output().unwrap()
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    async fn ready(client: &reqwest::Client, origin: &str) {
        for _ in 0..100 {
            if client.get(format!("{origin}/admin")).send().await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("server failed to start");
    }
    async fn token_works(client: &reqwest::Client, resource: &str, token: &str) -> bool {
        let response = client
            .post(resource)
            .bearer_auth(token)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .send()
            .await
            .unwrap();
        if !response.status().is_success() {
            return false;
        }
        let body: Value = response.json().await.unwrap();
        body.get("result").is_some()
    }
    let mut first = start();
    ready(&client, &origin).await;
    assert!(token_works(&client, &resource, &token).await);
    assert!(!rotate().status.success());
    assert!(
        Command::new("kill")
            .args(["-TERM", &first.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(first.0.wait().unwrap().success());
    drop(first);
    let output = rotate();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-new"));
    let second = start();
    ready(&client, &origin).await;
    assert!(token_works(&client, &resource, &token).await);
    let admin = client
        .get(format!("{origin}/admin/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(admin.status(), reqwest::StatusCode::SEE_OTHER);
    assert!(
        admin.headers()[reqwest::header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("https://discord.com/")
    );
    let store = ChannelStore::open(&db, &key).unwrap();
    assert_eq!(
        store
            .get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret)
            .unwrap(),
        "synthetic-new"
    );
    drop(second);
}
