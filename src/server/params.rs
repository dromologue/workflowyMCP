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
use crate::utils::lenient_int::{de_opt_string_or_int, de_string_or_int};


#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Search for nodes in Workflowy by text")]
pub struct SearchNodesParams {
    #[schemars(description = "Text query to search for in node names and descriptions")]
    pub query: String,
    #[schemars(description = "Maximum number of results to return (default: 20)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_results: Option<usize>,
    #[schemars(description = "Parent node ID to scope the search under")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to search (default: 3). Increase for deeper searches in large trees")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[schemars(description = "Serve the query from the persistent name index instead of walking the tree. O(1) lookups, no walk-budget timeouts. Token-AND match over node names AND descriptions (every whitespace-delimited query term must appear, in any order); descriptions of nodes not walked since the last index rebuild may be absent, so a live walk remains authoritative. Defaults to false — a live walk; set true after `build_name_index` to bypass the 20 s subtree-fetch budget on huge subtrees.")]
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
    /// Required, explicit destination (2026-06-16 host-coercion hardening).
    /// Pass the empty string `""` for the *deliberate* "workspace root"
    /// choice; pass a UUID / short hash to scope. Omitting the field or
    /// passing `null` is REJECTED at the wire with a field-named error —
    /// previously `null`/omit silently meant root, which let a host that
    /// stripped or coerced the parameter strand a write at the root
    /// undetected. The success message names the resolved parent so the
    /// caller can audit placement.
    #[schemars(description = "Required parent node ID (UUID or short hash). Pass empty string \"\" for workspace root; omitting or null is rejected.")]
    pub parent_id: NodeId,
    #[schemars(description = "Priority (position) among siblings. Lower = higher position")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub priority: Option<i32>,
    /// Optional best-effort idempotency key. Supply a stable, caller-generated
    /// token (e.g. a UUID) to make a retry of THIS create safe: if the same
    /// key was already used for a successful create within the server's
    /// retention window, the call returns the original node instead of writing
    /// a duplicate. Covers retry-after-success and retry-after-fail-before-write;
    /// does NOT cover an ambiguous timeout after the write was sent (the server
    /// never saw the success to record it) — there, read back before retrying.
    /// Server-process-scoped: not shared with the `wflow-do` CLI (a one-shot
    /// process has no prior call to dedupe against).
    #[schemars(description = "Optional best-effort idempotency key (a stable caller-generated token). A repeated key replays the original create instead of writing a duplicate, within the server's retention window. Does not cover an ambiguous post-write timeout — read back before retrying in that case.")]
    pub idempotency_key: Option<String>,
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
    /// Optional name-echo guard. When supplied, the server fetches the
    /// resolved node and refuses the delete unless its current name (trimmed)
    /// equals this string. Defends the irreversible-delete path against a
    /// host that coerces a null/placeholder `node_id` to a plausible-but-
    /// unintended UUID: the coerced node's name won't match the echo, so the
    /// delete is refused with a typed error instead of destroying the wrong
    /// node. Omit to skip the check (back-compatible).
    #[serde(default)]
    #[schemars(description = "Optional safety echo: the current name of the node you intend to delete. If set and it does not match the resolved node's name, the delete is refused. Use this on every delete where the node_id was resolved indirectly.")]
    pub expect_name: Option<String>,
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_results: Option<usize>,
    #[schemars(description = "Parent node ID to scope the search under")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Insert content as hierarchical nodes from indented text")]
pub struct InsertContentParams {
    /// Parent node ID. Omit OR set to `null` to insert at the workspace
    /// root — both behave the same. Failure-report 2026-05-03: until
    /// Required, explicit destination (2026-06-16 host-coercion hardening).
    /// Pass the empty string `""` for the deliberate "workspace root"
    /// choice; pass a UUID / short hash to scope. Omitting or `null` is
    /// REJECTED at the wire with a field-named error.
    ///
    /// History: 2026-05-04 this field was relaxed from a non-optional
    /// `NodeId` to `Option<NodeId>` because a caller passing `null` hit a
    /// schema error while `create_node` et al. accepted null — the
    /// asymmetry drove a low success rate. The 2026-06-16 hardening
    /// re-tightens ALL four write tools together (create_node,
    /// batch_create_nodes, insert_content, create_mirror) to required
    /// `NodeId` with `""`-means-root, so the surfaces stay symmetric AND
    /// the null/stripped-parameter misroute is closed.
    #[schemars(description = "Required parent node ID to insert content under (UUID or short hash). Pass empty string \"\" for workspace root; omitting or null is rejected.")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub selection: Option<usize>,
    #[schemars(description = "Parent node ID to scope the search under")]
    pub parent_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub selection: Option<usize>,
    #[schemars(description = "Insert position: 'top' or 'bottom' (default: 'bottom')")]
    pub position: Option<String>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Daily review: overdue items, upcoming deadlines, and recent changes in one call")]
pub struct DailyReviewParams {
    #[schemars(description = "Optional root node ID to scope the review")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Max overdue items to return (default: 10)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub overdue_limit: Option<usize>,
    #[schemars(description = "Days ahead to look for upcoming items (default: 7)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub upcoming_days: Option<usize>,
    #[schemars(description = "Days back to look for recent changes (default: 1)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub recent_days: Option<usize>,
    #[schemars(description = "Max pending todos to return (default: 20)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub pending_limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Get recently modified nodes within a time window")]
pub struct GetRecentChangesParams {
    #[schemars(description = "Number of days to look back (default: 7)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub days: Option<usize>,
    #[schemars(description = "Optional root node ID to scope the search")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Include completed items (default: true)")]
    pub include_completed: Option<bool>,
    #[schemars(description = "Maximum results (default: 50)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "List items with upcoming due dates")]
pub struct ListUpcomingParams {
    #[schemars(description = "Days ahead to look (default: 14)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub days: Option<usize>,
    #[schemars(description = "Optional root node ID to scope the search")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Include items without due dates (default: false)")]
    pub include_no_due_date: Option<bool>,
    #[schemars(description = "Maximum results (default: 50)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub recently_modified_days: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Find all nodes that contain a Workflowy link to a given node")]
pub struct FindBacklinksParams {
    #[schemars(description = "The node ID to find backlinks for")]
    pub node_id: NodeId,
    #[schemars(description = "Maximum results (default: 50)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 3)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub limit: Option<usize>,
    #[schemars(description = "Maximum tree depth to search (default: 5)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub limit: Option<usize>,
    #[schemars(description = "Only return entries finished at or after this unix-millis timestamp")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    /// Required, explicit destination (2026-06-16 host-coercion hardening).
    /// Pass empty string `""` for workspace root; UUID / short hash to
    /// scope. Omitting or `null` is rejected at the wire with a
    /// field-named error.
    #[schemars(description = "Required parent node ID (UUID or short hash). Pass empty string \"\" for workspace root; omitting or null is rejected.")]
    pub parent_id: NodeId,
    #[schemars(description = "Optional priority/sort key (lower sorts earlier)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
#[schemars(description = "One read operation in a read_batch call.")]
pub struct ReadBatchOpParams {
    #[schemars(description = "Operation kind: get_node | list_children | get_subtree")]
    pub op: String,
    /// Required for `get_node` and `get_subtree`. For `list_children` the
    /// field is optional / nullable — omit (or pass `null`) to list the
    /// workspace root's top-level nodes, matching the bare `list_children`
    /// semantics.
    #[schemars(description = "Target node ID (UUID or short hash). Required for get_node and get_subtree; optional for list_children (omit/null = workspace root).")]
    pub node_id: Option<NodeId>,
    #[schemars(description = "Maximum depth to traverse (get_subtree only; defaults to 5).")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Run many reads in one call (get_node / list_children / get_subtree). Operations are dispatched in input order; results carry per-operation status. The operations-array wrapper inherits the same host-encoding resilience that batch_create_nodes already gives writes — a UUID inside an operation object survives the same hosts that strip bare-string node_id parameters.")]
pub struct ReadBatchParams {
    #[schemars(description = "List of read operations to apply, in order.")]
    pub operations: Vec<ReadBatchOpParams>,
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub priority: Option<i32>,
    /// Optional name-echo guard for `delete` ops only (ignored otherwise).
    /// Mirrors `delete_node.expect_name`: when set, the delete step refuses
    /// unless the target's current name matches.
    #[serde(default)]
    #[schemars(description = "For delete ops: optional safety echo of the node's current name. If set and it does not match, the delete (and the whole transaction) is refused and rolled back.")]
    pub expect_name: Option<String>,
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
#[schemars(description = "Place a set of nodes in a specific order under a given parent. Each entry in `node_ids` becomes the next sibling under `parent_id`, in the order given. Nodes not currently under `parent_id` are reparented as a side effect — the call is a reorder primitive built on `move_node`, not a sibling-only assertion. The orchestration walks the desired list in reverse and issues `priority=0` for each move so the final state is the requested order at the head of the parent's children, regardless of how many other siblings the parent already has. Workflowy normalises priorities after each move; the reverse-priority-0 trick avoids the self-fighting batched-reorder problem the naive forward `priority=0,1,2,…` loop runs into.")]
pub struct ReorderNodesParams {
    #[schemars(description = "Parent under which the listed nodes will be ordered. Required: the call refuses to guess. Pass a UUID or 12-char short hash.")]
    pub parent_id: NodeId,
    #[schemars(description = "Desired order, head-first. The 0th entry will be the first child of `parent_id`; the last entry will appear after every other id in this list (other siblings of `parent_id` are pushed after the listed nodes). Must be non-empty; duplicates are rejected; capped at `defaults::MAX_REORDER_NODES` (200).")]
    pub node_ids: Vec<NodeId>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Cheap incremental check: did this node change after the given timestamp?")]
pub struct SinceParams {
    #[schemars(description = "Node ID (full UUID or 12-char short hash)")]
    pub node_id: NodeId,
    #[schemars(description = "Threshold timestamp in unix milliseconds. Returns whether node.last_modified >= this value.")]
    #[serde(deserialize_with = "de_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Maximum results (default 50)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
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
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Create a convention-based mirror of a canonical node under a new parent. Workflowy's public REST API does not expose native mirror creation, so this tool implements the documented `mirror_of:` / `canonical_of:` note convention that `audit_mirrors` already understands: a new node is created under target_parent with the same name as the canonical, its description carries `mirror_of: <canonical_uuid>`, and (optionally, when `pillar` is supplied) the canonical's description gains `canonical_of: <pillar>` if it lacks one. Edits to the canonical do NOT propagate to the mirror — the link is structural and human-curated, not live. Use this when you want a single canonical surfaced from multiple places in the workspace and want `audit_mirrors` to surface drift. The response carries `scope_resolved` so callers can verify what the server actually targeted; pass `dry_run=true` to preview the resolved canonical/target without writing.")]
pub struct CreateMirrorParams {
    #[schemars(description = "UUID or short hash of the canonical node to mirror. The mirror's name is copied verbatim from this node at creation time.")]
    pub canonical_node_id: NodeId,
    /// Required, explicit destination (2026-06-16 host-coercion hardening).
    /// Pass empty string `""` for workspace root; UUID / short hash to
    /// scope. Omitting or `null` is rejected at the wire with a
    /// field-named error. The response's `scope_resolved` field names what
    /// the server actually resolved.
    #[schemars(description = "Required parent under which the mirror should appear (UUID or short hash). Pass empty string \"\" for workspace root; omitting or null is rejected.")]
    pub target_parent_id: NodeId,
    #[schemars(description = "Optional priority/sort key for the mirror among its siblings (lower = earlier)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub priority: Option<i32>,
    #[schemars(description = "Optional pillar token (opaque, e.g. 'lead', 'build') to write to the canonical's `canonical_of:` marker if it lacks one. Skipped when omitted; if the canonical already has a canonical_of marker it is never overwritten.")]
    pub pillar: Option<String>,
    /// 2026-05-09 failure-report addition: when batching mirror passes
    /// across a synthesis the caller wants to verify resolution before
    /// committing eight writes. `dry_run=true` resolves canonical_id +
    /// target_parent_id, looks up the canonical's name, decides whether
    /// the optional pillar would annotate, and returns that envelope —
    /// no mutation, no side effects.
    #[schemars(description = "Resolve canonical and target without writing. Default false. When true the server returns the would-be mirror name, target_parent_id, and pillar-annotation decision; nothing is created and no canonical is annotated. Pair with the production call once the resolved scope is verified.")]
    pub dry_run: Option<bool>,
}

/// Parameters for `audit_mirrors`. Defaults `root_id` to the user's
/// Distillations subtree (the only place the mirror convention is
/// applied today). `max_depth` is bounded by the standard subtree-walk
/// budget; deeper trees return partial results with a truncation flag.
#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Audit canonical_of/mirror_of markers across a subtree per the wflow Mirror Discipline convention")]
pub struct AuditMirrorsParams {
    #[schemars(description = "Root of the audit. Default when omitted: the WORKFLOWY_REVIEW_ROOT env node (errors asking for an explicit root_id if that env var is unset).")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth (default 8)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Chunked walk: list root's direct children and walk each separately, then aggregate. Avoids the 10 000-node walk cap on large subtrees. Defaults to true when root_id is omitted (the default env scope), false when an explicit root is supplied. Pass true to force chunking for any scope; pass false to opt out.")]
    pub chunked: Option<bool>,
    #[schemars(description = "Widen canonical resolution beyond the walked scope by consulting the persistent name index. Mirror Discipline is built around cross-pillar references, so a canonical living outside the walk is the normal case rather than a fault. Defaults to true. Set false to restore the legacy in-scope-only classifier (any cross-scope mirror will then classify as BROKEN).")]
    pub cross_scope_resolve: Option<bool>,
}

/// Parameters for `review`. Same scoping shape as `audit_mirrors`.
/// `days_stale` defaults to 90 — cross-pillar concept maps with no
/// edits in that window are surfaced for re-read.
#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Surface revisit-due, multi-pillar, stale, and recently-cited content under a subtree")]
pub struct ReviewParams {
    #[schemars(description = "Root of the review. Default when omitted: the WORKFLOWY_REVIEW_ROOT env node (errors asking for an explicit root_id if that env var is unset).")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth (default 8)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Days without edit before a cross-pillar map is reported stale (default 90)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub days_stale: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Populate the opportunistic name index by walking a subtree")]
pub struct BuildNameIndexParams {
    #[schemars(description = "Root node to start the walk from. Omit with allow_root_scan=true to walk the workspace root (expensive on large trees)")]
    pub root_id: Option<NodeId>,
    #[schemars(description = "Maximum tree depth to walk (default: 10)")]
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Opt in to an unscoped walk when root_id is omitted. Refused by default")]
    pub allow_root_scan: Option<bool>,
}

#[cfg(test)]
mod tests {
    //! Wire-level pins for the 2026-06-24 Cowork-host stringified-integer
    //! quirk: integer params arrive serialised as JSON strings. Every integer
    //! MCP parameter routes through `lenient_int` so `"3100"` and `3100` reach
    //! the handler as the same value, and `""` / `null` / omission map to
    //! `None`. These tests exercise the actual param structs (not the helper
    //! in isolation) so a field that loses its `deserialize_with` attribute
    //! fails the build.
    use super::*;

