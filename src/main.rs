//! MCP Server entry point.

use std::sync::Arc;
use workflowy_mcp_server::{
    api::WorkflowyClient,
    config::validate_config,
    server::run_server,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing — MUST write to stderr, stdout is reserved for MCP JSON-RPC
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info"))
        )
        .with_ansi(false)
        .init();

    // Validate configuration
    let config = validate_config().map_err(|e| anyhow::anyhow!("{}", e))?;
    tracing::info!("Configuration validated");

    // Initialize Workflowy API client. The attached node cache is the SAME
    // global instance the server uses for invalidate_for_mutation, so the
    // client's listing cache and the server's write invalidation converge.
    let client = Arc::new(
        WorkflowyClient::new(
            config.workflowy_base_url.clone(),
            config.workflowy_api_key.clone(),
        )
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .with_node_cache(workflowy_mcp_server::utils::cache::get_cache()),
    );

    // Start MCP server on stdio
    run_server(client).await
}
