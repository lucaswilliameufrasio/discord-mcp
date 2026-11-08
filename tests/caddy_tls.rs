//! Opt-in fixture: Caddy must already run in the same isolated network namespace.
use std::{
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use discord_mcp::channels::ChannelStore;
use rmcp::serde_json::json;

#[tokio::test]
#[ignore = "requires isolated Caddy fixture with synthetic local CA"]
async fn caddy_https_preserves_host_and_enforces_mcp_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let generated_dir = dir.path().join("generated");
    std::fs::create_dir(&generated_dir).unwrap();
    let generation = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .args(["setup", "remote", "caddy", "example.test"])
        .arg(&generated_dir)
        .arg("--confirm")
        .output()
        .unwrap();
    assert!(generation.status.success());
    let generated = std::fs::read_to_string(generated_dir.join("discord-mcp.caddy")).unwrap();
    let config_path =
        std::env::var_os("DISCORD_MCP_CADDY_CONFIG").expect("fixture configuration path required");
    let fixture = std::fs::read_to_string(config_path).unwrap();
    assert_eq!(
        fixture,
        format!(
            "{}{generated}",
            include_str!("../drills/caddy/global-options.caddy")
        )
    );
    let database = dir.path().join("channels.db");
    let key = [42u8; 32];
    let store = ChannelStore::open(&database, &key).unwrap();
    let origin = "https://example.test:8443";
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let token = store
        .issue_access_token("1", &format!("{origin}/mcp"), now, 300)
        .unwrap();
    drop(store);
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .arg("serve")
        .env_clear()
        .env("DISCORD_MCP_DB", &database)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .env("DISCORD_MCP_LISTEN", "127.0.0.1:8000")
        .env("DISCORD_MCP_PUBLIC_ORIGIN", origin)
        .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
        .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "synthetic-secret")
        .env("DISCORD_MCP_ADMIN_ID", "1")
        .env("DISCORD_MCP_CLIENT_ID", "fixture")
        .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
        .env(
            "DISCORD_MCP_CLIENT_REDIRECT",
            "http://127.0.0.1:8765/callback",
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Ensure the child is reaped even if an assertion fails.
    struct Cleanup<'a>(&'a mut std::process::Child);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let guard = Cleanup(&mut child);
    let cert_path = std::env::var_os("DISCORD_MCP_CADDY_CA").expect("synthetic CA path required");
    let certificate = reqwest::Certificate::from_pem(&std::fs::read(cert_path).unwrap()).unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .tls_certs_only([certificate])
        .resolve("example.test", "127.0.0.1:8443".parse().unwrap())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let url = format!("{origin}/mcp?synthetic-log-marker=1");
    let request = || {
        client
            .post(&url)
            .header("MCP-Protocol-Version", "2025-11-25")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header("x-synthetic-secret", "synthetic-log-marker")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
    };
    let mut response = None;
    for _ in 0..50 {
        if let Ok(value) = request().send().await
            && value.status() != reqwest::StatusCode::BAD_GATEWAY
        {
            response = Some(value);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        response.expect("fixture not ready").status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let rejected = request()
        .bearer_auth("synthetic-log-marker")
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::UNAUTHORIZED);
    let authorized = request().bearer_auth(&token).send().await.unwrap();
    assert_eq!(authorized.status(), reqwest::StatusCode::OK);
    let body: rmcp::serde_json::Value = authorized.json().await.unwrap();
    assert!(body["result"]["tools"].is_array());
    let wrong_host = request()
        .header(reqwest::header::HOST, "other.example.test:8443")
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    // Caddy may return an empty HTTP 200 for an unmatched site; what matters
    // here is that it did not return the authenticated MCP tools response.
    let wrong_body = wrong_host.bytes().await.unwrap();
    let wrong_json = rmcp::serde_json::from_slice::<rmcp::serde_json::Value>(&wrong_body)
        .unwrap_or(rmcp::serde_json::Value::Null);
    assert!(!wrong_json["result"]["tools"].is_array());
    drop(guard);
}
