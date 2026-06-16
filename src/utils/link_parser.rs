//! Workflowy link → short-hash extractor.
//!
//! Single canonical entry point for turning any user-supplied "link"
//! string into a normalised lowercase hex short hash (12 chars, 8
//! chars, or a 32-char UUID). Used by both `resolve_link` (MCP) and
//! `wflow-do resolve-link` (CLI) so the two surfaces cannot drift on
//! URL parsing — the pre-2026-05-19 inline parsers each had subtly
//! different bugs, the most painful of which silently treated every
//! hex character anywhere in the URL as part of the hash and so
//! produced garbage on URLs carrying `?focusedItem=…` query parameters
//! or other ancillary hex.
//!
//! The supported input forms, in priority order:
//!
//! 1. `?focusedItem=<hash>` query parameter (Workflowy's "share this
//!    bullet, focused under that parent" link form — the focusedItem
//!    is the *inner* target, so it beats the path fragment).
//! 2. `/#/<hash>` URL fragment (address-bar form after focusing a node).
//! 3. `/s/<slug>/<hash>` shared-URL trailing segment.
//! 4. Bare 32-char UUID (with or without hyphens).
//! 5. Bare 12-char URL-suffix short hash.
//! 6. Bare 8-char doc-form prefix short hash.
//!
//! Anything else returns `None` — the caller should surface a typed
//! invalid-params error naming what was tried, not silently invent a
//! hash.

