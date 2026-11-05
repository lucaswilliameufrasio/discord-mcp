use discord_mcp::webhook::{WebhookClient, WebhookUrl};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SendMessage {
    /// Message text, up to 2000 UTF-16 code units. Mentions are disabled.
    content: String,
}

#[derive(Clone)]
struct DiscordMcp {
    webhook: Option<WebhookUrl>,
    client: WebhookClient,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DiscordMcp {
    #[tool(description = "Send a message to the locally configured Discord webhook")]
    async fn send_message(&self, Parameters(input): Parameters<SendMessage>) -> CallToolResult {
        let Some(webhook) = &self.webhook else {
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
    let webhook = match std::env::var("DISCORD_WEBHOOK_URL") {
        Ok(value) => Some(WebhookUrl::parse(&value)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("invalid webhook environment value".into()),
    };
    let server = DiscordMcp {
        webhook,
        client: WebhookClient::new()?,
        tool_router: DiscordMcp::tool_router(),
    }
    .serve(rmcp::transport::stdio())
    .await?;
    server.waiting().await?;
    Ok(())
}
