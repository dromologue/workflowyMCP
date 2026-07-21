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
///
/// `Deserialize` is a hand-written impl rather than `#[derive]` so the
/// host-encoded literal strings `"null"` / `"undefined"` reject up-front
/// at the parameter boundary instead of routing as opaque IDs that the
/// API layer fails on later. Surfaced 2026-05-09 by a Claude Desktop
/// session that observed `parent_id="null"` (string, not JSON `null`)
/// landing at contextually-derived destinations across three calls in a
/// row — the symptom of a host serialiser that emits `"null"` for what
/// should have been an explicit UUID. The wire-level fix is to refuse
/// the literal at the deserialiser, with a path-aware message naming
/// the field, so the model self-corrects on retry.
///
/// Empty string is preserved (some handlers special-case `""` as the
/// workspace-root sentinel — see `list_children`'s `None | Some("")`
/// pattern). Whitespace-only is rejected because no real UUID is
/// whitespace, and silent acceptance hides quoting bugs.
///
/// Pinned by the regression tests `null_required_uuid_field_error_names_the_field`
/// (JSON-null) and `literal_null_string_in_required_uuid_field_error_names_the_field`
/// (string-"null") plus the `tests::node_id_*` family below.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, Default)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let trimmed = s.trim();
        // Reject host-encoded JS literals that surfaced as silently
        // routed UUIDs in the 2026-05-09 incident. The skill's UUID
        // Parameter Discipline tells the assistant never to emit the
        // literal four-char string "null"; the server now enforces it.
        if trimmed.eq_ignore_ascii_case("null") || trimmed.eq_ignore_ascii_case("undefined") {
            return Err(serde::de::Error::custom(format!(
                "literal \"{}\" is not a valid UUID; supply an explicit UUID or omit the field",
                trimmed
            )));
        }
        // Reject whitespace-only payloads. An empty string ("") is left
        // alone because some handlers (e.g. list_children) treat it as
        // the workspace-root sentinel. Whitespace, by contrast, is never
        // a real UUID and silently accepting it hides host-side quoting
        // bugs that should be caught at the deserialiser.
        if !s.is_empty() && trimmed.is_empty() {
            return Err(serde::de::Error::custom(
                "whitespace-only string is not a valid UUID; pass an explicit UUID, an empty string for workspace root, or omit the field",
            ));
        }
        Ok(NodeId(s))
    }
}

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
    // Absent-`None` fields and the empty `children` list are skipped on
    // serialisation (2026-07-21): a 10 000-node `get_subtree` used to emit
    // `"color": null, "tags": null, …` for every node — the bulk of a large
    // walk payload was literal nulls, paid for in tokens and transport on
    // every big read. Absent-vs-null is semantically identical to every
    // known consumer, and the read path is unaffected (`Option` + `default`
    // deserialise absent fields fine).
    /// Maps from API field "note"
    #[serde(alias = "note", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Maps from API field "modifiedAt"
    #[serde(alias = "modifiedAt", skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified_user_id: Option<String>,
    /// Maps from API field "completedAt"
    #[serde(alias = "completedAt", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    #[serde(default)]
    pub shared: bool,
    /// Maps from API field "completed"
    #[serde(default)]
    pub completed: bool,
    /// Maps from API field "createdAt"
    #[serde(default, alias = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// Nested data object from API (contains layoutMode etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<NodeData>,
    /// Maps from API field "priority"
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

    /// Pinning the 2026-05-09 fix: literal `"null"` (JS string, not JSON null)
    /// is rejected at the deserialiser. Surfaced when a Claude Desktop
    /// session emitted `parent_id="null"` for `create_node` and `create_mirror`
    /// across three calls; the server silently routed those to contextual
    /// destinations rather than failing loudly. Refusing the literal here
    /// means the failure is observable at the wire and the host can
    /// self-correct on retry. Case-insensitive because hosts also emit
    /// `"NULL"` and `"Null"`; whitespace-trimmed for the same reason.
    #[test]
    fn test_node_id_rejects_literal_null_string() {
        for variant in ["null", "NULL", "Null", "  null  "] {
            let result: serde_json::Result<NodeId> =
                serde_json::from_value(json!(variant));
            let err = result.expect_err(&format!(
                "variant {:?} must reject — literal `null` is not a UUID",
                variant
            ));
            let msg = err.to_string();
            assert!(
                msg.contains("not a valid UUID"),
                "error must explain the constraint, got: {}",
                msg
            );
        }
    }

    /// Symmetric pin: literal `"undefined"` (JS host emits this when a
    /// reactive UUID binding is unresolved) is rejected the same way.
    #[test]
    fn test_node_id_rejects_literal_undefined_string() {
        let result: serde_json::Result<NodeId> = serde_json::from_value(json!("undefined"));
        let err = result.expect_err("literal `undefined` must reject");
        assert!(err.to_string().contains("not a valid UUID"));
    }

    /// Empty string is preserved — handlers like `list_children` use
    /// it as the workspace-root sentinel via `None | Some("")`. The
    /// deserialiser must NOT reject empty; the rejection lives at the
    /// handler when the operation requires a real ID.
    #[test]
    fn test_node_id_accepts_empty_string() {
        let id: NodeId = serde_json::from_value(json!("")).expect("empty string must deserialize");
        assert_eq!(id.as_str(), "");
    }

    /// Whitespace-only input is rejected. No real UUID is whitespace,
    /// and silent acceptance hides host-side quoting bugs.
    #[test]
    fn test_node_id_rejects_whitespace_only() {
        for variant in ["   ", "\t", "\n"] {
            let result: serde_json::Result<NodeId> =
                serde_json::from_value(json!(variant));
            assert!(
                result.is_err(),
                "whitespace-only {:?} must reject",
                variant
            );
        }
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

    /// Serialised nodes omit absent optionals and the empty children list
    /// (2026-07-21): on a 10k-node walk the nulls were the bulk of the
    /// payload. Present values must still serialise, and a trimmed
    /// payload must round-trip losslessly.
    #[test]
    fn serialised_node_omits_none_fields_and_empty_children() {
        let sparse = WorkflowyNode {
            id: "n1".to_string(),
            name: "Sparse".to_string(),
            ..Default::default()
        };
        let v = serde_json::to_value(&sparse).unwrap();
        let obj = v.as_object().unwrap();
        for absent in [
            "description", "parent_id", "last_modified", "last_modified_user_id",
            "completed_at", "layout_mode", "color", "tags", "assignee",
            "children", "created_at", "data", "priority",
        ] {
            assert!(!obj.contains_key(absent), "{} must be omitted when absent", absent);
        }
        assert_eq!(obj["id"], "n1");

        let full = WorkflowyNode {
            id: "n2".to_string(),
            name: "Full".to_string(),
            description: Some("note".to_string()),
            parent_id: Some("p".to_string()),
            priority: Some(3),
            children: vec!["c1".to_string()],
            ..Default::default()
        };
        let v = serde_json::to_value(&full).unwrap();
        assert_eq!(v["description"], "note");
        assert_eq!(v["parent_id"], "p");
        assert_eq!(v["priority"], 3);
        assert_eq!(v["children"][0], "c1");

        let back: WorkflowyNode = serde_json::from_value(serde_json::to_value(&sparse).unwrap()).unwrap();
        assert_eq!(back.id, sparse.id);
        assert_eq!(back.description, None);
        assert!(back.children.is_empty());
    }
}
