//! Optional Secrets Manager boundary. Diagnostics never include provider payloads.
use std::time::Duration;

use aws_sdk_secretsmanager::Client;

pub async fn discord_oauth_secret(secret_id: &str) -> Result<String, &'static str> {
    let config = tokio::time::timeout(
        Duration::from_secs(15),
        aws_config::load_defaults(aws_config::BehaviorVersion::latest()),
    )
    .await
    .map_err(|_| "AWS configuration timed out")?;
    let client_config = aws_sdk_secretsmanager::config::Builder::from(&config)
        .timeout_config(
            aws_sdk_secretsmanager::config::timeout::TimeoutConfig::builder()
                .operation_timeout(Duration::from_secs(15))
                .build(),
        )
        .build();
    read_discord_oauth_secret(&Client::from_conf(client_config), secret_id).await
}

/// SecretString must contain only the OAuth client secret, not a JSON object.
pub async fn read_discord_oauth_secret(
    client: &Client,
    secret_id: &str,
) -> Result<String, &'static str> {
    if secret_id.is_empty()
        || secret_id.len() > 2048
        || !secret_id.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err("invalid AWS secret identifier");
    }
    let output = client
        .get_secret_value()
        .secret_id(secret_id)
        .send()
        .await
        .map_err(|_| "AWS Secrets Manager request failed")?;
    let value = output
        .secret_string()
        .ok_or("AWS secret must be a SecretString")?;
    if value.is_empty() || value.len() > 512 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("invalid AWS OAuth secret value");
    }
    Ok(value.to_owned())
}
