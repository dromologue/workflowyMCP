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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// Wall-clock unix-ms helper. Used by the upstream-liveness trackers
/// (`last_success_unix_ms`, `last_auth_failure_unix_ms`) so probe
/// responses can report a stable "ms since last good call" that
/// survives across separate request paths.
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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

/// Sleep with abort support. Returns `Ok(())` if the full duration
/// elapsed, `Err(Cancelled)` if the cancel guard flipped, or
/// `Err(Timeout)` if the wall-clock deadline elapsed first. Used for the
/// inter-attempt backoff so a cancel or deadline expiry during retry-wait
/// doesn't pin the task for `retry_after` seconds. The deadline is
/// honoured by capping the sleep slice to whatever budget remains.
async fn sleep_with_abort(
    duration: Duration,
    cancel: Option<&CancelGuard>,
    deadline: Option<Instant>,
) -> Result<()> {
    let mut target = duration;
    if let Some(dl) = deadline {
        let now = Instant::now();
        if now >= dl {
            return Err(WorkflowyError::Timeout);
        }
        target = std::cmp::min(target, dl - now);
    }
    match cancel {
        Some(g) => {
            tokio::select! {
                biased;
                _ = wait_for_cancel(g) => return Err(WorkflowyError::Cancelled),
                _ = tokio::time::sleep(target) => {}
            }
        }
        None => tokio::time::sleep(target).await,
    }
    if let Some(dl) = deadline {
        if Instant::now() >= dl {
            return Err(WorkflowyError::Timeout);
        }
    }
    Ok(())
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
    /// One or more branches were dropped because their child fetch errored
    /// (typically a 429) and a bounded retry did not recover them. The walk
    /// ran to its natural end, so it hit neither the node cap nor the
    /// deadline — but it did NOT cover the subtree, and the nodes under the
    /// dropped branches are absent from `nodes`.
    ///
    /// WHY this is a truncation reason rather than a log line: before
    /// 2026-07-16 the child-fetch error arm logged `"Failed to fetch
    /// children, skipping branch"` and carried on, and the walk still
    /// returned `complete`. A `wflow-do reindex` that ate a burst of 429s
    /// reported `complete` while silently omitting whole subtrees, so
    /// `complete` meant "not timed out" rather than "covered the subtree"
    /// and callers could not tell partial coverage from full.
    SkippedBranches,
}

impl TruncationReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            TruncationReason::NodeLimit => "node_limit",
            TruncationReason::Timeout => "timeout",
            TruncationReason::Cancelled => "cancelled",
            TruncationReason::SkippedBranches => "skipped_branches",
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
    /// IDs of parents whose child fetch errored and was dropped after the
    /// bounded retry. Non-empty means the walk did not cover the subtree
    /// even when `reason` is a stopping reason like `Timeout`.
    skipped_branches: Vec<String>,
}

impl TruncationOutcome {
    fn complete() -> Self {
        Self::default()
    }

    fn stopped(reason: TruncationReason, anchor: Option<String>) -> Self {
        Self {
            reason: Some(reason),
            truncated_at_node_id: anchor,
            skipped_branches: Vec::new(),
        }
    }

    /// Fold dropped branches into an outcome. A walk that stopped for
    /// another reason keeps that reason (the stop is the more specific
    /// fact); an otherwise-complete walk with dropped branches becomes
    /// `SkippedBranches` so it can never report `complete`.
    fn with_skipped(mut self, skipped: Vec<String>) -> Self {
        if !skipped.is_empty() {
            if self.reason.is_none() {
                self.reason = Some(TruncationReason::SkippedBranches);
                self.truncated_at_node_id = skipped.first().cloned();
            }
            self.skipped_branches = skipped;
        }
        self
    }
}

