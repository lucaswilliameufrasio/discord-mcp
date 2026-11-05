//! Synthetic process-level investigation, not an application write-gate.
#![cfg(unix)]

use std::{
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use discord_mcp::channels::{ChannelAction, ChannelStore};
use rmcp::serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::{sleep, timeout},
};

#[test]
fn delete_worker() {
    let Some(path) = std::env::var_os("DELETE_FIXTURE_DB") else {
        return;
    };
    let store = ChannelStore::open(std::path::Path::new(&path), &[184; 32]).unwrap();
    eprintln!("READY");
    let mut command = String::new();
    std::io::stdin().read_line(&mut command).unwrap();
    assert_eq!(command.trim(), "delete");
    eprintln!("DELETING");
    store.delete("support").unwrap();
}

#[tokio::test]
async fn deletion_waits_for_writer_and_preserves_audit_and_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("delete.db");
    let store = ChannelStore::open(&database, &[184; 32]).unwrap();
    store.put_bot("support", "123", "synthetic-token").unwrap();
    store
        .set_permissions("101", "support", true, false)
        .unwrap();
    let mut worker = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .env("DELETE_FIXTURE_DB", &database)
        .args(["--exact", "delete_worker", "--nocapture"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(worker.stderr.take().unwrap());
    let mut marker = String::new();
    timeout(Duration::from_secs(5), stderr.read_line(&mut marker))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(marker.trim(), "READY");
    let blocker = rusqlite::Connection::open(&database).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    worker
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"delete\n")
        .await
        .unwrap();
    marker.clear();
    timeout(Duration::from_secs(5), stderr.read_line(&mut marker))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(marker.trim(), "DELETING");
    assert!(
        timeout(Duration::from_millis(300), worker.wait())
            .await
            .is_err(),
        "delete failed instead of waiting for the concurrent writer"
    );
    assert!(
        store
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    blocker.execute_batch("COMMIT").unwrap();
    assert!(
        timeout(Duration::from_secs(6), worker.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!store.list().unwrap().contains(&"support".to_owned()));
    let grants: i64 = blocker
        .query_row("SELECT COUNT(*) FROM channel_permissions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(grants, 0);
    assert_eq!(
        store
            .audit_events(None, Some("channel_grants_revoked"), 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .audit_events(None, Some("channel_deleted"), 10)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn sqlite_writer_reservation_blocks_cli_until_released() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("coordination.db");
    let key = [181u8; 32];
    let store = ChannelStore::open(&db, &key).unwrap();
    store.put_bot("support", "123", "synthetic-token").unwrap();
    let lock = rusqlite::Connection::open(&db).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args(["set-permission", "101", "support", "read"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(400)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "CLI exited while the SQLite write reservation was held"
    );
    let reader = rusqlite::Connection::open(&db).unwrap();
    let grants: i64 = reader
        .query_row("SELECT COUNT(*) FROM channel_permissions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        grants, 0,
        "WAL reader must see the unchanged committed state"
    );
    let competitor = rusqlite::Connection::open(&db).unwrap();
    competitor.busy_timeout(Duration::ZERO).unwrap();
    assert!(competitor.execute_batch("BEGIN IMMEDIATE").is_err());
    lock.execute_batch("ROLLBACK").unwrap();
    let result = timeout(Duration::from_secs(8), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "CLI failed after reservation release"
    );
    assert!(
        store
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert_eq!(
        store
            .audit_events(None, Some("permission_change"), 10)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn prolonged_writer_reservation_makes_cli_fail_without_applying_permission() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("coordination.db");
    let key = [183u8; 32];
    let store = ChannelStore::open(&db, &key).unwrap();
    store.put_bot("support", "123", "synthetic-token").unwrap();
    let lock = rusqlite::Connection::open(&db).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .args(["set-permission", "101", "support", "read"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let result = timeout(Duration::from_secs(9), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!result.status.success());
    lock.execute_batch("ROLLBACK").unwrap();
    assert!(
        !store
            .is_permitted("101", "support", ChannelAction::Read)
            .unwrap()
    );
    assert!(
        store
            .audit_events(None, Some("permission_change"), 10)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn sqlite_writer_reservation_delays_server_request_until_released() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("coordination.db");
    let key = [182u8; 32];
    let store = ChannelStore::open(&db, &key).unwrap();
    store.put_bot("support", "123", "synthetic-token").unwrap();
    store
        .set_permissions("101", "support", true, false)
        .unwrap();
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    let token = store.issue_access_token("101", &resource, now, 60).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_clear()
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .env("DISCORD_MCP_LISTEN", address.to_string())
        .env("DISCORD_MCP_PUBLIC_ORIGIN", &origin)
        .env("DISCORD_MCP_DISCORD_CLIENT_ID", "123")
        .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "synthetic-secret")
        .env("DISCORD_MCP_ADMIN_ID", "123")
        .env("DISCORD_MCP_CLIENT_ID", "fixture")
        .env("DISCORD_MCP_CLIENT_NAME", "Fixture")
        .env(
            "DISCORD_MCP_CLIENT_REDIRECT",
            "http://127.0.0.1:8765/callback",
        )
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut ready = String::new();
    timeout(Duration::from_secs(8), stderr.read_line(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert!(ready.contains("loopback only"));
    let lock = rusqlite::Connection::open(&db).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(12))
        .build()
        .unwrap();
    let task = tokio::spawn(async move {
        client.post(resource).bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_messages","arguments":{"channel":"support","limit":0}}}))
            .send().await.unwrap().json::<Value>().await.unwrap()
    });
    sleep(Duration::from_millis(400)).await;
    assert!(
        !task.is_finished(),
        "server request completed before release"
    );
    lock.execute_batch("ROLLBACK").unwrap();
    let response = timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("limit must be")
    );
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let metrics = store
        .operational_summary(now.saturating_sub(60), now + 60)
        .unwrap();
    assert_eq!(metrics.http_requests, 1);
    assert_eq!(metrics.read_completed, 1);
    let result = Command::new("kill")
        .args(["-TERM", &child.id().unwrap().to_string()])
        .status()
        .await
        .unwrap();
    assert!(result.success());
    assert!(
        timeout(Duration::from_secs(8), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
