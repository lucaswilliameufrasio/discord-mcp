#![cfg(target_os = "linux")]

use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    time::timeout,
};

fn command(db: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
    cmd.env_clear()
        .env("DISCORD_MCP_DB", db)
        .env("DISCORD_MCP_KEY", hex::encode([44_u8; 32]))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

async fn success(child: Child) -> bool {
    timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
        .status
        .success()
}

#[tokio::test]
async fn remote_service_and_local_processes_exclude_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("database.db");
    assert!(success(command(&db).arg("list-channels").spawn().unwrap()).await);
    let mut server = command(&db)
        .env("DISCORD_MCP_LISTEN", "127.0.0.1:0")
        .env("DISCORD_MCP_PUBLIC_ORIGIN", "https://example.invalid")
        .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
        .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "synthetic-secret")
        .env("DISCORD_MCP_ADMIN_ID", "123")
        .env("DISCORD_MCP_CLIENT_ID", "client")
        .env("DISCORD_MCP_CLIENT_NAME", "synthetic")
        .env(
            "DISCORD_MCP_CLIENT_REDIRECT",
            "http://127.0.0.1:8765/callback",
        )
        .arg("serve")
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(server.stderr.take().unwrap());
    let mut line = String::new();
    timeout(Duration::from_secs(10), stderr.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(line.contains("MCP HTTP listening"), "{line}");
    // Advisory locks do not stop SQLite clients that ignore this protocol.
    let outsider = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(
        outsider
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(outsider);
    assert!(!success(command(&db).arg("list-channels").spawn().unwrap()).await);
    assert!(!success(command(&db).arg("web").spawn().unwrap()).await);
    assert!(!success(command(&db).arg("serve").spawn().unwrap()).await);
    assert!(
        !success(
            command(&db)
                .args([
                    "recovery-prepare",
                    db.to_str().unwrap(),
                    dir.path().join("copy.db").to_str().unwrap()
                ])
                .spawn()
                .unwrap()
        )
        .await
    );
    assert!(!success(command(&db).stdin(Stdio::piped()).spawn().unwrap()).await);
    server.kill().await.unwrap();
    assert!(success(command(&db).arg("list-channels").spawn().unwrap()).await);

    let mut first = command(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut second = command(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    for child in [&mut first, &mut second] {
        let input = child.stdin.as_mut().unwrap();
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n").await.unwrap();
        input.flush().await.unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut response = String::new();
        timeout(Duration::from_secs(10), output.read_line(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.contains("\"result\""), "{response}");
    }
    assert!(!success(command(&db).arg("serve").spawn().unwrap()).await);
    drop(first.stdin.take());
    drop(second.stdin.take());
    assert!(
        timeout(Duration::from_secs(10), first.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(
        timeout(Duration::from_secs(10), second.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
