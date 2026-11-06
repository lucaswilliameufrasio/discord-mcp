use std::{
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use rmcp::serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

// Compile the production router and Discord client with their existing
// cfg(test) provider injection; no provider override is enabled in production.
pub use discord_mcp::{channels, http_auth};
#[path = "../src/admin_web.rs"]
mod admin_web;
#[path = "../src/cli_authorization.rs"]
mod cli_authorization;
#[path = "../src/discord_login.rs"]
mod discord_login;

async fn browser_login(browser: &reqwest::Client, origin: &str) -> reqwest::Response {
    use axum::http::header;
    let login = browser
        .get(format!("{origin}/admin/login"))
        .send()
        .await
        .unwrap();
    let cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let location =
        reqwest::Url::parse(login.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    let state = location
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let mut callback = reqwest::Url::parse(&format!("{origin}/admin/discord/callback")).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &state)
        .append_pair("code", "synthetic-code")
        .append_pair("iss", "https://discord.com");
    browser
        .get(callback)
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn cli_real_router_discord_login_and_consent() {
    use axum::http::header;
    use discord_mcp::channels::{ChannelAction, ChannelStore};
    use discord_mcp::http_auth::AuthState;

    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("service.db");
    let forbidden_db = dir.path().join("cli.db");
    let key = [72_u8; 32];
    let store = ChannelStore::open(&database, &key).unwrap();
    store.put_bot("alerts", "123", "synthetic-token").unwrap();
    let identities = Arc::new(AtomicUsize::new(0));
    let provider = Router::new()
        .route(
            "/api/oauth2/token",
            post(|| async { Json(json!({"access_token":"synthetic","token_type":"Bearer"})) }),
        )
        .route(
            "/api/v10/users/@me",
            axum::routing::get(|State(count): State<Arc<AtomicUsize>>| async move {
                let id = if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    "999"
                } else {
                    "123"
                };
                Json(json!({"id":id,"username":"Synthetic"}))
            }),
        )
        .with_state(identities);
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_base = reqwest::Url::parse(&format!(
        "http://{}/",
        provider_listener.local_addr().unwrap()
    ))
    .unwrap();
    let provider_task =
        tokio::spawn(async move { axum::serve(provider_listener, provider).await.unwrap() });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let discord = discord_login::DiscordLoginClient::new(
        "123",
        "synthetic-secret",
        &format!("{origin}/admin/discord/callback"),
    )
    .unwrap()
    .with_test_base(provider_base);
    let config = admin_web::AdminConfig {
        auth: AuthState {
            database_path: database,
            encryption_key: key,
            resource: format!("{origin}/mcp"),
            metrics: Default::default(),
            audit: Default::default(),
        },
        public_origin: origin.clone(),
        admin_user_id: "123".into(),
        discord,
    };
    let cli_requests = Arc::new(cli_authorization::CliAuthorizations::default());
    let requests_for_server = cli_requests.clone();
    let service = tokio::spawn(async move {
        axum::serve(
            listener,
            admin_web::router_with_cli(config, requests_for_server).unwrap(),
        )
        .await
        .unwrap()
    });
    let mut cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &forbidden_db)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-set-permission", &origin, "101", "alerts", "read"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(cli.stderr.take().unwrap());
    let mut line = String::new();
    timeout(Duration::from_secs(10), stderr.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let consent = line.split_whitespace().last().unwrap().to_owned();
    let consent_url = reqwest::Url::parse(&consent).unwrap();
    let id = consent_url
        .query_pairs()
        .find(|(k, _)| k == "request_id")
        .unwrap()
        .1
        .into_owned();
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let guest = browser
        .get(&consent)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(guest.contains("Login Discord"));
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser_login(&browser, &origin).await.status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        !store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    let response = browser_login(&browser, &origin).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let session = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let consent_page = browser
        .get(&consent)
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(consent_page.contains("101") && consent_page.contains("alerts"));
    assert!(
        !store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    cli.stdin.take().unwrap().write_all(b"\n").await.unwrap();
    let status = timeout(Duration::from_secs(10), cli.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(
        store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .audit_events(None, Some("permission_change"), 10)
            .unwrap()
            .is_empty()
    );
    assert!(!forbidden_db.exists());
    // Exercise expiration after approval through the real HTTP handlers and
    // executable. Only the test registry exposes this deterministic time hook.
    let mut expired_cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &forbidden_db)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-set-permission", &origin, "102", "alerts", "read"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut expired_stderr = BufReader::new(expired_cli.stderr.take().unwrap());
    let mut expired_line = String::new();
    timeout(
        Duration::from_secs(10),
        expired_stderr.read_line(&mut expired_line),
    )
    .await
    .unwrap()
    .unwrap();
    let expired_url = reqwest::Url::parse(expired_line.split_whitespace().last().unwrap()).unwrap();
    let expired_id = expired_url
        .query_pairs()
        .find(|(k, _)| k == "request_id")
        .unwrap()
        .1
        .into_owned();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &expired_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    cli_requests.expire_for_test(&expired_id);
    expired_cli
        .stdin
        .take()
        .unwrap()
        .write_all(b"\n")
        .await
        .unwrap();
    assert!(
        !timeout(Duration::from_secs(10), expired_cli.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(
        !store
            .is_permitted("102", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert!(!forbidden_db.exists());
    service.abort();
    provider_task.abort();
}

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
