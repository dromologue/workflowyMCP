//! Centralized default values and constants for the Workflowy MCP server.
//! All magic numbers belong here so they're easy to find and tune.

// --- Cache ---
/// How long cached nodes remain valid (seconds)
pub const CACHE_TTL_SECS: u64 = 30;
/// Maximum number of entries the cache will hold
pub const CACHE_MAX_SIZE: usize = 10_000;

// --- Retry ---
/// Maximum number of retry attempts for API calls (increased from 3 to handle 429 retry_after waits)
pub const RETRY_MAX_ATTEMPTS: u32 = 5;
/// Initial delay between retries (milliseconds)
pub const RETRY_BASE_DELAY_MS: u64 = 1_000;
/// Maximum delay between retries (milliseconds)
pub const RETRY_MAX_DELAY_MS: u64 = 10_000;
/// HTTP status codes that trigger a retry
pub const RETRY_STATUSES: &[u16] = &[429, 500, 502, 503, 504, 408];

// --- Rate Limiting ---
/// Sustained request rate (requests per second)
pub const RATE_LIMIT_RPS: u32 = 10;
/// Maximum burst size above sustained rate
pub const RATE_LIMIT_BURST: u32 = 20;

// --- Queue ---
/// Maximum concurrent batch operations
pub const QUEUE_MAX_CONCURRENCY: usize = 3;
/// Delay between batch items (milliseconds)
pub const QUEUE_BATCH_DELAY_MS: u64 = 50;
/// Maximum items per batch
pub const QUEUE_MAX_BATCH_SIZE: usize = 20;

// --- Job Queue ---
/// How long completed jobs are kept (seconds)
pub const JOB_TTL_SECS: u64 = 1_800; // 30 minutes
/// Maximum number of completed jobs retained
pub const JOB_MAX_HISTORY: usize = 1_000;
/// How often the cleanup task runs (seconds)
pub const JOB_CLEANUP_INTERVAL_SECS: u64 = 60;

// --- Validation ---
/// Maximum length for search queries
pub const MAX_QUERY_LENGTH: usize = 1_000;
/// Maximum length for node names
pub const MAX_NAME_LENGTH: usize = 5_000;
/// Maximum length for node descriptions
pub const MAX_DESCRIPTION_LENGTH: usize = 50_000;
/// Maximum length for insert_content bodies
pub const MAX_CONTENT_LENGTH: usize = 500_000;
/// Maximum number of lines (= nodes to create) accepted by a single
/// `insert_content` call. Brief 2026-05-02: large payloads (~130 nodes)
/// were being dropped at the MCP transport layer before the handler
/// ran, surfacing as a bare `Tool execution failed` with no diagnostic
/// and no per-tool counter movement. We cannot fix transport drops
/// from inside the server, but we can stop pretending the bounded
/// budget is end-to-end: this cap rejects oversized payloads at the
/// handler boundary with a typed error and a chunking instruction, so
/// the caller sees an actionable message instead of an unobservable
/// silent failure. Empirical safe ceiling is ~80 lines per call; we
/// set the cap at 200 to leave headroom for callers who have measured
/// their own client and know their transport can carry it.
pub const MAX_INSERT_CONTENT_LINES: usize = 200;
/// Soft warn threshold for `insert_content` payload size. Above this,
/// the success response includes a hint recommending chunking — but
/// the request still runs. Below the hard cap, above the soft warn:
/// the user gets an early signal before they hit the wall.
pub const SOFT_WARN_INSERT_CONTENT_LINES: usize = 80;
/// Hard cap on max_results for any search/list tool
pub const HARD_MAX_RESULTS: usize = 100;
/// Default max_results when not specified
pub const DEFAULT_MAX_RESULTS: usize = 20;
/// Maximum response text size (~50KB)
pub const MAX_RESPONSE_SIZE: usize = 50_000;
/// Default tool timeout (seconds)
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 30;
/// Wall-clock budget for a single `edit_node` call. Bounds the entire
/// retry loop (including the split name+description POSTs), so a flaky
/// upstream returns Timeout in ~60 s rather than burning all 5 retry
/// attempts × HTTP_TIMEOUT_SECS on each transient read-timeout.
pub const EDIT_NODE_TIMEOUT_MS: u64 = 60_000;
/// Wall-clock budget for a single-node read tool call (`get_node`,
/// `list_children`, and the read paths inside other handlers). Bounds
/// the whole retry loop end-to-end so a hung upstream cannot wedge the
/// MCP tool surface for the full 5 attempts × 30 s (~3.5 min) it would
/// take retry-exhaustion to report. Picked at 30 s because the per-attempt
/// reqwest timeout is also 30 s — one full-budget HTTP attempt fits, and
/// a flat-out hung connection returns Timeout to the caller immediately
/// after, freeing the rate-limiter and connection-pool slot.
pub const READ_NODE_TIMEOUT_MS: u64 = 30_000;
/// Wall-clock budget for a single write call (`create_node`,
/// `delete_node`). Caps the retry loop end-to-end at ~15 s so a
/// transient upstream slowness cannot make one node-creation burn
/// the full `RETRY_MAX_ATTEMPTS × HTTP_TIMEOUT_SECS` (~150 s) — the
/// failure mode that produced the 4-minute insert_content hangs in
/// the 2026-05-02 report. Picked at 15 s because a healthy create
/// completes in <300 ms; budget = 50× headroom is generous without
/// allowing one slow call to push a 140-node insert past the MCP
/// client's 4-min timeout.
pub const WRITE_NODE_TIMEOUT_MS: u64 = 15_000;
/// Window during which a recent successful API call proves the
/// upstream is reachable, regardless of whether the current liveness
/// probe succeeded. Health/status responses surface
/// `api_reachable: true` when a real tool call has returned a 2xx
/// within this window. Without this, two consecutive probe timeouts
/// during a heavy write burst flip the status to degraded even
/// though the burst itself is the proof of liveness — the lag
/// described in the 2026-05-02 report.
pub const API_REACHABILITY_FRESHNESS_MS: u64 = 30_000;
/// Wall-clock budget for a whole `insert_content` operation. The MCP
/// client (Claude Desktop) hard-times out at 4 min; we return a
/// structured partial-success well before that so the caller learns
/// what was inserted instead of seeing a "no result received" with
/// no diagnostic. Combined with `WRITE_NODE_TIMEOUT_MS` per single
/// create, this guarantees `insert_content` either completes or
/// returns deterministic partial-success data.
pub const INSERT_CONTENT_TIMEOUT_MS: u64 = 210_000;