/// Extract a normalised lowercase hex hash from a user-supplied link
/// string. See module docs for the supported input forms.
///
/// Returns `Some(hash)` with `hash.len()` in `{8, 12, 32}` — never
/// hyphenated, always lowercase. Returns `None` when no recognised
/// form matches.
pub fn extract_workflowy_short_hash(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 1. `?focusedItem=<hash>` (case-insensitive on the key — the
    // browser preserves the case Workflowy emits, but a paste-from-
    // chat round-trip can mangle it; this branch is the most common
    // failure mode the 2026-05-19 user-report surfaced).
    if let Some(hash) = extract_query_param_value(trimmed, "focusedItem") {
        if let Some(norm) = normalise_hex_candidate(&hash) {
            return Some(norm);
        }
    }

    // 2. `/#/<hash>` URL fragment — take everything after `/#/` up to
    // the next `?` / `&` / `/` so a trailing query string doesn't
    // pollute the candidate.
    if let Some(idx) = trimmed.find("/#/") {
        let after = &trimmed[idx + "/#/".len()..];
        let end = after
            .find(|c: char| c == '?' || c == '&' || c == '/' || c == '#')
            .unwrap_or(after.len());
        let candidate = &after[..end];
        if let Some(norm) = normalise_hex_candidate(candidate) {
            return Some(norm);
        }
    }

    // 3. `/s/<slug>/<hash>` — Workflowy's share-link form. The hash is
    // the last `/`-separated segment of the URL path.
    if trimmed.contains("/s/") {
        // Strip query / fragment so we look at the path only.
        let path_only = trimmed
            .split_once('?')
            .map(|(a, _)| a)
            .unwrap_or(trimmed)
            .split_once('#')
            .map(|(a, _)| a)
            .unwrap_or(trimmed);
        if let Some(last) = path_only.rsplit('/').next() {
            if let Some(norm) = normalise_hex_candidate(last) {
                return Some(norm);
            }
        }
    }

    // 4–6. Bare input. Strip anything that isn't hex or hyphen, then
    // check the resulting length. We deliberately do NOT keep hex
    // characters from across the whole input here — only from the
    // last path segment after the leading `#` — because a URL that
    // slipped past the branches above is almost certainly malformed
    // and we'd rather return None than invent a hash from incidental
    // hex letters in domain names or query keys.
    let last_seg = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let last_seg = last_seg.trim_start_matches('#');
    let end = last_seg
        .find(|c: char| c == '?' || c == '&' || c == '/' || c == '#')
        .unwrap_or(last_seg.len());
    let candidate = &last_seg[..end];
    if let Some(norm) = normalise_hex_candidate(candidate) {
        return Some(norm);
    }

    None
}

/// Backlink predicate: does `node` reference `target_uuid` in its name
/// or description?
///
/// True iff the name OR description contains either (a) the canonical
/// Workflowy link URL `https://workflowy.com/#/{uuid}` for the full
/// target UUID, OR (b) the bare 12-char trailing short hash of the
/// target. The hash comparison is case-insensitive; the full-URL match
/// is a plain substring (UUIDs are already lowercase hex on the wire).
///
/// Single source of truth for the backlink match predicate, shared by
/// the MCP `find_backlinks` handler and the `wflow-do backlinks`
/// subcommand. Pre-2026-06-16 the MCP matched the full URL only (via a
/// regex) while the CLI did a bare substring scan over the UUID and its
/// short hash; this helper is the UNION of both, so a node linking via
/// either form is found on both surfaces.
pub fn node_links_to(node: &crate::types::WorkflowyNode, target_uuid: &str) -> bool {
    let uuid = target_uuid.trim();
    if uuid.is_empty() {
        return false;
    }
    let full_url = format!("https://workflowy.com/#/{}", uuid);
    let short_hash = if uuid.len() >= 12 {
        &uuid[uuid.len() - 12..]
    } else {
        uuid
    };
    let short_lower = short_hash.to_lowercase();

    let field_links = |text: &str| -> bool {
        text.contains(&full_url) || text.to_lowercase().contains(&short_lower)
    };

    field_links(&node.name)
        || node
            .description
            .as_deref()
            .map(field_links)
            .unwrap_or(false)
}

/// Pull the value of `?<key>=<value>` (or `&<key>=<value>`) out of a
/// URL-like string. Matches the key case-insensitively. Stops at the
/// next `&` or `#`. Returns `None` when the key is absent.
fn extract_query_param_value(input: &str, key: &str) -> Option<String> {
    let key_lower = key.to_lowercase();
    let mut search_from = 0;
    while let Some(idx) = input[search_from..].find('=') {
        let abs_idx = search_from + idx;
        let prefix = &input[..abs_idx];
        let key_start = prefix
            .rfind(|c: char| c == '?' || c == '&')
            .map(|i| i + 1)
            .unwrap_or(0);
        let observed_key = &input[key_start..abs_idx];
        if observed_key.to_lowercase() == key_lower {
            let value_start = abs_idx + 1;
            let after = &input[value_start..];
            let end = after
                .find(|c: char| c == '&' || c == '#')
                .unwrap_or(after.len());
            return Some(after[..end].to_string());
        }
        search_from = abs_idx + 1;
    }
    None
}

/// Strip hyphens, lowercase, and require the result to be all hex with
/// length 8, 12, or 32. Returns the canonical form or `None`.
fn normalise_hex_candidate(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|c| *c != '-').collect();
    if !matches!(cleaned.len(), 8 | 12 | 32) {
        return None;
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(cleaned.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::WorkflowyNode;

    fn node_with(name: &str, desc: Option<&str>) -> WorkflowyNode {
        WorkflowyNode {
            id: "n1".into(),
            name: name.into(),
            description: desc.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn node_links_to_matches_full_url_and_short_hash() {
        let target = "550e8400e29b41d4a716446655440000";
        let short_hash = "446655440000"; // trailing 12 chars

        // Full canonical URL in the name.
        let n1 = node_with(
            "see https://workflowy.com/#/550e8400e29b41d4a716446655440000 for context",
            None,
        );
        assert!(node_links_to(&n1, target));

        // Full canonical URL in the description.
        let n2 = node_with(
            "plain",
            Some("ref https://workflowy.com/#/550e8400e29b41d4a716446655440000"),
        );
        assert!(node_links_to(&n2, target));

        // Bare 12-char short hash anywhere in the text.
        let n3 = node_with(&format!("linked to {}", short_hash), None);
        assert!(node_links_to(&n3, target));

        // Case-insensitive on the short hash.
        let n4 = node_with(&short_hash.to_uppercase(), None);
        assert!(node_links_to(&n4, target));

        // No reference → no match.
        let n5 = node_with("unrelated text", Some("nothing here"));
        assert!(!node_links_to(&n5, target));

        // Empty target → never matches.
        assert!(!node_links_to(&n1, ""));
    }

    #[test]
    fn bare_12_char_hash_passes_through() {
        assert_eq!(
            extract_workflowy_short_hash("c4ae1944b67e"),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn uppercase_bare_hash_normalises_to_lowercase() {
        assert_eq!(
            extract_workflowy_short_hash("C4AE1944B67E"),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn bare_8_char_doc_form_prefix_passes_through() {
        assert_eq!(
            extract_workflowy_short_hash("c4ae1944"),
            Some("c4ae1944".to_string()),
        );
    }

    #[test]
    fn bare_full_uuid_passes_through_unhyphenated() {
        assert_eq!(
            extract_workflowy_short_hash("550e8400-e29b-41d4-a716-446655440000"),
            Some("550e8400e29b41d4a716446655440000".to_string()),
        );
    }

    #[test]
    fn bare_full_uuid_unhyphenated_passes_through() {
        assert_eq!(
            extract_workflowy_short_hash("550e8400e29b41d4a716446655440000"),
            Some("550e8400e29b41d4a716446655440000".to_string()),
        );
    }

    #[test]
    fn url_fragment_form_extracts_trailing_hash() {
        assert_eq!(
            extract_workflowy_short_hash("https://workflowy.com/#/c4ae1944b67e"),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn url_fragment_with_trailing_query_extracts_fragment_only() {
        assert_eq!(
            extract_workflowy_short_hash("https://workflowy.com/#/c4ae1944b67e?someKey=val"),
            Some("c4ae1944b67e".to_string()),
        );
    }

    /// THIS IS THE 2026-05-19 USER-REPORT BUG.
    /// Workflowy's "copy link to this bullet" emits a URL that focuses
    /// the parent in the fragment and names the actual target in
    /// `?focusedItem=…`. The pre-fix parser hex-filtered the whole
    /// string and so produced a long garbage hash; this branch extracts
    /// the focusedItem value and ignores everything else.
    #[test]
    fn focused_item_query_param_wins_over_fragment() {
        let url = "https://workflowy.com/#/aaaaaaaaaaaa?focusedItem=c4ae1944b67e";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn focused_item_query_param_alone_resolves() {
        let url = "https://workflowy.com/?focusedItem=c4ae1944b67e";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn focused_item_query_with_extra_params_resolves() {
        let url = "https://workflowy.com/#/parent?other=foo&focusedItem=c4ae1944b67e&trailing=bar";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn shared_url_with_slug_extracts_trailing_segment() {
        let url = "https://workflowy.com/s/project-plan/c4ae1944b67e";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn shared_url_with_slug_and_trailing_query_resolves() {
        let url = "https://workflowy.com/s/project-plan/c4ae1944b67e?token=xyz";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn hyphenated_uuid_in_url_fragment_resolves() {
        let url = "https://workflowy.com/#/550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("550e8400e29b41d4a716446655440000".to_string()),
        );
    }

    #[test]
    fn whitespace_around_input_trims() {
        assert_eq!(
            extract_workflowy_short_hash("  c4ae1944b67e  \n"),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn empty_input_returns_none() {
        assert_eq!(extract_workflowy_short_hash(""), None);
        assert_eq!(extract_workflowy_short_hash("   "), None);
    }

    #[test]
    fn non_hex_input_returns_none() {
        assert_eq!(extract_workflowy_short_hash("not-a-real-link"), None);
    }

    #[test]
    fn wrong_length_hex_returns_none() {
        // 10 chars, all hex — not a valid hash form.
        assert_eq!(extract_workflowy_short_hash("abcdef0123"), None);
        // 13 chars — also invalid.
        assert_eq!(extract_workflowy_short_hash("abcdef012345f"), None);
    }

    /// Regression for the pre-fix bug: a URL with extra hex in the
    /// path / query must not concatenate all hex characters into one
    /// long string and pretend it's a hash.
    #[test]
    fn extra_hex_in_query_does_not_pollute_result() {
        let url = "https://workflowy.com/#/c4ae1944b67e?token=deadbeef1234";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("c4ae1944b67e".to_string()),
        );
    }

    #[test]
    fn focused_item_with_hyphenated_uuid_resolves() {
        let url = "https://workflowy.com/#/parent?focusedItem=550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("550e8400e29b41d4a716446655440000".to_string()),
        );
    }

    /// `extract_query_param_value` matches the key case-insensitively so
    /// pasted-from-chat URLs (which sometimes lowercase the key) still
    /// resolve.
    #[test]
    fn focused_item_query_key_case_insensitive() {
        let url = "https://workflowy.com/#/parent?focuseditem=c4ae1944b67e";
        assert_eq!(
            extract_workflowy_short_hash(url),
            Some("c4ae1944b67e".to_string()),
        );
    }
}
