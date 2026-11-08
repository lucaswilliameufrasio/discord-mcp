//! Opt-in protocol check against a local synthetic Ciphera API, never production.
use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use rmcp::serde_json::{Value, json};

async fn bootstrap_secrets(
    State(mode): State<Arc<AtomicU8>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer synthetic-token")
    {
        return Err((StatusCode::UNAUTHORIZED, Json(json!({}))));
    }
    match mode.load(Ordering::SeqCst) {
        0 => Err((StatusCode::UNAUTHORIZED, Json(json!({})))),
        1 => Ok(Json(
            json!([{"key":"DISCORD_MCP_DISCORD_CLIENT_SECRET","value":"synthetic-oauth-secret","version":1}]),
        )),
        3 => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"message":"synthetic-provider-error-marker"})),
        )),
        _ => Ok(Json(json!([
            {"key":"DISCORD_MCP_KEY","value":"47".repeat(32),"version":1},
            {"key":"DISCORD_MCP_DISCORD_CLIENT_SECRET","value":"synthetic-oauth-secret","version":1}
        ]))),
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a separately provided Ciphera CLI executable"]
async fn ciphera_injected_key_recovers_without_overwriting_or_restoring_authority() {
    use discord_mcp::channels::{ChannelAction, ChannelStore};
    use std::{ffi::OsStr, os::unix::fs::PermissionsExt};

    let cli = std::env::var_os("CIPHERA_TEST_BINARY").expect("provide a test Ciphera binary");
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = dir.path().join("source.db");
    let destination = dir.path().join("recovered.db");
    let occupied = dir.path().join("occupied.db");
    let key = [0x47u8; 32];
    let resource = "https://example.invalid/mcp";
    let store = ChannelStore::open(&source, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-bot-token")
        .unwrap();
    store.set_permissions("101", "alerts", true, true).unwrap();
    let token = store.issue_access_token("101", resource, 1000, 60).unwrap();
    drop(store);
    let baseline = std::fs::read(&source).unwrap();
    std::fs::write(&occupied, "existing synthetic data").unwrap();
    let project = "00000000-0000-4000-8000-000000000001";
    let mode = Arc::new(AtomicU8::new(0));
    let router = Router::new()
        .route(
            &format!("/v1/projects/{project}/secrets/test"),
            get(bootstrap_secrets),
        )
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let run = |db: &std::path::Path, args: &[&OsStr]| {
        Command::new(&cli)
            .env_clear()
            .env("HOME", dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("CIPHERA_TOKEN", "synthetic-token")
            .env("DISCORD_MCP_DB", db)
            .args([
                "run",
                "--api-url",
                &format!("http://{address}"),
                "--project",
                project,
                "--env",
                "test",
                "--",
                env!("CARGO_BIN_EXE_discord-mcp"),
            ])
            .args(args)
            .output()
            .unwrap()
    };
    let prepare = |target: &std::path::Path| {
        run(
            &source,
            &[
                "recovery-prepare".as_ref(),
                source.as_os_str(),
                target.as_os_str(),
            ],
        )
    };
    for failure in [0, 1, 3] {
        mode.store(failure, Ordering::SeqCst);
        let output = prepare(&destination);
        assert!(!output.status.success());
        assert!(!destination.exists());
        assert_eq!(std::fs::read(&source).unwrap(), baseline);
    }
    mode.store(2, Ordering::SeqCst);
    assert!(!prepare(&occupied).status.success());
    assert_eq!(
        std::fs::read(&occupied).unwrap(),
        b"existing synthetic data"
    );
    assert!(prepare(&destination).status.success());
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    assert!(
        !run(&destination, &["list-channels".as_ref()])
            .status
            .success()
    );
    assert!(
        !run(&destination, &["recovery-activate".as_ref()])
            .status
            .success()
    );
    assert!(
        run(
            &destination,
            &["recovery-activate".as_ref(), "--confirm".as_ref()]
        )
        .status
        .success()
    );
    let store = ChannelStore::open(&destination, &key).unwrap();
    assert!(store.get_channel("alerts").is_ok());
    assert!(
        !store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert_eq!(
        store.verify_access_token(&token, resource, 1001).unwrap(),
        None
    );
    assert_eq!(std::fs::read(&source).unwrap(), baseline);
    task.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a separately provided Ciphera CLI executable"]
async fn dropin_shell_with_real_cli_preserves_existing_database_on_failures() {
    use discord_mcp::channels::ChannelStore;
    use std::{os::unix::fs::PermissionsExt, process::Stdio, time::Duration};

    let cli = std::env::var("CIPHERA_TEST_BINARY").expect("provide a test Ciphera binary");
    if let Some(bundle) = std::env::var_os("DISCORD_MCP_BUNDLE_PATH") {
        let bundle = std::path::Path::new(&bundle);
        assert_eq!(
            std::fs::read_to_string(bundle.join("ciphera.conf")).unwrap(),
            include_str!("../deploy/systemd/ciphera.conf")
        );
        assert_eq!(
            std::fs::read_to_string(bundle.join("discord-mcp.service")).unwrap(),
            include_str!("../deploy/systemd/discord-mcp.service")
        );
        assert_eq!(
            std::fs::read_to_string(bundle.join("service.env.example")).unwrap(),
            include_str!("../deploy/systemd/service.env.example")
        );
        assert_eq!(
            std::fs::read_to_string(bundle.join("RECOVERY.md")).unwrap(),
            include_str!("../deploy/systemd/RECOVERY.md")
        );
    }
    let proxy_ca = std::env::var_os("CIPHERA_TEST_PROXY_CA");
    let origin = if proxy_ca.is_some() {
        "https://example.test:8443"
    } else {
        "http://127.0.0.1:8000"
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = dir.path().join("existing.db");
    let key = [0x47u8; 32];
    let store = ChannelStore::open(&database, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-bot-token")
        .unwrap();
    drop(store);
    let baseline = std::fs::read(&database).unwrap();
    std::fs::write(dir.path().join("ciphera_token"), "synthetic-token").unwrap();
    let project = "00000000-0000-4000-8000-000000000001";
    let mode = Arc::new(AtomicU8::new(0));
    let router = Router::new()
        .route(
            &format!("/v1/projects/{project}/secrets/test"),
            get(bootstrap_secrets),
        )
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let line = include_str!("../deploy/systemd/ciphera.conf")
        .lines()
        .find(|line| line.starts_with("ExecStart=/bin/sh -c '"))
        .unwrap();
    let shell = line
        .strip_prefix("ExecStart=/bin/sh -c '")
        .unwrap()
        .strip_suffix('\'')
        .unwrap()
        .replace("$$", "$")
        .replace("/usr/local/bin/ciphera", &cli)
        .replace(
            "/usr/local/bin/discord-mcp",
            env!("CARGO_BIN_EXE_discord-mcp"),
        );
    let command = || {
        let mut cmd = Command::new("/bin/sh");
        cmd.env_clear()
            .args(["-c", &shell])
            .env("HOME", dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("CREDENTIALS_DIRECTORY", dir.path())
            .env("CIPHERA_API_URL", format!("http://{address}"))
            .env("CIPHERA_PROJECT", project)
            .env("CIPHERA_ENV", "test")
            .env("DISCORD_MCP_DB", &database)
            .env("DISCORD_MCP_LISTEN", "127.0.0.1:8000")
            .env("DISCORD_MCP_PUBLIC_ORIGIN", origin)
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
    for failure in [0, 1, 3] {
        mode.store(failure, Ordering::SeqCst);
        let output = command().output().unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-token"));
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("synthetic-provider-error-marker")
        );
        assert_eq!(std::fs::read(&database).unwrap(), baseline);
    }
    mode.store(2, Ordering::SeqCst);
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
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1));
    if let Some(path) = proxy_ca {
        let certificate = reqwest::Certificate::from_pem(&std::fs::read(path).unwrap()).unwrap();
        builder = builder
            .tls_certs_only([certificate])
            .resolve("example.test", "127.0.0.1:8443".parse().unwrap());
    }
    let client = builder.build().unwrap();
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
    assert!(ready, "synthetic bootstrap did not start the service");
    drop(guard);
    let store = ChannelStore::open(&database, &key).unwrap();
    assert!(store.get_channel("alerts").is_ok());
    task.abort();
}

async fn fake_secrets(
    State(allow): State<Arc<AtomicBool>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer synthetic-token")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !allow.load(Ordering::SeqCst) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(json!([
        {"key":"DISCORD_MCP_KEY","value":"47".repeat(32),"version":1},
        {"key":"DISCORD_MCP_DISCORD_CLIENT_SECRET","value":"synthetic-oauth-secret","version":1}
    ])))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a separately provided Ciphera CLI executable"]
async fn real_ciphera_cli_fails_closed_and_injects_synthetic_secrets() {
    let cli = std::env::var_os("CIPHERA_TEST_BINARY").expect("provide a test Ciphera binary");
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("no-database.db");
    let project = "00000000-0000-4000-8000-000000000001";
    let allow = Arc::new(AtomicBool::new(false));
    let router = Router::new()
        .route(
            &format!("/v1/projects/{project}/secrets/test"),
            get(fake_secrets),
        )
        .with_state(allow.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let run = |args: &[&str]| {
        Command::new(&cli)
            .env_clear()
            .env("HOME", dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("CIPHERA_TOKEN", "synthetic-token")
            .env("DISCORD_MCP_DB", &database)
            .env("DISCORD_MCP_LISTEN", "127.0.0.1:8000")
            .env("DISCORD_MCP_PUBLIC_ORIGIN", "http://127.0.0.1:8000")
            .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
            .env("DISCORD_MCP_ADMIN_ID", "1")
            .env("DISCORD_MCP_CLIENT_ID", "fixture")
            .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
            .env(
                "DISCORD_MCP_CLIENT_REDIRECT",
                "http://127.0.0.1:8765/callback",
            )
            .args([
                "run",
                "--api-url",
                &format!("http://{address}"),
                "--project",
                project,
                "--env",
                "test",
                "--",
                env!("CARGO_BIN_EXE_discord-mcp"),
            ])
            .args(args)
            .output()
            .unwrap()
    };
    let denied = run(&["setup", "local", "--init"]);
    assert!(!denied.status.success());
    assert!(!database.exists());
    assert!(!String::from_utf8_lossy(&denied.stderr).contains("synthetic-token"));
    allow.store(true, Ordering::SeqCst);
    let success = run(&["setup", "remote", "--check"]);
    assert!(success.status.success(), "preflight failed");
    let output = String::from_utf8_lossy(&success.stdout);
    assert!(output.contains("Server configuration validation passed"));
    assert!(!output.contains("synthetic-oauth-secret"));
    assert!(!output.contains(&"47".repeat(32)));
    assert!(!database.exists());
    task.abort();
}
