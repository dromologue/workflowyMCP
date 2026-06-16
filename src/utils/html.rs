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
}

/// Strip `<...>` tag runs from `s`, returning the plain text. A `<` with no
/// closing `>` is left intact (the regex requires a terminating `>`), so
/// `Cost < 5` round-trips unchanged. Entities (`&amp;` etc.) are not decoded
/// — callers that need that do it separately.
pub fn strip_html(s: &str) -> String {
    HTML_TAG_RE.replace_all(s, "").to_string()
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
}
