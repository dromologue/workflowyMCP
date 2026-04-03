/// Core types for Workflowy data structures
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowyNode {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub parent_id: Option<String>,
    pub last_modified: Option<i64>,
    pub last_modified_user_id: Option<String>,
    pub completed_at: Option<i64>,
    pub layout_mode: Option<String>,
    pub color: Option<String>,
    pub tags: Option<Vec<String>>,
    pub assignee: Option<String>,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub shared: bool,
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
pub struct ConceptMapResult {
    pub html: String,
    pub node_count: usize,
}

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
