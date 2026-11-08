use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use discord_mcp::infisical::discord_oauth_secret;
use rmcp::serde_json::json;

async fn fixture(
    State(mode): State<Arc<AtomicUsize>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer synthetic-token")
        || query.get("projectId").map(String::as_str) != Some("fixture-project")
        || query.get("environment").map(String::as_str) != Some("test")
        || query.get("secretPath").map(String::as_str) != Some("/discord")
        || query.get("includeImports").map(String::as_str) != Some("false")
        || query.get("expandSecretReferences").map(String::as_str) != Some("false")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match mode.load(Ordering::SeqCst) {
        0 => (StatusCode::OK, axum::Json(json!({"secret":{"secretKey":"DISCORD_MCP_DISCORD_CLIENT_SECRET","secretValue":"synthetic-oauth-secret","secretPath":"/discord","environment":"test","secretValueHidden":false}}))).into_response(),
        1 => (StatusCode::FORBIDDEN, "synthetic-provider-error-marker").into_response(),
        2 => (StatusCode::OK, "x".repeat(65537)).into_response(),
        3 => (StatusCode::OK, axum::Json(json!({"secret":{"secretKey":"DISCORD_MCP_DISCORD_CLIENT_SECRET","secretValue":"synthetic-oauth-secret","secretPath":"/other","environment":"test","secretValueHidden":false}}))).into_response(),
        _ => (StatusCode::FOUND, [("location", "http://127.0.0.1:9/")]).into_response(),
    }
}

async fn migration_fixture(
    State((mode, requests)): State<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
    query: Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    requests.fetch_add(1, Ordering::SeqCst);
    fixture(State(mode), query, headers).await
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_infisical_migration_preserves_source_and_never_overwrites() {
    use discord_mcp::channels::{ChannelStore, EmbeddedSecret, StoreError};
    use std::{os::unix::fs::PermissionsExt, process::Command};

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = dir.path().join("existing.db");
    let recovered = dir.path().join("quarantined.db");
    let key = [43u8; 32];
    let mode = Arc::new(AtomicUsize::new(1));
    let requests = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route(
            "/api/v4/secrets/DISCORD_MCP_DISCORD_CLIENT_SECRET",
            get(migration_fixture),
        )
        .with_state((mode.clone(), requests.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let command = |path: &std::path::Path, key: &[u8; 32], action: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        cmd.env_clear()
            .args([
                "setup",
                "local",
                "vault",
                "migrate-oauth-from-infisical",
                action,
            ])
            .env("DISCORD_MCP_DB", path)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_MCP_INFISICAL_ORIGIN", &origin)
            .env("DISCORD_MCP_INFISICAL_PROJECT_ID", "fixture-project")
            .env("DISCORD_MCP_INFISICAL_ENVIRONMENT", "test")
            .env("DISCORD_MCP_INFISICAL_SECRET_PATH", "/discord")
            .env("DISCORD_MCP_INFISICAL_TOKEN", "synthetic-token");
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
    discord_mcp::recovery::prepare_restored_copy(&database, &recovered, &key, 90).unwrap();
    let count = requests.load(Ordering::SeqCst);
    assert!(!command(&recovered, &key, "--confirm").status.success());
    assert_eq!(requests.load(Ordering::SeqCst), count);
    mode.store(0, Ordering::SeqCst);
    let output = command(&database, &key, "--confirm");
    assert!(output.status.success());
    for text in [
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    ] {
        assert!(!text.contains("synthetic-oauth-secret"));
        assert!(!text.contains("synthetic-token"));
    }
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
    assert_eq!(
        discord_oauth_secret(
            &origin,
            "fixture-project",
            "test",
            "/discord",
            "synthetic-token"
        )
        .await
        .unwrap(),
        "synthetic-oauth-secret"
    );
    drop(store);
    let race_db = dir.path().join("race.db");
    drop(ChannelStore::open(&race_db, &key).unwrap());
    let successes = std::thread::scope(|scope| {
        let first = scope.spawn(|| command(&race_db, &key, "--confirm").status.success());
        let second = scope.spawn(|| command(&race_db, &key, "--confirm").status.success());
        usize::from(first.join().unwrap()) + usize::from(second.join().unwrap())
    });
    assert_eq!(successes, 1);
    let store = ChannelStore::open(&race_db, &key).unwrap();
    assert_eq!(
        store
            .get_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret)
            .unwrap(),
        "synthetic-oauth-secret"
    );
    task.abort();
}

#[tokio::test]
async fn infisical_v4_protocol_is_scoped_bounded_and_fails_closed() {
    let state = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route(
            "/api/v4/secrets/DISCORD_MCP_DISCORD_CLIENT_SECRET",
            get(fixture),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let fetch = || {
        discord_oauth_secret(
            &origin,
            "fixture-project",
            "test",
            "/discord",
            "synthetic-token",
        )
    };
    assert_eq!(fetch().await.unwrap(), "synthetic-oauth-secret");
    for failure in [1, 2, 3, 4] {
        state.store(failure, Ordering::SeqCst);
        let error = fetch().await.unwrap_err();
        assert!(!error.contains("synthetic-provider-error-marker"));
        assert!(!error.contains("synthetic-token"));
    }
    assert!(
        discord_oauth_secret("http://example.test", "p", "test", "/", "synthetic-token")
            .await
            .is_err()
    );
    assert!(
        discord_oauth_secret(&origin, "p", "test", "not-absolute", "synthetic-token")
            .await
            .is_err()
    );
    task.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_provider_starts_real_server_and_never_falls_back() {
    use discord_mcp::channels::ChannelStore;
    use std::{
        net::TcpListener,
        process::{Command, Stdio},
        time::Duration,
    };

    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("channels.db");
    let key = [45u8; 32];
    drop(ChannelStore::open(&database, &key).unwrap());
    let mode = Arc::new(AtomicUsize::new(1));
    let router = Router::new()
        .route(
            "/api/v4/secrets/DISCORD_MCP_DISCORD_CLIENT_SECRET",
            get(fixture),
        )
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    drop(server);
    let origin = format!("http://{address}");
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        cmd.env_clear()
            .arg("serve")
            .env("DISCORD_MCP_DB", &database)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_MCP_SECRET_PROVIDER", "infisical")
            .env("DISCORD_MCP_INFISICAL_ORIGIN", &provider_origin)
            .env("DISCORD_MCP_INFISICAL_PROJECT_ID", "fixture-project")
            .env("DISCORD_MCP_INFISICAL_ENVIRONMENT", "test")
            .env("DISCORD_MCP_INFISICAL_SECRET_PATH", "/discord")
            .env("DISCORD_MCP_INFISICAL_TOKEN", "synthetic-token")
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
    assert!(ready, "server did not start with synthetic provider");
    drop(guard);
    task.abort();
}
