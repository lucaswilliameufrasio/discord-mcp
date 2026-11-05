use std::{io::Read, path::PathBuf};

mod local_web;
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
    if let [command, source, destination] = args.as_slice()
        && command == "recovery-prepare"
    {
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
    if !args.is_empty() {
        if args.as_slice() == ["--help"] || args.as_slice() == ["help"] {
            println!(
                "discord-mcp: [serve|web|add-channel NAME|list-channels|set-permission USER CHANNEL none|read|send|both|show-permissions CHANNEL|recovery-prepare SOURCE DESTINATION|recovery-activate --confirm]"
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
