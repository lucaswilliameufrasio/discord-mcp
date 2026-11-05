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
            .contains("Local Discord webhook and bot channels")
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
    ChannelStore::open(&path, &key)
        .unwrap()
        .put_bot("robot", "123", "synthetic-token")
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
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_messages","arguments":{"channel":"alpha"}}})).unwrap();
    input.flush().unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let response: Value = rmcp::serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 3);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("do not support reading")
    );
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read_messages","arguments":{"channel":"robot","limit":0}}})).unwrap();
    input.flush().unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let response: Value = rmcp::serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 4);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("limit must be")
    );
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[tokio::test]
async fn multiple_stdio_instances_observe_shared_channel_changes() {
    use discord_mcp::channels::ChannelStore;
    use std::time::Duration;
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncReader},
        process::{Child, ChildStdin, ChildStdout, Command as AsyncCommand},
        time::timeout,
    };

    async fn request(
        input: &mut ChildStdin,
        output: &mut AsyncReader<ChildStdout>,
        value: Value,
    ) -> Value {
        input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
        let mut line = String::new();
        let count = timeout(Duration::from_secs(5), output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0, "stdio process exited before responding");
        rmcp::serde_json::from_str(&line).unwrap()
    }

    async fn start(
        path: &std::path::Path,
        key: &[u8; 32],
    ) -> (Child, ChildStdin, AsyncReader<ChildStdout>) {
        let mut child = AsyncCommand::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .env("DISCORD_MCP_DB", path)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = AsyncReader::new(child.stdout.take().unwrap());
        let response = request(
            &mut input,
            &mut output,
            json!({
                "jsonrpc":"2.0", "id":1, "method":"initialize",
                "params":{"protocolVersion":"2025-11-25", "capabilities":{},
                    "clientInfo":{"name":"multi-stdio-test", "version":"1"}}
            }),
        )
        .await;
        assert!(response.get("error").is_none());
        input
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        input.flush().await.unwrap();
        (child, input, output)
    }

    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("shared.db");
    let key = [51_u8; 32];
    let store = ChannelStore::open(&database, &key).unwrap();
    let (mut first, mut first_input, mut first_output) = start(&database, &key).await;
    let (mut second, mut second_input, mut second_output) = start(&database, &key).await;
    store.put_bot("shared", "123", "synthetic-token").unwrap();
    let call = json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{
        "name":"read_messages", "arguments":{"channel":"shared", "limit":0}
    }});
    let (one, two) = tokio::join!(
        request(&mut first_input, &mut first_output, call.clone()),
        request(&mut second_input, &mut second_output, call),
    );
    for response in [one, two] {
        assert_eq!(response["id"], 2);
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("limit must be")
        );
    }
    // Two independent administrative processes write while both stdio servers
    // remain live. SQLite serializes their separate transactions.
    let mut writers = Vec::new();
    for name in ["first", "second"] {
        let mut writer = AsyncCommand::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env_clear()
            .env("DISCORD_MCP_DB", &database)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .args(["add-channel", name])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(b"https://discord.com/api/webhooks/123/synthetic\n")
            .await
            .unwrap();
        writers.push(writer);
    }
    for writer in writers {
        let result = timeout(Duration::from_secs(8), writer.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.status.success(),
            "administrative writer failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    for (input, output) in [
        (&mut first_input, &mut first_output),
        (&mut second_input, &mut second_output),
    ] {
        for (id, name) in [(3, "first"), (4, "second")] {
            let response = request(
                input,
                output,
                json!({
                    "jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{
                        "name":"read_messages", "arguments":{"channel":name}
                    }
                }),
            )
            .await;
            assert_eq!(response["id"], id);
            assert!(
                response["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("do not support reading")
            );
        }
    }
    assert_eq!(store.list().unwrap(), vec!["first", "second", "shared"]);
    drop(first_input);
    drop(second_input);
    assert!(
        timeout(Duration::from_secs(5), first.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(
        timeout(Duration::from_secs(5), second.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
