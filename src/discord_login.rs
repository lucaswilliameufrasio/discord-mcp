//! Discord identity provider boundary. This is not an MCP authorization server.
use std::{
    fmt,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Debug)]
pub enum LoginError {
    InvalidConfiguration,
    InvalidCallback,
    Expired,
    ProviderUnavailable,
    ProviderRejected,
    InvalidResponse,
}

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "invalid Discord login configuration",
            Self::InvalidCallback => "invalid Discord login callback",
            Self::Expired => "Discord login attempt expired",
            Self::ProviderUnavailable => "Discord login service unavailable",
            Self::ProviderRejected => "Discord login was rejected",
            Self::InvalidResponse => "invalid Discord login response",
        })
    }
}

impl std::error::Error for LoginError {}

#[derive(Clone)]
pub struct DiscordLoginClient {
    client: Client,
    client_id: String,
    client_secret: String,
    redirect_uri: Url,
}

pub struct PendingDiscordLogin {
    state: String,
    browser_binding: String,
    verifier: String,
    expires: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
}

#[derive(Deserialize)]
pub struct DiscordIdentity {
    pub id: String,
    pub username: String,
}

fn random_value() -> Result<String, LoginError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| LoginError::ProviderUnavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

impl DiscordLoginClient {
    /// `redirect_uri` is operator-configured and must match the Discord application.
    pub fn new(
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
    ) -> Result<Self, LoginError> {
        let uri = Url::parse(redirect_uri).map_err(|_| LoginError::InvalidConfiguration)?;
        let loopback = uri.scheme() == "http"
            && matches!(
                uri.host_str(),
                Some("127.0.0.1") | Some("localhost") | Some("[::1]")
            );
        if client_id.is_empty()
            || client_id.len() > 20
            || !client_id.bytes().all(|c| c.is_ascii_digit())
            || client_secret.is_empty()
            || client_secret.len() > 512
            || !client_secret.bytes().all(|c| c.is_ascii_graphic())
            || !(uri.scheme() == "https" || loopback)
            || uri.host_str().is_none()
            || !uri.username().is_empty()
            || uri.password().is_some()
            || uri.fragment().is_some()
            || uri.as_str() != redirect_uri
        {
            return Err(LoginError::InvalidConfiguration);
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| LoginError::ProviderUnavailable)?;
        Ok(Self {
            client,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            redirect_uri: uri,
        })
    }

    /// The caller must put browser_binding in a protected, HttpOnly, SameSite cookie.
    /// Store the pending attempt server-side and remove it atomically on callback.
    pub fn begin(&self) -> Result<(PendingDiscordLogin, Url), LoginError> {
        let state = random_value()?;
        let browser_binding = random_value()?;
        let verifier = random_value()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = Url::parse("https://discord.com/oauth2/authorize")
            .map_err(|_| LoginError::InvalidConfiguration)?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", self.redirect_uri.as_str())
            .append_pair("scope", "identify")
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok((
            PendingDiscordLogin {
                state,
                browser_binding,
                verifier,
                expires: Instant::now() + Duration::from_secs(300),
            },
            url,
        ))
    }

    pub async fn finish(
        &self,
        pending: PendingDiscordLogin,
        state: &str,
        browser_binding: &str,
        issuer: &str,
        code: &str,
    ) -> Result<DiscordIdentity, LoginError> {
        let base =
            Url::parse("https://discord.com/").map_err(|_| LoginError::InvalidConfiguration)?;
        self.finish_at(pending, state, browser_binding, issuer, code, &base)
            .await
    }

    async fn finish_at(
        &self,
        pending: PendingDiscordLogin,
        state: &str,
        browser_binding: &str,
        issuer: &str,
        code: &str,
        base: &Url,
    ) -> Result<DiscordIdentity, LoginError> {
        if Instant::now() >= pending.expires {
            return Err(LoginError::Expired);
        }
        if !equal(&pending.state, state)
            || !equal(&pending.browser_binding, browser_binding)
            || issuer != "https://discord.com"
            || code.is_empty()
            || code.len() > 2048
            || !code.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(LoginError::InvalidCallback);
        }
        let token_url = base
            .join("api/oauth2/token")
            .map_err(|_| LoginError::InvalidConfiguration)?;
        let response = self
            .client
            .post(token_url)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("code_verifier", pending.verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|_| LoginError::ProviderUnavailable)?;
        let token: TokenResponse = parse_response(response).await?;
        if !token.token_type.eq_ignore_ascii_case("bearer")
            || token.access_token.is_empty()
            || token.access_token.len() > 2048
            || !token.access_token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(LoginError::InvalidResponse);
        }
        let user_url = base
            .join("api/v10/users/@me")
            .map_err(|_| LoginError::InvalidConfiguration)?;
        let response = self
            .client
            .get(user_url)
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|_| LoginError::ProviderUnavailable)?;
        let identity: DiscordIdentity = parse_response(response).await?;
        if identity.id.is_empty()
            || identity.id.len() > 20
            || !identity.id.bytes().all(|b| b.is_ascii_digit())
            || identity.username.is_empty()
            || identity.username.len() > 256
        {
            return Err(LoginError::InvalidResponse);
        }
        Ok(identity)
    }
}

