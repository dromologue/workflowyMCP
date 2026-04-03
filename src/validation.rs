/// Input validation utilities for MCP tool parameters.
/// Addresses: MCP Principle #14 (API Design Fundamentals) and #5 (Security First)

use crate::error::{WorkflowyError, Result};

/// Maximum length for text query inputs
pub const MAX_QUERY_LENGTH: usize = 1000;

/// Maximum length for node name
pub const MAX_NAME_LENGTH: usize = 5000;

/// Maximum length for node description
pub const MAX_DESCRIPTION_LENGTH: usize = 50_000;

/// Maximum length for insert_content body
pub const MAX_CONTENT_LENGTH: usize = 500_000;

/// Hard cap on max_results for any search/list tool
pub const HARD_MAX_RESULTS: usize = 100;

/// Default max_results when not specified
pub const DEFAULT_MAX_RESULTS: usize = 20;

/// Maximum response text size in bytes (~50KB)
pub const MAX_RESPONSE_SIZE: usize = 50_000;

/// Default tool timeout in seconds
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 30;

/// Validate that a string looks like a UUID (Workflowy node ID).
/// Accepts both hyphenated and non-hyphenated UUIDs.
pub fn validate_node_id(id: &str) -> Result<()> {
    // Workflowy uses standard UUID format: 8-4-4-4-12 hex chars
    let stripped = id.replace('-', "");
    if stripped.len() != 32 || !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WorkflowyError::InvalidInput {
            reason: format!(
                "Invalid node ID '{}': must be a valid UUID (e.g. 550e8400-e29b-41d4-a716-446655440000)",
                id
            ),
        });
    }
    Ok(())
}

/// Validate text input: no null bytes, no control characters (except newline/tab), length cap.
pub fn validate_text(value: &str, field_name: &str, max_length: usize) -> Result<()> {
    if value.len() > max_length {
        return Err(WorkflowyError::InvalidInput {
            reason: format!(
                "{} exceeds maximum length ({} > {} bytes)",
                field_name,
                value.len(),
                max_length
            ),
        });
    }

    if value.contains('\0') {
        return Err(WorkflowyError::InvalidInput {
            reason: format!("{} contains null bytes", field_name),
        });
    }

    // Reject control characters except \n, \r, \t
    if value.chars().any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t') {
        return Err(WorkflowyError::InvalidInput {
            reason: format!("{} contains invalid control characters", field_name),
        });
    }

    Ok(())
}

/// Clamp max_results to the hard cap
pub fn clamp_max_results(requested: Option<usize>, default: usize) -> usize {
    requested.unwrap_or(default).min(HARD_MAX_RESULTS)
}

/// Truncate response text to MAX_RESPONSE_SIZE with a continuation indicator.
/// Returns (text, was_truncated).
pub fn truncate_response(text: &str, total_count: Option<usize>) -> String {
    if text.len() <= MAX_RESPONSE_SIZE {
        return text.to_string();
    }

    // Find a safe truncation point (don't split mid-line)
    let truncate_at = text[..MAX_RESPONSE_SIZE]
        .rfind('\n')
        .unwrap_or(MAX_RESPONSE_SIZE);

    let suffix = if let Some(total) = total_count {
        format!("\n\n... (response truncated at {}KB, {} total items — use offset/max_results to paginate)",
            MAX_RESPONSE_SIZE / 1000, total)
    } else {
        format!("\n\n... (response truncated at {}KB)", MAX_RESPONSE_SIZE / 1000)
    };

    format!("{}{}", &text[..truncate_at], suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_uuid_hyphenated() {
        assert!(validate_node_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
    }

    #[test]
    fn test_valid_uuid_no_hyphens() {
        assert!(validate_node_id("550e8400e29b41d4a716446655440000").is_ok());
    }

    #[test]
    fn test_invalid_uuid_too_short() {
        assert!(validate_node_id("abc123").is_err());
    }

    #[test]
    fn test_invalid_uuid_non_hex() {
        assert!(validate_node_id("550e8400-e29b-41d4-a716-44665544zzzz").is_err());
    }

    #[test]
    fn test_text_with_null_bytes() {
        assert!(validate_text("hello\0world", "test", 100).is_err());
    }

    #[test]
    fn test_text_too_long() {
        let long = "a".repeat(101);
        assert!(validate_text(&long, "test", 100).is_err());
    }

    #[test]
    fn test_text_valid() {
        assert!(validate_text("hello\nworld\ttab", "test", 100).is_ok());
    }

    #[test]
    fn test_text_control_chars() {
        assert!(validate_text("hello\x01world", "test", 100).is_err());
    }

    #[test]
    fn test_clamp_max_results() {
        assert_eq!(clamp_max_results(None, 20), 20);
        assert_eq!(clamp_max_results(Some(50), 20), 50);
        assert_eq!(clamp_max_results(Some(200), 20), 100);
    }

    #[test]
    fn test_truncate_short_response() {
        let text = "short text";
        assert_eq!(truncate_response(text, None), text);
    }

    #[test]
    fn test_truncate_long_response() {
        let text = "a\n".repeat(30_000); // ~60KB
        let result = truncate_response(&text, Some(500));
        assert!(result.len() < 55_000);
        assert!(result.contains("truncated"));
        assert!(result.contains("500 total items"));
    }
}
