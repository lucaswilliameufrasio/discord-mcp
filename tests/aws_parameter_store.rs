#![cfg(feature = "aws-parameter-store")]

use aws_sdk_ssm::{
    Client,
    config::{Credentials, Region},
};
use axum::{
    Router,
    body::Bytes,
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use discord_mcp::aws_parameter_store::read_discord_oauth_secret;
use rmcp::serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn fixture(
    State(mode): State<Arc<AtomicUsize>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: Value = match rmcp::serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if body["Name"] != "/fixture/discord/oauth"
        || body["WithDecryption"] != true
        || !headers.contains_key("authorization")
        || headers.get("x-amz-target").and_then(|v| v.to_str().ok())
            != Some("AmazonSSM.GetParameter")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match mode.load(Ordering::SeqCst) {
        0 => (StatusCode::OK, Json(json!({"Parameter":{"Name":"/fixture/discord/oauth","Type":"SecureString","Value":"synthetic-oauth-secret"}}))).into_response(),
        1 => (StatusCode::BAD_REQUEST, Json(json!({"__type":"AccessDeniedException","message":"synthetic-provider-error-marker"}))).into_response(),
        2 => (StatusCode::OK, Json(json!({"Parameter":{"Name":"/fixture/discord/oauth","Type":"String","Value":"synthetic-oauth-secret"}}))).into_response(),
        3 => (StatusCode::OK, Json(json!({"Parameter":{"Name":"/fixture/discord/oauth","Type":"SecureString"}}))).into_response(),
        _ => (StatusCode::OK, Json(json!({"Parameter":{"Name":"/fixture/discord/oauth","Type":"SecureString","Value":"x".repeat(513)}}))).into_response(),
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parameter_migration_is_explicit_insert_only_and_preserves_source() {
    use discord_mcp::channels::{ChannelStore, EmbeddedSecret, StoreError};
    use std::{os::unix::fs::PermissionsExt, process::Command};
    async fn counted(
        State((mode, count)): State<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        count.fetch_add(1, Ordering::SeqCst);
        fixture(State(mode), headers, body).await
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = dir.path().join("existing.db");
    let recovery = dir.path().join("recovery.db");
    let key = [43u8; 32];
    let mode = Arc::new(AtomicUsize::new(1));
    let count = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/", post(counted))
        .with_state((mode.clone(), count.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let command = |path: &std::path::Path, k: &[u8; 32], action: &str| {
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .args([
                "setup",
                "local",
                "vault",
                "migrate-oauth-from-aws-parameter-store",
                action,
            ])
            .env("HOME", dir.path())
            .env("AWS_ACCESS_KEY_ID", "test")
            .env("AWS_SECRET_ACCESS_KEY", "test")
            .env("AWS_REGION", "us-east-1")
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env("AWS_ENDPOINT_URL_SSM", &endpoint)
            .env("DISCORD_MCP_DB", path)
            .env("DISCORD_MCP_KEY", hex::encode(k))
            .env("DISCORD_MCP_AWS_PARAMETER_NAME", "/fixture/discord/oauth")
            .output()
            .unwrap()
    };
    assert!(command(&db, &key, "--preview").status.success());
    assert!(!command(&db, &key, "--confirm").status.success());
    assert!(!db.exists());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-channel-token")
        .unwrap();
    drop(store);
    assert!(!command(&db, &[44; 32], "--confirm").status.success());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let denied = command(&db, &key, "--confirm");
    assert!(!denied.status.success());
    assert!(!String::from_utf8_lossy(&denied.stderr).contains("synthetic-provider-error-marker"));
    let store = ChannelStore::open(&db, &key).unwrap();
    assert!(matches!(
        store.get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret),
        Err(StoreError::NotFound)
    ));
    drop(store);
    discord_mcp::recovery::prepare_restored_copy(&db, &recovery, &key, 90).unwrap();
    let before = count.load(Ordering::SeqCst);
    assert!(!command(&recovery, &key, "--confirm").status.success());
    assert_eq!(count.load(Ordering::SeqCst), before);
    mode.store(0, Ordering::SeqCst);
    let output = command(&db, &key, "--confirm");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-oauth-secret"));
    let before = count.load(Ordering::SeqCst);
    assert!(!command(&db, &key, "--confirm").status.success());
    assert_eq!(count.load(Ordering::SeqCst), before);
    let store = ChannelStore::open(&db, &key).unwrap();
    assert!(store.get_channel("alerts").is_ok());
    assert_eq!(
        store
            .get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret)
            .unwrap(),
        "synthetic-oauth-secret"
    );
    let client = Client::from_conf(
        aws_sdk_ssm::Config::builder()
            .behavior_version_latest()
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new("test", "test", None, None, "synthetic"))
            .endpoint_url(&endpoint)
            .build(),
    );
    assert_eq!(
        read_discord_oauth_secret(&client, "/fixture/discord/oauth")
            .await
            .unwrap(),
        "synthetic-oauth-secret"
    );
    let race = dir.path().join("race.db");
    drop(ChannelStore::open(&race, &key).unwrap());
    let wins = std::thread::scope(|scope| {
        let a = scope.spawn(|| command(&race, &key, "--confirm").status.success());
        let b = scope.spawn(|| command(&race, &key, "--confirm").status.success());
        usize::from(a.join().unwrap()) + usize::from(b.join().unwrap())
    });
    assert_eq!(wins, 1);
    task.abort();
}

#[tokio::test]
async fn sdk_requires_decryption_secure_string_and_fails_closed() {
    let mode = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/", post(fixture))
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = aws_sdk_ssm::Config::builder()
        .behavior_version_latest()
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new("test", "test", None, None, "synthetic"))
        .endpoint_url(endpoint)
        .build();
    let client = Client::from_conf(config);
    assert_eq!(
        read_discord_oauth_secret(&client, "/fixture/discord/oauth")
            .await
            .unwrap(),
        "synthetic-oauth-secret"
    );
    for failure in [1, 2, 3, 4] {
        mode.store(failure, Ordering::SeqCst);
        let error = read_discord_oauth_secret(&client, "/fixture/discord/oauth")
            .await
            .unwrap_err();
        assert!(!error.contains("synthetic-provider-error-marker"));
        assert!(!error.contains("synthetic-oauth-secret"));
    }
    assert!(read_discord_oauth_secret(&client, "").await.is_err());
    task.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_parameter_provider_starts_server_without_fallback() {
    use discord_mcp::channels::ChannelStore;
    use std::{
        net::TcpListener,
        process::{Command, Stdio},
        time::Duration,
    };
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("existing.db");
    let key = [39; 32];
    drop(ChannelStore::open(&database, &key).unwrap());
    let mode = Arc::new(AtomicUsize::new(1));
    let app = Router::new()
        .route("/", post(fixture))
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    drop(server);
    let origin = format!("http://{address}");
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        cmd.env_clear()
            .arg("serve")
            .env("HOME", dir.path())
            .env("AWS_ACCESS_KEY_ID", "test")
            .env("AWS_SECRET_ACCESS_KEY", "test")
            .env("AWS_REGION", "us-east-1")
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env("AWS_ENDPOINT_URL_SSM", &endpoint)
            .env("DISCORD_MCP_DB", &database)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_MCP_SECRET_PROVIDER", "aws-parameter-store")
            .env("DISCORD_MCP_AWS_PARAMETER_NAME", "/fixture/discord/oauth")
            .env("DISCORD_MCP_LISTEN", address.to_string())
            .env("DISCORD_MCP_PUBLIC_ORIGIN", &origin)
            .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
            .env("DISCORD_MCP_ADMIN_ID", "1")
            .env("DISCORD_MCP_CLIENT_ID", "fixture")
            .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
            .env(
                "DISCORD_MCP_CLIENT_REDIRECT",
                "http://127.0.0.1:8765/callback",
            );
        cmd
    };
    let denied = command().output().unwrap();
    assert!(!denied.status.success());
    assert!(!String::from_utf8_lossy(&denied.stderr).contains("synthetic-provider-error-marker"));
    let conflict = command()
        .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "old-secret-marker")
        .output()
        .unwrap();
    assert!(!conflict.status.success());
    assert!(!String::from_utf8_lossy(&conflict.stderr).contains("old-secret-marker"));
    mode.store(0, Ordering::SeqCst);
    let mut child = command()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    struct Cleanup<'a>(&'a mut std::process::Child);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let guard = Cleanup(&mut child);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let mut ready = false;
    for _ in 0..50 {
        if let Ok(response) = client.get(format!("{origin}/admin")).send().await
            && response.status().is_success()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "server did not start with synthetic SSM endpoint");
    drop(guard);
    task.abort();
}
