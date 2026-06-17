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
/// `insert_content` call. Brief 2026-05-02 introduced the cap; failure
/// report 2026-05-03 lowered it from 200 to 80 on 2026-05-04.
///
/// Original framing: oversized payloads (~130 nodes) were dropped at
/// the MCP transport layer before the handler ran — bare
/// `Tool execution failed` with no diagnostic and no per-tool counter
/// movement. We cannot fix transport drops; we can refuse to pretend
/// the bounded-budget contract is end-to-end. The cap rejects
/// oversized payloads at the handler boundary with a typed error and a
/// chunking instruction.
///
/// Why 80 specifically: the failure-report 2026-05-03 session observed
/// a 130-line call failing twice in a row at the transport, then every
/// ~40-line chunk succeeding. The 200-line ceiling was theoretical
/// headroom that no observed caller could reliably reach — payloads
/// above ~80 failed intermittently with no diagnostic. Lowering the
/// cap to the empirical safe ceiling makes the contract honest:
/// callers either succeed atomically at ≤80 or get a structured
/// "split into ≤80-line batches" error.
pub const MAX_INSERT_CONTENT_LINES: usize = 80;
/// Maximum number of node IDs accepted in a single `reorder_nodes`
/// call. Each id costs one `move_node` POST (the priority-rebalance
/// trick is documented in `crate::workflows::reorder_nodes_via_priority`).
/// 200 keeps the call comfortably inside the bulk-tool budget on a
/// healthy upstream (each move ~150–300 ms) while bounding the worst
/// case. Above the cap callers chunk the desired order — the trailing
/// chunk lands first since the algorithm walks in reverse.
pub const MAX_REORDER_NODES: usize = 200;
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
/// client (Claude Desktop, claude.ai web) hard-times out at 4 min; we
/// return a structured partial-success well before that so the caller
/// learns what was inserted instead of seeing a "no result received"
/// with no diagnostic. The value was 210_000 (3.5 min) through
/// 2026-05-09 — close enough to the transport cap that a single slow
/// HTTP write at the end of the budget could race the client's
/// timeout and lose the partial-success envelope. The 2026-05-09
/// failure-report (sub-cap payload, parent_id=null, 4-min hang with
/// no diagnostic) matched that race exactly. Lowered to 180_000 (3
/// min) to give a 60-second margin under the transport cap so the
/// partial-success envelope is reachable on every surface — including
/// claude.ai web, where the report originated. Combined with
/// `WRITE_NODE_TIMEOUT_MS` per single create, this guarantees
/// `insert_content` either completes or returns deterministic
/// partial-success data well before the client gives up.
///
/// Pinned by `bulk_budget_leaves_mcp_transport_margin` ([C-server-008]).
pub const INSERT_CONTENT_TIMEOUT_MS: u64 = 180_000;
/// MCP transport hard-timeout window (Claude Desktop / claude.ai web).
/// We never use this directly — but tests assert
/// `INSERT_CONTENT_TIMEOUT_MS` stays comfortably below it so the
/// partial-success envelope is reachable before the client gives up.
pub const MCP_TRANSPORT_HARD_TIMEOUT_MS: u64 = 240_000;

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
/// short-hash miss in `resolve_link` / `resolve_node_ref`. Aligned with
/// `SUBTREE_FETCH_TIMEOUT_MS` so the worst-case wait a caller pays for
/// a cold-cache resolve matches every other walk-shaped tool. Pre-2026-05-22
/// this was 180 s (3 min) on the theory that the user was *waiting*
/// for the resolution and we should give the walk every chance to find
/// the target — but on the user-report workspaces (56k+ nodes) that
/// budget is hopeless anyway (level-by-level child fetches at the
/// rate-limit ceiling cannot drain that tree in under three minutes)
/// AND it left the caller hanging for the full budget on a guaranteed
/// miss. The 20 s budget converts the failure mode from "three minutes,
/// then null" to "twenty seconds, then null" so callers can branch on
/// the miss envelope and try a scoped retry instead of losing minutes
/// per attempt. Pinned by `resolve_walk_budget_leaves_mcp_transport_margin`.
pub const RESOLVE_WALK_TIMEOUT_MS: u64 = SUBTREE_FETCH_TIMEOUT_MS;
/// Wall-clock budget for the **background** name-index refresher (every
/// `INDEX_REFRESH_INTERVAL_SECS`). Distinct from `RESOLVE_WALK_TIMEOUT_MS`
/// because the background walk is unattended — no caller is waiting on
/// it — so a longer budget is the right trade-off (more nodes ingested
/// per cycle, faster index convergence on large trees). Capped at 180 s
/// so it still sits 60 s below the MCP transport hard timeout; the
/// background walk doesn't have a transport caller, but the same margin
/// rule keeps the budget visibly bounded.
pub const BACKGROUND_INDEX_WALK_BUDGET_MS: u64 = 180_000;
/// Node cap for the resolution walk. Set generously so a moderately
/// large tree can be exhaustively walked while still bounding worst
/// case memory use.
pub const RESOLVE_WALK_NODE_CAP: usize = 100_000;
/// Environment variable that sets the on-disk path for the persistent
/// name index. Unset or empty disables persistence — the index then
/// lives only in memory for the lifetime of the process.
pub const INDEX_PATH_ENV: &str = "WORKFLOWY_INDEX_PATH";