impl PendingDiscordLogin {
    /// Return only to the browser as a secure session cookie, never in a URL.
    pub fn browser_binding(&self) -> &str {
        &self.browser_binding
    }
}

async fn parse_response<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, LoginError> {
    if !response.status().is_success() {
        return Err(LoginError::ProviderRejected);
    }
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LoginError::ProviderUnavailable)?
    {
        if data.len() + chunk.len() > 64 * 1024 {
            return Err(LoginError::InvalidResponse);
        }
        data.extend_from_slice(&chunk);
    }
    serde_json_from_slice(&data)
}

fn serde_json_from_slice<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, LoginError> {
    rmcp::serde_json::from_slice(data).map_err(|_| LoginError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn state_from(url: &Url) -> String {
        url.query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned()
    }

    #[tokio::test]
    async fn callback_requires_original_state_browser_issuer_and_unexpired_attempt() {
        let client =
            DiscordLoginClient::new("123", "synthetic-secret", "http://127.0.0.1:8765/callback")
                .unwrap();
        let base = Url::parse("http://127.0.0.1:1/").unwrap();
        let (pending, url) = client.begin().unwrap();
        let state = state_from(&url);
        let binding = pending.browser_binding().to_owned();
        assert!(matches!(
            client
                .finish_at(
                    pending,
                    "wrong",
                    &binding,
                    "https://discord.com",
                    "code",
                    &base
                )
                .await,
            Err(LoginError::InvalidCallback)
        ));
        let (pending, _) = client.begin().unwrap();
        assert!(matches!(
            client
                .finish_at(
                    pending,
                    &state,
                    "wrong",
                    "https://discord.com",
                    "code",
                    &base
                )
                .await,
            Err(LoginError::InvalidCallback)
        ));
        let (pending, url) = client.begin().unwrap();
        let state = state_from(&url);
        let binding = pending.browser_binding().to_owned();
        assert!(matches!(
            client
                .finish_at(
                    pending,
                    &state,
                    &binding,
                    "https://evil.invalid",
                    "code",
                    &base
                )
                .await,
            Err(LoginError::InvalidCallback)
        ));
        let (mut pending, url) = client.begin().unwrap();
        let state = state_from(&url);
        let binding = pending.browser_binding().to_owned();
        pending.expires = Instant::now();
        assert!(matches!(
            client
                .finish_at(
                    pending,
                    &state,
                    &binding,
                    "https://discord.com",
                    "code",
                    &base
                )
                .await,
            Err(LoginError::Expired)
        ));
    }

    #[tokio::test]
    async fn code_exchange_and_identity_use_discord_protocol_without_exposing_secrets() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let fake = thread::spawn(move || {
            let mut requests = Vec::new();
            for body in [
                r#"{"access_token":"synthetic-access","token_type":"Bearer"}"#,
                r#"{"id":"987","username":"synthetic-user"}"#,
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let count = stream.read(&mut chunk).unwrap();
                    data.extend_from_slice(&chunk[..count]);
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                if name.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(data).unwrap());
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
            }
            requests
        });
        let client =
            DiscordLoginClient::new("123", "synthetic-secret", "http://127.0.0.1:8765/callback")
                .unwrap();
        let (pending, url) = client.begin().unwrap();
        let state = state_from(&url);
        let binding = pending.browser_binding().to_owned();
        let challenge = url
            .query_pairs()
            .find(|(k, _)| k == "code_challenge")
            .unwrap()
            .1
            .into_owned();
        let identity = client
            .finish_at(
                pending,
                &state,
                &binding,
                "https://discord.com",
                "synthetic-code",
                &base,
            )
            .await
            .unwrap();
        assert_eq!(identity.id, "987");
        assert_eq!(identity.username, "synthetic-user");
        let requests = fake.join().unwrap();
        assert!(requests[0].starts_with("POST /api/oauth2/token HTTP/1.1"));
        assert!(requests[1].starts_with("GET /api/v10/users/@me HTTP/1.1"));
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("authorization: bearer synthetic-access")
        );
        let body = requests[0].split_once("\r\n\r\n").unwrap().1;
        let parsed = Url::parse(&format!("http://localhost/?{body}")).unwrap();
        let verifier = parsed
            .query_pairs()
            .find(|(k, _)| k == "code_verifier")
            .unwrap()
            .1
            .into_owned();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            challenge
        );
        assert!(
            url.query_pairs()
                .any(|(k, v)| k == "scope" && v == "identify")
        );
        assert!(!url.as_str().contains("synthetic-secret"));
    }
}
