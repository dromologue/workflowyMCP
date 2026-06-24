//! Tag and assignee extraction from node text.
//! Parses #tags and @mentions.

use regex::Regex;
use lazy_static::lazy_static;
use std::collections::HashSet;

use crate::types::WorkflowyNode;

lazy_static! {
    static ref TAG_RE: Regex = Regex::new(r"#([\w-]+)").unwrap();
    static ref ASSIGNEE_RE: Regex = Regex::new(r"@([\w-]+)").unwrap();
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedTags {
    pub tags: Vec<String>,
    pub assignees: Vec<String>,
}

/// Parse #tags and @mentions from text. Returns lowercased, deduplicated results.
pub fn parse_tags(text: &str) -> ParsedTags {
    let mut tag_set = HashSet::new();
    let mut assignee_set = HashSet::new();

    for cap in TAG_RE.captures_iter(text) {
        // Skip #due-YYYY-MM-DD patterns (those are dates, not tags)
        let tag = cap[1].to_lowercase();
        if !tag.starts_with("due-") || tag.len() != 14 {
            tag_set.insert(tag);
        }
    }

    for cap in ASSIGNEE_RE.captures_iter(text) {
        assignee_set.insert(cap[1].to_lowercase());
    }

    let mut tags: Vec<String> = tag_set.into_iter().collect();
    let mut assignees: Vec<String> = assignee_set.into_iter().collect();
    tags.sort();
    assignees.sort();

    ParsedTags { tags, assignees }
}

/// Whole-tag idempotency check: does `text` already carry `tag` as a complete
/// tag (case-insensitive, leading `#` optional)?
///
/// Routes through `parse_tags` rather than a bare `name.contains("#tag")` so a
/// shorter pillar tag is not silently shadowed by a longer existing tag —
/// `text_contains_tag("#leadership", "lead")` returns false. The substring
/// shape was the 2026-05-24 `bulk_tag` shadowing bug.
pub fn text_contains_tag(text: &str, tag: &str) -> bool {
    let needle = tag.trim_start_matches('#').to_lowercase();
    if needle.is_empty() {
        return false;
    }
    parse_tags(text).tags.iter().any(|t| t == &needle)
}

/// Append `tag` (with or without a leading `#`) to `name` as a whole tag,
/// returning the new name — or `None` when `name` already carries the tag
/// (whole-tag, case-insensitive). The `None`-on-present return encodes
/// idempotency so every caller skips the write uniformly.
///
/// Single source of truth for "add this tag to a node name", shared by the
/// `bulk_tag` tool (MCP handler + `wflow-do bulk-tag` CLI) and
/// `workflows::apply_bulk_op(AddTag)`. Pre-2026-06-16 the CLI re-implemented
/// the append without the whole-tag idempotency check and double-tagged on
/// re-runs; routing all three sites here closes that drift by construction.
pub fn add_tag_to_name(name: &str, tag: &str) -> Option<String> {
    let bare = tag.trim_start_matches('#');
    if bare.is_empty() || text_contains_tag(name, bare) {
        return None;
    }
    Some(format!("{} #{}", name.trim_end(), bare))
}

/// Compile the whole-tag strip pattern for `tag`. Returns `None` for an empty
/// tag (nothing to strip). Whole-tag boundary via `\b`/`$` so removing `#lead`
/// does not touch `#leadership`. Exposed so a bulk caller can compile the
/// pattern ONCE and reuse it across many nodes (the `tag` is fixed for the
/// whole bulk operation) rather than recompiling it per node — see
/// `workflows::apply_bulk_op(RemoveTag)`. Single source of the pattern string,
/// so the per-call and per-node paths cannot drift.
pub fn compile_tag_strip_regex(tag: &str) -> Option<Regex> {
    let bare = tag.trim_start_matches('#');
    if bare.is_empty() {
        return None;
    }
    Some(
        Regex::new(&format!(r"\s*#{}(?:\b|$)", regex::escape(bare)))
            .expect("escaped pattern is always valid regex"),
    )
}

/// Strip every whole-tag occurrence matched by `re` (built via
/// [`compile_tag_strip_regex`]) from `name`.
pub fn strip_tag_with_regex(re: &Regex, name: &str) -> String {
    re.replace_all(name, "").to_string()
}

/// Remove every whole-tag occurrence of `tag` from `name`, returning the new
/// name (unchanged if the tag is absent). Whole-tag boundary via `\b`/`$` so
/// removing `#lead` does not touch `#leadership`. Shared by
/// `workflows::apply_bulk_op(RemoveTag)`. For a bulk loop over many nodes,
/// prefer [`compile_tag_strip_regex`] + [`strip_tag_with_regex`] to compile
/// the pattern once.
pub fn remove_tag_from_name(name: &str, tag: &str) -> String {
    match compile_tag_strip_regex(tag) {
        None => name.to_string(),
        Some(re) => strip_tag_with_regex(&re, name),
    }
}

/// Whole-tag predicate over a node: does the node carry `needle` as a
/// complete tag (case-insensitive, leading `#`/`@` optional)?
///
/// Routes through `parse_node_tags` (name + description) so the match
/// is whole-tag, not the buggy substring scan the MCP `tag_search` and
/// `find_by_tag_and_path` handlers previously used — `#lead` must not
/// match `#leadership`. When `needle` starts with `@` the assignee list
/// is checked; otherwise the tag list. A bare needle (no sigil) checks
/// the tag list, mirroring how callers pass `#tag` and `@person`.
///
/// Single source of truth shared by the `tag_search` /
/// `find_by_tag_and_path` MCP handlers and the matching `wflow-do`
/// subcommands so the predicate cannot drift between surfaces.
pub fn node_has_tag(node: &WorkflowyNode, needle: &str) -> bool {
    let is_assignee = needle.starts_with('@');
    let bare = needle
        .trim_start_matches('#')
        .trim_start_matches('@')
        .to_lowercase();
    if bare.is_empty() {
        return false;
    }
    let parsed = parse_node_tags(node);
    if is_assignee {
        parsed.assignees.iter().any(|a| a == &bare)
    } else {
        parsed.tags.iter().any(|t| t == &bare)
    }
}

/// Parse tags from a node's name and description combined.
pub fn parse_node_tags(node: &WorkflowyNode) -> ParsedTags {
    let mut combined = parse_tags(&node.name);
    if let Some(desc) = &node.description {
        let desc_tags = parse_tags(desc);
        // Merge and dedup
        let mut tag_set: HashSet<String> = combined.tags.into_iter().collect();
        tag_set.extend(desc_tags.tags);
        let mut assignee_set: HashSet<String> = combined.assignees.into_iter().collect();
        assignee_set.extend(desc_tags.assignees);

        let mut tags: Vec<String> = tag_set.into_iter().collect();
        let mut assignees: Vec<String> = assignee_set.into_iter().collect();
        tags.sort();
        assignees.sort();
        combined = ParsedTags { tags, assignees };
    }
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_tag() {
        let result = parse_tags("Task #urgent");
        assert_eq!(result.tags, vec!["urgent"]);
        assert!(result.assignees.is_empty());
    }

    #[test]
    fn test_parse_multiple_tags() {
        let result = parse_tags("Task #urgent #review #inbox");
        assert_eq!(result.tags, vec!["inbox", "review", "urgent"]);
    }

    #[test]
    fn test_parse_assignee() {
        let result = parse_tags("Task @alice");
        assert!(result.tags.is_empty());
        assert_eq!(result.assignees, vec!["alice"]);
    }

    #[test]
    fn test_parse_mixed() {
        let result = parse_tags("#project @bob review #urgent");
        assert_eq!(result.tags, vec!["project", "urgent"]);
        assert_eq!(result.assignees, vec!["bob"]);
    }

    #[test]
    fn test_dedup() {
        let result = parse_tags("#urgent #URGENT #Urgent");
        assert_eq!(result.tags, vec!["urgent"]);
    }

    #[test]
    fn test_no_tags() {
        let result = parse_tags("Plain text without tags");
        assert!(result.tags.is_empty());
        assert!(result.assignees.is_empty());
    }

    #[test]
    fn test_due_date_tag_excluded() {
        let result = parse_tags("Task #due-2026-03-15 #urgent");
        assert_eq!(result.tags, vec!["urgent"]);
    }

    #[test]
    fn test_hyphenated_tags() {
        let result = parse_tags("#follow-up @team-lead");
        assert_eq!(result.tags, vec!["follow-up"]);
        assert_eq!(result.assignees, vec!["team-lead"]);
    }

    #[test]
    fn text_contains_tag_matches_whole_tag_not_substring() {
        // Shadow check: shorter tag must not match longer existing tag.
        assert!(!text_contains_tag("#leadership notes", "lead"));
        assert!(!text_contains_tag("Working on #learning", "learn"));
        assert!(!text_contains_tag("#transformation work", "transform"));
        // Whole-tag presence is detected (leading # optional).
        assert!(text_contains_tag("Note about #lead and follow", "lead"));
        assert!(text_contains_tag("Note about #lead and follow", "#lead"));
        // Case-insensitive match.
        assert!(text_contains_tag("Note about #Lead", "lead"));
        // Hyphen is a tag char, so `#lead` does not match `#lead-time`.
        assert!(!text_contains_tag("Working on #lead-time", "lead"));
        // Empty / missing tag.
        assert!(!text_contains_tag("anything", ""));
        assert!(!text_contains_tag("anything", "#"));
        // Absent tag.
        assert!(!text_contains_tag("just text", "anything"));
    }

    #[test]
    fn test_parse_node_tags_combined() {
        let node = WorkflowyNode {
            id: "n1".into(),
            name: "#project @alice".into(),
            description: Some("#review @bob".into()),
            ..Default::default()
        };
        let result = parse_node_tags(&node);
        assert_eq!(result.tags, vec!["project", "review"]);
        assert_eq!(result.assignees, vec!["alice", "bob"]);
    }

    #[test]
    fn node_has_tag_matches_whole_tag_not_substring() {
        let node = WorkflowyNode {
            id: "n1".into(),
            name: "Notes on #leadership and #lead".into(),
            description: Some("assigned @team-lead".into()),
            ..Default::default()
        };
        // Whole-tag presence (leading # optional).
        assert!(node_has_tag(&node, "lead"));
        assert!(node_has_tag(&node, "#lead"));
        assert!(node_has_tag(&node, "leadership"));
        // Case-insensitive.
        assert!(node_has_tag(&node, "#LEAD"));
        // Assignee via leading @.
        assert!(node_has_tag(&node, "@team-lead"));
        // A tag-form needle must not match an assignee, and vice versa.
        assert!(!node_has_tag(&node, "team-lead"));
        assert!(!node_has_tag(&node, "@lead"));
        // Empty / sigil-only needle.
        assert!(!node_has_tag(&node, ""));
        assert!(!node_has_tag(&node, "#"));
        // Absent tag.
        assert!(!node_has_tag(&node, "transform"));

        // A node carrying only the longer tag must NOT match the shorter.
        let only_long = WorkflowyNode {
            id: "n2".into(),
            name: "#leadership only".into(),
            ..Default::default()
        };
        assert!(!node_has_tag(&only_long, "lead"));
        assert!(node_has_tag(&only_long, "leadership"));
    }

    #[test]
    fn test_parse_node_tags_dedup_across_fields() {
        let node = WorkflowyNode {
            id: "n1".into(),
            name: "#urgent @alice".into(),
            description: Some("#urgent @alice more text".into()),
            ..Default::default()
        };
        let result = parse_node_tags(&node);
        assert_eq!(result.tags, vec!["urgent"]);
        assert_eq!(result.assignees, vec!["alice"]);
    }
}
