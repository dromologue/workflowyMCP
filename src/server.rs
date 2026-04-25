//! MCP Server implementation using rmcp
//! Implements ServerHandler with tool_router for all Workflowy tools

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{ErrorCode, *},
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

use crate::api::{BatchCreateOp, FetchControls, SubtreeFetch, TruncationReason, WorkflowyClient};
use crate::defaults;
use crate::types::{WorkflowyNode, NodeId};
use crate::utils::cache::NodeCache;
use crate::utils::cancel::CancelRegistry;
use crate::utils::date_parser::{parse_due_date_from_node, is_overdue};
use crate::utils::name_index::NameIndex;
use crate::utils::node_paths::{build_node_path_with_map, build_node_map};
use crate::utils::op_log::OpLog;
use crate::utils::subtree::{is_todo, is_completed};
use crate::utils::tag_parser::parse_node_tags;
use crate::validation::validate_node_id;
use std::time::{Duration, Instant};

/// Compute a per-tool health summary from the op log: for each tool that
/// has been called at least once, report `total`, `ok`, `err`, and a
/// `status` of `"healthy"` (≥75% ok in the recent window),
/// `"degraded"` (50–75% ok), or `"failing"` (<50% ok). Bounded by the
/// 200 most recent entries so a flood of failures gets noticed quickly
/// without ancient history skewing the picture.
///
/// Brief P4 #3: callers checking whether a heavy query is safe can read
/// this field instead of probing with multiple call types.
fn per_tool_health(log: &OpLog) -> serde_json::Value {
    use std::collections::BTreeMap;
    let recent = log.recent(200, None);
    let mut by_tool: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for entry in recent {
        let counts = by_tool.entry(entry.tool.clone()).or_insert((0, 0));
        match entry.status {
            crate::utils::OpStatus::Ok => counts.0 += 1,
            crate::utils::OpStatus::Err => counts.1 += 1,
        }
    }
    let mut out = serde_json::Map::new();
    for (tool, (ok, err)) in by_tool {
        let total = ok + err;
        let ok_rate = if total == 0 { 1.0 } else { ok as f64 / total as f64 };
        let status = if ok_rate >= 0.75 {
            "healthy"
        } else if ok_rate >= 0.5 {
            "degraded"
        } else {
            "failing"
        };
        out.insert(
            tool,
            serde_json::json!({
                "total": total,
                "ok": ok,
                "err": err,
                "ok_rate": (ok_rate * 100.0).round() / 100.0,
                "status": status,
            }),
        );
    }
    serde_json::Value::Object(out)
}

/// Build a structured `McpError` for a tool failure. Picks a JSON-RPC error
/// code based on the underlying error class and attaches a `data` payload
/// with `{operation, node_id, hint, error}` so even minimal clients can
/// extract the proximate cause when their UI renders only the generic
/// "tool failed" surface. Supersedes the previous direct calls to
/// `McpError::internal_error(format!("Failed: {}", e), None)` which were
/// being truncated to "Tool execution failed" by some clients.
fn tool_error(operation: &str, node_id: Option<&str>, err: impl std::fmt::Display) -> McpError {
    let err_str = err.to_string();
    let lower = err_str.to_lowercase();
    let (code, hint) = if lower.contains("404") || lower.contains("not found") {
        (
            ErrorCode::RESOURCE_NOT_FOUND,
            "node may not yet exist (propagation lag), or has been deleted",
        )
    } else if lower.contains("cancelled") {
        (
            ErrorCode::INTERNAL_ERROR,
            "cancelled by cancel_all — the call was preempted, retry",
        )
    } else if lower.contains("timeout") || lower.contains("timed out") {
        (
            ErrorCode::INTERNAL_ERROR,
            "upstream timeout — narrow scope or wait for load to drop",
        )
    } else if lower.contains("api error 5") {
        (
            ErrorCode::INTERNAL_ERROR,
            "Workflowy backend error — try again shortly",
        )
    } else if lower.contains("401") || lower.contains("403") || lower.contains("unauthor") {
        (
            ErrorCode::INTERNAL_ERROR,
            "auth failure — check WORKFLOWY_API_KEY",
        )
    } else {
        (ErrorCode::INTERNAL_ERROR, "see data field for details")
    };
    let data = serde_json::json!({
        "operation": operation,
        "node_id": node_id,
        "hint": hint,
        "error": err_str,
    });
    McpError::new(
        code,
        format!("{}: {}", operation, err_str),
        Some(data),
    )
}

/// Wrap a handler body so the call is recorded in the per-call op log.
/// Use as the outermost expression of every tool handler:
///
/// ```ignore
/// async fn foo(&self, Parameters(params): Parameters<FooParams>) -> Result<CallToolResult, McpError> {
///     record_op!(self, "foo", params, {
///         // existing body, including `?` early returns
///     })
/// }
/// ```
///
/// The macro hashes the params, opens a recorder, runs the body inside
/// an async block (so `?` returns from the block, not the outer
/// function), then finishes the recorder with Ok/Err before returning.
macro_rules! record_op {
    ($self:ident, $tool:literal, $params:ident, $body:block) => {{
        let __pj = serde_json::to_value(&$params).unwrap_or(serde_json::Value::Null);
        let __recorder = $self.op_log.record($tool, &__pj);
        let __result: Result<CallToolResult, McpError> = async move $body.await;
        match &__result {
            Ok(_) => __recorder.finish_ok(),
            Err(e) => __recorder.finish_err(e.to_string()),
        }
        __result
    }};
}

/// Validate a node_id parameter, returning McpError on failure. The
/// underlying validator rejects the empty string, so any call where the
/// serde layer has defaulted a missing `node_id` to `""` is caught here.
/// Also emits a `warn!` line on failure so an assistant scraping the
/// stderr log can correlate validation errors with the calling tool.
///
/// As of Pass 4 this also accepts a 12-char hex short-hash form (the
/// trailing 12 chars of a UUID, as used in Workflowy URLs) — but only
/// the validator-level check. Handlers that need to use the resolved
/// full UUID for an API call should go through
/// [`WorkflowyMcpServer::resolve_node_ref`] instead.
fn check_node_id(id: impl AsRef<str>) -> Result<(), McpError> {
    let id_ref = id.as_ref();
    if is_short_hash(id_ref) {
        // A short hash on its own is fine at the validator boundary; the
        // resolver inside the handler will turn it into a full UUID.
        return Ok(());
    }
    match validate_node_id(id_ref) {
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!(
                node_id_len = id_ref.len(),
                node_id_preview = %id_ref.chars().take(16).collect::<String>(),
                error = %e,
                "Rejected node_id at handler boundary; check the calling MCP client for a missing/null id"
            );
            Err(McpError::invalid_params(e.to_string(), None))
        }
    }
}

/// Heuristic: is `s` a 12-char hex short hash? Used to short-circuit
/// `check_node_id` so callers can pass either form transparently.
fn is_short_hash(s: &str) -> bool {
    let stripped: String = s.chars().filter(|c| *c != '-').collect();
    stripped.len() == crate::utils::name_index::SHORT_HASH_LEN
        && stripped.chars().all(|c| c.is_ascii_hexdigit())
}

