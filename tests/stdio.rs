use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use rmcp::serde_json::{Value, json};

#[test]
fn initializes_over_stdio_without_advertising_discord_operations() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
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
            .contains("No Discord operations")
    );
    assert!(response["result"]["capabilities"].get("tools").is_none());
    drop(stdin);
    assert!(child.wait().expect("wait for server").success());
}