// --- Tree Traversal ---
/// Default max_depth for search operations
pub const DEFAULT_SEARCH_DEPTH: usize = 3;
/// Default max_depth for review/todo operations
pub const DEFAULT_REVIEW_DEPTH: usize = 5;
/// Maximum recursion depth for subtree fetching
pub const MAX_TREE_DEPTH: usize = 10;
/// Hard cap on nodes returned by a single subtree fetch.
/// Callers receive a `truncated` flag whenever this cap is hit.
pub const MAX_SUBTREE_NODES: usize = 10_000;
/// Wall-clock budget for a subtree walk (milliseconds). When hit, the walk
/// returns whatever has been collected plus a timeout truncation flag.
pub const SUBTREE_FETCH_TIMEOUT_MS: u64 = 20_000;
/// Concurrency for parallel child fetches inside a subtree walk. Kept close
/// to the rate-limit burst so the limiter, not this cap, shapes throughput.
pub const SUBTREE_FETCH_CONCURRENCY: usize = 5;
/// Wall-clock budget for health checks (milliseconds). Must stay sub-second
/// on any tree size so the tool is usable when the API is degraded.
pub const HEALTH_CHECK_TIMEOUT_MS: u64 = 5_000;
/// Window after the most recent observed 401/403 during which probes still
/// report `authenticated: false`. Outside this window the signal flips back
/// to true — auth failures are sticky-but-not-permanent so a transient 401
/// from upstream maintenance doesn't leave the server reporting "unauth"
/// forever after the next successful call. 5 minutes is long enough to
/// cover a brief upstream blip without masking a genuine, persistent auth
/// issue, since any subsequent failed call will re-stamp the timestamp.
pub const AUTH_FAILURE_WINDOW_SECS: u64 = 5 * 60;

// --- API ---
/// Workflowy API base URL
pub const WORKFLOWY_BASE_URL: &str = "https://workflowy.com/api/v1";
/// HTTP client timeout (seconds)
pub const HTTP_TIMEOUT_SECS: u64 = 30;

