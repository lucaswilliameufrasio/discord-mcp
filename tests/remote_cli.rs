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
#[cfg(feature = "encrypted-s3-backup")]
pub use discord_mcp::encrypted_backup;
pub use discord_mcp::{channels, http_auth, webhook};
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
async fn invalid_revision_blocks_admin_form_and_approved_real_cli() {
    use axum::http::header;
    use discord_mcp::channels::{ChannelAction, ChannelStore};
    use discord_mcp::http_auth::AuthState;

    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("service.db");
    let cli_database = dir.path().join("cli.db");
    let key = [83_u8; 32];
    let initial = ChannelStore::open(&database, &key).unwrap();
    initial.put_bot("alerts", "123", "synthetic-token").unwrap();
    initial
        .set_permissions("101", "alerts", true, false)
        .unwrap();
    drop(initial);
    let writer = rusqlite::Connection::open(&database).unwrap();
    writer.execute_batch("DROP TABLE channel_permission_revisions; CREATE TABLE channel_permission_revisions (channel_name TEXT PRIMARY KEY NOT NULL REFERENCES channels(name) ON DELETE CASCADE, revision INTEGER NOT NULL); INSERT INTO channel_permission_revisions VALUES ('alerts', 3);").unwrap();
    let store = ChannelStore::open(&database, &key).unwrap();
    let provider = Router::new()
        .route(
            "/api/oauth2/token",
            post(|| async { Json(json!({"access_token":"synthetic","token_type":"Bearer"})) }),
        )
        .route(
            "/api/v10/users/@me",
            axum::routing::get(|| async { Json(json!({"id":"123","username":"Admin"})) }),
        );
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = reqwest::Url::parse(&format!(
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
    .with_test_base(provider_url.clone());
    let config = admin_web::AdminConfig {
        auth: AuthState {
            database_path: database.clone(),
            encryption_key: key,
            resource: format!("{origin}/mcp"),
            metrics: Default::default(),
            audit: Default::default(),
        },
        public_origin: origin.clone(),
        admin_user_id: "123".into(),
        discord,
        #[cfg(feature = "encrypted-s3-backup")]
        backup: None,
    };
    let service = tokio::spawn(async move {
        axum::serve(listener, admin_web::router(config).unwrap())
            .await
            .unwrap()
    });
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let login = browser_login(&browser, &origin).await;
    assert_eq!(login.status(), StatusCode::SEE_OTHER);
    let session = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let page = browser
        .get(format!("{origin}/admin/channel?name=alerts"))
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let html = page.text().await.unwrap();
    let extract = |name: &str| {
        html.split(&format!("name=\"{name}\" value=\""))
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_owned()
    };
    let version = extract("version");
    let revision = extract("revision");

    let mut cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &cli_database)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-set-permission", &origin, "101", "alerts", "send"])
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
    let consent = reqwest::Url::parse(line.split_whitespace().last().unwrap()).unwrap();
    let id = consent
        .query_pairs()
        .find(|(key, _)| key == "request_id")
        .unwrap()
        .1
        .into_owned();
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
    let audit_before: i64 = writer.query_row("SELECT count(*) FROM audit_events WHERE event_type='permission_change' AND outcome='success'", [], |row| row.get(0)).unwrap();
    writer
        .execute(
            "UPDATE channel_permission_revisions SET revision=-1 WHERE channel_name='alerts'",
            [],
        )
        .unwrap();
    assert_eq!(
        browser
            .get(format!("{origin}/admin/channel?name=alerts"))
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let form = [
        ("channel", "alerts"),
        ("user_id", "101"),
        ("permission", "send"),
        ("version", version.as_str()),
        ("revision", revision.as_str()),
    ];
    assert_eq!(
        browser
            .post(format!("{origin}/admin/permissions"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&form)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    cli.stdin.take().unwrap().write_all(b"\n").await.unwrap();
    assert!(
        !timeout(Duration::from_secs(10), cli.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!cli_database.exists());
    assert!(
        store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("101", "alerts", ChannelAction::Send)
            .unwrap()
    );
    let audit_after: i64 = writer.query_row("SELECT count(*) FROM audit_events WHERE event_type='permission_change' AND outcome='success'", [], |row| row.get(0)).unwrap();
    assert_eq!(audit_after, audit_before);
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
    service.abort();
    provider_task.abort();
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
            database_path: database.clone(),
            encryption_key: key,
            resource: format!("{origin}/mcp"),
            metrics: Default::default(),
            audit: Default::default(),
        },
        public_origin: origin.clone(),
        admin_user_id: "123".into(),
        discord,
        #[cfg(feature = "encrypted-s3-backup")]
        backup: None,
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
    let mut list_cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &forbidden_db)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-list-channels", &origin])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut list_stderr = BufReader::new(list_cli.stderr.take().unwrap());
    let mut list_line = String::new();
    timeout(
        Duration::from_secs(10),
        list_stderr.read_line(&mut list_line),
    )
    .await
    .unwrap()
    .unwrap();
    let list_url = reqwest::Url::parse(list_line.split_whitespace().last().unwrap()).unwrap();
    let list_id = list_url
        .query_pairs()
        .find(|(k, _)| k == "request_id")
        .unwrap()
        .1
        .into_owned();
    let list_page = browser
        .get(list_url)
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(list_page.contains("Consultar nomes dos canais cadastrados"));
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &list_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    list_cli
        .stdin
        .take()
        .unwrap()
        .write_all(b"\n")
        .await
        .unwrap();
    let list_result = timeout(Duration::from_secs(10), list_cli.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(list_result.status.success());
    assert_eq!(String::from_utf8(list_result.stdout).unwrap(), "alerts\n");
    let verifier = [73_u8; 32];
    let request: Value = browser
        .post(format!("{origin}/v1/cli/list-requests"))
        .json(&json!({"challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_id = request["request_id"].as_str().unwrap();
    let execute = |proof: String| {
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":request_id,"verifier":proof}))
    };
    assert_eq!(
        execute(hex::encode(verifier))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", request_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        execute(hex::encode([74_u8; 32]))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let list_response = execute(hex::encode(verifier)).send().await.unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    assert_eq!(list_response.headers()[header::CACHE_CONTROL], "no-store");
    let names: Value = list_response.json().await.unwrap();
    assert_eq!(names["channels"], json!(["alerts"]));
    assert_eq!(
        execute(hex::encode(verifier))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &list_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let mut permission_cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &forbidden_db)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-show-permissions", &origin, "alerts"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut permission_stderr = BufReader::new(permission_cli.stderr.take().unwrap());
    let mut permission_line = String::new();
    timeout(
        Duration::from_secs(10),
        permission_stderr.read_line(&mut permission_line),
    )
    .await
    .unwrap()
    .unwrap();
    let permission_url =
        reqwest::Url::parse(permission_line.split_whitespace().last().unwrap()).unwrap();
    let permission_id = permission_url
        .query_pairs()
        .find(|(k, _)| k == "request_id")
        .unwrap()
        .1
        .into_owned();
    let html = browser
        .get(permission_url)
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("Consultar permissões do canal: alerts"));
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &permission_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    permission_cli
        .stdin
        .take()
        .unwrap()
        .write_all(b"\n")
        .await
        .unwrap();
    let output = timeout(Duration::from_secs(10), permission_cli.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "101\tread=true\tsend=false\n"
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &permission_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let missing: Value = browser
        .post(format!("{origin}/v1/cli/permission-requests"))
        .json(&json!({"channel":"missing","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let missing_id = missing["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", missing_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":missing_id,"verifier":hex::encode(verifier)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let synthetic_webhook = "https://discord.com/api/webhooks/888/synthetic-secret";
    let mut register = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &forbidden_db)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-add-webhook", &origin, "new_channel"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    register
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{synthetic_webhook}\n").as_bytes())
        .await
        .unwrap();
    let mut register_stderr = BufReader::new(register.stderr.take().unwrap());
    let mut register_line = String::new();
    timeout(
        Duration::from_secs(10),
        register_stderr.read_line(&mut register_line),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!register_line.contains("synthetic-secret"));
    let register_url =
        reqwest::Url::parse(register_line.split_whitespace().last().unwrap()).unwrap();
    let register_id = register_url
        .query_pairs()
        .find(|(k, _)| k == "request_id")
        .unwrap()
        .1
        .into_owned();
    let html = browser
        .get(register_url)
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("new_channel") && html.contains("888") && html.contains("revoga"));
    assert!(!html.contains("synthetic-secret"));
    assert!(!store.list().unwrap().contains(&"new_channel".to_owned()));
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &register_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    register
        .stdin
        .take()
        .unwrap()
        .write_all(b"\n")
        .await
        .unwrap();
    let output = timeout(Duration::from_secs(10), register.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-secret"));
    assert!(
        store.get_webhook("new_channel").unwrap()
            == webhook::WebhookUrl::parse(synthetic_webhook).unwrap()
    );
    let audit = store
        .audit_events(None, Some("channel_configured"), 10)
        .unwrap();
    assert!(audit.iter().any(|entry| entry.actor_kind == "discord"
        && entry.actor_id.as_deref() == Some("123")
        && entry.channel.as_deref() == Some("new_channel")));
    let db_bytes = std::fs::read(&database).unwrap();
    assert!(
        !db_bytes
            .windows("synthetic-secret".len())
            .any(|part| part == b"synthetic-secret")
    );
    let ciphertext: Vec<u8> = rusqlite::Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT ciphertext FROM channels WHERE name='new_channel'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !ciphertext
            .windows("synthetic-secret".len())
            .any(|part| part == b"synthetic-secret")
    );
    assert!(!forbidden_db.exists());
    let synthetic_token = "synthetic-bot-credential";
    let mut bot_cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &forbidden_db)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-add-bot", &origin, "bot_new", "321"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    bot_cli
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{synthetic_token}\n").as_bytes())
        .await
        .unwrap();
    let mut bot_stderr = BufReader::new(bot_cli.stderr.take().unwrap());
    let mut bot_line = String::new();
    timeout(Duration::from_secs(10), bot_stderr.read_line(&mut bot_line))
        .await
        .unwrap()
        .unwrap();
    assert!(!bot_line.contains(synthetic_token));
    let bot_url = reqwest::Url::parse(bot_line.split_whitespace().last().unwrap()).unwrap();
    let bot_id = bot_url
        .query_pairs()
        .find(|(k, _)| k == "request_id")
        .unwrap()
        .1
        .into_owned();
    let html = browser
        .get(bot_url)
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        html.contains("Cadastrar bot")
            && html.contains("bot_new")
            && html.contains("321")
            && html.contains("revoga")
    );
    assert!(!html.contains(synthetic_token));
    assert!(!store.list().unwrap().contains(&"bot_new".to_owned()));
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &bot_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    bot_cli
        .stdin
        .take()
        .unwrap()
        .write_all(b"\n")
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(10), bot_cli.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(result.status.success());
    assert!(!String::from_utf8_lossy(&result.stdout).contains(synthetic_token));
    assert!(matches!(
        store.get_channel("bot_new"),
        Ok(discord_mcp::channels::Channel::Bot(_))
    ));
    store.set_permissions("103", "bot_new", true, true).unwrap();
    assert!(
        store
            .is_permitted("103", "bot_new", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        store
            .is_permitted("103", "bot_new", ChannelAction::Send)
            .unwrap()
    );
    assert!(
        store
            .audit_events(None, Some("channel_configured"), 20)
            .unwrap()
            .iter()
            .any(|entry| entry.actor_kind == "discord"
                && entry.actor_id.as_deref() == Some("123")
                && entry.channel.as_deref() == Some("bot_new"))
    );
    let encrypted: Vec<u8> = rusqlite::Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT ciphertext FROM channels WHERE name='bot_new'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !encrypted
            .windows(synthetic_token.len())
            .any(|part| part == synthetic_token.as_bytes())
    );
    let request: Value = browser.post(format!("{origin}/v1/cli/bot-requests"))
        .json(&json!({"channel":"bot_second","discord_channel_id":"654","commitment":hex::encode(Sha256::digest(synthetic_token.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let request_id = request["request_id"].as_str().unwrap();
    let payload = json!({"request_id":request_id,"verifier":hex::encode(verifier),"bot_token":synthetic_token});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", request_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(browser.post(format!("{origin}/v1/cli/execute")).json(&json!({"request_id":request_id,"verifier":hex::encode(verifier),"bot_token":"different-synthetic"})).send().await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(!store.list().unwrap().contains(&"bot_second".to_owned()));
    let expired: Value = browser.post(format!("{origin}/v1/cli/bot-requests"))
        .json(&json!({"channel":"bot_second","discord_channel_id":"654","commitment":hex::encode(Sha256::digest(synthetic_token.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let expired_id = expired["request_id"].as_str().unwrap();
    cli_requests.expire_for_test(expired_id);
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", expired_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let request: Value = browser.post(format!("{origin}/v1/cli/bot-requests"))
        .json(&json!({"channel":"bot_new","discord_channel_id":"654","commitment":hex::encode(Sha256::digest(synthetic_token.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let id = request["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let payload =
        json!({"request_id":id,"verifier":hex::encode(verifier),"bot_token":synthetic_token});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        !store
            .is_permitted("103", "bot_new", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        store
            .audit_events(None, Some("channel_grants_revoked"), 20)
            .unwrap()
            .iter()
            .any(|entry| entry.actor_kind == "discord"
                && entry.actor_id.as_deref() == Some("123")
                && entry.channel.as_deref() == Some("bot_new"))
    );
    let request: Value = browser
        .post(format!("{origin}/v1/cli/permission-requests"))
        .json(&json!({"channel":"alerts","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_id = request["request_id"].as_str().unwrap();
    let execute_permissions = |proof: String| {
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":request_id,"verifier":proof}))
    };
    assert_eq!(
        execute_permissions(hex::encode(verifier))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    store
        .put_bot("replace_me", "123", "synthetic-token")
        .unwrap();
    store
        .set_permissions("102", "replace_me", true, false)
        .unwrap();
    let request: Value = browser.post(format!("{origin}/v1/cli/webhook-requests"))
        .json(&json!({"channel":"replace_me","webhook_id":"888","commitment":hex::encode(Sha256::digest(synthetic_webhook.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let replacement_id = request["request_id"].as_str().unwrap();
    let replacement_payload = json!({"request_id":replacement_id,"verifier":hex::encode(verifier),"webhook":synthetic_webhook});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&replacement_payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", replacement_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(browser.post(format!("{origin}/v1/cli/execute")).json(&json!({"request_id":replacement_id,"verifier":hex::encode(verifier),"webhook":"https://discord.com/api/webhooks/888/different-synthetic"})).send().await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&replacement_payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        store
            .is_permitted("102", "replace_me", ChannelAction::Read)
            .unwrap()
    );
    let request: Value = browser.post(format!("{origin}/v1/cli/webhook-requests"))
        .json(&json!({"channel":"replace_me","webhook_id":"888","commitment":hex::encode(Sha256::digest(synthetic_webhook.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let replacement_id = request["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", replacement_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let payload = json!({"request_id":replacement_id,"verifier":hex::encode(verifier),"webhook":synthetic_webhook});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        !store
            .is_permitted("102", "replace_me", ChannelAction::Read)
            .unwrap()
    );
    let expired: Value = browser
        .post(format!("{origin}/v1/cli/permission-requests"))
        .json(&json!({"channel":"alerts","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let expired_permission_id = expired["request_id"].as_str().unwrap();
    cli_requests.expire_for_test(expired_permission_id);
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", expired_permission_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", request_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        execute_permissions(hex::encode([74_u8; 32]))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = execute_permissions(hex::encode(verifier))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body["permissions"],
        json!([{"user_id":"101","can_read":true,"can_send":false}])
    );
    store
        .put_bot("delete_me", "444", "synthetic-token")
        .unwrap();
    store
        .set_permissions("104", "delete_me", true, false)
        .unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/delete-requests"))
            .json(
                &json!({"channel":"../invalid","challenge":hex::encode(Sha256::digest(verifier))})
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut delete_cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &forbidden_db)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-delete-channel", &origin, "delete_me"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut delete_stderr = BufReader::new(delete_cli.stderr.take().unwrap());
    let mut delete_line = String::new();
    timeout(
        Duration::from_secs(10),
        delete_stderr.read_line(&mut delete_line),
    )
    .await
    .unwrap()
    .unwrap();
    let delete_url = reqwest::Url::parse(delete_line.split_whitespace().last().unwrap()).unwrap();
    let delete_id = delete_url
        .query_pairs()
        .find(|(k, _)| k == "request_id")
        .unwrap()
        .1
        .into_owned();
    let html = browser
        .get(delete_url)
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        html.contains("Excluir canal: delete_me") && html.contains("não exclui o canal no Discord")
    );
    assert!(
        store
            .is_permitted("104", "delete_me", ChannelAction::Read)
            .unwrap()
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &delete_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    delete_cli
        .stdin
        .take()
        .unwrap()
        .write_all(b"\n")
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_secs(10), delete_cli.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!store.list().unwrap().contains(&"delete_me".to_owned()));
    let audit = store
        .audit_events(None, Some("channel_deleted"), 20)
        .unwrap();
    assert!(audit.iter().any(|entry| entry.actor_kind == "discord"
        && entry.actor_id.as_deref() == Some("123")
        && entry.channel.as_deref() == Some("delete_me")));
    let audit = store
        .audit_events(None, Some("channel_grants_revoked"), 20)
        .unwrap();
    assert!(audit.iter().any(
        |entry| entry.actor_kind == "discord" && entry.channel.as_deref() == Some("delete_me")
    ));
    store
        .put_bot("delete_second", "444", "synthetic-token")
        .unwrap();
    let second: Value = browser
        .post(format!("{origin}/v1/cli/delete-requests"))
        .json(&json!({"channel":"delete_second","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_id = second["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", second_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":second_id,"verifier":hex::encode([74_u8; 32])}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let second_payload = json!({"request_id":second_id,"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&second_payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&second_payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let missing: Value = browser
        .post(format!("{origin}/v1/cli/delete-requests"))
        .json(&json!({"channel":"missing","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let missing_id = missing["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", missing_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":missing_id,"verifier":hex::encode(verifier)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let expired: Value = browser
        .post(format!("{origin}/v1/cli/delete-requests"))
        .json(&json!({"channel":"alerts","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let expired_id = expired["request_id"].as_str().unwrap();
    cli_requests.expire_for_test(expired_id);
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", expired_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let request: Value = browser
        .post(format!("{origin}/v1/cli/delete-requests"))
        .json(&json!({"channel":"alerts","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let changed_id = request["request_id"].as_str().unwrap();
    let payload = json!({"request_id":changed_id,"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", changed_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    store
        .put_bot("alerts", "555", "replacement-synthetic")
        .unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(store.list().unwrap().contains(&"alerts".to_owned()));
    assert!(!forbidden_db.exists());
    let mut simultaneous = Vec::new();
    for (read, send) in [(true, false), (false, true)] {
        let request: Value = browser.post(format!("{origin}/v1/cli/requests"))
            .json(&json!({"user_id":"108","channel":"alerts","read":read,"send":send,"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
        let id = request["request_id"].as_str().unwrap().to_owned();
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
        simultaneous.push(id);
    }
    let first = json!({"request_id":simultaneous[0],"verifier":hex::encode(verifier)});
    let second = json!({"request_id":simultaneous[1],"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&first)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&second)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert!(
        store
            .is_permitted("108", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        !store
            .is_permitted("108", "alerts", ChannelAction::Send)
            .unwrap()
    );
    for (kind, existed) in [
        ("bot", false),
        ("webhook", false),
        ("bot", true),
        ("webhook", true),
    ] {
        let name = format!("stale_{kind}_{existed}");
        if existed {
            store.put_bot(&name, "111", "synthetic-token").unwrap();
        }
        let body = if kind == "bot" {
            json!({"channel":name,"discord_channel_id":"654","commitment":hex::encode(Sha256::digest(synthetic_token.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})
        } else {
            json!({"channel":name,"webhook_id":"888","commitment":hex::encode(Sha256::digest(synthetic_webhook.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})
        };
        let request: Value = browser
            .post(format!("{origin}/v1/cli/{kind}-requests"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = request["request_id"].as_str().unwrap();
        store
            .put_bot(&name, "777", "replacement-synthetic")
            .unwrap();
        store.set_permissions("107", &name, true, false).unwrap();
        assert_eq!(
            browser
                .post(format!("{origin}/admin/cli"))
                .header(header::COOKIE, &session)
                .header(header::ORIGIN, &origin)
                .form(&[("request_id", id)])
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let mut payload = json!({"request_id":id,"verifier":hex::encode(verifier)});
        if kind == "bot" {
            payload["bot_token"] = synthetic_token.into();
        } else {
            payload["webhook"] = synthetic_webhook.into();
        }
        assert_eq!(
            browser
                .post(format!("{origin}/v1/cli/execute"))
                .json(&payload)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert!(matches!(
            store.get_channel(&name),
            Ok(discord_mcp::channels::Channel::Bot(_))
        ));
        assert!(
            store
                .is_permitted("107", &name, ChannelAction::Read)
                .unwrap()
        );
    }
    let request: Value = browser.post(format!("{origin}/v1/cli/requests"))
        .json(&json!({"user_id":"106","channel":"alerts","read":true,"send":false,"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let stale_permission_id = request["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", stale_permission_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    store
        .put_bot("alerts", "777", "replacement-synthetic")
        .unwrap();
    let stale_payload = json!({"request_id":stale_permission_id,"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&stale_payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert!(
        !store
            .is_permitted("106", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&stale_payload)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        execute_permissions(hex::encode(verifier))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let request: Value = browser
        .post(format!("{origin}/v1/cli/permission-requests"))
        .json(&json!({"channel":"alerts","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let quarantine_id = request["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", quarantine_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let pending_delete: Value = browser
        .post(format!("{origin}/v1/cli/delete-requests"))
        .json(&json!({"channel":"alerts","challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pending_delete_id = pending_delete["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", pending_delete_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let quarantined_webhook: Value = browser.post(format!("{origin}/v1/cli/webhook-requests"))
        .json(&json!({"channel":"quarantined","webhook_id":"888","commitment":hex::encode(Sha256::digest(synthetic_webhook.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let quarantined_webhook_id = quarantined_webhook["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", quarantined_webhook_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let request: Value = browser.post(format!("{origin}/v1/cli/bot-requests"))
        .json(&json!({"channel":"bot_quarantined","discord_channel_id":"654","commitment":hex::encode(Sha256::digest(synthetic_token.as_bytes())),"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let id = request["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute(
            "INSERT INTO recovery_control(id,quarantined) VALUES(1,1)",
            [],
        )
        .unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":quarantine_id,"verifier":hex::encode(verifier)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":pending_delete_id,"verifier":hex::encode(verifier)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(store.list().unwrap().contains(&"alerts".to_owned()));
    assert_eq!(browser.post(format!("{origin}/v1/cli/execute")).json(&json!({"request_id":quarantined_webhook_id,"verifier":hex::encode(verifier),"webhook":synthetic_webhook})).send().await.unwrap().status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!store.list().unwrap().contains(&"quarantined".to_owned()));
    assert_eq!(browser.post(format!("{origin}/v1/cli/execute")).json(&json!({"request_id":id,"verifier":hex::encode(verifier),"bot_token":synthetic_token})).send().await.unwrap().status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        !store
            .list()
            .unwrap()
            .contains(&"bot_quarantined".to_owned())
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

#[cfg(all(feature = "encrypted-s3-backup", unix))]
#[tokio::test]
#[ignore = "requires isolated local Floci fixture"]
async fn mediated_backup_requires_consent_and_restores_from_floci() {
    use axum::http::header;
    use discord_mcp::channels::{ChannelAction, ChannelStore};
    use discord_mcp::http_auth::AuthState;
    use std::os::unix::fs::PermissionsExt;

    let address: std::net::SocketAddr = std::env::var("DISCORD_MCP_FLOCI_FIXTURE_ADDRESS")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        address.ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    );
    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .endpoint_url(format!("http://{address}"))
        .force_path_style(true)
        .credentials_provider(aws_sdk_s3::config::Credentials::new(
            "test", "test", None, None, "fixture",
        ))
        .build();
    let s3 = aws_sdk_s3::Client::from_conf(config);
    let bucket = format!("discord-mcp-mediated-{}", std::process::id());
    s3.create_bucket().bucket(&bucket).send().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = dir.path().join("service.db");
    let cli_database = dir.path().join("must-not-open.db");
    let key = [91_u8; 32];
    let store = ChannelStore::open(&database, &key).unwrap();
    store.put_bot("alerts", "123", "synthetic-token").unwrap();
    store.set_permissions("101", "alerts", true, false).unwrap();
    let identity = age::x25519::Identity::generate();
    let executor = Arc::new(
        encrypted_backup::ServiceBackup::new_floci(
            address,
            bucket.clone(),
            database.clone(),
            dir.path().into(),
            identity.to_public(),
        )
        .unwrap(),
    );
    let binding = executor.destination_binding();
    let provider = Router::new()
        .route(
            "/api/oauth2/token",
            post(|| async { Json(json!({"access_token":"synthetic","token_type":"Bearer"})) }),
        )
        .route(
            "/api/v10/users/@me",
            axum::routing::get(|| async { Json(json!({"id":"123","username":"Admin"})) }),
        );
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = reqwest::Url::parse(&format!(
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
    .with_test_base(provider_url.clone());
    let admin = admin_web::AdminConfig {
        auth: AuthState {
            database_path: database.clone(),
            encryption_key: key,
            resource: format!("{origin}/mcp"),
            metrics: Default::default(),
            audit: Default::default(),
        },
        public_origin: origin.clone(),
        admin_user_id: "123".into(),
        discord,
        backup: Some(executor),
    };
    let service = tokio::spawn(async move {
        axum::serve(listener, admin_web::router(admin).unwrap())
            .await
            .unwrap()
    });
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let login = browser_login(&browser, &origin).await;
    let session = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mut cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &cli_database)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-backup", &origin, "backup-1"])
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
    let consent = reqwest::Url::parse(line.split_whitespace().last().unwrap()).unwrap();
    let id = consent
        .query_pairs()
        .find(|(key, _)| key == "request_id")
        .unwrap()
        .1
        .into_owned();
    let page = browser
        .get(consent)
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        page.contains("backup-1")
            && page.contains(&bucket)
            && page.contains(&identity.to_public().to_string())
    );
    assert!(
        s3.get_object()
            .bucket(&bucket)
            .key("service-backups/backup-1.age")
            .send()
            .await
            .is_err()
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
    assert!(
        timeout(Duration::from_secs(30), cli.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!cli_database.exists());
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
    let restored = dir.path().join("restored.db");
    let report = encrypted_backup::restore_quarantined(
        &s3,
        &bucket,
        "service-backups/backup-1.age",
        &restored,
        &identity,
        &key,
        90,
    )
    .await
    .unwrap();
    assert_eq!(report.permissions, 1);
    let recovered = ChannelStore::open(&restored, &key).unwrap();
    assert!(recovered.recovery_quarantined().unwrap());
    recovered.activate_recovered_copy().unwrap();
    assert!(
        !recovered
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    let verifier = [44_u8; 32];
    let challenge = hex::encode(Sha256::digest(verifier));
    let begin: Value = browser
        .post(format!("{origin}/v1/cli/backup-requests"))
        .json(&json!({"backup_id":"backup-2", "challenge":challenge}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_id = begin["request_id"].as_str().unwrap();
    let execute = json!({"request_id":request_id,"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&execute)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", request_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let wrong = json!({"request_id":request_id,"verifier":hex::encode([45_u8; 32])});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&wrong)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&execute)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&execute)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let duplicate: Value = browser
        .post(format!("{origin}/v1/cli/backup-requests"))
        .json(&json!({"backup_id":"backup-2", "challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let duplicate_id = duplicate["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", duplicate_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let duplicate_execution = json!({"request_id":duplicate_id,"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&duplicate_execution)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&duplicate_execution)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let audit_rows = store.recent_backup_audit().unwrap();
    assert!(
        audit_rows
            .iter()
            .any(|row| row.backup_id == "backup-1" && row.state == "confirmed")
    );
    assert!(
        audit_rows
            .iter()
            .any(|row| row.backup_id == "backup-2" && row.state == "confirmed")
    );
    assert!(
        audit_rows
            .iter()
            .any(|row| row.backup_id == "backup-2" && row.state == "unknown")
    );
    let audit_html = browser
        .get(format!("{origin}/admin/backup-audit"))
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(audit_html.contains("backup-1") && audit_html.contains("unknown"));
    assert!(!audit_html.contains("synthetic-token"));
    let before_preview = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    let preview = browser
        .get(format!("{origin}/admin/backup-audit?days=1"))
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        preview.contains("Pré-visualização somente leitura")
            && preview.contains("Unknown preservados: 1")
            && preview.contains("Nenhum registro ou objeto foi excluído")
    );
    assert_eq!(
        browser
            .get(format!("{origin}/admin/backup-audit?days=0"))
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        browser
            .get(format!("{origin}/admin/backup-audit?days=1"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let after_preview = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    assert_eq!(before_preview, after_preview);
    println!("PASS audit preview: authorized, read-only and inconclusive attempts preserved");
    assert_eq!(
        browser
            .get(format!("{origin}/admin/backup-audit"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    println!("PASS backup audit: confirmed and unknown results visible without credentials");
    assert_eq!(
        browser
            .post(format!("{origin}/admin/backup-investigate"))
            .form(&[("request_id", duplicate_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/backup-investigate"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", id.as_str())])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/backup-investigate"))
            .header(header::COOKIE, &session)
            .form(&[("request_id", duplicate_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let investigation = browser
        .post(format!("{origin}/admin/backup-investigate"))
        .header(header::COOKIE, &session)
        .header(header::ORIGIN, &origin)
        .form(&[("request_id", duplicate_id)])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        investigation.contains("Objeto encontrado")
            && investigation.contains("integridade, origem e recuperação não foram verificadas")
    );
    assert!(investigation.contains("não autentica a identidade do servidor"));
    assert!(
        store
            .recent_backup_audit()
            .unwrap()
            .iter()
            .any(|row| row.request_id == duplicate_id && row.state == "unknown")
    );
    use age::secrecy::ExposeSecret;
    // Synthetic lost-completion fixture: keep the original pre-PUT commitment
    // and object, but leave the durable attempt inconclusive.
    let verification_target = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .find(|row| row.backup_id == "backup-1")
        .unwrap()
        .request_id;
    let verification_sql = rusqlite::Connection::open(&database).unwrap();
    verification_sql
        .execute(
            "UPDATE backup_audit SET state='unknown',finished_at=NULL WHERE request_id=?1",
            [&verification_target],
        )
        .unwrap();
    let baseline = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    let object_before = s3
        .get_object()
        .bucket(&bucket)
        .key("service-backups/backup-1.age")
        .send()
        .await
        .unwrap()
        .body
        .collect()
        .await
        .unwrap()
        .into_bytes();
    let mut verify_cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &cli_database)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-verify-backup", &origin, &verification_target])
        .arg(dir.path())
        .arg("--confirm-isolated")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let malformed = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &cli_database)
        .env("DISCORD_MCP_KEY", "invalid")
        .args(["remote-verify-backup", &origin, duplicate_id])
        .output()
        .await
        .unwrap();
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("--confirm-isolated"));
    assert!(!cli_database.exists());
    let mut verify_stderr = BufReader::new(verify_cli.stderr.take().unwrap());
    let mut verify_line = String::new();
    timeout(
        Duration::from_secs(10),
        verify_stderr.read_line(&mut verify_line),
    )
    .await
    .unwrap()
    .unwrap();
    let verify_consent =
        reqwest::Url::parse(verify_line.split_whitespace().last().unwrap()).unwrap();
    let verify_id = verify_consent
        .query_pairs()
        .find(|(key, _)| key == "request_id")
        .unwrap()
        .1
        .into_owned();
    let verify_page = browser
        .get(verify_consent)
        .header(header::COOKIE, &session)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        verify_page.contains(&verification_target)
            && verify_page.contains("backup-1")
            && verify_page.contains(&bucket)
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", &verify_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let mut input = verify_cli.stdin.take().unwrap();
    input
        .write_all(
            format!(
                "\n{}\n{}\n",
                identity.to_string().expose_secret(),
                hex::encode(key)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    drop(input);
    let output = timeout(Duration::from_secs(30), verify_cli.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "verification CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Synthetic verification passed"));
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(identity.to_string().expose_secret())
    );
    assert!(!cli_database.exists());
    let after = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    assert_eq!(baseline, after);
    assert_eq!(
        object_before,
        s3.get_object()
            .bucket(&bucket)
            .key("service-backups/backup-1.age")
            .send()
            .await
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes()
    );
    println!(
        "PASS remote authorized isolated verification: attempt binding, audit and object unchanged"
    );
    let verification_request: Value = browser
        .post(format!("{origin}/v1/cli/backup-verification-requests"))
        .json(&json!({"attempt_id":duplicate_id,"challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let verification_id = verification_request["request_id"].as_str().unwrap();
    let verification_execute =
        json!({"request_id":verification_id,"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&verification_execute)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", verification_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let verification_wrong =
        json!({"request_id":verification_id,"verifier":hex::encode([45_u8; 32])});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&verification_wrong)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let binding_db = rusqlite::Connection::open(&database).unwrap();
    binding_db
        .execute(
            "UPDATE backup_target_binding SET fingerprint=?2 WHERE request_id=?1",
            rusqlite::params![duplicate_id, [99_u8; 32].as_slice()],
        )
        .unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&verification_execute)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&verification_execute)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    binding_db
        .execute(
            "UPDATE backup_target_binding SET fingerprint=?2 WHERE request_id=?1",
            rusqlite::params![duplicate_id, binding.as_slice()],
        )
        .unwrap();
    let collision: Value = browser
        .post(format!("{origin}/v1/cli/backup-verification-requests"))
        .json(&json!({"attempt_id":duplicate_id,"challenge":hex::encode(Sha256::digest(verifier))}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let collision_id = collision["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", collision_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":collision_id,"verifier":hex::encode(verifier)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    println!("PASS preexisting object rejected when ciphertext commitment differs");
    let before_comparison = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    let divergent = browser
        .post(format!("{origin}/admin/backup-compare"))
        .header(header::COOKIE, &session)
        .header(header::ORIGIN, &origin)
        .form(&[("request_id", duplicate_id)])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        divergent.contains("diverge do compromisso") && divergent.contains("continua inconclusiva")
    );
    assert_eq!(
        before_comparison,
        store
            .recent_backup_audit()
            .unwrap()
            .into_iter()
            .map(|row| (row.request_id, row.state))
            .collect::<Vec<_>>()
    );
    let legacy_commitment_id = "c".repeat(64);
    store
        .begin_backup_audit(
            &legacy_commitment_id,
            "legacy-commitment",
            "123",
            discord_mcp::channels::current_timestamp().unwrap(),
            &binding,
        )
        .unwrap();
    store
        .finish_backup_audit(
            &legacy_commitment_id,
            false,
            discord_mcp::channels::current_timestamp().unwrap(),
        )
        .unwrap();
    s3.put_object()
        .bucket(&bucket)
        .key("service-backups/legacy-commitment.age")
        .body(object_before.clone().into())
        .send()
        .await
        .unwrap();
    let legacy_request: Value = browser.post(format!("{origin}/v1/cli/backup-verification-requests"))
        .json(&json!({"attempt_id":legacy_commitment_id,"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let legacy_request_id = legacy_request["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", legacy_request_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let legacy_response = browser
        .post(format!("{origin}/v1/cli/execute"))
        .json(&json!({"request_id":legacy_request_id,"verifier":hex::encode(verifier)}))
        .send()
        .await
        .unwrap();
    assert_eq!(legacy_response.status(), StatusCode::OK);
    assert_eq!(
        legacy_response
            .headers()
            .get("x-backup-ciphertext-binding")
            .unwrap(),
        "unavailable"
    );
    assert_eq!(legacy_response.bytes().await.unwrap(), object_before);
    println!("PASS legacy attempt remains unbound without changing old audit records");
    let old = browser
        .post(format!("{origin}/admin/backup-compare"))
        .header(header::COOKIE, &session)
        .header(header::ORIGIN, &origin)
        .form(&[("request_id", &legacy_commitment_id)])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(old.contains("Sem compromisso pré-envio") && old.contains("nenhuma consulta"));
    println!(
        "PASS remote verification: unapproved, wrong verifier, changed binding and replay refused"
    );
    let missing_id = "a".repeat(64);
    store
        .begin_backup_audit(
            &missing_id,
            "absent-fixture",
            "123",
            discord_mcp::channels::current_timestamp().unwrap(),
            &binding,
        )
        .unwrap();
    store
        .finish_backup_audit(
            &missing_id,
            false,
            discord_mcp::channels::current_timestamp().unwrap(),
        )
        .unwrap();
    let missing = browser
        .post(format!("{origin}/admin/backup-investigate"))
        .header(header::COOKIE, &session)
        .header(header::ORIGIN, &origin)
        .form(&[("request_id", &missing_id)])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(missing.contains("Objeto não encontrado"));
    let other_bucket = format!("discord-mcp-other-{}", std::process::id());
    s3.create_bucket()
        .bucket(&other_bucket)
        .send()
        .await
        .unwrap();
    s3.put_object()
        .bucket(&other_bucket)
        .key("service-backups/backup-2.age")
        .body(aws_sdk_s3::primitives::ByteStream::from_static(
            b"other destination object",
        ))
        .send()
        .await
        .unwrap();
    let other = encrypted_backup::ServiceBackup::new_floci(
        address,
        other_bucket,
        database.clone(),
        dir.path().into(),
        identity.to_public(),
    )
    .unwrap();
    let alternate_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alternate_origin = format!("http://{}", alternate_listener.local_addr().unwrap());
    let alternate_discord = discord_login::DiscordLoginClient::new(
        "123",
        "synthetic-secret",
        &format!("{alternate_origin}/admin/discord/callback"),
    )
    .unwrap()
    .with_test_base(provider_url);
    let alternate_config = admin_web::AdminConfig {
        auth: AuthState {
            database_path: database.clone(),
            encryption_key: key,
            resource: format!("{alternate_origin}/mcp"),
            metrics: Default::default(),
            audit: Default::default(),
        },
        public_origin: alternate_origin.clone(),
        admin_user_id: "123".into(),
        discord: alternate_discord,
        backup: Some(Arc::new(other)),
    };
    let alternate_task = tokio::spawn(async move {
        axum::serve(
            alternate_listener,
            admin_web::router(alternate_config).unwrap(),
        )
        .await
        .unwrap()
    });
    let alternate_login = browser_login(&browser, &alternate_origin).await;
    let alternate_session = alternate_login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mismatch = browser
        .post(format!("{alternate_origin}/admin/backup-investigate"))
        .header(header::COOKIE, &alternate_session)
        .header(header::ORIGIN, &alternate_origin)
        .form(&[("request_id", duplicate_id)])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        mismatch.contains("destino configurado diverge")
            && mismatch.contains("Nenhuma consulta ao armazenamento")
    );
    assert_eq!(browser.post(format!("{alternate_origin}/v1/cli/backup-verification-requests")).json(&json!({"attempt_id":duplicate_id,"challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().status(), StatusCode::CONFLICT);
    alternate_task.abort();
    let binding_sql = rusqlite::Connection::open(&database).unwrap();
    binding_sql
        .execute(
            "DELETE FROM backup_target_binding WHERE request_id=?1",
            [&missing_id],
        )
        .unwrap();
    let legacy = browser
        .post(format!("{origin}/admin/backup-investigate"))
        .header(header::COOKIE, &session)
        .header(header::ORIGIN, &origin)
        .form(&[("request_id", &missing_id)])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        legacy.contains("Registro antigo sem vínculo")
            && legacy.contains("Nenhuma consulta ao armazenamento")
    );
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/backup-verification-requests"))
            .json(
                &json!({"attempt_id":missing_id,"challenge":hex::encode(Sha256::digest(verifier))})
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(
        store
            .recent_backup_audit()
            .unwrap()
            .iter()
            .any(|row| row.request_id == missing_id && row.state == "unknown")
    );
    println!("PASS investigation refuses different Floci bucket and legacy unbound record");
    let corrupt_id = "b".repeat(64);
    store
        .begin_backup_audit(
            &corrupt_id,
            "corrupt-fixture",
            "123",
            discord_mcp::channels::current_timestamp().unwrap(),
            &binding,
        )
        .unwrap();
    store
        .finish_backup_audit(
            &corrupt_id,
            false,
            discord_mcp::channels::current_timestamp().unwrap(),
        )
        .unwrap();
    s3.put_object()
        .bucket(&bucket)
        .key("service-backups/corrupt-fixture.age")
        .body(aws_sdk_s3::primitives::ByteStream::from_static(
            b"synthetic corrupt object",
        ))
        .send()
        .await
        .unwrap();
    let corrupted = browser
        .post(format!("{origin}/admin/backup-investigate"))
        .header(header::COOKIE, &session)
        .header(header::ORIGIN, &origin)
        .form(&[("request_id", &corrupt_id)])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        corrupted.contains("Objeto encontrado")
            && corrupted.contains("integridade, origem e recuperação não foram verificadas")
    );
    assert!(
        encrypted_backup::restore_quarantined(
            &s3,
            &bucket,
            "service-backups/corrupt-fixture.age",
            &dir.path().join("not-published.db"),
            &identity,
            &key,
            90
        )
        .await
        .is_err()
    );
    assert!(!dir.path().join("not-published.db").exists());
    println!(
        "PASS authorized investigation: found, missing, corrupt object, no inferred integrity or state change"
    );
    let sql = rusqlite::Connection::open(&database).unwrap();
    let third: Value = browser.post(format!("{origin}/v1/cli/backup-requests"))
        .json(&json!({"backup_id":"audit-failure", "challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let third_id = third["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", third_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    sql.execute_batch("CREATE TRIGGER synthetic_backup_finish_failure BEFORE UPDATE ON backup_audit BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;").unwrap();
    let third_execution = json!({"request_id":third_id,"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&third_execution)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(
        s3.get_object()
            .bucket(&bucket)
            .key("service-backups/audit-failure.age")
            .send()
            .await
            .is_ok()
    );
    assert!(
        store
            .recent_backup_audit()
            .unwrap()
            .iter()
            .any(|row| row.backup_id == "audit-failure" && row.state == "started")
    );
    sql.execute_batch("DROP TRIGGER synthetic_backup_finish_failure")
        .unwrap();
    println!("PASS backup audit: lost result after upload leaves durable inconclusive intent");
    let lost_ciphertext = s3
        .get_object()
        .bucket(&bucket)
        .key("service-backups/audit-failure.age")
        .send()
        .await
        .unwrap()
        .body
        .collect()
        .await
        .unwrap()
        .into_bytes();
    let (lost_digest, lost_length) = store
        .backup_ciphertext_commitment(third_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        lost_digest.as_slice(),
        Sha256::digest(&lost_ciphertext).as_slice()
    );
    assert_eq!(lost_length, lost_ciphertext.len() as u64);
    println!(
        "PASS lost result: pre-PUT commitment matches observed ciphertext without inferring recoverability"
    );
    let saved_object = s3
        .get_object()
        .bucket(&bucket)
        .key("service-backups/backup-2.age")
        .send()
        .await
        .unwrap()
        .body
        .collect()
        .await
        .unwrap()
        .into_bytes();
    let audit_before = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    let mut verifier_cli = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .args(["verify-floci", &address.to_string(), &bucket, "backup-2"])
        .arg(dir.path())
        .arg("--confirm-isolated")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let input = format!(
        "{}\n{}\n",
        identity.to_string().expose_secret(),
        hex::encode(key)
    );
    let mut stdin = verifier_cli.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).await.unwrap();
    drop(stdin);
    let checked = timeout(Duration::from_secs(30), verifier_cli.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        checked.status.success(),
        "isolated verifier failed: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(String::from_utf8_lossy(&checked.stdout).contains("Synthetic verification passed"));
    let audit_after = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    assert_eq!(audit_before, audit_after);
    assert_eq!(
        saved_object,
        s3.get_object()
            .bucket(&bucket)
            .key("service-backups/backup-2.age")
            .send()
            .await
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes()
    );
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".discord-mcp-verify-")
    }));
    println!("PASS local verification of inconclusive object: audit and Floci object unchanged");
    let fourth: Value = browser.post(format!("{origin}/v1/cli/backup-requests"))
        .json(&json!({"backup_id":"audit-start-failure", "challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let fourth_id = fourth["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", fourth_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    sql.execute_batch("CREATE TRIGGER synthetic_backup_start_failure BEFORE INSERT ON backup_audit BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;").unwrap();
    let fourth_execution = json!({"request_id":fourth_id,"verifier":hex::encode(verifier)});
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&fourth_execution)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        s3.get_object()
            .bucket(&bucket)
            .key("service-backups/audit-start-failure.age")
            .send()
            .await
            .is_err()
    );
    sql.execute_batch("DROP TRIGGER synthetic_backup_start_failure")
        .unwrap();
    println!("PASS backup audit: failed intent prevents upload");
    let commitment_fail: Value = browser.post(format!("{origin}/v1/cli/backup-requests"))
        .json(&json!({"backup_id":"commitment-failure", "challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let commitment_fail_id = commitment_fail["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", commitment_fail_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    sql.execute_batch("CREATE TRIGGER synthetic_commitment_failure BEFORE INSERT ON backup_ciphertext_commitment BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;").unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/v1/cli/execute"))
            .json(&json!({"request_id":commitment_fail_id,"verifier":hex::encode(verifier)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_GATEWAY
    );
    assert!(
        s3.get_object()
            .bucket(&bucket)
            .key("service-backups/commitment-failure.age")
            .send()
            .await
            .is_err()
    );
    assert!(
        store
            .backup_ciphertext_commitment(commitment_fail_id)
            .unwrap()
            .is_none()
    );
    sql.execute_batch("DROP TRIGGER synthetic_commitment_failure")
        .unwrap();
    println!("PASS failed pre-PUT commitment prevents remote object creation");
    let lost: Value = browser.post(format!("{origin}/v1/cli/backup-requests"))
        .json(&json!({"backup_id":"response-lost", "challenge":hex::encode(Sha256::digest(verifier))})).send().await.unwrap().json().await.unwrap();
    let lost_id = lost["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", lost_id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let payload = json!({"request_id":lost_id,"verifier":hex::encode(verifier)}).to_string();
    let mut socket = tokio::net::TcpStream::connect(
        reqwest::Url::parse(&origin)
            .unwrap()
            .socket_addrs(|| None)
            .unwrap()[0],
    )
    .await
    .unwrap();
    socket.write_all(format!("POST /v1/cli/execute HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", payload.len(), payload).as_bytes()).await.unwrap();
    let mut observed = false;
    for _ in 0..100 {
        let rows = store.recent_backup_audit().unwrap();
        if rows.iter().any(|row| row.backup_id == "response-lost") {
            observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        observed,
        "service did not record intent before client disconnect"
    );
    drop(socket);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let rows = store.recent_backup_audit().unwrap();
    let state = &rows
        .iter()
        .find(|row| row.backup_id == "response-lost")
        .unwrap()
        .state;
    assert!(["started", "confirmed", "unknown"].contains(&state.as_str()));
    if state == "confirmed" {
        assert!(
            s3.get_object()
                .bucket(&bucket)
                .key("service-backups/response-lost.age")
                .send()
                .await
                .is_ok()
        );
    }
    println!("PASS backup audit: client disconnect preserves state={state}");
    println!("PASS backup approval: denied before consent, wrong verifier denied, replay denied");
    println!(
        "PASS remote CLI consent -> service-owned encrypted backup -> Floci -> quarantined restore"
    );
    service.abort();
    provider_task.abort();
}

#[cfg(all(feature = "encrypted-s3-backup", unix))]
async fn drop_successful_put_proxy(
    upstream: std::net::SocketAddr,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let dropped = Arc::new(AtomicUsize::new(0));
    let result = dropped.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut incoming, _)) = listener.accept().await else {
                break;
            };
            let dropped = dropped.clone();
            tokio::spawn(async move {
                let mut headers = Vec::new();
                let mut byte = [0_u8; 1];
                while headers.len() < 32768 {
                    if incoming.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    headers.push(byte[0]);
                    if headers.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                if !headers.ends_with(b"\r\n\r\n") {
                    return;
                }
                let is_put = headers.starts_with(b"PUT ");
                let Ok(mut outgoing) = tokio::net::TcpStream::connect(upstream).await else {
                    return;
                };
                if outgoing.write_all(&headers).await.is_err() {
                    return;
                }
                let (mut client_read, mut client_write) = incoming.into_split();
                let (mut upstream_read, mut upstream_write) = outgoing.into_split();
                let upload = tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut client_read, &mut upstream_write).await;
                });
                loop {
                    let mut response_headers = Vec::new();
                    while response_headers.len() < 32768 {
                        if upstream_read.read_exact(&mut byte).await.is_err() {
                            upload.abort();
                            return;
                        }
                        response_headers.push(byte[0]);
                        if response_headers.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    if !response_headers.ends_with(b"\r\n\r\n") {
                        upload.abort();
                        return;
                    }
                    if is_put
                        && (response_headers.starts_with(b"HTTP/1.1 200 ")
                            || response_headers.starts_with(b"HTTP/1.1 201 "))
                    {
                        dropped.fetch_add(1, Ordering::SeqCst);
                        upload.abort();
                        return;
                    }
                    if client_write.write_all(&response_headers).await.is_err() {
                        upload.abort();
                        return;
                    }
                    if response_headers.starts_with(b"HTTP/1.1 100 ") {
                        continue;
                    }
                    let _ = tokio::io::copy(&mut upstream_read, &mut client_write).await;
                    upload.abort();
                    return;
                }
            });
        }
    });
    (address, result, task)
}

#[cfg(all(feature = "encrypted-s3-backup", unix))]
#[tokio::test]
#[ignore = "requires isolated local Floci fixture"]
async fn lost_put_response_authorized_comparison_with_floci() {
    use axum::http::header;
    use discord_mcp::channels::{ChannelAction, ChannelStore};
    use discord_mcp::http_auth::AuthState;
    use std::os::unix::fs::PermissionsExt;
    let upstream: std::net::SocketAddr = std::env::var("DISCORD_MCP_FLOCI_FIXTURE_ADDRESS")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        upstream.ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    );
    let s3_config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .endpoint_url(format!("http://{upstream}"))
        .force_path_style(true)
        .credentials_provider(aws_sdk_s3::config::Credentials::new(
            "test", "test", None, None, "fixture",
        ))
        .build();
    let s3 = aws_sdk_s3::Client::from_conf(s3_config);
    let bucket = format!("discord-mcp-lost-{}", std::process::id());
    s3.create_bucket().bucket(&bucket).send().await.unwrap();
    let (proxy_address, dropped, proxy) = drop_successful_put_proxy(upstream).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = dir.path().join("service.db");
    let key = [57_u8; 32];
    let store = ChannelStore::open(&database, &key).unwrap();
    store.put_bot("alerts", "123", "synthetic-token").unwrap();
    store.set_permissions("101", "alerts", true, false).unwrap();
    let identity = age::x25519::Identity::generate();
    let executor = Arc::new(
        encrypted_backup::ServiceBackup::new_floci(
            proxy_address,
            bucket.clone(),
            database.clone(),
            dir.path().into(),
            identity.to_public(),
        )
        .unwrap(),
    );
    let provider = Router::new()
        .route(
            "/api/oauth2/token",
            post(|| async { Json(json!({"access_token":"synthetic","token_type":"Bearer"})) }),
        )
        .route(
            "/api/v10/users/@me",
            axum::routing::get(|| async { Json(json!({"id":"123","username":"Admin"})) }),
        );
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = reqwest::Url::parse(&format!(
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
    .with_test_base(provider_url);
    let admin = admin_web::AdminConfig {
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
        backup: Some(executor),
    };
    let service = tokio::spawn(async move {
        axum::serve(listener, admin_web::router(admin).unwrap())
            .await
            .unwrap()
    });
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(45))
        .build()
        .unwrap();
    let login = browser_login(&browser, &origin).await;
    let session = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let verifier = [72_u8; 32];
    let begin: Value = browser
        .post(format!("{origin}/v1/cli/backup-requests"))
        .json(
            &json!({"backup_id":"response-lost","challenge":hex::encode(Sha256::digest(verifier))}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = begin["request_id"].as_str().unwrap();
    assert_eq!(
        browser
            .post(format!("{origin}/admin/cli"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .form(&[("request_id", id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let result = browser
        .post(format!("{origin}/v1/cli/execute"))
        .json(&json!({"request_id":id,"verifier":hex::encode(verifier)}))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::BAD_GATEWAY);
    assert!(
        dropped.load(Ordering::SeqCst) > 0,
        "proxy did not discard a successful PUT response"
    );
    let before_object = s3
        .get_object()
        .bucket(&bucket)
        .key("service-backups/response-lost.age")
        .send()
        .await
        .unwrap()
        .body
        .collect()
        .await
        .unwrap()
        .into_bytes();
    let commitment = store.backup_ciphertext_commitment(id).unwrap().unwrap();
    assert_eq!(
        commitment.0.as_slice(),
        Sha256::digest(&before_object).as_slice()
    );
    assert_eq!(commitment.1, before_object.len() as u64);
    let before = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    assert!(
        before
            .iter()
            .any(|(request, state)| request == id && state == "unknown")
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/backup-compare"))
            .form(&[("request_id", id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser
            .post(format!("{origin}/admin/backup-compare"))
            .header(header::COOKIE, &session)
            .form(&[("request_id", id)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let compared = browser
        .post(format!("{origin}/admin/backup-compare"))
        .header(header::COOKIE, &session)
        .header(header::ORIGIN, &origin)
        .form(&[("request_id", id)])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        compared.contains("ciphertext observado coincide")
            && compared.contains("não comprova quem gravou")
            && compared.contains("recuperabilidade")
    );
    let after = store
        .recent_backup_audit()
        .unwrap()
        .into_iter()
        .map(|row| (row.request_id, row.state))
        .collect::<Vec<_>>();
    assert_eq!(before, after);
    let after_object = s3
        .get_object()
        .bucket(&bucket)
        .key("service-backups/response-lost.age")
        .send()
        .await
        .unwrap()
        .body
        .collect()
        .await
        .unwrap()
        .into_bytes();
    assert_eq!(before_object, after_object);
    assert!(
        store
            .is_permitted("101", "alerts", ChannelAction::Read)
            .unwrap()
    );
    println!(
        "PASS Floci committed PUT; proxy dropped success response; authorized comparison matches commitment while audit remains unknown"
    );
    service.abort();
    provider_task.abort();
    proxy.abort();
}
