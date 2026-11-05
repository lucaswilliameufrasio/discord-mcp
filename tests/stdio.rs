use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use rmcp::serde_json::{Value, json};

#[test]
fn initializes_over_stdio_and_advertises_local_webhook_scope() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_remove("DISCORD_WEBHOOK_URL")
        .env_remove("DISCORD_MCP_DB")
        .env_remove("DISCORD_MCP_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start server");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"}}
        })
    )
    .expect("write initialize");
    stdin.flush().expect("flush");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read response");
    let response: Value = rmcp::serde_json::from_str(&line).expect("valid JSON-RPC");
    assert_eq!(response["id"], 1);
    assert!(response.get("error").is_none());
    assert!(
        response["result"]["instructions"]
            .as_str()
            .expect("instructions")
            .contains("Local webhook sending only")
    );
    assert!(response["result"]["capabilities"].get("tools").is_some());
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0", "method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(stdin, "{}", json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"send_message", "arguments":{"content":"hello"}}})).unwrap();
    stdin.flush().unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let response: Value = rmcp::serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"]["isError"], true);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No webhook is configured")
    );
    drop(stdin);
    assert!(child.wait().expect("wait for server").success());
}

#[test]
fn registered_channel_is_resolved_by_mcp() {
    use discord_mcp::channels::ChannelStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("channels.db");
    let key = [5_u8; 32];
    ChannelStore::open(&path, &key)
        .unwrap()
        .put_webhook("alpha", "https://discord.com/api/webhooks/123/synthetic")
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env_remove("DISCORD_WEBHOOK_URL")
        .env("DISCORD_MCP_DB", &path)
        .env("DISCORD_MCP_KEY", hex::encode(key))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"send_message","arguments":{"channel":"alpha","content":" "}}})).unwrap();
    input.flush().unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let response: Value = rmcp::serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"]["isError"], true);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("message must contain")
    );
    drop(input);
    assert!(child.wait().unwrap().success());
}
