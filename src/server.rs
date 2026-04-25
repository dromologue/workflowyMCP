//! MCP Server implementation using rmcp
//! Implements ServerHandler with tool_router for all Workflowy tools

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use chrono::{NaiveDate, Utc};
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use tracing::{error, info};

use crate::api::{FetchControls, SubtreeFetch, TruncationReason, WorkflowyClient};
use crate::defaults;
use crate::types::{WorkflowyNode, NodeId};
use crate::utils::cache::NodeCache;
use crate::utils::cancel::CancelRegistry;
use crate::utils::date_parser::{parse_due_date_from_node, is_overdue};
use crate::utils::name_index::NameIndex;
use crate::utils::node_paths::{build_node_path_with_map, build_node_map};
use crate::utils::subtree::{is_todo, is_completed};
use crate::utils::tag_parser::parse_node_tags;
use crate::validation::validate_node_id;
use std::time::{Duration, Instant};

/// Validate a node_id parameter, returning McpError on failure. The
/// underlying validator rejects the empty string, so any call where the
/// serde layer has defaulted a missing `node_id` to `""` is caught here.
fn check_node_id(id: impl AsRef<str>) -> Result<(), McpError> {
    validate_node_id(id).map_err(|e| McpError::invalid_params(e.to_string(), None))
}

/// Build a truncation warning string for text responses. Empty when the fetch
/// was complete; otherwise announces the cap and the reason so the caller
/// knows whether to narrow the scope, raise the budget, or retry.
fn truncation_banner(truncated: bool, limit: usize) -> String {
    truncation_banner_with_reason(truncated, limit, None)
}

fn truncation_banner_with_reason(
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
) -> String {
    if !truncated {
        return String::new();
    }
    match reason {
        Some(TruncationReason::Timeout) => format!(
            "⚠ subtree walk timed out before completion (budget {} ms). Results below reflect whatever was collected — retry with narrower parent_id/max_depth or raise the budget.\n\n",
            defaults::SUBTREE_FETCH_TIMEOUT_MS,
        ),
        Some(TruncationReason::Cancelled) => {
            "⚠ subtree walk was cancelled; results below are partial.\n\n".to_string()
        }
        _ => format!(
            "⚠ subtree truncated at {} nodes — results below may be incomplete. Narrow parent_id or max_depth.\n\n",
            limit
        ),
    }
}

/// The main MCP server struct
#[derive(Clone)]
pub struct WorkflowyMcpServer {
    tool_router: ToolRouter<Self>,
    client: Arc<WorkflowyClient>,
    cache: Arc<NodeCache>,
    /// Shared cancellation registry: the `cancel_all` tool bumps the generation,
    /// so every outstanding tree walk sees its guard flip and bails out early.
    cancel_registry: CancelRegistry,
    /// Opportunistic name index populated whenever a walk returns. Invalidated
    /// on every write so it cannot disagree with the tree for longer than the
    /// current mutation.
    name_index: Arc<NameIndex>,
    /// Server start time; surfaced by health_check for uptime visibility.
    started_at: Instant,
}

// --- Parameter structs ---

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Search for nodes in Workflowy by text")]
pub struct SearchNodesParams {
    #[schemars(description = "Text query to search for in node names and descriptions")]
    pub query: String,
    #[schemars(description = "Maximum number of results to return (default: 20)")]
    pub max_results: Option<usize>,
    #[schemars(description = "Parent node ID to scope the search under")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to search (default: 3). Increase for deeper searches in large trees")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Get a specific node by its ID")]
pub struct GetNodeParams {
    #[schemars(description = "The UUID of the node to retrieve")]
    pub node_id: NodeId,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Create a new node in Workflowy")]
pub struct CreateNodeParams {
    #[schemars(description = "The title/name of the new node")]
    pub name: String,
    #[schemars(description = "Optional description/note for the node")]
    pub description: Option<String>,
    #[schemars(description = "Parent node ID. If omitted, creates at root level")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Priority (position) among siblings. Lower = higher position")]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Edit an existing node's name or description")]
pub struct EditNodeParams {
    #[schemars(description = "The UUID of the node to edit")]
    pub node_id: NodeId,
    #[schemars(description = "New name for the node (leave empty to keep current)")]
    pub name: Option<String>,
    #[schemars(description = "New description for the node (leave empty to keep current)")]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Delete a node from Workflowy")]
pub struct DeleteNodeParams {
    #[schemars(description = "The UUID of the node to delete")]
    pub node_id: NodeId,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Move a node to a new parent")]
pub struct MoveNodeParams {
    #[schemars(description = "The UUID of the node to move")]
    pub node_id: NodeId,
    #[schemars(description = "The UUID of the new parent node")]
    pub new_parent_id: NodeId,
    #[schemars(description = "Position among siblings in the new parent")]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Get all children of a node")]
pub struct GetChildrenParams {
    #[schemars(description = "The UUID of the parent node")]
    pub node_id: NodeId,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Search nodes by tag")]
pub struct TagSearchParams {
    #[schemars(description = "Tag to search for (e.g. '#project' or '@person')")]
    pub tag: String,
    #[schemars(description = "Maximum results to return (default: 50)")]
    pub max_results: Option<usize>,
    #[schemars(description = "Parent node ID to scope the search under")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Insert content as hierarchical nodes from indented text")]
pub struct InsertContentParams {
    #[schemars(description = "Parent node ID to insert content under")]
    pub parent_id: NodeId,
    #[schemars(description = "Content in 2-space indented text format. Each line becomes a node, indentation creates hierarchy")]
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Get the full tree under a node")]
pub struct GetSubtreeParams {
    #[schemars(description = "The UUID of the root node")]
    pub node_id: NodeId,
    #[schemars(description = "Maximum depth to traverse (default: unlimited)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Find a node by name with match mode support")]
pub struct FindNodeParams {
    #[schemars(description = "Name of the node to find")]
    pub name: String,
    #[schemars(description = "Match mode: 'exact' (default), 'contains', or 'starts_with'")]
    pub match_mode: Option<String>,
    #[schemars(description = "1-based selection index when multiple matches exist")]
    pub selection: Option<usize>,
    #[schemars(description = "Parent node ID to scope the search under")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Opt in to scanning from the workspace root when parent_id is omitted. Disabled by default because unscoped contains-searches on large trees time out; use sparingly, or build a name index first with build_name_index")]
    pub allow_root_scan: Option<bool>,
    #[schemars(description = "Serve results from the opportunistic name index when it has data instead of walking the tree. Safe for stable names; will miss recently-created nodes not yet indexed. Default: false")]
    pub use_index: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Search for a target node and insert content under it")]
pub struct SmartInsertParams {
    #[schemars(description = "Search text to find the target node")]
    pub search_query: String,
    #[schemars(description = "Content in 2-space indented text format to insert")]
    pub content: String,
    #[schemars(description = "1-based selection index when multiple matches exist")]
    pub selection: Option<usize>,
    #[schemars(description = "Insert position: 'top' or 'bottom' (default: 'bottom')")]
    pub position: Option<String>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Daily review: overdue items, upcoming deadlines, and recent changes in one call")]
pub struct DailyReviewParams {
    #[schemars(description = "Optional root node ID to scope the review")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Max overdue items to return (default: 10)")]
    pub overdue_limit: Option<usize>,
    #[schemars(description = "Days ahead to look for upcoming items (default: 7)")]
    pub upcoming_days: Option<usize>,
    #[schemars(description = "Days back to look for recent changes (default: 1)")]
    pub recent_days: Option<usize>,
    #[schemars(description = "Max pending todos to return (default: 20)")]
    pub pending_limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Get recently modified nodes within a time window")]
pub struct GetRecentChangesParams {
    #[schemars(description = "Number of days to look back (default: 7)")]
    pub days: Option<usize>,
    #[schemars(description = "Optional root node ID to scope the search")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Include completed items (default: true)")]
    pub include_completed: Option<bool>,
    #[schemars(description = "Maximum results (default: 50)")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "List overdue items sorted by most overdue first")]
pub struct ListOverdueParams {
    #[schemars(description = "Optional root node ID to scope the search")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Include completed items (default: false)")]
    pub include_completed: Option<bool>,
    #[schemars(description = "Maximum results (default: 50)")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "List items with upcoming due dates")]
pub struct ListUpcomingParams {
    #[schemars(description = "Days ahead to look (default: 14)")]
    pub days: Option<usize>,
    #[schemars(description = "Optional root node ID to scope the search")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Include items without due dates (default: false)")]
    pub include_no_due_date: Option<bool>,
    #[schemars(description = "Maximum results (default: 50)")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Get project summary with stats, tags, and recent changes")]
pub struct GetProjectSummaryParams {
    #[schemars(description = "Root node ID of the project")]
    pub node_id: NodeId,
    #[schemars(description = "Include tag and assignee counts (default: true)")]
    pub include_tags: Option<bool>,
    #[schemars(description = "Days back for recently modified list (default: 7)")]
    pub recently_modified_days: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Find all nodes that contain a Workflowy link to a given node")]
pub struct FindBacklinksParams {
    #[schemars(description = "The node ID to find backlinks for")]
    pub node_id: NodeId,
    #[schemars(description = "Maximum results (default: 50)")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "List todo items with optional filtering")]
pub struct ListTodosParams {
    #[schemars(description = "Parent node ID to scope todos under")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Filter: 'all', 'pending', or 'completed' (default: 'all')")]
    pub status: Option<String>,
    #[schemars(description = "Optional text search within todos")]
    pub query: Option<String>,
    #[schemars(description = "Maximum results (default: 50)")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Deep-copy a node and its subtree to a new location")]
pub struct DuplicateNodeParams {
    #[schemars(description = "The node ID to duplicate")]
    pub node_id: NodeId,
    #[schemars(description = "Parent node ID for the copy")]
    pub target_parent_id: NodeId,
    #[schemars(description = "Include children (default: true)")]
    pub include_children: Option<bool>,
    #[schemars(description = "Prefix to add to the root node name")]
    pub name_prefix: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Copy a template node with {{variable}} substitution")]
pub struct CreateFromTemplateParams {
    #[schemars(description = "Template node ID to copy from")]
    pub template_node_id: NodeId,
    #[schemars(description = "Parent node ID to insert the copy under")]
    pub target_parent_id: NodeId,
    #[schemars(description = "Variables for {{key}} substitution as JSON object")]
    pub variables: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Apply an operation to all nodes matching a filter")]
pub struct BulkUpdateParams {
    #[schemars(description = "Text search filter")]
    pub query: Option<String>,
    #[schemars(description = "Filter by tag (e.g. 'urgent')")]
    pub tag: Option<String>,
    #[schemars(description = "Root node ID to scope the filter")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Status filter: 'all', 'pending', 'completed' (default: 'all')")]
    pub status: Option<String>,
    #[schemars(description = "Operation: 'complete', 'uncomplete', 'delete', 'add_tag', 'remove_tag'")]
    pub operation: String,
    #[schemars(description = "Tag value for add_tag/remove_tag operations")]
    pub operation_tag: Option<String>,
    #[schemars(description = "Preview only, no mutations (default: false)")]
    pub dry_run: Option<bool>,
    #[schemars(description = "Safety limit on affected nodes (default: 20)")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Convert markdown to Workflowy-compatible indented text format")]
pub struct ConvertMarkdownParams {
    #[schemars(description = "Markdown content to convert")]
    pub markdown: String,
    #[schemars(description = "If true, only return stats without converting (default: false)")]
    pub analyze_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[schemars(description = "Quick diagnostic: verify Workflowy API reachability without a tree walk")]
pub struct HealthCheckParams {}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[schemars(description = "Cancel any in-flight tree walks. Future calls are unaffected")]
pub struct CancelAllParams {}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(description = "Populate the opportunistic name index by walking a subtree")]
pub struct BuildNameIndexParams {
    #[schemars(description = "Root node to start the walk from. Omit with allow_root_scan=true to walk the workspace root (expensive on large trees)")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to walk (default: 10)")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Opt in to an unscoped walk when root_id is omitted. Refused by default")]
    pub allow_root_scan: Option<bool>,
}

// --- Tool router and handler ---

#[tool_router]
impl WorkflowyMcpServer {
    pub fn new(client: Arc<WorkflowyClient>) -> Self {
        Self::with_cache(client, crate::utils::cache::get_cache())
    }

