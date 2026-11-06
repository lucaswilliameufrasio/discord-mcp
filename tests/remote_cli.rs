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