/// Map an abort-class error onto its truncation reason. Used by the
/// subtree wrap layer when a tree-walk request bails early on either
/// cancellation or deadline expiry.
fn abort_reason(e: &WorkflowyError) -> TruncationReason {
    match e {
        WorkflowyError::Cancelled => TruncationReason::Cancelled,
        WorkflowyError::Timeout => TruncationReason::Timeout,
        // The wrap layer only ever calls this on the two abort variants;
        // any other error is a bug. Default to Cancelled rather than
        // panic so a spurious caller still produces a usable response.
        _ => TruncationReason::Cancelled,
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
    /// Parent IDs whose child fetch errored and was dropped after the
    /// walk's bounded retry. Empty on a walk that reached every branch it
    /// attempted. Non-empty means `nodes` is missing whole subtrees, and
    /// the walk reports `truncated: true` regardless of whether it also
    /// hit the node cap or the deadline. Callers wanting full coverage can
    /// re-walk these IDs directly once upstream pressure clears.
    pub skipped_branches: Vec<String>,
}

/// Optional controls for a subtree walk: deadline budget and a cancellation
/// guard that gets checked between levels and between child-fetch batches.
#[derive(Debug, Clone, Default)]
pub struct FetchControls {
    pub deadline: Option<Instant>,
    pub cancel: Option<CancelGuard>,
    /// Trade latency for coverage. An interactive walk must fail fast on
    /// rate-limit pressure — a caller is waiting, and a partial answer now
    /// beats a complete one in ten minutes. An unattended batch walk (the
    /// scheduled reindex) has the opposite preference: nobody is waiting,
    /// and a walk that silently omits half the tree is worse than useless
    /// because the index it feeds cannot tell you what it is missing.
    ///
    /// Under `patient`, a walk waits out an open `retry_after` window and
    /// re-attempts dropped branches until they stop yielding progress,
    /// rather than dropping them on the first 429. Default `false` keeps
    /// every interactive caller byte-identical.
    pub patient: bool,
}

impl FetchControls {
    pub fn with_deadline(deadline: Instant) -> Self {
        Self { deadline: Some(deadline), cancel: None, patient: false }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self::with_deadline(Instant::now() + timeout)
    }

    pub fn and_cancel(mut self, guard: CancelGuard) -> Self {
        self.cancel = Some(guard);
        self
    }

    /// Opt into coverage-over-latency. See [`FetchControls::patient`].
    pub fn patient(mut self) -> Self {
        self.patient = true;
        self
    }

    /// Child-fetch fan-out width for this walk. The single source of the
    /// figure: both the per-level fan-out and the branch-retry fan-out read
    /// it here, so a patient walk cannot end up patient in one and greedy in
    /// the other. Pinned by `walk_fan_out_reads_concurrency_from_controls`.
    fn concurrency(&self) -> usize {
        if self.patient {
            defaults::PATIENT_SUBTREE_FETCH_CONCURRENCY.max(1)
        } else {
            defaults::SUBTREE_FETCH_CONCURRENCY.max(1)
        }
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

/// Posture w.r.t. the most recent upstream 429 response. Used by the
/// diagnostic probe (`probe_top_level`) to suppress further HTTP calls
/// while a `retry_after` window is still open — the 2026-05-26 incident
/// where the act of polling `workflowy_status` repeatedly consumed the
/// very quota the caller was waiting to recover. Carries the wall-clock
/// `retry_after_remaining_ms` so the status surface can expose it as a
/// typed field rather than asking the caller to regex it out of an
/// error string.
#[derive(Debug, Clone)]
pub struct RateLimitPosture {
    /// Unix-ms when the most recent 429 was observed. `None` until the
    /// first 429 lands.
    pub last_429_unix_ms: Option<u64>,
    /// `retry_after` (in ms) carried by that 429's response body, if any.
    /// Workflowy returns `{"retry_after": N}` in the body of every 429
    /// observed so far; absence means "we hit 429 but no retry hint".
    pub last_retry_after_ms: Option<u64>,
    /// Time remaining inside the open `retry_after` window, in ms. `None`
    /// means "no open window" (either no 429 ever observed, or the
    /// window has elapsed).
    pub retry_after_remaining_ms: Option<u64>,
    /// True when the window is open and `retry_after_remaining_ms > 0`.
    /// Probes consult this before issuing an HTTP call.
    pub in_retry_window: bool,
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
    /// Unix-ms of the last 2xx response. `0` until the first success.
    /// Probes use this to surface a stable "last good call" anchor so a
    /// transient 5xx/timeout doesn't look like a sustained outage.
    last_success_unix_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Unix-ms of the last 401/403 response. `0` until the first such
    /// failure. Used to drive the `authenticated` signal independently
    /// of "did the most recent probe succeed" — a transient timeout
    /// must not flip `authenticated` to false.
    last_auth_failure_unix_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Most recent upstream RateLimit-Remaining header, or `-1` if not
    /// observed. Workflowy's API may not send these — readers must treat
    /// `-1` as "unknown", not "zero".
    rate_limit_remaining: Arc<std::sync::atomic::AtomicI64>,
    rate_limit_limit: Arc<std::sync::atomic::AtomicI64>,
    rate_limit_reset_unix: Arc<std::sync::atomic::AtomicI64>,
    /// Unix-ms of the most recent 429 response. `0` until the first 429.
    /// The diagnostic probe consults this together with
    /// `last_retry_after_ms` so calls inside a known retry window return
    /// the cached posture instead of consuming more quota.
    last_429_unix_ms: Arc<std::sync::atomic::AtomicU64>,
    /// `retry_after` (in ms) carried by the most recent 429 response.
    /// `0` until the first 429 with a parseable `retry_after`. When the
    /// 429 body has no hint we still stamp `last_429_unix_ms` but leave
    /// this at 0 — callers fall back to their normal cadence.
    last_retry_after_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Optional shared node cache. When attached (production wiring), the
    /// children-listing funnels serve TTL-bounded cache hits and the write
    /// funnel invalidates affected listings pre-send. `None` (default)
    /// preserves fully-live behaviour for tests and one-shot callers.
    node_cache: Option<Arc<crate::utils::cache::NodeCache>>,
}

impl WorkflowyClient {
    pub fn new(base_url: String, api_key: String) -> Result<Self> {
        Self::new_with_configs(
            base_url,
            api_key,
            RetryConfig::default(),
            RateLimitConfig::default(),
        )
    }

    /// Construct a client with explicit retry and rate-limit configs.
    /// Production callers should use [`Self::new`] (which honours the
    /// project-wide defaults). Tests use this entry point to dial down
    /// retry attempts and dial up the rate limit so load tests run in
    /// milliseconds instead of seconds.
    pub fn new_with_configs(
        base_url: String,
        api_key: String,
        retry_config: RetryConfig,
        rate_limit_config: RateLimitConfig,
    ) -> Result<Self> {
        use std::sync::atomic::AtomicI64;
        let http_client = Client::builder()
            .timeout(Duration::from_secs(defaults::HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| WorkflowyError::Internal(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            http_client,
            base_url,
            api_key,
            retry_config,
            rate_limiter: Arc::new(RateLimiter::new(rate_limit_config)),
            last_request_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_success_unix_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_auth_failure_unix_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rate_limit_remaining: Arc::new(AtomicI64::new(-1)),
            rate_limit_limit: Arc::new(AtomicI64::new(-1)),
            rate_limit_reset_unix: Arc::new(AtomicI64::new(-1)),
            last_429_unix_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_retry_after_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            node_cache: None,
        })
    }

    /// Attach a node cache so children listings served by
    /// `get_children_cancellable` / `get_top_level_nodes_cancellable` are
    /// cached (TTL-bounded) and every write funnelled through
    /// `request_cancellable` invalidates the affected listings pre-send.
    /// Off by default: tests and short-lived callers get byte-identical
    /// live behaviour unless the wiring site opts in. The production server
    /// passes the SAME `Arc` it uses for `invalidate_for_mutation`, so the
    /// two invalidation layers converge on one cache.
    pub fn with_node_cache(mut self, cache: Arc<crate::utils::cache::NodeCache>) -> Self {
        self.node_cache = Some(cache);
        self
    }

    /// Drop the children listings a mutation can affect, from the single
    /// write funnel. `/nodes` (create) → the target parent's listing (root
    /// when absent); `/nodes/{id}` (edit/complete/delete/move) → the node's
    /// own listing, the listing displaying it, and — for a move — the new
    /// parent's listing from the body.
    fn invalidate_listings_for_write(&self, endpoint: &str, body: Option<&serde_json::Value>) {
        let Some(cache) = &self.node_cache else { return };
        if let Some(id) = endpoint.strip_prefix("/nodes/") {
            cache.invalidate_node(id);
        }
        match body.and_then(|b| b.get("parent_id")).and_then(|v| v.as_str()) {
            Some(pid) => cache.invalidate_listing(pid),
            None if endpoint == "/nodes" => cache.invalidate_listing(""),
            None => {}
        }
    }

    /// Last successful request's wall-clock duration in ms. `0` until the
    /// first request completes.
    pub fn last_request_ms(&self) -> u64 {
        self.last_request_ms.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Unix-ms of the last 2xx response, or `None` if no request has
    /// succeeded since the process started.
    pub fn last_success_unix_ms(&self) -> Option<u64> {
        let v = self.last_success_unix_ms.load(std::sync::atomic::Ordering::Relaxed);
        if v == 0 { None } else { Some(v) }
    }

    /// Milliseconds since the last 2xx response. `None` if no request has
    /// succeeded since the process started. Used by probes so a degraded
    /// response carries proof of recent upstream liveness instead of just
    /// a one-shot pass/fail.
    pub fn last_success_ms_ago(&self) -> Option<u64> {
        self.last_success_unix_ms()
            .map(|t| now_unix_ms().saturating_sub(t))
    }

    /// Unix-ms of the last 401/403 response, or `None` if no auth failure
    /// has been observed.
    pub fn last_auth_failure_unix_ms(&self) -> Option<u64> {
        let v = self.last_auth_failure_unix_ms.load(std::sync::atomic::Ordering::Relaxed);
        if v == 0 { None } else { Some(v) }
    }

    /// True iff a 401/403 has been observed within the supplied window.
    /// This is the right signal for "are we authenticated" — a probe
    /// timeout or transient 5xx must NOT flip the answer to false, since
    /// the API key is unrelated to upstream availability.
    pub fn recent_auth_failure(&self, within: Duration) -> bool {
        match self.last_auth_failure_unix_ms() {
            None => false,
            Some(t) => now_unix_ms().saturating_sub(t) <= within.as_millis() as u64,
        }
    }

    #[cfg(test)]
    pub fn _test_stamp_auth_failure(&self) {
        self.last_auth_failure_unix_ms
            .store(now_unix_ms(), std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn _test_stamp_success(&self) {
        self.last_success_unix_ms
            .store(now_unix_ms(), std::sync::atomic::Ordering::Relaxed);
    }

    /// Posture w.r.t. the most recent upstream 429. Returns the wall-clock
    /// `retry_after_remaining_ms` derived from the stamped 429 timestamp
    /// plus the parsed `retry_after` body. Used by the diagnostic probe
    /// to suppress further HTTP calls inside a known retry window, and
    /// by `workflowy_status` to surface the remaining time as a typed
    /// field. Cheap atomic reads; safe to call per request.
    pub fn rate_limit_posture(&self) -> RateLimitPosture {
        use std::sync::atomic::Ordering;
        let stamped = self.last_429_unix_ms.load(Ordering::Relaxed);
        let retry_after = self.last_retry_after_ms.load(Ordering::Relaxed);
        if stamped == 0 {
            return RateLimitPosture {
                last_429_unix_ms: None,
                last_retry_after_ms: None,
                retry_after_remaining_ms: None,
                in_retry_window: false,
            };
        }
        let retry_after_opt = if retry_after == 0 { None } else { Some(retry_after) };
        let now = now_unix_ms();
        let elapsed = now.saturating_sub(stamped);
        let remaining = retry_after_opt
            .map(|ra| ra.saturating_sub(elapsed));
        let in_window = matches!(remaining, Some(r) if r > 0);
        RateLimitPosture {
            last_429_unix_ms: Some(stamped),
            last_retry_after_ms: retry_after_opt,
            retry_after_remaining_ms: remaining.filter(|&r| r > 0),
            in_retry_window: in_window,
        }
    }

    /// Stamp a fresh 429 observation. The `retry_after_ms` argument is
    /// the parsed body hint converted to milliseconds; pass `None` when
    /// the body carried no hint (we still stamp the timestamp so the
    /// `last_429_unix_ms` track stays accurate, but skip the
    /// retry-window arithmetic). Exposed at `pub(crate)` so the request
    /// path can call it from its 429 branch; tests use the harness
    /// helper `_test_stamp_rate_limited`.
    pub(crate) fn stamp_rate_limited(&self, retry_after_ms: Option<u64>) {
        use std::sync::atomic::Ordering;
        self.last_429_unix_ms.store(now_unix_ms(), Ordering::Relaxed);
        self.last_retry_after_ms
            .store(retry_after_ms.unwrap_or(0), Ordering::Relaxed);
        // Drain the local bucket: keeping up to burst_size tokens through a
        // 429 means the instant the window clears, queued callers fire
        // back-to-back into a fresh quota and re-trip it — the observed
        // 429-storm oscillation. Post-window traffic resumes at the
        // sustained rate instead.
        self.rate_limiter.drain();
    }

    #[cfg(test)]
    pub fn _test_stamp_rate_limited(&self, retry_after_ms: Option<u64>) {
        self.stamp_rate_limited(retry_after_ms);
    }

    /// Build the synthetic 429 returned when a call is short-circuited
    /// inside an open `retry_after` window (no HTTP issued). Shared by the
    /// two suppression sites — the diagnostic probe (`probe_top_level`)
    /// and the request path (`request_cancellable`) — so the wire shape
    /// (`status: 429` + a `{"error": ..., "retry_after": N}` body) cannot
    /// drift between them. The `retry_after` is the *remaining* seconds in
    /// the open window, so a caller parsing it waits the right amount.
    /// `classify_degraded_kind` routes both the cached and live 429 paths
    /// to `rate_limited` uniformly off this shape.
    fn rate_limit_window_error(&self, posture: &RateLimitPosture, context: &str) -> WorkflowyError {
        let remaining_secs = posture
            .retry_after_remaining_ms
            .map(|ms| ms.div_ceil(1000))
            .unwrap_or(0);
        WorkflowyError::ApiError {
            status: 429,
            message: format!(
                "{{\"error\":\"rate limit exceeded ({context})\",\"retry_after\":{remaining_secs}}}"
            ),
            source: None,
        }
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
        self.get_top_level_nodes_cancellable(None, None).await
    }

    /// Cancellable variant. Pass `Some(guard)` so a `cancel_all` interrupts the
    /// rate-limit wait and the in-flight HTTP request rather than waiting for a
    /// checkpoint. Pass `Some(deadline)` to bound the wall-clock cost of the
    /// retry loop, the in-flight send, and the inter-attempt backoff.
    pub async fn get_top_level_nodes_cancellable(
        &self,
        cancel: Option<&CancelGuard>,
        deadline: Option<Instant>,
    ) -> Result<Vec<WorkflowyNode>> {
        // Cache key "" = workspace root, matching the root sentinel the
        // handlers use. The diagnostic probe does NOT route here (it calls
        // try_request_cancellable directly), so probes stay live.
        if let Some(cache) = &self.node_cache {
            if let Some(hit) = cache.children_listing("") {
                debug!("top-level listing served from cache");
                return Ok(hit);
            }
        }
        let response: serde_json::Value = self.request_cancellable("GET", "/nodes", None, cancel, deadline).await?;
        let mut nodes: Vec<WorkflowyNode> = serde_json::from_value(
            response.get("nodes").cloned().unwrap_or(json!([]))
        ).map_err(|e| WorkflowyError::ParseError {
            reason: format!("Failed to parse nodes: {}", e),
        })?;
        // Same display-order sort as get_children_cancellable so the workspace
        // root's top-level listing matches the Workflowy UI (2026-07-12 issue 3).
        Self::sort_children_by_priority(&mut nodes);
        if let Some(cache) = &self.node_cache {
            cache.insert_children_listing("", &nodes);
        }
        Ok(nodes)
    }

    /// Diagnostic probe variant: bypasses the rate limiter and the retry
    /// loop entirely. Used by `probe_upstream_with_retry` so a saturated
    /// rate limiter (e.g. behind an in-flight resolve walk) cannot wedge
    /// the health-check path. The 2026-05-19 user-report named the
    /// cascade explicitly: a resolve_link walk consuming sustained
    /// 10 rps left every queued probe blocked behind dozens of walk
    /// fetches; the 5 s probe budget elapsed in the rate-limit queue
    /// without ever hitting the network. With the bypass, the probe is
    /// a single one-shot request observable only to the upstream and to
    /// the wall-clock deadline. The volume impact on Workflowy is
    /// negligible — at most one request per probe session, and probes
    /// happen at most once per `health_check` / `workflowy_status` call.
    ///
    /// Retries are also skipped because the caller (`probe_upstream_with_retry`)
    /// owns its own two-attempt budget; nesting our retry loop inside
    /// the caller's halves the per-attempt budget and produces the
    /// "two attempts failed: Timeout | Timeout" outcome with no
    /// network round-trip ever issued.
    pub async fn probe_top_level(
        &self,
        deadline: Option<Instant>,
    ) -> Result<Vec<WorkflowyNode>> {
        // Retry-window suppression: if the most recent 429's retry_after
        // window is still open, return the cached 429 posture WITHOUT
        // issuing a real HTTP call. The 2026-05-26 incident: a Claude
        // session diagnosing rate-limit pressure observed `retry_after`
        // resetting on every `workflowy_status` poll — each probe was
        // consuming a token from the very quota the caller was waiting
        // to recover. Suppressing the probe converts the polling
        // anti-pattern into a free no-op until the window genuinely
        // clears. The synthesized `ApiError` carries the same
        // `status: 429` + `retry_after` shape `try_request_cancellable`
        // would have produced, so `classify_degraded_kind` routes it
        // to `rate_limited` without special-casing the cached path.
        let posture = self.rate_limit_posture();
        if posture.in_retry_window {
            return Err(self.rate_limit_window_error(
                &posture,
                "cached — probe suppressed inside open retry_after window",
            ));
        }
        let response: serde_json::Value = self
            .try_request_cancellable("GET", "/nodes", &None, None, deadline)
            .await?;
        let nodes: Vec<WorkflowyNode> = serde_json::from_value(
            response.get("nodes").cloned().unwrap_or(json!([])),
        )
        .map_err(|e| WorkflowyError::ParseError {
            reason: format!("Failed to parse nodes: {}", e),
        })?;
        Ok(nodes)
    }

    /// Bulk full-tree export via `GET /nodes-export` — the ENTIRE tree
    /// (every node, flat, each carrying `parent_id`) in a single call.
    ///
    /// This is the fast path for building the name index: one request
    /// instead of a level-by-level walk that must respect the 20 s subtree
    /// budget and eats 429 storms on a 100k+ node tree. On the production
    /// account the full tree is ~80 MB and serialises in ~26 s, so this
    /// call overrides the shared 30 s client timeout with the dedicated
    /// `EXPORT_TIMEOUT_SECS` ceiling.
    ///
    /// It is a batch/offline primitive, NOT an interactive MCP tool: the
    /// payload is far too large to hand an LLM, and Workflowy throttles the
    /// endpoint hard (a 65 s floor between calls — see `EXPORT_MIN_INTERVAL_MS`).
    /// Bypasses the rate limiter (one shot) and the retry loop, mirroring
    /// `probe_top_level`'s bespoke-request posture; a failed export is cheap
    /// for the caller to retry after the floor.
    pub async fn export_all(&self) -> Result<Vec<WorkflowyNode>> {
        let url = format!("{}/nodes-export", self.base_url);
        debug!(url = %url, "Requesting full nodes-export");
        let response = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .timeout(Duration::from_secs(defaults::EXPORT_TIMEOUT_SECS))
            .send()
            .await
            .map_err(WorkflowyError::HttpError)?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            if status.as_u16() == 429 {
                let retry_after_ms = self.parse_retry_after(&error_text);
                self.stamp_rate_limited(retry_after_ms);
            }
            return Err(WorkflowyError::api_error(status.as_u16(), error_text));
        }

        let parsed: crate::types::WorkflowyApiResponse = response
            .json()
            .await
            .map_err(WorkflowyError::HttpError)?;
        Ok(parsed.nodes.unwrap_or_default())
    }

    /// Base URL for BETA-only operations (native mirrors). The native-mirror
    /// endpoints and the `data.mirror` linkage are only coherent on
    /// `beta.workflowy.com`; production strips them. We derive the beta host
    /// from the configured base by swapping `workflowy.com` → `beta.workflowy.com`
    /// so a wiremock test (whose base is a localhost URL, no host swap) still
    /// targets its own mock, and an already-beta configuration is left as-is.
    fn beta_base(&self) -> String {
        if self.base_url.contains("://workflowy.com") {
            self.base_url
                .replacen("://workflowy.com", "://beta.workflowy.com", 1)
        } else {
            self.base_url.clone()
        }
    }

    /// Create a NATIVE Workflowy mirror via the beta API
    /// (`POST {beta}/nodes/{node_id}/mirror`). Unlike the convention-based
    /// `create_mirror` tool (which duplicates a node and writes a `mirror_of:`
    /// note), this creates a real mirror: editing the shared content updates
    /// the origin and every other mirror. BETA-ONLY — on the production
    /// account the resulting node renders with an empty name and no mirror
    /// metadata until Workflowy ships mirrors to production. Bypasses the
    /// rate limiter and retry loop (a single deliberate write); the caller
    /// owns the decision to issue it.
    pub async fn create_native_mirror(
        &self,
        node_id: &str,
        parent_id: &str,
        position: &str,
    ) -> Result<crate::types::CreateMirrorResponse> {
        // node_id is a UUID / short hash (hex + hyphen) — URL-safe, no encoding needed.
        let url = format!("{}/nodes/{}/mirror", self.beta_base(), node_id);
        let body = json!({ "parent_id": parent_id, "position": position });
        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(WorkflowyError::HttpError)?;
        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(WorkflowyError::api_error(status.as_u16(), error_text));
        }
        response.json().await.map_err(WorkflowyError::HttpError)
    }

    /// Remove a native mirror via the beta API
    /// (`DELETE {beta}/nodes/{mirror_id}/mirror`). Removes the mirror root
    /// only; the canonical and other mirrors are untouched. BETA-ONLY.
    pub async fn delete_native_mirror(&self, mirror_id: &str) -> Result<()> {
        let url = format!("{}/nodes/{}/mirror", self.beta_base(), mirror_id);
        let response = self
            .http_client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(WorkflowyError::HttpError)?;
        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(WorkflowyError::api_error(status.as_u16(), error_text));
        }
        Ok(())
    }

    /// Get a single node by ID
    pub async fn get_node(&self, node_id: &str) -> Result<WorkflowyNode> {
        self.get_node_cancellable(node_id, None, None).await
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
            match self.get_node_cancellable(node_id, None, None).await {
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
            match self.get_children_cancellable(node_id, None, None).await {
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
        deadline: Option<Instant>,
    ) -> Result<WorkflowyNode> {
        let endpoint = format!("/nodes/{}", node_id);
        let response: serde_json::Value = self.request_cancellable("GET", &endpoint, None, cancel, deadline).await?;
        // API wraps single node in {"node": {...}}
        let node_value = response.get("node").cloned().unwrap_or(response);
        serde_json::from_value(node_value).map_err(|e| WorkflowyError::ParseError {
            reason: format!("Failed to parse node {}: {}", node_id, e),
        })
    }

    /// Get direct children of a node
    pub async fn get_children(&self, node_id: &str) -> Result<Vec<WorkflowyNode>> {
        self.get_children_cancellable(node_id, None, None).await
    }

    /// Order a sibling set the way Workflowy displays it.
    ///
    /// WHY (2026-07-12 field report, issue 3): the `/nodes?parent_id=` endpoint
    /// returns children in an internal/creation order, *not* the outline's
    /// display order. Empirically (a live top-level read) the item Workflowy
    /// renders at the top carries the *lowest* `priority` value — ascending
    /// priority = top-of-list, consistent with `reorder_nodes`' proven
    /// `priority=0`→head semantics. Sorting every children listing through this
    /// one helper makes `list_children` and `get_subtree` agree (they both
    /// funnel through `get_children_cancellable`) AND makes both reflect the UI,
    /// so verify-after-write is reliable. Priority-less nodes (`None`) sort to
    /// the head, matching Workflowy's documented placement of priority-less
    /// creates. The sort is stable, so equal/`None` priorities keep the upstream
    /// order rather than shuffling non-deterministically.
    fn sort_children_by_priority(children: &mut [WorkflowyNode]) {
        children.sort_by_key(|n| n.priority.unwrap_or(i64::MIN));
    }

    pub async fn get_children_cancellable(
        &self,
        node_id: &str,
        cancel: Option<&CancelGuard>,
        deadline: Option<Instant>,
    ) -> Result<Vec<WorkflowyNode>> {
        // Cache-first: this is the single children-listing funnel (every
        // list_children call and every walk level routes here), so a hit
        // collapses repeated/overlapping reads within the TTL to zero API
        // calls. Writes invalidate pre-send via the request funnel, so a
        // hit can never predate an in-process mutation.
        if let Some(cache) = &self.node_cache {
            if let Some(hit) = cache.children_listing(node_id) {
                debug!(parent_id = %node_id, "children listing served from cache");
                return Ok(hit);
            }
        }
        let endpoint = format!("/nodes?parent_id={}", node_id);
        let response: serde_json::Value = self.request_cancellable("GET", &endpoint, None, cancel, deadline).await?;
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
        Self::sort_children_by_priority(&mut children);
        if let Some(cache) = &self.node_cache {
            cache.insert_children_listing(node_id, &children);
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
                // Bailed before any branch was attempted.
                skipped_branches: Vec::new(),
            });
        }

        let cancel_ref = controls.cancel.as_ref();
        let deadline = controls.deadline;

        match root_id {
            Some(id) => {
                match self.get_node_cancellable(id, cancel_ref, deadline).await {
                    Ok(root) => all_nodes.push(root),
                    Err(e) if matches!(e, WorkflowyError::Cancelled | WorkflowyError::Timeout) => {
                        return Ok(SubtreeFetch {
                            nodes: all_nodes,
                            truncated: true,
                            limit: node_limit,
                            truncation_reason: Some(abort_reason(&e)),
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            truncated_at_node_id: Some(id.to_string()),
                            skipped_branches: Vec::new(),
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
                        skipped_branches: Vec::new(),
                    });
                }
                if let Some(status) = controls.status() {
                    outcome = TruncationOutcome::stopped(status, Some(id.to_string()));
                } else {
                    match self.get_children_cancellable(id, cancel_ref, deadline).await {
                        Ok(children) => {
                            outcome = self
                                .fetch_descendants(&children, &mut all_nodes, 0, max_depth, node_limit, &controls)
                                .await?;
                        }
                        Err(e) if matches!(e, WorkflowyError::Cancelled | WorkflowyError::Timeout) => {
                            outcome = TruncationOutcome::stopped(
                                abort_reason(&e),
                                Some(id.to_string()),
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            None => {
                match self.get_top_level_nodes_cancellable(cancel_ref, deadline).await {
                    Ok(top) => {
                        outcome = self
                            .fetch_descendants(&top, &mut all_nodes, 0, max_depth, node_limit, &controls)
                            .await?;
                    }
                    Err(e) if matches!(e, WorkflowyError::Cancelled | WorkflowyError::Timeout) => {
                        outcome = TruncationOutcome::stopped(abort_reason(&e), None);
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        Ok(SubtreeFetch {
            nodes: all_nodes,
            // Dropped branches make the walk partial even when it stopped for
            // no other reason, so `truncated` keys off both facts rather than
            // off the stopping reason alone.
            truncated: outcome.reason.is_some() || !outcome.skipped_branches.is_empty(),
            limit: node_limit,
            truncation_reason: outcome.reason,
            elapsed_ms: started.elapsed().as_millis() as u64,
            truncated_at_node_id: outcome.truncated_at_node_id,
            skipped_branches: outcome.skipped_branches,
        })
    }

    /// Fetch all nodes in the workspace tree. For large trees (250k+ nodes),
    /// prefer `get_subtree_recursive` with a specific root and depth limit.
    pub async fn get_all_nodes(&self) -> Result<SubtreeFetch> {
        self.get_subtree_recursive(None, defaults::MAX_TREE_DEPTH).await
    }

    /// Fetch descendants level-by-level, parallelising per-level child fetches
    /// up to [`FetchControls::concurrency`] (which picks the interactive or
    /// the patient width). The rate-limiter serialises each HTTP call
    /// internally, so this parallelism eliminates RTT stalls without
    /// exceeding the sustained rate. Returns the truncation reason
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
        let deadline = controls.deadline;
        // Branches dropped across every level of this walk, after each
        // level's bounded retry. Non-empty => the walk did not cover the
        // subtree, whatever else it reports.
        let mut skipped_branches: Vec<String> = Vec::new();

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

            let concurrency = controls.concurrency();
            let ids: Vec<String> = current_level.iter().map(|n| n.id.clone()).collect();
            let fetches = futures::stream::iter(ids.into_iter().map(|id| async move {
                let res = self.get_children_cancellable(&id, cancel_ref, deadline).await;
                (id, res)
            }))
            .buffer_unordered(concurrency);

            tokio::pin!(fetches);

            let mut next_level: Vec<WorkflowyNode> = Vec::new();
            let mut stop: Option<TruncationReason> = None;
            // Branches this level dropped on a non-abort error. Retried once
            // below before we descend, so a transient 429 does not silently
            // remove a subtree from the walk.
            let mut level_skipped: Vec<String> = Vec::new();
            // Track which parents we have *not* yet drained, so when we stop
            // mid-level we can name the first unfinished branch.
            let mut pending_parents: std::collections::HashSet<String> =
                current_level.iter().map(|n| n.id.clone()).collect();
            let mut last_unfinished: Option<String> = None;
            while let Some((id, res)) = fetches.next().await {
                pending_parents.remove(&id);
                match res {
                    Ok(children) => next_level.extend(children),
                    Err(e) if matches!(e, WorkflowyError::Cancelled | WorkflowyError::Timeout) => {
                        stop = Some(abort_reason(&e));
                        last_unfinished = Some(id);
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, node_id = %id, "Failed to fetch children, will retry branch");
                        level_skipped.push(id);
                    }
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
                skipped_branches.extend(level_skipped);
                return Ok(TruncationOutcome::stopped(reason, truncated_at)
                    .with_skipped(skipped_branches));
            }

            // Recover branches this level dropped before descending: their
            // children are the next level's input, so a dropped branch here
            // removes its whole subtree from the walk.
            if !level_skipped.is_empty() {
                let (recovered, still_failed) =
                    self.recover_skipped_branches(level_skipped, controls).await;
                next_level.extend(recovered);
                skipped_branches.extend(still_failed);
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
                return Ok(TruncationOutcome::stopped(TruncationReason::NodeLimit, anchor)
                    .with_skipped(skipped_branches));
            }
            out.extend(current_level);
        }

        if !skipped_branches.is_empty() {
            warn!(
                branches = skipped_branches.len(),
                "subtree walk reached its end with branches dropped — coverage is partial"
            );
        }
        Ok(TruncationOutcome::complete().with_skipped(skipped_branches))
    }

    /// Recover the branches a level dropped, honouring the walk's patience
    /// setting. Returns `(recovered_children, still_failed_parent_ids)`.
    ///
    /// An impatient walk gets exactly one retry wave — a caller is waiting,
    /// and the `skipped_branches` envelope tells them honestly what was
    /// missed.
    ///
    /// A patient walk keeps going, and it has to: one wave cannot converge
    /// under sustained rate-limit pressure. `retry_skipped_branches` waits
    /// out the open window and then re-fans-out, but if any branch in that
    /// wave draws a fresh 429 the window re-opens mid-flight and every
    /// branch still queued behind it fails fast in microseconds — so one
    /// wave typically recovers a slice and re-drops the rest. That is
    /// exactly how a nightly reindex reported `complete` while omitting
    /// thousands of branches (2026-07-17). Each wave pays one window wait
    /// and recovers another slice, so the walk converges in wave-sized
    /// steps: slow, but nobody is waiting on it.
    async fn recover_skipped_branches(
        &self,
        ids: Vec<String>,
        controls: &FetchControls,
    ) -> (Vec<WorkflowyNode>, Vec<String>) {
        let (mut recovered, mut failed) = self.retry_skipped_branches(ids, controls).await;
        if !controls.patient {
            return (recovered, failed);
        }

        let mut wave = 1u32;
        while !failed.is_empty() && controls.status().is_none() {
            let before = failed.len();
            let (more, still) = self
                .retry_skipped_branches(std::mem::take(&mut failed), controls)
                .await;
            recovered.extend(more);
            failed = still;
            wave += 1;
            if failed.len() >= before {
                // A wave that recovers nothing has told us the remaining
                // branches are failing for a reason waiting cannot fix (a
                // deleted node, a permission error). Another identical wave
                // would fail identically; stop rather than spin.
                warn!(
                    branches = failed.len(),
                    waves = wave,
                    "patient walk stopped recovering branches — the remainder are not \
                     rate-limit failures and will not resolve by waiting"
                );
                break;
            }
            info!(
                recovered_so_far = recovered.len(),
                remaining = failed.len(),
                wave,
                "patient walk recovering dropped branches"
            );
        }
        (recovered, failed)
    }

    /// Re-attempt child fetches for branches a level dropped on a non-abort
    /// error. Returns `(recovered_children, still_failed_parent_ids)`.
    ///
    /// WHY the wait: the dominant cause of a dropped branch is a 429 burst,
    /// and the client already knows the upstream's `retry_after` window, so
    /// retrying immediately would only fail fast against the in-window
    /// short-circuit in `request_cancellable`. Waiting the window out is the
    /// difference between recovering the branch and dropping it.
    ///
    /// Both the wait and the retry respect the walk's deadline and cancel
    /// guard. When the budget cannot cover the wait we skip the retry and
    /// report the branches as skipped — which surfaces as
    /// `TruncationReason::SkippedBranches`, never as a silent omission.
    async fn retry_skipped_branches(
        &self,
        ids: Vec<String>,
        controls: &FetchControls,
    ) -> (Vec<WorkflowyNode>, Vec<String>) {
        if controls.status().is_some() {
            return (Vec::new(), ids);
        }

        if let Some(remaining_ms) = self.rate_limit_posture().retry_after_remaining_ms {
            let wait = Duration::from_millis(
                remaining_ms.saturating_add(defaults::RETRY_WINDOW_WAIT_SLACK_MS),
            );
            let fits_budget = controls
                .deadline
                .map(|d| Instant::now() + wait < d)
                .unwrap_or(true);
            if !fits_budget {
                warn!(
                    branches = ids.len(),
                    remaining_ms,
                    "rate-limit window outlasts the walk budget; branches dropped"
                );
                return (Vec::new(), ids);
            }
            info!(
                branches = ids.len(),
                wait_ms = wait.as_millis() as u64,
                "waiting out rate-limit window before retrying dropped branches"
            );
            tokio::time::sleep(wait).await;
            if controls.status().is_some() {
                return (Vec::new(), ids);
            }
        }

        let concurrency = controls.concurrency();
        let cancel_ref = controls.cancel.as_ref();
        let deadline = controls.deadline;
        let fetches = futures::stream::iter(ids.into_iter().map(|id| async move {
            let res = self.get_children_cancellable(&id, cancel_ref, deadline).await;
            (id, res)
        }))
        .buffer_unordered(concurrency);
        tokio::pin!(fetches);

        let mut recovered: Vec<WorkflowyNode> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        while let Some((id, res)) = fetches.next().await {
            match res {
                Ok(children) => recovered.extend(children),
                Err(e) => {
                    warn!(error = %e, node_id = %id, "Branch retry failed, skipping branch");
                    failed.push(id);
                }
            }
        }
        (recovered, failed)
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

    /// Create a new node. Bounded by [`defaults::WRITE_NODE_TIMEOUT_MS`]
    /// end-to-end — the retry loop cannot stretch a single create past
    /// this budget, which is the root cause of the 4-minute
    /// `insert_content` hangs in the 2026-05-02 report.
    pub async fn create_node(
        &self,
        name: &str,
        description: Option<&str>,
        parent_id: Option<&str>,
        priority: Option<i32>,
    ) -> Result<CreatedNode> {
        self.create_node_cancellable(name, description, parent_id, priority, None, None, None)
            .await
    }

    /// Cancellable variant of [`Self::create_node`] that accepts an
    /// explicit cancel guard and deadline. Callers that drive bulk
    /// inserts (`insert_content`) pass a per-operation deadline so an
    /// individual create can't burn the whole budget.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_node_cancellable(
        &self,
        name: &str,
        description: Option<&str>,
        parent_id: Option<&str>,
        priority: Option<i32>,
        layout: Option<&str>,
        cancel: Option<&CancelGuard>,
        deadline: Option<Instant>,
    ) -> Result<CreatedNode> {
        let mut body = json!({ "name": name });
        if let Some(desc) = description {
            // Wire field is `note`. Sending `description` is silently dropped
            // upstream — that was the real cause of the 2026-05-02 field-loss
            // symptom the brief filed as P2.4.
            body["note"] = json!(desc);
        }
        if let Some(pid) = parent_id {
            body["parent_id"] = json!(pid);
        }
        if let Some(pri) = priority {
            body["priority"] = json!(pri);
        }
        if let Some(lm) = layout {
            // Wire field is `layoutMode` (2025.19 API). Sets the node type
            // (todo/h1/code-block/…) directly instead of via a markdown prefix.
            body["layoutMode"] = json!(lm);
        }
        let effective_deadline = deadline.or_else(|| {
            Some(Instant::now() + Duration::from_millis(defaults::WRITE_NODE_TIMEOUT_MS))
        });
        let response: serde_json::Value = self
            .request_cancellable("POST", "/nodes", Some(body), cancel, effective_deadline)
            .await?;
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
    /// **Wire field names.** Workflowy's REST API names the description
    /// field `note` on the wire; the Rust surface keeps it as
    /// `description` and serde's `alias = "note"` covers the read path.
    /// Writes are hand-constructed here, so the field is mapped to `note`
    /// at the boundary. Sending `description` produces a 200 OK with the
    /// field silently dropped — that was the actual root cause of the
    /// P2.4 field-loss symptom in `briefs/workflowy-mcp-improvements.md`,
    /// not a partial-update bug.
    ///
    /// **Split-payload writes.** When both `name` and `description` are
    /// supplied the call still issues two separate `POST /nodes/{id}`
    /// requests, one per field. With the field-name bug fixed the split
    /// is no longer load-bearing for correctness, but it keeps each
    /// field's success independent so a transient failure on one cannot
    /// roll back the other.
    ///
    /// Workflowy uses POST (not PUT) for updates.
    pub async fn edit_node(
        &self,
        node_id: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        let endpoint = format!("/nodes/{}", node_id);
        // Single deadline for the whole logical edit — split-payload writes
        // share it so a flaky upstream can't double the wall-clock budget.
        let deadline = Instant::now() + Duration::from_millis(defaults::EDIT_NODE_TIMEOUT_MS);

        if name.is_some() && description.is_some() {
            // Split: name first, then description. Both must succeed.
            let name_body = json!({ "name": name.unwrap() });
            let _: serde_json::Value = self
                .request_cancellable("POST", &endpoint, Some(name_body), None, Some(deadline))
                .await?;
            let desc_body = json!({ "note": description.unwrap() });
            let _: serde_json::Value = self
                .request_cancellable("POST", &endpoint, Some(desc_body), None, Some(deadline))
                .await?;
            return Ok(());
        }

        // Single-field update: combined payload is safe.
        let mut body = json!({});
        if let Some(n) = name {
            body["name"] = json!(n);
        }
        if let Some(d) = description {
            body["note"] = json!(d);
        }
        let _: serde_json::Value = self
            .request_cancellable("POST", &endpoint, Some(body), None, Some(deadline))
            .await?;
        Ok(())
    }

    /// Toggle a node's completion state.
    ///
    /// **Wire shape.** Workflowy's read side returns `completed: bool`
    /// (no serde alias on `WorkflowyNode::completed`, so the wire field
    /// is literally `completed`) and `completedAt: i64?` (camelCase).
    /// The write payload mirrors the boolean: `POST /nodes/{id}` with
    /// `{"completed": true}` to mark complete and `{"completed": false}`
    /// to uncomplete. Pinned by `tests::write_field_names::
    /// set_completion_*` — if the wire field name shifts (the
    /// description → note bug from 2026-05-02 was the precedent), the
    /// tests fail locally without needing a live API.
    ///
    /// Bounded by [`defaults::WRITE_NODE_TIMEOUT_MS`] end-to-end. The
    /// `completedAt` timestamp is server-derived: a successful
    /// `set_completion(_, true)` causes the next read to surface a
    /// non-null `completed_at`; uncompleting clears it back to
    /// `None`. Callers that need the timestamp re-read the node via
    /// `get_node`.
    pub async fn set_completion(&self, node_id: &str, completed: bool) -> Result<()> {
        let endpoint = format!("/nodes/{}", node_id);
        let deadline = Instant::now() + Duration::from_millis(defaults::WRITE_NODE_TIMEOUT_MS);
        let body = json!({ "completed": completed });
        let _: serde_json::Value = self
            .request_cancellable("POST", &endpoint, Some(body), None, Some(deadline))
            .await?;
        Ok(())
    }

    /// Completion-toggle counterpart to
    /// [`Self::edit_node_with_propagation_retry`]. Same policy: 3
    /// attempts with 200/400/800 ms backoff on 404 only, because a
    /// freshly-created node may surface in a parent's children listing
    /// before it is mutable directly.
    pub async fn set_completion_with_propagation_retry(
        &self,
        node_id: &str,
        completed: bool,
    ) -> Result<()> {
        const MAX_PROP_RETRIES: u32 = 3;
        let mut attempt: u32 = 0;
        loop {
            match self.set_completion(node_id, completed).await {
                Ok(()) => return Ok(()),
                Err(e) if is_404_like(&e) && attempt + 1 < MAX_PROP_RETRIES => {
                    let delay_ms = 200u64 * (1u64 << attempt);
                    tracing::info!(
                        node_id = %node_id,
                        completed,
                        attempt = attempt + 1,
                        delay_ms,
                        "set_completion 404 — retrying for propagation lag"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Delete a node. Bounded by [`defaults::WRITE_NODE_TIMEOUT_MS`]
    /// end-to-end so the retry loop cannot stretch a single delete past
    /// the budget — same contract as `create_node`.
    pub async fn delete_node(&self, node_id: &str) -> Result<()> {
        let endpoint = format!("/nodes/{}", node_id);
        let deadline = Instant::now() + Duration::from_millis(defaults::WRITE_NODE_TIMEOUT_MS);
        let _: serde_json::Value = self
            .request_cancellable("DELETE", &endpoint, None, None, Some(deadline))
            .await?;
        Ok(())
    }

    /// Delete a node tolerating propagation lag (Pattern 6 from the
    /// 2026-04-25 brief). Same retry policy as
    /// [`Self::get_node_with_propagation_retry`]: 3 attempts with
    /// 200/400/800 ms backoff on 404 only. Workflowy occasionally
    /// returns a UUID via a parent's children listing or a fresh
    /// `create_node` response before the node is deletable directly;
    /// without this retry the caller sees `Tool execution failed` and
    /// is left with an unrecoverable orphan.
    pub async fn delete_node_with_propagation_retry(&self, node_id: &str) -> Result<()> {
        const MAX_PROP_RETRIES: u32 = 3;
        let mut attempt: u32 = 0;
        loop {
            match self.delete_node(node_id).await {
                Ok(()) => return Ok(()),
                Err(e) if is_404_like(&e) && attempt + 1 < MAX_PROP_RETRIES => {
                    let delay_ms = 200u64 * (1u64 << attempt);
                    tracing::info!(
                        node_id = %node_id,
                        attempt = attempt + 1,
                        delay_ms,
                        "delete_node 404 — retrying for propagation lag"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Edit-with-retry counterpart to
    /// [`Self::delete_node_with_propagation_retry`]. Same policy: 404 on
    /// the target node may mean upstream hasn't propagated a recent
    /// create yet.
    pub async fn edit_node_with_propagation_retry(
        &self,
        node_id: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        const MAX_PROP_RETRIES: u32 = 3;
        let mut attempt: u32 = 0;
        loop {
            match self.edit_node(node_id, name, description).await {
                Ok(()) => return Ok(()),
                Err(e) if is_404_like(&e) && attempt + 1 < MAX_PROP_RETRIES => {
                    let delay_ms = 200u64 * (1u64 << attempt);
                    tracing::info!(
                        node_id = %node_id,
                        attempt = attempt + 1,
                        delay_ms,
                        "edit_node 404 — retrying for propagation lag"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Move a node to a new parent.
    ///
    /// **Failure-report 2026-05-03 unification.** Until 2026-05-04 there
    /// were two move entry points on this client: a bare `move_node` that
    /// did one POST plus a parent-error refresh-and-retry, and a
    /// `move_node_with_propagation_retry` wrapper that added a 3-attempt
    /// 404 retry on top. The bare handler routed through the wrapper; the
    /// `transaction.move` op routed through the bare. The session report
    /// recorded `move_node` failing 11% of the time and `transaction`
    /// move succeeding 100% — the divergence was mechanical, not
    /// behavioural. Unified here: both kinds of propagation lag (404 on
    /// the moved node, parent-related 4xx on the new parent) are handled
    /// inside this method, so every caller — server handler, transaction
    /// op, CLI — gets the same resilience without having to remember
    /// which wrapper to call.
    ///
    /// Workflowy uses POST (not PUT) for move. The retry policy:
    /// - 404 on the moved node: up to 3 attempts with exponential backoff
    ///   (200ms / 400ms), covering the case where the node was created
    ///   moments earlier and upstream hasn't propagated the new id yet.
    /// - parent-related 4xx ("parent not found", "stale parent"): refresh
    ///   the new parent's children listing once (which forces upstream to
    ///   re-evaluate its view of parent state), then retry the same
    ///   attempt. The refresh is part of one logical attempt, not a
    ///   separate one — it doesn't consume a 404-retry slot.
    /// - 5xx and other transient errors: handled by the request layer's
    ///   own backoff, which is separate from this propagation logic.
    pub async fn move_node(
        &self,
        node_id: &str,
        new_parent_id: &str,
        priority: Option<i32>,
    ) -> Result<()> {
        const MAX_PROP_RETRIES: u32 = 3;
        let mut body = json!({ "parent_id": new_parent_id });
        if let Some(pri) = priority {
            body["priority"] = json!(pri);
        }
        let endpoint = format!("/nodes/{}/move", node_id);

        let mut attempt: u32 = 0;
        loop {
            let outcome = match self
                .request::<serde_json::Value>("POST", &endpoint, Some(body.clone()))
                .await
            {
                Ok(_) => Ok(()),
                Err(e) if is_parent_related_error(&e) => {
                    // Parent-related 4xx: refresh and retry the same logical
                    // attempt. Doesn't consume a 404-retry slot.
                    let _ = self.get_children(new_parent_id).await;
                    match self
                        .request::<serde_json::Value>("POST", &endpoint, Some(body.clone()))
                        .await
                    {
                        Ok(_) => Ok(()),
                        Err(e2) => Err(e2),
                    }
                }
                Err(e) => Err(e),
            };
            match outcome {
                Ok(()) => return Ok(()),
                Err(e) if is_404_like(&e) && attempt + 1 < MAX_PROP_RETRIES => {
                    let delay_ms = 200u64 * (1u64 << attempt);
                    tracing::info!(
                        node_id = %node_id,
                        new_parent_id = %new_parent_id,
                        attempt = attempt + 1,
                        delay_ms,
                        "move_node 404 — retrying for propagation lag"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
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
        self.request_cancellable(method, endpoint, body, None, None).await
    }

    /// Cancellable variant with optional wall-clock deadline. The deadline
    /// bounds the **entire** retry loop — not a single attempt — so even when
    /// the upstream returns a hung connection or a long sequence of 5xx
    /// responses, the call returns `WorkflowyError::Timeout` once the deadline
    /// has passed. Cancellation and deadline-expiry interrupt:
    /// (a) the rate-limiter wait,
    /// (b) the in-flight HTTP send (raced via `tokio::select!`),
    /// (c) the inter-attempt backoff sleep.
    /// In each case the function returns the appropriate abort variant
    /// without holding tokens or workers, so a `cancel_all` (or budget
    /// expiry) actually frees the shared `RateLimiter` for new tool calls.
    pub async fn request_cancellable<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<serde_json::Value>,
        cancel: Option<&CancelGuard>,
        deadline: Option<Instant>,
    ) -> Result<T> {
        // Pre-write listing invalidation: every mutation routes through this
        // funnel, so dropping the affected children listings HERE — before
        // the HTTP send — means no future write path can forget it, and a
        // timeout/cancel mid-flight can never strand a stale listing
        // (constitution: invalidate before the mutation lands, not after).
        if method != "GET" {
            self.invalidate_listings_for_write(endpoint, body.as_ref());
        }

        // Fail-fast inside an open retry_after window. Pre-2026-06-01 a
        // call (read or write) issued while the upstream's 429 window was
        // still open queued behind the rate limiter and held for the full
        // MCP transport timeout (~4 min) before returning "no result" —
        // the single biggest time-sink in a bulk write session, and
        // indistinguishable from a crashed server. Short-circuit with the
        // same synthetic 429 the diagnostic probe uses so the caller gets
        // a structured rate-limit error (carrying the remaining
        // retry_after) within microseconds and can wait the window out
        // instead of hanging. The probe path bypasses this (it calls
        // try_request_cancellable directly) and owns its own suppression,
        // so the two don't double-fire. WHY: 2026-06-01 429-storm session.
        let posture = self.rate_limit_posture();
        if posture.in_retry_window {
            return Err(self.rate_limit_window_error(
                &posture,
                "request suppressed inside open retry_after window",
            ));
        }

        let mut attempt = 0;

        loop {
            attempt += 1;

            if let Some(g) = cancel {
                if g.is_cancelled() {
                    return Err(WorkflowyError::Cancelled);
                }
            }
            if let Some(dl) = deadline {
                if Instant::now() >= dl {
                    return Err(WorkflowyError::Timeout);
                }
            }

            // Rate limit: wait for a token, but bail if cancellation flips
            // while we are queued behind earlier waiters. The 50 ms slice
            // inside `acquire_cancellable` keeps deadline checking timely
            // even without an explicit deadline-aware acquire.
            match cancel {
                Some(g) => {
                    if !self.rate_limiter.acquire_cancellable(g).await {
                        return Err(WorkflowyError::Cancelled);
                    }
                }
                None => self.rate_limiter.acquire().await,
            }
            // Re-check deadline after rate-limit wait — token acquisition
            // itself can absorb measurable budget when the bucket is drained.
            if let Some(dl) = deadline {
                if Instant::now() >= dl {
                    return Err(WorkflowyError::Timeout);
                }
            }

            match self.try_request_cancellable::<T>(method, endpoint, &body, cancel, deadline).await {
                Ok(result) => return Ok(result),
                Err(WorkflowyError::Cancelled) => return Err(WorkflowyError::Cancelled),
                Err(WorkflowyError::Timeout) => return Err(WorkflowyError::Timeout),
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
                        sleep_with_abort(Duration::from_millis(delay_ms), cancel, deadline).await?;
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
        deadline: Option<Instant>,
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
        // Race the in-flight HTTP send against the cancel guard *and* the
        // wall-clock deadline. Dropping the send future cleanly cancels the
        // underlying connection in reqwest, so a hung upstream cannot keep
        // the task alive past `deadline`. `pending::<()>()` plays the role
        // of a never-resolving future so we keep one `tokio::select!` shape
        // regardless of which controls the caller supplied.
        use std::future::pending;
        let cancel_branch = async {
            match cancel {
                Some(g) => wait_for_cancel(g).await,
                None => pending::<()>().await,
            }
        };
        let timeout_branch = async {
            match deadline {
                Some(dl) => {
                    let now = Instant::now();
                    if now < dl {
                        tokio::time::sleep(dl - now).await;
                    }
                }
                None => pending::<()>().await,
            }
        };
        let response = tokio::select! {
            biased;
            _ = cancel_branch => return Err(WorkflowyError::Cancelled),
            _ = timeout_branch => return Err(WorkflowyError::Timeout),
            res = req.send() => res.map_err(WorkflowyError::HttpError)?,
        };

        // Record latency and any rate-limit headers regardless of success —
        // a 429 still tells us something useful about upstream throttling.
        let elapsed_ms = send_started.elapsed().as_millis() as u64;
        self.last_request_ms.store(elapsed_ms, std::sync::atomic::Ordering::Relaxed);
        self.record_rate_limit_headers(response.headers());

        let status = response.status();

        if status.is_success() {
            // Stamp BEFORE attempting body parse — the upstream definitely
            // answered 2xx, which is the signal probes care about.
            // Parse failure here is a serialization issue, not an upstream
            // liveness issue.
            self.last_success_unix_ms
                .store(now_unix_ms(), std::sync::atomic::Ordering::Relaxed);
            // AIMD additive recovery: each quiet success walks a 429-cut
            // refill rate back toward the configured ceiling.
            self.rate_limiter.reward();
            response
                .json::<T>()
                .await
                .map_err(WorkflowyError::HttpError)
        } else {
            // Auth failures are special: they prove the API key is wrong
            // (or revoked), independent of network/upstream health. Track
            // them on a separate axis so probes can answer
            // "authenticated?" without conflating it with "reachable?".
            if matches!(status.as_u16(), 401 | 403) {
                self.last_auth_failure_unix_ms
                    .store(now_unix_ms(), std::sync::atomic::Ordering::Relaxed);
            }

            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            // 429 stamping: record the timestamp + parsed retry_after so the
            // diagnostic probe can suppress further HTTP calls inside the
            // window. The 2026-05-26 incident showed that without this, the
            // probe itself consumes the very quota the caller is waiting to
            // recover — observable as a retry_after that resets every time
            // `workflowy_status` runs.
            if status.as_u16() == 429 {
                let retry_after_ms = self.parse_retry_after(&error_text);
                self.stamp_rate_limited(retry_after_ms);
            }

            if matches!(status.as_u16(), 401 | 403) {
                self.last_auth_failure_unix_ms
                    .store(now_unix_ms(), std::sync::atomic::Ordering::Relaxed);
            }

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

    fn node_with_priority(id: &str, priority: Option<i64>) -> WorkflowyNode {
        WorkflowyNode {
            id: id.to_string(),
            name: id.to_string(),
            priority,
            ..Default::default()
        }
    }

    #[test]
    fn sort_children_by_priority_ascending_none_first_stable() {
        // Ascending priority = display order (lowest priority renders at the
        // top of the outline; 2026-07-12 issue 3). Priority-less (None) sorts
        // to the head, and equal priorities keep their upstream order (stable).
        let mut children = vec![
            node_with_priority("c", Some(6400)),
            node_with_priority("a", Some(5700)),
            node_with_priority("none1", None),
            node_with_priority("b", Some(6100)),
            node_with_priority("none2", None),
            node_with_priority("dup1", Some(6100)),
        ];
        WorkflowyClient::sort_children_by_priority(&mut children);
        let order: Vec<&str> = children.iter().map(|n| n.id.as_str()).collect();
        // None entries first (head), preserving their relative upstream order;
        // then ascending priority; ties (b, dup1 both 6100) keep upstream order.
        assert_eq!(order, vec!["none1", "none2", "a", "b", "dup1", "c"]);
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
            skipped_branches: Vec::new(),
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

    /// Regression test for the 2026-04-30 wiring incident: the probe's
    /// liveness signal must come from `last_success_unix_ms` and the
    /// `authenticated` signal must come from `last_auth_failure_unix_ms`,
    /// each on their own axis. Until either has been stamped the
    /// accessors return `None`, so callers can tell "never observed"
    /// apart from "observed long ago".
    #[test]
    fn test_success_and_auth_failure_trackers_default_to_none() {
        let client = test_client();
        assert!(client.last_success_unix_ms().is_none());
        assert!(client.last_success_ms_ago().is_none());
        assert!(client.last_auth_failure_unix_ms().is_none());
        assert!(
            !client.recent_auth_failure(Duration::from_secs(60)),
            "no failures observed yet — recent_auth_failure must be false"
        );
    }

    #[test]
    fn test_recent_auth_failure_respects_window() {
        let client = test_client();
        client._test_stamp_auth_failure();
        // Within window: just stamped, must be true.
        assert!(client.recent_auth_failure(Duration::from_secs(60)));
        // A zero window is the degenerate case — even a fresh stamp
        // should not register, because "within zero ms" includes
        // nothing strictly older than the moment of the call. We
        // permit equality (saturating arithmetic gives 0 ≤ 0) to keep
        // the helper simple, which is the documented behaviour.
    }

    #[test]
    fn test_last_success_ms_ago_returns_some_after_stamp() {
        let client = test_client();
        client._test_stamp_success();
        let ago = client.last_success_ms_ago().expect("stamped success — must be Some");
        // Stamping happens within microseconds of the read; the
        // saturating-sub means the value can be 0. Anything below the
        // process clock skew threshold is fine.
        assert!(ago < 5_000, "stamp-then-read should be under 5s, got {ago} ms");
    }

    /// `RateLimitPosture::in_retry_window` defaults to false until a
    /// 429 is observed; once stamped with a retry_after, the window
    /// stays open while time remaining > 0, and closes when the
    /// window elapses (saturating arithmetic). Pinned because the
    /// 2026-05-26 incident relies on this for probe suppression.
    #[test]
    fn test_rate_limit_posture_defaults_to_no_window() {
        let client = test_client();
        let p = client.rate_limit_posture();
        assert!(p.last_429_unix_ms.is_none());
        assert!(p.last_retry_after_ms.is_none());
        assert!(p.retry_after_remaining_ms.is_none());
        assert!(!p.in_retry_window);
    }

    #[test]
    fn test_rate_limit_posture_opens_after_stamp_with_retry_after() {
        let client = test_client();
        client._test_stamp_rate_limited(Some(60_000));
        let p = client.rate_limit_posture();
        assert!(p.last_429_unix_ms.is_some());
        assert_eq!(p.last_retry_after_ms, Some(60_000));
        let remaining = p.retry_after_remaining_ms.expect("window is open");
        // Stamped milliseconds ago at most a handful — remaining must
        // be close to the full 60 s.
        assert!(
            (59_000..=60_000).contains(&remaining),
            "remaining must be ~60s; got {remaining}",
        );
        assert!(p.in_retry_window);
    }

    #[test]
    fn test_rate_limit_posture_closes_when_retry_after_elapses() {
        let client = test_client();
        // Stamp a 1-ms retry_after and sleep through it. The window
        // closes on the saturating arithmetic — `remaining` becomes
        // `None` and `in_retry_window` flips to false.
        client._test_stamp_rate_limited(Some(1));
        std::thread::sleep(std::time::Duration::from_millis(20));
        let p = client.rate_limit_posture();
        assert!(p.last_429_unix_ms.is_some(), "stamp should persist");
        assert!(
            p.retry_after_remaining_ms.is_none(),
            "elapsed window must report None remaining",
        );
        assert!(!p.in_retry_window, "elapsed window must close");
    }

    #[test]
    fn test_rate_limit_posture_without_retry_after_hint_does_not_open_window() {
        // A 429 with no parseable retry_after still stamps the
        // timestamp (useful for `last_429_unix_ms` accounting) but
        // does NOT open a window — probes proceed at their normal
        // cadence because there's no hint how long to wait.
        let client = test_client();
        client._test_stamp_rate_limited(None);
        let p = client.rate_limit_posture();
        assert!(p.last_429_unix_ms.is_some());
        assert!(p.last_retry_after_ms.is_none());
        assert!(p.retry_after_remaining_ms.is_none());
        assert!(!p.in_retry_window);
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
    async fn test_request_cancellable_bails_on_expired_deadline_before_any_send() {
        // If the deadline is already in the past, the retry loop must return
        // `Timeout` on the very first iteration — without consuming a rate
        // token or attempting an HTTP request. Regression guard for the
        // hang the user reported on `tag_search`: previously the deadline
        // was only checked between batches, so a stalled HTTP send could
        // run the full retry budget.
        let client = test_client();
        let past = Instant::now() - Duration::from_millis(1);
        let started = Instant::now();
        let res: Result<serde_json::Value> = client
            .request_cancellable("GET", "/nodes", None, None, Some(past))
            .await;
        let elapsed = started.elapsed();
        match res {
            Err(WorkflowyError::Timeout) => {}
            other => panic!("expected Timeout, got {:?}", other),
        }
        assert!(
            elapsed < Duration::from_millis(100),
            "expired-deadline check must short-circuit; took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_sleep_with_abort_returns_timeout_when_deadline_inside_duration() {
        // The inter-attempt backoff sleep must wake at the deadline, not at
        // the full backoff duration. Without this, a 5-attempt 10s backoff
        // would keep the call alive for ~30s past a 1s deadline.
        let started = Instant::now();
        let dl = started + Duration::from_millis(20);
        let res = sleep_with_abort(Duration::from_secs(5), None, Some(dl)).await;
        let elapsed = started.elapsed();
        assert!(matches!(res, Err(WorkflowyError::Timeout)));
        // 100 ms is generous slack for CI scheduling jitter; the point is
        // we must not wait for 5s.
        assert!(elapsed < Duration::from_millis(150), "took {:?}", elapsed);
    }

    #[tokio::test]
    async fn test_sleep_with_abort_completes_when_deadline_outside_duration() {
        // Sanity: when the deadline is generous, the sleep returns Ok.
        let started = Instant::now();
        let dl = started + Duration::from_secs(10);
        let res = sleep_with_abort(Duration::from_millis(20), None, Some(dl)).await;
        assert!(matches!(res, Ok(())));
        assert!(started.elapsed() >= Duration::from_millis(15));
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

    /// Coverage-honesty tests for the 2026-07-16 defect: a walk that ate a
    /// 429 on a branch fetch logged a warning, dropped the branch, and still
    /// reported `complete` — so `complete` meant "did not time out" rather
    /// than "covered the subtree". A `wflow-do reindex` reported 1408 nodes
    /// and `complete` while whole component menus were missing from the
    /// index, and no field in the response distinguished that from a full
    /// walk.
    /// The children-listing cache (2026-07-21): `get_children_cancellable`
    /// is the single funnel every `list_children` call and every walk level
    /// routes through, so a TTL-bounded cache there collapses repeated and
    /// overlapping reads to one upstream call — the dominant avoidable load
    /// in a many-large-frequent-queries second-brain session. Writes
    /// invalidate pre-send via the `request_cancellable` funnel, so a hit
    /// can never predate an in-process mutation. Caching is opt-in via
    /// `with_node_cache`; a bare client stays fully live.
    mod children_listing_cache {
        use super::*;
        use crate::utils::cache::NodeCache;
        use serde_json::json;
        use std::sync::Arc;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn quick_configs() -> (RetryConfig, RateLimitConfig) {
            (
                RetryConfig {
                    max_attempts: 1,
                    base_delay_ms: 1,
                    max_delay_ms: 2,
                    retryable_statuses: defaults::RETRY_STATUSES,
                },
                RateLimitConfig { requests_per_second: 200, burst_size: 100 },
            )
        }

        fn cached_client(mock: &MockServer) -> WorkflowyClient {
            let (retry, rate) = quick_configs();
            WorkflowyClient::new_with_configs(mock.uri(), "test-key".to_string(), retry, rate)
                .expect("client must build against mock")
                .with_node_cache(Arc::new(NodeCache::new(100)))
        }

        #[tokio::test]
        async fn repeat_children_reads_within_ttl_hit_upstream_once() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "p"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                    {"id": "a", "name": "A", "priority": 1},
                    {"id": "b", "name": "B", "priority": 2}
                ]})))
                .expect(1)
                .mount(&mock)
                .await;

            let client = cached_client(&mock);
            let first = client.get_children("p").await.expect("live read");
            let second = client.get_children("p").await.expect("cached read");
            assert_eq!(first.len(), 2);
            assert_eq!(
                first.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
                second.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
                "cached listing must serve the same display order as the live read",
            );
            // MockServer verifies expect(1) on drop: the second read must
            // not have reached the upstream.
        }

        #[tokio::test]
        async fn write_through_funnel_invalidates_listing_so_next_read_is_live() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "p"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                    {"id": "a", "name": "A"}
                ]})))
                .expect(2)
                .mount(&mock)
                .await;
            Mock::given(method("POST"))
                .and(path("/nodes"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"item_id": "new-child"})),
                )
                .expect(1)
                .mount(&mock)
                .await;

            let client = cached_client(&mock);
            client.get_children("p").await.expect("first live read");
            client
                .create_node("New", None, Some("p"), None)
                .await
                .expect("create under p");
            client.get_children("p").await.expect("post-write read");
            // expect(2) on the GET proves the post-write read went live —
            // the create invalidated the cached listing before its POST.
        }

        #[tokio::test]
        async fn client_without_cache_stays_fully_live() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "p"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": []})))
                .expect(2)
                .mount(&mock)
                .await;

            let (retry, rate) = quick_configs();
            let client =
                WorkflowyClient::new_with_configs(mock.uri(), "test-key".to_string(), retry, rate)
                    .expect("client must build against mock");
            client.get_children("p").await.expect("read 1");
            client.get_children("p").await.expect("read 2");
            // expect(2): no cache attached, both reads reach the upstream.
        }
    }

    mod skipped_branch_coverage {
        use super::*;
        use serde_json::json;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn no_retry() -> RetryConfig {
            RetryConfig {
                max_attempts: 1,
                base_delay_ms: 1,
                max_delay_ms: 2,
                retryable_statuses: defaults::RETRY_STATUSES,
            }
        }

        fn fast_rate_limit() -> RateLimitConfig {
            RateLimitConfig {
                requests_per_second: 200,
                burst_size: 100,
            }
        }

        async fn mock_client(mock: &MockServer) -> WorkflowyClient {
            WorkflowyClient::new_with_configs(
                mock.uri(),
                "test-key".to_string(),
                no_retry(),
                fast_rate_limit(),
            )
            .expect("client must build against mock")
        }

        /// Root has two children. One child's child-fetch always 500s, so it
        /// is dropped even after the retry. The walk must NOT report a
        /// complete traversal.
        #[tokio::test]
        async fn walk_that_drops_a_branch_reports_skipped_branches_not_complete() {
            let mock = MockServer::start().await;

            // The scoped root itself.
            Mock::given(method("GET"))
                .and(path("/nodes/root"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"id": "root", "name": "Root"}),
                ))
                .mount(&mock)
                .await;
            // Root's children: one healthy, one whose own fetch will fail.
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "root"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                    {"id": "ok-branch", "name": "Healthy"},
                    {"id": "bad-branch", "name": "Rate limited"}
                ]})))
                .mount(&mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "ok-branch"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": []})))
                .mount(&mock)
                .await;
            // The branch that never recovers.
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "bad-branch"))
                .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": "boom"})))
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            let fetch = client
                .get_subtree_with_controls(Some("root"), 5, 10_000, FetchControls::default())
                .await
                .expect("a dropped branch is a partial walk, not an error");

            assert!(
                fetch.truncated,
                "a walk missing whole subtrees must not report truncated=false"
            );
            assert_eq!(
                fetch.truncation_reason,
                Some(TruncationReason::SkippedBranches),
                "the walk hit neither the node cap nor the deadline, so the reason \
                 must name the dropped branches rather than reporting completion"
            );
            assert_eq!(
                fetch.skipped_branches,
                vec!["bad-branch".to_string()],
                "the dropped branch must be named so the caller can re-walk it"
            );
        }

        /// The healthy case must stay clean: no dropped branches, no
        /// truncation, empty `skipped_branches`.
        #[tokio::test]
        async fn fully_walked_subtree_reports_complete_with_no_skipped_branches() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/nodes/root"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"id": "root", "name": "Root"})),
                )
                .mount(&mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "root"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                    {"id": "kid", "name": "Kid"}
                ]})))
                .mount(&mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "kid"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": []})))
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            let fetch = client
                .get_subtree_with_controls(Some("root"), 5, 10_000, FetchControls::default())
                .await
                .expect("healthy walk");

            assert!(!fetch.truncated, "healthy walk must not report truncation");
            assert_eq!(fetch.truncation_reason, None);
            assert!(fetch.skipped_branches.is_empty());
            assert_eq!(fetch.nodes.len(), 2, "root + kid");
        }

        /// A branch whose first fetch fails but whose retry succeeds must be
        /// recovered — children included, nothing reported as skipped. This
        /// is the transient-429 case the retry exists for.
        #[tokio::test]
        async fn transient_branch_failure_is_recovered_by_the_retry() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/nodes/root"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"id": "root", "name": "Root"})),
                )
                .mount(&mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "root"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                    {"id": "flaky", "name": "Flaky"}
                ]})))
                .mount(&mock)
                .await;
            // First child-fetch for `flaky` fails; the retry then succeeds.
            // `up_to_n_times` + mount order makes the first matching mock win.
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "flaky"))
                .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": "boom"})))
                .up_to_n_times(1)
                .mount(&mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "flaky"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                    {"id": "rescued", "name": "Rescued"}
                ]})))
                .mount(&mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "rescued"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": []})))
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            let fetch = client
                .get_subtree_with_controls(Some("root"), 5, 10_000, FetchControls::default())
                .await
                .expect("walk");

            assert!(
                fetch.skipped_branches.is_empty(),
                "the retry recovered the branch, so nothing should be reported skipped"
            );
            assert_eq!(fetch.truncation_reason, None);
            assert!(
                fetch.nodes.iter().any(|n| n.id == "rescued"),
                "the recovered branch's children must appear in the walk: {:?}",
                fetch.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
            );
        }

        /// The interactive contract: a caller is waiting, so a walk must not
        /// silently become patient. If this flips, every `resolve_link` miss
        /// starts waiting out 50-second rate-limit windows instead of
        /// returning its miss envelope in 20 seconds.
        #[test]
        fn fetch_controls_are_impatient_by_default() {
            assert!(!FetchControls::default().patient);
            assert!(!FetchControls::with_timeout(Duration::from_secs(1)).patient);
            assert!(FetchControls::default().patient().patient);
        }

        /// The walk's two fan-out sites must both read their width from
        /// `FetchControls::concurrency()`. Reading the interactive constant
        /// directly at either site would make a patient walk fan out five
        /// wide — re-tripping the very rate limit patience exists to avoid,
        /// and doing it in the retry path where the damage compounds.
        #[test]
        fn walk_fan_out_reads_concurrency_from_controls() {
            let src = include_str!("client.rs");
            let start = src
                .find(concat!("fn ", "fetch_descendants"))
                .expect("fetch_descendants must exist");
            // Bound at the next item's doc comment, not its `fn` line:
            // `batch_create_nodes` is not a walk and names the interactive
            // constant in its own docs quite legitimately.
            let end = src[start..]
                .find("/// Pipelined batch creator")
                .map(|o| start + o)
                .expect("batch_create_nodes doc marks the end of the walk functions");
            let walk = &src[start..end];
            assert!(
                !walk.contains(concat!("defaults::", "SUBTREE_FETCH_CONCURRENCY")),
                "the walk must take its fan-out width from controls.concurrency(), not the \
                 interactive constant — otherwise a patient walk fans out five wide and \
                 re-trips the rate limit it is meant to tiptoe around",
            );
            assert_eq!(
                walk.matches("controls.concurrency()").count(),
                2,
                "both the per-level fan-out and the branch-retry fan-out must read the \
                 width from controls",
            );
        }

        /// A patient walk keeps re-attempting until branches stop recovering.
        /// Here a branch fails its first two fetches (the initial attempt and
        /// the first retry wave) and succeeds on the third — which an
        /// impatient walk, having exactly one retry wave, drops.
        #[tokio::test]
        async fn patient_walk_recovers_a_branch_a_single_retry_wave_would_drop() {
            async fn build(mock: &MockServer) {
                Mock::given(method("GET"))
                    .and(path("/nodes/root"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({"id": "root", "name": "Root"})),
                    )
                    .mount(mock)
                    .await;
                Mock::given(method("GET"))
                    .and(path("/nodes"))
                    .and(query_param("parent_id", "root"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                        {"id": "stubborn", "name": "Stubborn"}
                    ]})))
                    .mount(mock)
                    .await;
                // Two failures: enough to exhaust the single retry wave.
                Mock::given(method("GET"))
                    .and(path("/nodes"))
                    .and(query_param("parent_id", "stubborn"))
                    .respond_with(
                        ResponseTemplate::new(500).set_body_json(json!({"error": "boom"})),
                    )
                    .up_to_n_times(2)
                    .mount(mock)
                    .await;
                Mock::given(method("GET"))
                    .and(path("/nodes"))
                    .and(query_param("parent_id", "stubborn"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                        {"id": "deep", "name": "Deep"}
                    ]})))
                    .mount(mock)
                    .await;
                Mock::given(method("GET"))
                    .and(path("/nodes"))
                    .and(query_param("parent_id", "deep"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": []})))
                    .mount(mock)
                    .await;
            }

            let patient_mock = MockServer::start().await;
            build(&patient_mock).await;
            let fetch = mock_client(&patient_mock)
                .await
                .get_subtree_with_controls(
                    Some("root"),
                    5,
                    10_000,
                    FetchControls::default().patient(),
                )
                .await
                .expect("walk");
            assert!(
                fetch.skipped_branches.is_empty(),
                "a patient walk must keep retrying until the branch recovers, got {:?}",
                fetch.skipped_branches
            );
            assert!(
                fetch.nodes.iter().any(|n| n.id == "deep"),
                "the recovered subtree must be present"
            );
            assert_eq!(fetch.truncation_reason, None);

            // The same tree, impatiently: one retry wave is not enough, so
            // the branch is dropped — and reported, never silently omitted.
            let impatient_mock = MockServer::start().await;
            build(&impatient_mock).await;
            let fetch = mock_client(&impatient_mock)
                .await
                .get_subtree_with_controls(Some("root"), 5, 10_000, FetchControls::default())
                .await
                .expect("walk");
            assert_eq!(
                fetch.skipped_branches,
                vec!["stubborn".to_string()],
                "an impatient walk drops the branch after its single retry wave"
            );
            assert_eq!(
                fetch.truncation_reason,
                Some(TruncationReason::SkippedBranches),
                "and says so — a dropped branch must never read as complete"
            );
        }

        /// Patience must not mean spinning. A branch failing for a reason
        /// waiting cannot fix (a 404, say) has to stop the loop, not retry
        /// until the deadline.
        #[tokio::test]
        async fn patient_walk_stops_when_a_wave_recovers_nothing() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/nodes/root"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"id": "root", "name": "Root"})),
                )
                .mount(&mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "root"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                    {"id": "gone", "name": "Gone"}
                ]})))
                .mount(&mock)
                .await;
            Mock::given(method("GET"))
                .and(path("/nodes"))
                .and(query_param("parent_id", "gone"))
                .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": "always"})))
                .mount(&mock)
                .await;

            let started = Instant::now();
            let fetch = mock_client(&mock)
                .await
                .get_subtree_with_controls(
                    Some("root"),
                    5,
                    10_000,
                    FetchControls::default().patient(),
                )
                .await
                .expect("walk");

            assert_eq!(
                fetch.skipped_branches,
                vec!["gone".to_string()],
                "a permanently-failing branch must be reported, not retried forever"
            );
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the no-progress guard must stop the loop promptly; took {:?}",
                started.elapsed()
            );
        }
    }

    /// Regression tests for the 2026-05-02 description field-loss bug.
    /// Workflowy's wire field for the description body is `note`. Sending
    /// `description` returns 200 OK with the field silently dropped. The
    /// brief filed this as P2.4 ("partial-update logic") but the actual
    /// root cause was a wire-format mismatch — the split-into-two-POSTs
    /// workaround masked it because both halves used the wrong key.
    ///
    /// These tests pin the wire shape with a body matcher: if the field
    /// renames back to `description`, the mock won't match, the call
    /// errors, and the test fails — catching the regression without
    /// needing a live API.
    mod write_field_names {
        use super::*;
        use serde_json::json;
        use wiremock::matchers::{body_partial_json, method, path, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn fast_retry() -> RetryConfig {
            RetryConfig {
                max_attempts: 1,
                base_delay_ms: 10,
                max_delay_ms: 20,
                retryable_statuses: defaults::RETRY_STATUSES,
            }
        }

        fn fast_rate_limit() -> RateLimitConfig {
            RateLimitConfig {
                requests_per_second: 200,
                burst_size: 100,
            }
        }

        async fn mock_client(mock: &MockServer) -> WorkflowyClient {
            WorkflowyClient::new_with_configs(
                mock.uri(),
                "test-key".to_string(),
                fast_retry(),
                fast_rate_limit(),
            )
            .expect("client must build against mock")
        }

        #[tokio::test]
        async fn create_node_sends_description_as_note() {
            let mock = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/nodes"))
                .and(body_partial_json(json!({
                    "name": "n",
                    "note": "d"
                })))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"item_id": "00000000-0000-0000-0000-000000000001"})),
                )
                .expect(1)
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            client
                .create_node("n", Some("d"), None, None)
                .await
                .expect("create must reach mock — if this fails, the wire field name regressed");
        }

        #[tokio::test]
        async fn create_node_sends_layout_as_layout_mode() {
            // 2026.01 adoption: a layout maps to the wire field `layoutMode`
            // (camelCase, the API's own name). Assert it reaches the wire so a
            // future rename of the field can't silently drop the node type.
            let mock = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/nodes"))
                .and(body_partial_json(json!({ "name": "task", "layoutMode": "todo" })))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"item_id": "00000000-0000-0000-0000-000000000002"})),
                )
                .expect(1)
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            client
                .create_node_cancellable("task", None, None, None, Some("todo"), None, None)
                .await
                .expect("create-with-layout must reach the mock with layoutMode on the wire");
        }

        #[tokio::test]
        async fn edit_node_description_only_sends_note() {
            let mock = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path_regex(r"^/nodes/[^/]+$"))
                .and(body_partial_json(json!({"note": "d"})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                .expect(1)
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            client
                .edit_node("00000000-0000-0000-0000-000000000001", None, Some("d"))
                .await
                .expect("edit must reach mock — if this fails, the wire field name regressed");
        }

        #[tokio::test]
        async fn edit_node_combined_splits_and_uses_note_for_description() {
            let mock = MockServer::start().await;
            // First POST: name only.
            Mock::given(method("POST"))
                .and(path_regex(r"^/nodes/[^/]+$"))
                .and(body_partial_json(json!({"name": "new"})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                .expect(1)
                .mount(&mock)
                .await;
            // Second POST: note only — pinned to `note`, not `description`.
            Mock::given(method("POST"))
                .and(path_regex(r"^/nodes/[^/]+$"))
                .and(body_partial_json(json!({"note": "d"})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                .expect(1)
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            client
                .edit_node("00000000-0000-0000-0000-000000000001", Some("new"), Some("d"))
                .await
                .expect("split edit must reach both mocks — if this fails, the wire field name regressed");
        }

        /// `set_completion(_, true)` must POST `{"completed": true}` to
        /// `/nodes/{id}`. Pins the wire shape so the description → note
        /// failure mode (200 OK with the field silently dropped) cannot
        /// recur on the completion path.
        #[tokio::test]
        async fn set_completion_true_sends_completed_true() {
            let mock = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path_regex(r"^/nodes/[^/]+$"))
                .and(body_partial_json(json!({"completed": true})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                .expect(1)
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            client
                .set_completion("00000000-0000-0000-0000-000000000001", true)
                .await
                .expect("complete must reach mock — if this fails, the wire field name regressed");
        }

        /// `set_completion(_, false)` is the symmetric uncomplete path.
        /// The mock requires the literal `false` value so a
        /// `{"completed": true}` regression on the uncomplete branch
        /// would also fail this test.
        #[tokio::test]
        async fn set_completion_false_sends_completed_false() {
            let mock = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path_regex(r"^/nodes/[^/]+$"))
                .and(body_partial_json(json!({"completed": false})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                .expect(1)
                .mount(&mock)
                .await;

            let client = mock_client(&mock).await;
            client
                .set_completion("00000000-0000-0000-0000-000000000001", false)
                .await
                .expect("uncomplete must reach mock — if this fails, the wire field name regressed");
        }
    }

    /// Bulk export (`GET /nodes-export`) and native-mirror (beta) methods,
    /// added 2026-07-22 from the workflowy-cli comparison. Verified against
    /// the live account before landing; these pins hold the wire shape.
    #[cfg(test)]
    mod export_and_native_mirror {
        use super::*;
        use crate::config::{RateLimitConfig, RetryConfig};
        use serde_json::json;
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn client(mock: &MockServer) -> WorkflowyClient {
            let retry = RetryConfig {
                max_attempts: 1,
                base_delay_ms: 1,
                max_delay_ms: 2,
                retryable_statuses: defaults::RETRY_STATUSES,
            };
            let rate = RateLimitConfig { requests_per_second: 200, burst_size: 100 };
            WorkflowyClient::new_with_configs(mock.uri(), "test-key".to_string(), retry, rate)
                .expect("client must build against mock")
        }

        #[tokio::test]
        async fn export_all_parses_the_whole_tree_from_the_bulk_endpoint() {
            let mock = MockServer::start().await;
            // One GET returns every node flat, each with parent_id and the
            // API's own field names (note, modifiedAt). expect(1): a single
            // call, not a per-level walk.
            Mock::given(method("GET"))
                .and(path("/nodes-export"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": [
                    {"id": "a", "name": "Root", "parent_id": null, "note": "root note", "modifiedAt": 1_784_678_400i64},
                    {"id": "b", "name": "Child", "parent_id": "a", "modifiedAt": 1_784_678_401i64}
                ]})))
                .expect(1)
                .mount(&mock)
                .await;

            let nodes = client(&mock).export_all().await.expect("export must parse");
            assert_eq!(nodes.len(), 2);
            assert_eq!(nodes[0].id, "a");
            // note → description, modifiedAt → last_modified (seconds, verbatim).
            assert_eq!(nodes[0].description.as_deref(), Some("root note"));
            assert_eq!(nodes[0].last_modified, Some(1_784_678_400));
            assert_eq!(nodes[1].parent_id.as_deref(), Some("a"));
        }

        #[tokio::test]
        async fn export_all_surfaces_upstream_error_status() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/nodes-export"))
                .respond_with(ResponseTemplate::new(429).set_body_string("{\"retry_after\": 65}"))
                .mount(&mock)
                .await;
            let err = client(&mock).export_all().await.expect_err("429 must be an error");
            assert!(matches!(err, WorkflowyError::ApiError { status: 429, .. }));
        }

        #[tokio::test]
        async fn create_native_mirror_posts_body_and_parses_item_and_origin() {
            let mock = MockServer::start().await;
            // POST /nodes/{id}/mirror with {parent_id, position}; response
            // carries the new mirror's item_id + the origin it points at.
            Mock::given(method("POST"))
                .and(path("/nodes/canon-1/mirror"))
                .and(body_partial_json(json!({"parent_id": "parent-1", "position": "top"})))
                .respond_with(ResponseTemplate::new(200)
                    .set_body_json(json!({"item_id": "mirror-9", "origin_id": "canon-1"})))
                .expect(1)
                .mount(&mock)
                .await;

            let resp = client(&mock)
                .create_native_mirror("canon-1", "parent-1", "top")
                .await
                .expect("native mirror create must parse");
            assert_eq!(resp.item_id, "mirror-9");
            assert_eq!(resp.origin_id, "canon-1");
        }

        #[tokio::test]
        async fn delete_native_mirror_hits_the_beta_mirror_endpoint() {
            let mock = MockServer::start().await;
            Mock::given(method("DELETE"))
                .and(path("/nodes/mirror-9/mirror"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                .expect(1)
                .mount(&mock)
                .await;
            client(&mock)
                .delete_native_mirror("mirror-9")
                .await
                .expect("native mirror delete must succeed");
        }

        #[test]
        fn beta_base_swaps_production_host_only() {
            let retry = RetryConfig::default();
            let rate = RateLimitConfig::default();
            // Production base → beta host.
            let prod = WorkflowyClient::new_with_configs(
                "https://workflowy.com/api/v1".to_string(),
                "k".to_string(),
                RetryConfig { ..retry },
                RateLimitConfig { ..rate },
            )
            .unwrap();
            assert_eq!(prod.beta_base(), "https://beta.workflowy.com/api/v1");
            // A non-production base (a mock / already-beta) is left as-is, so
            // wiremock tests hit their own host.
            let local = WorkflowyClient::new_with_configs(
                "http://127.0.0.1:8080".to_string(),
                "k".to_string(),
                RetryConfig::default(),
                RateLimitConfig::default(),
            )
            .unwrap();
            assert_eq!(local.beta_base(), "http://127.0.0.1:8080");
        }
    }
}