    pub fn with_cache(client: Arc<WorkflowyClient>, cache: Arc<NodeCache>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            client,
            cache,
            cancel_registry: CancelRegistry::new(),
            name_index: Arc::new(NameIndex::new()),
            started_at: Instant::now(),
        }
    }

    /// Build fetch controls that honour the server-wide cancel registry plus
    /// the configured subtree-walk timeout. All handlers go through this so
    /// cancellation and deadline enforcement are uniform.
    fn fetch_controls(&self) -> FetchControls {
        FetchControls::with_timeout(Duration::from_millis(defaults::SUBTREE_FETCH_TIMEOUT_MS))
            .and_cancel(self.cancel_registry.guard())
    }

    /// Walk a subtree with the server's standard controls and push every
    /// visited node through the name index before returning. Keeps the tree
    /// walk and the opportunistic index population in one place so no handler
    /// can forget to feed the index.
    async fn walk_subtree(
        &self,
        root_id: Option<&str>,
        max_depth: usize,
    ) -> crate::error::Result<SubtreeFetch> {
        let controls = self.fetch_controls();
        let fetch = self
            .client
            .get_subtree_with_controls(root_id, max_depth, defaults::MAX_SUBTREE_NODES, controls)
            .await?;
        self.name_index.ingest(&fetch.nodes);
        Ok(fetch)
    }

    #[tool(description = "Search for nodes in Workflowy by text query. Returns matching nodes with their IDs, names, and paths. For large trees, use parent_id to scope the search and max_depth to control depth.")]
    async fn search_nodes(
        &self,
        Parameters(params): Parameters<SearchNodesParams>,
    ) -> Result<CallToolResult, McpError> {
        let max_results = params.max_results.unwrap_or(20);
        let max_depth = params.max_depth.unwrap_or(3);
        info!(query = %params.query, max_results, max_depth, "Searching nodes");

        match self.walk_subtree(params.parent_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit, .. }) => {
                let query_lower = params.query.to_lowercase();
                let mut results: Vec<&WorkflowyNode> = nodes
                    .iter()
                    .filter(|n| {
                        let name_match = n.name.to_lowercase().contains(&query_lower);
                        let desc_match = n
                            .description
                            .as_ref()
                            .map(|d| d.to_lowercase().contains(&query_lower))
                            .unwrap_or(false);
                        name_match || desc_match
                    })
                    .collect();

                results.truncate(max_results);

                let body = if results.is_empty() {
                    format!("No nodes found matching '{}'", params.query)
                } else {
                    let items: Vec<String> = results
                        .iter()
                        .map(|n| {
                            let desc = n.description.as_deref().unwrap_or("");
                            let desc_preview = if desc.len() > 100 {
                                format!("{}...", &desc[..100])
                            } else {
                                desc.to_string()
                            };
                            format!(
                                "- **{}** (id: `{}`)\n  {}",
                                n.name, n.id, desc_preview
                            )
                        })
                        .collect();
                    format!(
                        "Found {} node(s) matching '{}':\n\n{}",
                        results.len(),
                        params.query,
                        items.join("\n")
                    )
                };

                let result_text = format!("{}{}", truncation_banner(truncated, limit), body);
                Ok(CallToolResult::success(vec![Content::text(result_text)]))
            }
            Err(e) => {
                error!(error = %e, "Failed to search nodes");
                Err(McpError::internal_error(
                    format!("Failed to search nodes: {}", e),
                    None,
                ))
            }
        }
    }

    #[tool(description = "Get a specific Workflowy node by its ID. Returns the node's full details including name, description, tags, and children.")]
    async fn get_node(
        &self,
        Parameters(params): Parameters<GetNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(node_id = %params.node_id, "Getting node");
        check_node_id(&params.node_id)?;

        match self.client.get_node(&params.node_id).await {
            Ok(node) => {
                let json = serde_json::to_string_pretty(&node).map_err(|e| {
                    McpError::internal_error(format!("Serialization error: {}", e), None)
                })?;
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to get node {}: {}", params.node_id, e),
                None,
            )),
        }
    }

    #[tool(description = "Create a new node in Workflowy. Optionally specify a parent node ID and position.")]
    async fn create_node(
        &self,
        Parameters(params): Parameters<CreateNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(name = %params.name, parent = ?params.parent_id, "Creating node");
        if let Some(pid) = &params.parent_id { check_node_id(pid)?; }

        match self
            .client
            .create_node(&params.name, params.description.as_deref(), params.parent_id.as_deref(), params.priority)
            .await
        {
            Ok(created) => {
                let msg = format!(
                    "Created node '{}' (id: `{}`)",
                    params.name, created.id
                );
                // Invalidate cache for parent
                if let Some(pid) = &params.parent_id {
                    self.cache.invalidate_node(pid);
                }
                // Seed the name index so subsequent lookups see the new node
                // without needing a fresh walk.
                self.name_index.ingest(&[WorkflowyNode {
                    id: created.id.clone(),
                    name: params.name.clone(),
                    description: params.description.clone(),
                    parent_id: params.parent_id.as_deref().map(|s| s.to_string()),
                    ..Default::default()
                }]);
                Ok(CallToolResult::success(vec![Content::text(msg)]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to create node: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Edit an existing Workflowy node's name or description. At least one of name/description must be provided.")]
    async fn edit_node(
        &self,
        Parameters(params): Parameters<EditNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(node_id = %params.node_id, "Editing node");
        check_node_id(&params.node_id)?;

        // Reject no-op edits at the boundary: the Workflowy API happily
        // accepts an empty PATCH body and returns success, which would mask
        // caller bugs where a field was dropped somewhere upstream.
        if params.name.is_none() && params.description.is_none() {
            return Err(McpError::invalid_params(
                "edit_node requires at least one of `name` or `description`".to_string(),
                None,
            ));
        }

        match self
            .client
            .edit_node(&params.node_id, params.name.as_deref(), params.description.as_deref())
            .await
        {
            Ok(_) => {
                self.cache.invalidate_node(&params.node_id);
                self.name_index.invalidate_node(&params.node_id);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Updated node `{}`",
                    params.node_id
                ))]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to edit node: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Delete a Workflowy node by its ID.")]
    async fn delete_node(
        &self,
        Parameters(params): Parameters<DeleteNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(node_id = %params.node_id, "Deleting node");
        check_node_id(&params.node_id)?;

        match self.client.delete_node(&params.node_id).await {
            Ok(_) => {
                self.cache.invalidate_node(&params.node_id);
                self.name_index.invalidate_node(&params.node_id);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Deleted node `{}`",
                    params.node_id
                ))]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to delete node: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Move a node to a new parent in Workflowy.")]
    async fn move_node(
        &self,
        Parameters(params): Parameters<MoveNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(node_id = %params.node_id, new_parent = %params.new_parent_id, "Moving node");
        check_node_id(&params.node_id)?;
        check_node_id(&params.new_parent_id)?;

        // Capture the current parent before the move so we can invalidate its
        // children listing afterwards. A failed pre-read is not fatal — the
        // move itself still runs and we fall back to invalidating just the
        // node and the new parent, as the code used to.
        let old_parent_id = self
            .client
            .get_node(&params.node_id)
            .await
            .ok()
            .and_then(|n| n.parent_id);

        match self
            .client
            .move_node(&params.node_id, &params.new_parent_id, params.priority)
            .await
        {
            Ok(_) => {
                self.cache.invalidate_node(&params.node_id);
                self.cache.invalidate_node(&params.new_parent_id);
                if let Some(pid) = &old_parent_id {
                    if pid.as_str() != params.new_parent_id.as_str() {
                        self.cache.invalidate_node(pid);
                    }
                }
                self.name_index.invalidate_node(&params.node_id);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Moved node `{}` under `{}`",
                    params.node_id, params.new_parent_id
                ))]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to move node: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "List all children of a Workflowy node.")]
    async fn list_children(
        &self,
        Parameters(params): Parameters<GetChildrenParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(node_id = %params.node_id, "Getting children");
        check_node_id(&params.node_id)?;

        match self.client.get_children(&params.node_id).await {
            Ok(children) => {
                if children.is_empty() {
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Node `{}` has no children",
                        params.node_id
                    ))]))
                } else {
                    let items: Vec<String> = children
                        .iter()
                        .map(|n| format!("- **{}** (id: `{}`)", n.name, n.id))
                        .collect();
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "{} children:\n\n{}",
                        children.len(),
                        items.join("\n")
                    ))]))
                }
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to get children: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Search for nodes by tag (e.g. #project, @person). Returns all nodes containing the specified tag. Use parent_id to scope and max_depth to control search depth.")]
    async fn tag_search(
        &self,
        Parameters(params): Parameters<TagSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let max_results = params.max_results.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(3);
        info!(tag = %params.tag, max_depth, "Tag search");

        match self.walk_subtree(params.parent_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit, .. }) => {
                let tag_lower = params.tag.to_lowercase();
                let mut results: Vec<&WorkflowyNode> = nodes
                    .iter()
                    .filter(|n| {
                        let in_name = n.name.to_lowercase().contains(&tag_lower);
                        let in_desc = n
                            .description
                            .as_ref()
                            .map(|d| d.to_lowercase().contains(&tag_lower))
                            .unwrap_or(false);
                        let in_tags = n
                            .tags
                            .as_ref()
                            .map(|tags| tags.iter().any(|t| t.to_lowercase().contains(&tag_lower)))
                            .unwrap_or(false);
                        in_name || in_desc || in_tags
                    })
                    .collect();

                results.truncate(max_results);

                let banner = truncation_banner(truncated, limit);
                if results.is_empty() {
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "{}No nodes found with tag '{}'",
                        banner, params.tag
                    ))]))
                } else {
                    let items: Vec<String> = results
                        .iter()
                        .map(|n| format!("- **{}** (id: `{}`)", n.name, n.id))
                        .collect();
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "{}Found {} node(s) with tag '{}':\n\n{}",
                        banner,
                        results.len(),
                        params.tag,
                        items.join("\n")
                    ))]))
                }
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to search by tag: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Insert hierarchical content under a parent node. Content uses 2-space indentation for hierarchy — each indent level creates a child of the node above it.")]
    async fn insert_content(
        &self,
        Parameters(params): Parameters<InsertContentParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(parent_id = %params.parent_id, "Inserting content");
        check_node_id(&params.parent_id)?;

        // Parse indented lines
        struct ParsedLine<'a> { text: &'a str, indent: usize }
        let parsed: Vec<ParsedLine> = params.content.lines().filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() { return None; }
            let leading = line.len() - line.trim_start().len();
            Some(ParsedLine { text: trimmed, indent: leading / 2 })
        }).collect();

        if parsed.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text("No content to insert (all lines empty)".to_string())]));
        }

        // Parent stack: index = indent level, value = node ID at that level
        let mut parent_stack: Vec<String> = vec![params.parent_id.0.clone()];
        let mut created_count = 0;

        for line in &parsed {
            // Clamp indent to valid range
            let indent = line.indent.min(parent_stack.len().saturating_sub(1));
            let parent_id = &parent_stack[indent];

            match self.client.create_node(line.text, None, Some(parent_id), None).await {
                Ok(created) => {
                    created_count += 1;
                    // Set this node as parent for the next indent level
                    let next_level = indent + 1;
                    if next_level < parent_stack.len() {
                        parent_stack[next_level] = created.id;
                        parent_stack.truncate(next_level + 1);
                    } else {
                        parent_stack.push(created.id);
                    }
                }
                Err(e) => {
                    error!(error = %e, line = line.text, "Failed to insert line");
                    return Err(McpError::internal_error(
                        format!("Failed inserting '{}': {}", line.text, e), None,
                    ));
                }
            }
        }

