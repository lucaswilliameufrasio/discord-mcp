use std::time::{SystemTime, UNIX_EPOCH};

use discord_mcp::{
    channels::ChannelStore, discord_login::DiscordLoginClient, http_auth::AuthState, http_mcp,
    oauth_web::OAuthWebConfig,
};
use rmcp::serde_json::{Value, json};

#[tokio::test]
async fn streamable_http_authenticates_each_call_and_enforces_user_channel_action() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mcp.db");
    let key = [51; 32];
    let store = ChannelStore::open(&db, &key).unwrap();
    store
        .put_bot("alpha", "123", "synthetic-bot-token")
        .unwrap();
    store.put_bot("beta", "456", "synthetic-bot-token").unwrap();
    store.set_permissions("1", "alpha", true, false).unwrap();
    store.set_permissions("2", "alpha", false, true).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let resource = format!("{origin}/mcp");
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let reader = store.issue_access_token("1", &resource, now, 60).unwrap();
    let sender = store.issue_access_token("2", &resource, now, 60).unwrap();
    let wrong_resource = store
        .issue_access_token("1", "https://other.invalid/mcp", now, 60)
        .unwrap();
    let config = OAuthWebConfig {
        auth: AuthState {
            database_path: db,
            encryption_key: key,
            resource: resource.clone(),
            metrics: discord_mcp::http_auth::MetricsState::default(),
            audit: discord_mcp::http_auth::AuditState::default(),
        },
        public_origin: origin.clone(),
        client_id: "client".into(),
        client_name: "Client".into(),
        redirect_uri: "http://127.0.0.1:8765/callback".into(),
        discord: DiscordLoginClient::new(
            "123",
            "synthetic-secret",
            &format!("{origin}/oauth/discord/callback"),
        )
        .unwrap(),
    };
    let server = tokio::spawn(async move {
        axum::serve(listener, http_mcp::router(config).unwrap())
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    let call = |token: &str, method: &str, args: Value| {
        client.post(&resource).bearer_auth(token).header("MCP-Protocol-Version", "2025-11-25").header(reqwest::header::ACCEPT, "application/json, text/event-stream").json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":method,"arguments":args}}))
    };
    let missing = client.post(&resource).send().await.unwrap();
    assert_eq!(missing.status(), 401);
    assert!(
        missing.headers()[reqwest::header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("oauth-protected-resource")
    );
    assert_eq!(
        call(
            &wrong_resource,
            "read_messages",
            json!({"channel":"alpha","limit":0})
        )
        .send()
        .await
        .unwrap()
        .status(),
        401
    );
    let metadata: Value = client
        .get(format!("{origin}/.well-known/oauth-protected-resource"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["resource"], resource);
    let hostile_origin = call(
        &reader,
        "read_messages",
        json!({"channel":"alpha","limit":0}),
    )
    .header(reqwest::header::ORIGIN, "http://evil.invalid")
    .send()
    .await
    .unwrap();
    assert_eq!(hostile_origin.status(), 403);
    let read: Value = call(
        &reader,
        "read_messages",
        json!({"channel":"alpha","limit":0}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(
        read["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("limit must be")
    );
    let denied: Value = call(
        &reader,
        "send_message",
        json!({"channel":"alpha","content":" ", "user_id":"2"}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(
        denied["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("access denied")
    );
    let denied: Value = call(
        &reader,
        "read_messages",
        json!({"channel":"beta","limit":0}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(
        denied["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("access denied")
    );
    let send: Value = call(
        &sender,
        "send_message",
        json!({"channel":"alpha","content":" "}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(
        send["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("message must contain")
    );
    let denied: Value = call(
        &sender,
        "read_messages",
        json!({"channel":"alpha","limit":0}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(
        denied["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("access denied")
    );
    store.set_permissions("1", "alpha", false, false).unwrap();
    let denied: Value = call(
        &reader,
        "read_messages",
        json!({"channel":"alpha","limit":0}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(
        denied["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("access denied")
    );
    store.revoke_access_token(&reader).unwrap();
    assert_eq!(
        call(
            &reader,
            "read_messages",
            json!({"channel":"alpha","limit":0})
        )
        .send()
        .await
        .unwrap()
        .status(),
        401
    );
    let summary = store
        .operational_summary(now.saturating_sub(60), now + 60)
        .unwrap();
    assert_eq!((summary.http_requests, summary.http_failures), (10, 4));
    assert_eq!((summary.send_completed, summary.send_failures), (2, 2));
    assert_eq!((summary.read_completed, summary.read_failures), (4, 4));
    server.abort();
}
