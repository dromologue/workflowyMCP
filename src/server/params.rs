//! Parameter structs for every MCP tool the server exposes.
//!
//! Pulled out of `server/mod.rs` on 2026-05-03 as part of the
//! architecture-review file split — the parameter definitions formed a
//! 564-line slab between the wrapper-trait impl and the first handler,
//! and putting them in their own file makes the tool surface readable
//! without scrolling through schema declarations.
//!
//! **Wrapper rule**: every `#[tool]`-annotated handler in `server/mod.rs`
//! takes its argument as `Parameters<XxxParams>` (where `Parameters<T>`
//! is the wrapper defined in the parent module). The `rmcp-macros 0.16`
//! `#[tool]` proc macro auto-discovers the parameter type by matching
//! the literal identifier `Parameters` on the last path segment — see
//! the long-form discussion in `principles-architecture.md` Principle 8.
//! Renaming this wrapper away from `Parameters` produces silent empty-
//! properties schemas at the wire and is pinned by
//! `parameter_bearing_tools_publish_non_empty_input_schema_properties`.

use rmcp::schemars::JsonSchema;
use serde::Deserialize;

use crate::types::NodeId;


#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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
    /// Mirror of `find_node`'s gate. Brief 2026-05-02 reported
    /// `search_nodes` with `parent_id: null` walking workspace root and
    /// timing out at 20 s on any non-trivial tree, with no recovery
    /// path. The fix is the same gate `find_node` already has: refuse
    /// unscoped walks by default, require an explicit opt-in.
    #[schemars(description = "Opt in to scanning from the workspace root when parent_id is omitted. Disabled by default because unscoped searches on large trees time out; pass parent_id to scope or set this true to accept the full walk.")]
    pub allow_root_scan: Option<bool>,
    /// 2026-05-03 eval-run finding: searches scoped under big subtrees
    /// (Distillations grew past the 10 000-node walk cap) reliably time
    /// out at the 20 s budget, even with `parent_id` set. The recovery
    /// path is the same one `find_node` already exposes: when the
    /// persistent name index covers the target, query it directly and
    /// skip the API walk entirely. Match is name-only (description
    /// content needs the live walk), and the index must be populated
    /// — call `build_name_index` first if it isn't.
    #[schemars(description = "Serve the query from the persistent name index instead of walking the tree. O(1) lookups, no walk-budget timeouts. Match is name-only (description content needs the live walk). Defaults to false — a live walk; set true after `build_name_index` to bypass the 20 s subtree-fetch budget on huge subtrees.")]
    pub use_index: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Get a specific node by its ID")]
