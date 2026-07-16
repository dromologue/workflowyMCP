//! Truncation-envelope helpers shared between the MCP server handlers
//! (`src/server/mod.rs`) and the lifted workflows (`src/workflows.rs`)
//! that build response payloads for both the MCP and CLI surfaces.
//!
//! The four-field truncation envelope (`truncated`, `truncation_limit`,
//! `truncation_reason`, `truncation_recovery_hint`) is the canonical
//! shape every walk-shaped tool's JSON payload merges in so a caller
//! hitting the 20 s walk budget gets the same recovery info regardless
//! of which tool it called. Pre-2026-05-22 the helpers lived in
//! `server/mod.rs`; they were extracted here so the lifted resolve_link
//! workflow could call the same constructors as the server handler,
//! keeping the MCP and CLI envelope shapes byte-identical.
//!
//! Pin tests in `src/server/mod.rs::tests`:
//! - `every_walk_tool_emits_full_truncation_envelope_in_json` — every
//!   inline `"truncation_limit":` in mod.rs carries reason + hint
//!   companions.
//! - `envelope_construction_routes_through_one_helper_no_inline_fields`
//!   — mod.rs has no inline envelope construction outside test bodies;
//!   the helpers in *this* module are the only emitters.

use crate::api::TruncationReason;
use serde_json::{json, Map, Value};

/// Generic recovery hint surfaced on every truncated response that
/// doesn't carry a tool-specific hint. Points the caller at the
/// `use_index=true` bypass on the search tools. Tools whose failure
/// mode isn't name-keyed (notably `resolve_link`, which resolves a
/// short hash) should pass their own hint via the `_with_hint` variants.
pub const TRUNCATION_RECOVERY_HINT: &str = "Call build_name_index(parent_id=...) once to populate the persistent name index, then re-issue with use_index=true (search_nodes / find_node) to bypass the walk budget — name-only match, no walk timeout.";

/// Recovery hint for [`TruncationReason::SkippedBranches`]. Overrides both
/// the generic hint and any tool-specific one, because this failure mode
/// does not depend on which tool ran the walk — the branches were dropped
/// below every tool, in the walk itself.
///
/// WHY it must override: every other hint points at the persistent name
/// index, and the index is exactly what a dropped branch is missing from —
/// it is populated by these same walks. Telling a caller to consult the
/// index to recover from a partial walk would send them to a source that
/// silently shares the gap, which is the 2026-07-16 defect restated rather
/// than a recovery. Narrowing the scope is equally wrong: a narrower walk
/// drops the same branches.
pub const SKIPPED_BRANCHES_RECOVERY_HINT: &str = "The walk hit no budget but dropped one or more branches whose child fetch kept failing (typically upstream rate-limiting), so whole subtrees are missing and the persistent name index shares the gap. Do NOT treat these results — or an index lookup — as a full picture. Retry once upstream pressure clears; check workflowy_status for an open retry_after window first.";

/// Build the four-field truncation envelope as a `serde_json::Map` ready
/// to merge into a tool's JSON payload. `recovery_hint` defaults to
/// [`TRUNCATION_RECOVERY_HINT`] in the no-hint variant; tools whose
/// generic hint is misleading (e.g. `resolve_link`'s short-hash failure
/// mode) pass a tool-specific hint via [`truncation_envelope_with_hint`].
pub fn truncation_envelope(
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
) -> Map<String, Value> {
    truncation_envelope_with_hint(truncated, limit, reason, TRUNCATION_RECOVERY_HINT)
}

/// Variant of [`truncation_envelope`] that takes a tool-specific
/// recovery hint. Currently used by `resolve_link` (the generic hint
/// points at `use_index=true` on `find_node`/`search_nodes`, which is
/// the wrong tool for a short-hash failure).
pub fn truncation_envelope_with_hint(
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
    recovery_hint: &str,
) -> Map<String, Value> {
    // A dropped-branch walk gets its own hint whatever the caller passed:
    // the failure is in the walk, not in the tool, so no tool-specific hint
    // is right for it — and every other hint points at the name index, which
    // shares the gap.
    let recovery_hint = match reason {
        Some(TruncationReason::SkippedBranches) => SKIPPED_BRANCHES_RECOVERY_HINT,
        _ => recovery_hint,
    };
    let mut m = Map::new();
    m.insert("truncated".into(), json!(truncated));
    m.insert("truncation_limit".into(), json!(limit));
    m.insert("truncation_reason".into(), json!(reason.map(|r| r.as_str())));
    m.insert(
        "truncation_recovery_hint".into(),
        json!(if truncated { recovery_hint } else { "" }),
    );
    m
}

/// Combine a caller-built JSON payload with the four-field truncation
/// envelope. Every walk-shaped tool's success path uses this to return
/// a single `serde_json::Value` carrying both the tool-specific fields
/// and the envelope.
pub fn with_truncation_envelope(
    payload: Value,
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
) -> Value {
    with_truncation_envelope_and_hint(
        payload,
        truncated,
        limit,
        reason,
        TRUNCATION_RECOVERY_HINT,
    )
}

/// Variant of [`with_truncation_envelope`] that takes a tool-specific
/// recovery hint string.
pub fn with_truncation_envelope_and_hint(
    mut payload: Value,
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
    recovery_hint: &str,
) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.extend(truncation_envelope_with_hint(
            truncated,
            limit,
            reason,
            recovery_hint,
        ));
    } else {
        // Defensive: non-object payloads are a caller bug. Wrap so the
        // envelope is still attached, surfacing the misuse.
        let mut wrapped = Map::new();
        wrapped.insert("payload".into(), payload);
        wrapped.extend(truncation_envelope_with_hint(
            truncated,
            limit,
            reason,
            recovery_hint,
        ));
        return Value::Object(wrapped);
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_branches_envelope_overrides_even_a_tool_specific_hint() {
        // The reason-specific hint must win over a caller's own hint: the
        // branches were dropped in the walk, below whichever tool called it,
        // so a tool-specific hint cannot describe the recovery. Pins the
        // 2026-07-16 rule that no truncated response for this reason may
        // point the caller at the name index.
        let env = truncation_envelope_with_hint(
            true,
            10_000,
            Some(TruncationReason::SkippedBranches),
            "resolve_link specific hint",
        );
        let hint = env["truncation_recovery_hint"].as_str().expect("hint");
        assert_eq!(hint, SKIPPED_BRANCHES_RECOVERY_HINT);
        assert_eq!(env["truncation_reason"], "skipped_branches");
        assert!(
            !hint.contains("use_index=true"),
            "must not send the caller to the index, which shares the gap: {hint}"
        );
    }

    #[test]
    fn other_reasons_keep_their_tool_specific_hint() {
        let env = truncation_envelope_with_hint(
            true,
            10_000,
            Some(TruncationReason::Timeout),
            "tool specific hint",
        );
        assert_eq!(env["truncation_recovery_hint"], "tool specific hint");
    }

    #[test]
    fn untruncated_envelope_carries_no_hint() {
        let env = truncation_envelope(false, 10_000, None);
        assert_eq!(env["truncation_recovery_hint"], "");
        assert_eq!(env["truncated"], false);
        assert_eq!(env["truncation_reason"], Value::Null);
    }
}
