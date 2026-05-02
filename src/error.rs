//! Custom error types for the MCP server.
//! Addresses code review issue: "Incomplete error context".

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

    #[error("Cancelled")]
    Cancelled,

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

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    pub fn parse(reason: impl Into<String>) -> Self {
        Self::ParseError { reason: reason.into() }
    }

    /// Whether the request layer should re-enter its backoff loop on this
    /// error. Covers two families of transient failure:
    /// * server-side: 429 + 5xx status codes;
    /// * transport-side: connect/read/body timeouts and dropped requests
    ///   surfaced by reqwest as `HttpError`. Without this arm the first
    ///   read-timeout against a slow upstream returns `RetryExhausted`
    ///   after a single attempt.
    pub fn is_retryable(&self) -> bool {
        match self {
            WorkflowyError::ApiError { status, .. } => {
                matches!(*status, 429 | 500 | 502 | 503 | 504)
            }
            WorkflowyError::HttpError(e) => {
                e.is_timeout() || e.is_connect() || e.is_request() || e.is_body()
            }
            _ => false,
        }
    }

    pub fn is_path_traversal(&self) -> bool {
        matches!(self, WorkflowyError::InvalidPath { .. })
    }
}

pub type Result<T> = std::result::Result<T, WorkflowyError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_retryable_429() {
        let err = WorkflowyError::api_error(429, "Rate limited");
        assert!(err.is_retryable());
    }

    #[test]
    fn test_is_retryable_500() {
        let err = WorkflowyError::api_error(500, "Server error");
        assert!(err.is_retryable());
    }

    #[test]
    fn test_is_not_retryable_404() {
        let err = WorkflowyError::api_error(404, "Not found");
        assert!(!err.is_retryable());
    }

    /// Forces a real reqwest timeout by giving the client a 1 ns budget.
    /// The error returned has `is_timeout() == true`, which is exactly the
    /// failure mode upstream slowness produces in production. Without the
    /// HttpError arm in `is_retryable`, this returns false and the request
    /// layer gives up after a single attempt.
    #[tokio::test]
    async fn test_is_retryable_transport_timeout() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_nanos(1))
            .build()
            .unwrap();
        let reqwest_err = client
            .get("https://workflowy.com")
            .send()
            .await
            .expect_err("1 ns timeout must fail");
        assert!(reqwest_err.is_timeout(), "expected a timeout error");
        let err: WorkflowyError = reqwest_err.into();
        assert!(err.is_retryable(), "transport timeouts must be retryable");
    }

    #[test]
    fn test_internal_helper() {
        let err = WorkflowyError::internal("test error");
        assert!(matches!(err, WorkflowyError::Internal(msg) if msg == "test error"));
    }

    #[test]
    fn test_parse_helper() {
        let err = WorkflowyError::parse("bad json");
        assert!(matches!(err, WorkflowyError::ParseError { reason } if reason == "bad json"));
    }
}
