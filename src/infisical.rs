//! Narrow Infisical v4 read-only boundary. Never formats remote responses or tokens.
use std::time::Duration;

use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;

#[derive(Deserialize)]
struct Response {
    secret: Secret,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Secret {
    secret_key: String,
    secret_value: String,
    secret_path: String,
    environment: String,
    secret_value_hidden: bool,
}

/// Fetch exactly the named application secret; no imports or cross-path fallback.
pub async fn discord_oauth_secret(
    origin: &str,
    project: &str,
    environment: &str,
    path: &str,
    token: &str,
) -> Result<String, &'static str> {
    let mut url = Url::parse(origin).map_err(|_| "invalid Infisical origin")?;
    let loopback = url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if !(url.scheme() == "https" || loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || project.is_empty()
        || environment.is_empty()
        || !path.starts_with('/')
        || token.is_empty()
        || token.len() > 8192
        || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err("invalid Infisical configuration");
    }
    url.set_path("/api/v4/secrets/DISCORD_MCP_DISCORD_CLIENT_SECRET");
    url.query_pairs_mut()
        .append_pair("projectId", project)
        .append_pair("environment", environment)
        .append_pair("secretPath", path)
        .append_pair("includeImports", "false")
        .append_pair("expandSecretReferences", "false");
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "Infisical client unavailable")?;
    let mut response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "Infisical request failed")?;
    if !response.status().is_success() {
        return Err("Infisical rejected or failed the secret request");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Infisical response failed")?
    {
        if body.len().saturating_add(chunk.len()) > 65536 {
            return Err("Infisical response too large");
        }
        body.extend_from_slice(&chunk);
    }
    let parsed: Response =
        rmcp::serde_json::from_slice(&body).map_err(|_| "invalid Infisical response")?;
    let secret = parsed.secret;
    if secret.secret_key != "DISCORD_MCP_DISCORD_CLIENT_SECRET"
        || secret.environment != environment
        || secret.secret_path != path
        || secret.secret_value_hidden
        || secret.secret_value.is_empty()
        || secret.secret_value.len() > 512
        || !secret
            .secret_value
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
    {
        return Err("Infisical returned an invalid or mismatched secret");
    }
    Ok(secret.secret_value)
}