/// Environment variable that sets the on-disk root of the operational
/// `secondBrain` directory (drafts, session logs, briefs, memory).
/// Unset or empty disables every feature that reads from it (e.g. the
/// `review` tool's bucket-d session-log scan and the `wflow-do index`
/// default output path); the repository ships no machine-specific
/// fallback so each user wires the path through their MCP host config.
pub const SECONDBRAIN_DIR_ENV: &str = "SECONDBRAIN_DIR";

/// Resolve the on-disk root of the operational secondBrain directory
/// from `$SECONDBRAIN_DIR`. Returns `None` when the env var is unset or
/// empty — callers must treat that as "feature disabled" rather than
/// guessing a default location, so the repo carries no user-specific
/// path. Cheap enough to call per request: no caching, so a `setenv`
/// from the host MCP config takes effect on the next call.
pub fn secondbrain_dir() -> Option<std::path::PathBuf> {
    let raw = std::env::var(SECONDBRAIN_DIR_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(trimmed))
}

/// `$SECONDBRAIN_DIR/session-logs` when the env var is set; `None`
/// otherwise. The `review` tool (bucket d) and the `wflow-do index`
/// subcommand both treat `None` as "no session-log directory exists"
/// and skip gracefully.
pub fn session_logs_dir() -> Option<std::path::PathBuf> {
    secondbrain_dir().map(|p| p.join("session-logs"))
}

// --- Operation allow-lists & windows shared by the MCP server and the CLI ---
// Single source of truth: pre-2026-06-16 these were hand-written inline in
// both `src/server/mod.rs` and `src/bin/wflow_do.rs`, so adding a new op kind
// meant editing two validation sites that could silently drift.

/// Valid `op` kinds for `read_batch` (MCP) / `wflow-do read-batch` (CLI).
pub const READ_BATCH_VALID_OPS: &[&str] = &["get_node", "list_children", "get_subtree"];

/// Valid `operation` kinds for `bulk_update` (MCP) / `wflow-do bulk-update` (CLI).
pub const BULK_UPDATE_VALID_OPS: &[&str] =
    &["delete", "add_tag", "remove_tag", "complete", "uncomplete"];

/// Seconds in a day — used for the recent-changes / session-log time windows.
pub const SECONDS_PER_DAY: i64 = 86_400;

/// TTL for `create_node` idempotency-key entries (10 min). The window must
/// comfortably cover a rate-limit `retry_after` (tens of seconds) plus a
/// human/agent pause before a retry, but stay short enough that the in-memory
/// store can't grow unbounded over a long session. After this, the same key
/// is treated as fresh. 2026-06-17: best-effort idempotency.
pub const IDEMPOTENCY_TTL_MS: u64 = 600_000;

/// Hard cap on retained idempotency-key entries — a backstop so a session
/// that supplies a unique key per call can't grow the map without bound.
/// Oldest entries are evicted first when the cap is hit (in addition to the
/// TTL prune on every access).
pub const IDEMPOTENCY_MAX_ENTRIES: usize = 4_096;