/// Render a subtree as nested Markdown bullets. Depth is determined
/// by following parent_id chains within the supplied node set, so the
/// output mirrors the actual tree shape regardless of the order
/// `nodes` was returned in.
fn render_subtree_markdown(nodes: &[WorkflowyNode], root_id: &str) -> String {
    use std::collections::HashMap;
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
        children_of: &std::collections::HashMap<String, Vec<&WorkflowyNode>>,
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
/// re-import this losslessly enough for backup/exchange. We escape
/// the four XML metacharacters and emit each node as a single-line
/// `<outline>` element.
fn render_subtree_opml(nodes: &[WorkflowyNode], root_id: &str) -> String {
    use std::collections::HashMap;
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
        children_of: &std::collections::HashMap<String, Vec<&WorkflowyNode>>,
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

/// RAII guard that bumps an in-flight counter on construction and
/// decrements on drop. Ensures `workflowy_status` reports an accurate
/// figure even if a handler aborts early.
struct WalkGuard {
    counter: Arc<std::sync::atomic::AtomicUsize>,
}

impl WalkGuard {
    fn new(counter: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for WalkGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Test-only shorthand: build a banner without a path. Production callers go
/// through [`truncation_banner_from_fetch`].
#[cfg(test)]
fn truncation_banner(truncated: bool, limit: usize) -> String {
    truncation_banner_with_reason(truncated, limit, None)
}

#[cfg(test)]
fn truncation_banner_with_reason(
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
) -> String {
    truncation_banner_full(truncated, limit, reason, None)
}

/// Full truncation banner including the path of the unfinished subtree, when
/// known. Callers that have a `SubtreeFetch` to hand should prefer
/// [`truncation_banner_from_fetch`] so the path computation is centralised.
fn truncation_banner_full(
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
    truncated_at_path: Option<&str>,
) -> String {
    if !truncated {
        return String::new();
    }
    let suffix = match truncated_at_path {
        Some(path) if !path.is_empty() => format!(" Walk stopped at: {}.", path),
        _ => String::new(),
    };
    match reason {
        Some(TruncationReason::Timeout) => format!(
            "⚠ subtree walk timed out before completion (budget {} ms). Results below reflect whatever was collected — retry with narrower parent_id/max_depth or raise the budget.{}\n\n",
            defaults::SUBTREE_FETCH_TIMEOUT_MS,
            suffix,
        ),
        Some(TruncationReason::Cancelled) => format!(
            "⚠ subtree walk was cancelled; results below are partial.{}\n\n",
            suffix,
        ),
        _ => format!(
            "⚠ subtree truncated at {} nodes — results below may be incomplete. Narrow parent_id or max_depth.{}\n\n",
            limit, suffix,
        ),
    }
}

/// Convenience: produce a banner from a [`SubtreeFetch`], resolving the
/// `truncated_at_node_id` against the fetched nodes to display a path.
fn truncation_banner_from_fetch(fetch: &SubtreeFetch) -> String {
    let path = fetch
        .truncated_at_node_id
        .as_deref()
        .map(|id| {
            crate::utils::node_paths::build_node_path(id, &fetch.nodes)
        })
        .filter(|p| !p.is_empty());
    truncation_banner_full(
        fetch.truncated,
        fetch.limit,
        fetch.truncation_reason,
        path.as_deref(),
    )
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
    /// Count of subtree walks currently in flight. Surfaced by
    /// `workflowy_status` so a caller deciding whether to launch a heavy
    /// query can see if the shared rate limiter is already busy.
    in_flight_walks: Arc<std::sync::atomic::AtomicUsize>,
    /// Best-effort estimate of total tree size: updated whenever a walk
    /// completes with a non-truncated count, surfaced by
    /// `workflowy_status`. A `0` means "no full walk has happened yet".
    tree_size_estimate: Arc<std::sync::atomic::AtomicUsize>,
    /// Per-call ring-buffer log: every tool invocation records start/end
    /// timestamps, params hash, and outcome. The assistant queries this
    /// via `get_recent_tool_calls` to self-diagnose hangs and
    /// unexpected returns within a session.
    op_log: OpLog,
}

// --- Parameter structs ---

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[schemars(description = "Get a specific node by its ID")]
pub struct GetNodeParams {
    #[schemars(description = "The UUID of the node to retrieve")]
    pub node_id: NodeId,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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
#[schemars(description = "Delete a node from Workflowy")]
pub struct DeleteNodeParams {
    #[schemars(description = "The UUID of the node to delete")]
    pub node_id: NodeId,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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
#[schemars(description = "Get all children of a node")]
pub struct GetChildrenParams {
    #[schemars(description = "The UUID of the parent node")]
    pub node_id: NodeId,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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
#[schemars(description = "Insert content as hierarchical nodes from indented text")]
pub struct InsertContentParams {
    #[schemars(description = "Parent node ID to insert content under")]
    pub parent_id: NodeId,
    #[schemars(description = "Content in 2-space indented text format. Each line becomes a node, indentation creates hierarchy")]
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[schemars(description = "Get the full tree under a node")]
pub struct GetSubtreeParams {
    #[schemars(description = "The UUID of the root node")]
    pub node_id: NodeId,
    #[schemars(description = "Maximum depth to traverse (default: unlimited)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[schemars(description = "Convert markdown to Workflowy-compatible indented text format")]
pub struct ConvertMarkdownParams {
    #[schemars(description = "Markdown content to convert")]
    pub markdown: String,
    #[schemars(description = "If true, only return stats without converting (default: false)")]
    pub analyze_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema, Default, serde::Serialize)]
#[schemars(description = "Quick diagnostic: verify Workflowy API reachability without a tree walk")]
pub struct HealthCheckParams {}

#[derive(Debug, Deserialize, JsonSchema, Default, serde::Serialize)]
#[schemars(description = "Extended diagnostic: liveness plus in-flight workload, last-request latency, tree-size estimate, and upstream rate-limit headers")]
pub struct WorkflowyStatusParams {}

#[derive(Debug, Deserialize, JsonSchema, Default, serde::Serialize)]
#[schemars(description = "Cancel any in-flight tree walks. Future calls are unaffected")]
pub struct CancelAllParams {}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[schemars(description = "Return recent tool invocations from the in-memory ring buffer")]
pub struct GetRecentToolCallsParams {
    #[schemars(description = "Maximum number of entries to return (default: 50, max bounded by buffer capacity)")]
    pub limit: Option<usize>,
    #[schemars(description = "Only return entries finished at or after this unix-millis timestamp")]
    pub since_unix_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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
#[schemars(description = "Create many nodes in one call. Operations run with bounded concurrency; results are returned in input order with per-operation status.")]
pub struct BatchCreateNodesParams {
    #[schemars(description = "List of create operations to apply")]
    pub operations: Vec<BatchCreateOpParams>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[schemars(description = "One operation inside a transaction. `op` is one of: create, edit, delete, move.")]
pub struct TransactionOpParams {
    #[schemars(description = "Operation kind: create | edit | delete | move")]
    pub op: String,
    /// For create: parent_id and name (required), description/priority (optional).
    /// For edit: node_id (required), name and/or description.
    /// For delete: node_id (required).
    /// For move: node_id, new_parent_id (required), priority (optional).
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
#[schemars(description = "Apply a sequence of create/edit/delete/move operations with best-effort atomicity. Operations run sequentially (so dependencies resolve in order); on first failure the server replays inverse operations to roll back what already succeeded. True atomicity is not possible without upstream transaction support — this is a best-effort wrapper around per-op rollback.")]
pub struct TransactionParams {
    #[schemars(description = "Operations to apply, in execution order")]
    pub operations: Vec<TransactionOpParams>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[schemars(description = "Return the canonical hierarchical path from root to node")]
pub struct PathOfParams {
    #[schemars(description = "Node ID (full UUID or 12-char short hash)")]
    pub node_id: NodeId,
    #[schemars(description = "Maximum ancestors to walk (default 50)")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[schemars(description = "Apply a single tag to many nodes in one call")]
pub struct BulkTagParams {
    #[schemars(description = "List of node IDs to tag")]
    pub node_ids: Vec<NodeId>,
    #[schemars(description = "Tag to apply (without leading #)")]
    pub tag: String,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
#[schemars(description = "Cheap incremental check: did this node change after the given timestamp?")]
pub struct SinceParams {
    #[schemars(description = "Node ID (full UUID or 12-char short hash)")]
    pub node_id: NodeId,
    #[schemars(description = "Threshold timestamp in unix milliseconds. Returns whether node.last_modified >= this value.")]
    pub timestamp_unix_ms: i64,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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
#[schemars(description = "Stub for native Workflowy mirror creation. Workflowy's public REST surface does not expose mirror creation; this tool returns an informative error so callers don't silently fall back to a 'mirror_of: <uuid>' note convention.")]
pub struct CreateMirrorParams {
    #[schemars(description = "Canonical node to mirror")]
    pub canonical_node_id: NodeId,
    #[schemars(description = "Parent under which the mirror should appear")]
    pub target_parent_id: NodeId,
    #[schemars(description = "Optional priority/sort key")]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema, serde::Serialize)]
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
            in_flight_walks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            tree_size_estimate: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            op_log: OpLog::new(),
        }
    }

    /// Test/inspection accessor for the operation log.
    #[cfg(test)]
    pub(crate) fn op_log(&self) -> &OpLog {
        &self.op_log
    }

    /// Build fetch controls that honour the server-wide cancel registry plus
    /// the configured subtree-walk timeout. All handlers go through this so
    /// cancellation and deadline enforcement are uniform.
    fn fetch_controls(&self) -> FetchControls {
        FetchControls::with_timeout(Duration::from_millis(defaults::SUBTREE_FETCH_TIMEOUT_MS))
            .and_cancel(self.cancel_registry.guard())
    }

    /// Resolve a `node_id` parameter to a full UUID string. Accepts either
    /// the canonical 32-hex-char UUID (with or without hyphens) or the
    /// 12-char short-hash form Workflowy uses in URLs. Short-hash
    /// resolution requires the name index to have seen the target node;
    /// a miss returns a pointed error rather than silently failing.
    ///
    /// Tools that need to call the Workflowy API with the resolved id
    /// should call this instead of treating the param string as the full
    /// UUID directly.
    fn resolve_node_ref(&self, raw: &str) -> Result<String, McpError> {
        if is_short_hash(raw) {
            match self.name_index.resolve_short_hash(raw) {
                Some(full) => Ok(full),
                None => Err(McpError::invalid_params(
                    format!(
                        "Short-hash '{}' is not in the name index. Pass the full UUID, or run build_name_index first to populate.",
                        raw
                    ),
                    None,
                )),
            }
        } else {
            // Already a full UUID (or we'll fail later in the API call).
            Ok(raw.to_string())
        }
    }

    /// Walk a subtree with the server's standard controls and push every
    /// visited node through the name index before returning. Keeps the tree
    /// walk and the opportunistic index population in one place so no handler
    /// can forget to feed the index. Also maintains the `in_flight_walks`
    /// counter and the best-effort `tree_size_estimate` for diagnostic
    /// surfaces.
    async fn walk_subtree(
        &self,
        root_id: Option<&str>,
        max_depth: usize,
    ) -> crate::error::Result<SubtreeFetch> {
        let _guard = WalkGuard::new(self.in_flight_walks.clone());
        let controls = self.fetch_controls();
        let fetch = self
            .client
            .get_subtree_with_controls(root_id, max_depth, defaults::MAX_SUBTREE_NODES, controls)
            .await?;
        self.name_index.ingest(&fetch.nodes);
        // A non-truncated, root-scoped walk gives us a fresh tree-size
        // sample. We deliberately do not lower the estimate on partial
        // walks — stale-but-larger is more useful than zero.
        if !fetch.truncated && root_id.is_none() {
            self.tree_size_estimate
                .store(fetch.nodes.len(), std::sync::atomic::Ordering::Relaxed);
        }
        Ok(fetch)
    }

    #[tool(description = "Search for nodes in Workflowy by text query. Returns matching nodes with their IDs, names, and paths. For large trees, use parent_id to scope the search and max_depth to control depth.")]
    async fn search_nodes(
        &self,
        Parameters(params): Parameters<SearchNodesParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "search_nodes", params, {
        let max_results = params.max_results.unwrap_or(20);
        let max_depth = params.max_depth.unwrap_or(3);
        info!(query = %params.query, max_results, max_depth, "Searching nodes");

        match self.walk_subtree(params.parent_id.as_deref(), max_depth).await {
            Ok(fetch) => {
                let banner = truncation_banner_from_fetch(&fetch);
                let query_lower = params.query.to_lowercase();
                let mut results: Vec<&WorkflowyNode> = fetch.nodes
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

                let result_text = format!("{}{}", banner, body);
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
        })
    }

    #[tool(description = "Get a specific Workflowy node by its ID. Returns the node's full details (name, description, tags) plus a depth-1 listing of its direct children — matching what list_children would return for the same ID. The children listing costs one extra HTTP call; use list_children directly when you don't need the parent metadata.")]
    async fn get_node(
        &self,
        Parameters(params): Parameters<GetNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "get_node", params, {
        info!(node_id = %params.node_id, "Getting node");
        check_node_id(&params.node_id)?;
        let resolved = self.resolve_node_ref(&params.node_id)?;

        // Fetch the node and its direct children in parallel — they are
        // independent API calls, and previously `get_node` returned an empty
        // `children: []` field that disagreed with `list_children`. Surfacing
        // the children alongside the parent removes that footgun without
        // forcing callers to make a second tool call.
        //
        // Both calls go through the propagation-retry path: Workflowy has
        // been observed to return a node ID via a parent's children listing
        // before the same ID is queryable directly. The retry waits up to
        // ~1.4 s total (200 + 400 + 800 ms) before giving up.
        let node_fut = self.client.get_node_with_propagation_retry(&resolved);
        let children_fut = self.client.get_children_with_propagation_retry(&resolved);
        let (node_res, children_res) = tokio::join!(node_fut, children_fut);

        let node = match node_res {
            Ok(n) => n,
            Err(e) => return Err(tool_error("get_node", Some(&resolved), e)),
        };

        // Children fetch is best-effort — surface the parent even if the
        // children call failed (e.g. node has no children, or a transient
        // error on the listing endpoint). The error is logged so the caller
        // can correlate against the empty list.
        let children: Vec<WorkflowyNode> = match children_res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(node_id = %resolved, error = %e, "get_node: child fetch failed; returning parent with empty children");
                Vec::new()
            }
        };

        // Feed the name index opportunistically — every fetched node is a
        // free index entry.
        if !children.is_empty() {
            self.name_index.ingest(&children);
        }
        self.name_index.ingest(std::slice::from_ref(&node));

        let payload = json!({
            "node": node,
            "children": children,
        });
        let json = serde_json::to_string_pretty(&payload).map_err(|e| {
            McpError::internal_error(format!("Serialization error: {}", e), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
        })
    }

    #[tool(description = "Create a new node in Workflowy. Optionally specify a parent node ID and position.")]
    async fn create_node(
        &self,
        Parameters(params): Parameters<CreateNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "create_node", params, {
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
        })
    }

    #[tool(description = "Edit an existing Workflowy node's name or description. At least one of name/description must be provided.")]
    async fn edit_node(
        &self,
        Parameters(params): Parameters<EditNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "edit_node", params, {
        info!(node_id = %params.node_id, "Editing node");
        check_node_id(&params.node_id)?;
        let resolved = self.resolve_node_ref(&params.node_id)?;

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
            .edit_node(&resolved, params.name.as_deref(), params.description.as_deref())
            .await
        {
            Ok(_) => {
                self.cache.invalidate_node(&resolved);
                self.name_index.invalidate_node(&resolved);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Updated node `{}`",
                    resolved
                ))]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to edit node: {}", e),
                None,
            )),
        }
        })
    }

    #[tool(description = "Delete a Workflowy node by its ID.")]
    async fn delete_node(
        &self,
        Parameters(params): Parameters<DeleteNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "delete_node", params, {
        info!(node_id = %params.node_id, "Deleting node");
        check_node_id(&params.node_id)?;
        let resolved = self.resolve_node_ref(&params.node_id)?;

        match self.client.delete_node(&resolved).await {
            Ok(_) => {
                self.cache.invalidate_node(&resolved);
                self.name_index.invalidate_node(&resolved);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Deleted node `{}`",
                    resolved
                ))]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to delete node: {}", e),
                None,
            )),
        }
        })
    }

    #[tool(description = "Move a node to a new parent in Workflowy.")]
    async fn move_node(
        &self,
        Parameters(params): Parameters<MoveNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "move_node", params, {
        info!(node_id = %params.node_id, new_parent = %params.new_parent_id, "Moving node");
        check_node_id(&params.node_id)?;
        check_node_id(&params.new_parent_id)?;
        let resolved_node = self.resolve_node_ref(&params.node_id)?;
        let resolved_parent = self.resolve_node_ref(&params.new_parent_id)?;

        // Capture the current parent before the move so we can invalidate its
        // children listing afterwards. A failed pre-read is not fatal — the
        // move itself still runs and we fall back to invalidating just the
        // node and the new parent, as the code used to.
        let old_parent_id = self
            .client
            .get_node(&resolved_node)
            .await
            .ok()
            .and_then(|n| n.parent_id);

        match self
            .client
            .move_node(&resolved_node, &resolved_parent, params.priority)
            .await
        {
            Ok(_) => {
                self.cache.invalidate_node(&resolved_node);
                self.cache.invalidate_node(&resolved_parent);
                if let Some(pid) = &old_parent_id {
                    if pid.as_str() != resolved_parent.as_str() {
                        self.cache.invalidate_node(pid);
                    }
                }
                self.name_index.invalidate_node(&resolved_node);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Moved node `{}` under `{}`",
                    resolved_node, resolved_parent
                ))]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to move node: {}", e),
                None,
            )),
        }
        })
    }

    #[tool(description = "List all children of a Workflowy node.")]
    async fn list_children(
        &self,
        Parameters(params): Parameters<GetChildrenParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "list_children", params, {
        info!(node_id = %params.node_id, "Getting children");
        check_node_id(&params.node_id)?;
        let resolved = self.resolve_node_ref(&params.node_id)?;

        match self.client.get_children_with_propagation_retry(&resolved).await {
            Ok(children) => {
                if children.is_empty() {
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Node `{}` has no children",
                        resolved
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
            Err(e) => Err(tool_error("list_children", Some(&resolved), e)),
        }
        })
    }

    #[tool(description = "Search for nodes by tag (e.g. #project, @person). Returns all nodes containing the specified tag. Use parent_id to scope and max_depth to control search depth.")]
    async fn tag_search(
        &self,
        Parameters(params): Parameters<TagSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "tag_search", params, {
        let max_results = params.max_results.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(3);
        info!(tag = %params.tag, max_depth, "Tag search");

        match self.walk_subtree(params.parent_id.as_deref(), max_depth).await {
            Ok(fetch) => {
                let banner = truncation_banner_from_fetch(&fetch);
                let tag_lower = params.tag.to_lowercase();
                let mut results: Vec<&WorkflowyNode> = fetch.nodes
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
        })
    }

    #[tool(description = "Insert hierarchical content under a parent node. Content uses 2-space indentation for hierarchy — each indent level creates a child of the node above it.")]
    async fn insert_content(
        &self,
        Parameters(params): Parameters<InsertContentParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "insert_content", params, {
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
        })
    }

    #[tool(description = "Get the full subtree under a node, showing the hierarchical structure. Use max_depth to limit traversal depth for large trees.")]
    async fn get_subtree(
        &self,
        Parameters(params): Parameters<GetSubtreeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "get_subtree", params, {
        let max_depth = params.max_depth.unwrap_or(5);
        info!(node_id = %params.node_id, max_depth, "Getting subtree");
        check_node_id(&params.node_id)?;

        match self.walk_subtree(Some(&params.node_id), max_depth).await {
            Ok(fetch) => {
                if fetch.nodes.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        format!("Node `{}` not found or has no descendants", params.node_id)
                    )]));
                }
                let banner = truncation_banner_from_fetch(&fetch);
                let root_name = fetch.nodes.first().map(|n| n.name.as_str()).unwrap_or("unknown").to_string();
                let total = fetch.nodes.len();
                let json = serde_json::to_string_pretty(&fetch.nodes).map_err(|e| {
                    McpError::internal_error(format!("Serialization error: {}", e), None)
                })?;
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "{}Subtree for '{}' ({} nodes):\n\n{}",
                    banner, root_name, total, json
                ))]))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to get subtree: {}", e),
                None,
            )),
        }
        })
    }

    // --- New tools required by wmanage skill ---

    #[tool(description = "Find a node by name. Supports exact, contains, and starts_with match modes. Returns node_id for use with other tools. Omitting parent_id triggers a root-of-tree walk, which is refused by default on large trees — pass allow_root_scan=true to opt in, or use_index=true to serve from the opportunistic name index. Use selection to disambiguate multiple matches.")]
    async fn find_node(
        &self,
        Parameters(params): Parameters<FindNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "find_node", params, {
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
            Ok(fetch) => {
                let search = params.name.to_lowercase();
                let matches: Vec<&WorkflowyNode> = fetch.nodes.iter().filter(|n| {
                    let name = n.name.to_lowercase();
                    match match_mode {
                        "contains" => name.contains(&search),
                        "starts_with" => name.starts_with(&search),
                        _ => name == search, // exact
                    }
                }).collect();

                let node_map = build_node_map(&fetch.nodes);
                let banner = truncation_banner_from_fetch(&fetch);
                let truncated_at_path = fetch.truncated_at_node_id.as_deref().map(|id| {
                    build_node_path_with_map(id, &node_map)
                });
                let truncated = fetch.truncated;
                let limit = fetch.limit;
                let truncation_reason = fetch.truncation_reason;

                if matches.is_empty() {
                    let mut result = json!({
                        "found": false,
                        "truncated": truncated,
                        "truncation_limit": limit,
                        "truncation_reason": truncation_reason.map(|r| r.as_str()),
                        "truncated_at_path": truncated_at_path,
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
                        "truncated_at_path": truncated_at_path,
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
                        "truncated_at_path": truncated_at_path,
                        "count": matches.len(),
                        "options": options,
                        "message": format!("Found {} matches for '{}'. Use selection parameter to choose.", matches.len(), params.name)
                    });
                    Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                }
            }
            Err(e) => Err(McpError::internal_error(format!("Failed to find node: {}", e), None)),
        }
        })
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
        record_op!(self, "smart_insert", params, {
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
        })
    }

    #[tool(description = "Daily review: get overdue items, upcoming deadlines, recent changes, and pending todos in one call. Use root_id to scope and max_depth to control depth.")]
    async fn daily_review(
        &self,
        Parameters(params): Parameters<DailyReviewParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "daily_review", params, {
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
        })
    }

    #[tool(description = "Get recently modified nodes within a time window.")]
    async fn get_recent_changes(
        &self,
        Parameters(params): Parameters<GetRecentChangesParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "get_recent_changes", params, {
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
        })
    }

    #[tool(description = "List overdue items (past due date, incomplete) sorted by most overdue first.")]
    async fn list_overdue(
        &self,
        Parameters(params): Parameters<ListOverdueParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "list_overdue", params, {
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
        })
    }

    #[tool(description = "List items with upcoming due dates, sorted by nearest deadline first.")]
    async fn list_upcoming(
        &self,
        Parameters(params): Parameters<ListUpcomingParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "list_upcoming", params, {
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
        })
    }

    #[tool(description = "Get project summary with stats, tag counts, assignee counts, and recently modified nodes.")]
    async fn get_project_summary(
        &self,
        Parameters(params): Parameters<GetProjectSummaryParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "get_project_summary", params, {
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
        })
    }

    // --- Remaining planned tools ---

    #[tool(description = "Find all nodes that contain a Workflowy link to the given node.")]
    async fn find_backlinks(
        &self,
        Parameters(params): Parameters<FindBacklinksParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "find_backlinks", params, {
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
        })
    }

    #[tool(description = "List todo items, optionally filtered by parent, status, or text query.")]
    async fn list_todos(
        &self,
        Parameters(params): Parameters<ListTodosParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "list_todos", params, {
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
        })
    }

    #[tool(description = "Deep-copy a node and its subtree to a new location.")]
    async fn duplicate_node(
        &self,
        Parameters(params): Parameters<DuplicateNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "duplicate_node", params, {
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
        })
    }

    #[tool(description = "Copy a template node with {{variable}} substitution in names and descriptions.")]
    async fn create_from_template(
        &self,
        Parameters(params): Parameters<CreateFromTemplateParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "create_from_template", params, {
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
        })
    }

    #[tool(description = "Apply an operation to all nodes matching a filter. Supports delete, add_tag, remove_tag. Use dry_run to preview. Note: complete/uncomplete are not yet implemented and will be rejected.")]
    async fn bulk_update(
        &self,
        Parameters(params): Parameters<BulkUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "bulk_update", params, {
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
        })
    }

    #[tool(description = "Convert markdown to Workflowy-compatible 2-space indented text format. Handles headers, lists, code blocks, blockquotes, and tables.")]
    async fn convert_markdown(
        &self,
        Parameters(params): Parameters<ConvertMarkdownParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "convert_markdown", params, {
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
        })
    }

    #[tool(description = "Quick diagnostic. Calls the Workflowy API with a short budget to confirm reachability and reports cache/name-index sizes. Sub-second regardless of tree size; use this to decide whether a larger tool call will succeed.")]
    async fn health_check(
        &self,
        Parameters(_params): Parameters<HealthCheckParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "health_check", _params, {
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
        })
    }

    #[tool(description = "Extended liveness probe: confirms Workflowy reachability AND surfaces in-flight walk count, last-request latency, tree-size estimate, and the most recent upstream rate-limit headers. Use this in preference to health_check when deciding whether to launch a heavy query — it tells you both whether the server is up and whether it is busy.")]
    async fn workflowy_status(
        &self,
        Parameters(_params): Parameters<WorkflowyStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "workflowy_status", _params, {
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
        let rate_limit = self.client.rate_limit_snapshot();
        let in_flight = self.in_flight_walks.load(std::sync::atomic::Ordering::Relaxed);
        let tree_estimate = self.tree_size_estimate.load(std::sync::atomic::Ordering::Relaxed);
        let per_tool = per_tool_health(&self.op_log);
        let result = json!({
            "status": if api_reachable { "ok" } else { "degraded" },
            "api_reachable": api_reachable,
            "latency_ms": elapsed_ms,
            "budget_ms": timeout.as_millis() as u64,
            "top_level_count": top_level_count,
            "in_flight_walks": in_flight,
            "last_request_ms": self.client.last_request_ms(),
            "tree_size_estimate": tree_estimate,
            "tree_size_estimate_known": tree_estimate > 0,
            "cache": {
                "node_count": cache_stats.node_count,
                "parent_count": cache_stats.parent_count,
            },
            "name_index": {
                "size": self.name_index.size(),
                "populated": self.name_index.is_populated(),
            },
            "rate_limit": {
                "remaining": rate_limit.remaining,
                "limit": rate_limit.limit,
                "reset_unix_seconds": rate_limit.reset_unix_seconds,
                "observed": rate_limit.remaining.is_some() || rate_limit.limit.is_some() || rate_limit.reset_unix_seconds.is_some(),
            },
            "per_tool_health": per_tool,
            "uptime_seconds": self.started_at.elapsed().as_secs(),
            "cancel_generation": self.cancel_registry.generation(),
            "error": error,
        });
        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
        })
    }

    #[tool(description = "Cancel every in-flight tree walk. Subsequent calls are unaffected. Use when a find_node / get_subtree / search is taking longer than the client is willing to wait.")]
    async fn cancel_all(
        &self,
        Parameters(_params): Parameters<CancelAllParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "cancel_all", _params, {
        let new_gen = self.cancel_registry.cancel_all();
        let result = json!({
            "status": "cancelled",
            "generation": new_gen,
            "message": "In-flight walks have been signalled to return partial results; new calls start fresh."
        });
        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
        })
    }

    #[tool(description = "Walk a subtree and populate the opportunistic name index. After this, find_node with use_index=true can answer lookups without touching the API. Walks are bounded by the standard subtree-fetch timeout and node-count cap, so large scopes may return partial results.")]
    async fn build_name_index(
        &self,
        Parameters(params): Parameters<BuildNameIndexParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "build_name_index", params, {
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
            Ok(SubtreeFetch { nodes, truncated, limit, truncation_reason, elapsed_ms, .. }) => {
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
        })
    }

    #[tool(description = "Create many nodes in one call. Operations are pipelined with bounded concurrency; results are returned in input order with per-operation Ok(node_id) or Err(message). Faster than sequential create_node calls for medium-to-large batches; not transactional.")]
    async fn batch_create_nodes(
        &self,
        Parameters(params): Parameters<BatchCreateNodesParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "batch_create_nodes", params, {
        if params.operations.is_empty() {
            return Err(McpError::invalid_params(
                "operations must not be empty".to_string(),
                None,
            ));
        }
        // Validate every node id eagerly so we don't waste round-trips
        // on a known-bad batch.
        for op in &params.operations {
            if let Some(pid) = &op.parent_id {
                check_node_id(pid)?;
            }
        }

        // Resolve short-hash parents up front so the batch sees full UUIDs.
        let resolved_ops: Vec<BatchCreateOp> = params
            .operations
            .into_iter()
            .map(|o| {
                let parent_id = match o.parent_id {
                    Some(pid) => Some(self.resolve_node_ref(&pid)?),
                    None => None,
                };
                Ok::<BatchCreateOp, McpError>(BatchCreateOp {
                    name: o.name,
                    description: o.description,
                    parent_id,
                    priority: o.priority,
                })
            })
            .collect::<Result<_, _>>()?;

        let parents_to_invalidate: std::collections::HashSet<String> = resolved_ops
            .iter()
            .filter_map(|o| o.parent_id.clone())
            .collect();

        let results = self.client.batch_create_nodes(resolved_ops).await;

        let mut succeeded = 0usize;
        let mut failed = 0usize;
        let entries: Vec<serde_json::Value> = results
            .into_iter()
            .enumerate()
            .map(|(i, r)| match r {
                Ok(created) => {
                    succeeded += 1;
                    self.name_index.ingest(&[WorkflowyNode {
                        id: created.id.clone(),
                        name: created.name.clone(),
                        parent_id: created.parent_id.clone(),
                        ..Default::default()
                    }]);
                    json!({
                        "index": i,
                        "ok": true,
                        "id": created.id,
                        "name": created.name,
                        "parent_id": created.parent_id,
                    })
                }
                Err(e) => {
                    failed += 1;
                    json!({ "index": i, "ok": false, "error": e.to_string() })
                }
            })
            .collect();

        for pid in parents_to_invalidate {
            self.cache.invalidate_node(&pid);
        }

        let payload = json!({
            "total": entries.len(),
            "succeeded": succeeded,
            "failed": failed,
            "results": entries,
        });
        Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
        })
    }

    #[tool(description = "Apply a sequence of create/edit/delete/move operations with best-effort atomicity. Operations run sequentially so dependencies resolve in order; on first failure the server replays inverse operations to roll back what already succeeded. Rollback is best-effort — not all operations are perfectly invertible (a deleted node's children cannot be perfectly recreated). True atomicity needs upstream transaction support which Workflowy does not expose; this wrapper is the closest you get without that.")]
    async fn transaction(
        &self,
        Parameters(params): Parameters<TransactionParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "transaction", params, {
        if params.operations.is_empty() {
            return Err(McpError::invalid_params("operations must not be empty".to_string(), None));
        }

        // Each entry: (description-of-completed-op, inverse-action)
        let mut applied: Vec<TxnInverse> = Vec::new();
        let mut applied_results: Vec<serde_json::Value> = Vec::new();

        for (idx, op) in params.operations.iter().enumerate() {
            let outcome = self.apply_txn_op(op).await;
            match outcome {
                Ok((summary, inverse)) => {
                    applied_results.push(json!({ "index": idx, "ok": true, "summary": summary }));
                    if let Some(inv) = inverse {
                        applied.push(inv);
                    }
                }
                Err(err) => {
                    // Roll back in reverse order. Each inverse failure is
                    // logged but does not abort the rollback — we want to
                    // get as much state back as possible.
                    let mut rollback_log: Vec<serde_json::Value> = Vec::new();
                    while let Some(inv) = applied.pop() {
                        match self.run_inverse(inv).await {
                            Ok(summary) => rollback_log.push(json!({ "ok": true, "summary": summary })),
                            Err(e) => rollback_log.push(json!({ "ok": false, "error": e.to_string() })),
                        }
                    }
                    let payload = json!({
                        "status": "rolled_back",
                        "failed_at_index": idx,
                        "error": err.to_string(),
                        "applied_before_failure": applied_results,
                        "rollback": rollback_log,
                    });
                    return Ok(CallToolResult::success(vec![Content::text(payload.to_string())]));
                }
            }
        }

        let payload = json!({
            "status": "applied",
            "operations": applied_results,
        });
        Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
        })
    }

    #[tool(description = "Return the canonical hierarchical path from root to the given node, by walking parent_id pointers via repeated get_node calls. Bounded by max_depth (default 50) so a malformed cycle doesn't loop forever. Each segment is { id, name }; use this for citation in distillations or for any caller that needs a stable, human-readable location.")]
    async fn path_of(
        &self,
        Parameters(params): Parameters<PathOfParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "path_of", params, {
        check_node_id(&params.node_id)?;
        let resolved = self.resolve_node_ref(&params.node_id)?;
        let max_depth = params.max_depth.unwrap_or(50);

        // Walk parent_id chain. We stop at the first None, the first
        // missing-node error, the first cycle (id we've seen), or the
        // depth cap. Each step is one HTTP call; for typical Workflowy
        // trees (depth 5-10) this is cheap.
        let mut segments: Vec<serde_json::Value> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut current_id: Option<String> = Some(resolved.clone());
        let mut depth = 0usize;
        while let Some(id) = current_id.take() {
            if !seen.insert(id.clone()) {
                tracing::warn!(node_id = %id, "path_of: cycle detected, stopping walk");
                break;
            }
            if depth >= max_depth {
                tracing::warn!(max_depth, "path_of: max_depth reached, returning partial path");
                break;
            }
            depth += 1;
            match self.client.get_node(&id).await {
                Ok(node) => {
                    segments.push(json!({ "id": node.id, "name": node.name }));
                    current_id = node.parent_id;
                }
                Err(e) => {
                    tracing::warn!(node_id = %id, error = %e, "path_of: get_node failed; returning partial path");
                    break;
                }
            }
        }
        // Reverse so index 0 is root, last is the requested node.
        segments.reverse();
        let display_path = segments
            .iter()
            .map(|s| s["name"].as_str().unwrap_or("(untitled)").to_string())
            .collect::<Vec<_>>()
            .join(" > ");
        let payload = json!({
            "node_id": resolved,
            "path": display_path,
            "segments": segments,
            "depth": segments.len(),
            "truncated": depth >= max_depth,
        });
        Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
        })
    }

    #[tool(description = "Apply a tag to many nodes in one call. A thin wrapper over bulk_update with operation=add_tag, optimised for the case where the caller already knows the exact node IDs and doesn't need a tree walk to find them. Each node is edited in parallel up to the standard concurrency cap.")]
    async fn bulk_tag(
        &self,
        Parameters(params): Parameters<BulkTagParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "bulk_tag", params, {
        if params.node_ids.is_empty() {
            return Err(McpError::invalid_params("node_ids must not be empty".to_string(), None));
        }
        let tag = params.tag.trim();
        if tag.is_empty() || tag.contains(char::is_whitespace) {
            return Err(McpError::invalid_params(
                "tag must be non-empty and contain no whitespace".to_string(),
                None,
            ));
        }
        let needle = format!("#{}", tag.trim_start_matches('#'));

        // Resolve every id eagerly so a short-hash miss is reported
        // before we start mutating.
        let ids: Vec<String> = params
            .node_ids
            .iter()
            .map(|id| {
                check_node_id(id)?;
                self.resolve_node_ref(id)
            })
            .collect::<Result<_, _>>()?;

        let concurrency = defaults::SUBTREE_FETCH_CONCURRENCY.max(1);
        use futures::StreamExt;
        let stream = futures::stream::iter(ids.into_iter().enumerate().map(|(idx, id)| {
            let needle = needle.clone();
            async move {
                let outcome = match self.client.get_node(&id).await {
                    Ok(node) => {
                        if node.name.contains(&needle) {
                            // Already tagged — no-op.
                            (idx, Ok(false))
                        } else {
                            let new_name = format!("{} {}", node.name.trim_end(), needle);
                            match self
                                .client
                                .edit_node(&id, Some(&new_name), None)
                                .await
                            {
                                Ok(_) => {
                                    self.cache.invalidate_node(&id);
                                    self.name_index.invalidate_node(&id);
                                    (idx, Ok(true))
                                }
                                Err(e) => (idx, Err(e.to_string())),
                            }
                        }
                    }
                    Err(e) => (idx, Err(e.to_string())),
                };
                outcome
            }
        }))
        .buffer_unordered(concurrency);

        let mut collected: Vec<(usize, Result<bool, String>)> = Vec::new();
        futures::pin_mut!(stream);
        while let Some(item) = stream.next().await {
            collected.push(item);
        }
        collected.sort_by_key(|(i, _)| *i);

        let mut tagged = 0usize;
        let mut already = 0usize;
        let mut failed = 0usize;
        let entries: Vec<serde_json::Value> = collected
            .into_iter()
            .enumerate()
            .map(|(idx, (_, r))| match r {
                Ok(true) => { tagged += 1; json!({ "index": idx, "status": "tagged" }) }
                Ok(false) => { already += 1; json!({ "index": idx, "status": "already_tagged" }) }
                Err(e) => { failed += 1; json!({ "index": idx, "status": "error", "error": e }) }
            })
            .collect();

        let payload = json!({
            "tag": needle,
            "total": entries.len(),
            "tagged": tagged,
            "already_tagged": already,
            "failed": failed,
            "results": entries,
        });
        Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
        })
    }

    #[tool(description = "Cheap incremental sync helper: returns whether the given node has been modified at or after the threshold timestamp (unix milliseconds). One API call. Useful for polling a small set of known-interesting nodes without re-walking the tree.")]
    async fn since(
        &self,
        Parameters(params): Parameters<SinceParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "since", params, {
        check_node_id(&params.node_id)?;
        let resolved = self.resolve_node_ref(&params.node_id)?;
        let node = self.client.get_node(&resolved).await.map_err(|e| {
            McpError::internal_error(format!("Failed to get node: {}", e), None)
        })?;
        let last_modified = node.last_modified.unwrap_or(0);
        let changed = last_modified >= params.timestamp_unix_ms;
        let payload = json!({
            "node_id": resolved,
            "name": node.name,
            "last_modified_unix_ms": last_modified,
            "threshold_unix_ms": params.timestamp_unix_ms,
            "changed_since": changed,
        });
        Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
        })
    }

    #[tool(description = "Find nodes that match BOTH a tag and a path prefix. Combines the lateral (tag) and vertical (PARA-style hierarchical path) graph axes in one query, so callers don't have to fetch a tag_search result and post-filter by path.")]
    async fn find_by_tag_and_path(
        &self,
        Parameters(params): Parameters<FindByTagAndPathParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "find_by_tag_and_path", params, {
        let max_depth = params.max_depth.unwrap_or(5);
        let limit = params.limit.unwrap_or(50);
        if let Some(rid) = &params.root_id { check_node_id(rid)?; }
        let scope = match &params.root_id {
            Some(r) => Some(self.resolve_node_ref(r)?),
            None => None,
        };

        match self.walk_subtree(scope.as_deref(), max_depth).await {
            Ok(fetch) => {
                let banner = truncation_banner_from_fetch(&fetch);
                let node_map = build_node_map(&fetch.nodes);
                let tag_lower = params.tag.to_lowercase();
                let prefix_lower = params.path_prefix.to_lowercase();

                let mut hits: Vec<serde_json::Value> = Vec::new();
                for node in &fetch.nodes {
                    // Tag check: covers #tag in name/description plus the
                    // explicit tags array.
                    let in_name = node.name.to_lowercase().contains(&tag_lower);
                    let in_desc = node.description.as_ref()
                        .map(|d| d.to_lowercase().contains(&tag_lower))
                        .unwrap_or(false);
                    let in_tags = node.tags.as_ref()
                        .map(|tags| tags.iter().any(|t| t.to_lowercase().contains(&tag_lower)))
                        .unwrap_or(false);
                    if !(in_name || in_desc || in_tags) { continue; }

                    let path = build_node_path_with_map(&node.id, &node_map);
                    if !path.to_lowercase().contains(&prefix_lower) { continue; }

                    hits.push(json!({
                        "id": node.id,
                        "name": node.name,
                        "path": path,
                    }));
                    if hits.len() >= limit { break; }
                }

                let body = json!({
                    "tag": params.tag,
                    "path_prefix": params.path_prefix,
                    "count": hits.len(),
                    "hits": hits,
                });
                let result_text = format!("{}{}", banner, body);
                Ok(CallToolResult::success(vec![Content::text(result_text)]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed: {}", e), None)),
        }
        })
    }

    #[tool(description = "Export a subtree in OPML (for Workflowy/outliner compatibility), Markdown (nested bullets), or JSON (raw node array). For backup, hand-off to other tools, or external processing. Subject to the standard 10 000-node and 20-second walk budgets — large subtrees may return partial output with a truncation marker.")]
    async fn export_subtree(
        &self,
        Parameters(params): Parameters<ExportSubtreeParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "export_subtree", params, {
        check_node_id(&params.node_id)?;
        let resolved = self.resolve_node_ref(&params.node_id)?;
        let max_depth = params.max_depth.unwrap_or(10);
        let format = params.format.to_lowercase();
        if !matches!(format.as_str(), "opml" | "markdown" | "json") {
            return Err(McpError::invalid_params(
                format!("unknown format '{}'; expected opml | markdown | json", params.format),
                None,
            ));
        }

        match self.walk_subtree(Some(&resolved), max_depth).await {
            Ok(fetch) => {
                let banner = truncation_banner_from_fetch(&fetch);
                let body = match format.as_str() {
                    "json" => serde_json::to_string_pretty(&fetch.nodes).map_err(|e| {
                        McpError::internal_error(format!("JSON encoding failed: {}", e), None)
                    })?,
                    "markdown" => render_subtree_markdown(&fetch.nodes, &resolved),
                    "opml" => render_subtree_opml(&fetch.nodes, &resolved),
                    _ => unreachable!("format validated above"),
                };
                Ok(CallToolResult::success(vec![Content::text(format!("{}{}", banner, body))]))
            }
            Err(e) => Err(McpError::internal_error(format!("Failed to walk subtree: {}", e), None)),
        }
        })
    }

    #[tool(description = "Stub for native Workflowy mirror creation. Workflowy's public REST API does not expose mirror creation, so this tool always returns an explanatory error. The user-facing 'mirror_of: <uuid>' note convention remains the documented workaround. Removing this stub once upstream adds the endpoint is tracked in the multi-pass plan (T-157).")]
    async fn create_mirror(
        &self,
        Parameters(params): Parameters<CreateMirrorParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "create_mirror", params, {
        Err(McpError::invalid_params(
            "create_mirror is not implemented: Workflowy's public REST API does not expose mirror creation. Use a `mirror_of: <uuid>` note on the duplicate node as a documentation convention; updates do not propagate. Tracked in tasks/reliability-and-ergonomics.md (Pass 6).".to_string(),
            None,
        ))
        })
    }

    #[tool(description = "Return recent tool invocations from the in-memory ring buffer. Use for self-diagnosis: when a call hangs or returns unexpectedly, the previous N entries reveal the workload that produced the symptom. Includes tool name, params hash, start/finish timestamps, duration, and ok/err status.")]
    async fn get_recent_tool_calls(
        &self,
        Parameters(params): Parameters<GetRecentToolCallsParams>,
    ) -> Result<CallToolResult, McpError> {
        // Note: this handler does NOT record itself — the act of querying
        // the log shouldn't perturb the log it's reporting on.
        let limit = params
            .limit
            .unwrap_or(50)
            .min(self.op_log.capacity());
        let entries = self.op_log.recent(limit, params.since_unix_ms);
        let payload = json!({
            "buffer_capacity": self.op_log.capacity(),
            "buffer_size": self.op_log.len(),
            "total_recorded": self.op_log.total_recorded(),
            "returned": entries.len(),
            "entries": entries,
        });
        Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
    }
}

