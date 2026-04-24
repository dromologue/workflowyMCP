//! Workflowy API client with retry logic and proper error handling
//! Addresses: path traversal, error context, retry handling

use crate::config::{RetryConfig, RateLimitConfig};
use crate::defaults;
use crate::error::{Result, WorkflowyError};
use crate::types::{WorkflowyNode, CreatedNode};
use crate::utils::RateLimiter;
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Result of a subtree fetch. `truncated` is true when the node cap was hit
/// and `nodes` therefore reflects a partial view of the tree.
#[derive(Debug, Clone)]
pub struct SubtreeFetch {
    pub nodes: Vec<WorkflowyNode>,
    pub truncated: bool,
    pub limit: usize,
}

pub struct WorkflowyClient {
    http_client: Client,
    base_url: String,
    api_key: String,
    retry_config: RetryConfig,
    rate_limiter: Arc<RateLimiter>,
}

impl WorkflowyClient {
    pub fn new(base_url: String, api_key: String) -> Result<Self> {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(defaults::HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| WorkflowyError::Internal(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            http_client,
            base_url,
            api_key,
            retry_config: RetryConfig::default(),
            rate_limiter: Arc::new(RateLimiter::new(RateLimitConfig::default())),
        })
    }

    // --- High-level API methods ---

    /// Get top-level nodes only (direct children of root)
    pub async fn get_top_level_nodes(&self) -> Result<Vec<WorkflowyNode>> {
        let response: serde_json::Value = self.request("GET", "/nodes", None).await?;
        let nodes: Vec<WorkflowyNode> = serde_json::from_value(
            response.get("nodes").cloned().unwrap_or(json!([]))
        ).map_err(|e| WorkflowyError::ParseError {
            reason: format!("Failed to parse nodes: {}", e),
        })?;
        Ok(nodes)
    }

    /// Get a single node by ID
    pub async fn get_node(&self, node_id: &str) -> Result<WorkflowyNode> {
        let endpoint = format!("/nodes/{}", node_id);
        let response: serde_json::Value = self.request("GET", &endpoint, None).await?;
        // API wraps single node in {"node": {...}}
        let node_value = response.get("node").cloned().unwrap_or(response);
        serde_json::from_value(node_value).map_err(|e| WorkflowyError::ParseError {
            reason: format!("Failed to parse node {}: {}", node_id, e),
        })
    }

    /// Get direct children of a node
    pub async fn get_children(&self, node_id: &str) -> Result<Vec<WorkflowyNode>> {
        let endpoint = format!("/nodes?parent_id={}", node_id);
        let response: serde_json::Value = self.request("GET", &endpoint, None).await?;
        let mut children: Vec<WorkflowyNode> = serde_json::from_value(
            response.get("nodes").cloned().unwrap_or(json!([]))
        ).map_err(|e| WorkflowyError::ParseError {
            reason: format!("Failed to parse children: {}", e),
        })?;
        // API returns parent_id as null; set it so client-side tree utilities work
        for child in &mut children {
            if child.parent_id.is_none() {
                child.parent_id = Some(node_id.to_string());
            }
        }
        Ok(children)
    }

    /// Recursively fetch a subtree rooted at `root_id` up to `max_depth` levels.
    /// If `root_id` is None, fetches from the workspace root (top-level nodes).
    /// When `root_id` is Some, the root node itself is included as the first element.
    /// Returns a [`SubtreeFetch`] with the flat node list and a `truncated` flag
    /// indicating whether the [`defaults::MAX_SUBTREE_NODES`] cap was hit.
    pub async fn get_subtree_recursive(
        &self,
        root_id: Option<&str>,
        max_depth: usize,
    ) -> Result<SubtreeFetch> {
        self.get_subtree_with_limit(root_id, max_depth, defaults::MAX_SUBTREE_NODES).await
    }