/// Default scope for the `review` and `audit_mirrors` tools when no `root_id`
/// is supplied, read from the `WORKFLOWY_REVIEW_ROOT` env var.
///
/// There is **no hardcoded fallback** — the repo ships no personal node IDs
/// (constitution: "no machine-specific IDs"). When the env var is unset and the
/// caller omits `root_id`, both tools return a typed invalid-params error
/// asking for an explicit `root_id`. Each user sets `WORKFLOWY_REVIEW_ROOT`
/// (their Distillations / review-anchor node) via `.env` or their MCP host
/// config. Shared by the MCP handlers and the `wflow-do` CLI so the two
/// surfaces can't drift.
pub fn default_review_root() -> Option<String> {
    std::env::var("WORKFLOWY_REVIEW_ROOT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

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

    /// [C-server-008] The bulk-tool budget (governing every Bulk-kind
    /// handler including `insert_content`) must leave a comfortable
    /// margin under the MCP transport's hard timeout so that the
    /// structured partial-success / timeout envelopes are reachable
    /// before the client gives up. The 2026-05-09 failure-report
    /// observed a sub-cap `insert_content` payload hanging the full
    /// 4 minutes with no diagnostic — a race between the previous
    /// 210 s budget and the 240 s transport cap. 60 seconds of
    /// margin is the minimum (allows the partial-success envelope to
    /// serialise + traverse the wire without competing with the
    /// transport cap).
    #[test]
    fn bulk_budget_leaves_mcp_transport_margin() {
        const MIN_MARGIN_MS: u64 = 60_000;
        assert!(
            INSERT_CONTENT_TIMEOUT_MS + MIN_MARGIN_MS <= MCP_TRANSPORT_HARD_TIMEOUT_MS,
            "INSERT_CONTENT_TIMEOUT_MS = {} ms must leave at least {} ms margin under \
             MCP_TRANSPORT_HARD_TIMEOUT_MS = {} ms so partial-success envelopes are \
             reachable before the client times out",
            INSERT_CONTENT_TIMEOUT_MS,
            MIN_MARGIN_MS,
            MCP_TRANSPORT_HARD_TIMEOUT_MS,
        );
    }

    /// The resolve-walk budget must leave the same 60-second margin
    /// under the MCP transport hard timeout. Pre-2026-05-19 this was
    /// 5 minutes, which sat *above* the 4-minute transport cap — the
    /// client ripped the connection while the server-side walk
    /// continued running, burning rate-limit tokens on a call the
    /// caller had already abandoned. Subsequent calls then queued
    /// behind the orphan walk's tokens, producing the resolve_link
    /// cascade the user reported: API flapping between healthy and
    /// degraded under a cold 56k-node workspace cache. Pinned so a
    /// future contributor cannot regress the bound silently.
    #[test]
    fn resolve_walk_budget_leaves_mcp_transport_margin() {
        const MIN_MARGIN_MS: u64 = 60_000;
        assert!(
            RESOLVE_WALK_TIMEOUT_MS + MIN_MARGIN_MS <= MCP_TRANSPORT_HARD_TIMEOUT_MS,
            "RESOLVE_WALK_TIMEOUT_MS = {} ms must leave at least {} ms margin under \
             MCP_TRANSPORT_HARD_TIMEOUT_MS = {} ms so the resolve-walk completion or \
             truncation envelope is reachable before the client times out",
            RESOLVE_WALK_TIMEOUT_MS,
            MIN_MARGIN_MS,
            MCP_TRANSPORT_HARD_TIMEOUT_MS,
        );
    }

    #[test]
    fn secondbrain_dir_returns_env_value_when_set() {
        let key = SECONDBRAIN_DIR_ENV;
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "/tmp/wflow-secondbrain-test");
        let got = secondbrain_dir().expect("env path");
        assert_eq!(got.to_string_lossy(), "/tmp/wflow-secondbrain-test");
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn secondbrain_dir_returns_none_when_env_empty_or_unset() {
        // Pin the no-fallback contract: no machine-specific default
        // path lives in the source. An unset or empty env var must
        // yield None so callers treat the feature as disabled rather
        // than guessing a HOME-relative location.
        let key = SECONDBRAIN_DIR_ENV;
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "");
        assert!(secondbrain_dir().is_none(), "empty env disables");
        std::env::remove_var(key);
        assert!(secondbrain_dir().is_none(), "unset env disables");
        if let Some(v) = prev {
            std::env::set_var(key, v);
        }
    }

    #[test]
    fn session_logs_dir_appends_session_logs_subdir() {
        let key = SECONDBRAIN_DIR_ENV;
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "/tmp/wflow-secondbrain-test");
        let got = session_logs_dir().expect("env path");
        assert_eq!(got.to_string_lossy(), "/tmp/wflow-secondbrain-test/session-logs");
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
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