/// Inverse of a successful transaction step. Applied in reverse order
/// during rollback. Not every operation has a clean inverse — see
/// [`WorkflowyMcpServer::apply_txn_op`] for which operations support
/// rollback and which only run forward.
enum TxnInverse {
    /// Roll back a `create` by deleting the new node.
    DeleteCreated { node_id: String, parent_id: Option<String> },
    /// Roll back an `edit` by reapplying the previous name/description.
    RestoreEdit {
        node_id: String,
        prev_name: Option<String>,
        prev_description: Option<String>,
    },
    /// Roll back a `move` by moving the node back to its previous parent.
    UnMove {
        node_id: String,
        prev_parent_id: Option<String>,
        prev_priority: Option<i32>,
    },
}

/// Out-of-router helpers for `transaction`. Kept outside the
/// `#[tool_router]` impl so they don't get registered as tools.
impl WorkflowyMcpServer {
    /// Execute one transaction op. Returns a human-readable summary plus
    /// the inverse to replay on rollback (`None` if the op is not
    /// invertible — currently only `delete`).
    async fn apply_txn_op(
        &self,
        op: &TransactionOpParams,
    ) -> Result<(serde_json::Value, Option<TxnInverse>), McpError> {
        match op.op.as_str() {
            "create" => {
                let parent_id = match &op.parent_id {
                    Some(pid) => Some(self.resolve_node_ref(pid)?),
                    None => None,
                };
                let name = op.name.as_deref().ok_or_else(|| {
                    McpError::invalid_params("create requires `name`".to_string(), None)
                })?;
                let created = self
                    .client
                    .create_node(name, op.description.as_deref(), parent_id.as_deref(), op.priority)
                    .await
                    .map_err(|e| McpError::internal_error(format!("create failed: {}", e), None))?;
                if let Some(pid) = &parent_id {
                    self.cache.invalidate_node(pid);
                }
                self.name_index.ingest(&[WorkflowyNode {
                    id: created.id.clone(),
                    name: created.name.clone(),
                    parent_id: parent_id.clone(),
                    ..Default::default()
                }]);
                let summary = json!({
                    "op": "create",
                    "id": created.id.clone(),
                    "name": created.name.clone(),
                });
                let inverse = TxnInverse::DeleteCreated {
                    node_id: created.id,
                    parent_id,
                };
                Ok((summary, Some(inverse)))
            }
            "edit" => {
                let node_id_raw = op.node_id.as_ref().ok_or_else(|| {
                    McpError::invalid_params("edit requires `node_id`".to_string(), None)
                })?;
                let node_id = self.resolve_node_ref(node_id_raw)?;
                if op.name.is_none() && op.description.is_none() {
                    return Err(McpError::invalid_params(
                        "edit requires at least one of `name`/`description`".to_string(),
                        None,
                    ));
                }
                // Capture the prior state so we can restore it on
                // rollback. A failed pre-read disables the rollback for
                // this op rather than aborting — partial rollback is
                // better than none.
                let prev = self.client.get_node(&node_id).await.ok();
                self.client
                    .edit_node(&node_id, op.name.as_deref(), op.description.as_deref())
                    .await
                    .map_err(|e| McpError::internal_error(format!("edit failed: {}", e), None))?;
                self.cache.invalidate_node(&node_id);
                self.name_index.invalidate_node(&node_id);
                let summary = json!({ "op": "edit", "id": node_id.clone() });
                let inverse = prev.map(|n| TxnInverse::RestoreEdit {
                    node_id,
                    prev_name: Some(n.name),
                    prev_description: n.description,
                });
                Ok((summary, inverse))
            }
            "delete" => {
                let node_id_raw = op.node_id.as_ref().ok_or_else(|| {
                    McpError::invalid_params("delete requires `node_id`".to_string(), None)
                })?;
                let node_id = self.resolve_node_ref(node_id_raw)?;
                self.client.delete_node(&node_id).await.map_err(|e| {
                    McpError::internal_error(format!("delete failed: {}", e), None)
                })?;
                self.cache.invalidate_node(&node_id);
                self.name_index.invalidate_node(&node_id);
                let summary = json!({ "op": "delete", "id": node_id });
                // Delete is intentionally NOT invertible — recreating a
                // deleted subtree (with stable ids and modification
                // timestamps) is not something this server can promise.
                // Caller should sequence deletes last in a transaction.
                Ok((summary, None))
            }
            "move" => {
                let node_id_raw = op.node_id.as_ref().ok_or_else(|| {
                    McpError::invalid_params("move requires `node_id`".to_string(), None)
                })?;
                let new_parent_raw = op.new_parent_id.as_ref().ok_or_else(|| {
                    McpError::invalid_params("move requires `new_parent_id`".to_string(), None)
                })?;
                let node_id = self.resolve_node_ref(node_id_raw)?;
                let new_parent = self.resolve_node_ref(new_parent_raw)?;
                let prev = self.client.get_node(&node_id).await.ok();
                let prev_parent_id = prev.as_ref().and_then(|n| n.parent_id.clone());
                let prev_priority = prev.as_ref().and_then(|n| n.priority).map(|p| p as i32);
                self.client
                    .move_node(&node_id, &new_parent, op.priority)
                    .await
                    .map_err(|e| McpError::internal_error(format!("move failed: {}", e), None))?;
                self.cache.invalidate_node(&node_id);
                self.cache.invalidate_node(&new_parent);
                if let Some(pid) = &prev_parent_id {
                    self.cache.invalidate_node(pid);
                }
                self.name_index.invalidate_node(&node_id);
                let summary = json!({ "op": "move", "id": node_id.clone(), "to": new_parent.clone() });
                let inverse = TxnInverse::UnMove {
                    node_id,
                    prev_parent_id,
                    prev_priority,
                };
                Ok((summary, Some(inverse)))
            }
            other => Err(McpError::invalid_params(
                format!("unknown transaction op '{}'; expected create/edit/delete/move", other),
                None,
            )),
        }
    }