pub struct GetNodeParams {
    #[schemars(description = "The UUID of the node to retrieve")]
    pub node_id: NodeId,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Create a new node in Workflowy")]
pub struct CreateNodeParams {
    #[schemars(description = "The title/name of the new node")]
    pub name: String,
    #[schemars(description = "Optional description/note for the node")]
    pub description: Option<String>,
    #[schemars(description = "Parent node ID. Omit OR set to null to place the new node at the workspace root — both behave the same. The success message names the resolved parent so the caller can audit placement.")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Priority (position) among siblings. Lower = higher position")]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Edit an existing node's name or description")]
pub struct EditNodeParams {
    #[schemars(description = "The UUID of the node to edit")]
    pub node_id: NodeId,
    #[schemars(description = "New name for the node (leave empty to keep current)")]
    pub name: Option<String>,
    #[schemars(description = "New description for the node (leave empty to keep current)")]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Delete a node from Workflowy")]
pub struct DeleteNodeParams {
    #[schemars(description = "The UUID of the node to delete")]
    pub node_id: NodeId,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Toggle a node's completion state. Replaces the tag-based completion workaround documented in the wflow skill — `#done` markers no longer need to be applied as a substitute for native completion.")]
pub struct CompleteNodeParams {
    #[schemars(description = "The UUID of the node to mark complete or uncomplete")]
    pub node_id: NodeId,
    #[schemars(description = "Target completion state. Default true (mark complete); pass false to uncomplete a previously-completed node.")]
    pub completed: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Move a node to a new parent")]
pub struct MoveNodeParams {
    #[schemars(description = "The UUID of the node to move")]
    pub node_id: NodeId,
    #[schemars(description = "The UUID of the new parent node")]
    pub new_parent_id: NodeId,
    #[schemars(description = "Position among siblings in the new parent")]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Get all children of a node")]
pub struct GetChildrenParams {
    /// Parent node ID. Omit OR set to `null` to list the workspace root's
    /// top-level nodes. Both forms are accepted because some MCP clients
    /// send `{}` for missing fields and others send `{"node_id": null}`;
    /// previously the latter intermittently surfaced "Tool execution
    /// failed" while the former returned the root listing.
    ///
    /// Accepts the alias `parent_id` because every other tool in this
    /// server that scopes to a parent uses that name. Brief 2026-05-02
    /// pinned the "list_children intermittently returns workspace root"
    /// symptom to callers (correctly) sending `parent_id`, which serde
    /// silently dropped before `deny_unknown_fields` was wired in.
    #[serde(default, alias = "parent_id")]
    #[schemars(description = "Parent node ID (UUID or short hash). Omit OR pass null to list the workspace root's top-level nodes. Accepts `parent_id` as an alias.")]
    pub node_id: Option<NodeId>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Insert content as hierarchical nodes from indented text")]
pub struct InsertContentParams {
    #[schemars(description = "Parent node ID to insert content under")]
    pub parent_id: NodeId,
    #[schemars(description = "Content in 2-space indented text format. Each line becomes a node, indentation creates hierarchy")]
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Get the full tree under a node")]
pub struct GetSubtreeParams {
    #[schemars(description = "The UUID of the root node")]
    pub node_id: NodeId,
    #[schemars(description = "Maximum depth to traverse (default: unlimited)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Get project summary with stats, tags, and recent changes")]
pub struct GetProjectSummaryParams {
    #[schemars(description = "Root node ID of the project")]
    pub node_id: NodeId,
    #[schemars(description = "Include tag and assignee counts (default: true)")]
    pub include_tags: Option<bool>,
    #[schemars(description = "Days back for recently modified list (default: 7)")]
    pub recently_modified_days: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Find all nodes that contain a Workflowy link to a given node")]
pub struct FindBacklinksParams {
    #[schemars(description = "The node ID to find backlinks for")]
    pub node_id: NodeId,
    #[schemars(description = "Maximum results (default: 50)")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Copy a template node with {{variable}} substitution")]
pub struct CreateFromTemplateParams {
    #[schemars(description = "Template node ID to copy from")]
    pub template_node_id: NodeId,
    #[schemars(description = "Parent node ID to insert the copy under")]
    pub target_parent_id: NodeId,
    #[schemars(description = "Variables for {{key}} substitution as JSON object")]
    pub variables: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
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
    #[schemars(description = "Operation: 'complete', 'uncomplete', 'delete', 'add_tag', 'remove_tag'. `complete`/`uncomplete` toggle native Workflowy completion state via `client.set_completion`; same code path as the single-node `complete_node` tool.")]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Convert markdown to Workflowy-compatible indented text format")]
pub struct ConvertMarkdownParams {
    #[schemars(description = "Markdown content to convert")]
    pub markdown: String,
    #[schemars(description = "If true, only return stats without converting (default: false)")]
    pub analyze_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema, Default, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Quick diagnostic: verify Workflowy API reachability without a tree walk")]
pub struct HealthCheckParams {}

#[derive(Debug, Deserialize, JsonSchema, Default, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Extended diagnostic: liveness plus in-flight workload, last-request latency, tree-size estimate, and upstream rate-limit headers")]
pub struct WorkflowyStatusParams {}

#[derive(Debug, Deserialize, JsonSchema, Default, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Cancel any in-flight tree walks. Future calls are unaffected")]
pub struct CancelAllParams {}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Return recent tool invocations from the in-memory ring buffer")]
pub struct GetRecentToolCallsParams {
    #[schemars(description = "Maximum number of entries to return (default: 50, max bounded by buffer capacity)")]
    pub limit: Option<usize>,
    #[schemars(description = "Only return entries finished at or after this unix-millis timestamp")]
    pub since_unix_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "One create operation in a batch")]
pub struct BatchCreateOpParams {
    #[schemars(description = "Name (text content) of the new node")]
    pub name: String,
    #[schemars(description = "Optional note/description for the new node")]
    pub description: Option<String>,
    #[schemars(description = "Optional parent node ID (UUID or short hash). Omit to create at workspace root.")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Optional priority/sort key (lower sorts earlier)")]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Create many nodes in one call. Operations run with bounded concurrency; results are returned in input order with per-operation status.")]
pub struct BatchCreateNodesParams {
    #[schemars(description = "List of create operations to apply")]
    pub operations: Vec<BatchCreateOpParams>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "One operation inside a transaction. `op` is one of: create, edit, delete, move, complete, uncomplete.")]
pub struct TransactionOpParams {
    #[schemars(description = "Operation kind: create | edit | delete | move | complete | uncomplete")]
    pub op: String,
    /// For create: parent_id and name (required), description/priority (optional).
    /// For edit: node_id (required), name and/or description.
    /// For delete: node_id (required).
    /// For move: node_id, new_parent_id (required), priority (optional).
    /// For complete / uncomplete: node_id (required).
    #[schemars(description = "Node ID — required for edit, delete, and move operations")]
    pub node_id: Option<NodeId>,
    #[schemars(description = "Parent node ID — required for create, ignored for others")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Target parent ID — required for move operations")]
    pub new_parent_id: Option<NodeId>,
    #[schemars(description = "Name field — required for create, optional for edit")]
    pub name: Option<String>,
    #[schemars(description = "Description/note — optional for create and edit")]
    pub description: Option<String>,
    #[schemars(description = "Priority/sort key — optional for create and move")]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Apply a sequence of create/edit/delete/move operations with best-effort atomicity. Operations run sequentially (so dependencies resolve in order); on first failure the server replays inverse operations to roll back what already succeeded. True atomicity is not possible without upstream transaction support — this is a best-effort wrapper around per-op rollback.")]
pub struct TransactionParams {
    #[schemars(description = "Operations to apply, in execution order")]
    pub operations: Vec<TransactionOpParams>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Return the canonical hierarchical path from root to node")]
pub struct PathOfParams {
    #[schemars(description = "Node ID (full UUID or 12-char short hash)")]
    pub node_id: NodeId,
    #[schemars(description = "Maximum ancestors to walk (default 50)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Apply a single tag to many nodes in one call")]
pub struct BulkTagParams {
    #[schemars(description = "List of node IDs to tag")]
    pub node_ids: Vec<NodeId>,
    #[schemars(description = "Tag to apply (without leading #)")]
    pub tag: String,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Cheap incremental check: did this node change after the given timestamp?")]
pub struct SinceParams {
    #[schemars(description = "Node ID (full UUID or 12-char short hash)")]
    pub node_id: NodeId,
    #[schemars(description = "Threshold timestamp in unix milliseconds. Returns whether node.last_modified >= this value.")]
    pub timestamp_unix_ms: i64,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Find nodes that combine a tag with a path-prefix filter")]
pub struct FindByTagAndPathParams {
    #[schemars(description = "Tag to match (without leading #)")]
    pub tag: String,
    #[schemars(description = "Path prefix to match against the > -separated hierarchical path")]
    pub path_prefix: String,
    #[schemars(description = "Optional scope root; defaults to workspace root")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth (default 5)")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Maximum results (default 50)")]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Resolve a hierarchical path of node names to its UUID by walking each level — fast on huge trees because it costs one API call per path segment, not a full tree walk")]
pub struct NodeAtPathParams {
    #[schemars(description = "Path segments from the start parent to the target. E.g. ['Areas', 'Personal', 'Opportunities', 'Nedbank']. Each segment is matched case-insensitively against children's names with HTML stripped. Whitespace is trimmed. Use this when you know where a node lives but not its UUID — far faster than search_nodes on large workspaces.")]
    pub path: Vec<String>,
    #[schemars(description = "Optional starting parent. Default: workspace root. Pass a known UUID or short hash to skip leading segments and shave API calls.")]
    pub start_parent_id: Option<NodeId>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Resolve a Workflowy internal link or short hash to its full node info — purpose-built for the 'I have a Workflowy URL, give me the node' workflow")]
pub struct ResolveLinkParams {
    #[schemars(description = "Workflowy URL fragment or short hash. Accepts: full URLs ('https://workflowy.com/#/c4ae1944b67e'), bare 12-char URL-suffix hashes ('c4ae1944b67e'), 8-char doc-form prefixes ('c4ae1944'), or full UUIDs (in which case it just looks up node info).")]
    pub link: String,
    #[schemars(description = "Optional parent path to scope the resolution walk under, as a list of node names from root. E.g. ['Areas', 'Personal']. Resolved via node_at_path internally; the resolution walk runs only inside that subtree, which is dramatically faster than a full-workspace walk on big trees.")]
    pub search_parent_path: Option<Vec<String>>,
    #[schemars(description = "Alternative to search_parent_path: a parent UUID or short hash directly. Ignored if search_parent_path is also provided.")]
    pub search_parent_id: Option<NodeId>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Export a subtree as OPML, Markdown, or JSON for backup or external processing")]
pub struct ExportSubtreeParams {
    #[schemars(description = "Root of the subtree to export")]
    pub node_id: NodeId,
    #[schemars(description = "Output format: opml | markdown | json")]
    pub format: String,
    #[schemars(description = "Maximum tree depth (default 10)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Stub for native Workflowy mirror creation. Workflowy's public REST surface does not expose mirror creation; this tool returns an informative error so callers don't silently fall back to a 'mirror_of: <uuid>' note convention.")]
pub struct CreateMirrorParams {
    #[schemars(description = "Canonical node to mirror")]
    pub canonical_node_id: NodeId,
    #[schemars(description = "Parent under which the mirror should appear")]
    pub target_parent_id: NodeId,
    #[schemars(description = "Optional priority/sort key")]
    pub priority: Option<i32>,
}

/// Parameters for `audit_mirrors`. Defaults `root_id` to the user's
/// Distillations subtree (the only place the mirror convention is
/// applied today). `max_depth` is bounded by the standard subtree-walk
/// budget; deeper trees return partial results with a truncation flag.
#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Audit canonical_of/mirror_of markers across a subtree per the wflow Mirror Discipline convention")]
pub struct AuditMirrorsParams {
    #[schemars(description = "Root of the audit (default: Distillations 7e351f77-c7b4-4709-86a7-ea6733a63171)")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth (default 8)")]
    pub max_depth: Option<usize>,
}

/// Parameters for `review`. Same scoping shape as `audit_mirrors`.
/// `days_stale` defaults to 90 — cross-pillar concept maps with no
/// edits in that window are surfaced for re-read.
#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Surface revisit-due, multi-pillar, stale, and recently-cited content under a subtree")]
pub struct ReviewParams {
    #[schemars(description = "Root of the review (default: Distillations 7e351f77-c7b4-4709-86a7-ea6733a63171)")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth (default 8)")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Days without edit before a cross-pillar map is reported stale (default 90)")]
    pub days_stale: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Populate the opportunistic name index by walking a subtree")]
pub struct BuildNameIndexParams {
    #[schemars(description = "Root node to start the walk from. Omit with allow_root_scan=true to walk the workspace root (expensive on large trees)")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to walk (default: 10)")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Opt in to an unscoped walk when root_id is omitted. Refused by default")]
    pub allow_root_scan: Option<bool>,
}

