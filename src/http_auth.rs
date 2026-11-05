//! Authentication building blocks for the future HTTP MCP transport.
//! No network listener or OAuth endpoints are enabled by this module.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::channels::{
    AuditActor, Channel, ChannelAction, ChannelStore, LoginKind, OperationalEvent, StoreError,
    current_timestamp, valid_resource,
};

#[derive(Clone)]
pub struct AuditState {
    retention_days: u16,
    degraded: Arc<AtomicBool>,
}

impl Default for AuditState {
    fn default() -> Self {
        Self {
            retention_days: 90,
            degraded: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl AuditState {
    pub fn new(retention_days: u16) -> Result<Self, &'static str> {
        if !(1..=3650).contains(&retention_days) {
            return Err("audit retention must be between 1 and 3650 days");
        }
        Ok(Self {
            retention_days,
            degraded: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn retention_days(&self) -> u16 {
        self.retention_days
    }
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }
    pub fn mark_failure(&self) {
        self.degraded.store(true, Ordering::Relaxed);
    }
    pub fn record_login(
        &self,
        auth: &AuthState,
        kind: LoginKind,
        actor: AuditActor<'_>,
        success: bool,
    ) -> Result<(), StoreError> {
        let result = current_timestamp().and_then(|now| {
            ChannelStore::open(&auth.database_path, &auth.encryption_key)?.record_discord_login(
                kind,
                actor,
                success,
                now,
                self.retention_days,
            )
        });
        if result.is_err() {
            self.degraded.store(true, Ordering::Relaxed);
        }
        result
    }
}

#[derive(Clone)]
pub struct MetricsState {
    retention_days: u16,
    degraded: Arc<AtomicBool>,
}

impl Default for MetricsState {
    fn default() -> Self {
        Self {
            retention_days: 30,
            degraded: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl MetricsState {
    pub fn new(retention_days: u16) -> Result<Self, &'static str> {
        if !(1..=365).contains(&retention_days) {
            return Err("metrics retention must be between 1 and 365 days");
        }
        Ok(Self {
            retention_days,
            degraded: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    pub fn record(&self, auth: &AuthState, event: OperationalEvent) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|v| i64::try_from(v.as_secs()).ok());
        let result =
            now.ok_or(StoreError::Storage).and_then(|now| {
                ChannelStore::open(&auth.database_path, &auth.encryption_key)?
                    .record_operational_event(now, self.retention_days, event)
            });
        if result.is_err() {
            self.degraded.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
pub struct AuthState {
    pub database_path: PathBuf,
    pub encryption_key: [u8; 32],
    /// The fixed resource identifier configured by the server operator.
    pub resource: String,
    pub metrics: MetricsState,
    pub audit: AuditState,
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
    request: Request,
    next: Next,
) -> Response {
    let start = Instant::now();
    let response = authenticate_inner(&state, request, next).await;
    let failed = response.status().is_client_error() || response.status().is_server_error();
    let response_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    state.metrics.record(
        &state,
        OperationalEvent::Http {
            failed,
            response_ms,
        },
    );
    response
}

async fn authenticate_inner(state: &AuthState, mut request: Request, next: Next) -> Response {
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
