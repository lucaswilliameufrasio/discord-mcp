#![cfg(all(discord_mcp_synthetic_authority, target_os = "linux"))]

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use discord_mcp::channels::{ChannelStore, EmbeddedSecret};
use std::{
    io::Write,
    net::TcpListener,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

async fn token(State(epoch): State<Arc<AtomicUsize>>, headers: HeaderMap) -> Response {
    let expected = match epoch.load(Ordering::SeqCst) {
        0 => "123:synthetic-old",
        1 => "123:synthetic-new",
        _ => "no-valid-credential",
    };
    let auth = format!("Basic {}", STANDARD.encode(expected));
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(auth.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(rmcp::serde_json::json!({"access_token":"synthetic-access","token_type":"Bearer"}))
        .into_response()
}
async fn identity() -> axum::Json<rmcp::serde_json::Value> {
    axum::Json(rmcp::serde_json::json!({"id":"123","username":"fixture"}))
}

async fn infisical_fixture(State(stage): State<Arc<AtomicUsize>>, headers: HeaderMap) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bearer synthetic-token")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let value = match stage.load(Ordering::SeqCst) {
        0 => "synthetic-old",
        1 => "synthetic-new",
        _ => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    axum::Json(rmcp::serde_json::json!({"secret":{"secretKey":"DISCORD_MCP_DISCORD_CLIENT_SECRET","secretValue":value,"secretPath":"/discord","environment":"test","secretValueHidden":false}})).into_response()
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn instrumented_serve_rejects_non_loopback_and_non_synthetic_values() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    drop(ChannelStore::open(&db, &[96; 32]).unwrap());
    let run = |authority: Option<&str>, client_id: &str, secret: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        cmd.env_clear()
            .arg("serve")
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", hex::encode([96; 32]))
            .env("DISCORD_MCP_LISTEN", "127.0.0.1:42001")
            .env("DISCORD_MCP_PUBLIC_ORIGIN", "http://127.0.0.1:42001")
            .env("DISCORD_MCP_DISCORD_CLIENT_ID", client_id)
            .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", secret);
        if let Some(value) = authority {
            cmd.env("DISCORD_TEST_SYNTHETIC_AUTHORITY", value);
        }
        cmd.output().unwrap()
    };
    for authority in [
        None,
        Some("http://example.invalid:4566/"),
        Some("http://127.0.0.2:4566/"),
        Some("https://127.0.0.1:4566/"),
        Some("http://127.0.0.1/"),
        Some("http://127.0.0.1:4566/path"),
        Some("http://127.0.0.1:4566/?query=1"),
        Some("http://127.0.0.1:4566/#fragment"),
        Some("http://user@127.0.0.1:4566/"),
    ] {
        let output = run(authority, "123", "synthetic-old");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("InvalidConfiguration"));
    }
    for (client, secret) in [("124", "synthetic-old"), ("123", "not-synthetic")] {
        let output = run(Some("http://127.0.0.1:4566/"), client, secret);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("InvalidConfiguration"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
}

async fn ready(client: &reqwest::Client, origin: &str) {
    for _ in 0..100 {
        if client.get(format!("{origin}/admin")).send().await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("synthetic server did not start");
}
async fn login(
    client: &reqwest::Client,
    origin: &str,
    admin: bool,
) -> (StatusCode, Option<String>) {
    let url = if admin {
        format!("{origin}/admin/login")
    } else {
        let mut url = reqwest::Url::parse(&format!("{origin}/oauth/authorize")).unwrap();
        url.query_pairs_mut()
            .append_pair("client_id", "fixture")
            .append_pair("redirect_uri", "http://127.0.0.1:8765/callback")
            .append_pair("response_type", "code")
            .append_pair(
                "code_challenge",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            )
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &format!("{origin}/mcp"))
            .append_pair("state", "state");
        url.to_string()
    };
    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let location =
        reqwest::Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    let state = location
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let path = if admin {
        "/admin/discord/callback"
    } else {
        "/oauth/discord/callback"
    };
    let mut callback = reqwest::Url::parse(&format!("{origin}{path}")).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &state)
        .append_pair("code", "synthetic-code")
        .append_pair("iss", "https://discord.com");
    let result = client
        .get(callback)
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    let session = result
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap().to_owned())
        .find(|v| v.starts_with("discord_mcp_admin_session="));
    (result.status(), session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_supervisor_restart_and_revocation_remain_unverified() {
    let mode = Arc::new(AtomicUsize::new(1));
    let app = Router::new()
        .route("/api/oauth2/token", post(token))
        .route("/api/v10/users/@me", get(identity))
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("http://{}/", listener.local_addr().unwrap());
    let fixture = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fixture.db");
    let key = [119; 32];
    let operation = [101; 16];
    let instance = [102; 16];
    let store = ChannelStore::open(&db, &key).unwrap();
    let secret = EmbeddedSecret::DiscordOAuthClientSecret;
    store
        .insert_embedded_secret(secret, "synthetic-old")
        .unwrap();
    store
        .rotate_embedded_secret_tracked(
            secret,
            "synthetic-old",
            "synthetic-new",
            operation,
            &[instance],
        )
        .unwrap();
    drop(store);
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let origin = format!("http://{address}");
    let start = || {
        Process(
            Command::new("/usr/bin/python3")
                .env_clear()
                .args([
                    "-I",
                    "-c",
                    include_str!("../drills/supervisor_serve_fixture.py"),
                    env!("CARGO_BIN_EXE_discord-mcp"),
                ])
                .env("DISCORD_MCP_DB", &db)
                .env("DISCORD_MCP_KEY", hex::encode(key))
                .env("DISCORD_TEST_SYNTHETIC_AUTHORITY", &authority)
                .env("DISCORD_MCP_SECRET_PROVIDER", "embedded")
                .env("DISCORD_MCP_ROTATION_OPERATION", hex::encode(operation))
                .env("DISCORD_MCP_ROTATION_INSTANCE", hex::encode(instance))
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let mut first = start();
    ready(&client, &origin).await;
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::SEE_OTHER);
    assert!(
        Command::new("kill")
            .args(["-TERM", &first.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(first.0.wait().unwrap().success());
    drop(first);
    let second = start();
    ready(&client, &origin).await;
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::SEE_OTHER);
    mode.store(2, Ordering::SeqCst);
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::FORBIDDEN);
    drop(second);
    let status = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args([
            "setup",
            "local",
            "vault",
            "rotation-status",
            &hex::encode(operation),
        ])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("adoption_verified=false"));
    fixture.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_fixture_receives_login_events_without_verifying_adoption() {
    use discord_mcp::linux_peer_identity::{
        MessageReceiver, PeerIdentityError, RotationScope, SupervisedPeer,
    };
    use std::{os::fd::OwnedFd, os::unix::fs::MetadataExt};
    // Fixture-only historical record. The opaque baseline is not an authority
    // and this must never authorize upstream actions or claim adoption.
    type FixtureBaseline = (i64, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);
    fn baseline(db: &std::path::Path, operation: [u8; 16]) -> FixtureBaseline {
        rusqlite::Connection::open(db).unwrap().query_row(
            "SELECT p.version,p.state,b.commitment,s.nonce,s.ciphertext FROM rotation_progress p JOIN rotation_secret_binding b ON b.revision=p.revision JOIN embedded_secrets s ON s.name=b.name WHERE p.revision=?1",
            [operation.as_slice()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).unwrap()
    }
    fn record_fixture(
        db: &std::path::Path,
        scope: RotationScope,
        event: &[u8],
        baseline: &FixtureBaseline,
    ) -> rusqlite::Result<bool> {
        if event != b"oauth" && event != b"admin" {
            return Ok(false);
        }
        let mut conn = rusqlite::Connection::open(db)?;
        conn.busy_timeout(Duration::ZERO)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT INTO fixture_supervised_decision(operation,instance,epoch,event) SELECT p.revision,e.instance,e.boot,?1 FROM rotation_progress p JOIN rotation_process_epoch e ON e.revision=p.revision JOIN rotation_secret_binding b ON b.revision=p.revision JOIN embedded_secrets s ON s.name=b.name WHERE p.revision=?2 AND e.instance=?3 AND e.boot=?4 AND p.version=?5 AND p.state=?6 AND b.commitment=?7 AND s.nonce=?8 AND s.ciphertext=?9",
            rusqlite::params![event,scope.operation.as_slice(),scope.instance.as_slice(),scope.boot.as_slice(),baseline.0,&baseline.1,&baseline.2,&baseline.3,&baseline.4],
        )?;
        tx.commit()?;
        Ok(changed == 1)
    }
    struct Guard(Option<SupervisedPeer>);
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(supervisor) = self.0.take() {
                let mut child = supervisor.into_child();
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mode = Arc::new(AtomicUsize::new(1));
    let app = Router::new()
        .route("/api/oauth2/token", post(token))
        .route("/api/v10/users/@me", get(identity))
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("http://{}/", listener.local_addr().unwrap());
    let fixture = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fixture.db");
    let key = [115; 32];
    let operation = [51; 16];
    let instance = [52; 16];
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .insert_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret, "synthetic-old")
        .unwrap();
    rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE fixture_supervised_decision (operation BLOB NOT NULL, instance BLOB NOT NULL, epoch BLOB NOT NULL, event BLOB NOT NULL)").unwrap();
    store
        .rotate_embedded_secret_tracked(
            EmbeddedSecret::DiscordOAuthClientSecret,
            "synthetic-old",
            "synthetic-new",
            operation,
            &[instance],
        )
        .unwrap();
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = socket.local_addr().unwrap();
    drop(socket);
    let origin = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let mut previous_boot = None;
    for round in 0..2 {
        let mut boot = [0u8; 16];
        getrandom::fill(&mut boot).unwrap();
        assert_ne!(boot, [0; 16]);
        let (receiver, sender) = MessageReceiver::pair().unwrap();
        let forged_sender = sender.try_clone().unwrap();
        let fd: OwnedFd = sender.into();
        let child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .arg("serve")
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_TEST_SYNTHETIC_AUTHORITY", &authority)
            .env("DISCORD_TEST_SUPERVISED_EVENTS", "1")
            .env("DISCORD_TEST_SUPERVISOR_EPOCH", hex::encode(boot))
            .env("DISCORD_MCP_SECRET_PROVIDER", "embedded")
            .env("DISCORD_MCP_ROTATION_OPERATION", hex::encode(operation))
            .env("DISCORD_MCP_ROTATION_INSTANCE", hex::encode(instance))
            .env("DISCORD_MCP_LISTEN", addr.to_string())
            .env("DISCORD_MCP_PUBLIC_ORIGIN", &origin)
            .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
            .env("DISCORD_MCP_ADMIN_ID", "123")
            .env("DISCORD_MCP_CLIENT_ID", "fixture")
            .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
            .env(
                "DISCORD_MCP_CLIENT_REDIRECT",
                "http://127.0.0.1:8765/callback",
            )
            .stdin(Stdio::from(fd))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        ready(&client, &origin).await;
        if let Some(previous) = previous_boot {
            assert_ne!(previous, boot);
        }
        previous_boot = Some(boot);
        let scope = RotationScope {
            operation,
            instance,
            boot,
        };
        let mut guard = Guard(Some(SupervisedPeer::new(child, uid, scope).unwrap()));
        let supervisor = guard.0.as_mut().unwrap();
        let mut payload = [0; 32];
        store
            .check_rotation_process_epoch(operation, instance, boot)
            .unwrap();
        let initial = baseline(&db, operation);
        forged_sender.send(b"oauth").unwrap();
        assert_eq!(
            supervisor.receive_rotation_message(&receiver, &store, scope, &mut payload),
            Err(PeerIdentityError::Mismatch)
        );
        for (admin, event) in [(false, b"oauth".as_slice()), (true, b"admin".as_slice())] {
            assert_eq!(
                login(&client, &origin, admin).await.0,
                StatusCode::SEE_OTHER
            );
            let count = supervisor
                .receive_rotation_message(&receiver, &store, scope, &mut payload)
                .unwrap();
            assert_eq!(&payload[..count], event);
            if round == 0 && !admin {
                let conn = rusqlite::Connection::open(&db).unwrap();
                conn.execute_batch("CREATE TRIGGER fixture_fail BEFORE INSERT ON fixture_supervised_decision BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
                assert!(record_fixture(&db, scope, &payload[..count], &initial).is_err());
                let rows: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM fixture_supervised_decision",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(rows, 0);
                conn.execute_batch("DROP TRIGGER fixture_fail").unwrap();
                conn.execute_batch("BEGIN IMMEDIATE").unwrap();
                conn.execute(
                    "UPDATE rotation_process_epoch SET boot=?1 WHERE revision=?2 AND instance=?3",
                    rusqlite::params![
                        [54u8; 16].as_slice(),
                        operation.as_slice(),
                        instance.as_slice()
                    ],
                )
                .unwrap();
                let failure = record_fixture(&db, scope, &payload[..count], &initial).unwrap_err();
                assert!(
                    matches!(failure, rusqlite::Error::SqliteFailure(ref error, _) if error.code == rusqlite::ErrorCode::DatabaseBusy)
                );
                conn.execute_batch("ROLLBACK").unwrap();
                let rows: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM fixture_supervised_decision",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(rows, 0);
            }
            assert!(record_fixture(&db, scope, &payload[..count], &initial).unwrap());
        }
        assert_eq!(
            store
                .get_rotation_progress(operation)
                .unwrap()
                .unwrap()
                .1
                .publication(),
            discord_mcp::rotation_progress::Publication::Unknown
        );
        if round == 0 {
            // An earlier successful receive is only a snapshot, not a durable authorization.
            assert!(
                store
                    .check_rotation_process_epoch(operation, instance, boot)
                    .is_ok()
            );
            assert_eq!(
                login(&client, &origin, false).await.0,
                StatusCode::SEE_OTHER
            );
            store
                .begin_rotation_process(operation, instance, [53; 16], "synthetic-new")
                .unwrap();
            assert!(
                store
                    .check_rotation_process_epoch(operation, instance, boot)
                    .is_err()
            );
            assert!(!record_fixture(&db, scope, b"oauth", &initial).unwrap());
            assert_eq!(
                supervisor.receive_rotation_message(&receiver, &store, scope, &mut payload),
                Err(PeerIdentityError::TrackingUnavailable)
            );
        }
        if round == 1 {
            assert_eq!(
                login(&client, &origin, false).await.0,
                StatusCode::SEE_OTHER
            );
            store
                .advance_rotation_progress(
                    operation,
                    0,
                    discord_mcp::rotation_progress::ProgressEvent::Revoked,
                )
                .unwrap();
            assert!(!record_fixture(&db, scope, b"oauth", &initial).unwrap());
            assert_eq!(
                supervisor.receive_rotation_message(&receiver, &store, scope, &mut payload),
                Err(PeerIdentityError::TrackingUnavailable)
            );
            mode.store(2, Ordering::SeqCst);
            assert_eq!(
                login(&client, &origin, false).await.0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(login(&client, &origin, true).await.0, StatusCode::FORBIDDEN);
            assert_eq!(
                supervisor.receive_rotation_message(&receiver, &store, scope, &mut payload),
                Err(PeerIdentityError::MessageUnavailable)
            );
        }
        drop(guard);
    }
    let count: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM fixture_supervised_decision",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 4);
    let status = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args([
            "setup",
            "local",
            "vault",
            "rotation-status",
            &hex::encode(operation),
        ])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("adoption_verified=false"));
    fixture.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_serve_restart_and_revocation_with_loopback_authority() {
    assert!(
        discord_mcp::discord_login::DiscordLoginClient::new(
            "123",
            "not-a-synthetic-fixture-value",
            "http://127.0.0.1:8765/callback"
        )
        .is_err()
    );
    let mode = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/api/oauth2/token", post(token))
        .route("/api/v10/users/@me", get(identity))
        .with_state(mode.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("http://{}/", listener.local_addr().unwrap());
    let fixture = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let key = [95; 32];
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .insert_embedded_secret(EmbeddedSecret::DiscordOAuthClientSecret, "synthetic-old")
        .unwrap();
    drop(store);
    let sock = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    drop(sock);
    let origin = format!("http://{addr}");
    let base = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        cmd.env_clear()
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", hex::encode(key));
        cmd
    };
    let start = |tracked: bool| {
        let mut cmd = base();
        cmd.arg("serve")
            .env("DISCORD_TEST_SYNTHETIC_AUTHORITY", &authority)
            .env("DISCORD_MCP_SECRET_PROVIDER", "embedded")
            .env("DISCORD_MCP_LISTEN", addr.to_string())
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
            .stderr(Stdio::null());
        if tracked {
            cmd.env("DISCORD_MCP_ROTATION_OPERATION", hex::encode([11; 16]));
            cmd.env("DISCORD_MCP_ROTATION_INSTANCE", hex::encode([12; 16]));
        }
        Process(cmd.spawn().unwrap())
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let mut first = start(false);
    ready(&client, &origin).await;
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::SEE_OTHER
    );
    let (status, session) = login(&client, &origin, true).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let session = session.unwrap();
    mode.store(1, Ordering::SeqCst);
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        client
            .get(format!("{origin}/admin/channels"))
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert!(
        Command::new("kill")
            .args(["-TERM", &first.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(first.0.wait().unwrap().success());
    drop(first);
    let mut rotate = base()
        .args([
            "setup",
            "local",
            "vault",
            "rotate-oauth-tracked",
            &hex::encode([11; 16]),
            &hex::encode([12; 16]),
            "--confirm",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    rotate
        .stdin
        .take()
        .unwrap()
        .write_all(b"synthetic-old\nsynthetic-new\n")
        .unwrap();
    assert!(rotate.wait().unwrap().success());
    let second = start(true);
    ready(&client, &origin).await;
    assert_eq!(
        client
            .get(format!("{origin}/admin/channels"))
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let store = ChannelStore::open(&db, &key).unwrap();
    use discord_mcp::rotation_progress::Consumer;
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::Admin)
            .unwrap()
    );
    mode.store(2, Ordering::SeqCst);
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::FORBIDDEN);
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::Admin)
            .unwrap()
    );
    mode.store(1, Ordering::SeqCst);
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::SEE_OTHER
    );
    assert!(
        store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::Admin)
            .unwrap()
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::SEE_OTHER);
    assert!(
        store
            .rotation_login_observed([11; 16], [12; 16], Consumer::Admin)
            .unwrap()
    );
    assert_eq!(
        store
            .get_rotation_progress([11; 16])
            .unwrap()
            .unwrap()
            .1
            .publication(),
        discord_mcp::rotation_progress::Publication::Unknown
    );
    mode.store(2, Ordering::SeqCst);
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::FORBIDDEN);
    drop(second);
    let third = start(true);
    ready(&client, &origin).await;
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::Admin)
            .unwrap()
    );
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert!(
        !store
            .rotation_login_observed([11; 16], [12; 16], Consumer::OAuth)
            .unwrap()
    );
    drop(third);
    fixture.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_publication_requires_restart_and_failed_adoption_never_rolls_back() {
    let authority_stage = Arc::new(AtomicUsize::new(0));
    let provider_stage = Arc::new(AtomicUsize::new(0));
    let authority = Router::new()
        .route("/api/oauth2/token", post(token))
        .route("/api/v10/users/@me", get(identity))
        .with_state(authority_stage.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority_url = format!("http://{}/", listener.local_addr().unwrap());
    let authority_task =
        tokio::spawn(async move { axum::serve(listener, authority).await.unwrap() });
    let provider = Router::new()
        .route(
            "/api/v4/secrets/DISCORD_MCP_DISCORD_CLIENT_SECRET",
            get(infisical_fixture),
        )
        .with_state(provider_stage.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}", listener.local_addr().unwrap());
    let provider_task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let key = [98; 32];
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .put_bot("alerts", "123", "synthetic-channel-token")
        .unwrap();
    drop(store);
    let sock = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    drop(sock);
    let origin = format!("http://{addr}");
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        cmd.env_clear()
            .arg("serve")
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_TEST_SYNTHETIC_AUTHORITY", &authority_url)
            .env("DISCORD_MCP_SECRET_PROVIDER", "infisical")
            .env("DISCORD_MCP_INFISICAL_ORIGIN", &provider_url)
            .env("DISCORD_MCP_INFISICAL_PROJECT_ID", "fixture-project")
            .env("DISCORD_MCP_INFISICAL_ENVIRONMENT", "test")
            .env("DISCORD_MCP_INFISICAL_SECRET_PATH", "/discord")
            .env("DISCORD_MCP_INFISICAL_TOKEN", "synthetic-token")
            .env("DISCORD_MCP_LISTEN", addr.to_string())
            .env("DISCORD_MCP_PUBLIC_ORIGIN", &origin)
            .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
            .env("DISCORD_MCP_ADMIN_ID", "123")
            .env("DISCORD_MCP_CLIENT_ID", "fixture")
            .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
            .env(
                "DISCORD_MCP_CLIENT_REDIRECT",
                "http://127.0.0.1:8765/callback",
            );
        cmd
    };
    let start = || {
        Process(
            command()
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let first = start();
    ready(&client, &origin).await;
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::SEE_OTHER);
    // Publish to the external store; the running server still holds the old value.
    provider_stage.store(1, Ordering::SeqCst);
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::SEE_OTHER
    );
    authority_stage.store(1, Ordering::SeqCst);
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::FORBIDDEN);
    drop(first);
    // A failed provider read after publication cannot be treated as adoption or fall back.
    provider_stage.store(2, Ordering::SeqCst);
    let failed = command().output().unwrap();
    assert!(!failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("synthetic-token"));
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("synthetic-old"));
    provider_stage.store(1, Ordering::SeqCst);
    let second = start();
    ready(&client, &origin).await;
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::SEE_OTHER);
    authority_stage.store(2, Ordering::SeqCst);
    assert_eq!(
        login(&client, &origin, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(login(&client, &origin, true).await.0, StatusCode::FORBIDDEN);
    assert_eq!(provider_stage.load(Ordering::SeqCst), 1);
    assert!(
        ChannelStore::open(&db, &key)
            .unwrap()
            .get_channel("alerts")
            .is_ok()
    );
    drop(second);
    provider_task.abort();
    authority_task.abort();
}
