//! Browser-side Discord login and consent controller. Not exposed by a listener yet.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Form, Json, Router,
    extract::{DefaultBodyLimit, Query, State, rejection::FormRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use reqwest::Url;
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::{
    channels::{
        AuthorizationExchange, AuthorizationGrant, ChannelStore, StoreError, valid_challenge,
        valid_redirect, valid_resource,
    },
    discord_login::{DiscordLoginClient, PendingDiscordLogin},
    http_auth::AuthState,
};

const MAX_PENDING: usize = 1024;

#[derive(Clone)]
pub struct OAuthWebConfig {
    pub auth: AuthState,
    pub public_origin: String,
    pub client_id: String,
    pub client_name: String,
    pub redirect_uri: String,
    pub discord: DiscordLoginClient,
}

struct AuthRequest {
    client_state: String,
    challenge: String,
}

struct Attempt {
    pending: PendingDiscordLogin,
    request: AuthRequest,
    expires: Instant,
}

struct Consent {
    user_id: String,
    username: String,
    request: AuthRequest,
    expires: Instant,
}

struct Inner {
    config: OAuthWebConfig,
    attempts: Mutex<HashMap<String, Attempt>>,
    consents: Mutex<HashMap<String, Consent>>,
}

type Shared = Arc<Inner>;

/// Pre-register one client; no automatic or untrusted redirect registration.
/// The eventual public listener must use TLS except for loopback development.
pub fn router(config: OAuthWebConfig) -> Result<Router, &'static str> {
    let redirect_has_reserved_query = Url::parse(&config.redirect_uri)
        .map(|url| {
            url.query_pairs().any(|(key, _)| {
                matches!(
                    key.as_ref(),
                    "code" | "state" | "error" | "error_description" | "error_uri"
                )
            })
        })
        .unwrap_or(true);
    if !valid_resource(&config.auth.resource)
        || !valid_redirect(&config.redirect_uri)
        || redirect_has_reserved_query
        || config.client_id.is_empty()
        || config.client_id.len() > 512
        || !config.client_id.bytes().all(|b| b.is_ascii_graphic())
        || config.client_name.trim().is_empty()
        || config.client_name.len() > 100
        || !valid_origin(&config.public_origin)
        || config.discord.redirect_uri().as_str()
            != format!("{}/oauth/discord/callback", config.public_origin)
        || Url::parse(&config.auth.resource)
            .ok()
            .is_none_or(|url| url.origin().ascii_serialization() != config.public_origin)
    {
        return Err("invalid OAuth configuration");
    }
    let state = Arc::new(Inner {
        config,
        attempts: Mutex::new(HashMap::new()),
        consents: Mutex::new(HashMap::new()),
    });
    Ok(Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_metadata),
        )
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .route("/oauth/discord/callback", get(callback))
        .route("/oauth/consent", get(consent_page).post(consent_submit))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(state))
}

async fn resource_metadata(State(app): State<Shared>) -> Response {
    json_response(
        StatusCode::OK,
        rmcp::serde_json::json!({
            "resource": app.config.auth.resource,
            "authorization_servers": [app.config.public_origin],
            "bearer_methods_supported": ["header"]
        }),
    )
}