// --- Persistent name index ---
/// How often the periodic saver checks the dirty flag and rewrites the
/// on-disk index. Coalesces rapid bursts of mutations into one fsync;
/// the rename-on-success protocol means a crash mid-save never produces
/// a half-written file.
pub const INDEX_SAVE_INTERVAL_SECS: u64 = 30;
/// How often the background refresher walks the workspace root to keep
/// the index in sync with newly added/renamed nodes. The walk inherits
/// the resolution timeout and node cap, so a single pass on a huge
/// (250k+) tree only covers ~12k nodes — the convergence story is
/// many short walks stitched together over time rather than one long
/// one. 30 minutes leaves enough rate-limit headroom for foreground
/// requests while still building up coverage within a working day.
pub const INDEX_REFRESH_INTERVAL_SECS: u64 = 30 * 60;
/// Wall-clock budget for an on-demand resolution walk triggered by a
/// short-hash miss in `resolve_node_ref`. Bigger than the regular
/// subtree budget because the user is *waiting* for this specific
/// resolution and we want to give it a real chance to succeed before
/// surfacing the cache-miss error.
pub const RESOLVE_WALK_TIMEOUT_MS: u64 = 5 * 60 * 1_000;
/// Node cap for the resolution walk. Set generously so a moderately
/// large tree can be exhaustively walked while still bounding worst
/// case memory use.
pub const RESOLVE_WALK_NODE_CAP: usize = 100_000;
/// Environment variable that overrides the default on-disk path for
/// the persistent name index. Empty string means "disabled".
pub const INDEX_PATH_ENV: &str = "WORKFLOWY_INDEX_PATH";
/// Subdirectory under `$HOME/code/secondBrain/memory` that holds the
/// persistent index. Documented in the repo's setup guide so a fresh
/// user knows where to expect their cache to land.
pub const DEFAULT_INDEX_RELATIVE_PATH: &str = "code/secondBrain/memory/name_index.json";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_defaults_are_reasonable() {
        // Defence-in-depth: catch a careless edit that sets a bad default.
        assert!(CACHE_TTL_SECS > 0 && CACHE_TTL_SECS <= 300);
        assert!(RETRY_MAX_ATTEMPTS >= 1 && RETRY_MAX_ATTEMPTS <= 10);
        assert!(RATE_LIMIT_RPS >= 1);
        assert!(RATE_LIMIT_BURST >= RATE_LIMIT_RPS);
        assert!(HARD_MAX_RESULTS >= DEFAULT_MAX_RESULTS);
        assert!(DEFAULT_REVIEW_DEPTH >= DEFAULT_SEARCH_DEPTH);
        assert!(MAX_TREE_DEPTH >= DEFAULT_REVIEW_DEPTH);
        assert!(!RETRY_STATUSES.is_empty());
        assert!(RETRY_STATUSES.contains(&429)); // rate limit must be retried
        // New: timeouts must be positive and sane.
        assert!(SUBTREE_FETCH_TIMEOUT_MS >= 1_000 && SUBTREE_FETCH_TIMEOUT_MS <= 120_000);
        assert!(SUBTREE_FETCH_CONCURRENCY >= 1 && SUBTREE_FETCH_CONCURRENCY <= 50);
        assert!(HEALTH_CHECK_TIMEOUT_MS >= 500 && HEALTH_CHECK_TIMEOUT_MS <= 10_000);
        assert!(AUTH_FAILURE_WINDOW_SECS >= 60 && AUTH_FAILURE_WINDOW_SECS <= 60 * 60);
    }

    #[test]
    fn test_config_defaults_reference_defaults_module() {
        use crate::config::{RetryConfig, RateLimitConfig, JobQueueConfig};
        let retry = RetryConfig::default();
        assert_eq!(retry.max_attempts, RETRY_MAX_ATTEMPTS);
        assert_eq!(retry.base_delay_ms, RETRY_BASE_DELAY_MS);

        let rate = RateLimitConfig::default();
        assert_eq!(rate.requests_per_second, RATE_LIMIT_RPS);
        assert_eq!(rate.burst_size, RATE_LIMIT_BURST);

        let job = JobQueueConfig::default();
        assert_eq!(job.completed_job_ttl_secs, JOB_TTL_SECS);
        assert_eq!(job.max_job_history, JOB_MAX_HISTORY);
    }
}
