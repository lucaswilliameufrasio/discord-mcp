use std::{
    io::{BufRead, IsTerminal, Read},
    path::PathBuf,
};

#[cfg(feature = "encrypted-s3-backup")]
mod local_backup;
mod local_web;
#[cfg(target_os = "linux")]
mod process_guard;
mod server;
mod setup;

use discord_mcp::{
    bot::{BotChannel, BotClient},
    channels::{Channel, ChannelStore},
    webhook::{WebhookClient, WebhookUrl},
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SendMessage {
    /// Registered local channel name. Omit only when using the legacy environment webhook.
    channel: Option<String>,
    /// Message text, up to 2000 UTF-16 code units. Mentions are disabled.
    content: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ReadMessages {
    /// Registered bot channel name; webhook-only channels cannot be read.
    channel: String,
    /// Number of recent messages, between 1 and 20 (default 10).
    limit: Option<u8>,
}

#[derive(Clone)]
struct DiscordMcp {
    webhook: Option<WebhookUrl>,
    storage: Option<StorageConfig>,
    client: WebhookClient,
    bot_client: BotClient,
    tool_router: ToolRouter<Self>,
}

#[derive(Clone)]
struct StorageConfig {
    path: PathBuf,
    key: [u8; 32],
    audit_retention_days: u16,
}

impl StorageConfig {
    fn from_environment() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        match (
            std::env::var_os("DISCORD_MCP_DB"),
            std::env::var("DISCORD_MCP_KEY"),
        ) {
            (None, Err(std::env::VarError::NotPresent)) => Ok(None),
            (Some(path), Ok(value)) => {
                let audit_retention_days = match std::env::var("DISCORD_MCP_AUDIT_RETENTION_DAYS") {
                    Ok(value) => value
                        .parse::<u16>()
                        .map_err(|_| "invalid audit retention")?,
                    Err(std::env::VarError::NotPresent) => 90,
                    Err(_) => return Err("invalid audit retention".into()),
                };
                discord_mcp::http_auth::AuditState::new(audit_retention_days)?;
                let key: [u8; 32] = hex::decode(value)?
                    .try_into()
                    .map_err(|_| "key must be 32 bytes encoded in hex")?;
                Ok(Some(Self {
                    path: path.into(),
                    key,
                    audit_retention_days,
                }))
            }
            _ => Err("configure both DISCORD_MCP_DB and DISCORD_MCP_KEY".into()),
        }
    }

    fn open(&self) -> Result<ChannelStore, discord_mcp::channels::StoreError> {
        ChannelStore::open(&self.path, &self.key)?.with_audit_retention(self.audit_retention_days)
    }
}

#[tool_router]
impl DiscordMcp {
    #[tool(description = "Send a message to a registered Discord bot or webhook channel")]
    async fn send_message(&self, Parameters(input): Parameters<SendMessage>) -> CallToolResult {
        let selected = match (&input.channel, &self.storage) {
            (Some(channel), Some(storage)) => {
                match storage.open().and_then(|db| db.get_channel(channel)) {
                    Ok(channel) => Some(channel),
                    Err(error) => {
                        return CallToolResult::error(vec![ContentBlock::text(error.to_string())]);
                    }
                }
            }
            (Some(_), None) => {
                return CallToolResult::error(vec![ContentBlock::text(
                    "Channel storage is not configured.",
                )]);
            }
            (None, _) => self.webhook.clone().map(Channel::Webhook),
        };
        let Some(channel) = &selected else {
            return CallToolResult::error(vec![ContentBlock::text(
                "No webhook is configured. Set DISCORD_WEBHOOK_URL in the client environment.",
            )]);
        };
        let result = match channel {
            Channel::Webhook(webhook) => self
                .client
                .send(webhook, &input.content)
                .await
                .map_err(|e| e.to_string()),
            Channel::Bot(bot) => self
                .bot_client
                .send(bot, &input.content)
                .await
                .map_err(|e| e.to_string()),
        };
        match result {
            Ok(()) => CallToolResult::success(vec![ContentBlock::text("Message sent.")]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        }
    }

    #[tool(
        description = "Read recent messages from a registered Discord bot channel. Discord may restrict message content."
    )]
    async fn read_messages(&self, Parameters(input): Parameters<ReadMessages>) -> CallToolResult {
        let Some(storage) = &self.storage else {
            return CallToolResult::error(vec![ContentBlock::text(
                "Channel storage is not configured.",
            )]);
        };
        let channel = match storage.open().and_then(|db| db.get_channel(&input.channel)) {
            Ok(channel) => channel,
            Err(error) => {
                return CallToolResult::error(vec![ContentBlock::text(error.to_string())]);
            }
        };
        let Channel::Bot(bot) = channel else {
            return CallToolResult::error(vec![ContentBlock::text(
                "Webhook channels do not support reading.",
            )]);
        };
        match self.bot_client.read(&bot, input.limit.unwrap_or(10)).await {
            Ok(messages) => match rmcp::serde_json::to_string(&messages) {
                Ok(json) => CallToolResult::success(vec![ContentBlock::text(json)]),
                Err(_) => {
                    CallToolResult::error(vec![ContentBlock::text("Unable to encode messages.")])
                }
            },
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DiscordMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Local Discord webhook and bot channels. Remote mode and per-user permissions are not available yet.",
        )
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["setup", "local", "vault", "compare-oauth", "--confirm"] {
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if std::io::stdin().is_terminal() {
            return Err("provide the candidate on protected non-terminal stdin".into());
        }
        let mut input = String::new();
        std::io::stdin()
            .lock()
            .take(515)
            .read_to_string(&mut input)?;
        let line = input
            .strip_suffix('\n')
            .ok_or("provide exactly one newline-terminated candidate")?;
        let candidate = line.strip_suffix('\r').unwrap_or(line);
        let result = ChannelStore::compare_embedded_secret_read_only(
            &storage.path,
            &storage.key,
            discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret,
            candidate,
        )?;
        match result {
            Some(true) => println!("Snapshot: candidate matches the embedded OAuth secret."),
            Some(false) => println!("Snapshot: candidate differs from the embedded OAuth secret."),
            None => println!("Snapshot: no embedded OAuth secret is present."),
        }
        println!(
            "This does not prove the outcome of a particular operation, upstream validity, revocation status, or consumer adoption. Do not restore old credentials automatically."
        );
        return Ok(());
    }
    #[cfg(feature = "aws-parameter-store")]
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-aws-parameter-store",
            "--preview",
        ]
    {
        println!(
            "Preview: copy the configured AWS Parameter Store OAuth SecureString into an existing embedded vault only if absent. Confirmation required. Source and provider selection remain unchanged. No provider is contacted by this preview."
        );
        return Ok(());
    }
    #[cfg(feature = "aws-parameter-store")]
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-aws-parameter-store",
            "--confirm",
        ]
    {
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        let store = storage.open()?;
        let secret = discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret;
        match store.get_embedded_secret(secret) {
            Err(discord_mcp::channels::StoreError::NotFound) => {}
            Ok(_) => {
                return Err(
                    "embedded OAuth secret already exists; use explicit rotation instead".into(),
                );
            }
            Err(error) => return Err(error.into()),
        }
        let value = server::aws_parameter_store_oauth_secret().await?;
        store.insert_embedded_secret(secret, &value)?;
        eprintln!(
            "AWS Parameter Store OAuth secret copied to the embedded vault. Source and provider selection were not changed; verify adoption before removing any source."
        );
        return Ok(());
    }
    #[cfg(feature = "aws-secrets-manager")]
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-aws-secrets-manager",
            "--preview",
        ]
    {
        println!(
            "Preview: read the configured AWS Secrets Manager OAuth secret and copy it to an existing embedded vault only if absent. Confirmation is required. The source and provider selection are not changed. This preview reads no secrets and contacts no provider."
        );
        return Ok(());
    }
    #[cfg(feature = "aws-secrets-manager")]
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-aws-secrets-manager",
            "--confirm",
        ]
    {
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        let store = storage.open()?;
        let secret = discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret;
        match store.get_embedded_secret(secret) {
            Err(discord_mcp::channels::StoreError::NotFound) => {}
            Ok(_) => {
                return Err(
                    "embedded OAuth secret already exists; use explicit rotation instead".into(),
                );
            }
            Err(error) => return Err(error.into()),
        }
        let value = server::aws_secrets_manager_oauth_secret().await?;
        store.insert_embedded_secret(secret, &value)?;
        eprintln!(
            "AWS Secrets Manager OAuth secret copied to the embedded vault. Source and provider selection were not changed; verify adoption before removing any source."
        );
        return Ok(());
    }
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-infisical",
            "--preview",
        ]
    {
        println!(
            "Preview: read the configured Infisical OAuth secret and copy it to an existing embedded vault only if absent. Confirmation is required. The source and provider selection are not changed. This preview reads no secrets and contacts no provider."
        );
        return Ok(());
    }
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-infisical",
            "--confirm",
        ]
    {
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        let store = storage.open()?;
        let secret = discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret;
        match store.get_embedded_secret(secret) {
            Err(discord_mcp::channels::StoreError::NotFound) => {}
            Ok(_) => {
                return Err(
                    "embedded OAuth secret already exists; use explicit rotation instead".into(),
                );
            }
            Err(error) => return Err(error.into()),
        }
        let value = server::infisical_oauth_secret().await?;
        store.insert_embedded_secret(secret, &value)?;
        eprintln!(
            "Infisical OAuth secret copied to the embedded vault. Source and provider selection were not changed; verify adoption before removing any source."
        );
        return Ok(());
    }
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "rotate-oauth-tracked",
            "--preview",
        ]
    {
        println!(
            "Preview: atomically replace the local secret and create an unknown tracking record for one instance. Supply independently generated non-secret 16-byte hex operation and instance IDs before --confirm, and current/replacement secrets on protected non-terminal stdin. No upstream or consumer verification occurs."
        );
        return Ok(());
    }
    if let [setup, local, vault, command, operation, instance, confirm] = args.as_slice()
        && setup == "setup"
        && local == "local"
        && vault == "vault"
        && command == "rotate-oauth-tracked"
        && confirm == "--confirm"
    {
        let revision: [u8; 16] = hex::decode(operation)
            .map_err(|_| "invalid operation ID")?
            .try_into()
            .map_err(|_| "operation ID must be 16 bytes in hex")?;
        let instance: [u8; 16] = hex::decode(instance)
            .map_err(|_| "invalid instance ID")?
            .try_into()
            .map_err(|_| "instance ID must be 16 bytes in hex")?;
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        if std::io::stdin().is_terminal() {
            return Err(
                "provide current and replacement secrets on protected non-terminal stdin".into(),
            );
        }
        let mut input = String::new();
        std::io::stdin()
            .lock()
            .take(1029)
            .read_to_string(&mut input)?;
        let parts: Vec<_> = input.split('\n').collect();
        if parts.len() != 3 || !parts[2].is_empty() || input.len() > 1028 {
            return Err("provide exactly two newline-terminated secret values".into());
        }
        storage.open()?.rotate_embedded_secret_tracked(
            discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret,
            parts[0].strip_suffix('\r').unwrap_or(parts[0]),
            parts[1].strip_suffix('\r').unwrap_or(parts[1]),
            revision,
            &[instance],
        )?;
        eprintln!(
            "Local replacement and tracking committed. Publication status is unknown; upstream validity and consumer adoption were not verified."
        );
        return Ok(());
    }
    if let [setup, local, vault, command, operation, instance] = args.as_slice()
        && setup == "setup"
        && local == "local"
        && vault == "vault"
        && command == "rotation-status"
    {
        let revision: [u8; 16] = hex::decode(operation)
            .map_err(|_| "invalid operation ID")?
            .try_into()
            .map_err(|_| "invalid operation ID")?;
        let instance: [u8; 16] = hex::decode(instance)
            .map_err(|_| "invalid instance ID")?
            .try_into()
            .map_err(|_| "invalid instance ID")?;
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        let store = storage.open()?;
        let oauth = store.rotation_login_observed(
            revision,
            instance,
            discord_mcp::rotation_progress::Consumer::OAuth,
        )?;
        let admin = store.rotation_login_observed(
            revision,
            instance,
            discord_mcp::rotation_progress::Consumer::Admin,
        )?;
        println!(
            "oauth_login_observed={oauth} admin_login_observed={admin} workload_identity_verified=false adoption_verified=false; historical local observations only, instance label is operator supplied; current validity and adoption are not proven"
        );
        return Ok(());
    }
    if let [setup, local, vault, command, operation] = args.as_slice()
        && setup == "setup"
        && local == "local"
        && vault == "vault"
        && command == "rotation-status"
    {
        let revision: [u8; 16] = hex::decode(operation)
            .map_err(|_| "invalid operation ID")?
            .try_into()
            .map_err(|_| "operation ID must be 16 bytes in hex")?;
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        match storage.open()?.get_rotation_progress(revision)? {
            Some((version, state)) => println!(
                "version={version} publication={:?} all_consumers_reported={} workload_identity_verified=false adoption_verified=false (reports are not authenticated proof)",
                state.publication(),
                state.all_consumers_reported()
            ),
            None => println!(
                "No tracking record found. This does not prove an upstream operation was rolled back."
            ),
        }
        return Ok(());
    }
    if args.as_slice() == ["setup", "local", "vault", "rotate-oauth", "--preview"] {
        println!(
            "Preview: replace the existing embedded OAuth secret only if the supplied current secret still matches. Requires --confirm and exactly two lines on protected non-terminal stdin: current secret, then replacement. Stop the service first. No upstream rotation or server restart is performed."
        );
        return Ok(());
    }
    if args.as_slice() == ["setup", "local", "vault", "rotate-oauth", "--confirm"] {
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        if std::io::stdin().is_terminal() {
            return Err(
                "provide current and replacement secrets on protected non-terminal stdin".into(),
            );
        }
        let mut input = String::new();
        std::io::stdin()
            .lock()
            .take(1029)
            .read_to_string(&mut input)?;
        let parts: Vec<_> = input.split('\n').collect();
        if parts.len() != 3 || !parts[2].is_empty() || input.len() > 1028 {
            return Err("provide exactly two newline-terminated secret values".into());
        }
        storage.open()?.rotate_embedded_secret(
            discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret,
            parts[0].strip_suffix('\r').unwrap_or(parts[0]),
            parts[1].strip_suffix('\r').unwrap_or(parts[1]),
        )?;
        eprintln!(
            "Embedded OAuth secret replaced. Upstream rotation and running-server adoption were not verified. Do not automatically restore revoked credentials."
        );
        return Ok(());
    }
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-env",
            "--preview",
        ]
    {
        println!(
            "Preview: copy DISCORD_MCP_DISCORD_CLIENT_SECRET into the existing encrypted database only if no OAuth secret is stored. Requires --confirm. The source, provider selection, and server are not changed. No files are changed by this preview."
        );
        return Ok(());
    }
    if args.as_slice()
        == [
            "setup",
            "local",
            "vault",
            "migrate-oauth-from-env",
            "--confirm",
        ]
    {
        let value = std::env::var("DISCORD_MCP_DISCORD_CLIENT_SECRET")
            .map_err(|_| "missing or invalid DISCORD_MCP_DISCORD_CLIENT_SECRET")?;
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        storage.open()?.insert_embedded_secret(
            discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret,
            &value,
        )?;
        eprintln!(
            "OAuth secret copied to the embedded vault. Source configuration and provider selection were not changed; validate adoption before removing any source."
        );
        return Ok(());
    }
    if args.as_slice() == ["setup", "local", "vault", "discord-oauth", "--preview"] {
        println!(
            "Preview: add the Discord OAuth client secret to an existing encrypted database. Requires --confirm and protected non-terminal stdin; existing secrets are never replaced. No files are changed by this preview."
        );
        return Ok(());
    }
    if args.as_slice() == ["setup", "local", "vault", "discord-oauth", "--confirm"] {
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, false)?;
        if !storage.path.is_file() {
            return Err("an existing database is required".into());
        }
        let input = read_remote_secret()?;
        let value = input.trim_end_matches(['\r', '\n']);
        storage.open()?.insert_embedded_secret(
            discord_mcp::channels::EmbeddedSecret::DiscordOAuthClientSecret,
            value,
        )?;
        eprintln!("OAuth secret stored. Existing secrets were not replaced.");
        return Ok(());
    }
    if let [command, mode, kind, hostname, preview] = args.as_slice()
        && command == "setup"
        && mode == "remote"
        && kind == "bundle"
        && preview == "--preview"
    {
        return setup::preview_remote_bundle(hostname);
    }
    if let [command, mode, kind, hostname, directory, confirm] = args.as_slice()
        && command == "setup"
        && mode == "remote"
        && kind == "bundle"
        && confirm == "--confirm"
    {
        return setup::prepare_remote_bundle(hostname, std::path::Path::new(directory));
    }
    if args.first().is_some_and(|arg| arg == "setup")
        && args.get(1).is_some_and(|arg| arg == "remote")
        && args.get(2).is_some_and(|arg| arg == "bundle")
    {
        return Err("use setup remote bundle HOST --preview or setup remote bundle HOST NEW_DIRECTORY --confirm".into());
    }
    if let [command, mode, kind, hostname, preview] = args.as_slice()
        && command == "setup"
        && mode == "remote"
        && kind == "caddy"
        && preview == "--preview"
    {
        return setup::preview_caddy(hostname);
    }
    if let [command, mode, kind, hostname, directory, confirm] = args.as_slice()
        && command == "setup"
        && mode == "remote"
        && kind == "caddy"
        && confirm == "--confirm"
    {
        return setup::install_caddy(hostname, std::path::Path::new(directory));
    }
    if args.first().is_some_and(|arg| arg == "setup")
        && args.get(1).is_some_and(|arg| arg == "remote")
        && args.get(2).is_some_and(|arg| arg == "caddy")
    {
        return Err("use setup remote caddy HOST --preview or setup remote caddy HOST ABSOLUTE_DIRECTORY --confirm".into());
    }
    if args.as_slice() == ["setup", "local", "--generate-key", "--preview"] {
        eprintln!(
            "Preview: generate a new 32-byte key and write its hexadecimal encoding only to non-terminal stdout. No configuration or files are changed. Direct stdout to a trusted secret manager; preserve an independent recovery copy. Never replace the key for an existing database."
        );
        return Ok(());
    }
    if args.as_slice() == ["setup", "local", "--generate-key", "--confirm"] {
        return setup::generate_key();
    }
    if args.first().is_some_and(|arg| arg == "setup")
        && args.get(1).is_some_and(|arg| arg == "local")
        && args.get(2).is_some_and(|arg| arg == "--generate-key")
    {
        return Err("use setup local --generate-key --preview or --confirm".into());
    }
    if args.as_slice() == ["setup", "remote", "systemd", "--preview"] {
        setup::preview_systemd();
        return Ok(());
    }
    if let [command, mode, kind, action, directory, confirm] = args.as_slice()
        && command == "setup"
        && mode == "remote"
        && kind == "systemd"
        && action == "--install-ciphera-dropin"
        && confirm == "--confirm"
    {
        return setup::install_ciphera_dropin(std::path::Path::new(directory));
    }
    if let [command, mode, kind, action, directory, confirm] = args.as_slice()
        && command == "setup"
        && mode == "remote"
        && kind == "systemd"
        && action == "--install-env-template"
        && confirm == "--confirm"
    {
        return setup::install_systemd_env_template(std::path::Path::new(directory));
    }
    if let [command, mode, kind, action, directory, confirm] = args.as_slice()
        && command == "setup"
        && mode == "remote"
        && kind == "systemd"
        && action == "--install-unit"
        && confirm == "--confirm"
    {
        return setup::install_systemd_unit(std::path::Path::new(directory));
    }
    if let [command, mode, kind, destination, confirm] = args.as_slice()
        && command == "setup"
        && mode == "remote"
        && kind == "systemd"
        && !destination.starts_with('-')
        && confirm == "--confirm"
    {
        return setup::prepare_systemd(std::path::Path::new(destination));
    }
    if args.first().is_some_and(|arg| arg == "setup")
        && args.get(1).is_some_and(|arg| arg == "remote")
        && args.get(2).is_some_and(|arg| arg == "systemd")
    {
        return Err(
            "use setup remote systemd --preview, setup remote systemd NEW_DIRECTORY --confirm, or setup remote systemd --install-unit ABSOLUTE_DIRECTORY --confirm"
                .into(),
        );
    }
    if args.as_slice() == ["setup", "remote", "--check"] {
        if std::env::var_os("DISCORD_MCP_SECRET_PROVIDER").is_some_and(|value| {
            value == "embedded"
                || value == "infisical"
                || value == "aws-secrets-manager"
                || value == "aws-parameter-store"
        }) {
            return Err("read-only remote preflight does not open the selected secret provider; validate it through an isolated service start".into());
        }
        setup::check_remote()?;
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        server::check_configuration(&storage).await?;
        println!("Server configuration validation passed; no service was started.");
        return Ok(());
    }
    if args.as_slice() == ["setup", "remote", "--check-storage"] {
        return setup::check_storage();
    }
    if args.as_slice() == ["setup", "remote", "--check-https-origin"] {
        return setup::check_https_origin();
    }
    if args.as_slice() == ["setup", "local", "--init"] {
        let storage = StorageConfig::from_environment()?
            .ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY first")?;
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(&storage.path, false, true)?;
        let store = storage.open()?;
        if store.recovery_quarantined()? {
            return Err(
                "database is quarantined; review and explicitly activate recovery first".into(),
            );
        }
        println!("Local storage initialized. Start `discord-mcp web` to add channels.");
        return Ok(());
    }
    if let [command, mode] = args.as_slice()
        && command == "setup"
    {
        return setup::run(mode);
    }
    if args.first().is_some_and(|command| command == "setup") {
        return Err("invalid setup command; use --help".into());
    }
    #[cfg(feature = "encrypted-s3-backup")]
    if let [command, origin, attempt, staging, confirm] = args.as_slice()
        && command == "remote-verify-backup"
        && confirm == "--confirm-isolated"
    {
        return remote_operation(origin, RemoteOperation::VerifyBackup(attempt, staging)).await;
    }
    #[cfg(feature = "encrypted-s3-backup")]
    if args
        .first()
        .is_some_and(|command| command == "remote-verify-backup")
    {
        return Err(
            "use remote-verify-backup ORIGIN ATTEMPT_ID PRIVATE_STAGING --confirm-isolated".into(),
        );
    }
    #[cfg(feature = "encrypted-s3-backup")]
    if let [command, endpoint, bucket, id, staging, confirm] = args.as_slice()
        && command == "verify-floci"
        && confirm == "--confirm-isolated"
    {
        return local_backup::verify(endpoint, bucket, id, staging).await;
    }
    #[cfg(feature = "encrypted-s3-backup")]
    if let [command, origin, id] = args.as_slice()
        && command == "remote-backup"
    {
        return remote_operation(origin, RemoteOperation::Backup(id)).await;
    }
    #[cfg(feature = "encrypted-s3-backup")]
    if let [command, endpoint, bucket, object, destination] = args.as_slice()
        && command == "restore-floci"
    {
        return local_backup::restore(endpoint, bucket, object, destination).await;
    }
    if let [command, origin, user, channel, permission] = args.as_slice()
        && command == "remote-set-permission"
    {
        remote_operation(
            origin,
            RemoteOperation::SetPermission(user, channel, permission),
        )
        .await?;
        return Ok(());
    }
    if let [command, origin] = args.as_slice()
        && command == "remote-list-channels"
    {
        remote_operation(origin, RemoteOperation::ListChannels).await?;
        return Ok(());
    }
    if let [command, origin, channel] = args.as_slice()
        && command == "remote-show-permissions"
    {
        remote_operation(origin, RemoteOperation::ShowPermissions(channel)).await?;
        return Ok(());
    }
    if let [command, origin, channel] = args.as_slice()
        && command == "remote-delete-channel"
    {
        remote_operation(origin, RemoteOperation::DeleteChannel(channel)).await?;
        return Ok(());
    }
    if let [command, origin, channel, discord_channel_id] = args.as_slice()
        && command == "remote-add-bot"
    {
        let token = read_remote_secret()?;
        remote_operation(
            origin,
            RemoteOperation::RegisterBot(
                channel,
                discord_channel_id,
                token.trim_end_matches(['\r', '\n']),
            ),
        )
        .await?;
        return Ok(());
    }
    if let [command, origin, channel] = args.as_slice()
        && command == "remote-add-webhook"
    {
        let input = read_remote_secret()?;
        let webhook = input.trim_end_matches(['\r', '\n']);
        remote_operation(origin, RemoteOperation::RegisterWebhook(channel, webhook)).await?;
        return Ok(());
    }
    if let [command, source, destination] = args.as_slice()
        && command == "recovery-prepare"
    {
        #[cfg(target_os = "linux")]
        let _guard = process_guard::acquire(std::path::Path::new(source), true, false)?;
        let key = std::env::var("DISCORD_MCP_KEY").map_err(|_| "missing DISCORD_MCP_KEY")?;
        let key: [u8; 32] = hex::decode(key)?
            .try_into()
            .map_err(|_| "key must be 32 bytes encoded in hex")?;
        let days = match std::env::var("DISCORD_MCP_AUDIT_RETENTION_DAYS") {
            Ok(value) => value
                .parse::<u16>()
                .map_err(|_| "invalid audit retention")?,
            Err(std::env::VarError::NotPresent) => 90,
            Err(_) => return Err("invalid audit retention".into()),
        };
        let report = discord_mcp::recovery::prepare_restored_copy(
            std::path::Path::new(source),
            std::path::Path::new(destination),
            &key,
            days,
        )?;
        println!(
            "Quarantined copy prepared; invalidated permissions={}, tokens={}, codes={}. Review before activation.",
            report.permissions, report.tokens, report.codes
        );
        return Ok(());
    }
    if let [command, _, _, permission] = args.as_slice()
        && command == "set-permission"
        && !matches!(permission.as_str(), "none" | "read" | "send" | "both")
    {
        return Err("permission must be none, read, send or both".into());
    }
    if !valid_remaining_arguments(&args) {
        return Err("unknown or malformed command; use --help".into());
    }
    if args.as_slice() == ["--help"] || args.as_slice() == ["help"] {
        println!(
            "discord-mcp: [setup local [--init]|setup remote|serve|web|add-channel NAME|list-channels|set-permission USER CHANNEL none|read|send|both|remote-set-permission ORIGIN USER CHANNEL none|read|send|both|remote-list-channels ORIGIN|remote-show-permissions ORIGIN CHANNEL|remote-add-webhook ORIGIN CHANNEL|remote-add-bot ORIGIN CHANNEL DISCORD_CHANNEL_ID|remote-delete-channel ORIGIN CHANNEL|show-permissions CHANNEL|recovery-prepare SOURCE DESTINATION|recovery-activate --confirm]"
        );
        println!("Server configuration and security requirements: see README.md.");
        println!("Recovery planning: setup recovery (read-only).");
        println!(
            "Embedded OAuth secret: setup local vault discord-oauth --preview (requires existing encrypted database for enrollment)."
        );
        println!(
            "Copy existing OAuth environment secret: setup local vault migrate-oauth-from-env --preview."
        );
        println!(
            "Copy OAuth secret from Infisical: setup local vault migrate-oauth-from-infisical --preview."
        );
        #[cfg(feature = "aws-secrets-manager")]
        println!(
            "Copy OAuth secret from Secrets Manager: setup local vault migrate-oauth-from-aws-secrets-manager --preview."
        );
        println!("Replace embedded OAuth secret: setup local vault rotate-oauth --preview.");
        println!(
            "Generate a new local key: setup local --generate-key --preview (no files changed)."
        );
        println!("Remote configuration preflight: setup remote --check (read-only).");
        println!("Remote bundle: setup remote bundle HOST --preview (no installation).");
        println!("Caddy HTTPS site: setup remote caddy HOST --preview (no installation).");
        println!(
            "Configuration template: setup remote systemd --install-env-template ABSOLUTE_DIRECTORY --confirm (no secrets)."
        );
        println!(
            "Ciphera: setup remote systemd --install-ciphera-dropin ABSOLUTE_DIRECTORY --confirm (no credentials or restart)."
        );
        println!("Existing storage metadata: setup remote --check-storage (read-only).");
        println!("HTTPS origin syntax: setup remote --check-https-origin (no network).");
        println!(
            "Systemd bundle: setup remote systemd --preview | setup remote systemd NEW_DIRECTORY --confirm (local files only)."
        );
        #[cfg(feature = "encrypted-s3-backup")]
        println!(
            "Synthetic loopback Floci only: backup-floci ADDRESS BUCKET OBJECT AGE_RECIPIENT | restore-floci ADDRESS BUCKET OBJECT DESTINATION | verify-floci ADDRESS BUCKET ID PRIVATE_STAGING --confirm-isolated"
        );
        #[cfg(feature = "encrypted-s3-backup")]
        println!(
            "Mediated verification: remote-verify-backup ORIGIN ATTEMPT_ID PRIVATE_STAGING --confirm-isolated"
        );
        return Ok(());
    }
    let storage = StorageConfig::from_environment()?;
    let recovery_activation =
        matches!(args.first(), Some(command) if command == "recovery-activate");
    // The lock is cooperative: it separates this program's remote and local
    // processes, but does not confine other programs or protect replication.
    #[cfg(target_os = "linux")]
    let _guard = if let Some(config) = &storage {
        Some(process_guard::acquire(
            &config.path,
            args.as_slice() == ["serve"],
            !recovery_activation
                && args.first().is_none_or(|command| command != "backup-floci")
                && !(args.as_slice() == ["serve"]
                    && std::env::var_os("DISCORD_MCP_SECRET_PROVIDER").is_some_and(|value| {
                        value == "embedded"
                            || value == "infisical"
                            || value == "aws-secrets-manager"
                            || value == "aws-parameter-store"
                    })),
        )?)
    } else {
        None
    };
    if !args.is_empty() {
        let storage = storage.ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY")?;
        #[cfg(feature = "encrypted-s3-backup")]
        if let [command, endpoint, bucket, object, recipient] = args.as_slice()
            && command == "backup-floci"
        {
            return local_backup::backup(endpoint, bucket, object, recipient, &storage).await;
        }
        if args.as_slice() == ["web"] {
            if storage.open()?.recovery_quarantined()? {
                return Err(
                    "database is quarantined; review and explicitly activate recovery first".into(),
                );
            }
            return local_web::run(storage).await;
        }
        if args.as_slice() == ["serve"] {
            return server::run(storage).await;
        }
        let db = if recovery_activation {
            ChannelStore::open_recovery_copy(&storage.path, &storage.key)?
                .with_audit_retention(storage.audit_retention_days)?
        } else {
            storage.open()?
        };
        if db.recovery_quarantined()? && args.as_slice() != ["recovery-activate", "--confirm"] {
            return Err(
                "database is quarantined; review and explicitly activate recovery first".into(),
            );
        }
        match args.as_slice() {
            [command, confirmation]
                if command == "recovery-activate" && confirmation == "--confirm" =>
            {
                db.activate_recovered_copy()?;
                eprintln!(
                    "Recovery activated. All prior permissions and MCP tokens remain invalidated."
                );
            }
            [command, name] if command == "add-channel" => {
                let mut input = String::new();
                std::io::stdin()
                    .lock()
                    .take(4097)
                    .read_to_string(&mut input)?;
                if input.len() > 4096 {
                    return Err("input too long".into());
                }
                db.put_webhook(name, input.trim())?;
                eprintln!("Channel saved.");
            }
            [command] if command == "list-channels" => {
                for name in db.list()? {
                    println!("{name}");
                }
            }
            [command, user, channel, permission] if command == "set-permission" => {
                let (read, send) = match permission.as_str() {
                    "none" => (false, false),
                    "read" => (true, false),
                    "send" => (false, true),
                    "both" => (true, true),
                    _ => return Err("permission must be none, read, send or both".into()),
                };
                db.set_permissions(user, channel, read, send)?;
                eprintln!("Channel permissions updated.");
            }
            [command, channel] if command == "show-permissions" => {
                for entry in db.list_permissions(channel)? {
                    println!(
                        "{}\tread={}\tsend={}",
                        entry.user_id, entry.can_read, entry.can_send
                    );
                }
            }
            _ => return Err("unknown command; use --help".into()),
        }
        return Ok(());
    }
    if let Some(config) = &storage
        && config.open()?.recovery_quarantined()?
    {
        return Err(
            "database is quarantined; review and explicitly activate recovery first".into(),
        );
    }
    let webhook = match std::env::var("DISCORD_WEBHOOK_URL") {
        Ok(value) => Some(WebhookUrl::parse(&value)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("invalid webhook environment value".into()),
    };
    let server = DiscordMcp {
        webhook,
        storage,
        client: WebhookClient::new()?,
        bot_client: BotClient::new()?,
        tool_router: DiscordMcp::tool_router(),
    }
    .serve(rmcp::transport::stdio())
    .await?;
    server.waiting().await?;
    Ok(())
}

