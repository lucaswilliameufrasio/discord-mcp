//! Synthetic local Floci CLI only. Never connects to a provider or uses real credentials.
use std::{
    io::{IsTerminal, Read},
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};

use crate::StorageConfig;
use aws_sdk_s3::{
    Client,
    config::{Credentials, Region},
};

pub(crate) fn client(endpoint: &str) -> Result<Client, Box<dyn std::error::Error>> {
    let address: SocketAddr = endpoint
        .parse()
        .map_err(|_| "expected IPv4 loopback address and port")?;
    if address.ip() != Ipv4Addr::LOCALHOST || address.port() == 0 {
        return Err("only IPv4 loopback with an explicit port is allowed".into());
    }
    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(Region::new("us-east-1"))
        .endpoint_url(format!("http://{address}"))
        .force_path_style(true)
        .credentials_provider(Credentials::new("test", "test", None, None, "local-floci"))
        .build();
    Ok(Client::from_conf(config))
}

pub async fn backup(
    endpoint: &str,
    bucket: &str,
    object: &str,
    recipient: &str,
    storage: &StorageConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let recipient: age::x25519::Recipient =
        recipient.parse().map_err(|_| "invalid age recipient")?;
    let parent = storage
        .path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or("database must have a private parent directory")?;
    tokio::time::timeout(
        Duration::from_secs(60),
        discord_mcp::encrypted_backup::upload_snapshot(
            &client(endpoint)?,
            bucket,
            object,
            &storage.path,
            parent,
            &recipient,
        ),
    )
    .await??;
    println!("Encrypted synthetic snapshot uploaded.");
    Ok(())
}

pub async fn restore(
    endpoint: &str,
    bucket: &str,
    object: &str,
    destination: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = client(endpoint)?;
    if std::io::stdin().is_terminal() {
        return Err(
            "supply recovery identity and application key on protected non-terminal stdin".into(),
        );
    }
    let mut input = String::new();
    std::io::stdin()
        .lock()
        .take(1025)
        .read_to_string(&mut input)?;
    if input.len() > 1024 {
        return Err("invalid recovery input".into());
    }
    let mut lines = input.lines();
    let identity: age::x25519::Identity = lines
        .next()
        .ok_or("missing recovery identity")?
        .parse()
        .map_err(|_| "invalid recovery identity")?;
    let key: [u8; 32] = hex::decode(lines.next().ok_or("missing application key")?)
        .map_err(|_| "invalid application key")?
        .try_into()
        .map_err(|_| "invalid application key")?;
    if lines.next().is_some() {
        return Err("invalid recovery input".into());
    }
    let report = tokio::time::timeout(
        Duration::from_secs(60),
        discord_mcp::encrypted_backup::restore_quarantined(
            &client,
            bucket,
            object,
            Path::new(destination),
            &identity,
            &key,
            90,
        ),
    )
    .await??;
    println!(
        "Quarantined synthetic copy prepared; invalidated permissions={}, tokens={}, codes={}. Review before activation.",
        report.permissions, report.tokens, report.codes
    );
    Ok(())
}
