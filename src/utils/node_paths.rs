//! Path building for node display.
//! Builds hierarchical paths by following parent_id chains.

use std::collections::HashMap;
use crate::types::WorkflowyNode;

const MAX_SEGMENT_LEN: usize = 40;

/// Build a display path for a node by following parent_id links.
/// Returns a string like "Grandparent > Parent > Node".
pub fn build_node_path(node_id: &str, nodes: &[WorkflowyNode]) -> String {
    let node_map: HashMap<&str, &WorkflowyNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    build_node_path_with_map(node_id, &node_map)
}

/// Build path using a pre-built node map (more efficient for batch operations).
pub fn build_node_path_with_map(node_id: &str, node_map: &HashMap<&str, &WorkflowyNode>) -> String {
    let mut segments = Vec::new();
    let mut current_id = Some(node_id);
    let mut seen = std::collections::HashSet::new();

    while let Some(id) = current_id {
        if !seen.insert(id) {
            break; // Cycle protection
        }
        if let Some(node) = node_map.get(id) {
            let name = if node.name.is_empty() {
                "(untitled)".to_string()
            } else {
                truncate_segment(&node.name)
            };
            segments.push(name);
            current_id = node.parent_id.as_deref();
        } else {
            break;
        }
    }

    segments.reverse();
    segments.join(" > ")
}

/// Build a HashMap from a node slice for efficient lookups.
pub fn build_node_map(nodes: &[WorkflowyNode]) -> HashMap<&str, &WorkflowyNode> {
    nodes.iter().map(|n| (n.id.as_str(), n)).collect()
}

fn truncate_segment(s: &str) -> String {
    if s.len() <= MAX_SEGMENT_LEN {
        s.to_string()
    } else {
        format!("{}...", &s[..MAX_SEGMENT_LEN - 3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, name: &str, parent_id: Option<&str>) -> WorkflowyNode {
        WorkflowyNode {
            id: id.to_string(),
            name: name.to_string(),
            parent_id: parent_id.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn test_root_node_path() {
        let nodes = vec![make_node("a", "Root", None)];
        assert_eq!(build_node_path("a", &nodes), "Root");
    }

    #[test]
    fn test_nested_path() {
        let nodes = vec![
            make_node("a", "Grandparent", None),
            make_node("b", "Parent", Some("a")),
            make_node("c", "Child", Some("b")),
        ];
        assert_eq!(build_node_path("c", &nodes), "Grandparent > Parent > Child");
    }

    #[test]
    fn test_missing_parent() {
        let nodes = vec![make_node("c", "Orphan", Some("missing"))];
        assert_eq!(build_node_path("c", &nodes), "Orphan");
    }

    #[test]
    fn test_untitled_node() {
        let nodes = vec![make_node("a", "", None)];
        assert_eq!(build_node_path("a", &nodes), "(untitled)");
    }

    #[test]
    fn test_long_name_truncated() {
        let long_name = "A".repeat(50);
        let nodes = vec![make_node("a", &long_name, None)];
        let path = build_node_path("a", &nodes);
        assert!(path.len() <= MAX_SEGMENT_LEN);
        assert!(path.ends_with("..."));
    }

    #[test]
    fn test_nonexistent_node() {
        let nodes = vec![make_node("a", "Root", None)];
        assert_eq!(build_node_path("nonexistent", &nodes), "");
    }
}
