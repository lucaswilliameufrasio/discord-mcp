use std::{io::Read, path::PathBuf};

use discord_mcp::{
    channels::ChannelStore,
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

#[derive(Clone)]
struct DiscordMcp {
    webhook: Option<WebhookUrl>,
    storage: Option<StorageConfig>,
    client: WebhookClient,
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
    #[tool(description = "Send a message to the locally configured Discord webhook")]
    async fn send_message(&self, Parameters(input): Parameters<SendMessage>) -> CallToolResult {
        let selected = match (&input.channel, &self.storage) {
            (Some(channel), Some(storage)) => {
                match storage.open().and_then(|db| db.get_webhook(channel)) {
                    Ok(webhook) => Some(webhook),
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
            (None, _) => self.webhook.clone(),
        };
        let Some(webhook) = &selected else {
            return CallToolResult::error(vec![ContentBlock::text(
                "No webhook is configured. Set DISCORD_WEBHOOK_URL in the client environment.",
            )]);
        };
        match self.client.send(webhook, &input.content).await {
            Ok(()) => CallToolResult::success(vec![ContentBlock::text("Message sent.")]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DiscordMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Local webhook sending only. Channel management, bot access and remote mode are not available yet.",
        )
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let storage = StorageConfig::from_environment()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let storage = storage.ok_or("configure DISCORD_MCP_DB and DISCORD_MCP_KEY")?;
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
            _ => return Err("usage: discord-mcp [add-channel NAME|list-channels]".into()),
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
        tool_router: DiscordMcp::tool_router(),
    }
    .serve(rmcp::transport::stdio())
    .await?;
    server.waiting().await?;
    Ok(())
}
