//! Authentication building blocks for the future HTTP MCP transport.
//! No network listener or OAuth endpoints are enabled by this module.
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::channels::{Channel, ChannelAction, ChannelStore, StoreError, valid_resource};

#[derive(Clone)]
pub struct AuthState {
    pub database_path: PathBuf,
    pub encryption_key: [u8; 32],
    /// The fixed resource identifier configured by the server operator.
    pub resource: String,
}

#[derive(Clone)]
pub struct AuthenticatedUser {
    user_id: String,
}

impl AuthenticatedUser {
    /// Checks grants against the identity authenticated by the middleware.
    pub fn authorized_channel(
        &self,
        state: &AuthState,
        channel: &str,
        action: ChannelAction,
    ) -> Result<Channel, StoreError> {
        ChannelStore::open(&state.database_path, &state.encryption_key)?.authorized_channel(
            &self.user_id,
            channel,
            action,
        )
    }
}

pub async fn authenticate(
    State(state): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    if request
        .headers()
        .get_all(header::AUTHORIZATION)
        .iter()
        .count()
        != 1
    {
        return unauthorized(&state.resource);
    }
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .and_then(|(scheme, token)| {
            if scheme.eq_ignore_ascii_case("bearer") {
                Some(token)
            } else {
                None
            }
        });
    let Some(token) = token else {
        return unauthorized(&state.resource);
    };
    let now = match SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok())
    {
        Some(now) => now,
        None => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let store = match ChannelStore::open(&state.database_path, &state.encryption_key) {
        Ok(store) => store,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let user_id = match store.verify_access_token(token, &state.resource, now) {
        Ok(Some(user_id)) => user_id,
        Ok(None) => return unauthorized(&state.resource),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    drop(store);
    request
        .extensions_mut()
        .insert(AuthenticatedUser { user_id });
    next.run(request).await
}

fn unauthorized(resource: &str) -> Response {
    if !valid_resource(resource) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let Ok(url) = reqwest::Url::parse(resource) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let metadata = format!(
        "{}/.well-known/oauth-protected-resource",
        url.origin().ascii_serialization()
    );
    let challenge = format!("Bearer resource_metadata=\"{metadata}\"");
    let Ok(challenge) = axum::http::HeaderValue::from_str(&challenge) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, challenge);
    response
}
