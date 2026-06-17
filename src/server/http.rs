//! Streamable HTTP transport for the remote claude.ai custom connector.
//!
//! Serves the exact same `WorkflowyMcpServer` tool surface as the stdio binary
//! (Claude Desktop), but over the MCP Streamable HTTP transport behind an OAuth
//! resource-server gate. The shared setup (`super::build_and_spawn`) is reused
//! verbatim so the two transports cannot drift on server construction or the
//! background name-index tasks — the same parallel-surface discipline the
//! codebase applies to the `wflow-do` CLI.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{extract::Request, middleware::Next, routing::get, Json, Router};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tracing::{info, warn};

use super::auth::{self, OAuthConfig, TokenValidator};
use super::build_and_spawn;
use crate::api::WorkflowyClient;

/// Wiring for the HTTP connector, assembled from env in `bin/mcp_http.rs`.
pub struct HttpServerConfig {
    pub bind_addr: SocketAddr,
    pub oauth: OAuthConfig,
    /// Host allow-list for the rmcp service (defends against DNS-rebinding).
    /// Empty = leave the rmcp default in place.
    pub allowed_hosts: Vec<String>,
    /// Origin allow-list for the rmcp service. Empty = leave the default.
    pub allowed_origins: Vec<String>,
    /// Bypass the bearer gate. LOCAL TESTING ONLY — logged loudly at startup.
    /// Never set in any public deployment: the tool surface includes
    /// `delete_node` / `bulk_update`.
    pub auth_disabled: bool,
}

/// Build the connector router: public discovery + health endpoints, and the
/// auth-gated MCP service nested at `/mcp`. Split out from [`run_http_server`]
/// so it is unit-testable without binding a socket.
pub fn build_router(server: super::WorkflowyMcpServer, cfg: &HttpServerConfig) -> Router {
    let mut mcp_config = StreamableHttpServerConfig::default().with_stateful_mode(true);
    if !cfg.allowed_hosts.is_empty() {
        mcp_config = mcp_config.with_allowed_hosts(cfg.allowed_hosts.clone());
    }
    if !cfg.allowed_origins.is_empty() {
        mcp_config = mcp_config.with_allowed_origins(cfg.allowed_origins.clone());
    }

    let mcp_service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        mcp_config,
    );

    let mut mcp_router = Router::new().nest_service("/mcp", mcp_service);
    if cfg.auth_disabled {
        warn!(
            "⚠ AUTH DISABLED — /mcp is UNAUTHENTICATED. Local testing only; never \
             expose this publicly (delete_node / bulk_update are reachable)."
        );
    } else {
        if cfg.oauth.allowed_subjects.is_empty() {
            warn!(
                "⚠ MCP_ALLOWED_SUBJECTS is empty — ANY token valid for the OAuth issuer is \
                 authorised for full read/write (delete_node / bulk_update are reachable). \
                 Set MCP_ALLOWED_SUBJECTS to your OAuth subject id to lock the connector to \
                 your identity; the authenticated subject is logged on each call so you can \
                 discover it."
            );
        }
        let validator = Arc::new(TokenValidator::new(cfg.oauth.clone()));
        mcp_router = mcp_router.layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let v = validator.clone();
                async move { auth::require_bearer(v, req, next).await }
            },
        ));
    }

    let meta = auth::protected_resource_metadata_json(&cfg.oauth);
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(move || {
                let meta = meta.clone();
                async move { Json(meta) }
            }),
        )
        .route("/healthz", get(|| async { "ok" }))
        .merge(mcp_router)
}

/// Build the server (with background index tasks) and serve it over Streamable
/// HTTP until the process is shut down.
pub async fn run_http_server(
    client: Arc<WorkflowyClient>,
    cfg: HttpServerConfig,
) -> anyhow::Result<()> {
    let server = build_and_spawn(client);
    let app = build_router(server, &cfg);

    let listener = tokio::net::TcpListener::bind(cfg.bind_addr).await?;
    info!(
        addr = %cfg.bind_addr,
        resource = %cfg.oauth.resource(),
        auth = if cfg.auth_disabled { "DISABLED" } else { "oauth" },
        "Workflowy MCP connector listening (Streamable HTTP at /mcp)"
    );
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::WorkflowyClient;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // oneshot

    fn test_server() -> super::super::WorkflowyMcpServer {
        let client = Arc::new(
            WorkflowyClient::new("https://example.invalid".into(), "test-key".into()).unwrap(),
        );
        super::super::WorkflowyMcpServer::new(client)
    }

    fn cfg(auth_disabled: bool) -> HttpServerConfig {
        HttpServerConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            oauth: OAuthConfig {
                issuer: "https://issuer.example.com".into(),
                jwks_url: "https://issuer.example.com/jwks".into(),
                audience: vec!["https://app.example.com/mcp".into()],
                public_base_url: "https://app.example.com".into(),
                allowed_subjects: vec![],
            },
            allowed_hosts: vec![],
            allowed_origins: vec![],
            auth_disabled,
        }
    }

    #[tokio::test]
    async fn healthz_is_public() {
        let app = build_router(test_server(), &cfg(false));
        let res = app
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_resource_metadata_is_public_and_names_issuer() {
        let app = build_router(test_server(), &cfg(false));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/.well-known/oauth-protected-resource")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Discovery metadata must be reachable WITHOUT a bearer token.
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["resource"], "https://app.example.com/mcp");
        assert_eq!(json["authorization_servers"][0], "https://issuer.example.com");
    }

    #[tokio::test]
    async fn mcp_requires_bearer_and_points_at_metadata_when_auth_enabled() {
        let app = build_router(test_server(), &cfg(false));
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let challenge = res
            .headers()
            .get("www-authenticate")
            .unwrap()
            .to_str()
            .unwrap();
        // RFC 9728 bootstrap hint so claude.ai can discover the auth server.
        assert!(challenge.contains("resource_metadata="));
        assert!(challenge.contains("/.well-known/oauth-protected-resource"));
    }

    #[tokio::test]
    async fn mcp_gate_is_removed_when_auth_disabled() {
        // With auth disabled the bearer gate is gone: a request reaches the MCP
        // service (which rejects a non-initialize POST, but NOT with 401).
        let app = build_router(test_server(), &cfg(true));
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
