//! Workflowy API client with retry logic and proper error handling
//! Addresses: path traversal, error context, retry handling

use crate::config::{RetryConfig, RateLimitConfig};
use crate::defaults;
use crate::error::{Result, WorkflowyError};
use crate::types::{WorkflowyNode, CreatedNode};
use crate::utils::{CancelGuard, RateLimiter};
use futures::stream::StreamExt;
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// True when an error looks like a 404 from the Workflowy API — used by
/// the propagation-lag retry helpers to recognise the "not found yet"
/// case worth waiting for. Matches both the direct `ApiError` form and
/// the wrapped `RetryExhausted` form (since `is_retryable` returns false
/// for 404, retry exhaustion fires after one attempt).
fn is_404_like(e: &WorkflowyError) -> bool {
    let text = e.to_string().to_lowercase();
    text.contains("api error 404") || text.contains("not found")
}

/// True when an error looks like the upstream complaining about a
/// stale or missing parent reference during a move. Used by
/// [`WorkflowyClient::move_node`] to decide whether to retry-with-refresh.
/// Matches conservatively: parent-related errors are well-defined as
/// `RetryExhausted` wrapping a 4xx whose body mentions "parent" or
/// "not found"; a 5xx is left to the normal retry path. This is a
/// heuristic — if Workflowy adds a structured error code we should
/// switch to that, but until then a string match is the only signal we
/// have.
fn is_parent_related_error(e: &WorkflowyError) -> bool {
    let text = e.to_string().to_lowercase();
    if !text.contains("parent") && !text.contains("not found") {
        return false;
    }
    // Don't retry on 5xx — that's already covered by the request layer's
    // exponential backoff. The error string format is "API error 500:
    // ..." (see WorkflowyError::ApiError Display impl), so match on
    // "api error 5" (already lowercased) to detect server-side errors.
    if text.contains("api error 5") {
        return false;
    }
    true
}

/// Poll a [`CancelGuard`] until it flips to cancelled. Used inside
/// `tokio::select!` to race against an in-flight HTTP send, so a `cancel_all`
/// drops the connection within ~50 ms instead of waiting for the request to
/// complete on its own. Pulls outside the request module so the rate limiter
/// uses the same poll cadence.
async fn wait_for_cancel(guard: &CancelGuard) {
    use std::time::Duration;
    loop {
        if guard.is_cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Sleep with cancellation support. Returns `true` if the full duration
/// elapsed, `false` if cancellation was observed mid-sleep. Used for the
/// inter-attempt backoff so a cancel during retry-wait doesn't pin the
/// task for `retry_after` seconds.
async fn sleep_cancellable(duration: std::time::Duration, cancel: Option<&CancelGuard>) -> bool {
    match cancel {
        Some(g) => {
            tokio::select! {
                biased;
                _ = wait_for_cancel(g) => false,
                _ = tokio::time::sleep(duration) => true,
            }
        }
        None => {
            tokio::time::sleep(duration).await;
            true
        }
    }
}

/// Why a subtree fetch returned partial data, when it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationReason {
    /// [`defaults::MAX_SUBTREE_NODES`] cap was hit.
    NodeLimit,
    /// Wall-clock budget elapsed before the walk completed.
    Timeout,
    /// A [`CancelGuard`] reported cancellation mid-walk.
    Cancelled,
}

impl TruncationReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            TruncationReason::NodeLimit => "node_limit",
            TruncationReason::Timeout => "timeout",
            TruncationReason::Cancelled => "cancelled",
        }
    }
}

/// Internal return from [`WorkflowyClient::fetch_descendants`]. Carries both
/// the truncation reason (if any) and the parent node ID whose subtree was
/// not fully drained, so the outer call can surface a path to the caller.
#[derive(Debug, Clone, Default)]
struct TruncationOutcome {
    reason: Option<TruncationReason>,
    truncated_at_node_id: Option<String>,
}

impl TruncationOutcome {
    fn complete() -> Self {
        Self::default()
    }

    fn stopped(reason: TruncationReason, anchor: Option<String>) -> Self {
        Self {
            reason: Some(reason),
            truncated_at_node_id: anchor,
        }
    }
}

/// Result of a subtree fetch. `truncated` is true when the walk stopped
/// early; `truncation_reason` explains why.
#[derive(Debug, Clone)]
pub struct SubtreeFetch {
    pub nodes: Vec<WorkflowyNode>,
    pub truncated: bool,
    pub limit: usize,
    pub truncation_reason: Option<TruncationReason>,
    /// Wall-clock duration of the walk.
    pub elapsed_ms: u64,
    /// When truncated mid-level, the ID of the parent whose children walk was
    /// cut short. Callers can resolve this against `nodes` to display a path
    /// so the next call can re-scope intelligently. `None` when the walk
    /// completed or when truncation fired before any level was started.
    pub truncated_at_node_id: Option<String>,
}

