use std::{io::Read, path::PathBuf};

mod local_web;
#[cfg(target_os = "linux")]
mod process_guard;
mod server;

use discord_mcp::{
    bot::BotClient,
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
    let storage = StorageConfig::from_environment()?;
    // The lock is cooperative: it separates this program's remote and local
    // processes, but does not confine other programs or protect replication.
    #[cfg(target_os = "linux")]
    let _guard = if args.as_slice() == ["--help"] || args.as_slice() == ["help"] {
        None
    } else if let Some(config) = &storage {
        Some(process_guard::acquire(
            &config.path,
            args.as_slice() == ["serve"],
            true,
        )?)
    } else {
        None
    };
    if !args.is_empty() {
        if args.as_slice() == ["--help"] || args.as_slice() == ["help"] {
            println!(
                "discord-mcp: [serve|web|add-channel NAME|list-channels|set-permission USER CHANNEL none|read|send|both|remote-set-permission ORIGIN USER CHANNEL none|read|send|both|remote-list-channels ORIGIN|remote-show-permissions ORIGIN CHANNEL|show-permissions CHANNEL|recovery-prepare SOURCE DESTINATION|recovery-activate --confirm]"
            );
            println!("Server configuration and security requirements: see README.md.");
            return Ok(());
        }
        let storage = storage.ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY")?;
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
        let db = storage.open()?;
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

#[derive(Clone, Copy)]
enum RemoteOperation<'a> {
    SetPermission(&'a str, &'a str, &'a str),
    ListChannels,
    ShowPermissions(&'a str),
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
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let (endpoint, body) = match operation {
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
    std::io::stdin().read_line(&mut input)?;
    let response = client
        .post(url.join("/v1/cli/execute")?)
        .json(&rmcp::serde_json::json!({"request_id":id,"verifier":hex::encode(verifier)}))
        .send()
        .await?;
    match operation {
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
    }
    Ok(())
}
