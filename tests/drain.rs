#![cfg(unix)]

use std::{
    net::SocketAddr,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use discord_mcp::channels::ChannelStore;
use rmcp::serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    process::{Child, ChildStderr, Command},
    time::{Instant, sleep, timeout},
};

struct Fixture {
    _directory: tempfile::TempDir,
    child: Child,
    stderr: BufReader<ChildStderr>,
    address: SocketAddr,
    token: String,
}

impl Fixture {
    async fn start() -> Self {
        Self::start_with_supervisor(false).await
    }

    async fn start_with_supervisor(supervised: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db = directory.path().join("drain.db");
        let key = [121; 32];
        let store = ChannelStore::open(&db, &key).unwrap();
        store.put_bot("alerts", "123", "synthetic-token").unwrap();
        store.set_permissions("11", "alerts", false, true).unwrap();
        let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let origin = format!("http://{address}");
        let resource = format!("{origin}/mcp");
        let now = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let token = store.issue_access_token("11", &resource, now, 60).unwrap();
        drop(store);
        let mut child = if supervised {
            let script = include_str!("../drills/supervisor_serve_fixture.py");
            let mut command = Command::new("/usr/bin/python3");
            command.args(["-I", "-c", script, env!("CARGO_BIN_EXE_discord-mcp")]);
            command
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
            command.arg("serve");
            command
        };
        child
            .env_clear()
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .env("DISCORD_MCP_LISTEN", address.to_string())
            .env("DISCORD_MCP_PUBLIC_ORIGIN", origin)
            .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
            .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "synthetic-secret")
            .env("DISCORD_MCP_ADMIN_ID", "123")
            .env("DISCORD_MCP_CLIENT_ID", "synthetic-client")
            .env("DISCORD_MCP_CLIENT_NAME", "Test")
            .env(
                "DISCORD_MCP_CLIENT_REDIRECT",
                "http://127.0.0.1:8765/callback",
            )
            .env_remove("DISCORD_MCP_METRICS_RETENTION_DAYS")
            .env_remove("DISCORD_MCP_AUDIT_RETENTION_DAYS")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = child.spawn().unwrap();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        timeout(Duration::from_secs(5), stderr.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(line.contains("loopback only"));
        Self {
            _directory: directory,
            child,
            stderr,
            address,
            token,
        }
    }

    async fn begin_request(&self) -> (OwnedWriteHalf, BufReader<OwnedReadHalf>, Vec<u8>) {
        let stream = TcpStream::connect(self.address).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_message","arguments":{"channel":"alerts","content":" "}}}).to_string().into_bytes();
        let header = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2025-11-25\r\nContent-Length: {}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n",
            self.address,
            self.token,
            body.len()
        );
        writer.write_all(header.as_bytes()).await.unwrap();
        let mut line = String::new();
        timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(line.starts_with("HTTP/1.1 100"));
        line.clear();
        timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line, "\r\n");
        let split = body.len() / 2;
        writer.write_all(&body[..split]).await.unwrap();
        writer.flush().await.unwrap();
        (writer, reader, body[split..].to_vec())
    }

    async fn terminate(&mut self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let mut line = String::new();
        timeout(Duration::from_secs(5), self.stderr.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(line.contains("draining in-flight requests"));
    }

    async fn wait_until_listener_closed(&self) {
        timeout(Duration::from_secs(3), async {
            loop {
                if TcpStream::connect(self.address).await.is_err() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("listener did not close during drain");
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn fixture_supervisor_forwards_sigterm_and_real_serve_drains() {
    let mut fixture = Fixture::start_with_supervisor(true).await;
    let (mut writer, mut reader, rest) = fixture.begin_request().await;
    fixture.terminate().await;
    fixture.wait_until_listener_closed().await;
    writer.write_all(&rest).await.unwrap();
    writer.flush().await.unwrap();
    let mut response = String::new();
    timeout(Duration::from_secs(5), reader.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("message must contain"));
    assert!(
        timeout(Duration::from_secs(5), fixture.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn fixture_supervisor_sigkill_closes_real_serve_listener() {
    let mut fixture = Fixture::start_with_supervisor(true).await;
    // Parent death is abrupt: an in-flight request is not promised to finish.
    let (_writer, _reader, _rest) = fixture.begin_request().await;
    fixture.child.kill().await.unwrap();
    assert!(!fixture.child.wait().await.unwrap().success());
    fixture.wait_until_listener_closed().await;
}

#[tokio::test]
async fn sigterm_drains_a_request_already_being_received() {
    let mut fixture = Fixture::start().await;
    let (mut writer, mut reader, rest) = fixture.begin_request().await;
    fixture.terminate().await;
    fixture.wait_until_listener_closed().await;
    writer.write_all(&rest).await.unwrap();
    writer.flush().await.unwrap();
    let mut response = String::new();
    timeout(Duration::from_secs(5), reader.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("message must contain"));
    assert!(
        timeout(Duration::from_secs(5), fixture.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn sigterm_stops_waiting_after_bounded_drain_deadline() {
    let mut fixture = Fixture::start().await;
    let (_writer, _reader, _rest) = fixture.begin_request().await;
    let started = Instant::now();
    fixture.terminate().await;
    fixture.wait_until_listener_closed().await;
    let status = timeout(Duration::from_secs(19), fixture.child.wait())
        .await
        .expect("server exceeded drain deadline")
        .unwrap();
    assert!(!status.success());
    assert!(started.elapsed() >= Duration::from_secs(14));
    let mut output = String::new();
    fixture.stderr.read_to_string(&mut output).await.unwrap();
    assert!(output.contains("shutdown timed out"));
}
