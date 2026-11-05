//! Authenticated, stateless Streamable HTTP MCP router. No listener is started here.
use std::sync::Arc;

use axum::{Router, http::request::Parts, middleware};
use rmcp::{
    RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
    },
};

use crate::{
    bot::BotClient,
    channels::{Channel, ChannelAction},
    http_auth::{AuthState, AuthenticatedUser, authenticate},
    oauth_web::{OAuthWebConfig, router as oauth_router},
    webhook::WebhookClient,
};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SendMessage {
    /// Local channel name authorized for the current user.
    channel: String,
    /// Message text, at most 2000 UTF-16 code units. Mentions are disabled.
    content: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ReadMessages {
    /// Local bot channel name authorized for the current user.
    channel: String,
    /// Number of messages from 1 to 20, default 10.
    limit: Option<u8>,
}

#[derive(Clone)]
struct HttpMcp {
    auth: AuthState,
    webhook: WebhookClient,
    bot: BotClient,
    tool_router: ToolRouter<Self>,
}

impl HttpMcp {
    fn identity(context: &RequestContext<RoleServer>) -> Option<AuthenticatedUser> {
        context
            .extensions
            .get::<Parts>()?
            .extensions
            .get::<AuthenticatedUser>()
            .cloned()
    }

    fn denied() -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text("Channel access denied.")])
    }
}

#[tool_router]
impl HttpMcp {
    #[tool(description = "Send a message to an authorized Discord channel")]
    async fn send_message(
        &self,
        Parameters(input): Parameters<SendMessage>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let Some(user) = Self::identity(&context) else {
            return Self::denied();
        };
        let channel = match user.authorized_channel(&self.auth, &input.channel, ChannelAction::Send)
        {
            Ok(channel) => channel,
            Err(_) => return Self::denied(),
        };
        let result = match channel {
            Channel::Webhook(webhook) => self
                .webhook
                .send(&webhook, &input.content)
                .await
                .map_err(|error| error.to_string()),
            Channel::Bot(bot) => self
                .bot
                .send(&bot, &input.content)
                .await
                .map_err(|error| error.to_string()),
        };
        match result {
            Ok(()) => CallToolResult::success(vec![ContentBlock::text("Message sent.")]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        }
    }

    #[tool(description = "Read recent messages from an authorized Discord bot channel")]
    async fn read_messages(
        &self,
        Parameters(input): Parameters<ReadMessages>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let Some(user) = Self::identity(&context) else {
            return Self::denied();
        };
        let channel = match user.authorized_channel(&self.auth, &input.channel, ChannelAction::Read)
        {
            Ok(channel) => channel,
            Err(_) => return Self::denied(),
        };
        let Channel::Bot(bot) = channel else {
            return Self::denied();
        };
        match self.bot.read(&bot, input.limit.unwrap_or(10)).await {
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
impl ServerHandler for HttpMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Channel access is controlled independently for each authenticated user and operation.",
        )
    }
}

/// Assemble OAuth and MCP routes. The caller must enforce TLS outside loopback.
pub fn router(config: OAuthWebConfig) -> Result<Router, &'static str> {
    if config.auth.resource != format!("{}/mcp", config.public_origin) {
        return Err("MCP resource must match the configured /mcp endpoint");
    }
    let origin_url = reqwest::Url::parse(&config.public_origin).map_err(|_| "invalid origin")?;
    let host = origin_url.host_str().ok_or("invalid origin")?;
    let authority = match origin_url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let origin_port = origin_url
        .port_or_known_default()
        .ok_or("invalid origin port")?;
    let exact_origin = format!("{}://{host}:{origin_port}", origin_url.scheme());
    let http_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_max_request_body_bytes(64 * 1024)
        .with_allowed_hosts(vec![authority])
        .with_allowed_origins(vec![exact_origin]);
    let auth = config.auth.clone();
    let server = HttpMcp {
        auth: auth.clone(),
        webhook: WebhookClient::new().map_err(|_| "webhook client unavailable")?,
        bot: BotClient::new().map_err(|_| "bot client unavailable")?,
        tool_router: HttpMcp::tool_router(),
    };
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        http_config,
    );
    let mcp = Router::new()
        .route_service("/mcp", service)
        .route_layer(middleware::from_fn_with_state(auth, authenticate));
    Ok(oauth_router(config)?.merge(mcp))
}
