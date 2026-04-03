/// Configuration and environment variable management
/// Addresses code review issue: "Incomplete config validation"

use crate::error::{WorkflowyError, Result};
use std::env;

/// Validates all required configuration is present
pub fn validate_config() -> Result<Config> {
    // Required variables
    let workflowy_api_key = env::var("WORKFLOWY_API_KEY")
        .map_err(|_| WorkflowyError::ConfigError {
            reason: "WORKFLOWY_API_KEY environment variable is not set".to_string(),
        })?;

    // Optional Dropbox config - must be all or none
    let dropbox_app_key = env::var("DROPBOX_APP_KEY").ok();
    let dropbox_app_secret = env::var("DROPBOX_APP_SECRET").ok();
    let dropbox_refresh_token = env::var("DROPBOX_REFRESH_TOKEN").ok();

    let dropbox_config_count = [&dropbox_app_key, &dropbox_app_secret, &dropbox_refresh_token]
        .iter()
        .filter(|v| v.is_some())
        .count();

    if dropbox_config_count > 0 && dropbox_config_count < 3 {
        return Err(WorkflowyError::ConfigError {
            reason:
                "Dropbox configuration incomplete: need all three (DROPBOX_APP_KEY, DROPBOX_APP_SECRET, DROPBOX_REFRESH_TOKEN) or none"
                    .to_string(),
        });
    }

    #[cfg(not(debug_assertions))]
    {
        // Ensure secrets aren't logged in production
        if env::var("RUST_LOG").is_ok() && env::var("RUST_LOG").unwrap().contains("debug") {
            tracing::warn!("Debug logging enabled; ensure WORKFLOWY_API_KEY is not logged");
        }
    }

    Ok(Config {
        workflowy_api_key,
        workflowy_base_url: "https://workflowy.com/api/v1".to_string(),
        dropbox_config: if dropbox_config_count == 3 {
            Some(DropboxConfig {
                app_key: dropbox_app_key.unwrap(),
                app_secret: dropbox_app_secret.unwrap(),
                refresh_token: dropbox_refresh_token.unwrap(),
            })
        } else {
            None
        },
    })
}

#[derive(Clone, Debug)]
pub struct Config {
    pub workflowy_api_key: String,
    pub workflowy_base_url: String,
    pub dropbox_config: Option<DropboxConfig>,
}

#[derive(Clone, Debug)]
pub struct DropboxConfig {
    pub app_key: String,
    pub app_secret: String,
    pub refresh_token: String,
}

/// Cache configuration
pub const CACHE_TTL_SECS: u64 = 30;
pub const CACHE_MAX_SIZE: usize = 10000;

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
            max_attempts: 3,
            base_delay_ms: 1000,
            max_delay_ms: 10000,
            retryable_statuses: &[429, 500, 502, 503, 504, 408],
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
            max_concurrency: 3,
            batch_delay_ms: 50,
            max_batch_size: 20,
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
            requests_per_second: 5,
            burst_size: 10,
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
            completed_job_ttl_secs: 1800,  // 30 minutes
            max_job_history: 1000,          // Never keep more than 1000 historical jobs
            cleanup_interval_secs: 60,
        }
    }
}
