//! Subtree collection, todo detection, and subtree-shape renderers
//! shared between the MCP `export_subtree` handler and the `wflow-do
//! export` CLI subcommand. The renderers are pure functions over a
//! `&[WorkflowyNode]` slice so both surfaces call the same code (the
//! 2026-05-09 duplication audit caught two byte-identical copies in
//! `server/mod.rs` and `bin/wflow_do.rs`).

use std::collections::HashMap;
use crate::types::WorkflowyNode;

/// Render a subtree as nested Markdown bullets. Depth is determined by
/// following parent_id chains within the supplied node set, so the
/// output mirrors the actual tree shape regardless of the order
/// `nodes` was returned in.
pub fn render_subtree_markdown(nodes: &[WorkflowyNode], root_id: &str) -> String {
    let mut children_of: HashMap<String, Vec<&WorkflowyNode>> = HashMap::new();
    for n in nodes {
        if let Some(pid) = &n.parent_id {
            children_of.entry(pid.clone()).or_default().push(n);
        }
    }
    let mut out = String::new();
    fn walk(
        node: &WorkflowyNode,
        depth: usize,
        children_of: &HashMap<String, Vec<&WorkflowyNode>>,
        out: &mut String,
    ) {
        let indent = "  ".repeat(depth);
        out.push_str(&format!("{}- {}\n", indent, node.name));
        if let Some(desc) = &node.description {
            for line in desc.lines() {
                out.push_str(&format!("{}    {}\n", indent, line));
            }
        }
        if let Some(children) = children_of.get(&node.id) {
            for child in children {
                walk(child, depth + 1, children_of, out);
            }
        }
    }
    if let Some(root) = nodes.iter().find(|n| n.id == root_id) {
        walk(root, 0, &children_of, &mut out);
    }
    out
}

/// Render a subtree as OPML — Workflowy and other outliners can
/// re-import this losslessly enough for backup/exchange. We escape the
/// four XML metacharacters and emit each node as a single-line
/// `<outline>` element.
pub fn render_subtree_opml(nodes: &[WorkflowyNode], root_id: &str) -> String {
    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
    let mut children_of: HashMap<String, Vec<&WorkflowyNode>> = HashMap::new();
    for n in nodes {
        if let Some(pid) = &n.parent_id {
            children_of.entry(pid.clone()).or_default().push(n);
        }
    }
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n  <body>\n");
    fn walk(
        node: &WorkflowyNode,
        depth: usize,
        children_of: &HashMap<String, Vec<&WorkflowyNode>>,
        out: &mut String,
    ) {
        let indent = "  ".repeat(depth + 2);
        let name = xml_escape(&node.name);
        let desc = node
            .description
            .as_deref()
            .map(xml_escape)
            .unwrap_or_default();
        let descendants = children_of.get(&node.id);
        let self_closing = descendants.is_none() && desc.is_empty();
        if self_closing {
            out.push_str(&format!("{}<outline text=\"{}\"/>\n", indent, name));
        } else {
            if desc.is_empty() {
                out.push_str(&format!("{}<outline text=\"{}\">\n", indent, name));
            } else {
                out.push_str(&format!("{}<outline text=\"{}\" _note=\"{}\">\n", indent, name, desc));
            }
            if let Some(children) = descendants {
                for child in children {
                    walk(child, depth + 1, children_of, out);
                }
            }
            out.push_str(&format!("{}</outline>\n", indent));
        }
    }
    if let Some(root) = nodes.iter().find(|n| n.id == root_id) {
        walk(root, 0, &children_of, &mut out);
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

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
            parent_id: parent_id.map(String::from),
            ..Default::default()
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
