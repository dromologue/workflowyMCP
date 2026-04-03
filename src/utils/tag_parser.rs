/// Tag and assignee extraction from node text
/// Parses #tags and @mentions

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
    fn test_parse_node_tags_combined() {
        let node = WorkflowyNode {
            id: "n1".into(),
            name: "#project @alice".into(),
            description: Some("#review @bob".into()),
            parent_id: None,
            last_modified: None,
            last_modified_user_id: None,
            completed_at: None,
            layout_mode: None,
            color: None,
            tags: None,
            assignee: None,
            children: vec![],
            shared: false,
        };
        let result = parse_node_tags(&node);
        assert_eq!(result.tags, vec!["project", "review"]);
        assert_eq!(result.assignees, vec!["alice", "bob"]);
    }

    #[test]
    fn test_parse_node_tags_dedup_across_fields() {
        let node = WorkflowyNode {
            id: "n1".into(),
            name: "#urgent @alice".into(),
            description: Some("#urgent @alice more text".into()),
            parent_id: None,
            last_modified: None,
            last_modified_user_id: None,
            completed_at: None,
            layout_mode: None,
            color: None,
            tags: None,
            assignee: None,
            children: vec![],
            shared: false,
        };
        let result = parse_node_tags(&node);
        assert_eq!(result.tags, vec!["urgent"]);
        assert_eq!(result.assignees, vec!["alice"]);
    }
}