    /// Same as [`get_subtree_recursive`] but with an explicit node-count cap.
    /// Exposed mainly for tests; callers that honour the default should use
    /// [`get_subtree_recursive`].
    pub async fn get_subtree_with_limit(
        &self,
        root_id: Option<&str>,
        max_depth: usize,
        node_limit: usize,
    ) -> Result<SubtreeFetch> {
        let mut all_nodes = Vec::new();
        let truncated = match root_id {
            Some(id) => {
                let root = self.get_node(id).await?;
                all_nodes.push(root);
                let children = self.get_children(id).await?;
                self.fetch_descendants(&children, &mut all_nodes, 0, max_depth, node_limit).await?
            }
            None => {
                let top = self.get_top_level_nodes().await?;
                self.fetch_descendants(&top, &mut all_nodes, 0, max_depth, node_limit).await?
            }
        };
        Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit })
    }

    /// Fetch all nodes in the workspace tree. For large trees (250k+ nodes),
    /// prefer `get_subtree_recursive` with a specific root and depth limit.
    pub async fn get_all_nodes(&self) -> Result<SubtreeFetch> {
        self.get_subtree_recursive(None, defaults::MAX_TREE_DEPTH).await
    }

    /// Fetch descendants level-by-level, sequentially per node (rate-limited).
    /// Caps total nodes at `node_limit` to avoid runaway fetches on large trees.
    /// Returns `true` if the cap was hit and traversal stopped early.
    async fn fetch_descendants(
        &self,
        initial_nodes: &[WorkflowyNode],
        out: &mut Vec<WorkflowyNode>,
        start_depth: usize,
        max_depth: usize,
        node_limit: usize,
    ) -> Result<bool> {
        let mut current_level: Vec<WorkflowyNode> = initial_nodes.to_vec();
        let mut depth = start_depth;

        while depth < max_depth && !current_level.is_empty() {
            out.extend(current_level.iter().cloned());
            if out.len() >= node_limit {
                out.truncate(node_limit);
                warn!(limit = node_limit, "Node cap reached, subtree truncated");
                return Ok(true);
            }

            let mut next_level = Vec::new();
            let mut level_truncated = false;
            for node in &current_level {
                match self.get_children(&node.id).await {
                    Ok(children) => next_level.extend(children),
                    Err(e) => {
                        warn!(error = %e, node_id = %node.id, "Failed to fetch children, skipping branch");
                    }
                }
                if out.len() + next_level.len() >= node_limit {
                    level_truncated = true;
                    break;
                }
            }

            current_level = next_level;
            depth += 1;

            if level_truncated {
                let remaining = node_limit.saturating_sub(out.len());
                out.extend(current_level.into_iter().take(remaining));
                warn!(limit = node_limit, "Node cap reached during level fetch, subtree truncated");
                return Ok(true);
            }
        }

        // If we stopped because we hit max_depth, the final level's children
        // were not fetched; that is expected and is not a node-limit truncation.
        if !current_level.is_empty() && out.len() + current_level.len() > node_limit {
            let remaining = node_limit.saturating_sub(out.len());
            out.extend(current_level.into_iter().take(remaining));
            warn!(limit = node_limit, "Node cap reached on final level, subtree truncated");
            return Ok(true);
        }
        if !current_level.is_empty() {
            out.extend(current_level);
        }

        Ok(false)
    }

    /// Create a new node
    pub async fn create_node(
        &self,
        name: &str,
        description: Option<&str>,
        parent_id: Option<&str>,
        priority: Option<i32>,
    ) -> Result<CreatedNode> {
        let mut body = json!({ "name": name });
        if let Some(desc) = description {
            body["description"] = json!(desc);
        }
        if let Some(pid) = parent_id {
            body["parent_id"] = json!(pid);
        }
        if let Some(pri) = priority {
            body["priority"] = json!(pri);
        }
        let response: serde_json::Value = self.request("POST", "/nodes", Some(body)).await?;
        // Workflowy API returns "item_id" (not "id") for created nodes
        let id = response
            .get("item_id")
            .or_else(|| response.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| WorkflowyError::ParseError {
                reason: format!("Response missing 'item_id' field after node creation: {}", response),
            })?
            .to_owned();
        Ok(CreatedNode {
            id,
            name: name.to_string(),
            parent_id: parent_id.map(String::from),
        })
    }

    /// Edit a node's name or description
    /// Workflowy API uses POST (not PUT) for updates
    pub async fn edit_node(
        &self,
        node_id: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({});
        if let Some(n) = name {
            body["name"] = json!(n);
        }
        if let Some(d) = description {
            body["description"] = json!(d);
        }
        let endpoint = format!("/nodes/{}", node_id);
        let _: serde_json::Value = self.request("POST", &endpoint, Some(body)).await?;
        Ok(())
    }

    /// Delete a node
    pub async fn delete_node(&self, node_id: &str) -> Result<()> {
        let endpoint = format!("/nodes/{}", node_id);
        let _: serde_json::Value = self.request("DELETE", &endpoint, None).await?;
        Ok(())
    }

    /// Move a node to a new parent
    /// Workflowy API uses POST (not PUT) for move
    pub async fn move_node(
        &self,
        node_id: &str,
        new_parent_id: &str,
        priority: Option<i32>,
    ) -> Result<()> {
        let mut body = json!({ "parent_id": new_parent_id });
        if let Some(pri) = priority {
            body["priority"] = json!(pri);
        }
        let endpoint = format!("/nodes/{}/move", node_id);
        let _: serde_json::Value = self.request("POST", &endpoint, Some(body)).await?;
        Ok(())
    }

    // --- Low-level request with retry ---

    /// Make a request with rate limiting and automatic retry on retryable errors.
    /// Respects `retry_after` from 429 responses.
    pub async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T> {
        let mut attempt = 0;

        loop {
            attempt += 1;

            // Rate limit: wait for a token before each attempt
            self.rate_limiter.acquire().await;

            match self.try_request::<T>(method, endpoint, &body).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if attempt < self.retry_config.max_attempts && e.is_retryable() {
                        // Extract retry_after from 429 responses
                        let delay_ms = if let WorkflowyError::ApiError { status: 429, ref message, .. } = e {
                            self.parse_retry_after(message)
                                .unwrap_or_else(|| self.calculate_backoff(attempt))
                        } else {
                            self.calculate_backoff(attempt)
                        };
                        info!(
                            attempt = attempt,
                            delay_ms = delay_ms,
                            error = %e,
                            "Retrying request after backoff"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    } else {
                        return Err(WorkflowyError::RetryExhausted {
                            attempts: attempt,
                            reason: e.to_string(),
                        });
                    }
                }
            }
        }
    }

    /// Parse `retry_after` seconds from a 429 response body like `{"error": "...", "retry_after": 26}`
    /// Returns at least 1 second to avoid tight retry loops when retry_after is 0.
    fn parse_retry_after(&self, body: &str) -> Option<u64> {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()?
            .get("retry_after")?
            .as_u64()
            .map(|secs| secs.max(1) * 1000) // convert to ms, minimum 1 second
    }

    async fn try_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        endpoint: &str,
        body: &Option<serde_json::Value>,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, endpoint);
        debug!(url = %url, method = method, "Making API request");

        let mut req = match method.to_uppercase().as_str() {
            "GET" => self.http_client.get(&url),
            "POST" => self.http_client.post(&url),
            "PUT" => self.http_client.put(&url),
            "DELETE" => self.http_client.delete(&url),
            _ => {
                return Err(WorkflowyError::Internal(format!(
                    "Unsupported HTTP method: {}",
                    method
                )))
            }
        };

        req = req.header("Authorization", format!("Bearer {}", self.api_key));

        if let Some(body_value) = body {
            req = req.json(body_value);
        }

        let response = req
            .send()
            .await
            .map_err(WorkflowyError::HttpError)?;

        let status = response.status();

        if status.is_success() {
            response
                .json::<T>()
                .await
                .map_err(WorkflowyError::HttpError)
        } else {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            warn!(
                status = status.as_u16(),
                error = %error_text,
                "API request failed"
            );

            Err(WorkflowyError::ApiError {
                status: status.as_u16(),
                message: error_text,
                source: None,
            })
        }
    }

    /// Calculate exponential backoff with jitter (addresses retry logic improvement)
    fn calculate_backoff(&self, attempt: u32) -> u64 {
        let base_delay = self.retry_config.base_delay_ms;
        let max_delay = self.retry_config.max_delay_ms;

        let exponential = base_delay * 2_u64.pow(attempt - 1);
        let capped = exponential.min(max_delay);

        // Add jitter: ±10%
        let jitter = (capped as f64 * 0.1) as u64;
        let jitter_factor = (rand::random::<u64>() % (2 * jitter + 1)) as i64 - jitter as i64;

        ((capped as i64 + jitter_factor).max(0)) as u64
    }
}

