/// Configuration and environment variable management
/// Addresses code review issue: "Incomplete config validation"

use crate::defaults;
use crate::error::{WorkflowyError, Result};
use std::env;

/// Validates all required configuration is present
pub fn validate_config() -> Result<Config> {
    let workflowy_api_key = env::var("WORKFLOWY_API_KEY")
        .map_err(|_| WorkflowyError::ConfigError {
            reason: "WORKFLOWY_API_KEY environment variable is not set".to_string(),
        })?;

    Ok(Config {
        workflowy_api_key,
        workflowy_base_url: defaults::WORKFLOWY_BASE_URL.to_owned(),
    })
}

#[derive(Clone, Debug)]
pub struct Config {
    pub workflowy_api_key: String,
    pub workflowy_base_url: String,
}

/// Cache configuration — re-exported from defaults for backward compatibility
pub const CACHE_TTL_SECS: u64 = defaults::CACHE_TTL_SECS;
pub const CACHE_MAX_SIZE: usize = defaults::CACHE_MAX_SIZE;

/// Retry configuration
pub struct RetryConfig {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub retryable_statuses: &'static [u16],
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: defaults::RETRY_MAX_ATTEMPTS,
            base_delay_ms: defaults::RETRY_BASE_DELAY_MS,
            max_delay_ms: defaults::RETRY_MAX_DELAY_MS,
            retryable_statuses: defaults::RETRY_STATUSES,
        }
    }
}

/// Request queue configuration - addresses high-load scenarios
pub struct QueueConfig {
    /// Maximum parallel requests to Workflowy API
    pub max_concurrency: usize,
    /// Milliseconds to wait before processing batch
    pub batch_delay_ms: u64,
    /// Maximum operations per batch
    pub max_batch_size: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            max_concurrency: defaults::QUEUE_MAX_CONCURRENCY,
            batch_delay_ms: defaults::QUEUE_BATCH_DELAY_MS,
            max_batch_size: defaults::QUEUE_MAX_BATCH_SIZE,
        }
    }
}

/// Rate limiting configuration for proactive throttling
pub struct RateLimitConfig {
    /// Maximum requests per second
    pub requests_per_second: u32,
    /// Allow bursts up to this size
    pub burst_size: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_second: defaults::RATE_LIMIT_RPS,
            burst_size: defaults::RATE_LIMIT_BURST,
        }
    }
}

/// Job queue configuration
pub struct JobQueueConfig {
    /// How long to keep completed jobs in history (seconds)
    pub completed_job_ttl_secs: u64,
    /// Maximum number of jobs to keep in history
    pub max_job_history: usize,
    /// Cleanup interval (seconds)
    pub cleanup_interval_secs: u64,
}

impl Default for JobQueueConfig {
    fn default() -> Self {
        Self {
            completed_job_ttl_secs: defaults::JOB_TTL_SECS,
            max_job_history: defaults::JOB_MAX_HISTORY,
            cleanup_interval_secs: defaults::JOB_CLEANUP_INTERVAL_SECS,
        }
    }
}
