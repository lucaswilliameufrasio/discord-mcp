//! CLI server bootstrap. Always listens on loopback; use a TLS reverse proxy remotely.
use std::{net::SocketAddr, time::Duration};

use discord_mcp::{
    admin_web::{self, AdminConfig},
    discord_login::DiscordLoginClient,
    http_auth::AuthState,
    http_mcp,
    oauth_web::OAuthWebConfig,
};

use crate::StorageConfig;

fn required(name: &'static str) -> Result<String, Box<dyn std::error::Error>> {
    std::env::var(name).map_err(|_| format!("missing or invalid {name}").into())
}

pub async fn run(storage: StorageConfig) -> Result<(), Box<dyn std::error::Error>> {
    let listen: SocketAddr = std::env::var("DISCORD_MCP_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8000".into())
        .parse()
        .map_err(|_| "invalid DISCORD_MCP_LISTEN")?;
    if !listen.ip().is_loopback() {
        return Err("DISCORD_MCP_LISTEN must be a loopback address".into());
    }
    let origin = required("DISCORD_MCP_PUBLIC_ORIGIN")?;
    let url = reqwest::Url::parse(&origin).map_err(|_| "invalid DISCORD_MCP_PUBLIC_ORIGIN")?;
    if url.scheme() == "http" {
        let local = matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("[::1]")
        );
        if !local || url.port_or_known_default() != Some(listen.port()) || listen.port() == 0 {
            return Err("HTTP public origin must be loopback and use the listener port".into());
        }
    }
    let discord_id = required("DISCORD_MCP_DISCORD_CLIENT_ID")?;
    let discord_secret = required("DISCORD_MCP_DISCORD_CLIENT_SECRET")?;
    let discord = DiscordLoginClient::new(
        &discord_id,
        &discord_secret,
        &format!("{origin}/oauth/discord/callback"),
    )?;
    let admin_discord = DiscordLoginClient::new(
        &discord_id,
        &discord_secret,
        &format!("{origin}/admin/discord/callback"),
    )?;
    let retention_days = match std::env::var("DISCORD_MCP_METRICS_RETENTION_DAYS") {
        Ok(value) => value
            .parse::<u16>()
            .map_err(|_| "invalid metrics retention")?,
        Err(std::env::VarError::NotPresent) => 30,
        Err(_) => return Err("invalid metrics retention".into()),
    };
    let metrics = discord_mcp::http_auth::MetricsState::new(retention_days)?;
    let auth = AuthState {
        database_path: storage.path.clone(),
        encryption_key: storage.key,
        resource: format!("{origin}/mcp"),
        metrics,
        audit: discord_mcp::http_auth::AuditState::new(storage.audit_retention_days)?,
    };
    let admin = AdminConfig {
        auth: auth.clone(),
        public_origin: origin.clone(),
        admin_user_id: required("DISCORD_MCP_ADMIN_ID")?,
        discord: admin_discord,
        #[cfg(feature = "encrypted-s3-backup")]
        backup: floci_backup(&storage)?,
    };
    let config = OAuthWebConfig {
        auth,
        public_origin: origin,
        client_id: required("DISCORD_MCP_CLIENT_ID")?,
        client_name: required("DISCORD_MCP_CLIENT_NAME")?,
        redirect_uri: required("DISCORD_MCP_CLIENT_REDIRECT")?,
        discord,
    };
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?;
    let store = storage.open()?;
    if store.recovery_quarantined()? {
        return Err(
            "database is quarantined; review and explicitly activate recovery first".into(),
        );
    }
    store.prune_operational_metrics(now, retention_days)?;
    store.prune_audit(now, storage.audit_retention_days)?;
    let router = http_mcp::router(config)?.merge(admin_web::router(admin)?);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!(
        "MCP HTTP listening on {} (loopback only)",
        listener.local_addr()?
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    tokio::select! {
        result = &mut task => { result??; }
        signal = shutdown_signal() => {
            if signal.is_err() { task.abort(); return Err("unable to install shutdown signal handler".into()); }
            let _ = stop.send(());
            eprintln!("MCP HTTP draining in-flight requests (15 second limit)");
            match tokio::time::timeout(Duration::from_secs(15), &mut task).await {
                Ok(result) => { result??; }
                Err(_) => { task.abort(); return Err("shutdown timed out; in-flight requests may be incomplete".into()); }
            }
        }
    }
    Ok(())
}

#[cfg(feature = "encrypted-s3-backup")]
fn floci_backup(
    storage: &StorageConfig,
) -> Result<
    Option<std::sync::Arc<discord_mcp::encrypted_backup::ServiceBackup>>,
    Box<dyn std::error::Error>,
> {
    let address = match std::env::var("DISCORD_MCP_FLOCI_BACKUP_ADDRESS") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(_) => return Err("invalid Floci address".into()),
    };
    let bucket = required("DISCORD_MCP_FLOCI_BACKUP_BUCKET")?;
    let recipient: age::x25519::Recipient = required("DISCORD_MCP_FLOCI_BACKUP_RECIPIENT")?
        .parse()
        .map_err(|_| "invalid age recipient")?;
    let parent = storage
        .path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or("database requires a private parent")?;
    let backup = discord_mcp::encrypted_backup::ServiceBackup::new_floci(
        address.parse()?,
        bucket,
        storage.path.clone(),
        parent.to_path_buf(),
        recipient,
    )?;
    Ok(Some(std::sync::Arc::new(backup)))
}

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! { result = tokio::signal::ctrl_c() => result, _ = term.recv() => Ok(()) }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}