/// Validate file path to prevent directory traversal attacks
/// Addresses: "Path traversal risk"
pub fn validate_file_path(file_path: &str, allowed_base: &str) -> Result<std::path::PathBuf> {
    use std::path::{Component, Path};

    let path = Path::new(file_path);
    let base = Path::new(allowed_base);

    // Reject paths with absolute components or parent directory references
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WorkflowyError::InvalidPath {
                    reason: "Path traversal not allowed".to_string(),
                });
            }
            _ => {}
        }
    }

    // For paths that exist, verify using canonicalize
    let resolved = base.join(path);
    
    // Only canonicalize if the path exists; otherwise do string-based validation
    if resolved.exists() {
        let canonical_base = base.canonicalize().map_err(|e| WorkflowyError::InvalidPath {
            reason: format!("Failed to resolve base path: {}", e),
        })?;

        let canonical_file = resolved.canonicalize().map_err(|e| WorkflowyError::InvalidPath {
            reason: format!("Failed to resolve file path: {}", e),
        })?;

        if !canonical_file.starts_with(&canonical_base) {
            return Err(WorkflowyError::InvalidPath {
                reason: "Path escapes allowed directory".to_string(),
            });
        }
    } else {
        // For non-existent paths, canonicalize the base and check the joined path
        let canonical_base = base.canonicalize().map_err(|e| WorkflowyError::InvalidPath {
            reason: format!("Failed to resolve base path: {}", e),
        })?;
        let normalized = canonical_base.join(path);

        let normalized_str = normalized.to_string_lossy().into_owned();
        let canonical_base_str = canonical_base.to_string_lossy().into_owned();
        if !normalized_str.starts_with(&canonical_base_str) {
            return Err(WorkflowyError::InvalidPath {
                reason: "Path escapes allowed directory".to_string(),
            });
        }
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> WorkflowyClient {
        WorkflowyClient::new("http://invalid.local".to_string(), "test".to_string())
            .expect("client builds")
    }

    #[test]
    fn test_path_traversal_rejection() {
        let result = validate_file_path("../../etc/passwd", "/home/user");
        assert!(result.is_err());
    }

    #[test]
    fn test_absolute_path_rejection() {
        let result = validate_file_path("/etc/passwd", "/home/user");
        assert!(result.is_err());
    }

    #[test]
    fn test_valid_relative_path() {
        let tmpdir = std::env::temp_dir();
        let result = validate_file_path("file.txt", tmpdir.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_calculate_backoff_grows_and_caps() {
        let client = test_client();
        let one = client.calculate_backoff(1);
        let two = client.calculate_backoff(2);
        let three = client.calculate_backoff(3);
        // Allow 10% jitter in either direction.
        assert!((800..=1200).contains(&one), "attempt 1 = {one}");
        assert!((1800..=2200).contains(&two), "attempt 2 = {two}");
        assert!((3600..=4400).contains(&three), "attempt 3 = {three}");
        // Large attempt counts must not exceed the configured cap + jitter.
        let capped = client.calculate_backoff(20);
        assert!(
            capped <= defaults::RETRY_MAX_DELAY_MS + (defaults::RETRY_MAX_DELAY_MS / 10) + 1,
            "capped backoff = {capped}",
        );
    }

    #[test]
    fn test_parse_retry_after_extracts_seconds() {
        let client = test_client();
        let body = r#"{"error":"rate limited","retry_after":26}"#;
        assert_eq!(client.parse_retry_after(body), Some(26_000));
    }

    #[test]
    fn test_parse_retry_after_minimum_one_second() {
        let client = test_client();
        // A zero retry_after should be clamped to 1s to avoid tight retry loops.
        let body = r#"{"retry_after":0}"#;
        assert_eq!(client.parse_retry_after(body), Some(1_000));
    }

    #[test]
    fn test_parse_retry_after_missing_field() {
        let client = test_client();
        assert_eq!(client.parse_retry_after("{}"), None);
        assert_eq!(client.parse_retry_after("not json"), None);
    }

    #[test]
    fn test_subtree_fetch_clone_and_debug() {
        // Guard that the public type remains trivially cloneable and printable,
        // since callers pattern-match on it across tool handlers.
        let fetch = SubtreeFetch { nodes: Vec::new(), truncated: true, limit: 10_000 };
        let cloned = fetch.clone();
        assert!(cloned.truncated);
        assert_eq!(cloned.limit, 10_000);
        assert!(format!("{:?}", cloned).contains("SubtreeFetch"));
    }
}
