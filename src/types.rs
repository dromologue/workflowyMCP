/// Core types for Workflowy data structures
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeData {
    #[serde(alias = "layoutMode")]
    pub layout_mode: Option<String>,
}

/// Newtype wrapper for Workflowy node IDs (UUIDs).
/// Provides type safety to prevent mixing node IDs with arbitrary strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    /// Create a NodeId from a string without validation (for internal/trusted use).
    pub fn new_unchecked(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Get the inner string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for NodeId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for NodeId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for NodeId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl PartialEq<NodeId> for String {
    fn eq(&self, other: &NodeId) -> bool {
        self.as_str() == other.0.as_str()
    }
}

impl PartialEq<String> for NodeId {
    fn eq(&self, other: &String) -> bool {
        self.0.as_str() == other.as_str()
    }
}

impl PartialEq<&str> for NodeId {
    fn eq(&self, other: &&str) -> bool {
        self.0.as_str() == *other
    }
}

impl schemars::JsonSchema for NodeId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("NodeId")
    }

    fn json_schema(gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Render as a plain string in JSON Schema so MCP clients see a string field
        String::json_schema(gen)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowyNode {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Maps from API field "note"
    #[serde(alias = "note")]
    pub description: Option<String>,
    pub parent_id: Option<String>,
    /// Maps from API field "modifiedAt"
    #[serde(alias = "modifiedAt")]
    pub last_modified: Option<i64>,
    pub last_modified_user_id: Option<String>,
    /// Maps from API field "completedAt"
    #[serde(alias = "completedAt")]
    pub completed_at: Option<i64>,
    pub layout_mode: Option<String>,
    pub color: Option<String>,
    pub tags: Option<Vec<String>>,
    pub assignee: Option<String>,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub shared: bool,
    /// Maps from API field "completed"
    #[serde(default)]
    pub completed: bool,
    /// Maps from API field "createdAt"
    #[serde(default, alias = "createdAt")]
    pub created_at: Option<i64>,
    /// Nested data object from API (contains layoutMode etc.)
    #[serde(default)]
    pub data: Option<NodeData>,
    /// Maps from API field "priority"
    #[serde(default)]
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeWithPath {
    pub node: WorkflowyNode,
    pub path: Vec<String>,
    pub level: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedNode {
    pub id: String,
    pub name: String,
    pub relevance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedNode {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisContentNode {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisContentResult {
    pub nodes: Vec<AnalysisContentNode>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowyApiResponse {
    pub nodes: Option<Vec<WorkflowyNode>>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchOperationResult {
    pub nodes: Vec<CreatedNode>,
    pub errors: Vec<String>,
}

/// Types for request/response handling
#[derive(Debug, Clone)]
pub struct TaskMapResult {
    pub html: String,
    pub task_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeType {
    Subtree,
    Children,
    Self_,
}

impl std::str::FromStr for ScopeType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "subtree" => Ok(ScopeType::Subtree),
            "children" => Ok(ScopeType::Children),
            "self" => Ok(ScopeType::Self_),
            _ => Err(format!("Invalid scope: {}", s)),
        }
    }
}

/// Cache entry wrapper
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub node: WorkflowyNode,
    pub timestamp: std::time::SystemTime,
}

/// Parent-to-children mapping for O(n) cache invalidation
pub type ChildrenIndex = HashMap<String, Vec<String>>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- NodeId tests ---

    #[test]
    fn test_node_id_from_string() {
        let id = NodeId::from("abc-123".to_string());
        assert_eq!(id.as_str(), "abc-123");
    }

    #[test]
    fn test_node_id_from_str() {
        let id = NodeId::from("abc-123");
        assert_eq!(id.as_str(), "abc-123");
    }

    #[test]
    fn test_node_id_deref() {
        let id = NodeId::from("abc-123");
        // Should be usable wherever &str is expected
        assert!(id.contains("abc"));
        assert_eq!(&*id, "abc-123");
    }

    #[test]
    fn test_node_id_display() {
        let id = NodeId::from("abc-123");
        assert_eq!(format!("{}", id), "abc-123");
    }

    #[test]
    fn test_node_id_partial_eq_string() {
        let id = NodeId::from("abc-123");
        let s = "abc-123".to_string();
        assert_eq!(id, s);
        assert_eq!(s, id);
    }

    #[test]
    fn test_node_id_partial_eq_str() {
        let id = NodeId::from("abc-123");
        assert_eq!(id, "abc-123");
    }

    #[test]
    fn test_node_id_serde_transparent() {
        // Serialize: NodeId should produce a plain JSON string
        let id = NodeId::from("abc-123");
        let json = serde_json::to_value(&id).unwrap();
        assert_eq!(json, json!("abc-123"));

        // Deserialize: plain JSON string should produce NodeId
        let id2: NodeId = serde_json::from_value(json!("def-456")).unwrap();
        assert_eq!(id2.as_str(), "def-456");
    }

    #[test]
    fn test_node_id_default() {
        let id = NodeId::default();
        assert_eq!(id.as_str(), "");
    }

    // --- WorkflowyNode API deserialization tests ---

    #[test]
    fn test_workflowy_node_deserialize_api_format() {
        // Simulates the actual Workflowy API response format
        let api_json = json!({
            "id": "c1ef1ad5-ce38-8fed-bf6f-4737f286b86a",
            "name": "Tasks",
            "note": "Some description",
            "parent_id": null,
            "priority": 5900,
            "completed": false,
            "data": { "layoutMode": "h2" },
            "createdAt": 1765373200,
            "modifiedAt": 1772305834,
            "completedAt": null
        });

        let node: WorkflowyNode = serde_json::from_value(api_json).unwrap();
        assert_eq!(node.id, "c1ef1ad5-ce38-8fed-bf6f-4737f286b86a");
        assert_eq!(node.name, "Tasks");
        // "note" maps to "description" via serde alias
        assert_eq!(node.description.as_deref(), Some("Some description"));
        assert_eq!(node.parent_id, None);
        // "modifiedAt" maps to "last_modified" via serde alias
        assert_eq!(node.last_modified, Some(1772305834));
        // "createdAt" maps to "created_at" via serde alias
        assert_eq!(node.created_at, Some(1765373200));
        assert!(!node.completed);
        assert_eq!(node.priority, Some(5900));
        // Nested data
        let data = node.data.unwrap();
        assert_eq!(data.layout_mode.as_deref(), Some("h2"));
    }

    #[test]
    fn test_workflowy_node_deserialize_minimal() {
        // Minimal response — only required fields
        let api_json = json!({
            "id": "abc",
            "name": "Test"
        });

        let node: WorkflowyNode = serde_json::from_value(api_json).unwrap();
        assert_eq!(node.id, "abc");
        assert_eq!(node.name, "Test");
        assert_eq!(node.description, None);
        assert_eq!(node.last_modified, None);
        assert!(!node.completed);
    }

    #[test]
    fn test_workflowy_node_completed_at_alias() {
        let api_json = json!({
            "id": "abc",
            "name": "Done",
            "completed": true,
            "completedAt": 1700000000
        });

        let node: WorkflowyNode = serde_json::from_value(api_json).unwrap();
        assert!(node.completed);
        assert_eq!(node.completed_at, Some(1700000000));
    }
}
