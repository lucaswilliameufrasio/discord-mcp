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
    let (identity, key) = read_recovery_input()?;
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

pub(crate) fn read_recovery_input()
-> Result<(age::x25519::Identity, [u8; 32]), Box<dyn std::error::Error>> {
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
    Ok((identity, key))
}

/// Explicit local synthetic verification; does not open or modify the live DB.
pub async fn verify(
    endpoint: &str,
    bucket: &str,
    id: &str,
    parent: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("invalid backup ID".into());
    }
    let client = client(endpoint)?;
    let (identity, key) = read_recovery_input()?;
    let staging = create_verification_staging(parent)?;
    let destination = staging.path().join("quarantined.db");
    let object = format!("service-backups/{id}.age");
    let report = tokio::time::timeout(
        Duration::from_secs(60),
        discord_mcp::encrypted_backup::restore_quarantined(
            &client,
            bucket,
            &object,
            &destination,
            &identity,
            &key,
            90,
        ),
    )
    .await??;
    finish_verification(staging, &key, report)
}

fn create_verification_staging(
    parent: &str,
) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let parent = Path::new(parent);
    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.is_dir() {
        return Err("private staging directory required".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("private staging directory required".into());
        }
    }
    let staging = tempfile::Builder::new()
        .prefix(".discord-mcp-verify-")
        .tempdir_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(staging)
}

pub(crate) fn verify_ciphertext(
    encrypted: &[u8],
    parent: &str,
    identity: &age::x25519::Identity,
    key: &[u8; 32],
) -> Result<(), Box<dyn std::error::Error>> {
    let staging = create_verification_staging(parent)?;
    let report = discord_mcp::encrypted_backup::restore_ciphertext_quarantined(
        encrypted,
        &staging.path().join("quarantined.db"),
        identity,
        key,
        90,
    )?;
    finish_verification(staging, key, report)
}

fn finish_verification(
    staging: tempfile::TempDir,
    key: &[u8; 32],
    report: discord_mcp::channels::RecoveryInvalidations,
) -> Result<(), Box<dyn std::error::Error>> {
    let destination = staging.path().join("quarantined.db");
    let store = discord_mcp::channels::ChannelStore::open_recovery_copy(&destination, key)?;
    if !store.recovery_quarantined()?
        || !matches!(
            store.list_active(),
            Err(discord_mcp::channels::StoreError::RecoveryPending)
        )
    {
        return Err("quarantine validation failed".into());
    }
    let channels = store.list()?.len();
    drop(store);
    let db = rusqlite::Connection::open_with_flags(
        &destination,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err("integrity validation failed".into());
    }
    let violations: i64 =
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    let remaining: i64 = db.query_row("SELECT (SELECT count(*) FROM channel_permissions)+(SELECT count(*) FROM mcp_access_tokens)+(SELECT count(*) FROM mcp_authorization_codes)", [], |row| row.get(0))?;
    if violations != 0 || remaining != 0 {
        return Err("recovered authority validation failed".into());
    }
    drop(db);
    staging.close()?;
    println!(
        "Synthetic verification passed: authenticated snapshot, SQLite integrity and quarantine; channels={channels}, invalidated permissions={}, tokens={}, codes={}. No activation performed.",
        report.permissions, report.tokens, report.codes
    );
    Ok(())
}