    #[test]
    fn create_node_priority_accepts_stringified_int() {
        // The exact incident shape: create_node priority="3100".
        let from_str: CreateNodeParams =
            serde_json::from_str(r#"{"name":"n","parent_id":"","priority":"3100"}"#).unwrap();
        let from_int: CreateNodeParams =
            serde_json::from_str(r#"{"name":"n","parent_id":"","priority":3100}"#).unwrap();
        assert_eq!(from_str.priority, Some(3100));
        assert_eq!(from_str.priority, from_int.priority);
    }

    #[test]
    fn get_subtree_max_depth_accepts_stringified_int() {
        // The exact incident shape: get_subtree max_depth="2".
        let from_str: GetSubtreeParams =
            serde_json::from_str(r#"{"node_id":"abc","max_depth":"2"}"#).unwrap();
        let from_int: GetSubtreeParams =
            serde_json::from_str(r#"{"node_id":"abc","max_depth":2}"#).unwrap();
        assert_eq!(from_str.max_depth, Some(2));
        assert_eq!(from_str.max_depth, from_int.max_depth);
    }

    #[test]
    fn optional_int_empty_string_null_and_omitted_map_to_none() {
        let empty: GetSubtreeParams =
            serde_json::from_str(r#"{"node_id":"abc","max_depth":""}"#).unwrap();
        let null: GetSubtreeParams =
            serde_json::from_str(r#"{"node_id":"abc","max_depth":null}"#).unwrap();
        let omitted: GetSubtreeParams = serde_json::from_str(r#"{"node_id":"abc"}"#).unwrap();
        assert_eq!(empty.max_depth, None);
        assert_eq!(null.max_depth, None);
        assert_eq!(omitted.max_depth, None);
    }

    #[test]
    fn required_timestamp_accepts_stringified_int() {
        // SinceParams::timestamp_unix_ms is the one required (non-Option) int.
        let from_str: SinceParams =
            serde_json::from_str(r#"{"node_id":"abc","timestamp_unix_ms":"1700000000000"}"#)
                .unwrap();
        let from_int: SinceParams =
            serde_json::from_str(r#"{"node_id":"abc","timestamp_unix_ms":1700000000000}"#).unwrap();
        assert_eq!(from_str.timestamp_unix_ms, 1_700_000_000_000);
        assert_eq!(from_str.timestamp_unix_ms, from_int.timestamp_unix_ms);
    }

    #[test]
    fn lenient_int_does_not_weaken_deny_unknown_fields() {
        // Adding #[serde(default, ...)] to int fields must not open the struct
        // to unknown fields — deny_unknown_fields still applies.
        let err = serde_json::from_str::<GetSubtreeParams>(
            r#"{"node_id":"abc","max_depth":"2","bogus":1}"#,
        );
        assert!(err.is_err(), "deny_unknown_fields must still reject typos");
    }
}

