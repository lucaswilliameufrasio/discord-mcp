use rmcp::{ServerHandler, ServiceExt, model::ServerConfig};

struct DiscordMcp;

impl ServerHandler for DiscordMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::default().with_instructions(
            "Discord MCP is under development. No Discord operations are available yet.",
        )
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = DiscordMcp.serve(rmcp::transport::stdio()).await?;
    server.waiting().await?;
    Ok(())
}
