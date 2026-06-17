//! OAuth 2.1 resource-server gate for the remote HTTP connector.
//!
//! claude.ai custom connectors authenticate with OAuth 2.1 (auth-code + PKCE +
//! dynamic client registration). A **managed provider** (Stytch / WorkOS /
//! Auth0 / Scalekit) is the *authorization server* — it runs the authorize /
//! token / DCR endpoints that claude.ai drives. This module is the
//! *resource-server* half: it publishes RFC 9728 protected-resource metadata so
//! the client can discover that authorization server, and it validates every
//! incoming bearer JWT against the provider's JWKS before any tool call reaches
//! the `WorkflowyMcpServer`.
//!
//! Single-tenant (Phase 1): a valid token is the *gate*; the Workflowy key is a
//! deployment secret, so the token's identity is not yet used. The `sub` claim
//! is captured anyway so Phase 2 (multi-tenant) can key per-user state on it.

use std::sync::Arc;

use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use parking_lot::RwLock;
use serde::Deserialize;
use tracing::warn;

/// OAuth configuration for the resource server. Built from env in the
/// `workflowy-mcp-http` binary; see `docs/REMOTE-CONNECTOR.md`.
#[derive(Clone, Debug)]
pub struct OAuthConfig {
    /// Authorization-server issuer URL (the managed provider). Advertised to
    /// clients via protected-resource metadata and enforced as the JWT `iss`.
    pub issuer: String,
    /// JWKS endpoint to fetch the provider's signing keys from.
    pub jwks_url: String,
    /// Accepted JWT audiences — this resource's identifier(s) (RFC 8707). A
    /// token is accepted if ANY of its `aud` values matches ANY entry here.
    /// Usually one value (`<base>/mcp`), but a list lets the operator reconcile
    /// a provider that stamps a different `aud` without a rebuild.
    pub audience: Vec<String>,
    /// Public origin of this connector (e.g. `https://app.fly.dev`, no path,
    /// no trailing slash). Used to build the `resource` identifier and the
    /// `resource_metadata` URL named in the 401 challenge.
    pub public_base_url: String,
}

impl OAuthConfig {
    fn base(&self) -> &str {
        self.public_base_url.trim_end_matches('/')
    }

    /// The MCP endpoint clients connect to — also the resource identifier.
    pub fn resource(&self) -> String {
        format!("{}/mcp", self.base())
    }

    /// RFC 9728 protected-resource metadata URL (served at the host root).
    pub fn resource_metadata_url(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource", self.base())
    }
}

/// RFC 9728 protected-resource metadata. claude.ai reads this first to discover
/// which authorization server to run the OAuth flow against.
pub fn protected_resource_metadata_json(cfg: &OAuthConfig) -> serde_json::Value {
    serde_json::json!({
        "resource": cfg.resource(),
        "authorization_servers": [cfg.issuer],
        "bearer_methods_supported": ["header"],
    })
}

#[derive(Debug, Deserialize)]
struct Claims {
    // Not used in single-tenant mode; captured so Phase 2 can key per-user
    // state on the OAuth subject.
    #[allow(dead_code)]
    sub: Option<String>,
    // `aud` may be a single string or an array per RFC 7519.
    aud: Option<Audience>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn into_vec(self) -> Vec<String> {
        match self {
            Audience::One(s) => vec![s],
            Audience::Many(v) => v,
        }
    }
}

/// Validates bearer JWTs against the provider's JWKS, caching keys in memory and
/// refetching once on a `kid` miss (covers cold start + key rotation).
pub struct TokenValidator {
    cfg: OAuthConfig,
    http: reqwest::Client,
    keys: RwLock<JwkSet>,
}

