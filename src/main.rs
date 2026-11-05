use std::{io::Read, path::PathBuf};

mod local_web;

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
}

impl StorageConfig {
    fn from_environment() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        match (
            std::env::var_os("DISCORD_MCP_DB"),
            std::env::var("DISCORD_MCP_KEY"),
        ) {
            (None, Err(std::env::VarError::NotPresent)) => Ok(None),
            (Some(path), Ok(value)) => {
                let key: [u8; 32] = hex::decode(value)?
                    .try_into()
                    .map_err(|_| "key must be 32 bytes encoded in hex")?;
                Ok(Some(Self {
                    path: path.into(),
                    key,
                }))
            }
            _ => Err("configure both DISCORD_MCP_DB and DISCORD_MCP_KEY".into()),
        }
    }

    fn open(&self) -> Result<ChannelStore, discord_mcp::channels::StoreError> {
        ChannelStore::open(&self.path, &self.key)
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
    let storage = StorageConfig::from_environment()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let storage = storage.ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY")?;
        if args.as_slice() == ["web"] {
            storage.open()?;
            return local_web::run(storage).await;
        }
        let db = storage.open()?;
        match args.as_slice() {
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
            _ => return Err("usage: discord-mcp [web|add-channel NAME|list-channels]".into()),
        }
        return Ok(());
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
