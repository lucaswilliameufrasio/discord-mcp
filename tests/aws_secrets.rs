#![cfg(feature = "aws-secrets-manager")]

use aws_sdk_secretsmanager::{
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
use discord_mcp::aws_secrets::read_discord_oauth_secret;
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
    if body["SecretId"] != "fixture/oauth" || !headers.contains_key("authorization") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match mode.load(Ordering::SeqCst) {
        0 => (StatusCode::OK, Json(json!({"Name":"fixture/oauth","SecretString":"synthetic-oauth-secret"}))).into_response(),
        1 => (StatusCode::BAD_REQUEST, Json(json!({"__type":"AccessDeniedException","message":"synthetic-provider-error-marker"}))).into_response(),
        2 => (StatusCode::OK, Json(json!({"Name":"fixture/oauth","SecretBinary":"aGVsbG8="}))).into_response(),
        _ => (StatusCode::OK, Json(json!({"Name":"fixture/oauth","SecretString":"x".repeat(513)}))).into_response(),
    }
}

async fn migration_fixture(
    State((mode, requests)): State<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    requests.fetch_add(1, Ordering::SeqCst);
    fixture(State(mode), headers, body).await
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_aws_migration_never_overwrites_and_preserves_source() {
    use discord_mcp::channels::{ChannelStore, EmbeddedSecret, StoreError};
    use std::{os::unix::fs::PermissionsExt, process::Command};

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = dir.path().join("existing.db");
    let quarantined = dir.path().join("quarantined.db");
    let key = [43u8; 32];
    let mode = Arc::new(AtomicUsize::new(1));
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/", post(migration_fixture))
        .with_state((mode.clone(), requests.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let command = |path: &std::path::Path, key: &[u8; 32], action: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        cmd.env_clear()
            .args([
                "setup",
                "local",
                "vault",
                "migrate-oauth-from-aws-secrets-manager",
                action,
            ])
            .env("HOME", dir.path())
            .env("AWS_ACCESS_KEY_ID", "test")
            .env("AWS_SECRET_ACCESS_KEY", "test")
            .env("AWS_REGION", "us-east-1")
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env("AWS_ENDPOINT_URL_SECRETS_MANAGER", &endpoint)
            .env("DISCORD_MCP_DB", path)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_MCP_AWS_SECRET_ID", "fixture/oauth");
        cmd.output().unwrap()
    };
    assert!(command(&database, &key, "--preview").status.success());
    assert!(!command(&database, &key, "--confirm").status.success());
    assert!(!database.exists());
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    let store = ChannelStore::open(&database, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-channel-token")
        .unwrap();
    drop(store);
    assert!(!command(&database, &[44; 32], "--confirm").status.success());
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    let denied = command(&database, &key, "--confirm");
    assert!(!denied.status.success());
    assert!(!String::from_utf8_lossy(&denied.stderr).contains("synthetic-provider-error-marker"));
    let store = ChannelStore::open(&database, &key).unwrap();
    assert!(matches!(
        store.get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret),
        Err(StoreError::NotFound)
    ));
    drop(store);
    discord_mcp::recovery::prepare_restored_copy(&database, &quarantined, &key, 90).unwrap();
    let before = requests.load(Ordering::SeqCst);
    assert!(!command(&quarantined, &key, "--confirm").status.success());
    assert_eq!(requests.load(Ordering::SeqCst), before);
    mode.store(0, Ordering::SeqCst);
    let output = command(&database, &key, "--confirm");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-oauth-secret"));
    let count = requests.load(Ordering::SeqCst);
    assert!(!command(&database, &key, "--confirm").status.success());
    assert_eq!(requests.load(Ordering::SeqCst), count);
    let store = ChannelStore::open(&database, &key).unwrap();
    assert!(store.get_channel("alerts").is_ok());
    assert_eq!(
        store
            .get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret)
            .unwrap(),
        "synthetic-oauth-secret"
    );
    drop(store);
    let source = Client::from_conf(
        aws_sdk_secretsmanager::Config::builder()
            .behavior_version_latest()
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new("test", "test", None, None, "synthetic"))
            .endpoint_url(&endpoint)
            .build(),
    );
    assert_eq!(
        read_discord_oauth_secret(&source, "fixture/oauth")
            .await
            .unwrap(),
        "synthetic-oauth-secret"
    );
    let race = dir.path().join("race.db");
    drop(ChannelStore::open(&race, &key).unwrap());
    let successes = std::thread::scope(|scope| {
        let first = scope.spawn(|| command(&race, &key, "--confirm").status.success());
        let second = scope.spawn(|| command(&race, &key, "--confirm").status.success());
        usize::from(first.join().unwrap()) + usize::from(second.join().unwrap())
    });
    assert_eq!(successes, 1);
    task.abort();
}

#[tokio::test]
async fn real_aws_sdk_reads_exact_secret_and_rejects_failures() {
    let mode = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/", post(fixture))
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = aws_sdk_secretsmanager::Config::builder()
        .behavior_version_latest()
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new("test", "test", None, None, "synthetic"))
        .endpoint_url(endpoint)
        .build();
    let client = Client::from_conf(config);
    assert_eq!(
        read_discord_oauth_secret(&client, "fixture/oauth")
            .await
            .unwrap(),
        "synthetic-oauth-secret"
    );
    for failure in [1, 2, 3] {
        mode.store(failure, Ordering::SeqCst);
        let error = read_discord_oauth_secret(&client, "fixture/oauth")
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
async fn selected_aws_provider_starts_server_and_refuses_stale_secret() {
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
    let addr = server.local_addr().unwrap();
    drop(server);
    let origin = format!("http://{addr}");
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        command
            .env_clear()
            .arg("serve")
            .env("HOME", dir.path())
            .env("AWS_ACCESS_KEY_ID", "test")
            .env("AWS_SECRET_ACCESS_KEY", "test")
            .env("AWS_REGION", "us-east-1")
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env("AWS_ENDPOINT_URL_SECRETS_MANAGER", &endpoint)
            .env("DISCORD_MCP_DB", &database)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_MCP_SECRET_PROVIDER", "aws-secrets-manager")
            .env("DISCORD_MCP_AWS_SECRET_ID", "fixture/oauth")
            .env("DISCORD_MCP_LISTEN", addr.to_string())
            .env("DISCORD_MCP_PUBLIC_ORIGIN", &origin)
            .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
            .env("DISCORD_MCP_ADMIN_ID", "1")
            .env("DISCORD_MCP_CLIENT_ID", "fixture")
            .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
            .env(
                "DISCORD_MCP_CLIENT_REDIRECT",
                "http://127.0.0.1:8765/callback",
            );
        command
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
    assert!(ready, "server did not start with synthetic AWS endpoint");
    drop(guard);
    task.abort();
}
