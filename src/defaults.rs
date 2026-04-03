/// Centralized default values and constants for the Workflowy MCP server.
/// All magic numbers belong here so they're easy to find and tune.

// --- Cache ---
/// How long cached nodes remain valid (seconds)
pub const CACHE_TTL_SECS: u64 = 30;
/// Maximum number of entries the cache will hold
pub const CACHE_MAX_SIZE: usize = 10_000;

// --- Retry ---
/// Maximum number of retry attempts for API calls
pub const RETRY_MAX_ATTEMPTS: u32 = 3;
/// Initial delay between retries (milliseconds)
pub const RETRY_BASE_DELAY_MS: u64 = 1_000;
/// Maximum delay between retries (milliseconds)
pub const RETRY_MAX_DELAY_MS: u64 = 10_000;
/// HTTP status codes that trigger a retry
pub const RETRY_STATUSES: &[u16] = &[429, 500, 502, 503, 504, 408];

// --- Rate Limiting ---
/// Sustained request rate (requests per second)
pub const RATE_LIMIT_RPS: u32 = 5;
/// Maximum burst size above sustained rate
pub const RATE_LIMIT_BURST: u32 = 10;

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
/// Hard cap on max_results for any search/list tool
pub const HARD_MAX_RESULTS: usize = 100;
/// Default max_results when not specified
pub const DEFAULT_MAX_RESULTS: usize = 20;
/// Maximum response text size (~50KB)
pub const MAX_RESPONSE_SIZE: usize = 50_000;
/// Default tool timeout (seconds)
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 30;

// --- Tree Traversal ---
/// Default max_depth for search operations
pub const DEFAULT_SEARCH_DEPTH: usize = 3;
/// Default max_depth for review/todo operations
pub const DEFAULT_REVIEW_DEPTH: usize = 5;
/// Maximum recursion depth for subtree fetching
pub const MAX_TREE_DEPTH: usize = 10;

// --- API ---
/// Workflowy API base URL
pub const WORKFLOWY_BASE_URL: &str = "https://workflowy.com/api/v1";
/// HTTP client timeout (seconds)
pub const HTTP_TIMEOUT_SECS: u64 = 30;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_are_reasonable() {
        assert!(CACHE_TTL_SECS > 0 && CACHE_TTL_SECS <= 300);
        assert!(RETRY_MAX_ATTEMPTS >= 1 && RETRY_MAX_ATTEMPTS <= 10);
        assert!(RATE_LIMIT_RPS >= 1);
        assert!(RATE_LIMIT_BURST >= RATE_LIMIT_RPS);
        assert!(HARD_MAX_RESULTS >= DEFAULT_MAX_RESULTS);
        assert!(DEFAULT_REVIEW_DEPTH >= DEFAULT_SEARCH_DEPTH);
        assert!(MAX_TREE_DEPTH >= DEFAULT_REVIEW_DEPTH);
        assert!(!RETRY_STATUSES.is_empty());
        assert!(RETRY_STATUSES.contains(&429)); // rate limit must be retried
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