/// Optional controls for a subtree walk: deadline budget and a cancellation
/// guard that gets checked between levels and between child-fetch batches.
#[derive(Debug, Clone, Default)]
pub struct FetchControls {
    pub deadline: Option<Instant>,
    pub cancel: Option<CancelGuard>,
}

impl FetchControls {
    pub fn with_deadline(deadline: Instant) -> Self {
        Self { deadline: Some(deadline), cancel: None }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self::with_deadline(Instant::now() + timeout)
    }

    pub fn and_cancel(mut self, guard: CancelGuard) -> Self {
        self.cancel = Some(guard);
        self
    }

    fn status(&self) -> Option<TruncationReason> {
        if let Some(cancel) = &self.cancel {
            if cancel.is_cancelled() {
                return Some(TruncationReason::Cancelled);
            }
        }
        if let Some(deadline) = self.deadline {
            if Instant::now() >= deadline {
                return Some(TruncationReason::Timeout);
            }
        }
        None
    }
}

/// One create operation for [`WorkflowyClient::batch_create_nodes`].
#[derive(Debug, Clone)]
pub struct BatchCreateOp {
    pub name: String,
    pub description: Option<String>,
    pub parent_id: Option<String>,
    pub priority: Option<i32>,
}

/// Snapshot of upstream rate-limit headers as of the last response. `i64`
/// because Workflowy may not send these headers at all — we use `-1` to
/// mean "never seen one yet". An `Option`-based design would require a
/// `Mutex`; atomics let `workflowy_status` read without contention.
#[derive(Debug, Clone)]
pub struct RateLimitSnapshot {
    pub remaining: Option<i64>,
    pub reset_unix_seconds: Option<i64>,
    pub limit: Option<i64>,
}

pub struct WorkflowyClient {
    http_client: Client,
    base_url: String,
    api_key: String,
    retry_config: RetryConfig,
    rate_limiter: Arc<RateLimiter>,
    /// Elapsed milliseconds of the last successful request. `0` if no
    /// request has completed yet.
    last_request_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Most recent upstream RateLimit-Remaining header, or `-1` if not
    /// observed. Workflowy's API may not send these — readers must treat
    /// `-1` as "unknown", not "zero".
    rate_limit_remaining: Arc<std::sync::atomic::AtomicI64>,
    rate_limit_limit: Arc<std::sync::atomic::AtomicI64>,
    rate_limit_reset_unix: Arc<std::sync::atomic::AtomicI64>,
}

