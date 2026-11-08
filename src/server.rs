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

pub(crate) async fn infisical_oauth_secret() -> Result<String, Box<dyn std::error::Error>> {
    if std::env::var_os("DISCORD_MCP_DISCORD_CLIENT_SECRET").is_some() {
        return Err(
            "infisical provider cannot be combined with DISCORD_MCP_DISCORD_CLIENT_SECRET".into(),
        );
    }
    Ok(discord_mcp::infisical::discord_oauth_secret(
        &required("DISCORD_MCP_INFISICAL_ORIGIN")?,
        &required("DISCORD_MCP_INFISICAL_PROJECT_ID")?,
        &required("DISCORD_MCP_INFISICAL_ENVIRONMENT")?,
        &required("DISCORD_MCP_INFISICAL_SECRET_PATH")?,
        &required("DISCORD_MCP_INFISICAL_TOKEN")?,
    )
    .await?)
}

#[cfg(feature = "aws-secrets-manager")]
pub(crate) async fn aws_secrets_manager_oauth_secret() -> Result<String, Box<dyn std::error::Error>>
{
    if std::env::var_os("DISCORD_MCP_DISCORD_CLIENT_SECRET").is_some() {
        return Err("aws-secrets-manager provider cannot be combined with DISCORD_MCP_DISCORD_CLIENT_SECRET".into());
    }
    Ok(
        discord_mcp::aws_secrets::discord_oauth_secret(&required("DISCORD_MCP_AWS_SECRET_ID")?)
            .await?,
    )
}

#[cfg(feature = "aws-parameter-store")]
pub(crate) async fn aws_parameter_store_oauth_secret() -> Result<String, Box<dyn std::error::Error>>
{
    if std::env::var_os("DISCORD_MCP_DISCORD_CLIENT_SECRET").is_some() {
        return Err("aws-parameter-store provider cannot be combined with DISCORD_MCP_DISCORD_CLIENT_SECRET".into());
    }
    Ok(
        discord_mcp::aws_parameter_store::discord_oauth_secret(&required(
            "DISCORD_MCP_AWS_PARAMETER_NAME",
        )?)
        .await?,
    )
}

async fn configuration(
    storage: &StorageConfig,
    activate_tracking: bool,
) -> Result<(SocketAddr, OAuthWebConfig, AdminConfig, u16), Box<dyn std::error::Error>> {
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
    let tracked = match (
        std::env::var("DISCORD_MCP_ROTATION_OPERATION"),
        std::env::var("DISCORD_MCP_ROTATION_INSTANCE"),
    ) {
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => None,
        (Ok(operation), Ok(instance)) => {
            if std::env::var("DISCORD_MCP_SECRET_PROVIDER").as_deref() != Ok("embedded") {
                return Err("rotation tracking requires the embedded provider".into());
            }
            let operation: [u8; 16] = hex::decode(operation)
                .map_err(|_| "invalid rotation operation ID")?
                .try_into()
                .map_err(|_| "invalid rotation operation ID")?;
            let instance: [u8; 16] = hex::decode(instance)
                .map_err(|_| "invalid rotation instance ID")?
                .try_into()
                .map_err(|_| "invalid rotation instance ID")?;
            Some((operation, instance))
        }
        _ => return Err("configure both rotation operation and instance IDs".into()),
    };
    let discord_id = required("DISCORD_MCP_DISCORD_CLIENT_ID")?;
    let discord_secret = match std::env::var("DISCORD_MCP_SECRET_PROVIDER") {
        Err(std::env::VarError::NotPresent) => required("DISCORD_MCP_DISCORD_CLIENT_SECRET")?,
        Ok(provider) if provider == "environment" => required("DISCORD_MCP_DISCORD_CLIENT_SECRET")?,
        Ok(provider) if provider == "embedded" => {
            if std::env::var_os("DISCORD_MCP_DISCORD_CLIENT_SECRET").is_some() {
                return Err(
                    "embedded provider cannot be combined with DISCORD_MCP_DISCORD_CLIENT_SECRET"
                        .into(),
                );
            }
            match tracked {
                Some((revision, instance)) => storage
                    .open()?
                    .get_tracked_embedded_secret(revision, instance)?,
                None => storage.open()?.get_embedded_secret(
                    discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret,
                )?,
            }
        }
        Ok(provider) if provider == "infisical" => infisical_oauth_secret().await?,
        #[cfg(feature = "aws-secrets-manager")]
        Ok(provider) if provider == "aws-secrets-manager" => {
            aws_secrets_manager_oauth_secret().await?
        }
        #[cfg(feature = "aws-parameter-store")]
        Ok(provider) if provider == "aws-parameter-store" => {
            aws_parameter_store_oauth_secret().await?
        }
        _ => return Err("invalid or unsupported DISCORD_MCP_SECRET_PROVIDER".into()),
    };
    let mut discord = DiscordLoginClient::new(
        &discord_id,
        &discord_secret,
        &format!("{origin}/oauth/discord/callback"),
    )?;
    let mut admin_discord = DiscordLoginClient::new(
        &discord_id,
        &discord_secret,
        &format!("{origin}/admin/discord/callback"),
    )?;
    if let Some((revision, instance)) = tracked
        && activate_tracking
    {
        let mut boot = [0u8; 16];
        getrandom::fill(&mut boot).map_err(|_| "unable to create rotation process epoch")?;
        #[cfg(all(discord_mcp_synthetic_authority, target_os = "linux"))]
        if std::env::var_os("DISCORD_TEST_SUPERVISED_EVENTS").is_some() {
            boot = hex::decode(required("DISCORD_TEST_SUPERVISOR_EPOCH")?)?
                .try_into()
                .map_err(|_| "invalid synthetic supervisor epoch")?;
            if boot == [0; 16] {
                return Err("invalid synthetic supervisor epoch".into());
            }
        }
        storage
            .open()?
            .begin_rotation_process(revision, instance, boot, &discord_secret)?;
        discord = discord.with_rotation_observation(
            storage.path.clone(),
            storage.key,
            revision,
            instance,
            boot,
            discord_mcp::rotation_progress::Consumer::OAuth,
        );
        admin_discord = admin_discord.with_rotation_observation(
            storage.path.clone(),
            storage.key,
            revision,
            instance,
            boot,
            discord_mcp::rotation_progress::Consumer::Admin,
        );
    }
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
        backup: floci_backup(storage)?,
    };
    let config = OAuthWebConfig {
        auth,
        public_origin: origin,
        client_id: required("DISCORD_MCP_CLIENT_ID")?,
        client_name: required("DISCORD_MCP_CLIENT_NAME")?,
        redirect_uri: required("DISCORD_MCP_CLIENT_REDIRECT")?,
        discord,
    };
    Ok((listen, config, admin, retention_days))
}

pub async fn check_configuration(
    storage: &StorageConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let (_, config, admin, _) = configuration(storage, false).await?;
    let _router = http_mcp::router(config)?.merge(admin_web::router(admin)?);
    Ok(())
}

pub async fn run(storage: StorageConfig) -> Result<(), Box<dyn std::error::Error>> {
    let (listen, config, admin, retention_days) = configuration(&storage, true).await?;
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
