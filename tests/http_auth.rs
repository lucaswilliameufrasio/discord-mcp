use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Extension, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    routing::get,
};
use discord_mcp::{
    channels::{ChannelAction, ChannelStore},
    http_auth::{AuthState, AuthenticatedUser, authenticate},
};

async fn read_channel(
    State(state): State<AuthState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(channel): Path<String>,
) -> StatusCode {
    match user.authorized_channel(&state, &channel, ChannelAction::Read) {
        Ok(_) => StatusCode::OK,
        Err(discord_mcp::channels::StoreError::PermissionDenied) => StatusCode::FORBIDDEN,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[tokio::test]
async fn http_authenticates_each_request_and_enforces_channel_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.db");
    let key = [25; 32];
    let resource = "https://example.invalid/mcp";
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let store = ChannelStore::open(&path, &key).unwrap();
    store
        .put_bot("alpha", "123", "synthetic-bot-token")
        .unwrap();
    store.put_bot("beta", "456", "synthetic-bot-token").unwrap();
    store.set_permissions("1", "alpha", true, false).unwrap();
    let token = store.issue_access_token("1", resource, now, 60).unwrap();
    let other = store.issue_access_token("2", resource, now, 60).unwrap();
    let wrong_resource = store
        .issue_access_token("1", "https://other.invalid/mcp", now, 60)
        .unwrap();
    let state = AuthState {
        database_path: path,
        encryption_key: key,
        resource: resource.into(),
    };
    let router = Router::new()
        .route("/channels/{channel}", get(read_channel))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let alpha = format!("http://{address}/channels/alpha");
    let unauthenticated = client.get(&alpha).send().await.unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthenticated
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .unwrap(),
        "Bearer resource_metadata=\"https://example.invalid/.well-known/oauth-protected-resource\""
    );
    assert_eq!(
        client
            .get(&alpha)
            .bearer_auth(&wrong_resource)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&alpha)
            .bearer_auth(&other)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(&alpha)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("http://{address}/channels/beta"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    store.set_permissions("1", "alpha", false, false).unwrap();
    assert_eq!(
        client
            .get(&alpha)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    store.revoke_access_token(&token).unwrap();
    assert_eq!(
        client
            .get(&alpha)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    server.abort();
}