impl TokenValidator {
    pub fn new(cfg: OAuthConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
            keys: RwLock::new(JwkSet { keys: Vec::new() }),
        }
    }

    async fn fetch_jwks(&self) -> anyhow::Result<JwkSet> {
        let set = self
            .http
            .get(&self.cfg.jwks_url)
            .send()
            .await?
            .error_for_status()?
            .json::<JwkSet>()
            .await?;
        Ok(set)
    }

    /// Resolve a decoding key for `kid`, refetching JWKS once on a cache miss.
    async fn decoding_key(&self, kid: &str) -> anyhow::Result<DecodingKey> {
        if let Some(jwk) = self.keys.read().find(kid) {
            return Ok(DecodingKey::from_jwk(jwk)?);
        }
        let fresh = self.fetch_jwks().await?;
        let key = fresh
            .find(kid)
            .map(DecodingKey::from_jwk)
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("no JWKS key for kid {kid}"))?;
        *self.keys.write() = fresh;
        Ok(key)
    }

    /// Validate a raw bearer token (signature + `iss` + `aud` + expiry).
    /// Returns the `sub` claim on success.
    pub async fn validate(&self, token: &str) -> anyhow::Result<Option<String>> {
        let header = decode_header(token)?;
        // JWKS keys are asymmetric. Reject HMAC algorithms up front to remove
        // any alg-confusion ambiguity (attacker signing with the public key as
        // an HMAC secret).
        if matches!(header.alg, Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512) {
            anyhow::bail!("symmetric alg {:?} not accepted on a JWKS resource", header.alg);
        }
        let kid = header
            .kid
            .ok_or_else(|| anyhow::anyhow!("token header missing kid"))?;
        let key = self.decoding_key(&kid).await?;

        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.cfg.issuer]);
        // We validate the audience manually (below) rather than via
        // `set_audience`, so a mismatch can name the ACTUAL token `aud` in the
        // log instead of jsonwebtoken's opaque `InvalidAudience`.
        validation.validate_aud = false;
        let data = decode::<Claims>(token, &key, &validation)?;

        let token_auds = data.claims.aud.map(Audience::into_vec).unwrap_or_default();
        let matches = token_auds
            .iter()
            .any(|a| self.cfg.audience.iter().any(|expected| expected == a));
        if !matches {
            anyhow::bail!(
                "InvalidAudience: token aud {:?} matches none of accepted {:?}",
                token_auds,
                self.cfg.audience
            );
        }
        Ok(data.claims.sub)
    }
}

/// axum middleware: require a valid `Authorization: Bearer <jwt>` on the MCP
/// route. On any failure returns 401 with an RFC 9728 `WWW-Authenticate`
/// challenge pointing at the protected-resource metadata, so claude.ai can
/// bootstrap the OAuth flow.
pub async fn require_bearer(validator: Arc<TokenValidator>, req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
        })
        .map(str::trim);

    let Some(token) = token else {
        return unauthorized(&validator.cfg, "missing_token", "no bearer token");
    };

    match validator.validate(token).await {
        Ok(_sub) => next.run(req).await,
        Err(e) => {
            warn!(error = %e, "bearer token rejected");
            unauthorized(&validator.cfg, "invalid_token", "token validation failed")
        }
    }
}

fn unauthorized(cfg: &OAuthConfig, error: &str, desc: &str) -> Response {
    let challenge = format!(
        "Bearer error=\"{error}\", error_description=\"{desc}\", resource_metadata=\"{}\"",
        cfg.resource_metadata_url()
    );
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, challenge)],
        Json(serde_json::json!({ "error": error, "error_description": desc })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OAuthConfig {
        OAuthConfig {
            issuer: "https://issuer.example.com".to_string(),
            jwks_url: "https://issuer.example.com/.well-known/jwks.json".to_string(),
            audience: vec!["https://app.fly.dev/mcp".to_string()],
            public_base_url: "https://app.fly.dev/".to_string(),
        }
    }

    #[test]
    fn resource_and_metadata_urls_strip_trailing_slash() {
        let c = cfg();
        assert_eq!(c.resource(), "https://app.fly.dev/mcp");
        assert_eq!(
            c.resource_metadata_url(),
            "https://app.fly.dev/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn metadata_names_the_authorization_server() {
        let m = protected_resource_metadata_json(&cfg());
        assert_eq!(m["resource"], "https://app.fly.dev/mcp");
        assert_eq!(m["authorization_servers"][0], "https://issuer.example.com");
    }

    #[tokio::test]
    async fn rejects_token_without_kid() {
        // HS-signed token (no kid) must be rejected before any JWKS fetch.
        let v = TokenValidator::new(cfg());
        // A syntactically-valid but unsigned-by-JWKS token: header {"alg":"HS256"}.
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.c2ln";
        assert!(v.validate(token).await.is_err());
    }
}
