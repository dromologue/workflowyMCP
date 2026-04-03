/// Custom error types for the MCP server
/// Addresses code review issue: "Incomplete error context"

use thiserror::Error;

#[derive(Error, Debug)]
pub enum WorkflowyError {
    #[error("API error {status}: {message}")]
    ApiError {
        status: u16,
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Retry failed after {attempts} attempts: {reason}")]
    RetryExhausted { attempts: u32, reason: String },

    #[error("Request queue timeout: {operation}")]
    QueueTimeout { operation: String },

    #[error("Job {job_id} failed: {message}")]
    JobFailed { job_id: String, message: String },

    #[error("Invalid path: {reason}")]
    InvalidPath { reason: String },

    #[error("Invalid input: {reason}")]
    InvalidInput { reason: String },

    #[error("Configuration error: {reason}")]
    ConfigError { reason: String },

    #[error("Cache error: {reason}")]
    CacheError { reason: String },

    #[error("Parse error: {reason}")]
    ParseError { reason: String },

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("Timeout")]
    Timeout,

    #[error("Internal error: {0}")]
    Internal(String),
}

impl WorkflowyError {
    pub fn api_error(status: u16, message: impl Into<String>) -> Self {
        Self::ApiError {
            status,
            message: message.into(),
            source: None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            WorkflowyError::ApiError { status, .. } if matches!(*status, 429 | 500 | 502 | 503 | 504)
        )
    }

    pub fn is_path_traversal(&self) -> bool {
        matches!(self, WorkflowyError::InvalidPath { .. })
    }
}

pub type Result<T> = std::result::Result<T, WorkflowyError>;