        self.cache.invalidate_node(&params.parent_id);

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Inserted {} node(s) under `{}`",
            created_count, params.parent_id
        ))]))
    }

    #[tool(description = "Get the full subtree under a node, showing the hierarchical structure. Use max_depth to limit traversal depth for large trees.")]
    async fn get_subtree(
        &self,
        Parameters(params): Parameters<GetSubtreeParams>,
    ) -> Result<CallToolResult, McpError> {
        let max_depth = params.max_depth.unwrap_or(5);
        info!(node_id = %params.node_id, max_depth, "Getting subtree");
        check_node_id(&params.node_id)?;

        match self.walk_subtree(Some(&params.node_id), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit, .. }) => {
                if all_nodes.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        format!("Node `{}` not found or has no descendants", params.node_id)
                    )]));
                }
                let root_name = all_nodes.first().map(|n| n.name.as_str()).unwrap_or("unknown");
                let json = serde_json::to_string_pretty(&all_nodes).map_err(|e| {
                    McpError::internal_error(format!("Serialization error: {}", e), None)
                })?;
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "{}Subtree for '{}' ({} nodes):\n\n{}",
                    truncation_banner(truncated, limit),
                    root_name, all_nodes.len(), json
                ))]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to get subtree: {}", e),
                None,
            )),
        }
    }

    // --- New tools required by wmanage skill ---

    #[tool(description = "Find a node by name. Supports exact, contains, and starts_with match modes. Returns node_id for use with other tools. Omitting parent_id triggers a root-of-tree walk, which is refused by default on large trees — pass allow_root_scan=true to opt in, or use_index=true to serve from the opportunistic name index. Use selection to disambiguate multiple matches.")]
    async fn find_node(
        &self,
        Parameters(params): Parameters<FindNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let match_mode = params.match_mode.as_deref().unwrap_or("exact");
        let max_depth = params.max_depth.unwrap_or(3);
        let use_index = params.use_index.unwrap_or(false);
        let allow_root_scan = params.allow_root_scan.unwrap_or(false);
        if let Some(pid) = &params.parent_id {
            check_node_id(pid)?;
        }
        info!(name = %params.name, match_mode, max_depth, use_index, allow_root_scan, "Finding node");

        // Refuse unscoped walks by default so a caller that forgot `parent_id`
        // cannot blow the client timeout on a 250k-node tree. Index-backed
        // lookups are exempt because they don't touch the API.
        if params.parent_id.is_none() && !allow_root_scan && !use_index {
            return Err(McpError::invalid_params(
                "find_node refuses to scan from the workspace root by default. Pass parent_id to scope the search, set allow_root_scan=true to opt in, or set use_index=true to serve from the opportunistic name index.".to_string(),
                None,
            ));
        }

        // Index fast path. We still require either a scoped parent_id (so we
        // can filter hits) or an explicit allow_root_scan to return unscoped
        // results, to preserve the contract above.
        if use_index && self.name_index.is_populated() {
            let hits = self.name_index.lookup(&params.name, match_mode);
            let hits: Vec<_> = if let Some(parent) = params.parent_id.as_deref() {
                hits.into_iter()
                    .filter(|e| e.parent_id.as_deref() == Some(parent))
                    .collect()
            } else {
                hits
            };
            if !hits.is_empty() || !allow_root_scan {
                return Ok(self.render_find_node_index_result(&params, match_mode, hits));
            }
            // allow_root_scan=true and index empty -> fall through to live walk.
        }

        match self.walk_subtree(params.parent_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit, truncation_reason, .. }) => {
                let search = params.name.to_lowercase();
                let matches: Vec<&WorkflowyNode> = nodes.iter().filter(|n| {
                    let name = n.name.to_lowercase();
                    match match_mode {
                        "contains" => name.contains(&search),
                        "starts_with" => name.starts_with(&search),
                        _ => name == search, // exact
                    }
                }).collect();

                let node_map = build_node_map(&nodes);
                let banner = truncation_banner_with_reason(truncated, limit, truncation_reason);

                if matches.is_empty() {
                    let mut result = json!({
                        "found": false,
                        "truncated": truncated,
                        "truncation_limit": limit,
                        "truncation_reason": truncation_reason.map(|r| r.as_str()),
                        "banner": banner,
                        "message": format!("No nodes found matching '{}' (mode: {}). Try match_mode: 'contains'.", params.name, match_mode)
                    });
                    if truncated {
                        result["hint"] = json!("Results are partial — narrow parent_id or max_depth, or retry with use_index after build_name_index populates.");
                    }
                    Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                } else if matches.len() == 1 || params.selection.is_some() {
                    let idx = params.selection.unwrap_or(1);
                    if idx < 1 || idx > matches.len() {
                        return Err(McpError::invalid_params(
                            format!("Selection {} out of range (1-{})", idx, matches.len()), None
                        ));
                    }
                    let node = matches[idx - 1];
                    let path = build_node_path_with_map(&node.id, &node_map);
                    let result = json!({
                        "found": true,
                        "truncated": truncated,
                        "truncation_limit": limit,
                        "truncation_reason": truncation_reason.map(|r| r.as_str()),
                        "node_id": node.id,
                        "name": node.name,
                        "path": path,
                        "note": node.description,
                        "message": format!("Found '{}'", node.name)
                    });
                    Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                } else {
                    let options: Vec<serde_json::Value> = matches.iter().enumerate().map(|(i, n)| {
                        let path = build_node_path_with_map(&n.id, &node_map);
                        let note_preview = n.description.as_ref().map(|d| {
                            if d.len() > 60 { format!("{}...", &d[..60]) } else { d.clone() }
                        });
                        json!({
                            "option": i + 1,
                            "name": n.name,
                            "id": n.id,
                            "path": path,
                            "note_preview": note_preview
                        })
                    }).collect();
                    let result = json!({
                        "found": false,
                        "multiple_matches": true,
                        "truncated": truncated,
                        "truncation_limit": limit,
                        "truncation_reason": truncation_reason.map(|r| r.as_str()),
                        "count": matches.len(),
                        "options": options,
                        "message": format!("Found {} matches for '{}'. Use selection parameter to choose.", matches.len(), params.name)
                    });
                    Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                }
            }
            Err(e) => Err(McpError::internal_error(format!("Failed to find node: {}", e), None)),
        }
    }

    fn render_find_node_index_result(
        &self,
        params: &FindNodeParams,
        match_mode: &str,
        hits: Vec<crate::utils::name_index::NameIndexEntry>,
    ) -> CallToolResult {
        if hits.is_empty() {
            let result = json!({
                "found": false,
                "index_served": true,
                "message": format!("No nodes matching '{}' in name index (mode: {}). Retry without use_index for a live walk, or run build_name_index to populate.", params.name, match_mode)
            });
            return CallToolResult::success(vec![Content::text(result.to_string())]);
        }
        if hits.len() == 1 || params.selection.is_some() {
            let idx = params.selection.unwrap_or(1);
            if idx < 1 || idx > hits.len() {
                return CallToolResult::success(vec![Content::text(
                    json!({
                        "error": format!("Selection {} out of range (1-{})", idx, hits.len())
                    })
                    .to_string(),
                )]);
            }
            let hit = &hits[idx - 1];
            let result = json!({
                "found": true,
                "index_served": true,
                "node_id": hit.node_id,
                "name": hit.name,
                "parent_id": hit.parent_id,
                "message": format!("Found '{}' via name index", hit.name)
            });
            return CallToolResult::success(vec![Content::text(result.to_string())]);
        }
        let options: Vec<serde_json::Value> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| {
                json!({
                    "option": i + 1,
                    "name": h.name,
                    "id": h.node_id,
                    "parent_id": h.parent_id
                })
            })
            .collect();
        let result = json!({
            "found": false,
            "index_served": true,
            "multiple_matches": true,
            "count": hits.len(),
            "options": options,
            "message": format!("Found {} matches for '{}' via name index. Use selection to choose.", hits.len(), params.name)
        });
        CallToolResult::success(vec![Content::text(result.to_string())])
    }

    #[tool(description = "Search for a target node and insert content under it. Combines search and insert into one tool. If multiple matches, returns options for selection.")]
    async fn smart_insert(
        &self,
        Parameters(params): Parameters<SmartInsertParams>,
    ) -> Result<CallToolResult, McpError> {
        let max_depth = params.max_depth.unwrap_or(3);
        info!(query = %params.search_query, max_depth, "Smart insert");

        let content = params.content.trim();
        if content.is_empty() {
            return Err(McpError::invalid_params("Content cannot be empty".to_string(), None));
        }

        match self.walk_subtree(None, max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit, .. }) => {
                let query = params.search_query.to_lowercase();
                let matches: Vec<&WorkflowyNode> = nodes.iter().filter(|n| {
                    let in_name = n.name.to_lowercase().contains(&query);
                    let in_desc = n.description.as_ref()
                        .map(|d| d.to_lowercase().contains(&query))
                        .unwrap_or(false);
                    in_name || in_desc
                }).collect();

                if matches.is_empty() {
                    let hint = if truncated {
                        format!("No nodes found matching '{}' (subtree truncated at {} nodes — narrow search or raise cap)", params.search_query, limit)
                    } else {
                        format!("No nodes found matching '{}'", params.search_query)
                    };
                    return Err(McpError::invalid_params(hint, None));
                }

                if matches.len() > 1 && params.selection.is_none() {
                    let node_map = build_node_map(&nodes);
                    let options: Vec<serde_json::Value> = matches.iter().enumerate().map(|(i, n)| {
                        let path = build_node_path_with_map(&n.id, &node_map);
                        json!({ "option": i + 1, "name": n.name, "id": n.id, "path": path })
                    }).collect();
                    let result = json!({
                        "multiple_matches": true,
                        "truncated": truncated,
                        "truncation_limit": limit,
                        "count": matches.len(),
                        "options": options,
                        "message": format!("Found {} matches. Use selection parameter to choose.", matches.len())
                    });
                    return Ok(CallToolResult::success(vec![Content::text(result.to_string())]));
                }

                let idx = params.selection.unwrap_or(1);
                if idx < 1 || idx > matches.len() {
                    return Err(McpError::invalid_params(
                        format!("Selection {} out of range (1-{})", idx, matches.len()), None
                    ));
                }
                let target = matches[idx - 1];
                let target_id = target.id.clone();
                let target_name = target.name.clone();

                // Insert content lines as flat nodes under target
                let lines: Vec<&str> = content.lines().collect();
                let mut created_count = 0;
                for line in &lines {
                    let trimmed = line.trim();
                    if trimmed.is_empty() { continue; }
                    match self.client.create_node(trimmed, None, Some(&target_id), None).await {
                        Ok(_) => created_count += 1,
                        Err(e) => {
                            error!(error = %e, "Failed to insert line in smart_insert");
                            return Err(McpError::internal_error(
                                format!("Failed inserting '{}': {}", trimmed, e), None
                            ));
                        }
                    }
                }

                self.cache.invalidate_node(&target_id);

                let result = json!({
                    "success": true,
                    "truncated": truncated,
                    "truncation_limit": limit,
                    "created_count": created_count,
                    "target": { "id": target_id, "name": target_name },
                    "message": format!("Inserted {} node(s) under '{}'", created_count, target_name)
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "Daily review: get overdue items, upcoming deadlines, recent changes, and pending todos in one call. Use root_id to scope and max_depth to control depth.")]
    async fn daily_review(
        &self,
        Parameters(params): Parameters<DailyReviewParams>,
    ) -> Result<CallToolResult, McpError> {
        let max_depth = params.max_depth.unwrap_or(5);
        info!(max_depth, "Daily review");
        if let Some(rid) = &params.root_id { check_node_id(rid)?; }

        match self.walk_subtree(params.root_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit, .. }) => {
                let candidates: Vec<&WorkflowyNode> = all_nodes.iter().collect();

                let today = Utc::now().date_naive();
                let upcoming_days = params.upcoming_days.unwrap_or(7) as i64;
                let recent_days = params.recent_days.unwrap_or(1) as i64;
                let overdue_limit = params.overdue_limit.unwrap_or(10);
                let pending_limit = params.pending_limit.unwrap_or(20);
                let now_ms = Utc::now().timestamp_millis();
                let recent_cutoff = now_ms - (recent_days * 86_400_000);
                let node_map = build_node_map(&all_nodes);

                let mut overdue_items = Vec::new();
                let mut due_soon_items = Vec::new();
                let mut recent_items = Vec::new();
                let mut pending_items = Vec::new();
                let mut total = 0;
                let mut pending_count = 0;
                let mut due_today_count = 0;
                let mut modified_today = 0;

                for node in &candidates {
                    total += 1;
                    let completed = is_completed(node);
                    let todo = is_todo(node);

                    if todo && !completed {
                        pending_count += 1;
                        if pending_items.len() < pending_limit {
                            let path = build_node_path_with_map(&node.id, &node_map);
                            pending_items.push(json!({ "id": node.id, "name": node.name, "path": path }));
                        }
                    }

                    if !completed {
                        if let Some(due) = parse_due_date_from_node(node) {
                            let days_until = (due - today).num_days();
                            if days_until < 0 {
                                overdue_items.push((node, due, -days_until));
                            } else if days_until == 0 {
                                due_today_count += 1;
                                due_soon_items.push((node, due, days_until));
                            } else if days_until <= upcoming_days {
                                due_soon_items.push((node, due, days_until));
                            }
                        }
                    }

                    if let Some(mod_ts) = node.last_modified {
                        if mod_ts > recent_cutoff {
                            modified_today += 1;
                            recent_items.push((node, mod_ts));
                        }
                    }
                }

                overdue_items.sort_by(|a, b| b.2.cmp(&a.2));
                overdue_items.truncate(overdue_limit);
                due_soon_items.sort_by(|a, b| a.1.cmp(&b.1));
                due_soon_items.truncate(20);
                recent_items.sort_by(|a, b| b.1.cmp(&a.1));
                recent_items.truncate(20);

                let overdue_json: Vec<serde_json::Value> = overdue_items.iter().map(|(n, due, days)| {
                    let path = build_node_path_with_map(&n.id, &node_map);
                    json!({ "id": n.id, "name": n.name, "path": path, "due_date": due.to_string(), "days_overdue": days })
                }).collect();

                let due_soon_json: Vec<serde_json::Value> = due_soon_items.iter().map(|(n, due, days)| {
                    let path = build_node_path_with_map(&n.id, &node_map);
                    json!({ "id": n.id, "name": n.name, "path": path, "due_date": due.to_string(), "days_until_due": days })
                }).collect();

                let recent_json: Vec<serde_json::Value> = recent_items.iter().map(|(n, ts)| {
                    let path = build_node_path_with_map(&n.id, &node_map);
                    json!({ "id": n.id, "name": n.name, "path": path, "modifiedAt": ts, "completed": is_completed(n) })
                }).collect();

                let result = json!({
                    "as_of": today.to_string(),
                    "truncated": truncated,
                    "truncation_limit": limit,
                    "summary": {
                        "total_nodes": total,
                        "pending_todos": pending_count,
                        "overdue_count": overdue_items.len(),
                        "due_today": due_today_count,
                        "modified_today": modified_today
                    },
                    "overdue": overdue_json,
                    "due_soon": due_soon_json,
                    "recent_changes": recent_json,
                    "top_pending": pending_items
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed daily review: {}", e), None)),
        }
    }

    #[tool(description = "Get recently modified nodes within a time window.")]
    async fn get_recent_changes(
        &self,
        Parameters(params): Parameters<GetRecentChangesParams>,
    ) -> Result<CallToolResult, McpError> {
        let days = params.days.unwrap_or(7) as i64;
        let include_completed = params.include_completed.unwrap_or(true);
        let limit = params.limit.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(5);
        info!(days, max_depth, "Getting recent changes");
        if let Some(rid) = &params.root_id { check_node_id(rid)?; }

        match self.walk_subtree(params.root_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, .. }) => {
                let candidates: Vec<&WorkflowyNode> = all_nodes.iter().collect();

                let now_ms = Utc::now().timestamp_millis();
                let cutoff = now_ms - (days * 86_400_000);
                let node_map = build_node_map(&all_nodes);

                let mut changes: Vec<(&WorkflowyNode, i64)> = candidates.iter()
                    .filter_map(|n| {
                        let mod_ts = n.last_modified?;
                        if mod_ts <= cutoff { return None; }
                        if !include_completed && is_completed(n) { return None; }
                        Some((*n, mod_ts))
                    })
                    .collect();

                changes.sort_by(|a, b| b.1.cmp(&a.1));
                changes.truncate(limit);

                let today = Utc::now().date_naive();
                let since = today - chrono::Duration::days(days);

                let items: Vec<serde_json::Value> = changes.iter().map(|(n, ts)| {
                    let path = build_node_path_with_map(&n.id, &node_map);
                    json!({ "id": n.id, "name": n.name, "path": path, "modifiedAt": ts, "completed": is_completed(n) })
                }).collect();

                let result = json!({
                    "since": since.to_string(),
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "count": items.len(),
                    "changes": items
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "List overdue items (past due date, incomplete) sorted by most overdue first.")]
    async fn list_overdue(
        &self,
        Parameters(params): Parameters<ListOverdueParams>,
    ) -> Result<CallToolResult, McpError> {
        let include_completed = params.include_completed.unwrap_or(false);
        let limit = params.limit.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(5);
        info!(max_depth, "Listing overdue items");
        if let Some(rid) = &params.root_id { check_node_id(rid)?; }

        match self.walk_subtree(params.root_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, .. }) => {
                let candidates: Vec<&WorkflowyNode> = all_nodes.iter().collect();

                let today = Utc::now().date_naive();
                let node_map = build_node_map(&all_nodes);

                let mut overdue: Vec<(&WorkflowyNode, NaiveDate, i64)> = candidates.iter()
                    .filter_map(|n| {
                        if !include_completed && is_completed(n) { return None; }
                        let due = parse_due_date_from_node(n)?;
                        if due >= today { return None; }
                        let days_over = (today - due).num_days();
                        Some((*n, due, days_over))
                    })
                    .collect();

                overdue.sort_by(|a, b| b.2.cmp(&a.2));
                overdue.truncate(limit);

                let items: Vec<serde_json::Value> = overdue.iter().map(|(n, due, days)| {
                    let path = build_node_path_with_map(&n.id, &node_map);
                    json!({ "id": n.id, "name": n.name, "path": path, "due_date": due.to_string(), "days_overdue": days, "completed": is_completed(n) })
                }).collect();

                let result = json!({
                    "as_of": today.to_string(),
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "count": items.len(),
                    "overdue": items
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "List items with upcoming due dates, sorted by nearest deadline first.")]
    async fn list_upcoming(
        &self,
        Parameters(params): Parameters<ListUpcomingParams>,
    ) -> Result<CallToolResult, McpError> {
        let days = params.days.unwrap_or(14) as i64;
        let include_no_due_date = params.include_no_due_date.unwrap_or(false);
        let limit = params.limit.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(5);
        info!(days, max_depth, "Listing upcoming items");
        if let Some(rid) = &params.root_id { check_node_id(rid)?; }

        match self.walk_subtree(params.root_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, .. }) => {
                let candidates: Vec<&WorkflowyNode> = all_nodes.iter().collect();

                let today = Utc::now().date_naive();
                let cutoff = today + chrono::Duration::days(days);
                let node_map = build_node_map(&all_nodes);

                let mut upcoming: Vec<(&WorkflowyNode, NaiveDate, i64)> = Vec::new();
                let mut no_date: Vec<&WorkflowyNode> = Vec::new();

                for n in &candidates {
                    if is_completed(n) { continue; }
                    match parse_due_date_from_node(n) {
                        Some(due) if due <= cutoff => {
                            let days_until = (due - today).num_days();
                            upcoming.push((n, due, days_until));
                        }
                        None if include_no_due_date && is_todo(n) => {
                            no_date.push(n);
                        }
                        _ => {}
                    }
                }

                upcoming.sort_by(|a, b| a.1.cmp(&b.1));

                let mut items: Vec<serde_json::Value> = upcoming.iter().map(|(n, due, days_until)| {
                    let path = build_node_path_with_map(&n.id, &node_map);
                    json!({
                        "id": n.id, "name": n.name, "path": path,
                        "due_date": due.to_string(), "days_until_due": days_until,
                        "overdue": *days_until < 0
                    })
                }).collect();

                if include_no_due_date {
                    for n in &no_date {
                        let path = build_node_path_with_map(&n.id, &node_map);
                        items.push(json!({
                            "id": n.id, "name": n.name, "path": path,
                            "due_date": null, "days_until_due": null, "overdue": false
                        }));
                    }
                }

                items.truncate(limit);

                let result = json!({
                    "as_of": today.to_string(),
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "count": items.len(),
                    "upcoming": items
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "Get project summary with stats, tag counts, assignee counts, and recently modified nodes.")]
    async fn get_project_summary(
        &self,
        Parameters(params): Parameters<GetProjectSummaryParams>,
    ) -> Result<CallToolResult, McpError> {
        let include_tags = params.include_tags.unwrap_or(true);
        let recent_days = params.recently_modified_days.unwrap_or(7) as i64;
        info!(node_id = %params.node_id, "Getting project summary");
        check_node_id(&params.node_id)?;

        match self.walk_subtree(Some(&params.node_id), 10).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, .. }) => {
                let subtree: Vec<&WorkflowyNode> = all_nodes.iter().collect();
                if subtree.is_empty() {
                    return Err(McpError::invalid_params(
                        format!("Node '{}' not found or has no subtree", params.node_id), None
                    ));
                }

                let today = Utc::now().date_naive();
                let now_ms = Utc::now().timestamp_millis();
                let recent_cutoff = now_ms - (recent_days * 86_400_000);
                let node_map = build_node_map(&all_nodes);

                let root = subtree.iter().find(|n| n.id == params.node_id).unwrap();
                let root_path = build_node_path_with_map(&root.id, &node_map);

                let mut total = 0usize;
                let mut todo_total = 0usize;
                let mut todo_completed = 0usize;
                let mut overdue_count = 0usize;
                let mut has_due_dates = false;
                let mut tag_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                let mut assignee_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                let mut recent_modified: Vec<(&WorkflowyNode, i64)> = Vec::new();

                for node in &subtree {
                    total += 1;
                    let todo = is_todo(node);
                    let completed = is_completed(node);

                    if todo {
                        todo_total += 1;
                        if completed { todo_completed += 1; }
                    }

                    if parse_due_date_from_node(node).is_some() {
                        has_due_dates = true;
                    }
                    if is_overdue(node, today) {
                        overdue_count += 1;
                    }

                    if include_tags {
                        let parsed = parse_node_tags(node);
                        for t in &parsed.tags {
                            *tag_counts.entry(format!("#{}", t)).or_default() += 1;
                        }
                        for a in &parsed.assignees {
                            *assignee_counts.entry(format!("@{}", a)).or_default() += 1;
                        }
                    }

                    if let Some(mod_ts) = node.last_modified {
                        if mod_ts > recent_cutoff {
                            recent_modified.push((node, mod_ts));
                        }
                    }
                }

                recent_modified.sort_by(|a, b| b.1.cmp(&a.1));
                recent_modified.truncate(20);

                let completion_pct = if todo_total > 0 {
                    ((todo_completed as f64 / todo_total as f64) * 100.0).round() as usize
                } else { 0 };

                let recent_json: Vec<serde_json::Value> = recent_modified.iter().map(|(n, ts)| {
                    let path = build_node_path_with_map(&n.id, &node_map);
                    json!({ "id": n.id, "name": n.name, "modifiedAt": ts, "path": path })
                }).collect();

                let mut result = json!({
                    "root": { "id": root.id, "name": root.name, "path": root_path },
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "stats": {
                        "total_nodes": total,
                        "todo_total": todo_total,
                        "todo_pending": todo_total - todo_completed,
                        "todo_completed": todo_completed,
                        "completion_percent": completion_pct,
                        "has_due_dates": has_due_dates,
                        "overdue_count": overdue_count
                    },
                    "recently_modified": recent_json
                });

                if include_tags {
                    result["tags"] = json!(tag_counts);
                    result["assignees"] = json!(assignee_counts);
                }

                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    // --- Remaining planned tools ---

    #[tool(description = "Find all nodes that contain a Workflowy link to the given node.")]
    async fn find_backlinks(
        &self,
        Parameters(params): Parameters<FindBacklinksParams>,
    ) -> Result<CallToolResult, McpError> {
        check_node_id(&params.node_id)?;
        let limit = params.limit.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(3);
        info!(node_id = %params.node_id, max_depth, "Finding backlinks");

        match self.walk_subtree(None, max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit: node_limit, .. }) => {
                let node_map = build_node_map(&nodes);
                let target = node_map.get(params.node_id.as_str());
                let target_name = target.map(|n| n.name.as_str()).unwrap_or("unknown");

                // Match workflowy.com links containing the target node ID.
                // `regex::escape` guarantees a valid pattern, so the Regex::new
                // call below cannot fail.
                let link_re = Regex::new(&format!(
                    r"https?://workflowy\.com/#/{}",
                    regex::escape(&params.node_id)
                )).expect("escaped pattern is always valid regex");

                let mut backlinks: Vec<serde_json::Value> = Vec::new();
                for node in &nodes {
                    if node.id == params.node_id { continue; }
                    let in_name = link_re.is_match(&node.name);
                    let in_desc = node.description.as_ref().map(|d| link_re.is_match(d)).unwrap_or(false);
                    if in_name || in_desc {
                        let path = build_node_path_with_map(&node.id, &node_map);
                        let link_in = match (in_name, in_desc) {
                            (true, true) => "both",
                            (true, false) => "name",
                            _ => "note",
                        };
                        backlinks.push(json!({
                            "id": node.id, "name": node.name, "path": path, "link_in": link_in
                        }));
                        if backlinks.len() >= limit { break; }
                    }
                }

                let result = json!({
                    "target": { "id": params.node_id, "name": target_name },
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "count": backlinks.len(),
                    "backlinks": backlinks
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "List todo items, optionally filtered by parent, status, or text query.")]
    async fn list_todos(
        &self,
        Parameters(params): Parameters<ListTodosParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(50);
        let status = params.status.as_deref().unwrap_or("all");
        let max_depth = params.max_depth.unwrap_or(5);
        info!(status, max_depth, "Listing todos");
        if let Some(pid) = &params.parent_id { check_node_id(pid)?; }

        match self.walk_subtree(params.parent_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, .. }) => {
                let candidates: Vec<&WorkflowyNode> = all_nodes.iter().collect();

                let node_map = build_node_map(&all_nodes);
                let query_lower = params.query.as_ref().map(|q| q.to_lowercase());

                let mut todos: Vec<serde_json::Value> = Vec::new();
                for node in &candidates {
                    if !is_todo(node) { continue; }
                    let completed = is_completed(node);

                    match status {
                        "pending" if completed => continue,
                        "completed" if !completed => continue,
                        _ => {}
                    }

                    if let Some(q) = &query_lower {
                        let in_name = node.name.to_lowercase().contains(q);
                        let in_desc = node.description.as_ref().map(|d| d.to_lowercase().contains(q)).unwrap_or(false);
                        if !in_name && !in_desc { continue; }
                    }

                    let path = build_node_path_with_map(&node.id, &node_map);
                    todos.push(json!({
                        "id": node.id, "name": node.name, "path": path,
                        "note": node.description, "completed": completed,
                        "completed_at": node.completed_at
                    }));
                    if todos.len() >= limit { break; }
                }

                let result = json!({
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "count": todos.len(),
                    "todos": todos,
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "Deep-copy a node and its subtree to a new location.")]
    async fn duplicate_node(
        &self,
        Parameters(params): Parameters<DuplicateNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        check_node_id(&params.node_id)?;
        check_node_id(&params.target_parent_id)?;
        let include_children = params.include_children.unwrap_or(true);
        info!(node_id = %params.node_id, target = %params.target_parent_id, "Duplicating node");

        match self.walk_subtree(Some(&params.node_id), 10).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, .. }) => {
                if truncated {
                    return Err(McpError::invalid_params(
                        format!(
                            "Cannot duplicate: source subtree exceeds {} nodes. Refusing to produce a partial copy.",
                            node_limit
                        ),
                        None,
                    ));
                }
                let subtree: Vec<&WorkflowyNode> = if include_children {
                    all_nodes.iter().collect()
                } else {
                    all_nodes.iter().filter(|n| n.id == params.node_id).collect()
                };

                if subtree.is_empty() {
                    return Err(McpError::invalid_params(format!("Node '{}' not found", params.node_id), None));
                }

                // Build depth-first ordering from subtree
                let mut id_map: HashMap<String, String> = HashMap::new();
                let mut created_count = 0;

                // Process root first
                let root = subtree.iter().find(|n| n.id == params.node_id).unwrap();
                let root_name = if let Some(prefix) = &params.name_prefix {
                    format!("{}{}", prefix, root.name)
                } else {
                    root.name.clone()
                };

                match self.client.create_node(&root_name, root.description.as_deref(), Some(&params.target_parent_id), None).await {
                    Ok(created) => {
                        id_map.insert(root.id.clone(), created.id.clone());
                        created_count += 1;

                        // Process children in order
                        if include_children {
                            // Build children-of map for ordering
                            let mut children_of: HashMap<&str, Vec<&WorkflowyNode>> = HashMap::new();
                            for n in &subtree {
                                if let Some(pid) = &n.parent_id {
                                    children_of.entry(pid.as_str()).or_default().push(n);
                                }
                            }

                            // BFS to maintain tree order
                            let mut queue = std::collections::VecDeque::new();
                            if let Some(children) = children_of.get(root.id.as_str()) {
                                for child in children { queue.push_back(*child); }
                            }

                            while let Some(node) = queue.pop_front() {
                                let new_parent = node.parent_id.as_ref()
                                    .and_then(|pid| id_map.get(pid))
                                    .cloned()
                                    .unwrap_or_else(|| created.id.clone());

                                match self.client.create_node(&node.name, node.description.as_deref(), Some(&new_parent), None).await {
                                    Ok(new_node) => {
                                        id_map.insert(node.id.clone(), new_node.id.clone());
                                        created_count += 1;
                                        if let Some(children) = children_of.get(node.id.as_str()) {
                                            for child in children { queue.push_back(*child); }
                                        }
                                    }
                                    Err(e) => {
                                        error!(error = %e, "Failed to duplicate child node");
                                        return Err(McpError::internal_error(format!("Failed duplicating: {}", e), None));
                                    }
                                }
                            }
                        }

                        self.cache.invalidate_node(&params.target_parent_id);
                        let result = json!({
                            "success": true,
                            "original_id": params.node_id,
                            "new_root_id": created.id,
                            "nodes_created": created_count
                        });
                        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                    }
                    Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
                }
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "Copy a template node with {{variable}} substitution in names and descriptions.")]
    async fn create_from_template(
        &self,
        Parameters(params): Parameters<CreateFromTemplateParams>,
    ) -> Result<CallToolResult, McpError> {
        check_node_id(&params.template_node_id)?;
        check_node_id(&params.target_parent_id)?;
        let vars = params.variables.unwrap_or_default();
        info!(template = %params.template_node_id, "Creating from template");

        let var_re = Regex::new(r"\{\{(\w+)\}\}").expect("static template-variable pattern is valid");
        let substitute = |text: &str| -> String {
            var_re.replace_all(text, |caps: &regex::Captures| {
                vars.get(&caps[1]).cloned().unwrap_or_else(|| caps[0].to_string())
            }).to_string()
        };

        match self.walk_subtree(Some(&params.template_node_id), 10).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, .. }) => {
                if truncated {
                    return Err(McpError::invalid_params(
                        format!(
                            "Cannot instantiate template: source subtree exceeds {} nodes. Refusing to produce a partial copy.",
                            node_limit
                        ),
                        None,
                    ));
                }
                let subtree: Vec<&WorkflowyNode> = all_nodes.iter().collect();
                if subtree.is_empty() {
                    return Err(McpError::invalid_params(format!("Template '{}' not found", params.template_node_id), None));
                }

                let mut id_map: HashMap<String, String> = HashMap::new();
                let mut created_count = 0;
                let applied_vars: Vec<&String> = vars.keys().collect();

                let root = subtree.iter().find(|n| n.id == params.template_node_id).unwrap();
                let root_name = substitute(&root.name);
                let root_desc = root.description.as_ref().map(|d| substitute(d));

                match self.client.create_node(&root_name, root_desc.as_deref(), Some(&params.target_parent_id), None).await {
                    Ok(created) => {
                        let new_root_id = created.id.clone();
                        id_map.insert(root.id.clone(), created.id);
                        created_count += 1;

                        let mut children_of: HashMap<&str, Vec<&WorkflowyNode>> = HashMap::new();
                        for n in &subtree {
                            if let Some(pid) = &n.parent_id {
                                children_of.entry(pid.as_str()).or_default().push(n);
                            }
                        }

                        let mut queue = std::collections::VecDeque::new();
                        if let Some(children) = children_of.get(root.id.as_str()) {
                            for child in children { queue.push_back(*child); }
                        }

                        while let Some(node) = queue.pop_front() {
                            let new_parent = node.parent_id.as_ref()
                                .and_then(|pid| id_map.get(pid))
                                .cloned()
                                .unwrap_or_else(|| new_root_id.clone());

                            let name = substitute(&node.name);
                            let desc = node.description.as_ref().map(|d| substitute(d));

                            match self.client.create_node(&name, desc.as_deref(), Some(&new_parent), None).await {
                                Ok(new_node) => {
                                    id_map.insert(node.id.clone(), new_node.id);
                                    created_count += 1;
                                    if let Some(children) = children_of.get(node.id.as_str()) {
                                        for child in children { queue.push_back(*child); }
                                    }
                                }
                                Err(e) => return Err(McpError::internal_error(format!("Failed: {}", e), None)),
                            }
                        }

                        self.cache.invalidate_node(&params.target_parent_id);
                        let result = json!({
                            "success": true,
                            "template_id": params.template_node_id,
                            "new_root_id": new_root_id,
                            "nodes_created": created_count,
                            "variables_applied": applied_vars
                        });
                        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                    }
                    Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
                }
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "Apply an operation to all nodes matching a filter. Supports delete, add_tag, remove_tag. Use dry_run to preview. Note: complete/uncomplete are not yet implemented and will be rejected.")]
    async fn bulk_update(
        &self,
        Parameters(params): Parameters<BulkUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = params.dry_run.unwrap_or(false);
        let limit = params.limit.unwrap_or(20);
        let status = params.status.as_deref().unwrap_or("all");
        info!(operation = %params.operation, dry_run, "Bulk update");
        if let Some(rid) = &params.root_id { check_node_id(rid)?; }

        // Validate operation. complete/uncomplete are rejected up front because
        // the Workflowy completion endpoints are not yet modelled in the client
        // and silently returning success would be a data-integrity trap.
        let valid_ops = ["delete", "add_tag", "remove_tag"];
        if params.operation == "complete" || params.operation == "uncomplete" {
            return Err(McpError::invalid_params(
                format!(
                    "Operation '{}' is not yet supported: the Workflowy completion endpoints are not modelled. Use a tag-based workflow until completion is wired up.",
                    params.operation,
                ),
                None,
            ));
        }
        if !valid_ops.contains(&params.operation.as_str()) {
            return Err(McpError::invalid_params(
                format!("Invalid operation '{}'. Must be one of: {}", params.operation, valid_ops.join(", ")), None
            ));
        }
        if (params.operation == "add_tag" || params.operation == "remove_tag") && params.operation_tag.is_none() {
            return Err(McpError::invalid_params("operation_tag required for add_tag/remove_tag".to_string(), None));
        }

        let max_depth = params.max_depth.unwrap_or(5);
        match self.walk_subtree(params.root_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, .. }) => {
                // Refuse destructive bulk ops on a truncated view — we would
                // otherwise delete only a partial match set silently.
                if truncated && params.operation == "delete" && !dry_run {
                    return Err(McpError::invalid_params(
                        format!(
                            "Refusing to bulk-delete against a truncated subtree (capped at {} nodes). Narrow with root_id or reduce max_depth.",
                            node_limit
                        ),
                        None,
                    ));
                }

                let candidates: Vec<&WorkflowyNode> = all_nodes.iter().collect();

                let node_map = build_node_map(&all_nodes);
                let query_lower = params.query.as_ref().map(|q| q.to_lowercase());
                let tag_lower = params.tag.as_ref().map(|t| {
                    let t = t.trim_start_matches('#');
                    t.to_lowercase()
                });

                // Filter
                let matched: Vec<&WorkflowyNode> = candidates.into_iter().filter(|n| {
                    if let Some(q) = &query_lower {
                        let in_name = n.name.to_lowercase().contains(q);
                        let in_desc = n.description.as_ref().map(|d| d.to_lowercase().contains(q)).unwrap_or(false);
                        if !in_name && !in_desc { return false; }
                    }
                    if let Some(tag) = &tag_lower {
                        let parsed = parse_node_tags(n);
                        if !parsed.tags.iter().any(|t| t == tag) { return false; }
                    }
                    let completed = is_completed(n);
                    match status {
                        "pending" if completed => return false,
                        "completed" if !completed => return false,
                        _ => {}
                    }
                    true
                }).collect();

                if matched.len() > limit {
                    return Err(McpError::invalid_params(
                        format!("Matched {} nodes but limit is {}. Increase limit or narrow filter.", matched.len(), limit), None
                    ));
                }

                if dry_run {
                    let items: Vec<serde_json::Value> = matched.iter().map(|n| {
                        let path = build_node_path_with_map(&n.id, &node_map);
                        json!({ "id": n.id, "name": n.name, "path": path })
                    }).collect();
                    let result = json!({
                        "dry_run": true,
                        "truncated": truncated,
                        "truncation_limit": node_limit,
                        "matched_count": items.len(),
                        "operation": params.operation,
                        "nodes_matched": items
                    });
                    return Ok(CallToolResult::success(vec![Content::text(result.to_string())]));
                }

                // Execute operations
                let mut affected = 0;
                let mut affected_nodes: Vec<serde_json::Value> = Vec::new();
                // Collect parent IDs of mutated nodes so we can invalidate them
                // precisely instead of nuking the whole cache.
                let mut touched_parents: std::collections::HashSet<String> = std::collections::HashSet::new();

                for node in &matched {
                    let success = match params.operation.as_str() {
                        "delete" => self.client.delete_node(&node.id).await.is_ok(),
                        "add_tag" => {
                            let tag = params.operation_tag.as_ref().expect("validated non-None above");
                            let new_name = format!("{} #{}", node.name, tag.trim_start_matches('#'));
                            self.client.edit_node(&node.id, Some(&new_name), None).await.is_ok()
                        }
                        "remove_tag" => {
                            let tag = params.operation_tag.as_ref().expect("validated non-None above").trim_start_matches('#');
                            let tag_re = Regex::new(&format!(r"\s*#{}(?:\b|$)", regex::escape(tag)))
                                .expect("escaped pattern is always valid regex");
                            let new_name = tag_re.replace_all(&node.name, "").to_string();
                            self.client.edit_node(&node.id, Some(&new_name), None).await.is_ok()
                        }
                        _ => false,
                    };
                    if success {
                        affected += 1;
                        let path = build_node_path_with_map(&node.id, &node_map);
                        affected_nodes.push(json!({ "id": node.id, "name": node.name, "path": path }));
                        self.cache.invalidate_node(&node.id);
                        self.name_index.invalidate_node(&node.id);
                        if let Some(pid) = &node.parent_id {
                            touched_parents.insert(pid.clone());
                        }
                    }
                }

                // Targeted cache invalidation: touch each mutated node's parent
                // (so its children listing refreshes) plus the scoped root if
                // the caller provided one. No global cache wipe.
                for pid in &touched_parents {
                    self.cache.invalidate_node(pid);
                }
                if let Some(rid) = &params.root_id {
                    self.cache.invalidate_subtree(rid);
                }

                let result = json!({
                    "dry_run": false,
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "matched_count": matched.len(),
                    "affected_count": affected,
                    "operation": params.operation,
                    "nodes_affected": affected_nodes
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
    }

    #[tool(description = "Convert markdown to Workflowy-compatible 2-space indented text format. Handles headers, lists, code blocks, blockquotes, and tables.")]
    async fn convert_markdown(
        &self,
        Parameters(params): Parameters<ConvertMarkdownParams>,
    ) -> Result<CallToolResult, McpError> {
        let analyze_only = params.analyze_only.unwrap_or(false);
        info!(analyze_only, "Converting markdown");

        let mut output_lines: Vec<String> = Vec::new();
        let mut current_indent = 0usize;
        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_lines: Vec<String> = Vec::new();
        let mut stats = json!({
            "headers": 0, "list_items": 0, "code_blocks": 0,
            "tables": 0, "blockquotes": 0, "paragraphs": 0
        });

        for line in params.markdown.lines() {
            // Code block toggle
            if line.trim_start().starts_with("```") {
                if in_code_block {
                    // End code block
                    let indent = "  ".repeat(current_indent);
                    let label = if code_lang.is_empty() { "Code".to_string() } else { format!("Code: {}", code_lang) };
                    output_lines.push(format!("{}[{}]", indent, label));
                    for cl in &code_lines {
                        output_lines.push(format!("{}  {}", indent, cl));
                    }
                    code_lines.clear();
                    code_lang.clear();
                    in_code_block = false;
                    *stats.get_mut("code_blocks").unwrap() = json!(stats["code_blocks"].as_i64().unwrap() + 1);
                } else {
                    in_code_block = true;
                    code_lang = line.trim_start().trim_start_matches('`').trim().to_string();
                }
                continue;
            }
            if in_code_block {
                code_lines.push(line.to_string());
                continue;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }

            // ATX headers
            if let Some(rest) = trimmed.strip_prefix('#') {
                let level = 1 + rest.chars().take_while(|c| *c == '#').count();
                let text = rest.trim_start_matches('#').trim();
                if !text.is_empty() {
                    current_indent = (level - 1).min(9);
                    let indent = "  ".repeat(current_indent);
                    output_lines.push(format!("{}{}", indent, text));
                    *stats.get_mut("headers").unwrap() = json!(stats["headers"].as_i64().unwrap() + 1);
                }
                continue;
            }

            // Blockquotes
            if trimmed.starts_with('>') {
                let text = trimmed.trim_start_matches('>').trim();
                let indent = "  ".repeat(current_indent + 1);
                output_lines.push(format!("{}> {}", indent, text));
                *stats.get_mut("blockquotes").unwrap() = json!(stats["blockquotes"].as_i64().unwrap() + 1);
                continue;
            }

            // Horizontal rules
            if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                let indent = "  ".repeat(current_indent);
                output_lines.push(format!("{}---", indent));
                continue;
            }

            // Unordered lists
            if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
                let leading = line.len() - line.trim_start().len();
                let list_indent = current_indent + (leading / 2) + 1;
                let text = &trimmed[2..];
                let indent = "  ".repeat(list_indent);
                output_lines.push(format!("{}{}", indent, text));
                *stats.get_mut("list_items").unwrap() = json!(stats["list_items"].as_i64().unwrap() + 1);
                continue;
            }

            // Ordered lists
            if let Some(pos) = trimmed.find(". ") {
                if pos <= 3 && trimmed[..pos].chars().all(|c| c.is_ascii_digit()) {
                    let leading = line.len() - line.trim_start().len();
                    let list_indent = current_indent + (leading / 2) + 1;
                    let text = &trimmed[pos + 2..];
                    let indent = "  ".repeat(list_indent);
                    output_lines.push(format!("{}{}", indent, text));
                    *stats.get_mut("list_items").unwrap() = json!(stats["list_items"].as_i64().unwrap() + 1);
                    continue;
                }
            }

            // Table rows (pipe-delimited)
            if trimmed.starts_with('|') && trimmed.ends_with('|') {
                // Skip separator rows
                if trimmed.contains("---") { continue; }
                let cells: Vec<&str> = trimmed.split('|').filter(|s| !s.trim().is_empty()).map(|s| s.trim()).collect();
                let indent = "  ".repeat(current_indent + 1);
                for cell in &cells {
                    output_lines.push(format!("{}{}", indent, cell));
                }
                *stats.get_mut("tables").unwrap() = json!(stats["tables"].as_i64().unwrap() + 1);
                continue;
            }

            // Plain paragraph
            let indent = "  ".repeat(current_indent);
            output_lines.push(format!("{}{}", indent, trimmed));
            *stats.get_mut("paragraphs").unwrap() = json!(stats["paragraphs"].as_i64().unwrap() + 1);
        }

        // Close unclosed code block
        if in_code_block && !code_lines.is_empty() {
            let indent = "  ".repeat(current_indent);
            let label = if code_lang.is_empty() { "Code".to_string() } else { format!("Code: {}", code_lang) };
            output_lines.push(format!("{}[{}]", indent, label));
            for cl in &code_lines {
                output_lines.push(format!("{}  {}", indent, cl));
            }
            *stats.get_mut("code_blocks").unwrap() = json!(stats["code_blocks"].as_i64().unwrap() + 1);
        }

        let content = output_lines.join("\n");
        let node_count = output_lines.len();

        stats["original_lines"] = json!(params.markdown.lines().count());
        stats["output_lines"] = json!(node_count);

        if analyze_only {
            let result = json!({ "analyze_only": true, "node_count": node_count, "stats": stats });
            return Ok(CallToolResult::success(vec![Content::text(result.to_string())]));
        }

        let result = json!({
            "success": true,
            "content": content,
            "node_count": node_count,
            "stats": stats,
            "usage_hint": "Pass the 'content' field to insert_content to add to Workflowy"
        });
        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
    }

    #[tool(description = "Quick diagnostic. Calls the Workflowy API with a short budget to confirm reachability and reports cache/name-index sizes. Sub-second regardless of tree size; use this to decide whether a larger tool call will succeed.")]
    async fn health_check(
        &self,
        Parameters(_params): Parameters<HealthCheckParams>,
    ) -> Result<CallToolResult, McpError> {
        let started = Instant::now();
        let timeout = Duration::from_millis(defaults::HEALTH_CHECK_TIMEOUT_MS);
        let probe = tokio::time::timeout(timeout, self.client.get_top_level_nodes()).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (api_reachable, top_level_count, error) = match probe {
            Ok(Ok(nodes)) => (true, Some(nodes.len()), None::<String>),
            Ok(Err(e)) => (false, None, Some(e.to_string())),
            Err(_) => (false, None, Some(format!("timed out after {} ms", timeout.as_millis()))),
        };
        let cache_stats = self.cache.stats();
        let result = json!({
            "status": if api_reachable { "ok" } else { "degraded" },
            "api_reachable": api_reachable,
            "latency_ms": elapsed_ms,
            "budget_ms": timeout.as_millis() as u64,
            "top_level_count": top_level_count,
            "cache": {
                "node_count": cache_stats.node_count,
                "parent_count": cache_stats.parent_count,
            },
            "name_index": {
                "size": self.name_index.size(),
                "populated": self.name_index.is_populated(),
            },
            "uptime_seconds": self.started_at.elapsed().as_secs(),
            "cancel_generation": self.cancel_registry.generation(),
            "error": error,
        });
        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
    }

    #[tool(description = "Cancel every in-flight tree walk. Subsequent calls are unaffected. Use when a find_node / get_subtree / search is taking longer than the client is willing to wait.")]
    async fn cancel_all(
        &self,
        Parameters(_params): Parameters<CancelAllParams>,
    ) -> Result<CallToolResult, McpError> {
        let new_gen = self.cancel_registry.cancel_all();
        let result = json!({
            "status": "cancelled",
            "generation": new_gen,
            "message": "In-flight walks have been signalled to return partial results; new calls start fresh."
        });
        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
    }

    #[tool(description = "Walk a subtree and populate the opportunistic name index. After this, find_node with use_index=true can answer lookups without touching the API. Walks are bounded by the standard subtree-fetch timeout and node-count cap, so large scopes may return partial results.")]
    async fn build_name_index(
        &self,
        Parameters(params): Parameters<BuildNameIndexParams>,
    ) -> Result<CallToolResult, McpError> {
        let max_depth = params.max_depth.unwrap_or(defaults::MAX_TREE_DEPTH);
        let allow_root_scan = params.allow_root_scan.unwrap_or(false);
        if let Some(rid) = &params.root_id {
            check_node_id(rid)?;
        }
        if params.root_id.is_none() && !allow_root_scan {
            return Err(McpError::invalid_params(
                "build_name_index refuses an unscoped walk by default. Pass root_id to scope it, or set allow_root_scan=true to accept a full walk (bounded by the subtree-fetch budget).".to_string(),
                None,
            ));
        }
        info!(root_id = ?params.root_id, max_depth, allow_root_scan, "Building name index");

        match self.walk_subtree(params.root_id.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit, truncation_reason, elapsed_ms }) => {
                let result = json!({
                    "status": if truncated { "partial" } else { "ok" },
                    "nodes_indexed": nodes.len(),
                    "index_size_after": self.name_index.size(),
                    "truncated": truncated,
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_limit": limit,
                    "elapsed_ms": elapsed_ms,
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed to build name index: {}", e), None)),
        }
    }
}

#[tool_handler]
impl ServerHandler for WorkflowyMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_03_26,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "workflowy-mcp-server".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            instructions: Some(
                "I manage Workflowy content. Available operations:
- search_nodes: Search by text query
- find_node: Find node by name (exact/contains/starts_with). Requires parent_id or allow_root_scan=true; set use_index=true for cached fast path.
- get_node: Get a specific node by ID
- list_children: List children of a node
- get_subtree: Get the full tree under a node (bounded by timeout + node cap; see truncation_reason)
- create_node: Create a new node
- edit_node: Edit a node's name or description (at least one must be provided)
- delete_node: Delete a node
- move_node: Move a node to a new parent (invalidates old and new parent)
- insert_content: Insert hierarchical content from indented text
- smart_insert: Search + insert in one call
- tag_search: Search by tag (#tag or @person)
- daily_review: Overdue, upcoming, recent changes summary
- get_recent_changes: Recently modified nodes
- list_overdue: Past-due items
- list_upcoming: Upcoming deadlines
- get_project_summary: Project stats, tags, assignees
- find_backlinks: Find nodes linking to a given node
- list_todos: List todo items with filtering
- duplicate_node: Deep-copy a node subtree
- create_from_template: Copy template with {{variable}} substitution
- bulk_update: Apply operations to filtered nodes (with dry_run)
- convert_markdown: Convert markdown to Workflowy format
- health_check: Sub-second API + cache + index diagnostic
- cancel_all: Cancel in-flight tree walks so partial results return immediately
- build_name_index: Populate the opportunistic name index for fast find_node lookups"
                    .to_string(),
            ),
        }
    }
}

/// Start the MCP server on stdio transport
pub async fn run_server(client: Arc<WorkflowyClient>) -> anyhow::Result<()> {
    info!("Starting Workflowy MCP Server on stdio");

    let server = WorkflowyMcpServer::new(client);
    let service = server.serve(stdio()).await.inspect_err(|e| {
        error!("MCP serve error: {:?}", e);
    })?;

    service.waiting().await?;
    info!("MCP server shut down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- Parameter deserialization tests ---

    #[test]
    fn test_search_nodes_params_full() {
        let params: SearchNodesParams = serde_json::from_value(json!({
            "query": "test query",
            "max_results": 10,
            "parent_id": "abc-123"
        }))
        .unwrap();
        assert_eq!(params.query, "test query");
        assert_eq!(params.max_results, Some(10));
        assert_eq!(params.parent_id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn test_search_nodes_params_minimal() {
        let params: SearchNodesParams = serde_json::from_value(json!({
            "query": "test"
        }))
        .unwrap();
        assert_eq!(params.query, "test");
        assert_eq!(params.max_results, None);
        assert_eq!(params.parent_id, None);
    }

    #[test]
    fn test_search_nodes_params_missing_query() {
        let result: Result<SearchNodesParams, _> = serde_json::from_value(json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn test_create_node_params_full() {
        let params: CreateNodeParams = serde_json::from_value(json!({
            "name": "New Node",
            "description": "A description",
            "parent_id": "parent-uuid",
            "priority": 5
        }))
        .unwrap();
        assert_eq!(params.name, "New Node");
        assert_eq!(params.description.as_deref(), Some("A description"));
        assert_eq!(params.parent_id.as_deref(), Some("parent-uuid"));
        assert_eq!(params.priority, Some(5));
    }

    #[test]
    fn test_create_node_params_minimal() {
        let params: CreateNodeParams = serde_json::from_value(json!({
            "name": "Just a name"
        }))
        .unwrap();
        assert_eq!(params.name, "Just a name");
        assert_eq!(params.description, None);
        assert_eq!(params.parent_id, None);
        assert_eq!(params.priority, None);
    }

    #[test]
    fn test_edit_node_params() {
        let params: EditNodeParams = serde_json::from_value(json!({
            "node_id": "node-uuid",
            "name": "Updated Name"
        }))
        .unwrap();
        assert_eq!(params.node_id, "node-uuid");
        assert_eq!(params.name.as_deref(), Some("Updated Name"));
        assert_eq!(params.description, None);
    }

    #[test]
    fn test_move_node_params() {
        let params: MoveNodeParams = serde_json::from_value(json!({
            "node_id": "node-1",
            "new_parent_id": "parent-2",
            "priority": 0
        }))
        .unwrap();
        assert_eq!(params.node_id, "node-1");
        assert_eq!(params.new_parent_id, "parent-2");
        assert_eq!(params.priority, Some(0));
    }

    #[test]
    fn test_tag_search_params() {
        let params: TagSearchParams = serde_json::from_value(json!({
            "tag": "#urgent",
            "max_results": 25
        }))
        .unwrap();
        assert_eq!(params.tag, "#urgent");
        assert_eq!(params.max_results, Some(25));
    }

    #[test]
    fn test_insert_content_params() {
        let params: InsertContentParams = serde_json::from_value(json!({
            "parent_id": "target-node",
            "content": "Line 1\n  Child 1\n  Child 2\nLine 2"
        }))
        .unwrap();
        assert_eq!(params.parent_id, "target-node");
        assert!(params.content.contains("Child 1"));
    }

    #[test]
    fn test_get_subtree_params() {
        let params: GetSubtreeParams = serde_json::from_value(json!({
            "node_id": "root-node",
            "max_depth": 3
        }))
        .unwrap();
        assert_eq!(params.node_id, "root-node");
        assert_eq!(params.max_depth, Some(3));
    }

    // --- Server construction test ---

    #[test]
    fn test_server_info() {
        let client = Arc::new(WorkflowyClient::new(
            "https://workflowy.com/api/v1".to_string(),
            "test-key".to_string(),
        ).unwrap());
        let server = WorkflowyMcpServer::new(client);
        let info = server.get_info();
        assert_eq!(info.server_info.name, "workflowy-mcp-server");
        assert!(info.instructions.is_some());
        assert!(info.instructions.unwrap().contains("search_nodes"));
    }

    // --- Tool listing test ---

    #[test]
    fn test_server_lists_all_tools() {
        let client = Arc::new(WorkflowyClient::new(
            "https://workflowy.com/api/v1".to_string(),
            "test-key".to_string(),
        ).unwrap());
        let server = WorkflowyMcpServer::new(client);

        let expected_tools = [
            "search_nodes",
            "get_node",
            "create_node",
            "edit_node",
            "delete_node",
            "move_node",
            "list_children",
            "tag_search",
            "insert_content",
            "get_subtree",
            "find_node",
            "smart_insert",
            "daily_review",
            "get_recent_changes",
            "list_overdue",
            "list_upcoming",
            "get_project_summary",
            "find_backlinks",
            "list_todos",
            "duplicate_node",
            "create_from_template",
            "bulk_update",
            "convert_markdown",
        ];

        for tool_name in &expected_tools {
            assert!(
                server.get_tool(tool_name).is_some(),
                "Tool '{}' should be registered",
                tool_name
            );
        }

        // Verify get_children is NOT registered (renamed to list_children)
        assert!(server.get_tool("get_children").is_none(), "get_children should be renamed to list_children");
    }

    // --- New tool parameter tests ---

    #[test]
    fn test_find_node_params() {
        let params: FindNodeParams = serde_json::from_value(json!({
            "name": "Tasks",
            "match_mode": "contains",
            "selection": 2
        })).unwrap();
        assert_eq!(params.name, "Tasks");
        assert_eq!(params.match_mode.as_deref(), Some("contains"));
        assert_eq!(params.selection, Some(2));
    }

    #[test]
    fn test_find_node_params_minimal() {
        let params: FindNodeParams = serde_json::from_value(json!({
            "name": "Tasks"
        })).unwrap();
        assert_eq!(params.name, "Tasks");
        assert_eq!(params.match_mode, None);
        assert_eq!(params.selection, None);
    }

    #[test]
    fn test_smart_insert_params() {
        let params: SmartInsertParams = serde_json::from_value(json!({
            "search_query": "Office",
            "content": "New task\n  Sub-task",
            "selection": 1,
            "position": "top"
        })).unwrap();
        assert_eq!(params.search_query, "Office");
        assert!(params.content.contains("Sub-task"));
        assert_eq!(params.selection, Some(1));
        assert_eq!(params.position.as_deref(), Some("top"));
    }

    #[test]
    fn test_daily_review_params() {
        let params: DailyReviewParams = serde_json::from_value(json!({
            "root_id": "tasks-node",
            "overdue_limit": 5,
            "upcoming_days": 14,
            "recent_days": 3,
            "pending_limit": 10
        })).unwrap();
        assert_eq!(params.root_id.as_deref(), Some("tasks-node"));
        assert_eq!(params.overdue_limit, Some(5));
        assert_eq!(params.upcoming_days, Some(14));
        assert_eq!(params.recent_days, Some(3));
        assert_eq!(params.pending_limit, Some(10));
    }

    #[test]
    fn test_daily_review_params_defaults() {
        let params: DailyReviewParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(params.root_id, None);
        assert_eq!(params.overdue_limit, None);
    }

    #[test]
    fn test_get_recent_changes_params() {
        let params: GetRecentChangesParams = serde_json::from_value(json!({
            "days": 3,
            "root_id": "project-1",
            "include_completed": false,
            "limit": 25
        })).unwrap();
        assert_eq!(params.days, Some(3));
        assert_eq!(params.include_completed, Some(false));
        assert_eq!(params.limit, Some(25));
    }

    #[test]
    fn test_list_overdue_params() {
        let params: ListOverdueParams = serde_json::from_value(json!({
            "root_id": "tasks",
            "include_completed": true,
            "limit": 10
        })).unwrap();
        assert_eq!(params.root_id.as_deref(), Some("tasks"));
        assert_eq!(params.include_completed, Some(true));
        assert_eq!(params.limit, Some(10));
    }

    #[test]
    fn test_list_upcoming_params() {
        let params: ListUpcomingParams = serde_json::from_value(json!({
            "days": 30,
            "include_no_due_date": true,
            "limit": 100
        })).unwrap();
        assert_eq!(params.days, Some(30));
        assert_eq!(params.include_no_due_date, Some(true));
        assert_eq!(params.limit, Some(100));
    }

    #[test]
    fn test_get_project_summary_params() {
        let params: GetProjectSummaryParams = serde_json::from_value(json!({
            "node_id": "project-root",
            "include_tags": false,
            "recently_modified_days": 14
        })).unwrap();
        assert_eq!(params.node_id, "project-root");
        assert_eq!(params.include_tags, Some(false));
        assert_eq!(params.recently_modified_days, Some(14));
    }

    // --- New tool param tests (batch 2) ---

    #[test]
    fn test_find_backlinks_params() {
        let params: FindBacklinksParams = serde_json::from_value(json!({
            "node_id": "target-uuid",
            "limit": 25
        })).unwrap();
        assert_eq!(params.node_id, "target-uuid");
        assert_eq!(params.limit, Some(25));
    }

    #[test]
    fn test_list_todos_params() {
        let params: ListTodosParams = serde_json::from_value(json!({
            "parent_id": "tasks-root",
            "status": "pending",
            "query": "review",
            "limit": 10
        })).unwrap();
        assert_eq!(params.parent_id.as_deref(), Some("tasks-root"));
        assert_eq!(params.status.as_deref(), Some("pending"));
        assert_eq!(params.query.as_deref(), Some("review"));
        assert_eq!(params.limit, Some(10));
    }

    #[test]
    fn test_list_todos_params_minimal() {
        let params: ListTodosParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(params.parent_id, None);
        assert_eq!(params.status, None);
    }

    #[test]
    fn test_duplicate_node_params() {
        let params: DuplicateNodeParams = serde_json::from_value(json!({
            "node_id": "src-node",
            "target_parent_id": "dest-parent",
            "include_children": false,
            "name_prefix": "Copy of "
        })).unwrap();
        assert_eq!(params.node_id, "src-node");
        assert_eq!(params.target_parent_id, "dest-parent");
        assert_eq!(params.include_children, Some(false));
        assert_eq!(params.name_prefix.as_deref(), Some("Copy of "));
    }

    #[test]
    fn test_create_from_template_params() {
        let params: CreateFromTemplateParams = serde_json::from_value(json!({
            "template_node_id": "tmpl-1",
            "target_parent_id": "parent-1",
            "variables": { "project": "Alpha", "date": "2026-04-01" }
        })).unwrap();
        assert_eq!(params.template_node_id, "tmpl-1");
        assert_eq!(params.target_parent_id, "parent-1");
        let vars = params.variables.unwrap();
        assert_eq!(vars.get("project").unwrap(), "Alpha");
        assert_eq!(vars.get("date").unwrap(), "2026-04-01");
    }

    #[test]
    fn test_bulk_update_params() {
        let params: BulkUpdateParams = serde_json::from_value(json!({
            "query": "old items",
            "tag": "archive",
            "operation": "add_tag",
            "operation_tag": "archived",
            "dry_run": true,
            "limit": 50
        })).unwrap();
        assert_eq!(params.query.as_deref(), Some("old items"));
        assert_eq!(params.tag.as_deref(), Some("archive"));
        assert_eq!(params.operation, "add_tag");
        assert_eq!(params.operation_tag.as_deref(), Some("archived"));
        assert_eq!(params.dry_run, Some(true));
        assert_eq!(params.limit, Some(50));
    }

    #[test]
    fn test_bulk_update_params_minimal() {
        let params: BulkUpdateParams = serde_json::from_value(json!({
            "operation": "complete"
        })).unwrap();
        assert_eq!(params.operation, "complete");
        assert_eq!(params.query, None);
        assert_eq!(params.dry_run, None);
    }

    #[test]
    fn test_convert_markdown_params() {
        let params: ConvertMarkdownParams = serde_json::from_value(json!({
            "markdown": "# Hello\n\n- Item 1\n- Item 2",
            "analyze_only": true
        })).unwrap();
        assert!(params.markdown.contains("# Hello"));
        assert_eq!(params.analyze_only, Some(true));
    }

    #[test]
    fn test_convert_markdown_params_minimal() {
        let params: ConvertMarkdownParams = serde_json::from_value(json!({
            "markdown": "Just text"
        })).unwrap();
        assert_eq!(params.markdown, "Just text");
        assert_eq!(params.analyze_only, None);
    }

    // --- Truncation banner ---

    #[test]
    fn test_truncation_banner_silent_when_complete() {
        assert_eq!(truncation_banner(false, 10_000), "");
    }

    #[test]
    fn test_truncation_banner_announces_limit() {
        let banner = truncation_banner(true, 10_000);
        assert!(banner.contains("truncated"));
        assert!(banner.contains("10000"));
        assert!(banner.ends_with("\n\n"));
    }

    // --- bulk_update: complete/uncomplete must be rejected ---

    fn new_test_server() -> WorkflowyMcpServer {
        let client = Arc::new(WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        ).unwrap());
        WorkflowyMcpServer::new(client)
    }

    #[tokio::test]
    async fn test_bulk_update_rejects_complete() {
        let server = new_test_server();
        let params = BulkUpdateParams {
            root_id: None,
            operation: "complete".to_string(),
            query: None,
            tag: None,
            status: None,
            operation_tag: None,
            dry_run: Some(true),
            limit: Some(1),
            max_depth: None,
        };
        let result = server.bulk_update(Parameters(params)).await;
        let err = result.expect_err("complete must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("not yet supported"), "got: {msg}");
    }

    #[tokio::test]
    async fn test_bulk_update_rejects_uncomplete() {
        let server = new_test_server();
        let params = BulkUpdateParams {
            root_id: None,
            operation: "uncomplete".to_string(),
            query: None,
            tag: None,
            status: None,
            operation_tag: None,
            dry_run: Some(true),
            limit: Some(1),
            max_depth: None,
        };
        let result = server.bulk_update(Parameters(params)).await;
        let err = result.expect_err("uncomplete must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("not yet supported"), "got: {msg}");
    }

    #[tokio::test]
    async fn test_bulk_update_rejects_unknown_operation() {
        let server = new_test_server();
        let params = BulkUpdateParams {
            root_id: None,
            operation: "nuke".to_string(),
            query: None,
            tag: None,
            status: None,
            operation_tag: None,
            dry_run: Some(true),
            limit: Some(1),
            max_depth: None,
        };
        let result = server.bulk_update(Parameters(params)).await;
        let err = result.expect_err("unknown operations must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("invalid operation"), "got: {msg}");
    }

    // --- New hardening + tool tests ---

    fn valid_id() -> &'static str {
        "550e8400-e29b-41d4-a716-446655440000"
    }

    fn other_valid_id() -> &'static str {
        "550e8400-e29b-41d4-a716-446655440001"
    }

    #[test]
    fn find_node_params_accept_new_flags() {
        let params: FindNodeParams = serde_json::from_value(json!({
            "name": "Tasks",
            "allow_root_scan": true,
            "use_index": true,
        }))
        .unwrap();
        assert_eq!(params.allow_root_scan, Some(true));
        assert_eq!(params.use_index, Some(true));
    }

    #[test]
    fn find_node_params_default_both_flags_none() {
        let params: FindNodeParams = serde_json::from_value(json!({ "name": "Tasks" })).unwrap();
        assert_eq!(params.allow_root_scan, None);
        assert_eq!(params.use_index, None);
    }

    #[test]
    fn truncation_banner_timeout_text_differs_from_node_limit() {
        let timeout = truncation_banner_with_reason(true, 10_000, Some(TruncationReason::Timeout));
        let limit = truncation_banner_with_reason(true, 10_000, Some(TruncationReason::NodeLimit));
        assert!(timeout.contains("timed out"), "timeout banner: {timeout}");
        assert!(limit.contains("truncated at"), "limit banner: {limit}");
        assert_ne!(timeout, limit);
    }

    #[test]
    fn truncation_banner_cancelled_text() {
        let banner =
            truncation_banner_with_reason(true, 10_000, Some(TruncationReason::Cancelled));
        assert!(banner.contains("cancelled"), "banner: {banner}");
    }

    #[tokio::test]
    async fn edit_node_rejects_empty_update() {
        let server = new_test_server();
        let params = EditNodeParams {
            node_id: NodeId::from(valid_id()),
            name: None,
            description: None,
        };
        let err = server
            .edit_node(Parameters(params))
            .await
            .expect_err("edit_node with no fields must reject");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("at least one"), "got: {msg}");
    }

    #[tokio::test]
    async fn edit_node_rejects_invalid_id() {
        let server = new_test_server();
        let params = EditNodeParams {
            node_id: NodeId::from(""),
            name: Some("x".to_string()),
            description: None,
        };
        let err = server
            .edit_node(Parameters(params))
            .await
            .expect_err("edit_node with empty id must reject");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("invalid node id"), "got: {msg}");
    }

    #[tokio::test]
    async fn find_node_refuses_root_scan_by_default() {
        let server = new_test_server();
        let params = FindNodeParams {
            name: "Tasks".to_string(),
            match_mode: None,
            selection: None,
            parent_id: None,
            max_depth: None,
            allow_root_scan: None,
            use_index: None,
        };
        let err = server
            .find_node(Parameters(params))
            .await
            .expect_err("unscoped find_node must refuse by default");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("allow_root_scan"), "got: {msg}");
    }

    #[tokio::test]
    async fn find_node_rejects_invalid_parent_id() {
        let server = new_test_server();
        let params = FindNodeParams {
            name: "Tasks".to_string(),
            match_mode: None,
            selection: None,
            parent_id: Some(NodeId::from("not-a-uuid")),
            max_depth: None,
            allow_root_scan: None,
            use_index: None,
        };
        let err = server.find_node(Parameters(params)).await.expect_err("bad id must reject");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("invalid node id"), "got: {msg}");
    }

    #[tokio::test]
    async fn find_node_use_index_returns_hit_without_walking() {
        let server = new_test_server();
        // Seed the name index directly — the live client is unreachable so a
        // walk would fail. This asserts the fast path skips the walk.
        server.name_index.ingest(&[WorkflowyNode {
            id: valid_id().to_string(),
            name: "Tasks".to_string(),
            parent_id: None,
            ..Default::default()
        }]);
        let params = FindNodeParams {
            name: "Tasks".to_string(),
            match_mode: Some("exact".to_string()),
            selection: None,
            parent_id: None,
            max_depth: None,
            allow_root_scan: None,
            use_index: Some(true),
        };
        let result = server
            .find_node(Parameters(params))
            .await
            .expect("index path must succeed");
        let body = result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert!(body.contains("\"index_served\":true"), "body: {body}");
        assert!(body.contains(valid_id()), "body: {body}");
    }

    #[tokio::test]
    async fn cancel_all_bumps_generation() {
        let server = new_test_server();
        let before = server.cancel_registry.generation();
        let result = server
            .cancel_all(Parameters(CancelAllParams::default()))
            .await
            .expect("cancel_all never fails");
        let after = server.cancel_registry.generation();
        assert_eq!(after, before + 1);
        let body = result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert!(body.contains("\"status\":\"cancelled\""), "body: {body}");
    }

    #[tokio::test]
    async fn health_check_reports_degraded_on_unreachable_api() {
        // The test client points at an unreachable host, so the probe must
        // fail quickly without blowing the budget.
        let server = new_test_server();
        let result = server
            .health_check(Parameters(HealthCheckParams::default()))
            .await
            .expect("health_check must always return");
        let body = result
            .content
            .into_iter()
            .next()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert!(body.contains("\"status\":\"degraded\""), "body: {body}");
        assert!(body.contains("\"api_reachable\":false"), "body: {body}");
        assert!(body.contains("\"latency_ms\""), "body: {body}");
    }

    #[tokio::test]
    async fn build_name_index_refuses_root_scan_by_default() {
        let server = new_test_server();
        let params = BuildNameIndexParams {
            root_id: None,
            max_depth: None,
            allow_root_scan: None,
        };
        let err = server
            .build_name_index(Parameters(params))
            .await
            .expect_err("unscoped build_name_index must refuse");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("allow_root_scan"), "got: {msg}");
    }

    #[tokio::test]
    async fn move_node_rejects_invalid_target() {
        let server = new_test_server();
        let params = MoveNodeParams {
            node_id: NodeId::from(""),
            new_parent_id: NodeId::from(other_valid_id()),
            priority: None,
        };
        let err = server.move_node(Parameters(params)).await.expect_err("bad id must reject");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("invalid node id"), "got: {msg}");
    }

    #[tokio::test]
    async fn new_tools_are_registered() {
        let server = new_test_server();
        assert!(server.get_tool("health_check").is_some());
        assert!(server.get_tool("cancel_all").is_some());
        assert!(server.get_tool("build_name_index").is_some());
    }

    #[tokio::test]
    async fn cancel_registry_flips_guards_held_by_walk_controls() {
        let server = new_test_server();
        let guard = server.cancel_registry.guard();
        assert!(!guard.is_cancelled());
        server
            .cancel_all(Parameters(CancelAllParams::default()))
            .await
            .unwrap();
        assert!(guard.is_cancelled(), "cancel_all must flip outstanding guards");
    }
}
