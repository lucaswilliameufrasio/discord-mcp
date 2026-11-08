//! Optional SSM Parameter Store boundary. Never include provider payloads in errors.
use std::time::Duration;

use aws_sdk_ssm::{Client, types::ParameterType};

pub async fn discord_oauth_secret(parameter: &str) -> Result<String, &'static str> {
    let config = tokio::time::timeout(
        Duration::from_secs(15),
        aws_config::load_defaults(aws_config::BehaviorVersion::latest()),
    )
    .await
    .map_err(|_| "AWS configuration timed out")?;
    let client_config = aws_sdk_ssm::config::Builder::from(&config)
        .timeout_config(
            aws_sdk_ssm::config::timeout::TimeoutConfig::builder()
                .operation_timeout(Duration::from_secs(15))
                .build(),
        )
        .build();
    read_discord_oauth_secret(&Client::from_conf(client_config), parameter).await
}

/// Read one SecureString, using its entire value as the OAuth secret.
pub async fn read_discord_oauth_secret(
    client: &Client,
    name: &str,
) -> Result<String, &'static str> {
    if name.is_empty() || name.len() > 2048 || !name.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("invalid AWS parameter identifier");
    }
    let output = client
        .get_parameter()
        .name(name)
        .with_decryption(true)
        .send()
        .await
        .map_err(|_| "AWS Parameter Store request failed")?;
    let parameter = output.parameter().ok_or("AWS parameter missing")?;
    if parameter.r#type() != Some(&ParameterType::SecureString) {
        return Err("AWS parameter must be a SecureString");
    }
    let value = parameter.value().ok_or("AWS parameter value missing")?;
    if value.is_empty() || value.len() > 512 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("invalid AWS OAuth secret value");
    }
    Ok(value.to_owned())
}
