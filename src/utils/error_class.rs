//! Shared proximate-cause classification.
//!
//! The cause taxonomy and the string→cause classifier live here so the MCP
//! server (`server::classify_operational_error`, which adds MCP-specific
//! `ErrorCode` + hint mapping on top) and the `wflow-do` CLI (`classify`,
//! which only needs the stable string) route through ONE implementation.
//! Before this lift the CLI carried a hand-copied branch chain that drifted:
//! it was missing the `rate_limited` (429) branch the server added 2026-06-17,
//! so a rate-limited CLI error misclassified as `unknown`. A single source
//! makes that drift impossible.

/// Discrete proximate-cause classification for a tool failure. Brief
/// 2026-04-25 Test γ requires every error to carry one of these values so
/// callers can route on the cause rather than parsing human-readable hint
/// strings. Variants map 1:1 to the brief's requested enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProximateCause {
    Timeout,
    LockContention,
    CacheMiss,
    UpstreamError,
    Cancelled,
    NotFound,
    AuthFailure,
    /// Caller-supplied parameter is wrong (missing required field, invalid
    /// value, etc.). Distinguishes validation failures from operational ones —
    /// added 2026-05-03 alongside `tool_invalid_params`. Never produced by
    /// [`ProximateCause::from_error_message`] (validation failures are caught
    /// at the parameter boundary, not classified from an operational error
    /// string); the variant exists for the `tool_invalid_params` path.
    InvalidParams,
    /// Upstream returned 429 (or we short-circuited inside an open
    /// `retry_after` window). A *recoverable transient*, distinct from
    /// `UpstreamError`: the correct response is to wait the `retry_after` and
    /// retry, not to treat the write as failed. Added 2026-06-17 because
    /// `tool_error` previously fell a 429 through to `Unknown`, so a
    /// rate-limited write surfaced `proximate_cause: "unknown"` with no
    /// `retry_after` in the hint even though the status path
    /// (`classify_degraded_kind`) already classified the same 429 as
    /// `rate_limited`. The two envelopes now agree.
    RateLimited,
    Unknown,
}

impl ProximateCause {
    /// String form for the JSON-RPC `data.proximate_cause` field. Stable over
    /// the lifetime of the API: callers may match on these values.
    pub const fn as_str(self) -> &'static str {
        match self {
            ProximateCause::Timeout => "timeout",
            ProximateCause::LockContention => "lock_contention",
            ProximateCause::CacheMiss => "cache_miss",
            ProximateCause::UpstreamError => "upstream_error",
            ProximateCause::Cancelled => "cancelled",
            ProximateCause::NotFound => "not_found",
            ProximateCause::AuthFailure => "auth_failure",
            ProximateCause::InvalidParams => "invalid_params",
            ProximateCause::RateLimited => "rate_limited",
            ProximateCause::Unknown => "unknown",
        }
    }

    /// Classify an operational error string into its proximate cause. The
    /// 429/rate-limit branch is FIRST so it cannot be shadowed by a later
    /// string match (pre-2026-06-17 a 429 fell through to `Unknown`, burying
    /// the `retry_after`). Single source for both the MCP server and the CLI.
    pub fn from_error_message(err_str: &str) -> ProximateCause {
        let lower = err_str.to_lowercase();
        if lower.contains("429") || lower.contains("rate limit") {
            ProximateCause::RateLimited
        } else if lower.contains("404") || lower.contains("not found") {
            ProximateCause::NotFound
        } else if lower.contains("cancelled") {
            ProximateCause::Cancelled
        } else if lower.contains("timeout") || lower.contains("timed out") {
            ProximateCause::Timeout
        } else if lower.contains("api error 5") {
            ProximateCause::UpstreamError
        } else if lower.contains("401") || lower.contains("403") || lower.contains("unauthor") {
            ProximateCause::AuthFailure
        } else if lower.contains("lock") {
            ProximateCause::LockContention
        } else if lower.contains("cache") {
            ProximateCause::CacheMiss
        } else {
            ProximateCause::Unknown
        }
    }

    /// Whether a failure with this cause is worth retrying unchanged. A
    /// recoverable transient (rate limit, timeout, upstream blip, lock
    /// contention, propagation-lag 404, preemption) is retryable; an auth
    /// failure, a validation error, or an unknown fault is not. Derived from
    /// the cause — never hand-set per branch — so `retryable` cannot drift
    /// from the classification. The 2026-06-17 write-path report called
    /// `retryable` one of the two fields that would have changed the session.
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            ProximateCause::RateLimited
                | ProximateCause::Timeout
                | ProximateCause::UpstreamError
                | ProximateCause::LockContention
                | ProximateCause::CacheMiss
                | ProximateCause::NotFound
                | ProximateCause::Cancelled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_branch_is_first_and_not_shadowed() {
        // A 429 message that also contains "timeout"-ish words must still
        // classify as rate_limited (the branch is first).
        assert_eq!(
            ProximateCause::from_error_message("API error 429: rate limit; request timed out"),
            ProximateCause::RateLimited
        );
        assert_eq!(
            ProximateCause::from_error_message("rate limit exceeded"),
            ProximateCause::RateLimited
        );
    }

    #[test]
    fn from_error_message_covers_every_operational_branch() {
        let cases = [
            ("HTTP 429 too many requests", ProximateCause::RateLimited),
            ("node 404 not found", ProximateCause::NotFound),
            ("walk cancelled by cancel_all", ProximateCause::Cancelled),
            ("upstream timeout", ProximateCause::Timeout),
            ("request timed out", ProximateCause::Timeout),
            ("API error 503 service down", ProximateCause::UpstreamError),
            ("401 unauthorized", ProximateCause::AuthFailure),
            ("403 forbidden", ProximateCause::AuthFailure),
            ("unauthorized", ProximateCause::AuthFailure),
            ("internal lock contention", ProximateCause::LockContention),
            ("stale cache entry", ProximateCause::CacheMiss),
            ("something we have never seen", ProximateCause::Unknown),
        ];
        for (msg, want) in cases {
            assert_eq!(ProximateCause::from_error_message(msg), want, "msg: {msg}");
        }
    }

    #[test]
    fn retryable_matches_recoverable_transients() {
        for c in [
            ProximateCause::RateLimited,
            ProximateCause::Timeout,
            ProximateCause::UpstreamError,
            ProximateCause::LockContention,
            ProximateCause::CacheMiss,
            ProximateCause::NotFound,
            ProximateCause::Cancelled,
        ] {
            assert!(c.is_retryable(), "{} should be retryable", c.as_str());
        }
        for c in [
            ProximateCause::AuthFailure,
            ProximateCause::InvalidParams,
            ProximateCause::Unknown,
        ] {
            assert!(!c.is_retryable(), "{} should not be retryable", c.as_str());
        }
    }
}