async fn authorization_metadata(State(app): State<Shared>) -> Response {
    let origin = &app.config.public_origin;
    json_response(
        StatusCode::OK,
        rmcp::serde_json::json!({
            "issuer": origin,
            "authorization_endpoint": format!("{origin}/oauth/authorize"),
            "token_endpoint": format!("{origin}/oauth/token"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code"],
            "token_endpoint_auth_methods_supported": ["none"],
            "code_challenge_methods_supported": ["S256"]
        }),
    )
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    client_id: String,
    code: String,
    redirect_uri: String,
    resource: String,
    code_verifier: String,
    client_secret: Option<String>,
}

async fn token(
    State(app): State<Shared>,
    headers: HeaderMap,
    form: Result<Form<TokenRequest>, FormRejection>,
) -> Response {
    let Form(req) = match form {
        Ok(form) => form,
        Err(_) => return oauth_error("invalid_request"),
    };
    if headers.contains_key(header::AUTHORIZATION)
        || req.client_secret.is_some()
        || req.client_id != app.config.client_id
    {
        return oauth_error("invalid_client");
    }
    if req.grant_type != "authorization_code" {
        return oauth_error("unsupported_grant_type");
    }
    if req.redirect_uri != app.config.redirect_uri || req.resource != app.config.auth.resource {
        return oauth_error("invalid_grant");
    }
    let now = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_secs()).ok())
    {
        Some(value) => value,
        None => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let store = match ChannelStore::open(
        &app.config.auth.database_path,
        &app.config.auth.encryption_key,
    ) {
        Ok(store) => store,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let exchange = AuthorizationExchange {
        code: &req.code,
        client_id: &req.client_id,
        redirect_uri: &req.redirect_uri,
        resource: &req.resource,
        code_verifier: &req.code_verifier,
    };
    match store.exchange_authorization_code(&exchange, now) {
        Ok(access_token) => json_response(
            StatusCode::OK,
            rmcp::serde_json::json!({"access_token":access_token,"token_type":"Bearer","expires_in":3600}),
        ),
        Err(StoreError::AuthorizationDenied | StoreError::InvalidInput) => {
            oauth_error("invalid_grant")
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn oauth_error(code: &'static str) -> Response {
    json_response(
        StatusCode::BAD_REQUEST,
        rmcp::serde_json::json!({"error":code}),
    )
}

fn json_response(status: StatusCode, body: rmcp::serde_json::Value) -> Response {
    let mut response = (status, Json(body)).into_response();
    secure_headers(&mut response);
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn valid_origin(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let loopback = url.scheme() == "http"
        && matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]")
        );
    (url.scheme() == "https" || loopback)
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.origin().ascii_serialization() == value
}

#[derive(Deserialize)]
struct AuthorizationRequest {
    client_id: String,
    redirect_uri: String,
    response_type: String,
    code_challenge: String,
    code_challenge_method: String,
    resource: String,
    state: String,
}

async fn authorize(State(app): State<Shared>, Query(req): Query<AuthorizationRequest>) -> Response {
    if req.client_id != app.config.client_id
        || req.redirect_uri != app.config.redirect_uri
        || req.response_type != "code"
        || req.code_challenge_method != "S256"
        || req.resource != app.config.auth.resource
        || !valid_challenge(&req.code_challenge)
        || req.state.is_empty()
        || req.state.len() > 512
        || req.state.chars().any(char::is_control)
    {
        return error(StatusCode::BAD_REQUEST);
    }
    let (pending, discord_url) = match app.config.discord.begin() {
        Ok(result) => result,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let state = match discord_url.query_pairs().find(|(key, _)| key == "state") {
        Some((_, value)) => value.into_owned(),
        None => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let binding = pending.browser_binding().to_owned();
    let mut attempts = match app.attempts.lock() {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    attempts.retain(|_, v| v.expires > Instant::now());
    if attempts.len() >= MAX_PENDING {
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    attempts.insert(
        state,
        Attempt {
            pending,
            request: AuthRequest {
                client_state: req.state,
                challenge: req.code_challenge,
            },
            expires: Instant::now() + Duration::from_secs(300),
        },
    );
    drop(attempts);
    let mut response = redirect(discord_url.as_str());
    let secure = if app.config.public_origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "discord_mcp_oauth={binding}; Path=/oauth; HttpOnly; SameSite=Lax; Max-Age=300{secure}"
    );
    match HeaderValue::from_str(&cookie) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    }
    response
}

#[derive(Deserialize)]
struct DiscordCallback {
    state: String,
    code: String,
    iss: String,
}

async fn callback(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<DiscordCallback>,
) -> Response {
    let binding = match browser_cookie(&headers) {
        Some(binding) => binding,
        None => return error(StatusCode::FORBIDDEN),
    };
    let attempt = match app.attempts.lock() {
        Ok(mut attempts) => {
            let correct_browser = attempts.get(&query.state).is_some_and(|attempt| {
                bool::from(
                    attempt
                        .pending
                        .browser_binding()
                        .as_bytes()
                        .ct_eq(binding.as_bytes()),
                )
            });
            if !correct_browser {
                return error(StatusCode::FORBIDDEN);
            }
            attempts.remove(&query.state)
        }
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let Some(attempt) = attempt else {
        return error(StatusCode::FORBIDDEN);
    };
    if Instant::now() >= attempt.expires {
        return error(StatusCode::FORBIDDEN);
    }
    let identity = match app
        .config
        .discord
        .finish(
            attempt.pending,
            &query.state,
            &binding,
            &query.iss,
            &query.code,
        )
        .await
    {
        Ok(identity) => identity,
        Err(_) => return error(StatusCode::FORBIDDEN),
    };
    let mut consents = match app.consents.lock() {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    consents.retain(|_, value| value.expires > Instant::now());
    if consents.len() >= MAX_PENDING {
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    consents.insert(
        binding,
        Consent {
            user_id: identity.id,
            username: identity.username,
            request: attempt.request,
            expires: Instant::now() + Duration::from_secs(300),
        },
    );
    redirect("/oauth/consent")
}

async fn consent_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(binding) = browser_cookie(&headers) else {
        return error(StatusCode::FORBIDDEN);
    };
    let consents = match app.consents.lock() {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let Some(consent) = consents.get(&binding) else {
        return error(StatusCode::FORBIDDEN);
    };
    if Instant::now() >= consent.expires {
        return error(StatusCode::FORBIDDEN);
    }
    let body = format!(
        "<!doctype html><html lang=\"pt-BR\"><meta charset=\"utf-8\"><title>Autorizar cliente MCP</title><main><h1>Autorizar acesso</h1><p>Conta Discord: {}</p><p>Cliente: {}</p><p>Destino do retorno: {}</p><p>O acesso a cada canal continua sujeito às permissões concedidas pelo administrador.</p><form method=\"post\" action=\"/oauth/consent\"><button name=\"decision\" value=\"allow\">Autorizar</button><button name=\"decision\" value=\"deny\">Negar</button></form></main></html>",
        escape(&consent.username),
        escape(&app.config.client_name),
        escape(&app.config.redirect_uri)
    );
    let mut response = Html(body).into_response();
    secure_headers(&mut response);
    response
}

#[derive(Deserialize)]
struct ConsentForm {
    decision: String,
}

async fn consent_submit(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<ConsentForm>,
) -> Response {
    if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        != Some(app.config.public_origin.as_str())
        || !matches!(form.decision.as_str(), "allow" | "deny")
    {
        return error(StatusCode::FORBIDDEN);
    }
    let Some(binding) = browser_cookie(&headers) else {
        return error(StatusCode::FORBIDDEN);
    };
    let consent = match app.consents.lock() {
        Ok(mut consents) => consents.remove(&binding),
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let Some(consent) = consent else {
        return error(StatusCode::FORBIDDEN);
    };
    if Instant::now() >= consent.expires {
        return error(StatusCode::FORBIDDEN);
    }
    let mut redirect_uri = match Url::parse(&app.config.redirect_uri) {
        Ok(uri) => uri,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    if form.decision == "deny" {
        redirect_uri
            .query_pairs_mut()
            .append_pair("error", "access_denied")
            .append_pair("state", &consent.request.client_state);
        return redirect(redirect_uri.as_str());
    }
    let now = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_secs()).ok())
    {
        Some(value) => value,
        None => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let store = match ChannelStore::open(
        &app.config.auth.database_path,
        &app.config.auth.encryption_key,
    ) {
        Ok(store) => store,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let grant = AuthorizationGrant {
        user_id: &consent.user_id,
        client_id: &app.config.client_id,
        redirect_uri: &app.config.redirect_uri,
        resource: &app.config.auth.resource,
        code_challenge: &consent.request.challenge,
    };
    let code = match store.issue_authorization_code(&grant, now) {
        Ok(code) => code,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    redirect_uri
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &consent.request.client_state);
    redirect(redirect_uri.as_str())
}

fn browser_cookie(headers: &HeaderMap) -> Option<String> {
    if headers.get_all(header::COOKIE).iter().count() != 1 {
        return None;
    }
    let mut found = None;
    for (name, value) in headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
    {
        if name == "discord_mcp_oauth" {
            if found.is_some()
                || value.len() != 43
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            {
                return None;
            }
            found = Some(value.to_owned());
        }
    }
    found
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn secure_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
}

fn redirect(location: &str) -> Response {
    let value = match HeaderValue::from_str(location) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(header::LOCATION, value);
    secure_headers(&mut response);
    response
}

fn error(status: StatusCode) -> Response {
    let mut response = status.into_response();
    secure_headers(&mut response);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn fake_discord() -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let handle = thread::spawn(move || {
            for body in [
                r#"{"access_token":"synthetic-access","token_type":"Bearer"}"#,
                r#"{"id":"123","username":"Test <user>"}"#,
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    request.extend_from_slice(&buf[..n]);
                    if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                if key.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        (base, handle)
    }

    #[tokio::test]
    async fn browser_login_and_consent_bind_registered_client_and_issue_single_use_code() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("oauth.db");
        let key = [41; 32];
        let store = ChannelStore::open(&db, &key).unwrap();
        store
            .put_bot("permitted", "123", "synthetic-bot-token")
            .unwrap();
        store
            .set_permissions("123", "permitted", true, false)
            .unwrap();
        drop(store);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let resource = format!("{origin}/mcp");
        let redirect_uri = "http://127.0.0.1:8765/callback";
        let (fake_base, fake) = fake_discord();
        let discord = DiscordLoginClient::new(
            "123",
            "synthetic-secret",
            &format!("{origin}/oauth/discord/callback"),
        )
        .unwrap()
        .with_test_base(fake_base);
        let config = OAuthWebConfig {
            auth: AuthState {
                database_path: db.clone(),
                encryption_key: key,
                resource: resource.clone(),
            },
            public_origin: origin.clone(),
            client_id: "trusted-client".into(),
            client_name: "Test <client>".into(),
            redirect_uri: redirect_uri.into(),
            discord,
        };
        let server = tokio::spawn(async move {
            axum::serve(listener, crate::http_mcp::router(config).unwrap())
                .await
                .unwrap()
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let metadata = client
            .get(format!("{origin}/.well-known/oauth-protected-resource"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            metadata.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let metadata: rmcp::serde_json::Value = metadata.json().await.unwrap();
        assert_eq!(metadata["resource"], resource);
        assert_eq!(metadata["authorization_servers"][0], origin);
        let auth_metadata: rmcp::serde_json::Value = client
            .get(format!("{origin}/.well-known/oauth-authorization-server"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(auth_metadata["issuer"], origin);
        assert_eq!(auth_metadata["code_challenge_methods_supported"][0], "S256");
        assert_eq!(
            auth_metadata["token_endpoint"],
            format!("{origin}/oauth/token")
        );
        let mut url = Url::parse(&format!("{origin}/oauth/authorize")).unwrap();
        url.query_pairs_mut()
            .append_pair("client_id", "trusted-client")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair(
                "code_challenge",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            )
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &resource)
            .append_pair("state", "client-state");
        let mut bad_url = url.clone();
        bad_url
            .query_pairs_mut()
            .append_pair("redirect_uri", "https://evil.invalid/");
        assert_eq!(
            client.get(bad_url).send().await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        let response = client.get(url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let discord_url = Url::parse(
            response
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let state = discord_url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let mut callback = Url::parse(&format!("{origin}/oauth/discord/callback")).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", &state)
            .append_pair("code", "synthetic-code")
            .append_pair("iss", "https://discord.com");
        let wrong_cookie = format!("discord_mcp_oauth={}", "a".repeat(43));
        assert_eq!(
            client
                .get(callback.clone())
                .header(header::COOKIE, wrong_cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(callback.clone())
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            client
                .get(callback)
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        fake.join().unwrap();
        let consent_url = format!("{origin}/oauth/consent");
        let page = client
            .get(&consent_url)
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(page.contains("Test &lt;user&gt;"));
        assert!(page.contains("Test &lt;client&gt;"));
        let post = || {
            client
                .post(&consent_url)
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body("decision=allow")
        };
        assert_eq!(
            post()
                .header(header::ORIGIN, "http://evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = post().header(header::ORIGIN, &origin).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let target = Url::parse(
            response
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            format!(
                "{}://{}:{}{}",
                target.scheme(),
                target.host_str().unwrap(),
                target.port().unwrap(),
                target.path()
            ),
            redirect_uri
        );
        let code = target
            .query_pairs()
            .find(|(key, _)| key == "code")
            .unwrap()
            .1
            .into_owned();
        assert!(
            target
                .query_pairs()
                .any(|(key, value)| key == "state" && value == "client-state")
        );
        assert_eq!(
            post()
                .header(header::ORIGIN, &origin)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let store = ChannelStore::open(&db, &key).unwrap();
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let endpoint = format!("{origin}/oauth/token");
        let mut fields = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", "trusted-client"),
            ("redirect_uri", redirect_uri),
            ("resource", resource.as_str()),
            (
                "code_verifier",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
        ];
        let malformed = client
            .post(&endpoint)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body("grant_type=authorization_code")
            .send()
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            malformed.json::<rmcp::serde_json::Value>().await.unwrap()["error"],
            "invalid_request"
        );
        fields[2].1 = "unregistered-client";
        let wrong_client = client.post(&endpoint).form(&fields).send().await.unwrap();
        assert_eq!(wrong_client.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            wrong_client
                .json::<rmcp::serde_json::Value>()
                .await
                .unwrap()["error"],
            "invalid_client"
        );
        fields[2].1 = "trusted-client";
        let invalid = client.post(&endpoint).form(&fields).send().await.unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let body: rmcp::serde_json::Value = invalid.json().await.unwrap();
        assert_eq!(body["error"], "invalid_grant");
        fields[5].1 = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let success = client.post(&endpoint).form(&fields).send().await.unwrap();
        assert_eq!(success.status(), StatusCode::OK);
        assert_eq!(
            success.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let body: rmcp::serde_json::Value = success.json().await.unwrap();
        assert_eq!(body["token_type"], "Bearer");
        let token = body["access_token"].as_str().unwrap();
        let replay = client.post(&endpoint).form(&fields).send().await.unwrap();
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            store
                .verify_access_token(token, &resource, now)
                .unwrap()
                .as_deref(),
            Some("123")
        );
        let response: rmcp::serde_json::Value = client.post(&resource)
            .bearer_auth(token)
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&rmcp::serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"read_messages", "arguments":{"channel":"permitted", "limit":0}}}))
            .send().await.unwrap().json().await.unwrap();
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("limit must be")
        );
        server.abort();
    }

    #[tokio::test]
    async fn denied_consent_never_mints_a_code_and_cannot_be_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oauth.db");
        let key = [42; 32];
        ChannelStore::open(&path, &key).unwrap();
        let origin = "http://127.0.0.1:8766";
        let binding = "a".repeat(43);
        let state = Arc::new(Inner {
            config: OAuthWebConfig {
                auth: AuthState {
                    database_path: path.clone(),
                    encryption_key: key,
                    resource: format!("{origin}/mcp"),
                },
                public_origin: origin.into(),
                client_id: "client".into(),
                client_name: "Client".into(),
                redirect_uri: "http://127.0.0.1:8765/callback".into(),
                discord: DiscordLoginClient::new(
                    "123",
                    "synthetic-secret",
                    &format!("{origin}/oauth/discord/callback"),
                )
                .unwrap(),
            },
            attempts: Mutex::new(HashMap::new()),
            consents: Mutex::new(HashMap::from([(
                binding.clone(),
                Consent {
                    user_id: "123".into(),
                    username: "Test".into(),
                    request: AuthRequest {
                        client_state: "client-state".into(),
                        challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM".into(),
                    },
                    expires: Instant::now() + Duration::from_secs(60),
                },
            )])),
        });
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("discord_mcp_oauth={binding}")).unwrap(),
        );
        let result = consent_submit(
            State(state.clone()),
            headers.clone(),
            Form(ConsentForm {
                decision: "deny".into(),
            }),
        )
        .await;
        assert_eq!(result.status(), StatusCode::SEE_OTHER);
        let location = Url::parse(
            result
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert!(
            location
                .query_pairs()
                .any(|(key, value)| key == "error" && value == "access_denied")
        );
        assert!(!location.query_pairs().any(|(key, _)| key == "code"));
        let replay = consent_submit(
            State(state),
            headers,
            Form(ConsentForm {
                decision: "allow".into(),
            }),
        )
        .await;
        assert_eq!(replay.status(), StatusCode::FORBIDDEN);
        let db = rusqlite::Connection::open(path).unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM mcp_authorization_codes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