impl WorkflowyClient {
    pub fn new(base_url: String, api_key: String) -> Result<Self> {
        use std::sync::atomic::AtomicI64;
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
            last_request_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rate_limit_remaining: Arc::new(AtomicI64::new(-1)),
            rate_limit_limit: Arc::new(AtomicI64::new(-1)),
            rate_limit_reset_unix: Arc::new(AtomicI64::new(-1)),
        })
    }

    /// Last successful request's wall-clock duration in ms. `0` until the
    /// first request completes.
    pub fn last_request_ms(&self) -> u64 {
        self.last_request_ms.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Snapshot of the most recent upstream rate-limit headers. Each field
    /// is `None` if Workflowy has not (yet) returned that header — callers
    /// must not assume the upstream sends them.
    pub fn rate_limit_snapshot(&self) -> RateLimitSnapshot {
        use std::sync::atomic::Ordering;
        fn opt(v: i64) -> Option<i64> { if v < 0 { None } else { Some(v) } }
        RateLimitSnapshot {
            remaining: opt(self.rate_limit_remaining.load(Ordering::Relaxed)),
            limit: opt(self.rate_limit_limit.load(Ordering::Relaxed)),
            reset_unix_seconds: opt(self.rate_limit_reset_unix.load(Ordering::Relaxed)),
        }
    }

    fn record_rate_limit_headers(&self, headers: &reqwest::header::HeaderMap) {
        use std::sync::atomic::Ordering;
        // Workflowy's docs do not pin a specific header set; capture both
        // the IETF standard names (`RateLimit-*`) and the common GitHub-style
        // `X-RateLimit-*` so we work against either convention without
        // needing schema confirmation up front.
        for (name, atomic) in [
            ("ratelimit-remaining", &self.rate_limit_remaining),
            ("x-ratelimit-remaining", &self.rate_limit_remaining),
            ("ratelimit-limit", &self.rate_limit_limit),
            ("x-ratelimit-limit", &self.rate_limit_limit),
            ("ratelimit-reset", &self.rate_limit_reset_unix),
            ("x-ratelimit-reset", &self.rate_limit_reset_unix),
        ] {
            if let Some(v) = headers.get(name).and_then(|h| h.to_str().ok()).and_then(|s| s.parse::<i64>().ok()) {
                atomic.store(v.max(0), Ordering::Relaxed);
            }
        }
    }

    // --- High-level API methods ---

    /// Get top-level nodes only (direct children of root)
    pub async fn get_top_level_nodes(&self) -> Result<Vec<WorkflowyNode>> {
        self.get_top_level_nodes_cancellable(None).await
    }

    /// Cancellable variant. Pass `Some(guard)` so a `cancel_all` interrupts the
    /// rate-limit wait and the in-flight HTTP request rather than waiting for a
    /// checkpoint.
    pub async fn get_top_level_nodes_cancellable(
        &self,
        cancel: Option<&CancelGuard>,
    ) -> Result<Vec<WorkflowyNode>> {
        let response: serde_json::Value = self.request_cancellable("GET", "/nodes", None, cancel).await?;
        let nodes: Vec<WorkflowyNode> = serde_json::from_value(
            response.get("nodes").cloned().unwrap_or(json!([]))
        ).map_err(|e| WorkflowyError::ParseError {
            reason: format!("Failed to parse nodes: {}", e),
        })?;
        Ok(nodes)
    }

    /// Get a single node by ID
    pub async fn get_node(&self, node_id: &str) -> Result<WorkflowyNode> {
        self.get_node_cancellable(node_id, None).await
    }

    /// Get a node tolerating propagation lag.
    ///
    /// Workflowy's API has been observed to return a node ID via a parent's
    /// children listing before that node is queryable directly — typical
    /// eventual-consistency behaviour. The standard `get_node` returns a
    /// hard 404 in that window. This variant retries up to `MAX_PROP_RETRIES`
    /// times with exponential backoff (200 / 400 / 800 ms) so callers don't
    /// have to re-implement the wait themselves. Other errors propagate
    /// immediately.
    pub async fn get_node_with_propagation_retry(&self, node_id: &str) -> Result<WorkflowyNode> {
        const MAX_PROP_RETRIES: u32 = 3;
        let mut attempt: u32 = 0;
        loop {
            match self.get_node_cancellable(node_id, None).await {
                Ok(n) => return Ok(n),
                Err(e) if is_404_like(&e) && attempt + 1 < MAX_PROP_RETRIES => {
                    let delay_ms = 200u64 * (1u64 << attempt);
                    tracing::info!(
                        node_id = %node_id,
                        attempt = attempt + 1,
                        delay_ms,
                        "get_node 404 — retrying for propagation lag"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Children-listing variant of [`Self::get_node_with_propagation_retry`].
    /// Same retry policy: 404 means upstream may not have caught up yet.
    pub async fn get_children_with_propagation_retry(
        &self,
        node_id: &str,
    ) -> Result<Vec<WorkflowyNode>> {
        const MAX_PROP_RETRIES: u32 = 3;
        let mut attempt: u32 = 0;
        loop {
            match self.get_children_cancellable(node_id, None).await {
                Ok(c) => return Ok(c),
                Err(e) if is_404_like(&e) && attempt + 1 < MAX_PROP_RETRIES => {
                    let delay_ms = 200u64 * (1u64 << attempt);
                    tracing::info!(
                        node_id = %node_id,
                        attempt = attempt + 1,
                        delay_ms,
                        "get_children 404 — retrying for propagation lag"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn get_node_cancellable(
        &self,
        node_id: &str,
        cancel: Option<&CancelGuard>,
    ) -> Result<WorkflowyNode> {
        let endpoint = format!("/nodes/{}", node_id);
        let response: serde_json::Value = self.request_cancellable("GET", &endpoint, None, cancel).await?;
        // API wraps single node in {"node": {...}}
        let node_value = response.get("node").cloned().unwrap_or(response);
        serde_json::from_value(node_value).map_err(|e| WorkflowyError::ParseError {
            reason: format!("Failed to parse node {}: {}", node_id, e),
        })
    }

    /// Get direct children of a node
    pub async fn get_children(&self, node_id: &str) -> Result<Vec<WorkflowyNode>> {
        self.get_children_cancellable(node_id, None).await
    }

    pub async fn get_children_cancellable(
        &self,
        node_id: &str,
        cancel: Option<&CancelGuard>,
    ) -> Result<Vec<WorkflowyNode>> {
        let endpoint = format!("/nodes?parent_id={}", node_id);
        let response: serde_json::Value = self.request_cancellable("GET", &endpoint, None, cancel).await?;
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
    /// Uses the default node-limit cap and wall-clock timeout.
    pub async fn get_subtree_recursive(
        &self,
        root_id: Option<&str>,
        max_depth: usize,
    ) -> Result<SubtreeFetch> {
        let controls = FetchControls::with_timeout(Duration::from_millis(
            defaults::SUBTREE_FETCH_TIMEOUT_MS,
        ));
        self.get_subtree_with_controls(root_id, max_depth, defaults::MAX_SUBTREE_NODES, controls).await
    }

    /// Legacy entry point used by tests — same as [`get_subtree_recursive`]
    /// but with an explicit node-count cap and no wall-clock budget.
    pub async fn get_subtree_with_limit(
        &self,
        root_id: Option<&str>,
        max_depth: usize,
        node_limit: usize,
    ) -> Result<SubtreeFetch> {
        self.get_subtree_with_controls(root_id, max_depth, node_limit, FetchControls::default()).await
    }

    /// Core subtree walker: honours both the node-count cap and the caller's
    /// deadline/cancellation controls. Returns a partial [`SubtreeFetch`] on
    /// timeout or cancellation rather than erroring, so callers can surface
    /// whatever was collected.
    pub async fn get_subtree_with_controls(
        &self,
        root_id: Option<&str>,
        max_depth: usize,
        node_limit: usize,
        controls: FetchControls,
    ) -> Result<SubtreeFetch> {
        let started = Instant::now();
        let mut all_nodes = Vec::new();
        let outcome: TruncationOutcome;

        // Bail early if cancellation/deadline already fired before we made a request.
        if let Some(status) = controls.status() {
            return Ok(SubtreeFetch {
                nodes: all_nodes,
                truncated: true,
                limit: node_limit,
                truncation_reason: Some(status),
                elapsed_ms: started.elapsed().as_millis() as u64,
                // If the caller scoped to a specific root, surface it as the
                // anchor so the banner can still display a path even when no
                // nodes were fetched.
                truncated_at_node_id: root_id.map(str::to_string),
            });
        }

        let cancel_ref = controls.cancel.as_ref();

        match root_id {
            Some(id) => {
                match self.get_node_cancellable(id, cancel_ref).await {
                    Ok(root) => all_nodes.push(root),
                    Err(WorkflowyError::Cancelled) => {
                        return Ok(SubtreeFetch {
                            nodes: all_nodes,
                            truncated: true,
                            limit: node_limit,
                            truncation_reason: Some(TruncationReason::Cancelled),
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            truncated_at_node_id: Some(id.to_string()),
                        });
                    }
                    Err(e) => return Err(e),
                }
                if all_nodes.len() >= node_limit {
                    return Ok(SubtreeFetch {
                        nodes: all_nodes,
                        truncated: true,
                        limit: node_limit,
                        truncation_reason: Some(TruncationReason::NodeLimit),
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        truncated_at_node_id: Some(id.to_string()),
                    });
                }
                if let Some(status) = controls.status() {
                    outcome = TruncationOutcome::stopped(status, Some(id.to_string()));
                } else {
                    match self.get_children_cancellable(id, cancel_ref).await {
                        Ok(children) => {
                            outcome = self
                                .fetch_descendants(&children, &mut all_nodes, 0, max_depth, node_limit, &controls)
                                .await?;
                        }
                        Err(WorkflowyError::Cancelled) => {
                            outcome = TruncationOutcome::stopped(
                                TruncationReason::Cancelled,
                                Some(id.to_string()),
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            None => {
                match self.get_top_level_nodes_cancellable(cancel_ref).await {
                    Ok(top) => {
                        outcome = self
                            .fetch_descendants(&top, &mut all_nodes, 0, max_depth, node_limit, &controls)
                            .await?;
                    }
                    Err(WorkflowyError::Cancelled) => {
                        outcome = TruncationOutcome::stopped(TruncationReason::Cancelled, None);
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        Ok(SubtreeFetch {
            nodes: all_nodes,
            truncated: outcome.reason.is_some(),
            limit: node_limit,
            truncation_reason: outcome.reason,
            elapsed_ms: started.elapsed().as_millis() as u64,
            truncated_at_node_id: outcome.truncated_at_node_id,
        })
    }

    /// Fetch all nodes in the workspace tree. For large trees (250k+ nodes),
    /// prefer `get_subtree_recursive` with a specific root and depth limit.
    pub async fn get_all_nodes(&self) -> Result<SubtreeFetch> {
        self.get_subtree_recursive(None, defaults::MAX_TREE_DEPTH).await
    }

    /// Fetch descendants level-by-level, parallelising per-level child fetches
    /// up to [`defaults::SUBTREE_FETCH_CONCURRENCY`]. The rate-limiter serialises
    /// each HTTP call internally, so this parallelism eliminates RTT stalls
    /// without exceeding the sustained rate. Returns the truncation reason
    /// and, when truncation fired mid-level, the parent node ID whose
    /// children were not fully drained — callers can resolve that against
    /// `out` to display a path.
    async fn fetch_descendants(
        &self,
        initial_nodes: &[WorkflowyNode],
        out: &mut Vec<WorkflowyNode>,
        start_depth: usize,
        max_depth: usize,
        node_limit: usize,
        controls: &FetchControls,
    ) -> Result<TruncationOutcome> {
        let mut current_level: Vec<WorkflowyNode> = initial_nodes.to_vec();
        let mut depth = start_depth;
        let cancel_ref = controls.cancel.as_ref();

        while depth < max_depth && !current_level.is_empty() {
            // Accumulate this level before descending. If it blows the cap,
            // record a node-limit truncation and stop. The first node we
            // could not fit is the truncation anchor — caller can show its
            // parent path to make the cut visible.
            for (idx, node) in current_level.iter().enumerate() {
                if out.len() >= node_limit {
                    warn!(limit = node_limit, "Node cap reached, subtree truncated");
                    return Ok(TruncationOutcome::stopped(
                        TruncationReason::NodeLimit,
                        current_level.get(idx).and_then(|n| n.parent_id.clone()),
                    ));
                }
                out.push(node.clone());
            }

            if let Some(status) = controls.status() {
                return Ok(TruncationOutcome::stopped(
                    status,
                    current_level.first().and_then(|n| n.parent_id.clone()),
                ));
            }

            let concurrency = defaults::SUBTREE_FETCH_CONCURRENCY.max(1);
            let ids: Vec<String> = current_level.iter().map(|n| n.id.clone()).collect();
            let fetches = futures::stream::iter(ids.into_iter().map(|id| async move {
                let res = self.get_children_cancellable(&id, cancel_ref).await;
                (id, res)
            }))
            .buffer_unordered(concurrency);

            tokio::pin!(fetches);

            let mut next_level: Vec<WorkflowyNode> = Vec::new();
            let mut stop: Option<TruncationReason> = None;
            // Track which parents we have *not* yet drained, so when we stop
            // mid-level we can name the first unfinished branch.
            let mut pending_parents: std::collections::HashSet<String> =
                current_level.iter().map(|n| n.id.clone()).collect();
            let mut last_unfinished: Option<String> = None;
            while let Some((id, res)) = fetches.next().await {
                pending_parents.remove(&id);
                match res {
                    Ok(children) => next_level.extend(children),
                    Err(WorkflowyError::Cancelled) => {
                        stop = Some(TruncationReason::Cancelled);
                        last_unfinished = Some(id);
                        break;
                    }
                    Err(e) => warn!(error = %e, node_id = %id, "Failed to fetch children, skipping branch"),
                }
                if out.len() + next_level.len() >= node_limit {
                    stop = Some(TruncationReason::NodeLimit);
                    break;
                }
                if let Some(status) = controls.status() {
                    stop = Some(status);
                    break;
                }
            }
            drop(fetches);

            if let Some(reason) = stop {
                let remaining = node_limit.saturating_sub(out.len());
                out.extend(next_level.into_iter().take(remaining));
                if reason == TruncationReason::NodeLimit {
                    warn!(limit = node_limit, "Node cap reached during level fetch, subtree truncated");
                }
                let truncated_at = last_unfinished
                    .or_else(|| pending_parents.into_iter().next())
                    .or_else(|| current_level.first().map(|n| n.id.clone()));
                return Ok(TruncationOutcome::stopped(reason, truncated_at));
            }

            current_level = next_level;
            depth += 1;
        }

        // If we stopped because we hit max_depth, the final level's children
        // were not fetched; that is expected and is not a cap truncation.
        if !current_level.is_empty() {
            let remaining = node_limit.saturating_sub(out.len());
            if current_level.len() > remaining {
                let anchor = current_level.get(remaining).and_then(|n| n.parent_id.clone());
                out.extend(current_level.into_iter().take(remaining));
                warn!(limit = node_limit, "Node cap reached on final level, subtree truncated");
                return Ok(TruncationOutcome::stopped(TruncationReason::NodeLimit, anchor));
            }
            out.extend(current_level);
        }

        Ok(TruncationOutcome::complete())
    }

    /// Pipelined batch creator. Submits `operations.len()` create calls
    /// concurrently (bounded by [`defaults::SUBTREE_FETCH_CONCURRENCY`])
    /// and returns the resulting `CreatedNode` values **in input order**.
    /// Per-operation failures appear as `Err` in the output; a successful
    /// op returns `Ok(CreatedNode)`. The caller decides whether partial
    /// success is acceptable.
    ///
    /// Not transactional — Workflowy's REST surface does not expose
    /// multi-op transactions, so any rollback semantics belong in
    /// higher-level handlers (see `transaction` tool).
    pub async fn batch_create_nodes(
        &self,
        operations: Vec<BatchCreateOp>,
    ) -> Vec<Result<CreatedNode>> {
        let concurrency = defaults::SUBTREE_FETCH_CONCURRENCY.max(1);
        // Stream with index so we can sort back into input order — the
        // futures may complete out of order under buffer_unordered.
        let stream = futures::stream::iter(operations.into_iter().enumerate().map(
            |(idx, op)| async move {
                let res = self
                    .create_node(
                        &op.name,
                        op.description.as_deref(),
                        op.parent_id.as_deref(),
                        op.priority,
                    )
                    .await;
                (idx, res)
            },
        ))
        .buffer_unordered(concurrency);

        let mut collected: Vec<(usize, Result<CreatedNode>)> = Vec::new();
        futures::pin_mut!(stream);
        use futures::StreamExt;
        while let Some(item) = stream.next().await {
            collected.push(item);
        }
        collected.sort_by_key(|(i, _)| *i);
        collected.into_iter().map(|(_, r)| r).collect()
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

    /// Edit a node's name and/or description.
    ///
    /// **Wire behaviour.** When both `name` and `description` are
    /// supplied, the server issues two separate `POST /nodes/{id}`
    /// requests — one per field — instead of a single combined PATCH.
    /// This works around an observed upstream issue where a combined
    /// payload would intermittently lose one field; splitting costs an
    /// extra round-trip but produces deterministic results. The
    /// Workflowy MCP skill's wflow workaround documented this, and Pass
    /// 5 of the reliability plan moves the fix server-side so callers
    /// don't have to think about it.
    ///
    /// Workflowy uses POST (not PUT) for updates.
    pub async fn edit_node(
        &self,
        node_id: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        let endpoint = format!("/nodes/{}", node_id);

        if name.is_some() && description.is_some() {
            // Split: name first, then description. Both must succeed.
            let name_body = json!({ "name": name.unwrap() });
            let _: serde_json::Value = self.request("POST", &endpoint, Some(name_body)).await?;
            let desc_body = json!({ "description": description.unwrap() });
            let _: serde_json::Value = self.request("POST", &endpoint, Some(desc_body)).await?;
            return Ok(());
        }

        // Single-field update: combined payload is safe.
        let mut body = json!({});
        if let Some(n) = name {
            body["name"] = json!(n);
        }
        if let Some(d) = description {
            body["description"] = json!(d);
        }
        let _: serde_json::Value = self.request("POST", &endpoint, Some(body)).await?;
        Ok(())
    }

    /// Delete a node
    pub async fn delete_node(&self, node_id: &str) -> Result<()> {
        let endpoint = format!("/nodes/{}", node_id);
        let _: serde_json::Value = self.request("DELETE", &endpoint, None).await?;
        Ok(())
    }

    /// Move a node to a new parent.
    ///
    /// Workflowy uses POST (not PUT) for move. On a parent-related 4xx
    /// (e.g. "parent not found", "stale parent"), retry once after
    /// re-fetching the new parent's children listing — the wflow skill
    /// documented this as needed because IDs handed back by the server
    /// can stale faster than callers can use them. The retry refreshes
    /// any in-memory state the upstream relies on without changing the
    /// caller-visible contract: a single move request still returns
    /// either Ok or one Err.
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

        match self.request::<serde_json::Value>("POST", &endpoint, Some(body.clone())).await {
            Ok(_) => Ok(()),
            Err(e) if is_parent_related_error(&e) => {
                // Refresh the new parent's children listing (which forces
                // upstream to re-evaluate its view of the parent's state),
                // then retry the move once. If the second attempt also
                // fails, surface that error unchanged.
                let _ = self.get_children(new_parent_id).await;
                let _: serde_json::Value = self.request("POST", &endpoint, Some(body)).await?;
                Ok(())
            }
            Err(e) => Err(e),
        }
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
        self.request_cancellable(method, endpoint, body, None).await
    }

    /// Cancellable variant. When `cancel` is `Some`, cancellation interrupts:
    /// (a) the rate-limiter wait,
    /// (b) the in-flight HTTP send,
    /// (c) the inter-attempt backoff sleep.
    /// In each case the function returns [`WorkflowyError::Cancelled`] without
    /// holding tokens or workers, so a `cancel_all` actually frees the shared
    /// `RateLimiter` for new tool calls.
    pub async fn request_cancellable<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<serde_json::Value>,
        cancel: Option<&CancelGuard>,
    ) -> Result<T> {
        let mut attempt = 0;

        loop {
            attempt += 1;

            if let Some(g) = cancel {
                if g.is_cancelled() {
                    return Err(WorkflowyError::Cancelled);
                }
            }

            // Rate limit: wait for a token, but bail if cancellation flips
            // while we are queued behind earlier waiters.
            match cancel {
                Some(g) => {
                    if !self.rate_limiter.acquire_cancellable(g).await {
                        return Err(WorkflowyError::Cancelled);
                    }
                }
                None => self.rate_limiter.acquire().await,
            }

            match self.try_request_cancellable::<T>(method, endpoint, &body, cancel).await {
                Ok(result) => return Ok(result),
                Err(WorkflowyError::Cancelled) => return Err(WorkflowyError::Cancelled),
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
                        if !sleep_cancellable(Duration::from_millis(delay_ms), cancel).await {
                            return Err(WorkflowyError::Cancelled);
                        }
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

    async fn try_request_cancellable<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        endpoint: &str,
        body: &Option<serde_json::Value>,
        cancel: Option<&CancelGuard>,
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

        let send_started = Instant::now();
        // Race the in-flight HTTP send against a cancellation poll. Dropping
        // the send future cleanly cancels the underlying connection in reqwest.
        let response = match cancel {
            Some(g) => {
                tokio::select! {
                    biased;
                    _ = wait_for_cancel(g) => return Err(WorkflowyError::Cancelled),
                    res = req.send() => res.map_err(WorkflowyError::HttpError)?,
                }
            }
            None => req.send().await.map_err(WorkflowyError::HttpError)?,
        };

        // Record latency and any rate-limit headers regardless of success —
        // a 429 still tells us something useful about upstream throttling.
        let elapsed_ms = send_started.elapsed().as_millis() as u64;
        self.last_request_ms.store(elapsed_ms, std::sync::atomic::Ordering::Relaxed);
        self.record_rate_limit_headers(response.headers());

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
        let fetch = SubtreeFetch {
            nodes: Vec::new(),
            truncated: true,
            limit: 10_000,
            truncation_reason: Some(TruncationReason::NodeLimit),
            elapsed_ms: 5,
            truncated_at_node_id: Some("anchor-123".to_string()),
        };
        let cloned = fetch.clone();
        assert!(cloned.truncated);
        assert_eq!(cloned.limit, 10_000);
        assert_eq!(cloned.truncation_reason, Some(TruncationReason::NodeLimit));
        assert!(format!("{:?}", cloned).contains("SubtreeFetch"));
    }

    #[test]
    fn test_truncation_reason_as_str() {
        assert_eq!(TruncationReason::NodeLimit.as_str(), "node_limit");
        assert_eq!(TruncationReason::Timeout.as_str(), "timeout");
        assert_eq!(TruncationReason::Cancelled.as_str(), "cancelled");
    }

    #[test]
    fn test_fetch_controls_status_cancelled() {
        use crate::utils::CancelRegistry;
        let registry = CancelRegistry::new();
        let controls = FetchControls::default().and_cancel(registry.guard());
        assert!(controls.status().is_none());
        registry.cancel_all();
        assert_eq!(controls.status(), Some(TruncationReason::Cancelled));
    }

    #[test]
    fn test_fetch_controls_status_timeout() {
        let controls = FetchControls::with_deadline(Instant::now() - Duration::from_millis(1));
        assert_eq!(controls.status(), Some(TruncationReason::Timeout));
    }

    #[test]
    fn test_fetch_controls_cancel_takes_precedence_over_timeout() {
        use crate::utils::CancelRegistry;
        let registry = CancelRegistry::new();
        let controls = FetchControls::with_deadline(Instant::now() - Duration::from_millis(1))
            .and_cancel(registry.guard());
        // Timeout already fired, but cancel should win since we check it first.
        registry.cancel_all();
        assert_eq!(controls.status(), Some(TruncationReason::Cancelled));
    }

    #[test]
    fn test_fetch_controls_no_deadline_no_cancel() {
        let controls = FetchControls::default();
        assert!(controls.status().is_none());
    }

    #[tokio::test]
    async fn test_get_subtree_honours_expired_deadline_immediately() {
        // Even against an unreachable server, an already-expired deadline must
        // return a partial Ok rather than waste an HTTP round-trip.
        let client = test_client();
        let controls = FetchControls::with_deadline(Instant::now() - Duration::from_secs(1));
        let fetch = client
            .get_subtree_with_controls(None, 3, 100, controls)
            .await
            .expect("expired deadline should return Ok(partial)");
        assert!(fetch.truncated);
        assert_eq!(fetch.truncation_reason, Some(TruncationReason::Timeout));
        assert!(fetch.nodes.is_empty());
    }

    #[tokio::test]
    async fn test_get_subtree_honours_pre_cancelled_guard() {
        use crate::utils::CancelRegistry;
        let client = test_client();
        let registry = CancelRegistry::new();
        let guard = registry.guard();
        registry.cancel_all();
        let controls = FetchControls::default().and_cancel(guard);
        let fetch = client
            .get_subtree_with_controls(None, 3, 100, controls)
            .await
            .expect("cancelled guard should return Ok(partial)");
        assert!(fetch.truncated);
        assert_eq!(fetch.truncation_reason, Some(TruncationReason::Cancelled));
        assert!(fetch.nodes.is_empty());
    }

    #[test]
    fn test_is_parent_related_error_matches_4xx_text() {
        let api404 = WorkflowyError::api_error(404, "parent not found");
        let retried = WorkflowyError::RetryExhausted {
            attempts: 3,
            reason: "API error 400: parent missing".to_string(),
        };
        let unrelated = WorkflowyError::api_error(401, "unauthorized");
        let server_err = WorkflowyError::api_error(500, "parent not found");
        assert!(is_parent_related_error(&api404), "404+parent should match");
        assert!(is_parent_related_error(&retried), "wrapped retry should match");
        assert!(!is_parent_related_error(&unrelated), "401 unrelated must not match");
        assert!(
            !is_parent_related_error(&server_err),
            "5xx is left to backoff retry, must not be re-tried by move_node refresh"
        );
    }

    #[test]
    fn test_rate_limit_snapshot_unknown_until_request() {
        let client = test_client();
        let snap = client.rate_limit_snapshot();
        assert!(snap.remaining.is_none());
        assert!(snap.limit.is_none());
        assert!(snap.reset_unix_seconds.is_none());
        assert_eq!(client.last_request_ms(), 0);
    }

    #[test]
    fn test_rate_limit_snapshot_records_observed_headers() {
        // Direct exercise of the header parser — doesn't require a live API.
        let client = test_client();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("ratelimit-remaining", "42".parse().unwrap());
        headers.insert("ratelimit-limit", "100".parse().unwrap());
        headers.insert("ratelimit-reset", "1700000000".parse().unwrap());
        client.record_rate_limit_headers(&headers);
        let snap = client.rate_limit_snapshot();
        assert_eq!(snap.remaining, Some(42));
        assert_eq!(snap.limit, Some(100));
        assert_eq!(snap.reset_unix_seconds, Some(1_700_000_000));
    }

    #[test]
    fn test_rate_limit_snapshot_accepts_x_prefixed_variants() {
        let client = test_client();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-remaining", "7".parse().unwrap());
        client.record_rate_limit_headers(&headers);
        assert_eq!(client.rate_limit_snapshot().remaining, Some(7));
    }

    #[tokio::test]
    async fn test_get_subtree_with_root_pre_cancelled_returns_anchor() {
        // When cancellation fires before the first HTTP call against a scoped
        // walk, the truncation anchor should still be the requested root so
        // the caller can render a meaningful path.
        use crate::utils::CancelRegistry;
        let client = test_client();
        let registry = CancelRegistry::new();
        let guard = registry.guard();
        registry.cancel_all();
        let controls = FetchControls::default().and_cancel(guard);
        let fetch = client
            .get_subtree_with_controls(Some("root-id"), 3, 100, controls)
            .await
            .expect("cancelled guard should return Ok(partial)");
        assert!(fetch.truncated);
        assert_eq!(fetch.truncation_reason, Some(TruncationReason::Cancelled));
        assert_eq!(fetch.truncated_at_node_id.as_deref(), Some("root-id"));
    }
}
