//! HTML stripping for node text.
//!
//! Workflowy node names occasionally carry inline HTML (`<b>`, `<i>`, link
//! markup). Several surfaces need the plain-text form: `node_at_path`
//! matching, `path_of` rendering, and the `resolve_link` hit payload. This
//! is the single canonical stripper so those surfaces cannot disagree on
//! edge cases (an unterminated `<` used to be dropped-to-end by the
//! `resolve_link` char-scanner but left intact by the server regex).

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref HTML_TAG_RE: Regex = Regex::new(r"<[^>]+>").expect("static HTML-tag regex is valid");
    /// `<a ... href="URL" ...>TEXT</a>`, case-insensitive, non-greedy inner.
    static ref ANCHOR_RE: Regex = Regex::new(r#"(?is)<a\b[^>]*\bhref="([^"]*)"[^>]*>(.*?)</a>"#)
        .expect("static anchor regex is valid");
}

/// Strip `<...>` tag runs from `s`, returning the plain text. A `<` with no
/// closing `>` is left intact (the regex requires a terminating `>`), so
/// `Cost < 5` round-trips unchanged. Entities (`&amp;` etc.) are not decoded
/// — callers that need that do it separately.
///
/// Text-only by design: a `<a href="u">t</a>` collapses to `t`, dropping the
/// URL. Path-segment EXACT matching (`node_at_path`, `path_of`) depends on
/// that — a user typing the visible label must match. Display surfaces that
/// want the URL preserved use [`render_display`] instead.
pub fn strip_html(s: &str) -> String {
    HTML_TAG_RE.replace_all(s, "").to_string()
}

/// Render node HTML to human display text. Since 2026.01 the Workflowy API
/// parses markdown in the `name` field on every write (default-on, no
/// opt-out), so stored names carry `<b>`/`<i>`/`<a>`/`<time>` markup —
/// ~2.7% of a production tree. This is the canonical renderer that turns
/// that back into clean text for display and for the search index's
/// matchable form:
///
/// - Inline formatting tags (`<b>`, `<i>`, `<s>`, `<code>`, `<u>`) and
///   `<time>…label…</time>` collapse to their inner text.
/// - A link `<a href="u">t</a>` renders as `t (u)` — the URL is content in a
///   knowledge base, not markup, so unlike [`strip_html`] it is kept. When
///   the link has no visible text, or the text equals the URL, just `u`.
/// - The HTML entities the API emits are decoded (`&amp;`, `&lt;`, `&gt;`,
///   `&quot;`, `&#39;`/`&apos;`, `&nbsp;`, and the non-breaking-space char).
///
/// Fast-path: a string with no `<` and no `&` is returned unchanged (the
/// 97% case), so calling this per-entry in a hot search loop is cheap.
pub fn render_display(s: &str) -> String {
    if !s.contains('<') && !s.contains('&') && !s.contains('\u{a0}') {
        return s.to_string();
    }
    // Links first, before the generic tag strip erases the href.
    let with_links = ANCHOR_RE.replace_all(s, |caps: &regex::Captures| {
        let url = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let inner = strip_html(caps.get(2).map(|m| m.as_str()).unwrap_or(""));
        let text = inner.trim();
        if text.is_empty() || text == url {
            url.to_string()
        } else {
            format!("{text} ({url})")
        }
    });
    let no_tags = strip_html(&with_links);
    decode_entities(&no_tags)
}

/// Decode the small set of HTML entities the Workflowy API emits. `&amp;` is
/// decoded LAST so a double-encoded `&amp;lt;` yields the literal `&lt;`
/// rather than `<`.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') && !s.contains('\u{a0}') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace('\u{a0}', " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_leaves_bare_lt_intact() {
        assert_eq!(strip_html("<b>Bold</b>"), "Bold");
        assert_eq!(strip_html("<a href=\"x\">link</a>"), "link");
        // Unterminated `<` is preserved — the divergence the old
        // char-scanner got wrong (it dropped everything after a bare `<`).
        assert_eq!(strip_html("Cost < 5"), "Cost < 5");
        assert_eq!(strip_html("plain"), "plain");
    }

    #[test]
    fn render_display_reconstructs_clean_text_from_api_html() {
        // Plain text is untouched (fast-path).
        assert_eq!(render_display("plain text"), "plain text");
        // Inline formatting collapses to inner text.
        assert_eq!(
            render_display("<b>Distribution Patterns:</b> and <i>more</i>"),
            "Distribution Patterns: and more"
        );
        // A link keeps its URL — the target is content, not markup.
        assert_eq!(
            render_display(r#"<a href="https://x.com/p">Foo</a>"#),
            "Foo (https://x.com/p)"
        );
        // A link with no visible text (or text == url) renders as the URL.
        assert_eq!(
            render_display(r#"<a href="https://x.com">https://x.com</a>"#),
            "https://x.com"
        );
        // <time> unwraps to its human label (the exact API shape from the
        // live test: `[2026-08-01]` -> a <time> element).
        assert_eq!(
            render_display(
                r#"Due <time startYear="2026" startMonth="8" startDay="1">Sat, Aug 1, 2026</time> ok"#
            ),
            "Due Sat, Aug 1, 2026 ok"
        );
        // Entities decode; &amp; last so double-encoding is preserved.
        assert_eq!(render_display("A &amp; B &gt; C"), "A & B > C");
        assert_eq!(render_display("&amp;lt;"), "&lt;");
        // Non-breaking space (the API emits \xa0) normalises to a space.
        assert_eq!(render_display("a\u{a0}b"), "a b");
        // A bare `<` with no closing `>` is preserved, like strip_html.
        assert_eq!(render_display("Cost < 5"), "Cost < 5");
    }
}