    /// Apply a single inverse during rollback. Each path is best-effort:
    /// failures during rollback are surfaced to the caller in the
    /// transaction response but do not stop the rollback from continuing.
    async fn run_inverse(&self, inv: TxnInverse) -> Result<serde_json::Value, McpError> {
        match inv {
            TxnInverse::DeleteCreated { node_id, parent_id } => {
                self.client
                    .delete_node(&node_id)
                    .await
                    .map_err(|e| McpError::internal_error(format!("rollback delete failed: {}", e), None))?;
                if let Some(pid) = &parent_id {
                    self.cache.invalidate_node(pid);
                }
                self.name_index.invalidate_node(&node_id);
                Ok(json!({ "rolled_back": "create", "id": node_id }))
            }
            TxnInverse::RestoreEdit { node_id, prev_name, prev_description } => {
                self.client
                    .edit_node(&node_id, prev_name.as_deref(), prev_description.as_deref())
                    .await
                    .map_err(|e| McpError::internal_error(format!("rollback edit failed: {}", e), None))?;
                self.cache.invalidate_node(&node_id);
                self.name_index.invalidate_node(&node_id);
                Ok(json!({ "rolled_back": "edit", "id": node_id }))
            }
            TxnInverse::UnMove { node_id, prev_parent_id, prev_priority } => {
                if let Some(pid) = prev_parent_id {
                    self.client
                        .move_node(&node_id, &pid, prev_priority)
                        .await
                        .map_err(|e| McpError::internal_error(format!("rollback move failed: {}", e), None))?;
                    self.cache.invalidate_node(&node_id);
                    self.cache.invalidate_node(&pid);
                    Ok(json!({ "rolled_back": "move", "id": node_id, "to": pid }))
                } else {
                    Ok(json!({ "skipped": "move", "id": node_id, "reason": "previous parent unknown" }))
                }
            }
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
                "I manage Workflowy content. Node IDs accept either full UUIDs or 12-char short hashes (the trailing 12 hex of a UUID, as used in Workflowy URLs).

Search & Navigation:
- search_nodes: Search by text query (use parent_id + max_depth to scope on large trees)
- find_node: Find by name (exact/contains/starts_with). Requires parent_id or allow_root_scan=true; set use_index=true for the cached fast path.
- get_node: Fetch a node plus a depth-1 listing of its direct children
- list_children: List children of a node
- get_subtree: Full tree under a node (bounded by timeout + node cap; see truncation_reason and truncated_at_path)
- tag_search: Search by tag (#tag or @person)
- find_backlinks: Find nodes linking to a given node
- find_by_tag_and_path: Tag intersected with a hierarchical path prefix
- path_of: Canonical root→node path (segments + display string)

Content creation & editing:
- create_node: Create a new node
- batch_create_nodes: Pipelined batch creator with per-op status (not transactional)
- transaction: Sequential create/edit/delete/move with best-effort rollback
- edit_node: Edit name and/or description (at least one required; combined updates split into two POSTs to dodge upstream field-loss bug)
- delete_node: Delete a node
- move_node: Move with retry-on-stale-parent (refreshes parent listing on 4xx then retries once)
- insert_content: Insert hierarchical content from indented text
- smart_insert: Search + insert in one call
- bulk_update: Apply operations to filtered nodes (with dry_run)
- bulk_tag: Apply one tag to many node IDs in parallel
- duplicate_node: Deep-copy a node subtree
- create_from_template: Copy template with {{variable}} substitution
- convert_markdown: Convert markdown to Workflowy format
- export_subtree: Export as OPML | Markdown | JSON
- create_mirror: STUB — Workflowy's REST API does not expose mirror creation; returns an explanatory error

Todos & scheduling:
- daily_review: Overdue + upcoming + recent + pending in one call
- list_todos / list_overdue / list_upcoming
- get_recent_changes: Nodes modified in the last N days
- since: Cheap incremental check — has this node changed since a timestamp?

Project & summary:
- get_project_summary: Stats, tag counts, assignees

Diagnostics & ops:
- workflowy_status: Extended liveness — in_flight_walks, last_request_ms, tree_size_estimate, upstream rate-limit headers (preferred over health_check when deciding to launch a heavy query)
- health_check: Sub-second API + cache + index diagnostic
- cancel_all: Cancel in-flight tree walks; preempts the rate-limiter and HTTP send within ~50ms
- build_name_index: Populate the opportunistic name index for fast find_node lookups
- get_recent_tool_calls: Per-call ring-buffer log (tool, params hash, duration, ok/err) for self-diagnosis"
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
    async fn resolve_node_ref_returns_full_uuid_for_known_short_hash() {
        let server = new_test_server();
        let full = "550e8400-e29b-41d4-a716-446655440000";
        // Seed the index by ingesting a node with this id.
        server.name_index.ingest(&[WorkflowyNode {
            id: full.to_string(),
            name: "Tasks".to_string(),
            parent_id: None,
            ..Default::default()
        }]);
        let resolved = server
            .resolve_node_ref("446655440000")
            .expect("known short hash should resolve");
        assert_eq!(resolved, full);
    }

    #[tokio::test]
    async fn resolve_node_ref_errors_on_unknown_short_hash() {
        let server = new_test_server();
        let err = server
            .resolve_node_ref("ffffffffffff")
            .expect_err("unknown short hash must error");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("short-hash") || msg.contains("name index"),
            "got: {msg}"
        );
    }

    #[tokio::test]
    async fn resolve_node_ref_passes_full_uuid_through() {
        let server = new_test_server();
        let full = "550e8400-e29b-41d4-a716-446655440000";
        let resolved = server
            .resolve_node_ref(full)
            .expect("full UUID should pass through unchanged");
        assert_eq!(resolved, full);
    }

    #[tokio::test]
    async fn check_node_id_accepts_short_hash_at_boundary() {
        // The handler-boundary validator must let short hashes through;
        // resolve_node_ref handles the actual lookup later.
        check_node_id("446655440000").expect("12-char hex must validate");
        check_node_id("550e8400-e29b-41d4-a716-446655440000").expect("full UUID must validate");
        // But garbage still rejects.
        assert!(check_node_id("garbage").is_err());
        assert!(check_node_id("").is_err());
    }

    #[tokio::test]
    async fn op_log_records_handler_invocations_with_status() {
        let server = new_test_server();
        // First call: succeeds (no API needed)
        let _ = server
            .workflowy_status(Parameters(WorkflowyStatusParams::default()))
            .await
            .expect("status returns");
        // Second call: fails (invalid node id)
        let _ = server
            .get_node(Parameters(GetNodeParams { node_id: NodeId::from("") }))
            .await
            .expect_err("empty id rejected");

        let entries = server.op_log().recent(10, None);
        assert!(entries.len() >= 2, "expected at least 2 entries, got {}", entries.len());
        // Newest first.
        let names: Vec<&str> = entries.iter().map(|e| e.tool.as_str()).collect();
        assert!(names.contains(&"get_node"), "missing get_node entry; got {names:?}");
        assert!(names.contains(&"workflowy_status"), "missing workflowy_status entry; got {names:?}");

        // Status reflects the actual outcome of each call.
        let get_node_entry = entries.iter().find(|e| e.tool == "get_node").unwrap();
        assert_eq!(get_node_entry.status, crate::utils::OpStatus::Err);
        assert!(get_node_entry.error.is_some(), "err entry must include message");

        let status_entry = entries.iter().find(|e| e.tool == "workflowy_status").unwrap();
        assert_eq!(status_entry.status, crate::utils::OpStatus::Ok);
        assert!(status_entry.error.is_none());
    }

    #[tokio::test]
    async fn get_recent_tool_calls_returns_entries_without_recording_itself() {
        let server = new_test_server();
        // Drive a few calls.
        for _ in 0..3 {
            let _ = server
                .workflowy_status(Parameters(WorkflowyStatusParams::default()))
                .await;
        }
        let total_before = server.op_log().total_recorded();
        // Querying the log must NOT record itself — otherwise a caller can
        // never get a clean snapshot.
        let result = server
            .get_recent_tool_calls(Parameters(GetRecentToolCallsParams { limit: Some(10), since_unix_ms: None }))
            .await
            .expect("query returns");
        let total_after = server.op_log().total_recorded();
        assert_eq!(total_before, total_after, "get_recent_tool_calls must not record itself");
        let body = result_text(&result);
        let v: serde_json::Value = serde_json::from_str(&body).expect("payload JSON");
        assert!(v["entries"].as_array().unwrap().len() >= 3);
        assert_eq!(v["total_recorded"], total_after);
    }

    // --- Brief acceptance: 2026-04-25 (Pattern A/B/C transient failures) ---

    /// Brief acceptance #2: error surface fidelity. When a tool call fails,
    /// the response MUST include a JSON-RPC error code and a `data` payload
    /// naming the operation, the node_id, the proximate cause, and the raw
    /// error string. Bare "Tool execution failed" with no other detail is a
    /// regression — this test fails loud if `tool_error` ever drops the
    /// structured payload.
    #[tokio::test]
    async fn tool_error_carries_operation_node_id_and_hint() {
        // 404-class error → resource_not_found code + propagation-lag hint.
        let err = tool_error(
            "get_node",
            Some("d096140b-0ed4-498a-981d-582fc2a2c8d6"),
            "API error 404: not found",
        );
        assert_eq!(err.code, ErrorCode::RESOURCE_NOT_FOUND);
        let data = err.data.expect("404 error must carry structured data");
        assert_eq!(data["operation"], "get_node");
        assert_eq!(data["node_id"], "d096140b-0ed4-498a-981d-582fc2a2c8d6");
        let hint = data["hint"].as_str().expect("hint must be string");
        assert!(
            hint.contains("propagation lag") || hint.contains("not yet exist"),
            "404 hint should mention propagation/existence: {hint}"
        );

        // Timeout-class error → internal_error code + timeout hint.
        let err = tool_error("list_children", Some("abc"), "subtree walk timed out");
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
        let data = err.data.expect("timeout error must carry data");
        assert!(data["hint"].as_str().unwrap().contains("timeout"));

        // 5xx-class error → backend hint.
        let err = tool_error("edit_node", Some("xyz"), "API error 500: server error");
        let data = err.data.expect("5xx must carry data");
        assert!(data["hint"].as_str().unwrap().contains("backend"));

        // The message field always names the operation so even clients that
        // only show `message` can correlate the failure with a tool.
        assert!(err.message.starts_with("edit_node:"));
    }

    /// Brief acceptance #1: listing-then-lookup parity. A node ID returned
    /// by `list_children(parent_id)` must be retrievable via `get_node`
    /// without the caller having to handle propagation lag manually. The
    /// server-level half of this guarantee is the propagation-retry
    /// helpers on `WorkflowyClient`; this test confirms those helpers are
    /// the ones the handlers use, not the bare endpoints.
    #[tokio::test]
    async fn get_node_handler_uses_propagation_retry() {
        // We can't reach the live API in unit tests; instead this test
        // documents the wiring contract by reading the handler source via
        // a sentinel grep. The matching string lives in the handler body
        // — if a future refactor swaps `_with_propagation_retry` back to
        // the bare `get_node`/`get_children`, this test fails loud.
        let src = include_str!("server.rs");
        assert!(
            src.contains("get_node_with_propagation_retry"),
            "get_node handler must call propagation-retry variant; otherwise nodes \
             returned via list_children may 404 on direct lookup due to upstream lag"
        );
        assert!(
            src.contains("get_children_with_propagation_retry"),
            "list_children + get_node child fetch must both use propagation-retry"
        );
    }

    /// Brief P4 #3: per-call-type health. `workflowy_status` must distinguish
    /// between call paths so the assistant can read a single status response
    /// to know whether `get_node` is healthy while `search_nodes` is
    /// degraded — the diagnostic gap that made Pattern B hard to pinpoint.
    #[tokio::test]
    async fn workflowy_status_includes_per_tool_health() {
        let server = new_test_server();
        // Drive a mix of ok and err calls so the per-tool histogram has data.
        for _ in 0..3 {
            let _ = server
                .workflowy_status(Parameters(WorkflowyStatusParams::default()))
                .await
                .expect("status ok");
        }
        // get_node with empty id always errors — useful to seed an err entry.
        let _ = server
            .get_node(Parameters(GetNodeParams { node_id: NodeId::from("") }))
            .await
            .expect_err("empty id rejected");

        let result = server
            .workflowy_status(Parameters(WorkflowyStatusParams::default()))
            .await
            .expect("status returns");
        let body = result_text(&result);
        let v: serde_json::Value = serde_json::from_str(&body).expect("status payload");
        let per_tool = v.get("per_tool_health").expect("per_tool_health field present");
        let workflowy_status_health = per_tool.get("workflowy_status").expect("workflowy_status entry");
        assert_eq!(workflowy_status_health["status"], "healthy");
        let get_node_health = per_tool.get("get_node").expect("get_node entry");
        // The empty-id rejection is the only get_node call recorded — must
        // show up as failing (1.0 err_rate).
        assert_eq!(get_node_health["status"], "failing");
        assert_eq!(get_node_health["err"], 1);
    }

    #[tokio::test]
    async fn workflowy_status_returns_in_flight_and_rate_limit_fields() {
        let server = new_test_server();
        let result = server
            .workflowy_status(Parameters(WorkflowyStatusParams::default()))
            .await
            .expect("workflowy_status must always return");
        let body = result_text(&result);
        let v: serde_json::Value = serde_json::from_str(&body).expect("status payload is JSON");
        // Required new fields — caller relies on these to decide whether to
        // launch a heavy query.
        assert!(v.get("in_flight_walks").is_some(), "missing in_flight_walks: {body}");
        assert!(v.get("last_request_ms").is_some(), "missing last_request_ms: {body}");
        assert!(v.get("tree_size_estimate").is_some(), "missing tree_size_estimate: {body}");
        assert!(v.get("rate_limit").is_some(), "missing rate_limit: {body}");
        assert_eq!(v["in_flight_walks"], 0, "no walks running in test: {body}");
        assert_eq!(
            v["rate_limit"]["observed"], false,
            "no live API in this test, headers must be marked unobserved: {body}"
        );
    }

    fn result_text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .find_map(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn get_node_handler_returns_invalid_id_error() {
        // Sanity: get_node still validates IDs at the handler boundary even
        // though the handler now also fetches children. An empty ID must be
        // rejected before any HTTP call.
        let server = new_test_server();
        let params = GetNodeParams { node_id: NodeId::from("") };
        let err = server
            .get_node(Parameters(params))
            .await
            .expect_err("empty node_id must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("node_id") || msg.contains("invalid"), "got: {msg}");
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

    #[test]
    fn truncation_banner_from_fetch_includes_path_when_truncated_at_known_node() {
        // Build a small synthetic subtree where truncation fires at a known
        // node, and confirm the banner names the unfinished branch by path.
        let nodes = vec![
            WorkflowyNode { id: "a".into(), name: "Work".into(), parent_id: None, ..Default::default() },
            WorkflowyNode { id: "b".into(), name: "Projects".into(), parent_id: Some("a".into()), ..Default::default() },
            WorkflowyNode { id: "c".into(), name: "Customer Engagements".into(), parent_id: Some("b".into()), ..Default::default() },
        ];
        let fetch = SubtreeFetch {
            nodes,
            truncated: true,
            limit: 10_000,
            truncation_reason: Some(TruncationReason::NodeLimit),
            elapsed_ms: 12,
            truncated_at_node_id: Some("c".into()),
        };
        let banner = truncation_banner_from_fetch(&fetch);
        assert!(banner.contains("Walk stopped at:"), "banner: {banner}");
        assert!(
            banner.contains("Work > Projects > Customer Engagements"),
            "banner missing full path: {banner}"
        );
    }

    #[test]
    fn truncation_banner_from_fetch_omits_path_when_anchor_unknown() {
        let fetch = SubtreeFetch {
            nodes: Vec::new(),
            truncated: true,
            limit: 10_000,
            truncation_reason: Some(TruncationReason::Timeout),
            elapsed_ms: 20_001,
            truncated_at_node_id: None,
        };
        let banner = truncation_banner_from_fetch(&fetch);
        assert!(banner.contains("timed out"), "banner: {banner}");
        assert!(
            !banner.contains("Walk stopped at:"),
            "no path should appear when anchor is unknown: {banner}"
        );
    }

    #[test]
    fn truncation_banner_from_fetch_silent_when_complete() {
        let fetch = SubtreeFetch {
            nodes: Vec::new(),
            truncated: false,
            limit: 10_000,
            truncation_reason: None,
            elapsed_ms: 5,
            truncated_at_node_id: None,
        };
        assert_eq!(truncation_banner_from_fetch(&fetch), "");
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
        for tool in [
            "health_check",
            "workflowy_status",
            "cancel_all",
            "build_name_index",
            "get_recent_tool_calls",
            "batch_create_nodes",
            "transaction",
            "path_of",
            "bulk_tag",
            "since",
            "find_by_tag_and_path",
            "export_subtree",
            "create_mirror",
        ] {
            assert!(server.get_tool(tool).is_some(), "tool {tool} must be registered");
        }
    }

    #[test]
    fn render_subtree_markdown_nests_by_parent_id() {
        let nodes = vec![
            WorkflowyNode { id: "r".into(), name: "Root".into(), parent_id: None, ..Default::default() },
            WorkflowyNode { id: "a".into(), name: "A".into(), parent_id: Some("r".into()), ..Default::default() },
            WorkflowyNode { id: "a1".into(), name: "A1".into(), parent_id: Some("a".into()), ..Default::default() },
            WorkflowyNode { id: "b".into(), name: "B".into(), parent_id: Some("r".into()), description: Some("desc line".into()), ..Default::default() },
        ];
        let md = render_subtree_markdown(&nodes, "r");
        assert!(md.starts_with("- Root\n"), "got: {md}");
        assert!(md.contains("  - A\n"), "got: {md}");
        assert!(md.contains("    - A1\n"), "got: {md}");
        assert!(md.contains("  - B\n"), "got: {md}");
        assert!(md.contains("      desc line"), "description rendered: {md}");
    }

    #[test]
    fn render_subtree_opml_escapes_xml_metacharacters() {
        let nodes = vec![
            WorkflowyNode { id: "r".into(), name: "Root <special>".into(), parent_id: None, description: Some("note & stuff".into()), ..Default::default() },
            WorkflowyNode { id: "c".into(), name: "Child \"q\"".into(), parent_id: Some("r".into()), ..Default::default() },
        ];
        let opml = render_subtree_opml(&nodes, "r");
        assert!(opml.contains("<?xml version"), "got: {opml}");
        assert!(opml.contains("text=\"Root &lt;special&gt;\""), "got: {opml}");
        assert!(opml.contains("_note=\"note &amp; stuff\""), "got: {opml}");
        assert!(opml.contains("text=\"Child &quot;q&quot;\""), "got: {opml}");
        // Childless terminal nodes should self-close.
        assert!(opml.contains("/>") || opml.contains("</outline>"), "got: {opml}");
    }

    #[tokio::test]
    async fn export_subtree_rejects_unknown_format() {
        let server = new_test_server();
        let err = server
            .export_subtree(Parameters(ExportSubtreeParams {
                node_id: NodeId::from(valid_id()),
                format: "yaml".to_string(),
                max_depth: None,
            }))
            .await
            .expect_err("unknown format must reject");
        assert!(err.to_string().to_lowercase().contains("unknown format"));
    }

    #[tokio::test]
    async fn create_mirror_returns_explanatory_error() {
        let server = new_test_server();
        let err = server
            .create_mirror(Parameters(CreateMirrorParams {
                canonical_node_id: NodeId::from(valid_id()),
                target_parent_id: NodeId::from(other_valid_id()),
                priority: None,
            }))
            .await
            .expect_err("create_mirror is documented as not implemented");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("not implemented") || msg.contains("does not expose"), "got: {msg}");
    }

    #[tokio::test]
    async fn bulk_tag_rejects_empty_node_ids() {
        let server = new_test_server();
        let err = server
            .bulk_tag(Parameters(BulkTagParams {
                node_ids: Vec::new(),
                tag: "review".to_string(),
            }))
            .await
            .expect_err("empty node_ids must reject");
        assert!(err.to_string().to_lowercase().contains("empty"));
    }

    #[tokio::test]
    async fn bulk_tag_rejects_whitespace_tag() {
        let server = new_test_server();
        let err = server
            .bulk_tag(Parameters(BulkTagParams {
                node_ids: vec![NodeId::from(valid_id())],
                tag: "two words".to_string(),
            }))
            .await
            .expect_err("tags with whitespace must reject");
        assert!(err.to_string().to_lowercase().contains("whitespace"));
    }

    #[tokio::test]
    async fn find_by_tag_and_path_rejects_invalid_root() {
        let server = new_test_server();
        let err = server
            .find_by_tag_and_path(Parameters(FindByTagAndPathParams {
                tag: "review".to_string(),
                path_prefix: "Work".to_string(),
                root_id: Some(NodeId::from("not-a-uuid")),
                max_depth: None,
                limit: None,
            }))
            .await
            .expect_err("bad root id must reject");
        assert!(err.to_string().to_lowercase().contains("invalid"));
    }

    #[tokio::test]
    async fn batch_create_nodes_rejects_empty_operations() {
        let server = new_test_server();
        let err = server
            .batch_create_nodes(Parameters(BatchCreateNodesParams { operations: Vec::new() }))
            .await
            .expect_err("empty batch must reject");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("empty"), "got: {msg}");
    }

    #[tokio::test]
    async fn batch_create_nodes_validates_parent_ids_eagerly() {
        let server = new_test_server();
        let bad = BatchCreateOpParams {
            name: "x".to_string(),
            description: None,
            parent_id: Some(NodeId::from("not-a-uuid")),
            priority: None,
        };
        let good = BatchCreateOpParams {
            name: "y".to_string(),
            description: None,
            parent_id: None,
            priority: None,
        };
        let err = server
            .batch_create_nodes(Parameters(BatchCreateNodesParams {
                operations: vec![good, bad],
            }))
            .await
            .expect_err("invalid parent id must abort batch");
        assert!(err.to_string().to_lowercase().contains("invalid"));
    }

    #[tokio::test]
    async fn transaction_rejects_empty_operations() {
        let server = new_test_server();
        let err = server
            .transaction(Parameters(TransactionParams { operations: Vec::new() }))
            .await
            .expect_err("empty transaction must reject");
        assert!(err.to_string().to_lowercase().contains("empty"));
    }

    #[tokio::test]
    async fn transaction_rejects_unknown_op_kind() {
        let server = new_test_server();
        // First op is invalid; the handler rejects via apply_txn_op.
        let result = server
            .transaction(Parameters(TransactionParams {
                operations: vec![TransactionOpParams {
                    op: "frobnicate".to_string(),
                    node_id: None,
                    parent_id: None,
                    new_parent_id: None,
                    name: None,
                    description: None,
                    priority: None,
                }],
            }))
            .await
            .expect("handler returns Ok with rolled-back payload even on first-op failure");
        let body = result_text(&result);
        assert!(body.contains("rolled_back"), "expected rollback envelope, got: {body}");
        assert!(body.contains("frobnicate"), "expected error to mention bad op: {body}");
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
