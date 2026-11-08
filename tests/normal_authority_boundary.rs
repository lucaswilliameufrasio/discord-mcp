#![cfg(not(discord_mcp_synthetic_authority))]

#[test]
fn normal_server_binary_contains_no_synthetic_authority_configuration() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_discord-mcp")).unwrap();
    for marker in [
        b"DISCORD_TEST_SYNTHETIC_AUTHORITY".as_slice(),
        b"DISCORD_TEST_SUPERVISED_EVENTS".as_slice(),
        b"DISCORD_TEST_SUPERVISOR_EPOCH".as_slice(),
    ] {
        assert!(!binary.windows(marker.len()).any(|window| window == marker));
    }
    let discord = b"https://discord.com/";
    assert!(
        binary
            .windows(discord.len())
            .any(|window| window == discord)
    );
}

/// Must run in a newly created network namespace, not merely on a loopback URL.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires exclusive loopback-only network namespace"]
async fn normal_server_ignores_synthetic_authority_at_runtime() {
    use axum::{Router, extract::State, http::StatusCode, routing::any};
    use discord_mcp::channels::ChannelStore;
    use std::{
        net::TcpListener,
        process::{Child, Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    async fn count(State(counter): State<Arc<AtomicUsize>>) -> StatusCode {
        counter.fetch_add(1, Ordering::SeqCst);
        StatusCode::INTERNAL_SERVER_ERROR
    }
    struct Cleanup(Child);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let devices = std::fs::read_to_string("/proc/net/dev").unwrap();
    let interfaces: Vec<_> = devices
        .lines()
        .skip(2)
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim()))
        .collect();
    assert_eq!(
        interfaces,
        ["lo"],
        "a dedicated loopback-only network namespace is required"
    );
    let count_requests = Arc::new(AtomicUsize::new(0));
    let fixture = Router::new()
        .fallback(any(count))
        .with_state(count_requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_origin = format!("http://{}/", listener.local_addr().unwrap());
    let fixture_task = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("server.db");
    let key = [97; 32];
    drop(ChannelStore::open(&db, &key).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let origin = format!("http://{addr}");
    let server = Cleanup(
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .arg("serve")
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_MCP_LISTEN", addr.to_string())
            .env("DISCORD_MCP_PUBLIC_ORIGIN", &origin)
            .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
            .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "synthetic-old")
            .env("DISCORD_MCP_ADMIN_ID", "123")
            .env("DISCORD_MCP_CLIENT_ID", "fixture")
            .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
            .env(
                "DISCORD_MCP_CLIENT_REDIRECT",
                "http://127.0.0.1:8765/callback",
            )
            .env("DISCORD_TEST_SYNTHETIC_AUTHORITY", &fixture_origin)
            .env("DISCORD_TEST_SUPERVISED_EVENTS", "1")
            .env("DISCORD_TEST_SUPERVISOR_EPOCH", "invalid-test-value")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    assert_eq!(
        client.get(&fixture_origin).send().await.unwrap().status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(count_requests.swap(0, Ordering::SeqCst), 1);
    let mut ready = false;
    for _ in 0..100 {
        if client.get(format!("{origin}/admin")).send().await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ready);
    let response = client
        .get(format!("{origin}/admin/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let target = reqwest::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(target.host_str(), Some("discord.com"));
    let state = target
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
    let response = client
        .get(callback)
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(count_requests.load(Ordering::SeqCst), 0);
    drop(server);
    fixture_task.abort();
}
