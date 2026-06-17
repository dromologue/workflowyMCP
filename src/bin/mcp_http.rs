//! Remote MCP connector entry point — Streamable HTTP transport for claude.ai
//! custom connectors. Mirrors `src/main.rs` (the stdio binary) but serves the
//! same `WorkflowyMcpServer` over HTTP behind an OAuth resource-server gate.
//! See `docs/REMOTE-CONNECTOR.md` for provider + Fly.io deployment.

use std::net::SocketAddr;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;
use workflowy_mcp_server::{
    api::WorkflowyClient,
    config::validate_config,
    server::{run_http_server, HttpServerConfig, OAuthConfig},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // Logs to stderr (parity with the stdio binary + clean container capture);
    // unlike stdio, stdout is not reserved for JSON-RPC here.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .init();

    let config = validate_config().map_err(|e| anyhow::anyhow!("{}", e))?;
    tracing::info!("Configuration validated");

    let client = Arc::new(
        WorkflowyClient::new(
            config.workflowy_base_url.clone(),
            config.workflowy_api_key.clone(),
        )
        .map_err(|e| anyhow::anyhow!("{}", e))?,
    );

    let cfg = http_config_from_env()?;
    run_http_server(client, cfg).await
}

/// Assemble the HTTP server config from environment variables.
///
/// The OAuth gate **fails closed**: the issuer / JWKS URL / public base URL are
/// required unless `MCP_AUTH_DISABLED=1` is set explicitly (local testing only).
fn http_config_from_env() -> anyhow::Result<HttpServerConfig> {
    let host = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let bind_addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid BIND_ADDR/PORT ({host}:{port}): {e}"))?;

    let auth_disabled = std::env::var("MCP_AUTH_DISABLED").ok().as_deref() == Some("1");

    // Required when the gate is active (fail closed); only defaulted when
    // auth is explicitly disabled for local testing.
    let public_base_url = match env_required("MCP_PUBLIC_BASE_URL", auth_disabled) {
        Ok(v) => v,
        Err(_) if auth_disabled => format!("http://{bind_addr}"),
        Err(e) => return Err(e),
    };
    let issuer = env_required("MCP_OAUTH_ISSUER", auth_disabled)?;
    let jwks_url = env_required("MCP_OAUTH_JWKS_URL", auth_disabled)?;
    // Accepted audiences default to the resource identifier (public base +
    // /mcp), which is what an RFC 8707 resource-indicator flow stamps. Accepts
    // a comma-separated list so the operator can reconcile a provider that
    // stamps a different `aud` (e.g. a WorkOS environment default) via a
    // secret change, without a rebuild.
    let audience: Vec<String> = std::env::var("MCP_OAUTH_AUDIENCE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|x| x.to_string())
                .collect()
        })
        .unwrap_or_else(|| vec![format!("{}/mcp", public_base_url.trim_end_matches('/'))]);

    Ok(HttpServerConfig {
        bind_addr,
        oauth: OAuthConfig {
            issuer,
            jwks_url,
            audience,
            public_base_url,
        },
        allowed_hosts: split_env("MCP_ALLOWED_HOSTS"),
        allowed_origins: split_env("MCP_ALLOWED_ORIGINS"),
        auth_disabled,
    })
}

/// Read a required env var. When `lenient` (auth disabled for local testing) a
/// missing value yields an `Err` the caller may choose to default; when the
/// OAuth gate is active a missing value aborts startup (fail closed).
fn env_required(key: &str, lenient: bool) -> anyhow::Result<String> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ if lenient => anyhow::bail!("{key} unset (lenient)"),
        _ => anyhow::bail!(
            "{key} must be set to enable the OAuth gate. \
             Set MCP_AUTH_DISABLED=1 ONLY for local testing (never in a public deployment)."
        ),
    }
}

/// Parse a comma-separated env var into a list of trimmed, non-empty entries.
fn split_env(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}
