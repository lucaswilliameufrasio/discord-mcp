use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use rmcp::serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

#[tokio::test]
async fn remote_cli_uses_http_without_opening_local_storage() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("must-not-be-created.db");
    let challenge = Arc::new(Mutex::new(String::new()));
    let router = Router::new()
        .route(
            "/v1/cli/requests",
            post(
                |State(challenge): State<Arc<Mutex<String>>>, Json(body): Json<Value>| async move {
                    assert_eq!(body["user_id"], "101");
                    assert_eq!(body["channel"], "alerts");
                    assert_eq!(body["read"], true);
                    *challenge.lock().unwrap() = body["challenge"].as_str().unwrap().to_owned();
                    Json(json!({"request_id":"a".repeat(64)}))
                },
            ),
        )
        .route(
            "/v1/cli/execute",
            post(
                |State(challenge): State<Arc<Mutex<String>>>, Json(body): Json<Value>| async move {
                    let verifier = hex::decode(body["verifier"].as_str().unwrap()).unwrap();
                    assert_eq!(
                        hex::encode(Sha256::digest(verifier)),
                        *challenge.lock().unwrap()
                    );
                    assert_eq!(body["request_id"], "a".repeat(64));
                    StatusCode::NO_CONTENT
                },
            ),
        )
        .with_state(challenge);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", "invalid-key")
        .args(["remote-set-permission", &origin, "101", "alerts", "read"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"\n").await.unwrap();
    let result = timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!db.exists());
    server.abort();
}