fn valid_remaining_arguments(args: &[String]) -> bool {
    match args {
        [] => true,
        [command] => matches!(
            command.as_str(),
            "--help" | "help" | "serve" | "web" | "list-channels"
        ),
        [command, value] => {
            matches!(command.as_str(), "add-channel" | "show-permissions")
                || (command == "recovery-activate" && value == "--confirm")
        }
        [command, _, _, permission] => {
            command == "set-permission"
                && matches!(permission.as_str(), "none" | "read" | "send" | "both")
        }
        #[cfg(feature = "encrypted-s3-backup")]
        [command, _, _, _, _] => command == "backup-floci",
        _ => false,
    }
}

fn read_remote_secret() -> Result<String, Box<dyn std::error::Error>> {
    if std::io::stdin().is_terminal() {
        return Err(
            "provide the credential on protected non-terminal stdin, not as an argument".into(),
        );
    }
    let mut input = String::new();
    std::io::stdin().lock().take(4097).read_line(&mut input)?;
    if input.len() > 4096 || !input.ends_with('\n') {
        return Err("invalid credential input".into());
    }
    Ok(input)
}

#[derive(Clone, Copy)]
enum RemoteOperation<'a> {
    #[cfg(feature = "encrypted-s3-backup")]
    VerifyBackup(&'a str, &'a str),
    #[cfg(feature = "encrypted-s3-backup")]
    Backup(&'a str),
    SetPermission(&'a str, &'a str, &'a str),
    ListChannels,
    ShowPermissions(&'a str),
    RegisterWebhook(&'a str, &'a str),
    RegisterBot(&'a str, &'a str, &'a str),
    DeleteChannel(&'a str),
}

async fn remote_operation(
    origin: &str,
    operation: RemoteOperation<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};
    use std::time::Duration;

    let url = reqwest::Url::parse(origin)?;
    let local = url.scheme() == "http"
        && matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]")
        );
    if !(url.scheme() == "https" || local)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("remote origin must be HTTPS or loopback HTTP, without a path".into());
    }
    let mut verifier = [0u8; 32];
    getrandom::fill(&mut verifier)?;
    let challenge = hex::encode(Sha256::digest(verifier));
    #[cfg(feature = "encrypted-s3-backup")]
    let timeout_secs = if matches!(
        operation,
        RemoteOperation::Backup(_) | RemoteOperation::VerifyBackup(..)
    ) {
        90
    } else {
        15
    };
    #[cfg(not(feature = "encrypted-s3-backup"))]
    let timeout_secs = 15;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(timeout_secs))
        .build()?;
    let (endpoint, body) = match operation {
        #[cfg(feature = "encrypted-s3-backup")]
        RemoteOperation::VerifyBackup(attempt, _) => (
            "/v1/cli/backup-verification-requests",
            rmcp::serde_json::json!({"attempt_id":attempt,"challenge":challenge}),
        ),
        #[cfg(feature = "encrypted-s3-backup")]
        RemoteOperation::Backup(id) => (
            "/v1/cli/backup-requests",
            rmcp::serde_json::json!({"backup_id":id,"challenge":challenge}),
        ),
        RemoteOperation::SetPermission(user, channel, permission) => {
            let (read, send) = match permission {
                "none" => (false, false),
                "read" => (true, false),
                "send" => (false, true),
                "both" => (true, true),
                _ => return Err("permission must be none, read, send or both".into()),
            };
            (
                "/v1/cli/requests",
                rmcp::serde_json::json!({"user_id":user,"channel":channel,"read":read,"send":send,"challenge":challenge}),
            )
        }
        RemoteOperation::ListChannels => (
            "/v1/cli/list-requests",
            rmcp::serde_json::json!({"challenge":challenge}),
        ),
        RemoteOperation::ShowPermissions(channel) => (
            "/v1/cli/permission-requests",
            rmcp::serde_json::json!({"channel":channel,"challenge":challenge}),
        ),
        RemoteOperation::RegisterWebhook(channel, webhook) => {
            let parsed = WebhookUrl::parse(webhook)?;
            (
                "/v1/cli/webhook-requests",
                rmcp::serde_json::json!({"channel":channel,"webhook_id":parsed.id(),"commitment":hex::encode(Sha256::digest(webhook.as_bytes())),"challenge":challenge}),
            )
        }
        RemoteOperation::RegisterBot(channel, discord_channel_id, token) => {
            BotChannel::new(discord_channel_id, token)
                .ok_or("invalid bot credential or channel ID")?;
            (
                "/v1/cli/bot-requests",
                rmcp::serde_json::json!({"channel":channel,"discord_channel_id":discord_channel_id,"commitment":hex::encode(Sha256::digest(token.as_bytes())),"challenge":challenge}),
            )
        }
        RemoteOperation::DeleteChannel(channel) => (
            "/v1/cli/delete-requests",
            rmcp::serde_json::json!({"channel":channel,"challenge":challenge}),
        ),
    };
    let response = client
        .post(url.join(endpoint)?)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let body: rmcp::serde_json::Value = response.json().await?;
    let id = body
        .get("request_id")
        .and_then(|v| v.as_str())
        .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("invalid server response")?;
    let mut consent = url.join("/admin/cli")?;
    consent.query_pairs_mut().append_pair("request_id", id);
    eprintln!(
        "Abra no navegador, entre com Discord e autorize a operação: {consent}\nDepois pressione Enter nesta CLI."
    );
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input)? == 0 {
        return Err("confirmation input closed before authorization".into());
    }
    #[cfg(feature = "encrypted-s3-backup")]
    let recovery = if matches!(operation, RemoteOperation::VerifyBackup(..)) {
        eprintln!(
            "Forneça identidade age e chave da aplicação em stdin protegido; não serão enviadas ao servidor."
        );
        Some(local_backup::read_recovery_input()?)
    } else {
        None
    };
    let mut execution = rmcp::serde_json::json!({"request_id":id,"verifier":hex::encode(verifier)});
    if let RemoteOperation::RegisterWebhook(_, webhook) = operation {
        execution["webhook"] = webhook.into();
    }
    if let RemoteOperation::RegisterBot(_, _, token) = operation {
        execution["bot_token"] = token.into();
    }
    let response = client
        .post(url.join("/v1/cli/execute")?)
        .json(&execution)
        .send()
        .await?;
    match operation {
        #[cfg(feature = "encrypted-s3-backup")]
        RemoteOperation::VerifyBackup(_, staging) => {
            if response.status() != reqwest::StatusCode::OK {
                return Err("verification download denied or unavailable".into());
            }
            let binding = match response
                .headers()
                .get("x-backup-ciphertext-binding")
                .and_then(|value| value.to_str().ok())
            {
                Some("matched") => {
                    "O ciphertext coincide com o compromisso anterior ao envio; isso não prova quem escreveu o objeto."
                }
                Some("unavailable") => {
                    "Tentativa antiga sem compromisso de ciphertext; não é possível vincular os bytes à tentativa."
                }
                _ => return Err("missing or invalid ciphertext binding response".into()),
            };
            if response
                .content_length()
                .is_some_and(|n| n > discord_mcp::encrypted_backup::MAX_ENCRYPTED_OBJECT)
            {
                return Err("verification ciphertext exceeds limit".into());
            }
            let mut response = response;
            let mut ciphertext = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if ciphertext.len().saturating_add(chunk.len())
                    > discord_mcp::encrypted_backup::MAX_ENCRYPTED_OBJECT as usize
                {
                    return Err("verification ciphertext exceeds limit".into());
                }
                ciphertext.extend_from_slice(&chunk);
            }
            let (identity, key) = recovery.ok_or("missing recovery input")?;
            local_backup::verify_ciphertext(&ciphertext, staging, &identity, &key)?;
            eprintln!("{binding} Auditoria inalterada.");
        }
        #[cfg(feature = "encrypted-s3-backup")]
        RemoteOperation::Backup(_) => {
            if response.status() != reqwest::StatusCode::NO_CONTENT {
                return Err("backup was not confirmed; inspect the object before retrying".into());
            }
            eprintln!("Encrypted backup confirmed by remote service.");
        }
        RemoteOperation::SetPermission(..) => {
            if response.status() != reqwest::StatusCode::NO_CONTENT {
                return Err("authorization denied, expired, consumed or operation failed".into());
            }
            eprintln!("Channel permissions updated by remote service.");
        }
        RemoteOperation::ListChannels => {
            if response.status() != reqwest::StatusCode::OK {
                return Err("authorization denied, expired, consumed or operation failed".into());
            }
            #[derive(serde::Deserialize)]
            struct Channels {
                channels: Vec<String>,
            }
            let result: Channels = response.json().await?;
            for channel in result.channels {
                println!("{channel}");
            }
        }
        RemoteOperation::ShowPermissions(_) => {
            if response.status() != reqwest::StatusCode::OK {
                return Err("authorization denied, expired, consumed or operation failed".into());
            }
            #[derive(serde::Deserialize)]
            struct Permission {
                user_id: String,
                can_read: bool,
                can_send: bool,
            }
            #[derive(serde::Deserialize)]
            struct Permissions {
                permissions: Vec<Permission>,
            }
            let result: Permissions = response.json().await?;
            for permission in result.permissions {
                println!(
                    "{}\tread={}\tsend={}",
                    permission.user_id, permission.can_read, permission.can_send
                );
            }
        }
        RemoteOperation::RegisterWebhook(..) => {
            if response.status() != reqwest::StatusCode::NO_CONTENT {
                return Err("authorization denied, expired, consumed or operation failed".into());
            }
            eprintln!("Webhook channel configured by remote service.");
        }
        RemoteOperation::RegisterBot(..) => {
            if response.status() != reqwest::StatusCode::NO_CONTENT {
                return Err("authorization denied, expired, consumed or operation failed".into());
            }
            eprintln!("Bot channel configured by remote service.");
        }
        RemoteOperation::DeleteChannel(..) => {
            if response.status() != reqwest::StatusCode::NO_CONTENT {
                return Err(
                    "authorization denied, expired, consumed, channel changed or operation failed"
                        .into(),
                );
            }
            eprintln!("Local channel configuration deleted by remote service.");
        }
    }
    Ok(())
}
