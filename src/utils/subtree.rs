/// Subtree collection and todo detection utilities

use std::collections::HashMap;
use crate::types::WorkflowyNode;

/// Collect all nodes in a subtree (root + all descendants).
pub fn get_subtree_nodes<'a>(root_id: &str, nodes: &'a [WorkflowyNode]) -> Vec<&'a WorkflowyNode> {
    // Build parent -> children index
    let mut children_map: HashMap<&str, Vec<&WorkflowyNode>> = HashMap::new();
    let mut node_by_id: HashMap<&str, &WorkflowyNode> = HashMap::new();

    for node in nodes {
        node_by_id.insert(&node.id, node);
        if let Some(pid) = &node.parent_id {
            children_map.entry(pid.as_str()).or_default().push(node);
        }
    }

    let mut result = Vec::new();
    let mut stack = vec![root_id];

    while let Some(id) = stack.pop() {
        if let Some(node) = node_by_id.get(id) {
            result.push(*node);
        }
        if let Some(children) = children_map.get(id) {
            for child in children {
                stack.push(&child.id);
            }
        }
    }

    result
}

/// Check if a node is a todo item.
/// Checks layout_mode == "todo" or name starts with [ ] or [x].
pub fn is_todo(node: &WorkflowyNode) -> bool {
    if let Some(mode) = &node.layout_mode {
        if mode == "todo" {
            return true;
        }
    }
    let name = node.name.trim_start();
    name.starts_with("[ ]") || name.starts_with("[x]") || name.starts_with("[X]")
}

/// Check if a node is completed.
pub fn is_completed(node: &WorkflowyNode) -> bool {
    node.completed_at.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, name: &str, parent_id: Option<&str>) -> WorkflowyNode {
        WorkflowyNode {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            parent_id: parent_id.map(String::from),
            last_modified: None,
            last_modified_user_id: None,
            completed_at: None,
            layout_mode: None,
            color: None,
            tags: None,
            assignee: None,
            children: vec![],
            shared: false,
        }
    }

    #[test]
    fn test_subtree_flat() {
        let nodes = vec![
            make_node("root", "Root", None),
            make_node("c1", "Child 1", Some("root")),
            make_node("c2", "Child 2", Some("root")),
        ];
        let subtree = get_subtree_nodes("root", &nodes);
        assert_eq!(subtree.len(), 3);
    }

    #[test]
    fn test_subtree_nested() {
        let nodes = vec![
            make_node("root", "Root", None),
            make_node("c1", "Child", Some("root")),
            make_node("gc1", "Grandchild", Some("c1")),
        ];
        let subtree = get_subtree_nodes("root", &nodes);
        assert_eq!(subtree.len(), 3);
        let ids: Vec<&str> = subtree.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"root"));
        assert!(ids.contains(&"c1"));
        assert!(ids.contains(&"gc1"));
    }

    #[test]
    fn test_subtree_excludes_other_trees() {
        let nodes = vec![
            make_node("root", "Root", None),
            make_node("c1", "Child", Some("root")),
            make_node("other", "Other tree", None),
            make_node("oc1", "Other child", Some("other")),
        ];
        let subtree = get_subtree_nodes("root", &nodes);
        assert_eq!(subtree.len(), 2);
    }

    #[test]
    fn test_subtree_missing_root() {
        let nodes = vec![make_node("a", "A", None)];
        let subtree = get_subtree_nodes("nonexistent", &nodes);
        assert!(subtree.is_empty());
    }

    #[test]
    fn test_is_todo_layout_mode() {
        let mut node = make_node("n1", "Task", None);
        node.layout_mode = Some("todo".to_string());
        assert!(is_todo(&node));
    }

    #[test]
    fn test_is_todo_checkbox_unchecked() {
        let node = make_node("n1", "[ ] Task", None);
        assert!(is_todo(&node));
    }

    #[test]
    fn test_is_todo_checkbox_checked() {
        let node = make_node("n1", "[x] Done task", None);
        assert!(is_todo(&node));
    }

    #[test]
    fn test_is_not_todo() {
        let node = make_node("n1", "Regular note", None);
        assert!(!is_todo(&node));
    }

    #[test]
    fn test_is_completed() {
        let mut node = make_node("n1", "Done", None);
        node.completed_at = Some(1700000000000);
        assert!(is_completed(&node));
    }

    #[test]
    fn test_is_not_completed() {
        let node = make_node("n1", "Not done", None);
        assert!(!is_completed(&node));
    }
}
