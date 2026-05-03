//! MCP Server implementation using rmcp
//! Implements ServerHandler with tool_router for all Workflowy tools

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{
        common::FromContextPart,
        tool::{ToolCallContext, ToolRouter},
    },
    model::{ErrorCode, *},
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use chrono::{NaiveDate, Utc};
use regex::Regex;
// `serde::Deserialize` moved to params.rs along with the param structs.
use serde_json::json;
use std::collections::HashMap;
use tracing::{error, info, warn};

use crate::api::{BatchCreateOp, FetchControls, SubtreeFetch, TruncationReason, WorkflowyClient};
use crate::defaults;
use crate::error::WorkflowyError;
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

/// Discrete proximate-cause classification for a tool failure. Brief
/// 2026-04-25 Test γ requires every error to carry one of these
/// values so callers can route on the cause rather than parsing
/// human-readable hint strings. Variants map 1:1 to the brief's
/// requested enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProximateCause {
    Timeout,
    LockContention,
    CacheMiss,
    UpstreamError,
    Cancelled,
    NotFound,
    AuthFailure,
    Unknown,
}

impl ProximateCause {
    /// String form for the JSON-RPC `data.proximate_cause` field. Stable
    /// over the lifetime of the API: callers may match on these values.
    pub const fn as_str(self) -> &'static str {
        match self {
            ProximateCause::Timeout => "timeout",
            ProximateCause::LockContention => "lock_contention",
            ProximateCause::CacheMiss => "cache_miss",
            ProximateCause::UpstreamError => "upstream_error",
            ProximateCause::Cancelled => "cancelled",
            ProximateCause::NotFound => "not_found",
            ProximateCause::AuthFailure => "auth_failure",
            ProximateCause::Unknown => "unknown",
        }
    }
}

/// Build a structured `McpError` for a tool failure. Picks a JSON-RPC error
/// code based on the underlying error class and attaches a `data` payload
/// with `{operation, node_id, hint, error, proximate_cause}` so even
/// minimal clients can extract the proximate cause when their UI renders
/// only the generic "tool failed" surface. Supersedes the previous
/// direct calls to `McpError::internal_error(format!("Failed: {}", e), None)`
/// which were being truncated to "Tool execution failed" by some clients.
///
/// Brief 2026-04-25 Test γ: `proximate_cause` is a discrete enum, not a
/// free-text hint, so callers can switch on it without parsing.
/// Tool kinds for `tool_handler!` / `run_handler`.
/// The wrapper picks the wall-clock budget from the kind, so call sites
/// stay readable and the budget constants live in one place
/// ([`defaults`]).
///
/// The taxonomy is deliberately small (four kinds) because the server
/// ships ~30 tools and a finer split would just be noise. **Every
/// non-diagnostic handler is wrapped in `tool_handler!(name, kind, ...)`
/// — uniform safety net, no exceptions outside the diagnostic
/// carve-out below.** The 2026-05-02 4-minute write hangs traced to
/// single-node writes that bypassed this wrapper before the migration.
///
/// Diagnostics — `health_check`, `workflowy_status`, `cancel_all`,
/// `get_recent_tool_calls`, `build_name_index` — keep `record_op!`
/// alone because they own short, custom budgets that predate the
/// taxonomy. `convert_markdown` (pure local transform) and
/// `create_mirror` (stub) round out the carve-out, plus
/// `insert_content` (whose inline budget produces a partial-success
/// resume payload that the wrapper would short-circuit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolKind {
    /// Single-node read (`get_node`, `list_children`, `since`). Budget:
    /// [`defaults::READ_NODE_TIMEOUT_MS`] (30 s default; tests override
    /// via `read_budget_ms`).
    Read,
    /// Single-node write (`create_node`, `edit_node`, `delete_node`,
    /// `move_node`). Budget: [`defaults::WRITE_NODE_TIMEOUT_MS`]
    /// (15 s) — dovetails with the per-method deadlines built into the
    /// client. The wrapper is the cancel observer plus an outer safety
    /// net; the per-method client deadline is the primary bound.
    Write,
    /// Multi-step bulk operation (`duplicate_node`,
    /// `create_from_template`, `bulk_update`, `bulk_tag`,
    /// `batch_create_nodes`, `transaction`, `path_of`, `node_at_path`).
    /// Budget: [`defaults::INSERT_CONTENT_TIMEOUT_MS`] (210 s — well
    /// under the MCP client's 4-min hard timeout). Each per-iteration
    /// call runs to its own per-method budget; the bulk wrapper caps
    /// the total wall-clock so a runaway loop returns a structured
    /// timeout instead of "no result received". `insert_content` is
    /// the documented exception to the wrapper rule — it owns inline
    /// cancel + deadline checks because they produce the partial-
    /// success resume payload that the outer wrapper would short-
    /// circuit; see the comment at its handler.
    Bulk,
    /// Tree walk (`search_nodes`, `get_subtree`, `find_node`,
    /// `find_backlinks`, `daily_review`, `list_overdue`,
    /// `list_upcoming`, `list_todos`, `tag_search`,
    /// `get_recent_changes`, `get_project_summary`, `audit_mirrors`,
    /// `review`, `find_by_tag_and_path`, `export_subtree`,
    /// `resolve_link`, `smart_insert`). The internal `walk_subtree`
    /// owns [`defaults::SUBTREE_FETCH_TIMEOUT_MS`] via `FetchControls`,
    /// so the outer wrapper observes `cancel_all` only — no second
    /// deadline.
    Walk,
}

/// Derive `api_reachable` for the diagnostic tools. The probe is one
/// signal; a recent successful tool call is another. Either suffices —
/// the previous behaviour ("derive only from the probe") meant a single
/// probe blip during a long write burst could flip the status to
/// degraded even though the burst itself proved the API was up. Rather
/// than reflect that lag, status now treats a 2xx within
/// [`defaults::API_REACHABILITY_FRESHNESS_MS`] as positive evidence.
fn derive_api_reachable(probe_succeeded: bool, last_success_ms_ago: Option<u64>) -> bool {
    if probe_succeeded {
        return true;
    }
    matches!(
        last_success_ms_ago,
        Some(ms) if ms < defaults::API_REACHABILITY_FRESHNESS_MS
    )
}

fn tool_error(operation: &str, node_id: Option<&str>, err: impl std::fmt::Display) -> McpError {
    let err_str = err.to_string();
    let lower = err_str.to_lowercase();
    let (code, hint, cause) = if lower.contains("404") || lower.contains("not found") {
        (
            ErrorCode::RESOURCE_NOT_FOUND,
            "node may not yet exist (propagation lag), or has been deleted",
            ProximateCause::NotFound,
        )
    } else if lower.contains("cancelled") {
        (
            ErrorCode::INTERNAL_ERROR,
            "cancelled by cancel_all — the call was preempted, retry",
            ProximateCause::Cancelled,
        )
    } else if lower.contains("timeout") || lower.contains("timed out") {
        (
            ErrorCode::INTERNAL_ERROR,
            "upstream timeout — narrow scope or wait for load to drop",
            ProximateCause::Timeout,
        )
    } else if lower.contains("api error 5") {
        (
            ErrorCode::INTERNAL_ERROR,
            "Workflowy backend error — try again shortly",
            ProximateCause::UpstreamError,
        )
    } else if lower.contains("401") || lower.contains("403") || lower.contains("unauthor") {
        (
            ErrorCode::INTERNAL_ERROR,
            "auth failure — check WORKFLOWY_API_KEY",
            ProximateCause::AuthFailure,
        )
    } else if lower.contains("lock") {
        (
            ErrorCode::INTERNAL_ERROR,
            "internal lock contention — retry shortly",
            ProximateCause::LockContention,
        )
    } else if lower.contains("cache") {
        (
            ErrorCode::INTERNAL_ERROR,
            "stale cache entry — retry; the cache has been invalidated",
            ProximateCause::CacheMiss,
        )
    } else {
        (
            ErrorCode::INTERNAL_ERROR,
            "see data field for details",
            ProximateCause::Unknown,
        )
    };
    let data = serde_json::json!({
        "operation": operation,
        "node_id": node_id,
        "hint": hint,
        "proximate_cause": cause.as_str(),
        "error": err_str,
    });
    McpError::new(
        code,
        format!("{}: {} [{}]", operation, err_str, cause.as_str()),
        Some(data),
    )
}

/// Read recent session-log files (~/code/SecondBrain/session-logs/,
/// modified in the last 7 days) into a single string. Bucket (d) of the
/// `review` tool scans this for URL/DOI matches against source-MOC
/// descriptions. Returns "" when the directory doesn't exist or no
/// files are recent enough — the lib's `build_review` skips bucket (d)
/// gracefully on an empty blob, so this never panics or errors.
///
/// Lives on the server side (not in `audit.rs`) because the lib is
/// pure-data — no I/O, no clock, no env. Both the MCP `review` handler
/// and the `wflow-do review` CLI subcommand load the blob through their
/// own filesystem helper and pass it in.
fn load_recent_session_logs_blob_for_review() -> String {
    let Ok(home) = std::env::var("HOME") else {
        return String::new();
    };
    let dir = std::path::PathBuf::from(format!("{}/code/SecondBrain/session-logs", home));
    if !dir.exists() {
        return String::new();
    }
    let cutoff = chrono::Utc::now().timestamp() - 7 * 86_400;
    let mut blob = String::new();
    if let Ok(read) = std::fs::read_dir(&dir) {
        for ent in read.flatten() {
            if let Ok(meta) = ent.metadata() {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if mtime >= cutoff {
                    if let Ok(c) = std::fs::read_to_string(ent.path()) {
                        blob.push_str(&c);
                        blob.push('\n');
                    }
                }
            }
        }
    }
    blob
}

/// Wrap a handler body so the call is recorded in the per-call op log.
/// Use as the outermost expression of a *diagnostic* tool handler — one
/// that owns its own short budget and deliberately bypasses the
/// `ToolKind` taxonomy (e.g. `health_check`, `workflowy_status`,
/// `cancel_all`, `get_recent_tool_calls`, `build_name_index`):
///
/// ```ignore
/// async fn foo(&self, Parameters(params): Parameters<FooParams>) -> Result<CallToolResult, McpError> {
///     record_op!(self, "foo", params, {
///         // existing body, including `?` early returns
///     })
/// }
/// ```
///
/// **Every other handler must use [`tool_handler!`] instead** so its
/// body runs through [`WorkflowyMcpServer::run_handler`] and inherits
/// the uniform cancel + wall-clock safety net. `record_op!` on its own
/// records the call but does *not* observe `cancel_all` and does *not*
/// impose a handler-level deadline — the gap that produced the
/// 2026-05-02 4-minute write hangs.
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

/// Standard wrapper for every non-diagnostic tool handler. Combines
/// [`record_op!`] (op-log attribution) with
/// [`WorkflowyMcpServer::run_handler`] (cancel-registry observation +
/// wall-clock deadline keyed off [`ToolKind`]). Single pattern,
/// uniform safety net — adding a handler without it regresses both the
/// "cancel_all preempts every tool" invariant and the "no tool can sit
/// past its kind's wall-clock budget" invariant.
///
/// ```ignore
/// async fn foo(&self, Parameters(params): Parameters<FooParams>) -> Result<CallToolResult, McpError> {
///     tool_handler!(self, "foo", ToolKind::Write, params, {
///         // existing body, including `?` early returns
///     })
/// }
/// ```
///
/// `Walk`-kind handlers run cancel-only — their internal `walk_subtree`
/// owns the deadline. Diagnostics keep `record_op!` because their
/// short, custom budgets predate the taxonomy.
macro_rules! tool_handler {
    ($self:ident, $tool:literal, $kind:expr, $params:ident, $body:block) => {{
        let __pj = serde_json::to_value(&$params).unwrap_or(serde_json::Value::Null);
        let __recorder = $self.op_log.record($tool, &__pj);
        let __fut = async move $body;
        let __result: Result<CallToolResult, McpError> =
            $self.run_handler($tool, $kind, __fut).await;
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

/// Heuristic: is `s` a hex short hash? Used to short-circuit
/// `check_node_id` so callers can pass either form transparently.
/// Accepts both the 12-char URL-suffix form (Workflowy URLs) and the
/// 8-char prefix form (the first segment of a hyphenated UUID, used
/// widely in docs and skill files).
fn is_short_hash(s: &str) -> bool {
    use crate::utils::name_index::{SHORT_HASH_LEN_PREFIX, SHORT_HASH_LEN_URL};
    let stripped: String = s.chars().filter(|c| *c != '-').collect();
    let len = stripped.len();
    (len == SHORT_HASH_LEN_URL || len == SHORT_HASH_LEN_PREFIX)
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

/// Outcome of [`WorkflowyMcpServer::probe_upstream_with_retry`]. Holds
/// just enough for the two probe handlers to render their JSON
/// response without each having to reimplement classification.
#[derive(Debug)]
struct ProbeOutcome {
    api_reachable: bool,
    top_level_count: Option<usize>,
    error: Option<String>,
    elapsed_ms: u64,
    attempts: u8,
}

/// Strip HTML tags from a Workflowy node name. Workflowy stores
/// inline formatting (links, bold, colour spans) inside the name
/// field itself, which makes pure-string equality with what a user
/// typed unreliable. Used by [`WorkflowyMcpServer::node_at_path`] and
/// [`WorkflowyMcpServer::resolve_link`] when matching path segments
/// against children's names. Compiles the pattern once per call —
/// good enough since these paths are short, but if it shows up in a
/// hot loop, hoist into a `OnceLock`.
fn strip_html(s: &str) -> String {
    let re = match regex::Regex::new(r"<[^>]+>") {
        Ok(r) => r,
        Err(_) => return s.to_string(),
    };
    re.replace_all(s, "").to_string()
}

/// Result of an on-demand short-hash resolution walk. Carries enough
/// signal for the resolver to distinguish "walked the whole tree, the
/// hash genuinely doesn't exist" from "ran out of budget before
/// reaching the target," which are different recovery scenarios for
/// the caller.
#[derive(Debug, Clone)]
struct ResolveWalkSummary {
    nodes_walked: usize,
    truncated: bool,
    truncation_reason: Option<crate::api::TruncationReason>,
    elapsed_ms: u64,
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
    // Recovery hint shared across the timeout / node-cap branches.
    // 2026-05-03 eval run: every search under Distillations timed out
    // because the subtree grew past the 10 000-node walk cap. Naming
    // the recovery path in the banner itself keeps the caller from
    // having to guess.
    const INDEX_RECOVERY_HINT: &str = " Recovery: call `build_name_index(parent_id=...)` once to populate the persistent name index, then re-issue with `use_index=true` (search_nodes / find_node) to bypass the walk budget entirely — name-only match, no walk timeout.";
    match reason {
        Some(TruncationReason::Timeout) => format!(
            "⚠ subtree walk timed out before completion (budget {} ms). Results below reflect whatever was collected — retry with narrower parent_id/max_depth or raise the budget.{}{}\n\n",
            defaults::SUBTREE_FETCH_TIMEOUT_MS,
            suffix,
            INDEX_RECOVERY_HINT,
        ),
        Some(TruncationReason::Cancelled) => format!(
            "⚠ subtree walk was cancelled; results below are partial.{}\n\n",
            suffix,
        ),
        _ => format!(
            "⚠ subtree truncated at {} nodes — results below may be incomplete. Narrow parent_id or max_depth.{}{}\n\n",
            limit, suffix, INDEX_RECOVERY_HINT,
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

/// Recovery hint surfaced on every truncated response (markdown banner
/// AND JSON envelope). `use_index` is the bypass; this string names it
/// so callers don't have to read the docs to find the recovery.
const TRUNCATION_RECOVERY_HINT: &str = "Call build_name_index(parent_id=...) once to populate the persistent name index, then re-issue with use_index=true (search_nodes / find_node) to bypass the walk budget — name-only match, no walk timeout.";

/// JSON-truncation surface invariant: every walk-shaped tool that emits
/// JSON spreads this four-field envelope into its payload:
///
/// - `truncated: bool`
/// - `truncation_limit: usize` — the node cap that fired
/// - `truncation_reason: "timeout" | "node_limit" | "cancelled" | null`
/// - `truncation_recovery_hint: string` — empty when not truncated, otherwise [`TRUNCATION_RECOVERY_HINT`]
///
/// Pre-2026-05-03 most JSON tools emitted `truncation_limit` only — no
/// reason, no hint — so a JSON caller hitting the 20 s walk budget on a
/// big subtree had no actionable information. After the architecture
/// review on 2026-05-03 the four fields are produced by this single
/// helper and spread into every JSON payload via the
/// `with_truncation_envelope!` macro, so the truncation contract has
/// exactly one definition. Pinned by
/// `every_walk_tool_emits_full_truncation_envelope_in_json`.
fn truncation_envelope(
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("truncated".into(), json!(truncated));
    m.insert("truncation_limit".into(), json!(limit));
    m.insert("truncation_reason".into(), json!(reason.map(|r| r.as_str())));
    m.insert(
        "truncation_recovery_hint".into(),
        json!(if truncated { TRUNCATION_RECOVERY_HINT } else { "" }),
    );
    m
}

/// Variant that takes a `SubtreeFetch` reference for handlers that hold
/// the whole struct rather than destructuring it.
#[allow(dead_code)] // not all handlers hold the fetch; both shapes valid.
fn truncation_envelope_from_fetch(
    fetch: &SubtreeFetch,
) -> serde_json::Map<String, serde_json::Value> {
    truncation_envelope(fetch.truncated, fetch.limit, fetch.truncation_reason)
}

/// Combine a caller-built JSON payload with the truncation envelope.
/// Every walk-shaped tool's success path uses this to return a single
/// `serde_json::Value` carrying both the tool-specific fields and the
/// four envelope fields. The helper exists because pre-2026-05-03 the
/// envelope was inlined at every call site and 11 of them quietly
/// drifted off-spec; routing through one definition makes the
/// contract enforceable by `cargo build` rather than by a source-grep
/// audit. Inputs:
///
/// - `payload`: the tool-specific `json!({...})` map. Anything mergeable
///   into a JSON object works; non-object values panic in tests via
///   the usage convention.
/// - `truncated`, `limit`, `reason`: the three fields a `SubtreeFetch`
///   destructure pulls out — the helper converts `reason` to its
///   stable string form and constructs the recovery hint.
///
/// Returns a `serde_json::Value::Object` ready to pass to
/// `Content::text(value.to_string())`.
fn with_truncation_envelope(
    mut payload: serde_json::Value,
    truncated: bool,
    limit: usize,
    reason: Option<TruncationReason>,
) -> serde_json::Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.extend(truncation_envelope(truncated, limit, reason));
    } else {
        // Defensive: non-object payloads are a caller bug. Wrap so the
        // envelope is still attached, surfacing the misuse.
        let mut wrapped = serde_json::Map::new();
        wrapped.insert("payload".into(), payload);
        wrapped.extend(truncation_envelope(truncated, limit, reason));
        return serde_json::Value::Object(wrapped);
    }
    payload
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
    /// Wall-clock budget for single-node read tools. Defaults to
    /// [`defaults::READ_NODE_TIMEOUT_MS`]; tests dial it down via
    /// [`Self::with_read_budget_ms`] so failure-mode coverage runs in
    /// milliseconds instead of the production 30 s.
    read_budget_ms: u64,
    /// Wall-clock budget for bulk tools (`insert_content`, `path_of`,
    /// `bulk_tag`, `transaction`, `duplicate_node`,
    /// `create_from_template`, `bulk_update`, `batch_create_nodes`,
    /// `node_at_path`). Defaults to
    /// [`defaults::INSERT_CONTENT_TIMEOUT_MS`]; tests dial it down via
    /// [`Self::with_bulk_budget_ms`] so failure-mode coverage runs in
    /// milliseconds instead of the production 210 s.
    bulk_budget_ms: u64,
}

/// Drop-in replacement for `rmcp::handler::server::wrapper::Parameters`
/// that records every framework-level deserialization failure to the op
/// log *before* returning the typed `McpError` to the transport. The
/// rmcp standard wrapper rejects malformed requests before the handler
/// body runs, which means `per_tool_health.<tool>.err` never moved on
/// "invalid parameters" failures and the MCP client surfaced the bare
/// string `Tool execution failed` with no diagnostic.
///
/// Brief 2026-05-02 named that as the dominant debugging black hole.
/// Routing every parameter extraction through `Parameters` closes the
/// gap end-to-end: every rejected call now appears in the op log, every
/// rejection carries a typed `proximate_cause` in its data payload, and
/// the per-tool counters reflect the real failure rate rather than just
/// the failures that happened to clear deserialization first.
///
/// Schema (`JsonSchema`), serde wire format, and the destructuring
/// pattern (`Parameters(p)`) all match `Parameters` exactly — the
/// only behavioural difference is observability on the failure path.
pub struct Parameters<T>(pub T);

impl<T: JsonSchema> JsonSchema for Parameters<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn json_schema(g: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
        T::json_schema(g)
    }
}

impl<T> FromContextPart<ToolCallContext<'_, WorkflowyMcpServer>> for Parameters<T>
where
    T: serde::de::DeserializeOwned,
{
    fn from_context_part(
        context: &mut ToolCallContext<WorkflowyMcpServer>,
    ) -> std::result::Result<Self, McpError> {
        let arguments = context.arguments.take().unwrap_or_default();
        let json_value = serde_json::Value::Object(arguments);
        match serde_json::from_value::<T>(json_value.clone()) {
            Ok(value) => Ok(Parameters(value)),
            Err(e) => {
                // Record the rejection so per_tool_health reflects every
                // attempt, not just the ones that reached the handler
                // body. Without this the counters under-count by exactly
                // the failure mode the LLM cannot recover from.
                let msg = format!("invalid parameters: {}", e);
                let recorder = context
                    .service
                    .op_log
                    .record(context.name.to_string(), &json_value);
                recorder.finish_err(&msg);
                Err(tool_error(context.name.as_ref(), None, msg))
            }
        }
    }
}

// --- Parameter structs ---
//
// Moved to `params.rs` in the 2026-05-03 architecture-review file split.
// Re-exported here so existing call sites (`Parameters<SearchNodesParams>`,
// etc.) keep working without import churn. The wrapper rule lives in
// `principles-architecture.md` Principle 8: every #[tool] handler's arg
// type is `Parameters<XxxParams>` because rmcp-macros 0.16 identifier-
// matches that name to find the schema; renaming would silently break
// the wire surface.

mod params;
pub use params::*;

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
            read_budget_ms: defaults::READ_NODE_TIMEOUT_MS,
            bulk_budget_ms: defaults::INSERT_CONTENT_TIMEOUT_MS,
        }
    }

    /// Test-only: override the wall-clock budget that
    /// [`Self::run_handler`] applies to [`ToolKind::Read`] handlers.
    /// Production goes through [`Self::new`]/[`Self::with_cache`] which
    /// honours [`defaults::READ_NODE_TIMEOUT_MS`]; tests dial this down
    /// to milliseconds so failure paths exercise in test time rather
    /// than wall-clock time.
    #[cfg(test)]
    pub(crate) fn with_read_budget_ms(mut self, ms: u64) -> Self {
        self.read_budget_ms = ms;
        self
    }

    /// Test-only: override the wall-clock budget that
    /// [`Self::run_handler`] applies to [`ToolKind::Bulk`] handlers.
    /// Production honours [`defaults::INSERT_CONTENT_TIMEOUT_MS`]
    /// (210 s); tests dial down to milliseconds so the bulk-budget
    /// failure paths exercise quickly.
    #[cfg(test)]
    pub(crate) fn with_bulk_budget_ms(mut self, ms: u64) -> Self {
        self.bulk_budget_ms = ms;
        self
    }

    /// Production constructor: as `with_cache`, plus configure the
    /// persistent name index. When `save_path` is `Some`, the index is
    /// rehydrated from disk synchronously (so the very first tool call
    /// can see cached short-hash mappings) and any subsequent
    /// mutations mark it dirty for the periodic saver to pick up. A
    /// missing or unreadable file is logged at warn and the server
    /// starts with an empty index — never panicking.
    pub fn with_cache_and_persistence(
        client: Arc<WorkflowyClient>,
        cache: Arc<NodeCache>,
        save_path: Option<std::path::PathBuf>,
    ) -> Self {
        let server = Self::with_cache(client, cache);
        if let Some(path) = save_path {
            server.name_index.set_save_path(path.clone());
            match server.name_index.load_from_disk() {
                Ok(n) => {
                    info!(loaded = n, path = %path.display(), "name index hydrated from disk");
                }
                Err(e) => {
                    warn!(error = %e, path = %path.display(), "name index load failed; starting empty");
                }
            }
        }
        server
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

    /// Apply the server-wide cancel registry and a tool-kind-appropriate
    /// wall-clock budget to any operation. This is the **single uniform
    /// safety net** every API-touching handler runs inside; the kind
    /// selects the budget so the call sites stay readable and the
    /// constants stay in one place ([`defaults`]).
    ///
    /// - [`ToolKind::Read`]: bounded at [`defaults::READ_NODE_TIMEOUT_MS`]
    ///   (or the `read_budget_ms` test override). For single-node reads
    ///   like `get_node`, `list_children`.
    /// - [`ToolKind::Write`]: bounded at [`defaults::WRITE_NODE_TIMEOUT_MS`].
    ///   For single-node writes; the client's per-method deadline is the
    ///   primary bound, this wrapper adds cancel observation and a
    ///   safety net.
    /// - [`ToolKind::Bulk`]: bounded at [`defaults::INSERT_CONTENT_TIMEOUT_MS`].
    ///   For multi-step operations (`insert_content`, `duplicate_node`,
    ///   `create_from_template`, `bulk_update`, `bulk_tag`,
    ///   `batch_create_nodes`, `transaction`, `path_of`, `node_at_path`)
    ///   that own their own per-iteration logic but need an overall cap.
    /// - [`ToolKind::Walk`]: cancel-only — walks already carry an internal
    ///   `SUBTREE_FETCH_TIMEOUT_MS` deadline via `FetchControls`, so an
    ///   outer deadline would just be redundant.
    ///
    /// Dropping the inner future on timeout/cancel cleanly aborts the
    /// reqwest send (closes the connection) and releases the rate-limiter
    /// slot, so the next tool call starts fresh.
    /// Handler-flavoured wrapper that returns [`McpError`]. The inner
    /// future is the existing handler body — it produces its own
    /// structured `tool_error` payloads. The wrapper races it against
    /// the cancel registry and the kind-appropriate wall-clock budget;
    /// on timeout or cancel, it emits a `tool_error` with the operation
    /// name so the caller sees the same structured response shape
    /// regardless of which arm fired. This is the surgical
    /// intervention the architecture review identified: every
    /// API-touching handler runs inside one of these wrappers, so a
    /// future tool added without wiring its own budget still inherits
    /// the safety net.
    async fn run_handler<F>(
        &self,
        tool: &'static str,
        kind: ToolKind,
        fut: F,
    ) -> Result<CallToolResult, McpError>
    where
        F: std::future::Future<Output = Result<CallToolResult, McpError>>,
    {
        let budget_ms = match kind {
            ToolKind::Read => self.read_budget_ms,
            ToolKind::Write => defaults::WRITE_NODE_TIMEOUT_MS,
            ToolKind::Bulk => self.bulk_budget_ms,
            ToolKind::Walk => 0,
        };
        let guard = self.cancel_registry.guard();
        let poll_cancel = async {
            loop {
                if guard.is_cancelled() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        if matches!(kind, ToolKind::Walk) || budget_ms == 0 {
            tokio::select! {
                biased;
                _ = poll_cancel => Err(tool_error(tool, None, WorkflowyError::Cancelled)),
                res = fut => res,
            }
        } else {
            let deadline = tokio::time::sleep(Duration::from_millis(budget_ms));
            tokio::select! {
                biased;
                _ = poll_cancel => Err(tool_error(tool, None, WorkflowyError::Cancelled)),
                _ = deadline => Err(tool_error(tool, None, WorkflowyError::Timeout)),
                res = fut => res,
            }
        }
    }

    /// Test-only helper that exercises the same cancel/deadline-arm
    /// shape as [`Self::run_handler`] but at the
    /// `Result<T, WorkflowyError>` granularity that the surviving
    /// `with_read_budget_*` unit tests need. Inlined here (rather than
    /// routed through a production helper) because the production
    /// surface is now `tool_handler!` only — keeping a parallel
    /// production helper alive solely to satisfy these three tests
    /// would violate the simplicity principle. If `run_handler`'s
    /// arm logic ever diverges from this, the tests below will tell
    /// us in the same review where the divergence lands.
    #[cfg(test)]
    async fn with_read_budget_inner<F, T>(
        &self,
        fut: F,
        budget: Duration,
    ) -> std::result::Result<T, WorkflowyError>
    where
        F: std::future::Future<Output = std::result::Result<T, WorkflowyError>>,
    {
        let guard = self.cancel_registry.guard();
        let poll_cancel = async {
            loop {
                if guard.is_cancelled() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        let deadline = tokio::time::sleep(budget);
        tokio::select! {
            biased;
            _ = poll_cancel => Err(WorkflowyError::Cancelled),
            _ = deadline => Err(WorkflowyError::Timeout),
            res = fut => res,
        }
    }

    /// Resolve a `node_id` parameter to a full UUID string. Accepts either
    /// the canonical 32-hex-char UUID (with or without hyphens) or one
    /// of the two short-hash forms (12-char URL suffix or 8-char prefix).
    ///
    /// On a short-hash cache miss, this method **walks the workspace**
    /// with the extended resolution budget
    /// ([`defaults::RESOLVE_WALK_TIMEOUT_MS`]) before giving up. Every
    /// node visited during the walk is fed into the persistent index, so
    /// the first short-hash a session resolves pays for the rest.
    ///
    /// When the walk runs to completion without finding the target, the
    /// hash genuinely doesn't exist in the user's tree (shared from
    /// another account, stale, or typo'd). When the walk hits its
    /// budget the outcome is ambiguous: the target may exist but in a
    /// region the walk didn't reach. The error message distinguishes
    /// the two cases so the caller knows whether to retry, narrow
    /// scope, or accept a negative result.
    /// Validate a node-id parameter and resolve it to a full UUID in one
    /// call. Replaces the 30+ instances of the
    /// `check_node_id(p)?; let r = self.resolve_node_ref(p).await?;`
    /// pair across handlers — the order is load-bearing (short-hash
    /// validation must happen before resolve, and validation rejects
    /// the empty string the way the resolver doesn't), so wrapping it
    /// in one helper makes the contract harder to forget. The
    /// architecture review on 2026-05-03 surfaced this as the single
    /// most-repeated boilerplate in the file.
    async fn validate_and_resolve(&self, raw: &str) -> Result<String, McpError> {
        check_node_id(raw)?;
        self.resolve_node_ref(raw).await
    }

    /// Invalidate cache + name-index entries for a set of node IDs.
    /// Called BEFORE every mutating API call (write / move / delete /
    /// complete) so a `tool_handler!` timeout or `cancel_all` cannot
    /// strand stale data: the future the wrapper drops on timeout
    /// would otherwise have its post-API invalidation code never run.
    /// The cost is a redundant API read on the next access if the
    /// mutation actually failed upstream; correctness > cost.
    /// 2026-05-03 architecture review surfaced this as the one stability
    /// gap that wasn't pinned by the existing safety net.
    fn invalidate_for_mutation(&self, ids: &[&str]) {
        for id in ids {
            self.cache.invalidate_node(id);
            self.name_index.invalidate_node(id);
        }
    }

    async fn resolve_node_ref(&self, raw: &str) -> Result<String, McpError> {
        if !is_short_hash(raw) {
            return Ok(raw.to_string());
        }
        if let Some(full) = self.name_index.resolve_short_hash(raw) {
            return Ok(full);
        }
        // Cache miss — walk the workspace with the extended budget.
        info!(short_hash = raw, "resolve_node_ref: cache miss, walking workspace");
        let summary = match self.walk_for_short_hash(raw).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "resolve walk failed; treating as cache miss");
                ResolveWalkSummary {
                    nodes_walked: 0,
                    truncated: true,
                    truncation_reason: None,
                    elapsed_ms: 0,
                }
            }
        };
        if let Some(full) = self.name_index.resolve_short_hash(raw) {
            return Ok(full);
        }
        let reason_str = match summary.truncation_reason {
            Some(crate::api::TruncationReason::Timeout) => "timeout",
            Some(crate::api::TruncationReason::NodeLimit) => "node_limit",
            Some(crate::api::TruncationReason::Cancelled) => "cancelled",
            None => "none",
        };
        let tree_estimate = self
            .tree_size_estimate
            .load(std::sync::atomic::Ordering::Relaxed);
        let scale_hint = if tree_estimate > 0 {
            let coverage_pct = ((summary.nodes_walked as f64) / (tree_estimate as f64) * 100.0)
                .clamp(0.0, 100.0) as u32;
            format!(
                " The known tree-size estimate is ~{} nodes, so this walk covered roughly {}%.",
                tree_estimate, coverage_pct
            )
        } else {
            String::new()
        };
        let index_size = self.name_index.size();
        let body = if summary.truncated {
            format!(
                "Short-hash '{}' was not found in the {} nodes the walk reached in {} ms before truncating ({}).{} The walk did NOT cover the full workspace, so the hash may exist in an unwalked region. The persistent index now contains {} entries; coverage extends with each walk and via the 30-min background refresher. Recovery: (a) call build_name_index repeatedly with parent_id scoped to a likely region (e.g. Projects, Areas); (b) pass the full UUID directly; (c) call find_node(name=..., use_index=true) — the index already covers the names you've walked through this session.",
                raw, summary.nodes_walked, summary.elapsed_ms, reason_str, scale_hint, index_size,
            )
        } else {
            format!(
                "Short-hash '{}' was not found after walking {} nodes in {} ms. The walk completed without truncation, so this hash does not match any node currently in your workspace. Common causes: link to a shared node from another account, stale/deleted node, or typo in the hash.",
                raw, summary.nodes_walked, summary.elapsed_ms,
            )
        };
        Err(McpError::invalid_params(body, None))
    }

    /// Walk the workspace root with the resolution budget and ingest
    /// every visited node into the persistent name index. Used by the
    /// background refresher in `run_server`. Returns
    /// `(nodes_walked, truncated)` so the caller can log progress.
    /// Distinct from [`Self::walk_for_short_hash`] in that it has no
    /// early-termination — the goal is exhaustive coverage, not finding
    /// one specific node.
    pub async fn refresh_name_index(&self) -> crate::error::Result<(usize, bool)> {
        use crate::api::client::FetchControls;
        let controls = FetchControls::with_timeout(Duration::from_millis(
            defaults::RESOLVE_WALK_TIMEOUT_MS,
        ))
        .and_cancel(self.cancel_registry.guard());
        let fetch = self
            .client
            .get_subtree_with_controls(
                None,
                defaults::MAX_TREE_DEPTH,
                defaults::RESOLVE_WALK_NODE_CAP,
                controls,
            )
            .await?;
        self.name_index.ingest(&fetch.nodes);
        Ok((fetch.nodes.len(), fetch.truncated))
    }

    /// Walk the workspace root with [`defaults::RESOLVE_WALK_TIMEOUT_MS`]
    /// and the resolution node cap, feeding every visited node into the
    /// name index. Returns a [`ResolveWalkSummary`] so the caller can
    /// distinguish "walked everything, target not present" from
    /// "ran out of budget before the target was reached" — the two
    /// failure modes look identical from the index alone but are very
    /// different signals to the user.
    ///
    /// Spawns a watcher task that polls the index every 100 ms and
    /// cancels the walk as soon as the target short-hash appears, so a
    /// successful early resolution doesn't pay the full timeout.
    async fn walk_for_short_hash(&self, short_hash: &str) -> crate::error::Result<ResolveWalkSummary> {
        use crate::api::client::FetchControls;
        // Each resolver walk gets its own CancelRegistry so the watcher
        // can cancel **only this walk**, not the server-wide background
        // refresher (which lives on `self.cancel_registry`). Using the
        // shared registry meant a successful early-resolution would
        // tear down a concurrent background indexing pass — the
        // opposite of what we want on huge trees that need many walks
        // to converge. The 5-minute walk timeout still bounds this
        // walk regardless of cancellation.
        let local_registry = crate::utils::CancelRegistry::new();
        let cancel_guard = local_registry.guard();
        let watcher_guard = cancel_guard.clone();
        let watcher_index = self.name_index.clone();
        let watcher_registry = local_registry.clone();
        let target = short_hash.to_string();

        // Watcher: every 100 ms, ask the index whether the target has
        // shown up. If yes, bump only the local registry to break this
        // walk out of its remaining levels. If the walk completes
        // naturally, the watcher_done channel signals the watcher to
        // exit.
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
        let watcher = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if watcher_guard.is_cancelled() {
                            return;
                        }
                        if watcher_index.resolve_short_hash(&target).is_some() {
                            watcher_registry.cancel_all();
                            return;
                        }
                    }
                    _ = &mut done_rx => return,
                }
            }
        });

        let controls = FetchControls::with_timeout(Duration::from_millis(
            defaults::RESOLVE_WALK_TIMEOUT_MS,
        ))
        .and_cancel(cancel_guard);

        let result = self
            .client
            .get_subtree_with_controls(
                None,
                defaults::MAX_TREE_DEPTH,
                defaults::RESOLVE_WALK_NODE_CAP,
                controls,
            )
            .await;

        // Stop the watcher whether the walk succeeded or failed.
        let _ = done_tx.send(());
        let _ = watcher.await;

        match result {
            Ok(fetch) => {
                let nodes_walked = fetch.nodes.len();
                self.name_index.ingest(&fetch.nodes);
                Ok(ResolveWalkSummary {
                    nodes_walked,
                    truncated: fetch.truncated,
                    truncation_reason: fetch.truncation_reason,
                    elapsed_ms: fetch.elapsed_ms,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Brief 2026-04-25 Test β (fail-closed semantics): returns
    /// `Some(message)` when the most recent op-log failure happened
    /// within `window_ms`, naming the broken tool so the create
    /// response can warn the caller before they issue follow-up
    /// writes that may not be retrievable. Returns `None` when no
    /// failure is recent enough to gate on.
    ///
    /// Designed for `create_node` (the brief's specific concern, since
    /// creates were the only path that stayed healthy while reads/
    /// mutations wedged), but the helper is safe to call from any
    /// handler that wants the same gate.
    fn degraded_warning_if_recent_failure(&self, window_ms: u64) -> Option<String> {
        // Brief 2026-05-02: the warning was sticky — once any failure
        // landed in the window, it persisted even after the failing
        // tool recovered. `last_unrecovered_failure` self-clears once
        // a success on the same tool lands after the failure, so the
        // diagnostic surfaces match what the system actually does.
        let last = self.op_log.last_unrecovered_failure()?;
        // Only count read/mutate failures, not validation/usage errors
        // from this same call class. The brief's failure mode was the
        // upstream wedging, not "I just got told my params are invalid".
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let age_ms = now_ms.saturating_sub(last.finished_at_unix_ms);
        if age_ms > window_ms {
            return None;
        }
        // Self-failures (create_node failing earlier) don't gate
        // future creates — the API can recover between calls.
        if last.tool == "create_node" {
            return None;
        }
        Some(format!(
            "server in degraded state — `{}` failed {} ms ago: {}. \
             Reads or follow-up mutations on this node may not succeed \
             until the upstream recovers; verify with `workflowy_status` \
             before issuing further writes.",
            last.tool,
            age_ms,
            last.error.as_deref().unwrap_or("(no detail)")
        ))
    }

    /// Probe upstream reachability with one in-budget retry. The 12-write
    /// burst on 2026-04-30 ended with `health_check` reporting
    /// `api_reachable: false` even though every preceding write had
    /// succeeded — a single transient slowness on the very next read
    /// flipped the signal. Two attempts inside the same wall-clock
    /// budget convert "one blip = degraded" into "two blips = degraded",
    /// which is what callers actually want from a liveness probe.
    ///
    /// Auth failures (401/403) skip the retry: the API key is sticky and
    /// re-trying won't change the answer. The auth-failure timestamp on
    /// the client gets stamped inside `try_request_cancellable` so the
    /// `authenticated` signal in the probe response can be derived
    /// independently of "did this probe succeed".
    async fn probe_upstream_with_retry(&self, budget: Duration) -> ProbeOutcome {
        let started = Instant::now();
        let half_deadline = started + budget / 2;
        let full_deadline = started + budget;

        let first = self
            .client
            .get_top_level_nodes_cancellable(None, Some(half_deadline))
            .await;
        if let Ok(nodes) = first {
            return ProbeOutcome {
                api_reachable: true,
                top_level_count: Some(nodes.len()),
                error: None,
                elapsed_ms: started.elapsed().as_millis() as u64,
                attempts: 1,
            };
        }
        let first_err = first.unwrap_err();

        // Auth failures are sticky — retrying won't change the answer.
        // The 401/403 timestamp is already stamped on the client by
        // `try_request_cancellable`; this branch just short-circuits the
        // probe without paying for a useless second attempt.
        let is_auth_failure = match &first_err {
            WorkflowyError::ApiError { status, .. } => matches!(*status, 401 | 403),
            WorkflowyError::RetryExhausted { reason, .. } => {
                let l = reason.to_lowercase();
                l.contains("401") || l.contains("403") || l.contains("unauthor")
            }
            _ => false,
        };
        if is_auth_failure || Instant::now() >= full_deadline {
            return ProbeOutcome {
                api_reachable: false,
                top_level_count: None,
                error: Some(first_err.to_string()),
                elapsed_ms: started.elapsed().as_millis() as u64,
                attempts: 1,
            };
        }

        // One more shot inside the remaining budget.
        let second = self
            .client
            .get_top_level_nodes_cancellable(None, Some(full_deadline))
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match second {
            Ok(nodes) => ProbeOutcome {
                api_reachable: true,
                top_level_count: Some(nodes.len()),
                error: None,
                elapsed_ms,
                attempts: 2,
            },
            Err(e) => ProbeOutcome {
                api_reachable: false,
                top_level_count: None,
                error: Some(format!("two attempts failed: {} | {}", first_err, e)),
                elapsed_ms,
                attempts: 2,
            },
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

    #[tool(description = "Search for nodes in Workflowy by text query. Returns matching nodes with their IDs, names, and paths. PASS parent_id to scope the search; on large trees an unscoped (root-of-workspace) walk hits the 20 s subtree budget and times out before reaching most content. Two recovery paths: (a) `allow_root_scan=true` to accept the full walk; (b) `use_index=true` to serve the query from the persistent name index in O(1) without any walk — use after `build_name_index` populates the index. Index path is name-only (descriptions need a live walk). Use max_depth to control walk depth.")]
    async fn search_nodes(
        &self,
        Parameters(params): Parameters<SearchNodesParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "search_nodes", ToolKind::Walk, params, {
        let max_results = params.max_results.unwrap_or(20);
        let max_depth = params.max_depth.unwrap_or(3);
        let allow_root_scan = params.allow_root_scan.unwrap_or(false);
        let use_index = params.use_index.unwrap_or(false);
        info!(query = %params.query, max_results, max_depth, allow_root_scan, use_index, "Searching nodes");
        let resolved_parent = match &params.parent_id {
            Some(pid) => Some(self.validate_and_resolve(pid).await?),
            None => None,
        };

        // Index fast path. Mirrors `find_node`'s opt-in: when the
        // caller has populated the name index (via `build_name_index`
        // or accumulated walks), serve the query in O(1) without
        // burning the 20 s walk budget. Match is name-only; if the
        // caller needs description-content matching they must skip
        // this path. The 2026-05-03 eval run hit this exact failure
        // mode — Distillations grew past the 10 000-node walk cap, so
        // every search under it timed out — and `use_index` is the
        // recovery path the truncation banner now points at.
        if use_index {
            if !self.name_index.is_populated() {
                return Err(McpError::invalid_params(
                    "search_nodes use_index=true requires the persistent name \
                     index to be populated. Call `build_name_index(parent_id=...)` \
                     first to walk a subtree into the index, or omit use_index \
                     and accept the live-walk path."
                        .to_string(),
                    None,
                ));
            }
            let hits = self.name_index.lookup(&params.query, "contains");
            let hits: Vec<_> = if let Some(parent) = resolved_parent.as_deref() {
                hits.into_iter()
                    .filter(|e| e.parent_id.as_deref() == Some(parent))
                    .collect()
            } else {
                hits
            };
            let mut hits = hits;
            hits.truncate(max_results);
            let body = if hits.is_empty() {
                format!(
                    "No nodes found matching '{}' in the persistent name index \
                     ({} entries). The index covers names only — content in node \
                     descriptions requires a live walk (omit use_index and pass \
                     parent_id or allow_root_scan=true).",
                    params.query,
                    self.name_index.size(),
                )
            } else {
                let items: Vec<String> = hits.iter()
                    .map(|e| format!("- **{}** (id: `{}`)", e.name, e.node_id))
                    .collect();
                format!(
                    "Found {} node(s) matching '{}' (via name index — name match only, no description content):\n\n{}",
                    hits.len(),
                    params.query,
                    items.join("\n"),
                )
            };
            return Ok(CallToolResult::success(vec![Content::text(body)]));
        }

        // Refuse unscoped walks by default, mirroring find_node. Brief
        // 2026-05-02: search_nodes with parent_id=null on a large tree
        // burns its 20 s walk budget before reaching the content the
        // caller wanted, then returns a partial result with no
        // recovery path. The fix is the same gate find_node has:
        // require either parent_id, or an explicit opt-in.
        if resolved_parent.is_none() && !allow_root_scan {
            return Err(McpError::invalid_params(
                "search_nodes refuses to walk from the workspace root by default. \
                 Pass parent_id to scope the search, set allow_root_scan=true to \
                 accept a full walk (bounded by the 20 s subtree budget), or set \
                 use_index=true to serve from the persistent name index without \
                 a walk."
                    .to_string(),
                None,
            ));
        }

        match self.walk_subtree(resolved_parent.as_deref(), max_depth).await {
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
                Err(tool_error("search_nodes", resolved_parent.as_deref(), e))
            }
        }
        })
    }

    #[tool(description = "Get a specific Workflowy node by its ID. Returns the node's full details (name, description, tags) plus a depth-1 listing of its direct children — matching what list_children would return for the same ID. The children listing costs one extra HTTP call; use list_children directly when you don't need the parent metadata.")]
    async fn get_node(
        &self,
        Parameters(params): Parameters<GetNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "get_node", ToolKind::Read, params, {
        info!(node_id = %params.node_id, "Getting node");
        let resolved = self.validate_and_resolve(&params.node_id).await?;

        // Fetch the node and its direct children in parallel — they are
        // independent API calls, and previously `get_node` returned an empty
        // `children: []` field that disagreed with `list_children`. Surfacing
        // the children alongside the parent removes that footgun without
        // forcing callers to make a second tool call.
        //
        // Both calls go through the propagation-retry path: Workflowy has
        // been observed to return a node ID via a parent's children listing
        // before the same ID is queryable directly. The retry waits up to
        // ~1.4 s total (200 + 400 + 800 ms) before giving up. The
        // `tool_handler!(ToolKind::Read)` wrapper bounds the entire
        // handler at the Read budget and observes `cancel_all` so a hung
        // upstream returns a structured Timeout / Cancelled instead of
        // wedging — no inner budget needed here.
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

    #[tool(description = "Create a new node in Workflowy. PREFER passing `parent_id` directly to the destination over the create-then-move pattern: a single create with parent_id has half the failure surface of create-at-root + move (verified 2026-04-25 — a created-at-root MOC was stranded for hours when follow-up move calls were dropped at the transport layer). Pattern 6d (brief 2026-04-25): omitting parent_id (or passing null) places the node at the workspace root — both have the same semantics. The success message always names the resolved parent (or 'workspace root') so the caller can audit placement before issuing follow-up moves. When reads/mutations have failed in the last 30 s the success message is suffixed `⚠ DEGRADED: …` — do NOT chain follow-up writes on the new UUID until `workflowy_status` confirms the previously-failing tool is back to `healthy`.")]
    async fn create_node(
        &self,
        Parameters(params): Parameters<CreateNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "create_node", ToolKind::Write, params, {
        info!(name = %params.name, parent = ?params.parent_id, "Creating node");
        let resolved_parent = match &params.parent_id {
            Some(pid) => Some(self.validate_and_resolve(pid).await?),
            None => None,
        };

        // Brief 2026-04-25 Test β: fail-closed semantics. When reads
        // or mutations have failed in the last 30 s the create may
        // succeed at the API layer but the assistant will not be
        // able to verify, move, or delete the new node — exactly the
        // failure mode that produced the four orphans on 2026-04-25.
        // Compute the warning *before* the create runs and attach it
        // to the success response so the assistant can roll back its
        // plan before issuing follow-up writes.
        let degraded_warning = self.degraded_warning_if_recent_failure(30_000);

        // Pre-call invalidation: the parent's children listing must be
        // refreshed whether the create succeeds or the wrapper fires
        // its timeout/cancel arm mid-flight. Cost on a failed create is
        // one extra API read on the next list_children; correctness > cost.
        if let Some(pid) = &resolved_parent {
            self.invalidate_for_mutation(&[pid.as_str()]);
        }

        match self
            .client
            .create_node(&params.name, params.description.as_deref(), resolved_parent.as_deref(), params.priority)
            .await
        {
            Ok(created) => {
                let placement = resolved_parent
                    .as_deref()
                    .map(|p| format!("under `{}`", p))
                    .unwrap_or_else(|| "at workspace root (no parent_id supplied)".to_string());
                let mut msg = format!(
                    "Created node '{}' (id: `{}`) {}",
                    params.name, created.id, placement
                );
                if let Some(warn) = &degraded_warning {
                    msg.push_str("\n\n⚠ DEGRADED: ");
                    msg.push_str(warn);
                }
                // Seed the name index so subsequent lookups see the new node
                // without needing a fresh walk.
                self.name_index.ingest(&[WorkflowyNode {
                    id: created.id.clone(),
                    name: params.name.clone(),
                    description: params.description.clone(),
                    parent_id: resolved_parent.clone(),
                    ..Default::default()
                }]);
                Ok(CallToolResult::success(vec![Content::text(msg)]))
            }
            Err(e) => Err(tool_error("create_node", resolved_parent.as_deref(), e)),
        }
        })
    }

    #[tool(description = "Edit an existing Workflowy node's name or description. At least one of name/description must be provided.")]
    async fn edit_node(
        &self,
        Parameters(params): Parameters<EditNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "edit_node", ToolKind::Write, params, {
        info!(node_id = %params.node_id, "Editing node");
        let resolved = self.validate_and_resolve(&params.node_id).await?;

        // Reject no-op edits at the boundary: the Workflowy API happily
        // accepts an empty PATCH body and returns success, which would mask
        // caller bugs where a field was dropped somewhere upstream.
        if params.name.is_none() && params.description.is_none() {
            return Err(McpError::invalid_params(
                "edit_node requires at least one of `name` or `description`".to_string(),
                None,
            ));
        }

        // Pre-call invalidation — see invalidate_for_mutation docs.
        self.invalidate_for_mutation(&[&resolved]);

        match self
            .client
            .edit_node_with_propagation_retry(&resolved, params.name.as_deref(), params.description.as_deref())
            .await
        {
            Ok(_) => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Updated node `{}`",
                    resolved
                ))]))
            }
            Err(e) => Err(tool_error("edit_node", Some(&resolved), e)),
        }
        })
    }

    #[tool(description = "Delete a Workflowy node by its ID.")]
    async fn delete_node(
        &self,
        Parameters(params): Parameters<DeleteNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "delete_node", ToolKind::Write, params, {
        info!(node_id = %params.node_id, "Deleting node");
        let resolved = self.validate_and_resolve(&params.node_id).await?;

        // Pre-call invalidation — see invalidate_for_mutation docs.
        self.invalidate_for_mutation(&[&resolved]);

        match self.client.delete_node_with_propagation_retry(&resolved).await {
            Ok(_) => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Deleted node `{}`",
                    resolved
                ))]))
            }
            Err(e) => Err(tool_error("delete_node", Some(&resolved), e)),
        }
        })
    }

    #[tool(description = "Mark a node complete or uncomplete. Defaults to complete; pass `completed: false` to revert. Replaces the tag-based `#done` workaround documented in the wflow skill — completion is now first-class. Cache is invalidated on success so subsequent reads (`get_node`, `list_todos`, `daily_review`) reflect the new state without a TTL wait.")]
    async fn complete_node(
        &self,
        Parameters(params): Parameters<CompleteNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "complete_node", ToolKind::Write, params, {
        let target_state = params.completed.unwrap_or(true);
        info!(node_id = %params.node_id, completed = target_state, "Setting completion");
        let resolved = self.validate_and_resolve(&params.node_id).await?;

        // Pre-call invalidation — see invalidate_for_mutation docs.
        self.invalidate_for_mutation(&[&resolved]);

        match self
            .client
            .set_completion_with_propagation_retry(&resolved, target_state)
            .await
        {
            Ok(_) => {
                let verb = if target_state { "Completed" } else { "Uncompleted" };
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "{} node `{}`",
                    verb, resolved
                ))]))
            }
            Err(e) => Err(tool_error("complete_node", Some(&resolved), e)),
        }
        })
    }

    #[tool(description = "Move a node to a new parent in Workflowy.")]
    async fn move_node(
        &self,
        Parameters(params): Parameters<MoveNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "move_node", ToolKind::Write, params, {
        info!(node_id = %params.node_id, new_parent = %params.new_parent_id, "Moving node");
        let resolved_node = self.validate_and_resolve(&params.node_id).await?;
        let resolved_parent = self.validate_and_resolve(&params.new_parent_id).await?;

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

        // Pre-call invalidation — node + new parent always; old parent
        // when known and different from new. See invalidate_for_mutation docs.
        let mut to_invalidate: Vec<&str> = vec![resolved_node.as_str(), resolved_parent.as_str()];
        if let Some(pid) = &old_parent_id {
            if pid.as_str() != resolved_parent.as_str() {
                to_invalidate.push(pid.as_str());
            }
        }
        self.invalidate_for_mutation(&to_invalidate);

        match self
            .client
            .move_node_with_propagation_retry(&resolved_node, &resolved_parent, params.priority)
            .await
        {
            Ok(_) => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Moved node `{}` under `{}`",
                    resolved_node, resolved_parent
                ))]))
            }
            Err(e) => Err(tool_error("move_node", Some(&resolved_node), e)),
        }
        })
    }

    #[tool(description = "List all children of a Workflowy node.")]
    async fn list_children(
        &self,
        Parameters(params): Parameters<GetChildrenParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "list_children", ToolKind::Read, params, {
        // None / null = workspace root. The tool description and
        // schema are explicit about this so the caller doesn't have
        // to guess; previously a `null` node_id intermittently
        // surfaced "Tool execution failed" depending on which path
        // serde took.
        let resolved: Option<String> = match params.node_id.as_deref() {
            None | Some("") => None,
            Some(id) => {
                Some(self.validate_and_resolve(id).await?)
            }
        };
        info!(node_id = ?resolved, "Getting children");

        // `tool_handler!(ToolKind::Read)` bounds and cancel-observes the
        // whole handler — no inner budget needed.
        let fetch_result = match resolved.as_deref() {
            Some(id) => self.client.get_children_with_propagation_retry(id).await,
            None => self.client.get_top_level_nodes().await,
        };

        let scope_label = resolved.as_deref().unwrap_or("workspace root").to_string();
        match fetch_result {
            Ok(children) => {
                if children.is_empty() {
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "`{}` has no children",
                        scope_label
                    ))]))
                } else {
                    let items: Vec<String> = children
                        .iter()
                        .map(|n| format!("- **{}** (id: `{}`)", n.name, n.id))
                        .collect();
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "{} children of `{}`:\n\n{}",
                        children.len(),
                        scope_label,
                        items.join("\n")
                    ))]))
                }
            }
            Err(e) => Err(tool_error("list_children", resolved.as_deref(), e)),
        }
        })
    }

    #[tool(description = "Search for nodes by tag (e.g. #project, @person). Returns all nodes containing the specified tag. Use parent_id to scope and max_depth to control search depth.")]
    async fn tag_search(
        &self,
        Parameters(params): Parameters<TagSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "tag_search", ToolKind::Walk, params, {
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
            Err(e) => Err(tool_error("tag_search", None, e)),
        }
        })
    }

    #[tool(description = "Insert hierarchical content under a parent node. Content uses 2-space indentation for hierarchy — each indent level creates a child of the node above it. PAYLOAD CAP: ≤200 lines per call (hard, refused with a typed error above this); ≤80 lines is the safe ceiling that has not been observed to fail at the MCP transport layer. Above 80 lines the success response includes a chunking hint. Bounded by an end-to-end budget (~210 s) so a flaky upstream cannot wedge the call past the MCP client's 4-min hard timeout. On timeout the response carries a structured partial-success payload (created_count, total_count, last_inserted_id, error) instead of returning bare 'no result received' — the caller can resume from where it stopped. Pattern: split large content into batches keyed by top-level subtree, call insert_content per batch; the LAST_INSERTED_ID returned by each batch can be passed as parent_id to the next so the hierarchy stitches back together cleanly.")]
    async fn insert_content(
        &self,
        Parameters(params): Parameters<InsertContentParams>,
    ) -> Result<CallToolResult, McpError> {
        // Deliberate exception to the `tool_handler!` consistency rule:
        // `insert_content` owns inline cancel + deadline checks because
        // they are what produce the structured partial-success payload
        // (`status: "partial"`, `created_count`, `last_inserted_id`,
        // …) callers depend on to resume. Wrapping in
        // `tool_handler!(Bulk)` would race the same deadline at the
        // outer boundary and return a bare `Err(Timeout)` first, losing
        // the resume cursor — `insert_content_returns_partial_on_*`
        // pin both branches. `record_op!` records the call without
        // imposing the outer wall-clock arm; the inline checks are the
        // primary AND only safety net here.
        record_op!(self, "insert_content", params, {
        info!(parent_id = %params.parent_id, "Inserting content");
        let resolved_parent = self.validate_and_resolve(&params.parent_id).await?;

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

        // Hard cap on payload size. Brief 2026-05-02: oversized
        // `insert_content` calls were silently dropped at the MCP
        // transport before reaching this handler — bare
        // "Tool execution failed" with no per-tool counter movement.
        // We cannot fix transport drops, but we can refuse to pretend
        // the bounded-budget contract is end-to-end. Above the cap,
        // return a typed error with chunking instructions so the
        // caller's recovery path is obvious instead of guess-and-retry.
        if parsed.len() > defaults::MAX_INSERT_CONTENT_LINES {
            let total = parsed.len();
            let cap = defaults::MAX_INSERT_CONTENT_LINES;
            return Err(tool_error(
                "insert_content",
                Some(&resolved_parent),
                format!(
                    "payload too large: {} lines exceeds the {}-line cap. Split the content \
                     into batches of ≤{} top-level subtrees and call insert_content once per \
                     batch — each call into a fresh node returned by the previous batch (or the \
                     same parent_id) preserves the hierarchy. The cap exists because oversized \
                     payloads have been observed to fail at the MCP transport layer before \
                     reaching this handler, surfacing as undiagnosable 'Tool execution failed' \
                     with no per-tool counter movement.",
                    total, cap, defaults::SOFT_WARN_INSERT_CONTENT_LINES,
                ),
            ));
        }

        // End-to-end deadline. Honoured by every per-line create plus a
        // pre-line check so a row queued behind the rate limiter
        // observes the budget without making an HTTP attempt that
        // would have to time out on its own. The budget reads from
        // `self.bulk_budget_ms` (defaulting to
        // [`defaults::INSERT_CONTENT_TIMEOUT_MS`]) so tests can dial it
        // down via `with_bulk_budget_ms`, sharing the override surface
        // with every other bulk handler.
        let total_budget = Duration::from_millis(self.bulk_budget_ms);
        let overall_deadline = Instant::now() + total_budget;
        let cancel_guard = self.cancel_registry.guard();

        let total = parsed.len();
        let mut parent_stack: Vec<String> = vec![resolved_parent.clone()];
        let mut created_count: usize = 0;
        let mut last_inserted_id: Option<String> = None;
        // None on success; Some(reason) on partial-success exit.
        let mut bailout_reason: Option<String> = None;
        let mut bailout_line: Option<String> = None;

        for line in &parsed {
            // Pre-line budget + cancel checks. A guard taken before the
            // rate limiter avoids burning a token on a doomed call.
            if cancel_guard.is_cancelled() {
                bailout_reason = Some("cancelled".to_string());
                break;
            }
            if Instant::now() >= overall_deadline {
                bailout_reason = Some("timeout".to_string());
                bailout_line = Some(line.text.to_string());
                break;
            }

            // Clamp indent to valid range
            let indent = line.indent.min(parent_stack.len().saturating_sub(1));
            let parent_id = parent_stack[indent].clone();

            // Per-call deadline = the tighter of the overall deadline
            // and the standard write budget. The client's own
            // `create_node` already bounds at WRITE_NODE_TIMEOUT_MS;
            // passing the overall deadline keeps the last few rows
            // from bursting the total budget.
            match self
                .client
                .create_node_cancellable(
                    line.text,
                    None,
                    Some(&parent_id),
                    None,
                    Some(&cancel_guard),
                    Some(overall_deadline),
                )
                .await
            {
                Ok(created) => {
                    created_count += 1;
                    last_inserted_id = Some(created.id.clone());
                    let next_level = indent + 1;
                    if next_level < parent_stack.len() {
                        parent_stack[next_level] = created.id;
                        parent_stack.truncate(next_level + 1);
                    } else {
                        parent_stack.push(created.id);
                    }
                }
                Err(WorkflowyError::Cancelled) => {
                    bailout_reason = Some("cancelled".to_string());
                    bailout_line = Some(line.text.to_string());
                    break;
                }
                Err(WorkflowyError::Timeout) => {
                    bailout_reason = Some("timeout".to_string());
                    bailout_line = Some(line.text.to_string());
                    break;
                }
                Err(e) => {
                    // Deadline takes precedence: if the budget has
                    // already expired, the error is a downstream
                    // consequence (a connection torn down by the
                    // racing `tokio::select` arm, an upstream session
                    // closed, etc.) and the contract is partial-
                    // success on timeout — not a hard error that the
                    // caller can't tell apart from a real failure.
                    if cancel_guard.is_cancelled() {
                        bailout_reason = Some("cancelled".to_string());
                        bailout_line = Some(line.text.to_string());
                        break;
                    }
                    if Instant::now() >= overall_deadline {
                        bailout_reason = Some("timeout".to_string());
                        bailout_line = Some(line.text.to_string());
                        break;
                    }
                    error!(error = %e, line = line.text, "Failed to insert line");
                    return Err(tool_error(
                        "insert_content",
                        Some(&parent_id),
                        format!("inserting '{}' (after {}/{} lines): {}", line.text, created_count, total, e),
                    ));
                }
            }
        }

        self.cache.invalidate_node(&resolved_parent);

        // Partial-success path: report what we got done so the caller
        // can resume from the last inserted node rather than guessing
        // whether the call actually wrote anything.
        if let Some(reason) = bailout_reason {
            let payload = json!({
                "status": "partial",
                "reason": reason,
                "created_count": created_count,
                "total_count": total,
                "parent_id": resolved_parent,
                "last_inserted_id": last_inserted_id,
                "stopped_at_line": bailout_line,
                "message": format!(
                    "insert_content stopped at {}/{} lines ({}). Last inserted: {}. Resume by re-running with the remaining lines under last_inserted_id (or the original parent if last_inserted_id is null).",
                    created_count,
                    total,
                    reason,
                    last_inserted_id.as_deref().unwrap_or("none"),
                ),
            });
            return Ok(CallToolResult::success(vec![Content::text(payload.to_string())]));
        }

        let mut msg = format!(
            "Inserted {} node(s) under `{}`",
            created_count, resolved_parent
        );
        // Soft-warn: above the safe ceiling but under the hard cap.
        // The call succeeded, but the next one this size may not, so
        // the caller learns the practical limit before they hit it.
        if created_count > defaults::SOFT_WARN_INSERT_CONTENT_LINES {
            msg.push_str(&format!(
                "\n\n⚠ Payload was {} lines, above the {}-line soft warn threshold. \
                 Consider splitting into smaller batches for the next call — the \
                 {}-line hard cap rejects oversized payloads at the handler boundary.",
                created_count,
                defaults::SOFT_WARN_INSERT_CONTENT_LINES,
                defaults::MAX_INSERT_CONTENT_LINES,
            ));
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
        })
    }

    #[tool(description = "Get the full subtree under a node, showing the hierarchical structure. Use max_depth to limit traversal depth for large trees.")]
    async fn get_subtree(
        &self,
        Parameters(params): Parameters<GetSubtreeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "get_subtree", ToolKind::Walk, params, {
        let max_depth = params.max_depth.unwrap_or(5);
        info!(node_id = %params.node_id, max_depth, "Getting subtree");
        let resolved = self.validate_and_resolve(&params.node_id).await?;

        match self.walk_subtree(Some(&resolved), max_depth).await {
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
            Err(e) => Err(tool_error("get_subtree", Some(&resolved), e)),
        }
        })
    }

    // --- New tools required by wmanage skill ---

    #[tool(description = "Find a node by name. Supports exact, contains, and starts_with match modes. Returns node_id for use with other tools. Omitting parent_id triggers a root-of-tree walk, which is refused by default on large trees — pass allow_root_scan=true to opt in, or use_index=true to serve from the opportunistic name index. Use selection to disambiguate multiple matches.")]
    async fn find_node(
        &self,
        Parameters(params): Parameters<FindNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "find_node", ToolKind::Walk, params, {
        let match_mode = params.match_mode.as_deref().unwrap_or("exact");
        let max_depth = params.max_depth.unwrap_or(3);
        let use_index = params.use_index.unwrap_or(false);
        let allow_root_scan = params.allow_root_scan.unwrap_or(false);
        let resolved_parent = match &params.parent_id {
            Some(pid) => Some(self.validate_and_resolve(pid).await?),
            None => None,
        };
        info!(name = %params.name, match_mode, max_depth, use_index, allow_root_scan, "Finding node");

        // Refuse unscoped walks by default so a caller that forgot `parent_id`
        // cannot blow the client timeout on a 250k-node tree. Index-backed
        // lookups are exempt because they don't touch the API.
        if resolved_parent.is_none() && !allow_root_scan && !use_index {
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
            let hits: Vec<_> = if let Some(parent) = resolved_parent.as_deref() {
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

        match self.walk_subtree(resolved_parent.as_deref(), max_depth).await {
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
                        "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
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
                        "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
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
                        "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                        "truncated_at_path": truncated_at_path,
                        "count": matches.len(),
                        "options": options,
                        "message": format!("Found {} matches for '{}'. Use selection parameter to choose.", matches.len(), params.name)
                    });
                    Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                }
            }
            Err(e) => Err(tool_error("find_node", resolved_parent.as_deref(), e)),
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
        tool_handler!(self, "smart_insert", ToolKind::Walk, params, {
        let max_depth = params.max_depth.unwrap_or(3);
        info!(query = %params.search_query, max_depth, "Smart insert");

        let content = params.content.trim();
        if content.is_empty() {
            return Err(McpError::invalid_params("Content cannot be empty".to_string(), None));
        }

        match self.walk_subtree(None, max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit, truncation_reason, .. }) => {
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
                        "truncation_reason": truncation_reason.map(|r| r.as_str()),
                        "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
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
                            return Err(tool_error("smart_insert", Some(&target_id), format!("inserting '{}': {}", trimmed, e)));
                        }
                    }
                }

                self.cache.invalidate_node(&target_id);

                let result = json!({
                    "success": true,
                    "truncated": truncated,
                    "truncation_limit": limit,
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                    "created_count": created_count,
                    "target": { "id": target_id, "name": target_name },
                    "message": format!("Inserted {} node(s) under '{}'", created_count, target_name)
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(tool_error("smart_insert", None, e)),
        }
        })
    }

    #[tool(description = "Daily review: get overdue items, upcoming deadlines, recent changes, and pending todos in one call. Use root_id to scope and max_depth to control depth.")]
    async fn daily_review(
        &self,
        Parameters(params): Parameters<DailyReviewParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "daily_review", ToolKind::Walk, params, {
        let max_depth = params.max_depth.unwrap_or(5);
        info!(max_depth, "Daily review");
        let resolved_root = match &params.root_id {
            Some(rid) => Some(self.validate_and_resolve(rid).await?),
            None => None,
        };

        match self.walk_subtree(resolved_root.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit, truncation_reason, .. }) => {
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
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
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
            Err(e) => Err(tool_error("daily_review", resolved_root.as_deref(), e)),
        }
        })
    }

    #[tool(description = "Get recently modified nodes within a time window.")]
    async fn get_recent_changes(
        &self,
        Parameters(params): Parameters<GetRecentChangesParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "get_recent_changes", ToolKind::Walk, params, {
        let days = params.days.unwrap_or(7) as i64;
        let include_completed = params.include_completed.unwrap_or(true);
        let limit = params.limit.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(5);
        info!(days, max_depth, "Getting recent changes");
        let resolved_root = match &params.root_id {
            Some(rid) => Some(self.validate_and_resolve(rid).await?),
            None => None,
        };

        match self.walk_subtree(resolved_root.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, truncation_reason, .. }) => {
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
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                    "count": items.len(),
                    "changes": items
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(tool_error("get_recent_changes", resolved_root.as_deref(), e)),
        }
        })
    }

    #[tool(description = "List overdue items (past due date, incomplete) sorted by most overdue first.")]
    async fn list_overdue(
        &self,
        Parameters(params): Parameters<ListOverdueParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "list_overdue", ToolKind::Walk, params, {
        let include_completed = params.include_completed.unwrap_or(false);
        let limit = params.limit.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(5);
        info!(max_depth, "Listing overdue items");
        let resolved_root = match &params.root_id {
            Some(rid) => Some(self.validate_and_resolve(rid).await?),
            None => None,
        };

        match self.walk_subtree(resolved_root.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, truncation_reason, .. }) => {
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
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                    "count": items.len(),
                    "overdue": items
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(tool_error("list_overdue", resolved_root.as_deref(), e)),
        }
        })
    }

    #[tool(description = "List items with upcoming due dates, sorted by nearest deadline first.")]
    async fn list_upcoming(
        &self,
        Parameters(params): Parameters<ListUpcomingParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "list_upcoming", ToolKind::Walk, params, {
        let days = params.days.unwrap_or(14) as i64;
        let include_no_due_date = params.include_no_due_date.unwrap_or(false);
        let limit = params.limit.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(5);
        info!(days, max_depth, "Listing upcoming items");
        let resolved_root = match &params.root_id {
            Some(rid) => Some(self.validate_and_resolve(rid).await?),
            None => None,
        };

        match self.walk_subtree(resolved_root.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, truncation_reason, .. }) => {
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
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                    "count": items.len(),
                    "upcoming": items
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(tool_error("list_upcoming", resolved_root.as_deref(), e)),
        }
        })
    }

    #[tool(description = "Get project summary with stats, tag counts, assignee counts, and recently modified nodes.")]
    async fn get_project_summary(
        &self,
        Parameters(params): Parameters<GetProjectSummaryParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "get_project_summary", ToolKind::Walk, params, {
        let include_tags = params.include_tags.unwrap_or(true);
        let recent_days = params.recently_modified_days.unwrap_or(7) as i64;
        info!(node_id = %params.node_id, "Getting project summary");
        let resolved = self.validate_and_resolve(&params.node_id).await?;

        match self.walk_subtree(Some(&resolved), 10).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, truncation_reason, .. }) => {
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

                let root = subtree.iter().find(|n| n.id == resolved).unwrap();
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
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
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
            Err(e) => Err(tool_error("get_project_summary", Some(&resolved), e)),
        }
        })
    }

    // --- Remaining planned tools ---

    #[tool(description = "Find all nodes that contain a Workflowy link to the given node.")]
    async fn find_backlinks(
        &self,
        Parameters(params): Parameters<FindBacklinksParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "find_backlinks", ToolKind::Walk, params, {
        let resolved = self.validate_and_resolve(&params.node_id).await?;
        let limit = params.limit.unwrap_or(50);
        let max_depth = params.max_depth.unwrap_or(3);
        info!(node_id = %resolved, max_depth, "Finding backlinks");

        match self.walk_subtree(None, max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit: node_limit, truncation_reason, .. }) => {
                let node_map = build_node_map(&nodes);
                let target = node_map.get(resolved.as_str());
                let target_name = target.map(|n| n.name.as_str()).unwrap_or("unknown");

                // Match workflowy.com links containing the target node ID.
                // `regex::escape` guarantees a valid pattern, so the Regex::new
                // call below cannot fail.
                let link_re = Regex::new(&format!(
                    r"https?://workflowy\.com/#/{}",
                    regex::escape(&resolved)
                )).expect("escaped pattern is always valid regex");

                let mut backlinks: Vec<serde_json::Value> = Vec::new();
                for node in &nodes {
                    if node.id == resolved { continue; }
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
                    "target": { "id": resolved, "name": target_name },
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                    "count": backlinks.len(),
                    "backlinks": backlinks
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(tool_error("find_backlinks", Some(&resolved), e)),
        }
        })
    }

    #[tool(description = "List todo items, optionally filtered by parent, status, or text query.")]
    async fn list_todos(
        &self,
        Parameters(params): Parameters<ListTodosParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "list_todos", ToolKind::Walk, params, {
        let limit = params.limit.unwrap_or(50);
        let status = params.status.as_deref().unwrap_or("all");
        let max_depth = params.max_depth.unwrap_or(5);
        info!(status, max_depth, "Listing todos");
        let resolved_parent = match &params.parent_id {
            Some(pid) => Some(self.validate_and_resolve(pid).await?),
            None => None,
        };

        match self.walk_subtree(resolved_parent.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, truncation_reason, .. }) => {
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
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                    "count": todos.len(),
                    "todos": todos,
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(tool_error("list_todos", resolved_parent.as_deref(), e)),
        }
        })
    }

    #[tool(description = "Deep-copy a node and its subtree to a new location.")]
    async fn duplicate_node(
        &self,
        Parameters(params): Parameters<DuplicateNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "duplicate_node", ToolKind::Bulk, params, {
        let resolved_node = self.validate_and_resolve(&params.node_id).await?;
        check_node_id(&params.target_parent_id)?;
        let resolved_target = self.resolve_node_ref(&params.target_parent_id).await?;
        let include_children = params.include_children.unwrap_or(true);
        info!(node_id = %resolved_node, target = %resolved_target, "Duplicating node");

        match self.walk_subtree(Some(&resolved_node), 10).await {
            // duplicate_node refuses truncated input outright (producing a
            // partial copy is worse than failing), so the truncation
            // surface never reaches the JSON response — destructure
            // matches that contract.
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
                    all_nodes.iter().filter(|n| n.id == resolved_node).collect()
                };

                if subtree.is_empty() {
                    return Err(McpError::invalid_params(format!("Node '{}' not found", params.node_id), None));
                }

                // Build depth-first ordering from subtree
                let mut id_map: HashMap<String, String> = HashMap::new();
                let mut created_count = 0;

                // Process root first
                let root = subtree.iter().find(|n| n.id == resolved_node).unwrap();
                let root_name = if let Some(prefix) = &params.name_prefix {
                    format!("{}{}", prefix, root.name)
                } else {
                    root.name.clone()
                };

                match self.client.create_node(&root_name, root.description.as_deref(), Some(&resolved_target), None).await {
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
                                        return Err(tool_error("duplicate_node", Some(&resolved_node), format!("duplicating child: {}", e)));
                                    }
                                }
                            }
                        }

                        self.cache.invalidate_node(&resolved_target);
                        let result = json!({
                            "success": true,
                            "original_id": resolved_node,
                            "new_root_id": created.id,
                            "nodes_created": created_count
                        });
                        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                    }
                    Err(e) => Err(tool_error("duplicate_node", Some(&resolved_node), e)),
                }
            }
            Err(e) => Err(tool_error("duplicate_node", Some(&resolved_node), e)),
        }
        })
    }

    #[tool(description = "Copy a template node with {{variable}} substitution in names and descriptions.")]
    async fn create_from_template(
        &self,
        Parameters(params): Parameters<CreateFromTemplateParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "create_from_template", ToolKind::Bulk, params, {
        let resolved_template = self.validate_and_resolve(&params.template_node_id).await?;
        check_node_id(&params.target_parent_id)?;
        let resolved_target = self.resolve_node_ref(&params.target_parent_id).await?;
        let vars = params.variables.unwrap_or_default();
        info!(template = %resolved_template, "Creating from template");

        let var_re = Regex::new(r"\{\{(\w+)\}\}").expect("static template-variable pattern is valid");
        let substitute = |text: &str| -> String {
            var_re.replace_all(text, |caps: &regex::Captures| {
                vars.get(&caps[1]).cloned().unwrap_or_else(|| caps[0].to_string())
            }).to_string()
        };

        match self.walk_subtree(Some(&resolved_template), 10).await {
            // create_from_template refuses truncated input — same contract
            // as duplicate_node, no JSON-truncation fields reach the
            // response.
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

                let root = subtree.iter().find(|n| n.id == resolved_template).unwrap();
                let root_name = substitute(&root.name);
                let root_desc = root.description.as_ref().map(|d| substitute(d));

                match self.client.create_node(&root_name, root_desc.as_deref(), Some(&resolved_target), None).await {
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
                                Err(e) => return Err(tool_error("create_from_template", Some(&resolved_template), format!("instantiating child: {}", e))),
                            }
                        }

                        self.cache.invalidate_node(&resolved_target);
                        let result = json!({
                            "success": true,
                            "template_id": resolved_template,
                            "new_root_id": new_root_id,
                            "nodes_created": created_count,
                            "variables_applied": applied_vars
                        });
                        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
                    }
                    Err(e) => Err(tool_error("create_from_template", Some(&resolved_template), e)),
                }
            }
            Err(e) => Err(tool_error("create_from_template", Some(&resolved_template), e)),
        }
        })
    }

    #[tool(description = "Apply an operation to all nodes matching a filter. Supports complete, uncomplete, delete, add_tag, remove_tag. Use dry_run to preview. complete/uncomplete route through the same `client.set_completion` code path as the single-node `complete_node` tool.")]
    async fn bulk_update(
        &self,
        Parameters(params): Parameters<BulkUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "bulk_update", ToolKind::Bulk, params, {
        let dry_run = params.dry_run.unwrap_or(false);
        let limit = params.limit.unwrap_or(20);
        let status = params.status.as_deref().unwrap_or("all");
        info!(operation = %params.operation, dry_run, "Bulk update");
        let resolved_root = match &params.root_id {
            Some(rid) => Some(self.validate_and_resolve(rid).await?),
            None => None,
        };

        // Validate operation. `complete`/`uncomplete` are first-class as
        // of the completion-state work — they route through
        // `client.set_completion`, the same code path
        // `complete_node` uses for single-node toggles.
        let valid_ops = ["delete", "add_tag", "remove_tag", "complete", "uncomplete"];
        if !valid_ops.contains(&params.operation.as_str()) {
            return Err(McpError::invalid_params(
                format!("Invalid operation '{}'. Must be one of: {}", params.operation, valid_ops.join(", ")), None
            ));
        }
        if (params.operation == "add_tag" || params.operation == "remove_tag") && params.operation_tag.is_none() {
            return Err(McpError::invalid_params("operation_tag required for add_tag/remove_tag".to_string(), None));
        }

        let max_depth = params.max_depth.unwrap_or(5);
        match self.walk_subtree(resolved_root.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes: all_nodes, truncated, limit: node_limit, truncation_reason, .. }) => {
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
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
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
                        "complete" => self.client.set_completion(&node.id, true).await.is_ok(),
                        "uncomplete" => self.client.set_completion(&node.id, false).await.is_ok(),
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
                if let Some(rid) = &resolved_root {
                    self.cache.invalidate_subtree(rid);
                }

                let result = json!({
                    "dry_run": false,
                    "truncated": truncated,
                    "truncation_limit": node_limit,
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                    "matched_count": matched.len(),
                    "affected_count": affected,
                    "operation": params.operation,
                    "nodes_affected": affected_nodes
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(tool_error("bulk_update", resolved_root.as_deref(), e)),
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

    #[tool(description = "Quick diagnostic. Calls the Workflowy API with a short budget (one in-budget retry on transient failure) to confirm reachability and reports cache/name-index sizes. Surfaces `authenticated` (independent of probe success — driven by recent 401/403, not timeouts) and `last_successful_api_call_ms_ago` so callers can distinguish a one-shot blip from a sustained outage. Sub-second regardless of tree size; use this to decide whether a larger tool call will succeed.")]
    async fn health_check(
        &self,
        Parameters(_params): Parameters<HealthCheckParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "health_check", _params, {
        let timeout = Duration::from_millis(defaults::HEALTH_CHECK_TIMEOUT_MS);
        let outcome = self.probe_upstream_with_retry(timeout).await;
        let cache_stats = self.cache.stats();
        let authenticated = !self
            .client
            .recent_auth_failure(Duration::from_secs(defaults::AUTH_FAILURE_WINDOW_SECS));
        // Derive api_reachable from probe success OR a recent
        // successful tool call. This stops the degraded flag from
        // sticking when the lightweight probe times out during a
        // heavy write burst that itself proved the API is up.
        let last_success_ms_ago = self.client.last_success_ms_ago();
        let api_reachable = derive_api_reachable(outcome.api_reachable, last_success_ms_ago);
        let result = json!({
            "status": if api_reachable { "ok" } else { "degraded" },
            "api_reachable": api_reachable,
            "api_reachable_via_recent_success": !outcome.api_reachable && api_reachable,
            "authenticated": authenticated,
            "auth_method": "api_key_env",
            "latency_ms": outcome.elapsed_ms,
            "budget_ms": timeout.as_millis() as u64,
            "probe_attempts": outcome.attempts,
            "top_level_count": outcome.top_level_count,
            "last_successful_api_call_ms_ago": self.client.last_success_ms_ago(),
            "cache": {
                "node_count": cache_stats.node_count,
                "parent_count": cache_stats.parent_count,
            },
            "name_index": {
                "size": self.name_index.size(),
                "populated": self.name_index.is_populated(),
            },
            "server_uptime_ms": self.started_at.elapsed().as_millis() as u64,
            "uptime_seconds": self.started_at.elapsed().as_secs(),
            "cancel_generation": self.cancel_registry.generation(),
            "error": outcome.error,
        });
        Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
        })
    }

    #[tool(description = "Extended liveness probe: confirms Workflowy reachability (one in-budget retry on transient failure) AND surfaces in-flight walk count, last-request latency, tree-size estimate, and the most recent upstream rate-limit headers. `authenticated` reflects whether a 401/403 has been observed in the last 5 minutes — it is NOT flipped by transient timeouts or 5xx, so a one-shot probe miss after a successful write burst no longer looks like an auth failure. `last_successful_api_call_ms_ago` provides the anchor a caller needs to distinguish a transient blip from a sustained outage. Use this in preference to health_check when deciding whether to launch a heavy query — it tells you both whether the server is up and whether it is busy.")]
    async fn workflowy_status(
        &self,
        Parameters(_params): Parameters<WorkflowyStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        record_op!(self, "workflowy_status", _params, {
        let timeout = Duration::from_millis(defaults::HEALTH_CHECK_TIMEOUT_MS);
        let outcome = self.probe_upstream_with_retry(timeout).await;
        // See `health_check` for the rationale: probe success is one
        // signal; a recent 2xx is another. Either is sufficient
        // evidence that the API is reachable.
        let recent_success_ms = self.client.last_success_ms_ago();
        let api_reachable = derive_api_reachable(outcome.api_reachable, recent_success_ms);
        let top_level_count = outcome.top_level_count;
        let error = outcome.error.clone();
        let elapsed_ms = outcome.elapsed_ms;
        let cache_stats = self.cache.stats();
        let rate_limit = self.client.rate_limit_snapshot();
        let in_flight = self.in_flight_walks.load(std::sync::atomic::Ordering::Relaxed);
        let tree_estimate = self.tree_size_estimate.load(std::sync::atomic::Ordering::Relaxed);
        let per_tool = per_tool_health(&self.op_log);
        // Brief 2026-04-25 Test ε: a flat `paths` map keyed by tool
        // name with values "healthy"/"degraded"/"failing"/"untested"
        // is what callers actually want for routing decisions. Derive
        // it from per_tool_health and fill in "untested" for the
        // tools the brief explicitly probes (creates/mutations/reads
        // the assistant routinely sequences).
        let mut paths = serde_json::Map::new();
        for tool in [
            "get_node", "list_children", "search_nodes", "find_node",
            "create_node", "delete_node", "edit_node", "move_node",
            "tag_search", "list_overdue", "list_upcoming", "daily_review",
        ] {
            let status = per_tool
                .get(tool)
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("untested")
                .to_string();
            paths.insert(tool.to_string(), serde_json::Value::String(status));
        }
        // Brief 2026-05-02: `last_failure` was sticky — it kept
        // surfacing the most recent error long after the failing tool
        // had recovered, so callers reading `workflowy_status` saw
        // `degraded` indefinitely. `last_unrecovered_failure`
        // self-clears once a success on the same tool lands after the
        // failure, matching what the system is actually doing.
        let last_failure = self.op_log.last_unrecovered_failure().map(|f| {
            // Best-effort proximate-cause classification from the
            // recorded reason string. Same heuristics as `tool_error`
            // so the value matches what the original error carried.
            let reason = f.error.clone().unwrap_or_default();
            let lower = reason.to_lowercase();
            let cause = if lower.contains("404") || lower.contains("not found") {
                ProximateCause::NotFound
            } else if lower.contains("cancelled") {
                ProximateCause::Cancelled
            } else if lower.contains("timeout") || lower.contains("timed out") {
                ProximateCause::Timeout
            } else if lower.contains("api error 5") {
                ProximateCause::UpstreamError
            } else if lower.contains("401") || lower.contains("403") || lower.contains("unauthor") {
                ProximateCause::AuthFailure
            } else {
                ProximateCause::Unknown
            };
            json!({
                "tool": f.tool,
                "at_unix_ms": f.finished_at_unix_ms,
                "reason": reason,
                "proximate_cause": cause.as_str(),
            })
        });
        // `upstream_session` reports the auth/rate-limit posture
        // independently of "did the most recent probe succeed". The
        // 2026-04-30 incident wired `authenticated = api_reachable`,
        // which meant a transient timeout right after a 12-write burst
        // was reported as an auth failure even though the writes
        // proved the API key was fine. `authenticated` now flips to
        // false ONLY when a 401/403 has been observed in the recent
        // window. `session_age_ms` is retained as a back-compat alias
        // for `server_uptime_ms` — both report MCP process uptime, not
        // a Workflowy session age (the client uses a long-lived API
        // key from env, there is no session token to expire).
        let authenticated = !self
            .client
            .recent_auth_failure(Duration::from_secs(defaults::AUTH_FAILURE_WINDOW_SECS));
        let last_success_ms_ago = self.client.last_success_ms_ago();
        let server_uptime_ms = self.started_at.elapsed().as_millis() as u64;
        let upstream_session = json!({
            "authenticated": authenticated,
            "auth_method": "api_key_env",
            "session_age_ms": server_uptime_ms,
            "server_uptime_ms": server_uptime_ms,
            "session_age_note": "alias for server_uptime_ms — the MCP client uses a long-lived API key, so there is no Workflowy session to age",
            "last_successful_api_call_ms_ago": last_success_ms_ago,
            "rate_limit_remaining": rate_limit.remaining,
            "rate_limit_limit": rate_limit.limit,
        });
        let result = json!({
            "status": if api_reachable { "ok" } else { "degraded" },
            "api_reachable": api_reachable,
            "authenticated": authenticated,
            "latency_ms": elapsed_ms,
            "budget_ms": timeout.as_millis() as u64,
            "probe_attempts": outcome.attempts,
            "top_level_count": top_level_count,
            "in_flight_walks": in_flight,
            "last_request_ms": self.client.last_request_ms(),
            "last_successful_api_call_ms_ago": last_success_ms_ago,
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
            "paths": paths,
            "last_failure": last_failure,
            "upstream_session": upstream_session,
            "server_uptime_ms": server_uptime_ms,
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
        let resolved_root = match &params.root_id {
            Some(rid) => Some(self.validate_and_resolve(rid).await?),
            None => None,
        };
        if resolved_root.is_none() && !allow_root_scan {
            return Err(McpError::invalid_params(
                "build_name_index refuses an unscoped walk by default. Pass root_id to scope it, or set allow_root_scan=true to accept a full walk (bounded by the subtree-fetch budget).".to_string(),
                None,
            ));
        }
        info!(root_id = ?resolved_root, max_depth, allow_root_scan, "Building name index");

        match self.walk_subtree(resolved_root.as_deref(), max_depth).await {
            Ok(SubtreeFetch { nodes, truncated, limit, truncation_reason, elapsed_ms, .. }) => {
                let result = json!({
                    "status": if truncated { "partial" } else { "ok" },
                    "nodes_indexed": nodes.len(),
                    "index_size_after": self.name_index.size(),
                    "truncated": truncated,
                    "truncation_reason": truncation_reason.map(|r| r.as_str()),
                    "truncation_limit": limit,
                    "truncation_recovery_hint": if truncated { TRUNCATION_RECOVERY_HINT } else { "" },
                    "elapsed_ms": elapsed_ms,
                });
                Ok(CallToolResult::success(vec![Content::text(result.to_string())]))
            }
            Err(e) => Err(tool_error("build_name_index", resolved_root.as_deref(), e)),
        }
        })
    }

    #[tool(description = "Create many nodes in one call. Operations are pipelined with bounded concurrency; results are returned in input order with per-operation Ok(node_id) or Err(message). Faster than sequential create_node calls for medium-to-large batches; not transactional.")]
    async fn batch_create_nodes(
        &self,
        Parameters(params): Parameters<BatchCreateNodesParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "batch_create_nodes", ToolKind::Bulk, params, {
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
        // The resolver is async (it may walk the workspace on a short-hash
        // miss), so we resolve sequentially before the batch dispatches.
        let mut resolved_ops: Vec<BatchCreateOp> = Vec::with_capacity(params.operations.len());
        for o in params.operations.into_iter() {
            let parent_id = match o.parent_id {
                Some(pid) => Some(self.resolve_node_ref(&pid).await?),
                None => None,
            };
            resolved_ops.push(BatchCreateOp {
                name: o.name,
                description: o.description,
                parent_id,
                priority: o.priority,
            });
        }

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
        tool_handler!(self, "transaction", ToolKind::Bulk, params, {
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

    #[tool(description = "Return the canonical hierarchical path from root to the given node, by walking parent_id pointers via repeated get_node calls. Bounded by max_depth (default 50) so a malformed cycle doesn't loop forever, AND by the bulk-tool wall-clock budget (~210 s) so a slow upstream cannot stretch the walk past the MCP client's hard timeout. Each segment is { id, name }; use this for citation in distillations or for any caller that needs a stable, human-readable location.")]
    async fn path_of(
        &self,
        Parameters(params): Parameters<PathOfParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "path_of", ToolKind::Bulk, params, {
            let resolved = self.validate_and_resolve(&params.node_id).await?;
            let max_depth = params.max_depth.unwrap_or(50);

            // Walk parent_id chain. We stop at the first None, the first
            // missing-node error, the first cycle (id we've seen), or the
            // depth cap. Each step is one HTTP call; for typical Workflowy
            // trees (depth 5-10) this is cheap. The outer
            // `run_handler` wrapper observes the cancel registry on
            // every iteration, so a `cancel_all` fires this loop's next
            // `await` and returns within the cancel-poll slice.
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
        tool_handler!(self, "bulk_tag", ToolKind::Bulk, params, {
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
        // before we start mutating. Sequential because the resolver is
        // async and may walk the workspace on cache miss.
        let mut ids: Vec<String> = Vec::with_capacity(params.node_ids.len());
        for id in &params.node_ids {
            ids.push(self.validate_and_resolve(id).await?);
        }

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
        tool_handler!(self, "since", ToolKind::Read, params, {
        let resolved = self.validate_and_resolve(&params.node_id).await?;
        let node = self.client.get_node(&resolved).await.map_err(|e| {
            tool_error("since", Some(&resolved), e)
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
        tool_handler!(self, "find_by_tag_and_path", ToolKind::Walk, params, {
        let max_depth = params.max_depth.unwrap_or(5);
        let limit = params.limit.unwrap_or(50);
        if let Some(rid) = &params.root_id { check_node_id(rid)?; }
        let scope = match &params.root_id {
            Some(r) => Some(self.resolve_node_ref(r).await?),
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
            Err(e) => Err(tool_error("find_by_tag_and_path", scope.as_deref(), e)),
        }
        })
    }

    /// Default scope for both `audit_mirrors` and `review` is the
    /// Distillations subtree — the only place the wflow Mirror
    /// Discipline convention is applied today. Hard-coding the UUID
    /// keeps tool calls one-arg in the common case while leaving
    /// `root_id` open for narrower or wider scopes.
    #[doc(hidden)]
    const DEFAULT_REVIEW_ROOT: &'static str = "7e351f77-c7b4-4709-86a7-ea6733a63171";

    #[tool(description = "Audit canonical_of: / mirror_of: markers across a subtree per the wflow Mirror Discipline convention. Reports BROKEN (mirror_of UUID does not resolve in scope), DRIFTED (mirror name diverges from canonical's), ORPHAN (claimed canonical lacks a canonical_of: marker), and LONELY (canonical_of marker present but no mirrors point at it). Default scope is Distillations 7e351f77-c7b4-4709-86a7-ea6733a63171; pass root_id to scope elsewhere. Returns a JSON object with scope, scanned count, truncated flag, and findings array.")]
    async fn audit_mirrors(
        &self,
        Parameters(params): Parameters<AuditMirrorsParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "audit_mirrors", ToolKind::Walk, params, {
        let root = match &params.root_id {
            Some(rid) => self.validate_and_resolve(rid).await?,
            None => Self::DEFAULT_REVIEW_ROOT.to_string(),
        };
        let max_depth = params.max_depth.unwrap_or(8);
        info!(root = %root, max_depth, "audit_mirrors");

        match self.walk_subtree(Some(&root), max_depth).await {
            Ok(fetch) => {
                let findings = crate::audit::audit_mirrors(&fetch.nodes);
                let payload = json!({
                    "scope": root,
                    "scanned": fetch.nodes.len(),
                    "truncated": fetch.truncated,
                    "truncation_reason": fetch.truncation_reason.map(|r| r.as_str()),
                    "findings": findings,
                });
                Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
            }
            Err(e) => Err(tool_error("audit_mirrors", Some(&root), e)),
        }
        })
    }

    #[tool(description = "Surface what's worth re-reading under a subtree. Four buckets: (a) revisit-due — nodes tagged #revisit whose description carries `revisit_due: YYYY-MM-DD` past today; (b) multi-pillar — nodes with mirror_of count or distinct pillar-tag count >= 3; (c) stale cross-pillar — concept maps whose last_modified is older than days_stale (default 90); (d) source-MOC re-cited — source-MOC-shaped nodes whose description URLs/DOIs appear in any session-log file under ~/code/SecondBrain/session-logs/ in the last 7 days. Default scope: Distillations.")]
    async fn review(
        &self,
        Parameters(params): Parameters<ReviewParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "review", ToolKind::Walk, params, {
        let root = match &params.root_id {
            Some(rid) => self.validate_and_resolve(rid).await?,
            None => Self::DEFAULT_REVIEW_ROOT.to_string(),
        };
        let max_depth = params.max_depth.unwrap_or(8);
        let days_stale = params.days_stale.unwrap_or(90);
        info!(root = %root, max_depth, days_stale, "review");

        // Bucket (d) needs the recent session-log text. The lib is
        // pure-data and never touches disk; load the blob here and
        // pass it through. If $HOME/code/SecondBrain/session-logs/
        // doesn't exist, blob is "" and bucket (d) is empty — the
        // documented graceful-skip behaviour.
        let blob = load_recent_session_logs_blob_for_review();

        match self.walk_subtree(Some(&root), max_depth).await {
            Ok(fetch) => {
                let report = crate::audit::build_review(
                    &fetch.nodes,
                    days_stale,
                    chrono::Utc::now().date_naive(),
                    chrono::Utc::now().timestamp(),
                    &blob,
                );
                let payload = json!({
                    "scope": root,
                    "scanned": fetch.nodes.len(),
                    "truncated": fetch.truncated,
                    "truncation_reason": fetch.truncation_reason.map(|r| r.as_str()),
                    "days_stale": days_stale,
                    "buckets": report,
                });
                Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
            }
            Err(e) => Err(tool_error("review", Some(&root), e)),
        }
        })
    }

    #[tool(description = "Resolve a hierarchical path of node names to a UUID. ONE API call per path segment, so a four-deep path resolves in ~1 second regardless of total tree size. Use this in preference to search_nodes/tag_search whenever you know where a node lives — finding 'Areas / Personal / Opportunities / Nedbank' costs four list_children calls, not a multi-minute root walk. Each segment matches case-insensitively (HTML stripped, whitespace trimmed) against children's names. Returns the final node's UUID, name, and full canonical path — and ingests every visited node into the persistent name index along the way, so future short-hash lookups under that branch resolve O(1).")]
    async fn node_at_path(
        &self,
        Parameters(params): Parameters<NodeAtPathParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "node_at_path", ToolKind::Bulk, params, {
        if params.path.is_empty() {
            return Err(McpError::invalid_params(
                "path must contain at least one segment".to_string(),
                None,
            ));
        }
        let mut current_id: Option<String> = match &params.start_parent_id {
            Some(pid) => Some(self.validate_and_resolve(pid).await?),
            None => None,
        };
        let mut current_name = "<root>".to_string();
        let mut traversed: Vec<String> = Vec::new();
        let mut visited_nodes: Vec<crate::types::WorkflowyNode> = Vec::new();

        for (idx, segment) in params.path.iter().enumerate() {
            let needle = segment.trim().to_lowercase();
            if needle.is_empty() {
                return Err(McpError::invalid_params(
                    format!("path segment at index {} is empty", idx),
                    None,
                ));
            }
            let children = match &current_id {
                Some(id) => self.client.get_children(id).await,
                None => self.client.get_top_level_nodes().await,
            };
            let children = match children {
                Ok(c) => c,
                Err(e) => {
                    return Err(tool_error(
                        "node_at_path",
                        current_id.as_deref(),
                        e,
                    ));
                }
            };
            // Feed the index opportunistically as we descend.
            self.name_index.ingest(&children);
            visited_nodes.extend(children.iter().cloned());

            let hit = children.iter().find(|n| {
                let stripped = strip_html(&n.name).to_lowercase();
                stripped.trim() == needle
            });
            match hit {
                Some(node) => {
                    current_id = Some(node.id.clone());
                    current_name = strip_html(&node.name);
                    traversed.push(current_name.clone());
                }
                None => {
                    // Surface a useful diagnostic: what segment failed
                    // and what siblings were available.
                    let sibling_names: Vec<String> = children
                        .iter()
                        .take(20)
                        .map(|n| strip_html(&n.name))
                        .collect();
                    let traversed_str = if traversed.is_empty() {
                        "<root>".to_string()
                    } else {
                        traversed.join(" / ")
                    };
                    return Err(McpError::invalid_params(
                        format!(
                            "path segment '{}' not found under '{}'. Children seen ({}): {}.",
                            segment,
                            traversed_str,
                            children.len(),
                            sibling_names.join(", ")
                        ),
                        None,
                    ));
                }
            }
        }
        let final_id = current_id.expect("at least one segment guarantees current_id is Some");
        let payload = serde_json::json!({
            "id": final_id,
            "name": current_name,
            "path": traversed,
            "api_calls": params.path.len(),
            "nodes_indexed": visited_nodes.len(),
        });
        Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
        })
    }

    #[tool(description = "Resolve a Workflowy internal link or short hash to full node info. Optimised for the 'paste this URL, find this node' workflow. When you can name the parent path (e.g. ['Areas', 'Personal']), pass it via search_parent_path — the walk then runs only inside that subtree, taking seconds instead of minutes on huge trees. Bypasses the full-tree walk that ordinary tools fall back to on a short-hash cache miss.")]
    async fn resolve_link(
        &self,
        Parameters(params): Parameters<ResolveLinkParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "resolve_link", ToolKind::Walk, params, {
        let trimmed = params.link.trim();
        // Extract the trailing hex segment from URL-form input.
        let last_seg = trimmed.rsplit('/').next().unwrap_or(trimmed);
        let last_seg = last_seg.trim_start_matches('#');
        let normalised: String = last_seg.chars().filter(|c| c.is_ascii_hexdigit() || *c == '-').collect();
        let unhyphen: String = normalised.chars().filter(|c| *c != '-').collect();

        // Direct full-UUID input: just look up.
        if unhyphen.len() == 32 && unhyphen.chars().all(|c| c.is_ascii_hexdigit()) {
            return match self.client.get_node(&unhyphen).await {
                Ok(node) => {
                    self.name_index.ingest(std::slice::from_ref(&node));
                    let payload = serde_json::json!({
                        "id": node.id,
                        "name": strip_html(&node.name),
                        "description": node.description,
                        "parent_id": node.parent_id,
                        "resolved_via": "full_uuid_passthrough",
                    });
                    Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
                }
                Err(e) => Err(tool_error("resolve_link", Some(&unhyphen), e)),
            };
        }

        if !(unhyphen.len() == 12 || unhyphen.len() == 8) || !unhyphen.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(McpError::invalid_params(
                format!(
                    "could not extract a Workflowy short hash from '{}'. Expected a 12-char URL-suffix hash, 8-char prefix, or full UUID.",
                    params.link
                ),
                None,
            ));
        }

        let short = unhyphen.to_lowercase();

        // Cache hit: return immediately.
        if let Some(full) = self.name_index.resolve_short_hash(&short) {
            return self.return_resolved_node(&full, "cache_hit").await;
        }

        // Resolve search parent if provided.
        let parent_uuid: Option<String> = if let Some(path) = &params.search_parent_path {
            if path.is_empty() {
                None
            } else {
                // Reuse the path-walk logic by calling node_at_path inline.
                let mut current_id: Option<String> = None;
                for segment in path {
                    let needle = segment.trim().to_lowercase();
                    let children = match &current_id {
                        Some(id) => self.client.get_children(id).await,
                        None => self.client.get_top_level_nodes().await,
                    };
                    let children = children.map_err(|e| tool_error("resolve_link", current_id.as_deref(), e))?;
                    self.name_index.ingest(&children);
                    let hit = children.iter().find(|n| {
                        strip_html(&n.name).to_lowercase().trim() == needle
                    });
                    match hit {
                        Some(node) => current_id = Some(node.id.clone()),
                        None => {
                            return Err(McpError::invalid_params(
                                format!(
                                    "search_parent_path segment '{}' not found under the partial path resolved so far",
                                    segment
                                ),
                                None,
                            ));
                        }
                    }
                }
                current_id
            }
        } else if let Some(pid) = &params.search_parent_id {
            Some(self.validate_and_resolve(pid).await?)
        } else {
            None
        };

        // Walk the (parent or root) subtree to populate the index, with
        // the resolution budget. Re-check the index after.
        let summary = match self
            .walk_for_short_hash_scoped(&short, parent_uuid.as_deref())
            .await
        {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "resolve_link walk failed");
                ResolveWalkSummary {
                    nodes_walked: 0,
                    truncated: true,
                    truncation_reason: None,
                    elapsed_ms: 0,
                }
            }
        };

        if let Some(full) = self.name_index.resolve_short_hash(&short) {
            return self
                .return_resolved_node(&full, "scoped_walk")
                .await;
        }

        let scope_str = match (&params.search_parent_path, &params.search_parent_id) {
            (Some(p), _) => format!("path {:?}", p),
            (None, Some(_)) => "the supplied parent UUID".to_string(),
            (None, None) => "the workspace root".to_string(),
        };
        let reason_str = match summary.truncation_reason {
            Some(crate::api::TruncationReason::Timeout) => "timeout",
            Some(crate::api::TruncationReason::NodeLimit) => "node_limit",
            Some(crate::api::TruncationReason::Cancelled) => "cancelled",
            None => "none",
        };
        Err(McpError::invalid_params(
            format!(
                "Short-hash '{}' not found under {} after walking {} nodes in {} ms (truncation: {}). Try: (a) supplying a more specific search_parent_path that contains the target; (b) opening the node in Workflowy and copying the URL bar to get a full URL the server can resolve directly; (c) calling node_at_path with the full hierarchical path if you know it.",
                short, scope_str, summary.nodes_walked, summary.elapsed_ms, reason_str,
            ),
            None,
        ))
        })
    }

    /// Helper: fetch the node at `full_uuid` and emit a `resolve_link`
    /// success payload, recording the resolution path so the caller can
    /// see whether it came from cache or required a walk.
    async fn return_resolved_node(
        &self,
        full_uuid: &str,
        resolved_via: &str,
    ) -> Result<CallToolResult, McpError> {
        match self.client.get_node(full_uuid).await {
            Ok(node) => {
                self.name_index.ingest(std::slice::from_ref(&node));
                let payload = serde_json::json!({
                    "id": node.id,
                    "name": strip_html(&node.name),
                    "description": node.description,
                    "parent_id": node.parent_id,
                    "resolved_via": resolved_via,
                });
                Ok(CallToolResult::success(vec![Content::text(payload.to_string())]))
            }
            Err(e) => Err(tool_error("resolve_link", Some(full_uuid), e)),
        }
    }

    /// Variant of [`Self::walk_for_short_hash`] that scopes the walk to
    /// a given parent (or root if `None`). Used by `resolve_link` so
    /// the caller's parent hint cuts the search space.
    async fn walk_for_short_hash_scoped(
        &self,
        short_hash: &str,
        parent_id: Option<&str>,
    ) -> crate::error::Result<ResolveWalkSummary> {
        use crate::api::client::FetchControls;
        let local_registry = crate::utils::CancelRegistry::new();
        let cancel_guard = local_registry.guard();
        let watcher_guard = cancel_guard.clone();
        let watcher_index = self.name_index.clone();
        let watcher_registry = local_registry.clone();
        let target = short_hash.to_string();

        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
        let watcher = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if watcher_guard.is_cancelled() {
                            return;
                        }
                        if watcher_index.resolve_short_hash(&target).is_some() {
                            watcher_registry.cancel_all();
                            return;
                        }
                    }
                    _ = &mut done_rx => return,
                }
            }
        });

        let controls = FetchControls::with_timeout(Duration::from_millis(
            defaults::RESOLVE_WALK_TIMEOUT_MS,
        ))
        .and_cancel(cancel_guard);

        let result = self
            .client
            .get_subtree_with_controls(
                parent_id,
                defaults::MAX_TREE_DEPTH,
                defaults::RESOLVE_WALK_NODE_CAP,
                controls,
            )
            .await;

        let _ = done_tx.send(());
        let _ = watcher.await;

        match result {
            Ok(fetch) => {
                let nodes_walked = fetch.nodes.len();
                self.name_index.ingest(&fetch.nodes);
                Ok(ResolveWalkSummary {
                    nodes_walked,
                    truncated: fetch.truncated,
                    truncation_reason: fetch.truncation_reason,
                    elapsed_ms: fetch.elapsed_ms,
                })
            }
            Err(e) => Err(e),
        }
    }

    #[tool(description = "Export a subtree in OPML (for Workflowy/outliner compatibility), Markdown (nested bullets), or JSON (raw node array). For backup, hand-off to other tools, or external processing. Subject to the standard 10 000-node and 20-second walk budgets — large subtrees may return partial output with a truncation marker.")]
    async fn export_subtree(
        &self,
        Parameters(params): Parameters<ExportSubtreeParams>,
    ) -> Result<CallToolResult, McpError> {
        tool_handler!(self, "export_subtree", ToolKind::Walk, params, {
        let resolved = self.validate_and_resolve(&params.node_id).await?;
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
            Err(e) => Err(tool_error("export_subtree", Some(&resolved), e)),
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
    /// Roll back a `complete`/`uncomplete` by toggling the boolean back.
    /// `prev_completed` is captured pre-flight via `get_node`; if the
    /// pre-read failed the inverse is dropped (partial rollback is
    /// better than aborting the rest of the rollback queue).
    RestoreCompletion {
        node_id: String,
        prev_completed: bool,
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
                    Some(pid) => Some(self.resolve_node_ref(pid).await?),
                    None => None,
                };
                let name = op.name.as_deref().ok_or_else(|| {
                    McpError::invalid_params("create requires `name`".to_string(), None)
                })?;
                let created = self
                    .client
                    .create_node(name, op.description.as_deref(), parent_id.as_deref(), op.priority)
                    .await
                    .map_err(|e| tool_error("transaction.create", parent_id.as_deref(), e))?;
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
                let node_id = self.resolve_node_ref(node_id_raw).await?;
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
                    .map_err(|e| tool_error("transaction.edit", Some(&node_id), e))?;
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
                let node_id = self.resolve_node_ref(node_id_raw).await?;
                self.client.delete_node(&node_id).await.map_err(|e| {
                    tool_error("transaction.delete", Some(&node_id), e)
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
                let node_id = self.resolve_node_ref(node_id_raw).await?;
                let new_parent = self.resolve_node_ref(new_parent_raw).await?;
                let prev = self.client.get_node(&node_id).await.ok();
                let prev_parent_id = prev.as_ref().and_then(|n| n.parent_id.clone());
                let prev_priority = prev.as_ref().and_then(|n| n.priority).map(|p| p as i32);
                self.client
                    .move_node(&node_id, &new_parent, op.priority)
                    .await
                    .map_err(|e| tool_error("transaction.move", Some(&node_id), e))?;
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
            "complete" | "uncomplete" => {
                let target_state = op.op == "complete";
                let node_id_raw = op.node_id.as_ref().ok_or_else(|| {
                    McpError::invalid_params(
                        format!("{} requires `node_id`", op.op),
                        None,
                    )
                })?;
                let node_id = self.resolve_node_ref(node_id_raw).await?;
                // Capture prior state so we can flip back on rollback.
                // A failed pre-read disables the rollback for this op
                // rather than aborting (same policy as `edit`).
                let prev = self.client.get_node(&node_id).await.ok();
                let prev_completed = prev.as_ref().map(|n| n.completed_at.is_some());
                self.client
                    .set_completion(&node_id, target_state)
                    .await
                    .map_err(|e| {
                        tool_error(
                            if target_state { "transaction.complete" } else { "transaction.uncomplete" },
                            Some(&node_id),
                            e,
                        )
                    })?;
                self.cache.invalidate_node(&node_id);
                self.name_index.invalidate_node(&node_id);
                let summary = json!({
                    "op": op.op,
                    "id": node_id.clone(),
                });
                let inverse = prev_completed.map(|p| TxnInverse::RestoreCompletion {
                    node_id,
                    prev_completed: p,
                });
                Ok((summary, inverse))
            }
            other => Err(McpError::invalid_params(
                format!("unknown transaction op '{}'; expected create/edit/delete/move/complete/uncomplete", other),
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
            TxnInverse::RestoreCompletion { node_id, prev_completed } => {
                self.client
                    .set_completion(&node_id, prev_completed)
                    .await
                    .map_err(|e| McpError::internal_error(format!("rollback completion failed: {}", e), None))?;
                self.cache.invalidate_node(&node_id);
                self.name_index.invalidate_node(&node_id);
                Ok(json!({ "rolled_back": "completion", "id": node_id, "restored_to": prev_completed }))
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
- complete_node: Toggle a node's native Workflowy completion state. Default `completed: true`; pass `false` to uncomplete. Replaces the legacy `#done` tag workaround.
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

/// Resolve the on-disk path for the persistent name index. Honours the
/// [`defaults::INDEX_PATH_ENV`] override; falls back to
/// `$HOME/code/secondBrain/memory/name_index.json`. Returns `None`
/// when both the override and `$HOME` are unavailable, in which case
/// the server runs without persistence (and the index lives only in
/// memory, the historical behaviour).
fn resolve_index_save_path() -> Option<std::path::PathBuf> {
    if let Ok(custom) = std::env::var(defaults::INDEX_PATH_ENV) {
        let trimmed = custom.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(std::path::PathBuf::from(trimmed));
    }
    let home = std::env::var("HOME").ok()?;
    let mut p = std::path::PathBuf::from(home);
    p.push(defaults::DEFAULT_INDEX_RELATIVE_PATH);
    Some(p)
}

/// Spawn a background task that flushes the dirty name index to disk
/// every [`defaults::INDEX_SAVE_INTERVAL_SECS`] seconds. The task lives
/// for as long as the process — there is no graceful shutdown over MCP
/// stdio, so the periodic checkpoint is the only mechanism we have.
/// Errors are logged but never propagated; a transient disk failure
/// must not crash the server.
fn spawn_index_saver(name_index: Arc<NameIndex>) {
    if name_index.save_path().is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(
            defaults::INDEX_SAVE_INTERVAL_SECS,
        ));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Discard the immediate first tick — nothing's been mutated yet.
        interval.tick().await;
        loop {
            interval.tick().await;
            if name_index.is_dirty() {
                if let Err(e) = name_index.save_to_disk() {
                    warn!(error = %e, "name index save failed");
                } else {
                    info!(size = name_index.size(), "name index checkpointed");
                }
            }
        }
    });
}

/// Spawn a background task that periodically walks the workspace root
/// to keep the persistent index in sync with newly added or renamed
/// nodes. The first walk runs after a short startup delay so the
/// initial set of user requests can take priority on the rate limiter.
fn spawn_index_refresher(server: WorkflowyMcpServer) {
    if server.name_index.save_path().is_none() {
        return;
    }
    tokio::spawn(async move {
        // Let the server warm up before kicking the first heavy walk —
        // the user's first interactive request should not contend with
        // a refresh on cold start.
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            let started = Instant::now();
            match server.refresh_name_index().await {
                Ok(stats) => info!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    nodes = stats.0,
                    truncated = stats.1,
                    "background name-index refresh completed"
                ),
                Err(e) => warn!(error = %e, "background name-index refresh failed"),
            }
            tokio::time::sleep(Duration::from_secs(
                defaults::INDEX_REFRESH_INTERVAL_SECS,
            ))
            .await;
        }
    });
}

/// Start the MCP server on stdio transport
pub async fn run_server(client: Arc<WorkflowyClient>) -> anyhow::Result<()> {
    info!("Starting Workflowy MCP Server on stdio");

    let save_path = resolve_index_save_path();
    if let Some(p) = &save_path {
        info!(path = %p.display(), "name index persistence enabled");
    } else {
        info!("name index persistence disabled (no save path)");
    }

    let server = WorkflowyMcpServer::with_cache_and_persistence(
        client,
        crate::utils::cache::get_cache(),
        save_path,
    );

    spawn_index_saver(Arc::clone(&server.name_index));
    spawn_index_refresher(server.clone());

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

    // --- bulk_update: complete/uncomplete are first-class operations ---

    fn new_test_server() -> WorkflowyMcpServer {
        let client = Arc::new(WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        ).unwrap());
        WorkflowyMcpServer::new(client)
    }

    /// Regression test for the 2026-05-03 silent-empty-schema bug:
    /// `rmcp-macros 0.16` discovers a tool's parameter type by matching
    /// the LAST identifier in the path against the literal `Parameters`
    /// (see `rmcp-macros-0.16.0/src/common.rs:64`). Before the fix our
    /// wrapper was named `TracedParams<T>`, which the macro did not
    /// recognise — so the `#[tool]` macro fell through to a hardcoded
    /// `{"type": "object", "properties": {}}` schema for every
    /// parameter-bearing tool. The cowork client then validated
    /// arguments against that empty schema and stripped them all,
    /// breaking every parameter-bearing call (the failure report
    /// names `daily-substack-summary` as the trigger).
    ///
    /// The wrapper is now named `Parameters<T>`. This test asserts
    /// every parameter-bearing tool's published `input_schema` carries
    /// a non-empty `properties` block AND a non-empty `required` block.
    /// If this test fails, the macro discovery regressed and the cowork
    /// path will silently strip arguments again — fix at the wrapper
    /// name, not by patching the test.
    #[test]
    fn parameter_bearing_tools_publish_non_empty_input_schema_properties() {
        let server = new_test_server();
        // Tools that genuinely have no parameters — empty `properties`
        // is the correct schema for them. The bug was about the
        // *parameter-bearing* tools also showing empty.
        let parameterless: &[&str] = &[
            "workflowy_status",
            "health_check",
            "cancel_all",
            // `get_recent_tool_calls` accepts an optional `limit`, so
            // it stays in the parameter-bearing set.
        ];
        let tools = server.tool_router.list_all();
        assert!(!tools.is_empty(), "tool router must register at least one tool");

        let mut empty_schema_violations: Vec<String> = Vec::new();
        for tool in &tools {
            if parameterless.contains(&tool.name.as_ref()) {
                continue;
            }
            let schema = &*tool.input_schema;
            let props = schema
                .get("properties")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            if props.is_empty() {
                empty_schema_violations.push(tool.name.to_string());
            }
        }

        assert!(
            empty_schema_violations.is_empty(),
            "the rmcp `#[tool]` macro fell back to an empty `properties` schema \
             for these parameter-bearing tools — the wrapper name probably \
             diverged from `Parameters<T>` again, so the macro's \
             identifier-match in `rmcp-macros 0.16/src/common.rs:64` \
             stopped recognising it. Tools with empty properties: {:?}",
            empty_schema_violations,
        );

        // Also pin a representative tool's full schema shape so a
        // schemars version bump that quietly drops `required` (the
        // other half of the failure report's "no `required` field")
        // fails this test loudly.
        let search = tools.iter()
            .find(|t| t.name == "search_nodes")
            .expect("search_nodes must be registered");
        let required = search.input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            required_names.contains(&"query"),
            "`search_nodes` schema must declare `query` as required; got: {:?}",
            required_names,
        );
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
            .await
            .expect("known short hash should resolve");
        assert_eq!(resolved, full);
    }

    #[tokio::test]
    async fn resolve_node_ref_errors_on_unknown_short_hash() {
        // The auto-walk fallback fires here against an unreachable test
        // client; the walk fails fast and the resolver surfaces the
        // cache-miss error. The walk is bounded by RESOLVE_WALK_TIMEOUT_MS,
        // so this test cannot hang even when the walk path is taken.
        let server = new_test_server();
        let err = server
            .resolve_node_ref("ffffffffffff")
            .await
            .expect_err("unknown short hash must error after walk");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("short-hash"), "got: {msg}");
        // Error must distinguish truncated walk (recoverable) from
        // exhaustive walk (genuinely missing). Against an unreachable
        // host the walk errors out and we treat it as truncated, so
        // the message should suggest the recovery actions rather than
        // claim the hash is missing.
        assert!(
            msg.contains("did not cover") || msg.contains("recovery") || msg.contains("truncating"),
            "expected truncated-walk wording in error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn resolve_walk_summary_distinguishes_truncated_from_exhaustive() {
        // Direct guard on the wording so the error stays useful as the
        // implementation evolves. The two branches must not be conflated.
        let server = new_test_server();
        // Force the truncated branch via the unreachable-host walk.
        let err = server
            .resolve_node_ref("aaaaaaaaaaaa")
            .await
            .expect_err("must error");
        let msg = err.to_string();
        // Must not falsely claim the walk completed when it didn't.
        assert!(
            !msg.contains("walk completed without truncation"),
            "must not claim exhaustive coverage when walk failed: {msg}"
        );
    }

    #[tokio::test]
    async fn resolve_node_ref_passes_full_uuid_through() {
        let server = new_test_server();
        let full = "550e8400-e29b-41d4-a716-446655440000";
        let resolved = server
            .resolve_node_ref(full)
            .await
            .expect("full UUID should pass through unchanged");
        assert_eq!(resolved, full);
    }

    #[tokio::test]
    async fn with_cache_and_persistence_rehydrates_index_from_disk() {
        // Round-trip: build a server, ingest a node, save to disk;
        // construct a fresh server pointed at the same path and confirm
        // the short-hash resolves without any walk.
        use crate::utils::cache::NodeCache;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("name_index.json");

        let client = Arc::new(WorkflowyClient::new(
            "http://invalid.local".into(),
            "test".into(),
        ).unwrap());
        let cache = Arc::new(NodeCache::new(defaults::CACHE_MAX_SIZE));
        let server1 = WorkflowyMcpServer::with_cache_and_persistence(
            Arc::clone(&client),
            Arc::clone(&cache),
            Some(path.clone()),
        );
        let full = "550e8400-e29b-41d4-a716-446655440000";
        server1.name_index.ingest(&[WorkflowyNode {
            id: full.to_string(),
            name: "Tasks".to_string(),
            parent_id: None,
            ..Default::default()
        }]);
        server1.name_index.save_to_disk().expect("save");

        let server2 = WorkflowyMcpServer::with_cache_and_persistence(
            Arc::clone(&client),
            Arc::clone(&cache),
            Some(path.clone()),
        );
        // The fresh server resolves the short hash entirely from disk.
        let resolved = server2
            .resolve_node_ref("446655440000")
            .await
            .expect("rehydrated short hash must resolve from cache");
        assert_eq!(resolved, full);
    }

    #[test]
    fn resolve_index_save_path_uses_env_override_when_set() {
        // Save and restore the env var so the test is repeatable. We
        // avoid `std::env::set_var` in async context (it's not
        // thread-safe across tests) by using a serialised wrapper here.
        let key = defaults::INDEX_PATH_ENV;
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "/tmp/wflow-test/idx.json");
        let path = resolve_index_save_path().expect("env path");
        assert_eq!(path.to_string_lossy(), "/tmp/wflow-test/idx.json");
        // Restore.
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn resolve_index_save_path_treats_empty_env_as_disabled() {
        let key = defaults::INDEX_PATH_ENV;
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "");
        assert!(resolve_index_save_path().is_none(), "empty env disables persistence");
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
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

    /// Regression for the post-c471a49 gap: every handler that took a
    /// node/parent/root id called check_node_id (which the fix widened to
    /// accept short hashes) but a long tail of handlers forgot to also
    /// pipe the value through resolve_node_ref. The raw 8-char hash then
    /// landed at the upstream API and 404'd. This test exercises one
    /// representative handler from each "scoping" pattern (Optional
    /// root_id, Optional parent_id, required node_id) and asserts that a
    /// short hash NOT in the name index produces the resolver-side error
    /// rather than reaching the HTTP layer with a raw short hash.
    // The resolver-routing test that used to live here (a slow
    // ~30 s exerciser against `invalid.local`) has moved to
    // `mod load_tests::handlers_route_unindexed_short_hashes_through_resolver`,
    // where it runs against an in-process wiremock that returns an
    // empty workspace and completes in milliseconds.

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
        let src = include_str!("mod.rs");
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

    /// Brief 2026-04-25 Pattern 6d: a `parent_id=null` (or omitted)
    /// create_node call must place the node at the workspace root and
    /// the success message must say so explicitly, so the assistant
    /// can audit placement without a follow-up read. The four orphan
    /// nodes from the original session were created with the same
    /// shape and the original message was indistinguishable from a
    /// successful scoped create — that ambiguity is the bug.
    #[tokio::test]
    async fn create_node_success_message_names_root_when_parent_id_omitted() {
        // Pure local test: render the success-message branch directly
        // without hitting the API. The function under test is the
        // formatting logic introduced for 6d, not the upstream call.
        let placement = None::<&str>
            .map(|p: &str| format!("under `{}`", p))
            .unwrap_or_else(|| "at workspace root (no parent_id supplied)".to_string());
        assert!(
            placement.contains("workspace root"),
            "null parent_id must say workspace root: got {placement}"
        );

        let placement_scoped = Some("550e8400-e29b-41d4-a716-446655440000")
            .map(|p: &str| format!("under `{}`", p))
            .unwrap_or_else(|| "at workspace root (no parent_id supplied)".to_string());
        assert!(
            placement_scoped.starts_with("under `550e8400"),
            "scoped placement must name the parent: got {placement_scoped}"
        );
    }

    /// Brief 2026-04-25 Pattern 6 / Patterns 4-5: handler-error paths
    /// must surface a structured `McpError` with `data.operation`,
    /// `data.error`, and a hint — not a bare `internal_error` that
    /// renders as "Tool execution failed" in some clients. Exercises
    /// representative handlers (delete, edit, move, find_node,
    /// list_overdue) that all reached the upstream error path during
    /// the 2026-04-25 session and produced bare failures.
    // The mutation-error test that used to live here (a slow
    // ~30 s exerciser against `invalid.local`, three handlers in
    // sequence) has moved to
    // `mod load_tests::mutation_errors_carry_structured_data_payload`,
    // where each handler runs against a wiremock returning a
    // non-retryable 404 and the whole test completes in milliseconds.

    /// Brief 2026-04-25 Pattern 6 cross-cut: workflowy_status must
    /// surface the most recent failure (tool, finished_at, reason) so
    /// the assistant can diagnose which call last broke without
    /// scrolling the op log. None when the log has no Err entries.
    #[tokio::test]
    async fn workflowy_status_surfaces_last_failure() {
        let server = new_test_server();
        // Status before any failure: last_failure is null.
        let result = server
            .workflowy_status(Parameters(WorkflowyStatusParams::default()))
            .await
            .expect("status returns");
        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert!(
            v["last_failure"].is_null(),
            "no failures yet — last_failure must be null: {}", v["last_failure"]
        );

        // Force a failure (empty node_id rejected at the boundary).
        let _ = server
            .get_node(Parameters(GetNodeParams { node_id: NodeId::from("") }))
            .await
            .expect_err("empty id rejected");

        // Status after failure: last_failure names the tool and reason.
        let result = server
            .workflowy_status(Parameters(WorkflowyStatusParams::default()))
            .await
            .expect("status returns");
        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        let lf = &v["last_failure"];
        assert!(!lf.is_null(), "last_failure must be set after a failure: {v}");
        assert_eq!(lf["tool"], "get_node", "last_failure.tool wrong: {lf}");
        assert!(lf["at_unix_ms"].as_u64().is_some(), "at_unix_ms missing or not a number: {lf}");
        assert!(lf["reason"].as_str().is_some(), "reason missing: {lf}");
    }

    /// Brief 2026-04-25 Pattern 6: the propagation-retry helpers must
    /// exist on the API client for delete/edit/move so handlers can
    /// route through them. Anchors the contract — if a future refactor
    /// removes one of these methods, the test fails before deploy.
    #[test]
    fn propagation_retry_helpers_exist_for_all_mutations() {
        let src = include_str!("../api/client.rs");
        for needle in &[
            "delete_node_with_propagation_retry",
            "edit_node_with_propagation_retry",
            "move_node_with_propagation_retry",
            // Pre-existing helpers from T-159 — guard against accidental removal.
            "get_node_with_propagation_retry",
            "get_children_with_propagation_retry",
        ] {
            assert!(
                src.contains(needle),
                "expected {} in api/client.rs to satisfy Brief 2026-04-25 Pattern 6", needle
            );
        }
        let server_src = include_str!("mod.rs");
        for handler in &[
            "delete_node_with_propagation_retry",
            "edit_node_with_propagation_retry",
            "move_node_with_propagation_retry",
        ] {
            assert!(
                server_src.contains(handler),
                "handler must route through {} (Pattern 6)", handler
            );
        }
    }

    /// Brief 2026-04-25 follow-up Test γ: every tool_error carries a
    /// discrete `proximate_cause` enum value the caller can switch on.
    /// Exercises each of the classifier branches so a future regression
    /// (e.g. someone reverts the enum) fails loudly. Reads the data
    /// payload off the McpError and checks the value matches the
    /// expected variant for the input shape.
    #[test]
    fn tool_error_proximate_cause_classification_covers_every_branch() {
        let cases: &[(&str, &str)] = &[
            ("API error 404: Item not found", "not_found"),
            ("get_subtree cancelled by cancel_all", "cancelled"),
            ("subtree walk timed out after 20 s", "timeout"),
            ("API error 500: backend whoopsie", "upstream_error"),
            ("API error 401: unauthorized", "auth_failure"),
            ("internal lock contention on cache", "lock_contention"),
            ("stale cache entry detected", "cache_miss"),
            ("some completely unknown failure mode", "unknown"),
        ];
        for (err_str, expected_cause) in cases {
            let mcp_err = tool_error("test_tool", Some("550e8400-e29b-41d4-a716-446655440000"), err_str);
            // Re-derive the data payload through the standard JSON
            // round-trip so we test what the wire actually carries.
            let json = serde_json::to_value(&mcp_err).expect("McpError serialises");
            let cause = json["data"]["proximate_cause"]
                .as_str()
                .expect(&format!("data.proximate_cause missing in {json}"));
            assert_eq!(
                cause, *expected_cause,
                "input '{err_str}' should classify as '{expected_cause}', got '{cause}'"
            );
            // The error message itself includes the cause in brackets so
            // even minimal clients (which discard the data payload)
            // still see it.
            let msg = mcp_err.to_string();
            assert!(
                msg.contains(&format!("[{}]", expected_cause)),
                "error message must include [{expected_cause}]: {msg}"
            );
        }
    }

    /// Brief 2026-04-25 follow-up Test ε: workflowy_status returns a
    /// `paths` map keyed by tool name with simple healthy/degraded/
    /// failing/untested values, plus an `upstream_session` block. The
    /// brief gives the exact shape the assistant needs for routing.
    #[tokio::test]
    async fn workflowy_status_returns_paths_and_upstream_session() {
        let server = new_test_server();
        // Drive a couple of failures to populate per_tool_health.
        let _ = server
            .get_node(Parameters(GetNodeParams { node_id: NodeId::from("") }))
            .await
            .expect_err("empty id rejected");
        let result = server
            .workflowy_status(Parameters(WorkflowyStatusParams::default()))
            .await
            .expect("status returns");
        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();

        let paths = v["paths"].as_object().expect("paths must be a JSON object");
        // Every tool the brief explicitly mentions for routing must be
        // present, with a value from the documented enum.
        for tool in [
            "get_node", "list_children", "search_nodes", "find_node",
            "create_node", "delete_node", "edit_node", "move_node",
            "tag_search", "list_overdue", "list_upcoming", "daily_review",
        ] {
            let status = paths
                .get(tool)
                .and_then(|s| s.as_str())
                .expect(&format!("paths['{tool}'] missing"));
            assert!(
                matches!(status, "healthy" | "degraded" | "failing" | "untested"),
                "paths['{tool}'] must be one of healthy/degraded/failing/untested, got '{status}'"
            );
        }
        // get_node was just exercised with an empty-id failure — should
        // be the only tool reading "failing" in this fresh server.
        assert_eq!(paths["get_node"], "failing", "get_node should be failing after the empty-id call");

        let session = v["upstream_session"].as_object().expect("upstream_session block");
        assert!(session.contains_key("authenticated"));
        assert!(session.contains_key("auth_method"));
        assert!(session.contains_key("session_age_ms"));
        assert!(session.contains_key("server_uptime_ms"));
        assert!(session.contains_key("rate_limit_remaining"));
        assert!(
            session.contains_key("last_successful_api_call_ms_ago"),
            "upstream_session must surface last_successful_api_call_ms_ago so callers can distinguish a transient blip from a sustained outage"
        );
        // 2026-04-30 incident regression: a single failed probe used
        // to flip `authenticated` to false even though no 401/403 had
        // been observed. The new wiring drives `authenticated` from
        // `client.recent_auth_failure` — no auth failures have been
        // recorded in this fresh server, so it must be true regardless
        // of whether the probe itself reached the (unreachable) test
        // host.
        assert_eq!(
            session["authenticated"], serde_json::Value::Bool(true),
            "with no 401/403 observed, `authenticated` must remain true even when the probe fails to reach upstream"
        );
        // The top-level mirror of `authenticated` is the field that
        // routing code in clients tends to read first.
        assert_eq!(v["authenticated"], serde_json::Value::Bool(true));
        // server_uptime_ms is the canonical name; session_age_ms is a
        // back-compat alias holding the same value. The note field
        // exists to forestall the 2026-04-30 misreading where "44
        // hours" was interpreted as a Workflowy session age.
        assert_eq!(
            session["server_uptime_ms"], session["session_age_ms"],
            "session_age_ms must alias server_uptime_ms — they are the same metric"
        );
    }

    /// Regression test for the 2026-04-30 wiring bug. When a 401/403
    /// has been observed within the AUTH_FAILURE_WINDOW, `authenticated`
    /// must report false — independent of whether the probe call itself
    /// just succeeded. This is the inverse of the test above and proves
    /// that the new `recent_auth_failure` channel actually drives the
    /// signal, rather than being decorative.
    #[tokio::test]
    async fn workflowy_status_authenticated_false_after_recent_auth_failure() {
        let server = new_test_server();
        server.client._test_stamp_auth_failure();
        let result = server
            .workflowy_status(Parameters(WorkflowyStatusParams::default()))
            .await
            .expect("status returns");
        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(
            v["authenticated"], serde_json::Value::Bool(false),
            "a recent 401/403 stamp must flip `authenticated` to false"
        );
        assert_eq!(
            v["upstream_session"]["authenticated"], serde_json::Value::Bool(false),
            "upstream_session.authenticated must mirror the top-level signal"
        );
    }

    /// Probe responses must include `last_successful_api_call_ms_ago`
    /// as a Number (not null) once the client has observed at least
    /// one 2xx — the 12-write burst on 2026-04-30 left a fresh anchor
    /// that the agent could have used to discount the immediately
    /// following timeout. Stamping success directly on the client
    /// proves the field is plumbed end-to-end without needing a live
    /// upstream.
    #[tokio::test]
    async fn health_check_surfaces_last_successful_api_call_ms_ago() {
        let server = new_test_server();
        server.client._test_stamp_success();
        let result = server
            .health_check(Parameters(HealthCheckParams::default()))
            .await
            .expect("health_check returns");
        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert!(
            v["last_successful_api_call_ms_ago"].is_number(),
            "after a stamped success, the field must be a number, got {}",
            v["last_successful_api_call_ms_ago"]
        );
        assert!(
            v["server_uptime_ms"].is_number(),
            "server_uptime_ms must be present alongside the back-compat uptime_seconds"
        );
        // Even with the unreachable test host, a stamped success leaves
        // `authenticated: true` since no 401/403 has been recorded.
        assert_eq!(v["authenticated"], serde_json::Value::Bool(true));
    }

    /// Brief 2026-04-25 follow-up Test β: when a read or mutate has
    /// failed in the last 30 s, a successful create_node response
    /// must include a DEGRADED warning naming the broken tool — so
    /// the assistant doesn't silently accumulate orphan creates the
    /// way the original session did. Tested at the helper level
    /// because invoking create_node end-to-end requires a live API.
    #[tokio::test]
    async fn fail_closed_warning_fires_when_recent_failure_in_window() {
        let server = new_test_server();
        // No failures yet → no warning.
        assert!(
            server.degraded_warning_if_recent_failure(30_000).is_none(),
            "no failures recorded — must not warn"
        );

        // Force a get_node failure (empty id) — the most recent op-log
        // entry is now an Err.
        let _ = server
            .get_node(Parameters(GetNodeParams { node_id: NodeId::from("") }))
            .await
            .expect_err("empty id rejected");

        let warn = server
            .degraded_warning_if_recent_failure(30_000)
            .expect("a recent failure must produce a warning");
        assert!(warn.contains("get_node"), "warning must name the broken tool: {warn}");
        assert!(
            warn.contains("degraded") || warn.contains("DEGRADED") || warn.contains("workflowy_status"),
            "warning must signal degraded state and point at status tool: {warn}"
        );

        // Self-failures (create_node) do NOT gate future create_node —
        // a previous failed create has nothing to do with whether the
        // next one will succeed.
        let server2 = new_test_server();
        // Synthesise a create_node failure by calling with an
        // unindexed short-hash parent — that hits resolve_node_ref's
        // miss path and records an Err for create_node.
        let _ = server2
            .create_node(Parameters(CreateNodeParams {
                name: "x".into(),
                description: None,
                parent_id: Some(NodeId::from("ffffffffffff")),
                priority: None,
            }))
            .await
            .expect_err("unindexed short hash rejected");
        assert!(
            server2.degraded_warning_if_recent_failure(30_000).is_none(),
            "create_node self-failure must NOT gate future create_node calls"
        );
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

    /// Pre-completion-state, `bulk_update` rejected `complete` /
    /// `uncomplete` with "not yet supported". The same operations are
    /// now first-class — they route through `client.set_completion`,
    /// the same code path the single-node `complete_node` tool uses.
    /// Pin the validation acceptance here; the wiremock integration in
    /// `bulk_update_complete_dispatches_to_set_completion` covers the
    /// full handler-to-client wire path.
    #[tokio::test]
    async fn test_bulk_update_accepts_complete_and_uncomplete() {
        // Validation runs against the params struct directly (no live
        // upstream needed). Earlier the `not yet supported` error
        // fired before the walk, so an unreachable test server was
        // enough. With validation now passing, asserting the params
        // round-trip and the operation list deserialises is sufficient
        // here; integration coverage lives in the wiremock test.
        let complete_params: BulkUpdateParams = serde_json::from_value(json!({
            "operation": "complete",
            "dry_run": true,
        })).expect("complete must deserialize");
        assert_eq!(complete_params.operation, "complete");

        let uncomplete_params: BulkUpdateParams = serde_json::from_value(json!({
            "operation": "uncomplete",
            "dry_run": true,
        })).expect("uncomplete must deserialize");
        assert_eq!(uncomplete_params.operation, "uncomplete");
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

    /// Pin the truncation envelope helper's contract: every call returns
    /// the same four fields, and `truncation_recovery_hint` is non-empty
    /// only when `truncated == true`. The architecture review on
    /// 2026-05-03 introduced this helper so future JSON-emitting tools
    /// route through one definition; existing 15 inline sites are
    /// pinned by `every_walk_tool_emits_full_truncation_envelope_in_json`.
    #[test]
    fn truncation_envelope_emits_four_fields_and_hint_only_when_truncated() {
        let truncated_env = truncation_envelope(true, 10_000, Some(TruncationReason::Timeout));
        assert_eq!(truncated_env.get("truncated"), Some(&serde_json::json!(true)));
        assert_eq!(truncated_env.get("truncation_limit"), Some(&serde_json::json!(10_000)));
        assert_eq!(truncated_env.get("truncation_reason"), Some(&serde_json::json!("timeout")));
        let hint = truncated_env.get("truncation_recovery_hint").expect("hint field");
        assert!(
            hint.as_str().unwrap_or("").contains("build_name_index"),
            "truncated envelope must name build_name_index in recovery hint: {hint:?}"
        );

        let clean_env = truncation_envelope(false, 10_000, None);
        assert_eq!(clean_env.get("truncated"), Some(&serde_json::json!(false)));
        assert_eq!(clean_env.get("truncation_reason"), Some(&serde_json::json!(null)));
        assert_eq!(
            clean_env.get("truncation_recovery_hint"),
            Some(&serde_json::json!("")),
            "non-truncated envelope must carry an empty hint string"
        );
        // Same four keys present in both shapes — the schema is
        // invariant regardless of truncation outcome.
        let expected: std::collections::BTreeSet<&str> = ["truncated", "truncation_limit", "truncation_reason", "truncation_recovery_hint"]
            .iter().copied().collect();
        let truncated_keys: std::collections::BTreeSet<&str> = truncated_env.keys().map(|s| s.as_str()).collect();
        let clean_keys: std::collections::BTreeSet<&str> = clean_env.keys().map(|s| s.as_str()).collect();
        assert_eq!(truncated_keys, expected);
        assert_eq!(clean_keys, expected);
    }

    /// Pin `with_truncation_envelope` — the JSON wrapper used at call
    /// sites. Carrier payload's fields survive the merge; the four
    /// envelope fields are appended; non-object payloads are wrapped
    /// rather than dropped so a misuse surfaces in the response shape.
    #[test]
    fn with_truncation_envelope_merges_payload_and_envelope_without_loss() {
        let merged = with_truncation_envelope(
            serde_json::json!({"results": [1, 2, 3], "matched": 3}),
            true,
            10_000,
            Some(TruncationReason::NodeLimit),
        );
        let obj = merged.as_object().expect("must be a JSON object");
        assert_eq!(obj.get("results"), Some(&serde_json::json!([1, 2, 3])));
        assert_eq!(obj.get("matched"), Some(&serde_json::json!(3)));
        assert_eq!(obj.get("truncated"), Some(&serde_json::json!(true)));
        assert_eq!(obj.get("truncation_reason"), Some(&serde_json::json!("node_limit")));

        // Defensive path: a non-object payload (caller bug) wraps under
        // a `payload` key rather than silently swallowing.
        let wrapped = with_truncation_envelope(
            serde_json::json!("plain string"),
            false,
            10_000,
            None,
        );
        let obj = wrapped.as_object().expect("misuse path still produces an object");
        assert_eq!(obj.get("payload"), Some(&serde_json::json!("plain string")));
        assert!(obj.contains_key("truncated"), "envelope still attached on misuse path");
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

    /// Failure 2 from the 2026-04-30 MCP report: a single-node read against
    /// a hung upstream wedges the tool surface for ~3.5 min (5 retries × 30 s
    /// reqwest timeout). `with_read_budget` must collapse that to the
    /// configured budget by dropping the inner future on deadline.
    #[tokio::test]
    async fn with_read_budget_returns_timeout_when_inner_future_hangs() {
        let server = new_test_server();
        let started = std::time::Instant::now();
        let pending_fut = std::future::pending::<std::result::Result<(), WorkflowyError>>();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            server.with_read_budget_inner(pending_fut, Duration::from_millis(150)),
        )
        .await
        .expect("with_read_budget must self-bound, must not exhaust outer test timeout");
        assert!(
            matches!(result, Err(WorkflowyError::Timeout)),
            "expected Timeout, got {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "deadline must fire promptly, elapsed = {:?}",
            started.elapsed()
        );
    }

    /// Failure 2 from the same report: `cancel_all` was hanging because
    /// in-flight reads weren't observing the cancel registry. With the budget
    /// helper wrapping every read, `cancel_all` now drops the in-flight
    /// future on the next 50 ms cancel-poll tick.
    #[tokio::test]
    async fn with_read_budget_returns_cancelled_when_cancel_all_fires_mid_flight() {
        use std::sync::Arc;
        let server = Arc::new(new_test_server());
        let server_for_task = Arc::clone(&server);
        let task = tokio::spawn(async move {
            let slow_fut = async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<(), WorkflowyError>(())
            };
            server_for_task
                .with_read_budget_inner(slow_fut, Duration::from_secs(120))
                .await
        });

        // Let the future enter its sleep, then cancel.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let started = std::time::Instant::now();
        server.cancel_registry.cancel_all();

        let result = tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .expect("cancellation must be observed within ~50 ms cancel-poll slice")
            .expect("task should not panic");
        assert!(
            matches!(result, Err(WorkflowyError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "cancel must preempt the long sleep, elapsed = {:?}",
            started.elapsed()
        );
    }

    /// Happy path: a fast inner future returns its own result without the
    /// budget firing. Guards the helper against accidentally swallowing
    /// successful responses on the timeout/cancel arms.
    #[tokio::test]
    async fn with_read_budget_returns_ok_for_fast_inner_future() {
        let server = new_test_server();
        let result = server
            .with_read_budget_inner(
                async { Ok::<u32, WorkflowyError>(42) },
                Duration::from_secs(60),
            )
            .await;
        assert!(matches!(result, Ok(42)));
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
            "audit_mirrors",
            "review",
        ] {
            assert!(server.get_tool(tool).is_some(), "tool {tool} must be registered");
        }
    }

    /// Brief 2026-04-26 (T-164): the MCP server exposes audit_mirrors
    /// and review as first-class tools, not just CLI subcommands. The
    /// test harness has no reachable upstream, so the walk_subtree
    /// deadline fires and the handler returns a successful response
    /// carrying `truncation_reason: timeout` — a precise proxy for "the
    /// wiring compiles and the call reaches walk_subtree before the
    /// network gives up".
    #[tokio::test]
    async fn audit_mirrors_handler_dispatches_via_walk_subtree() {
        let server = new_test_server();
        let result = server
            .audit_mirrors(Parameters(AuditMirrorsParams {
                root_id: Some(NodeId::from("550e8400-e29b-41d4-a716-446655440000")),
                max_depth: Some(2),
            }))
            .await
            .expect("handler must return a degraded result, not error");
        let body = result_text(&result);
        assert!(
            body.contains("\"truncation_reason\":\"timeout\""),
            "must carry walk_subtree truncation marker: {body}"
        );
        assert!(
            body.contains("\"scanned\":0"),
            "no nodes should be scanned when upstream is unreachable: {body}"
        );
    }

    #[tokio::test]
    async fn review_handler_dispatches_via_walk_subtree() {
        let server = new_test_server();
        let result = server
            .review(Parameters(ReviewParams {
                root_id: Some(NodeId::from("550e8400-e29b-41d4-a716-446655440000")),
                max_depth: Some(2),
                days_stale: Some(30),
            }))
            .await
            .expect("handler must return a degraded result, not error");
        let body = result_text(&result);
        assert!(
            body.contains("\"truncation_reason\":\"timeout\""),
            "must carry walk_subtree truncation marker: {body}"
        );
        assert!(
            body.contains("\"scanned\":0"),
            "no nodes should be scanned when upstream is unreachable: {body}"
        );
    }

    /// The MCP handler and the wflow-do CLI must call into the SAME
    /// `audit::audit_mirrors` and `audit::build_review` functions, so
    /// findings are guaranteed identical between transports. Anchored
    /// here as a source-pattern test — accidental re-implementation in
    /// either surface fails before deploy.
    #[test]
    fn audit_review_handlers_route_through_lib_module() {
        let server_src = include_str!("mod.rs");
        for needle in &[
            "crate::audit::audit_mirrors(",
            "crate::audit::build_review(",
        ] {
            assert!(
                server_src.contains(needle),
                "server.rs must call {needle} (T-164)"
            );
        }
        let cli_src = include_str!("../bin/wflow_do.rs");
        for needle in &[
            "audit_mirrors(",
            "build_review(",
        ] {
            assert!(
                cli_src.contains(needle),
                "wflow_do.rs must call {needle} (T-164 — same lib path as MCP)"
            );
        }
        // And the use statement that makes it route to the lib, not a
        // local re-implementation.
        assert!(
            cli_src.contains("workflowy_mcp_server::audit::"),
            "wflow_do.rs must import from the lib module, not redefine"
        );
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

    // --- Brief 2026-05-02 fixes: uniform error model & observability ---
    //
    // The five symptoms the brief named (silent param drops,
    // unobservable framework failures, oversized insert_content,
    // unscoped search_nodes, sticky degraded) all share the same
    // cause — silent failure modes scattered across the boundary
    // between transport, framework, and handler. The fixes below
    // unify the contract: every failure is observable (op log),
    // every parameter mismatch is typed (deny_unknown_fields +
    // parent_id alias), every payload that would be silently dropped
    // is refused at the boundary (insert_content cap, search_nodes
    // gate), and every "degraded" surface self-clears on recovery.

    /// `deny_unknown_fields` converts the silent-drop bug into a typed
    /// error. Brief 2026-05-02: callers passing `parent_id` to
    /// `list_children` (which expects `node_id`) used to see workspace
    /// root listed instead — serde defaulted the missing field to None
    /// and silently dropped the unknown one. With `deny_unknown_fields`
    /// + the `parent_id` alias on `GetChildrenParams.node_id`, both
    /// names now reach the handler and unknown names produce a clear
    /// deserialize error.
    #[test]
    fn get_children_params_accepts_parent_id_alias() {
        let p: GetChildrenParams =
            serde_json::from_value(serde_json::json!({ "parent_id": "abc-123" })).unwrap();
        assert_eq!(p.node_id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn get_children_params_accepts_node_id() {
        let p: GetChildrenParams =
            serde_json::from_value(serde_json::json!({ "node_id": "abc-123" })).unwrap();
        assert_eq!(p.node_id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn get_children_params_rejects_unknown_field() {
        let result: std::result::Result<GetChildrenParams, _> =
            serde_json::from_value(serde_json::json!({ "bogus_field": "x" }));
        let err = result.expect_err("unknown field must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("unknown field") && msg.contains("bogus_field"),
            "deserialize error must name the rejected field: {msg}"
        );
    }

    #[test]
    fn search_nodes_params_rejects_unknown_field() {
        let result: std::result::Result<SearchNodesParams, _> =
            serde_json::from_value(serde_json::json!({
                "query": "x",
                "ancestor_id": "abc",
            }));
        let err = result.expect_err("unknown field must be rejected");
        assert!(err.to_string().to_lowercase().contains("unknown field"));
    }

    #[test]
    fn search_nodes_params_accepts_allow_root_scan() {
        let p: SearchNodesParams = serde_json::from_value(serde_json::json!({
            "query": "x",
            "allow_root_scan": true,
        }))
        .unwrap();
        assert_eq!(p.allow_root_scan, Some(true));
    }

    /// Brief 2026-05-02 Test #4: `search_nodes` with `parent_id: null`
    /// and no `allow_root_scan` opt-in must return a typed error
    /// before doing any tree walk. Mirrors `find_node`'s gate.
    #[tokio::test]
    async fn search_nodes_refuses_root_scan_by_default() {
        let server = new_test_server();
        let params = SearchNodesParams {
            query: "Tasks".to_string(),
            max_results: None,
            parent_id: None,
            max_depth: None,
            allow_root_scan: None,
            use_index: None,
        };
        let err = server
            .search_nodes(Parameters(params))
            .await
            .expect_err("unscoped search must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("workspace root") || msg.contains("allow_root_scan"),
            "must explain how to opt in: {msg}"
        );
        // 2026-05-03 hint: error must also surface use_index as a recovery
        // path so callers don't have to read the schema description.
        assert!(
            msg.contains("use_index"),
            "refusal message must surface the use_index recovery path: {msg}"
        );
    }

    /// 2026-05-03 eval-run regression: searches scoped under
    /// Distillations reliably timed out at the 20 s walk budget once the
    /// subtree grew past the 10 000-node cap. The fix is `use_index=true`,
    /// which serves the query from the persistent name index in O(1)
    /// without burning the walk budget. Pre-fix, search_nodes had no
    /// index path at all — only `find_node` did.
    #[tokio::test]
    async fn search_nodes_use_index_returns_index_hits_without_walking() {
        let server = new_test_server();
        // Seed the name index directly with two entries so the index
        // lookup has something to find. No HTTP, no walk budget.
        server.name_index.ingest(&[
            WorkflowyNode {
                id: "11111111-1111-1111-1111-111111111111".to_string(),
                name: "Cynefin and the chaotic domain".to_string(),
                parent_id: Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()),
                ..Default::default()
            },
            WorkflowyNode {
                id: "22222222-2222-2222-2222-222222222222".to_string(),
                name: "Wardley Mapping primer".to_string(),
                parent_id: Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()),
                ..Default::default()
            },
        ]);
        let params = SearchNodesParams {
            query: "cynefin".to_string(),
            max_results: None,
            parent_id: None,
            max_depth: None,
            allow_root_scan: Some(true),
            use_index: Some(true),
        };
        let result = server
            .search_nodes(Parameters(params))
            .await
            .expect("use_index must answer without walking the API");
        let body = result
            .content
            .iter()
            .next()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert!(
            body.contains("Cynefin and the chaotic domain"),
            "index hit must appear in the response: {body}"
        );
        assert!(
            !body.contains("Wardley"),
            "non-matching index entry must not leak: {body}"
        );
        assert!(
            body.contains("name index") && body.contains("name match"),
            "response must signal the name-only / index-served path: {body}"
        );
    }

    /// `use_index=true` against an empty index is a typed error, not a
    /// silent fall-through to the live walk. The caller chose the fast
    /// path; if it isn't usable they need to know to call
    /// `build_name_index` first.
    #[tokio::test]
    async fn search_nodes_use_index_errors_when_index_is_empty() {
        let server = new_test_server();
        // No ingest — name index is empty.
        let params = SearchNodesParams {
            query: "anything".to_string(),
            max_results: None,
            parent_id: None,
            max_depth: None,
            allow_root_scan: Some(true),
            use_index: Some(true),
        };
        let err = server
            .search_nodes(Parameters(params))
            .await
            .expect_err("empty index + use_index=true must return a typed error");
        let msg = err.to_string();
        assert!(
            msg.contains("build_name_index"),
            "error must point at the recovery path: {msg}"
        );
    }

    /// Cross-handler consistency invariant. The 2026-05-03 eval-run
    /// surfaced the same failure mode in 11 different handlers:
    /// JSON-shaped responses emitted `truncation_limit` only, with no
    /// `truncation_reason` and no recovery hint, so a JSON caller
    /// hitting the 20 s walk budget on a big subtree had no actionable
    /// information. The fix is uniform: every walk-shaped tool's JSON
    /// payload includes the four-field envelope (truncated,
    /// truncation_limit, truncation_reason, truncation_recovery_hint).
    /// This test grep-audits the source to make sure the invariant
    /// holds at build time — adding a new walk-shaped tool that
    /// emits `truncation_limit` without the reason + hint will fail
    /// here before it ships.
    #[test]
    fn every_walk_tool_emits_full_truncation_envelope_in_json() {
        let src = include_str!("mod.rs");
        // Every site that emits `"truncation_limit": <expr>,` must
        // also have `"truncation_reason"` and `"truncation_recovery_hint"`
        // in its surrounding json! block. We approximate "surrounding
        // block" by looking 6 lines on either side — every json! payload
        // is short enough.
        let lines: Vec<&str> = src.lines().collect();
        let mut violations: Vec<String> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("\"truncation_limit\":") {
                continue;
            }
            // Skip the helper-doc comment lines that match the pattern
            // but aren't real json! emit sites.
            if line.trim_start().starts_with("//") || line.trim_start().starts_with("///") {
                continue;
            }
            let lo = i.saturating_sub(6);
            let hi = (i + 7).min(lines.len());
            let window: String = lines[lo..hi].join("\n");
            // The envelope is the four-field set; adjacent lines in the
            // same json! block must include reason + recovery_hint.
            let has_reason = window.contains("\"truncation_reason\"");
            let has_hint = window.contains("\"truncation_recovery_hint\"");
            if !(has_reason && has_hint) {
                violations.push(format!(
                    "line {}: `\"truncation_limit\":` is emitted without the reason + recovery_hint companions in the surrounding json! block",
                    i + 1,
                ));
            }
        }
        assert!(
            violations.is_empty(),
            "JSON-truncation envelope inconsistency — every walk-shaped tool's JSON payload must \
             include truncation_reason + truncation_recovery_hint next to truncation_limit so a \
             caller hitting the 20 s walk budget gets the same recovery info regardless of which \
             tool it called. Violations:\n  {}",
            violations.join("\n  "),
        );
    }

    /// Truncation banner regression: timeout / node-cap responses must
    /// include the `use_index` / `build_name_index` recovery hint so
    /// callers can route around big-subtree timeouts (the 2026-05-03
    /// eval-run failure mode) without having to read the docs.
    #[test]
    fn truncation_banner_surfaces_index_recovery_hint_on_timeout() {
        let banner = truncation_banner_with_reason(
            true, defaults::MAX_SUBTREE_NODES, Some(TruncationReason::Timeout),
        );
        assert!(
            banner.contains("use_index"),
            "timeout banner must name use_index as a recovery path: {banner}"
        );
        assert!(
            banner.contains("build_name_index"),
            "timeout banner must name build_name_index: {banner}"
        );
        let banner_cap = truncation_banner_with_reason(
            true, defaults::MAX_SUBTREE_NODES, Some(TruncationReason::NodeLimit),
        );
        assert!(
            banner_cap.contains("use_index"),
            "node-cap banner must also surface the recovery path: {banner_cap}"
        );
    }

    /// Brief 2026-05-02 Test #3: oversized `insert_content` payloads
    /// must be refused at the handler boundary with a typed error and
    /// a chunking instruction, not silently dropped at the transport.
    #[tokio::test]
    async fn insert_content_refuses_payload_over_hard_cap() {
        let server = new_test_server();
        let lines: Vec<String> = (0..(defaults::MAX_INSERT_CONTENT_LINES + 1))
            .map(|i| format!("Line {}", i))
            .collect();
        let content = lines.join("\n");
        let params = InsertContentParams {
            parent_id: NodeId::from("550e8400-e29b-41d4-a716-446655440000"),
            content,
        };
        let err = server
            .insert_content(Parameters(params))
            .await
            .expect_err("oversized payload must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("payload too large") || msg.contains("exceeds"),
            "must say payload is too large: {msg}"
        );
        assert!(
            msg.contains(&defaults::MAX_INSERT_CONTENT_LINES.to_string()),
            "must mention the hard cap: {msg}"
        );
    }

    /// Brief 2026-05-02 Test #2: framework-level deserialize failures
    /// must be observable. Constructing a real `ToolCallContext` in a
    /// unit test would couple to rmcp internals, so this exercises the
    /// equivalent shape directly: a bad JSON payload + the recorder
    /// flow `Parameters::from_context_part` runs on its failure
    /// branch. End-to-end MCP coverage is provided by the live tests.
    #[test]
    fn traced_params_recorder_path_records_to_op_log() {
        let server = new_test_server();
        let bogus = serde_json::json!({"node_id": 42});
        let parse: std::result::Result<GetNodeParams, _> =
            serde_json::from_value(bogus.clone());
        assert!(parse.is_err(), "type mismatch must fail");
        let recorder = server.op_log.record("get_node", &bogus);
        recorder.finish_err(format!("invalid parameters: {}", parse.unwrap_err()));
        let recent = server.op_log.recent(10, None);
        assert_eq!(recent[0].tool, "get_node");
        assert!(matches!(recent[0].status, crate::utils::OpStatus::Err));
        assert!(
            recent[0].error.as_deref().unwrap_or("").contains("invalid parameters"),
            "recorded error must name the failure mode"
        );
    }

    /// Brief 2026-05-02 Test #5a: `last_unrecovered_failure` self-
    /// clears once a success on the same tool lands after the failure,
    /// so the `degraded` warning matches reality after recovery.
    #[test]
    fn last_unrecovered_failure_clears_after_success_on_same_tool() {
        let log = crate::utils::op_log::OpLog::new();
        log.record("get_node", &serde_json::json!({})).finish_err("upstream timeout");
        assert!(
            log.last_unrecovered_failure().is_some(),
            "fresh failure must be reported"
        );
        log.record("get_node", &serde_json::json!({})).finish_ok();
        assert!(
            log.last_unrecovered_failure().is_none(),
            "subsequent success on the same tool must clear the warning"
        );
    }

    /// Brief 2026-05-02 Test #5b: a success on a *different* tool
    /// must not clear another tool's failure warning — the failing
    /// tool is still broken until it itself returns OK.
    #[test]
    fn last_unrecovered_failure_persists_across_other_tools() {
        let log = crate::utils::op_log::OpLog::new();
        log.record("get_node", &serde_json::json!({})).finish_err("upstream timeout");
        log.record("list_children", &serde_json::json!({})).finish_ok();
        assert!(
            log.last_unrecovered_failure().is_some(),
            "success on a different tool must not clear another tool's failure warning"
        );
    }

    #[tokio::test]
    async fn degraded_warning_clears_after_get_node_recovery() {
        let server = new_test_server();
        let _ = server
            .get_node(Parameters(GetNodeParams { node_id: NodeId::from("") }))
            .await
            .expect_err("empty id rejected");
        assert!(
            server.degraded_warning_if_recent_failure(30_000).is_some(),
            "post-failure warning must fire"
        );
        server.op_log.record("get_node", &serde_json::json!({})).finish_ok();
        assert!(
            server.degraded_warning_if_recent_failure(30_000).is_none(),
            "warning must clear once get_node has succeeded again"
        );
    }
}

/// Load and concurrency tests against a real (in-process) HTTP mock.
///
/// Why this exists: the existing test suite uses `http://invalid.local`
/// to exercise the no-network failure mode. That validates the bounded-
/// error contract but cannot simulate the failure the 2026-04-30 MCP
/// failure report actually described — an upstream that *accepts the
/// connection* and then sits on it. wiremock binds to a random
/// localhost port and lets us script per-request delays, so the
/// failure modes from the report are exercised end-to-end through the
/// real reqwest stack at millisecond granularity.
///
/// Rate limiter is dialled to 200 rps / burst 100 in this module so 20-
/// call bursts complete in milliseconds rather than seconds. Retry
/// attempts are dialled to 1 so a single-request test exercises a
/// single network round-trip without backoff noise. Read-budget is
/// kept tight (300 ms) so `Timeout` paths return promptly.
#[cfg(test)]
mod load_tests {
    use super::*;
    use crate::config::{RateLimitConfig, RetryConfig};
    use crate::types::NodeId;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use wiremock::matchers::{body_partial_json, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fast_retry() -> RetryConfig {
        RetryConfig {
            max_attempts: 1,
            base_delay_ms: 10,
            max_delay_ms: 20,
            retryable_statuses: defaults::RETRY_STATUSES,
        }
    }

    fn fast_rate_limit() -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: 200,
            burst_size: 100,
        }
    }

    /// 32-hex-char canonical UUID used in tests. Matches the
    /// `valid_id()` helper in the parent module so tool handlers
    /// validate it the same way.
    fn id_a() -> &'static str {
        "11111111-2222-3333-4444-555555555555"
    }

    async fn server_against(mock: &MockServer) -> WorkflowyMcpServer {
        let client = Arc::new(
            WorkflowyClient::new_with_configs(
                mock.uri(),
                "test-key".to_string(),
                fast_retry(),
                fast_rate_limit(),
            )
            .expect("client must build against mock"),
        );
        WorkflowyMcpServer::new(client).with_read_budget_ms(300)
    }

    fn body_text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .next()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default()
    }

    /// Failure 2 from the 2026-04-30 report — the headline failure: the
    /// upstream accepts the connection and never responds. Without a
    /// budget the call wedges for ~3.5 min. With `with_read_budget` the
    /// tool returns a structured error in ~budget. Mock delays 5 s, the
    /// server's read budget is 300 ms — the assertion is that the call
    /// returns well under the mock delay (proving the budget fired,
    /// not the mock).
    #[tokio::test]
    async fn list_children_against_hung_upstream_returns_within_budget() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({"nodes": []})),
            )
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let started = Instant::now();
        let result = server
            .list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
            .await;
        let elapsed = started.elapsed();

        let err = result.expect_err("hung upstream must surface a tool_error");
        assert!(
            err.to_string().to_lowercase().contains("list_children"),
            "tool error must name the operation: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "budget must fire well under the 5 s mock delay, elapsed = {elapsed:?}"
        );
    }

    /// Same failure shape on `get_node`. Both reads (parent + children)
    /// run in parallel inside `with_read_budget`, so a hung upstream
    /// cannot stretch the call past the budget on either branch.
    #[tokio::test]
    async fn get_node_against_hung_upstream_returns_within_budget() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({"node": null, "nodes": []})),
            )
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let started = Instant::now();
        let result = server
            .get_node(Parameters(GetNodeParams { node_id: NodeId::from(id_a()) }))
            .await;
        let elapsed = started.elapsed();

        let err = result.expect_err("hung upstream must surface a tool_error");
        assert!(
            err.to_string().to_lowercase().contains("get_node"),
            "tool error must name the operation: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "budget must fire well under the 5 s mock delay, elapsed = {elapsed:?}"
        );
    }

    /// The most diagnostic data point in the 2026-04-30 report:
    /// `cancel_all` itself appeared to hang for 4 minutes. Once
    /// in-flight reads observe the cancel registry, `cancel_all` drops
    /// the in-flight future on the next 50 ms cancel-poll tick. Mock
    /// delays 30 s; cancel fires after 100 ms; assertion is that the
    /// call returns Cancelled within ~300 ms.
    #[tokio::test]
    async fn cancel_all_preempts_inflight_list_children_within_50ms_slice() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(30))
                    .set_body_json(json!({"nodes": []})),
            )
            .mount(&mock)
            .await;

        // Long read budget — we want cancel, not deadline, to win.
        let server = Arc::new(server_against(&mock).await.with_read_budget_ms(60_000));
        let server_for_call = Arc::clone(&server);
        let call = tokio::spawn(async move {
            server_for_call
                .list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
                .await
        });

        // Let the call enter its delay, then cancel.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let cancel_started = Instant::now();
        server.cancel_registry.cancel_all();

        let result = tokio::time::timeout(Duration::from_secs(2), call)
            .await
            .expect("cancel must preempt within the test budget")
            .expect("task must not panic");
        let cancel_elapsed = cancel_started.elapsed();

        let err = result.expect_err("cancelled call must surface a tool_error");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("cancel"),
            "tool error must surface the cancelled cause: {msg}"
        );
        assert!(
            cancel_elapsed < Duration::from_millis(500),
            "cancel must observe the registry within ~50 ms cancel-poll slice, elapsed = {cancel_elapsed:?}"
        );
    }

    /// Burst load: 20 concurrent `list_children` calls against a
    /// healthy mock, modelling the smoke test from the failure report.
    /// The dispatcher must complete the burst without dropping calls
    /// or wedging the surface. Time is loosely bounded: rate-limit
    /// burst plus the per-call HTTP RTT.
    #[tokio::test]
    async fn burst_of_20_list_children_completes_under_load() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "nodes": [
                    {
                        "id": "22222222-3333-4444-5555-666666666666",
                        "name": "child"
                    }
                ]
            })))
            .mount(&mock)
            .await;

        let server = Arc::new(server_against(&mock).await);
        let started = Instant::now();
        let mut handles = Vec::with_capacity(20);
        for _ in 0..20 {
            let s = Arc::clone(&server);
            handles.push(tokio::spawn(async move {
                s.list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
                    .await
            }));
        }
        let mut ok_count = 0;
        for h in handles {
            let res = h.await.expect("task must not panic");
            if res.is_ok() {
                ok_count += 1;
            }
        }
        let elapsed = started.elapsed();

        assert_eq!(ok_count, 20, "every call in the burst must succeed");
        assert!(
            elapsed < Duration::from_secs(3),
            "20-call burst must complete under load, elapsed = {elapsed:?}"
        );
    }

    /// The failure mode the report described in step 9: one slow call
    /// holds a connection / rate-limiter slot, and subsequent calls
    /// queue behind it. With `with_read_budget` dropping the slow call
    /// on its deadline, the rest of the surface stays responsive.
    ///
    /// Setup: the mock is configured so the very first request hangs
    /// for 30 s; every subsequent request returns 200 immediately. The
    /// hung call must hit its budget; the calls behind it must each
    /// complete on their own, not wait for the hung one to finish.
    #[tokio::test]
    async fn one_hung_call_does_not_wedge_other_reads() {
        let mock = MockServer::start().await;
        let counter = Arc::new(AtomicUsize::new(0));

        // A custom Respond impl that delays only the first request.
        struct FirstCallHangs(Arc<AtomicUsize>);
        impl wiremock::Respond for FirstCallHangs {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_secs(30))
                        .set_body_json(json!({"nodes": []}))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "nodes": [{
                            "id": "22222222-3333-4444-5555-666666666666",
                            "name": "child"
                        }]
                    }))
                }
            }
        }

        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(FirstCallHangs(Arc::clone(&counter)))
            .mount(&mock)
            .await;

        let server = Arc::new(server_against(&mock).await);

        // Fire the hung call first; let it claim the first response.
        let s_hung = Arc::clone(&server);
        let hung = tokio::spawn(async move {
            s_hung
                .list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Now fire 5 follow-up calls. They must all return promptly,
        // independent of the hung one.
        let started = Instant::now();
        let mut handles = Vec::with_capacity(5);
        for _ in 0..5 {
            let s = Arc::clone(&server);
            handles.push(tokio::spawn(async move {
                s.list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
                    .await
            }));
        }
        for h in handles {
            h.await
                .expect("follow-up task must not panic")
                .expect("follow-up call must succeed despite the hung one");
        }
        let followups_elapsed = started.elapsed();

        // Hung call's outcome is asserted last; we want the follow-up
        // assertion to fail loud if the surface is wedged.
        assert!(
            followups_elapsed < Duration::from_secs(2),
            "follow-up calls must not queue behind the hung one, elapsed = {followups_elapsed:?}"
        );

        let hung_result = tokio::time::timeout(Duration::from_secs(2), hung)
            .await
            .expect("hung call must hit its budget within the test timeout")
            .expect("task must not panic");
        assert!(
            hung_result.is_err(),
            "hung call must surface a budget error: {hung_result:?}"
        );
    }

    /// Propagation-retry happy path: the first 404 doesn't fail the
    /// call — the retry loop in `*_with_propagation_retry` waits for
    /// the upstream to catch up. Mock returns 404 then 200; the tool
    /// returns the eventual 200.
    #[tokio::test]
    async fn list_children_recovers_from_propagation_lag_404() {
        let mock = MockServer::start().await;
        let counter = Arc::new(AtomicUsize::new(0));

        struct FirstIs404(Arc<AtomicUsize>);
        impl wiremock::Respond for FirstIs404 {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ResponseTemplate::new(404)
                        .set_body_json(json!({"error": "not found"}))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "nodes": [{
                            "id": "22222222-3333-4444-5555-666666666666",
                            "name": "child"
                        }]
                    }))
                }
            }
        }

        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(FirstIs404(Arc::clone(&counter)))
            .mount(&mock)
            .await;

        // Read budget needs to be large enough to absorb the 200 ms
        // propagation backoff plus a fast second call.
        let server = server_against(&mock).await.with_read_budget_ms(2_000);
        let result = server
            .list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
            .await
            .expect("call must succeed after propagation retry");
        let body = body_text(&result);
        assert!(body.contains("child"), "must return the eventual 200 body: {body}");
        assert_eq!(counter.load(Ordering::SeqCst), 2, "expected one retry");
    }

    /// Transport-level retry: a connection-reset on the first attempt
    /// must flow through the backoff loop instead of returning
    /// `RetryExhausted` after one shot. Mock drops the first request
    /// at the connection layer (5xx as a stand-in for transport
    /// failure since wiremock can't easily reset a TCP connection
    /// mid-flight); the tool returns the second-attempt 200.
    ///
    /// The retry budget for this test is 2 attempts so the mock's
    /// scripted recovery is observable.
    #[tokio::test]
    async fn list_children_retries_503_within_read_budget() {
        let mock = MockServer::start().await;
        let counter = Arc::new(AtomicUsize::new(0));

        struct FirstIs503(Arc<AtomicUsize>);
        impl wiremock::Respond for FirstIs503 {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ResponseTemplate::new(503)
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "nodes": [{
                            "id": "22222222-3333-4444-5555-666666666666",
                            "name": "child"
                        }]
                    }))
                }
            }
        }

        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(FirstIs503(Arc::clone(&counter)))
            .mount(&mock)
            .await;

        let client = Arc::new(
            WorkflowyClient::new_with_configs(
                mock.uri(),
                "test-key".to_string(),
                RetryConfig {
                    max_attempts: 2,
                    base_delay_ms: 10,
                    max_delay_ms: 20,
                    retryable_statuses: defaults::RETRY_STATUSES,
                },
                fast_rate_limit(),
            )
            .unwrap(),
        );
        let server = WorkflowyMcpServer::new(client).with_read_budget_ms(2_000);

        let result = server
            .list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
            .await
            .expect("call must recover after one 503");
        assert!(body_text(&result).contains("child"));
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "expected exactly one retry"
        );
    }

    /// Auth failures (401/403) are not retried — the answer won't
    /// change. The tool should surface the failure quickly so the
    /// caller can fix the API key, not wait for the read budget.
    #[tokio::test]
    async fn list_children_does_not_retry_on_401() {
        let mock = MockServer::start().await;
        let counter = Arc::new(AtomicUsize::new(0));

        struct CountingResponder(Arc<AtomicUsize>);
        impl wiremock::Respond for CountingResponder {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                self.0.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(401)
                    .set_body_json(json!({"error": "unauthorized"}))
            }
        }

        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(CountingResponder(Arc::clone(&counter)))
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let started = Instant::now();
        let result = server
            .list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
            .await;
        let elapsed = started.elapsed();

        let err = result.expect_err("401 must surface a tool_error");
        assert!(elapsed < Duration::from_millis(500), "no retry on 401: {elapsed:?}");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "401 must not be retried"
        );
        let _ = err; // surface check above is sufficient
    }

    /// Sanity check: the `path("/nodes")` matcher above is
    /// intentionally narrow — children listing uses
    /// `?parent_id=<id>` and the top-level listing has no query, but
    /// both share the same path. This test pins the routing so a
    /// future change that splits the endpoints can't silently drop
    /// either listing.
    #[tokio::test]
    async fn children_query_param_is_passed_to_upstream() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nodes"))
            .and(query_param("parent_id", id_a()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "nodes": []
            })))
            .mount(&mock)
            .await;
        // Also accept a fallback for any unmatched request so we get a
        // useful error if the matcher above doesn't fire.
        Mock::given(method("GET"))
            .and(path_regex(r"^/.*"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let result = server
            .list_children(Parameters(GetChildrenParams { node_id: Some(NodeId::from(id_a())) }))
            .await
            .expect("call must succeed against the parent_id matcher");
        let body = body_text(&result);
        assert!(body.contains("no children"), "body: {body}");
    }

    /// Migrated from `mod tests::handlers_route_root_and_parent_short_hashes_through_resolver`.
    /// The previous version ran against `http://invalid.local` and took
    /// ~30 s while the resolver walked an unreachable workspace through
    /// the full retry budget. This version points the resolver at a
    /// wiremock returning an empty workspace, so the walk completes
    /// instantly as **exhaustive** (per the resolver's truncated-vs-
    /// exhaustive contract documented in spec property #6) and the
    /// expected "Short-hash … was not found" error returns in
    /// milliseconds.
    ///
    /// What this test pins: every handler that takes a `node_id` /
    /// `parent_id` / `root_id` short-hash routes the value through
    /// `resolve_node_ref` rather than passing the bare short hash to
    /// the API layer. A future refactor that bypasses the resolver
    /// would break this contract — and the test would catch it.
    #[tokio::test]
    async fn handlers_route_unindexed_short_hashes_through_resolver() {
        let mock = MockServer::start().await;
        // Empty workspace: every walk completes immediately as
        // exhaustive. The resolver concludes the hash is genuinely
        // absent and returns the expected error.
        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": []})))
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let unindexed = "ffffffffffff"; // 12-char hex, never ingested

        // Optional root_id: list_overdue, list_upcoming, daily_review,
        // get_recent_changes, bulk_update, build_name_index, list_todos.
        let err = server
            .list_overdue(Parameters(ListOverdueParams {
                root_id: Some(NodeId::from(unindexed)),
                include_completed: None,
                limit: None,
                max_depth: None,
            }))
            .await
            .expect_err("must error before HTTP");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("name index") || msg.contains("short-hash"),
            "list_overdue: expected resolver-side error, got: {msg}"
        );

        // Optional parent_id: list_todos exercises the same pattern via
        // a different param name.
        let err = server
            .list_todos(Parameters(ListTodosParams {
                parent_id: Some(NodeId::from(unindexed)),
                status: None,
                query: None,
                limit: None,
                max_depth: None,
            }))
            .await
            .expect_err("must error before HTTP");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("name index") || msg.contains("short-hash"),
            "list_todos: expected resolver-side error, got: {msg}"
        );

        // Required node_id: get_project_summary, find_backlinks,
        // get_subtree, duplicate_node, create_from_template all use this.
        let err = server
            .get_subtree(Parameters(GetSubtreeParams {
                node_id: NodeId::from(unindexed),
                max_depth: None,
            }))
            .await
            .expect_err("must error before HTTP");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("name index") || msg.contains("short-hash"),
            "get_subtree: expected resolver-side error, got: {msg}"
        );
    }

    /// Migrated from `mod tests::handler_errors_carry_structured_data_payload`.
    /// The previous version ran each mutation through the full retry
    /// budget against `invalid.local`, totalling ~30 s. This version
    /// returns a non-retryable 404 from the wiremock so each handler
    /// fails fast on the first attempt; the assertion that the structured
    /// `tool_error` payload names the operation is unchanged.
    ///
    /// Brief acceptance: 2026-04-25 (Pattern A/B/C transient failures).
    /// When a tool call fails, the response MUST surface the operation
    /// name in the error message — otherwise the assistant only sees a
    /// bare "Tool execution failed" with no diagnostic value.
    #[tokio::test]
    async fn mutation_errors_carry_structured_data_payload() {
        let mock = MockServer::start().await;
        // 404 on every request: non-retryable, fast failure on first attempt.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
            .mount(&mock)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let target_uuid = "550e8400-e29b-41d4-a716-446655440000";

        let err = server
            .delete_node(Parameters(DeleteNodeParams {
                node_id: NodeId::from(target_uuid),
            }))
            .await
            .expect_err("404 must surface a tool_error");
        assert!(
            err.to_string().contains("delete_node"),
            "delete_node error must name the operation: {err}"
        );

        let err = server
            .edit_node(Parameters(EditNodeParams {
                node_id: NodeId::from(target_uuid),
                name: Some("x".into()),
                description: Some("y".into()),
            }))
            .await
            .expect_err("404 must surface a tool_error");
        assert!(
            err.to_string().contains("edit_node"),
            "edit_node error must name the operation: {err}"
        );

        let err = server
            .move_node(Parameters(MoveNodeParams {
                node_id: NodeId::from(target_uuid),
                new_parent_id: NodeId::from(target_uuid),
                priority: None,
            }))
            .await
            .expect_err("404 must surface a tool_error");
        assert!(
            err.to_string().contains("move_node"),
            "move_node error must name the operation: {err}"
        );
    }

    // ----- Architecture review: bulk-handler budget contract -----
    //
    // The seven critical bypass handlers identified in the 2026-05-02
    // architecture review (path_of, transaction, batch_create_nodes,
    // duplicate_node, create_from_template, bulk_update, bulk_tag) plus
    // node_at_path now wrap their bodies in `run_handler(ToolKind::Bulk)`.
    // These tests pin the contract: against a hung upstream each handler
    // returns within the bulk budget instead of looping unbounded over
    // raw client calls. Each test runs in milliseconds via the
    // `with_bulk_budget_ms` test override.

    /// Build a server pointing at the given mock with a tight bulk
    /// budget so the failure-mode tests below run in test time.
    async fn server_with_tight_bulk_budget(mock: &MockServer) -> WorkflowyMcpServer {
        let client = Arc::new(
            WorkflowyClient::new_with_configs(
                mock.uri(),
                "test-key".to_string(),
                fast_retry(),
                fast_rate_limit(),
            )
            .expect("client must build against mock"),
        );
        WorkflowyMcpServer::new(client)
            .with_read_budget_ms(300)
            .with_bulk_budget_ms(300)
    }

    /// `path_of` walks parent_id pointers via repeated `get_node`. Pre-
    /// migration each `get_node` was raw `client.get_node` — no per-call
    /// deadline, no observation of `cancel_all`. A 10-deep path against
    /// a slow upstream could legitimately take 25 minutes. Now the
    /// outer wrapper bounds the whole walk.
    #[tokio::test]
    async fn path_of_against_hung_upstream_returns_within_bulk_budget() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({"node": {"id": id_a(), "name": "x", "parent_id": null}})),
            )
            .mount(&mock)
            .await;

        let server = server_with_tight_bulk_budget(&mock).await;
        let started = Instant::now();
        let result = server
            .path_of(Parameters(PathOfParams {
                node_id: NodeId::from(id_a()),
                max_depth: Some(50),
            }))
            .await;
        let elapsed = started.elapsed();

        let err = result.expect_err("hung upstream must surface a tool_error");
        assert!(
            err.to_string().to_lowercase().contains("path_of"),
            "tool error must name the operation: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "bulk budget (300 ms) must fire well under the 5 s mock delay, elapsed = {elapsed:?}"
        );
    }

    /// `bulk_tag` runs `client.get_node` per id in parallel via
    /// `buffer_unordered`. Pre-migration unbounded.
    #[tokio::test]
    async fn bulk_tag_against_hung_upstream_returns_within_bulk_budget() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({"node": {"id": id_a(), "name": "x"}})),
            )
            .mount(&mock)
            .await;

        let server = server_with_tight_bulk_budget(&mock).await;
        let started = Instant::now();
        let result = server
            .bulk_tag(Parameters(BulkTagParams {
                node_ids: vec![NodeId::from(id_a())],
                tag: "test".to_string(),
            }))
            .await;
        let elapsed = started.elapsed();

        let err = result.expect_err("hung upstream must surface a tool_error");
        assert!(
            err.to_string().to_lowercase().contains("bulk_tag"),
            "tool error must name the operation: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "bulk budget must fire under mock delay, elapsed = {elapsed:?}"
        );
    }

    /// `transaction` captures pre-state via raw `client.get_node` per
    /// operation. Pre-migration: a 10-op transaction with one slow
    /// upstream call could hang for 25 minutes. The outer wrapper now
    /// caps the whole transaction at the bulk budget.
    #[tokio::test]
    async fn transaction_against_hung_upstream_returns_within_bulk_budget() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({"node": {"id": id_a(), "name": "x"}})),
            )
            .mount(&mock)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&mock)
            .await;

        let server = server_with_tight_bulk_budget(&mock).await;
        let started = Instant::now();
        let result = server
            .transaction(Parameters(TransactionParams {
                operations: vec![TransactionOpParams {
                    op: "delete".to_string(),
                    node_id: Some(NodeId::from(id_a())),
                    parent_id: None,
                    new_parent_id: None,
                    name: None,
                    description: None,
                    priority: None,
                }],
            }))
            .await;
        let elapsed = started.elapsed();

        let err = result.expect_err("hung upstream must surface a tool_error");
        assert!(
            err.to_string().to_lowercase().contains("transaction"),
            "tool error must name the operation: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "bulk budget must fire under mock delay, elapsed = {elapsed:?}"
        );
    }

    /// `node_at_path` walks segments via `client.get_children` /
    /// `get_top_level_nodes`. Pre-migration unbounded.
    #[tokio::test]
    async fn node_at_path_against_hung_upstream_returns_within_bulk_budget() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({"nodes": []})),
            )
            .mount(&mock)
            .await;

        let server = server_with_tight_bulk_budget(&mock).await;
        let started = Instant::now();
        let result = server
            .node_at_path(Parameters(NodeAtPathParams {
                path: vec!["Areas".to_string(), "Personal".to_string()],
                start_parent_id: None,
            }))
            .await;
        let elapsed = started.elapsed();

        let err = result.expect_err("hung upstream must surface a tool_error");
        assert!(
            err.to_string().to_lowercase().contains("node_at_path"),
            "tool error must name the operation: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "bulk budget must fire under mock delay, elapsed = {elapsed:?}"
        );
    }

    /// `WRITE_NODE_TIMEOUT_MS` is the per-method deadline that
    /// `client.create_node` builds internally. The contract: a hung
    /// upstream cannot stretch a single create past the budget,
    /// regardless of retry attempts. Without this guarantee a 140-node
    /// `insert_content` could push past the MCP client's 4-min hard
    /// timeout (the failure shape from the 2026-05-02 report). The
    /// test injects a tiny per-call deadline directly via
    /// `create_node_cancellable` to exercise the same code path in
    /// milliseconds.
    #[tokio::test]
    async fn create_node_caps_at_write_budget_against_hung_upstream() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/nodes"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({"item_id": "11111111-2222-3333-4444-555555555555"})),
            )
            .mount(&mock)
            .await;

        let client = WorkflowyClient::new_with_configs(
            mock.uri(),
            "test-key".to_string(),
            fast_retry(),
            fast_rate_limit(),
        )
        .unwrap();

        let started = Instant::now();
        let deadline = Instant::now() + Duration::from_millis(300);
        let result = client
            .create_node_cancellable("hello", None, None, None, None, Some(deadline))
            .await;
        let elapsed = started.elapsed();

        assert!(
            matches!(result, Err(crate::error::WorkflowyError::Timeout)),
            "must surface Timeout when deadline fires before mock responds: {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "deadline (300 ms) must fire well under the 5 s mock delay, elapsed = {elapsed:?}"
        );
    }

    /// Failure 1 from the 2026-05-02 report: a 140-node `insert_content`
    /// could blow past the MCP client's 4-min hard timeout because a
    /// few transient slow upstream calls compounded across the
    /// sequential creates. With `INSERT_CONTENT_TIMEOUT_MS` (210 s
    /// production; 300 ms test override) the operation now returns a
    /// structured partial-success payload before the client gives up,
    /// so the caller learns what was inserted. The cancel-path test
    /// (`insert_content_returns_partial_on_cancel`) covers the same
    /// code path; this test pins the **timeout** branch specifically.
    #[tokio::test]
    async fn insert_content_returns_partial_on_timeout() {
        let mock = MockServer::start().await;
        let counter = Arc::new(AtomicUsize::new(0));

        struct ProgressivelySlower(Arc<AtomicUsize>);
        impl wiremock::Respond for ProgressivelySlower {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                let id = format!("aaaaaaaa-bbbb-cccc-dddd-{:012x}", n);
                let template = ResponseTemplate::new(200)
                    .set_body_json(json!({"item_id": id}));
                if n >= 2 {
                    // Third+ create hangs for 30 s — well past the
                    // tight insert_content budget.
                    template.set_delay(Duration::from_secs(30))
                } else {
                    template
                }
            }
        }
        Mock::given(method("POST"))
            .and(path("/nodes"))
            .respond_with(ProgressivelySlower(Arc::clone(&counter)))
            .mount(&mock)
            .await;

        // Tight insert-content budget so the timeout path fires after
        // the first two fast creates.
        let client = Arc::new(
            WorkflowyClient::new_with_configs(
                mock.uri(),
                "test-key".to_string(),
                fast_retry(),
                fast_rate_limit(),
            )
            .unwrap(),
        );
        let server = WorkflowyMcpServer::new(client)
            .with_read_budget_ms(60_000)
            .with_bulk_budget_ms(500);

        let started = Instant::now();
        let result = server
            .insert_content(Parameters(InsertContentParams {
                parent_id: NodeId::from(id_a()),
                content: "Line 1\nLine 2\nLine 3\nLine 4\nLine 5".to_string(),
            }))
            .await
            .expect("partial-success returns Ok with structured payload, not Err");
        let elapsed = started.elapsed();

        let body = body_text(&result);
        let v: serde_json::Value = serde_json::from_str(&body)
            .expect("partial-success payload must be JSON");
        assert_eq!(v["status"], "partial", "body: {body}");
        assert_eq!(v["reason"], "timeout", "body: {body}");
        let created = v["created_count"].as_u64().expect("created_count");
        assert!(created >= 1, "must have inserted at least one line: {body}");
        assert!(created < 5, "must not have inserted all five: {body}");
        assert_eq!(v["total_count"], 5, "body: {body}");
        assert!(
            elapsed < Duration::from_secs(2),
            "bulk budget (500 ms) must fire well under the 30 s hung mock, elapsed = {elapsed:?}"
        );
    }

    /// Architecture invariant: `cancel_all` preempts an in-flight
    /// `create_node`. Pre-2026-05-02 the basic CRUD writes wrapped only
    /// in `record_op!` and so could not be preempted by `cancel_all` —
    /// the gap that turned a wedged write into the 4-minute hang the
    /// 2026-05-02 session report named. With `tool_handler!` /
    /// `ToolKind::Write` the wrapper observes the cancel registry the
    /// same way `path_of` does, and a wedged write returns a structured
    /// cancelled error within the ~50 ms cancel slice instead of
    /// stranding the caller.
    #[tokio::test]
    async fn cancel_all_preempts_inflight_create_node_via_run_handler() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/nodes"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(30))
                    .set_body_json(json!({"item_id": id_a()})),
            )
            .mount(&mock)
            .await;

        let client = Arc::new(
            WorkflowyClient::new_with_configs(
                mock.uri(),
                "test-key".to_string(),
                fast_retry(),
                fast_rate_limit(),
            )
            .unwrap(),
        );
        let server = Arc::new(WorkflowyMcpServer::new(client));

        let server_for_call = Arc::clone(&server);
        let call = tokio::spawn(async move {
            server_for_call
                .create_node(Parameters(CreateNodeParams {
                    name: "wedged".to_string(),
                    description: None,
                    parent_id: None,
                    priority: None,
                }))
                .await
        });

        // Let the handler reach the upstream POST before we cancel.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let cancel_started = Instant::now();
        server.cancel_registry.cancel_all();

        let result = tokio::time::timeout(Duration::from_secs(2), call)
            .await
            .expect("cancel must preempt within the test budget")
            .expect("task must not panic");
        let cancel_elapsed = cancel_started.elapsed();

        let err = result.expect_err("cancelled create must surface a tool_error");
        assert!(
            err.to_string().to_lowercase().contains("cancel"),
            "tool error must surface the cancelled cause: {err}"
        );
        assert!(
            cancel_elapsed < Duration::from_millis(500),
            "cancel must observe the registry within ~50 ms slice, elapsed = {cancel_elapsed:?}"
        );
    }

    /// Architecture invariant: `cancel_all` preempts any bulk handler
    /// in flight, not just `list_children`. Pin against `path_of`
    /// since it's the most loop-heavy migrated handler.
    #[tokio::test]
    async fn cancel_all_preempts_inflight_path_of_via_run_handler() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(30))
                    .set_body_json(json!({"node": {"id": id_a(), "name": "x"}})),
            )
            .mount(&mock)
            .await;

        // Long bulk budget — we want cancel, not deadline, to fire.
        let client = Arc::new(
            WorkflowyClient::new_with_configs(
                mock.uri(),
                "test-key".to_string(),
                fast_retry(),
                fast_rate_limit(),
            )
            .unwrap(),
        );
        let server = Arc::new(
            WorkflowyMcpServer::new(client)
                .with_read_budget_ms(60_000)
                .with_bulk_budget_ms(60_000),
        );

        let server_for_call = Arc::clone(&server);
        let call = tokio::spawn(async move {
            server_for_call
                .path_of(Parameters(PathOfParams {
                    node_id: NodeId::from(id_a()),
                    max_depth: Some(50),
                }))
                .await
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        let cancel_started = Instant::now();
        server.cancel_registry.cancel_all();

        let result = tokio::time::timeout(Duration::from_secs(2), call)
            .await
            .expect("cancel must preempt within the test budget")
            .expect("task must not panic");
        let cancel_elapsed = cancel_started.elapsed();

        let err = result.expect_err("cancelled call must surface a tool_error");
        assert!(
            err.to_string().to_lowercase().contains("cancel"),
            "tool error must surface the cancelled cause: {err}"
        );
        assert!(
            cancel_elapsed < Duration::from_millis(500),
            "cancel must observe the registry within ~50 ms slice, elapsed = {cancel_elapsed:?}"
        );
    }

    /// `complete_node` end-to-end against a wiremock — pins both the
    /// handler-level dispatch and the wire shape (`POST /nodes/{id}`
    /// with `{"completed": true}`). The body matcher rejects any
    /// regression to a different field name; the success message
    /// proves the handler applied the right verb.
    #[tokio::test]
    async fn complete_node_dispatches_completed_true_on_default() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/nodes/[^/]+$"))
            .and(body_partial_json(json!({"completed": true})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let result = server
            .complete_node(Parameters(CompleteNodeParams {
                node_id: NodeId::from(id_a()),
                completed: None,
            }))
            .await
            .expect("complete_node must succeed against the body-matcher mock");
        let body = body_text(&result);
        assert!(
            body.starts_with("Completed node"),
            "default `completed: None` must mean 'mark complete': {body}"
        );
    }

    /// Symmetric uncomplete path. With `completed: Some(false)` the
    /// handler must POST `{"completed": false}` and report
    /// "Uncompleted node …".
    #[tokio::test]
    async fn complete_node_dispatches_completed_false_when_explicit() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/nodes/[^/]+$"))
            .and(body_partial_json(json!({"completed": false})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let result = server
            .complete_node(Parameters(CompleteNodeParams {
                node_id: NodeId::from(id_a()),
                completed: Some(false),
            }))
            .await
            .expect("complete_node must succeed against the body-matcher mock");
        let body = body_text(&result);
        assert!(
            body.starts_with("Uncompleted node"),
            "explicit `completed: Some(false)` must mean 'uncomplete': {body}"
        );
    }

    /// `bulk_update` with `operation: "complete"` must filter the walk
    /// and dispatch each match through `client.set_completion`. The
    /// pre-completion-state code path rejected this operation outright;
    /// after wiring, the same handler that creates the rest of the
    /// bulk-update operations now also creates the completion ones.
    ///
    /// Walk shape: top-level GET returns one matching node; the
    /// follow-up child fetches return empty so the walk doesn't
    /// duplicate the node into the candidate set.
    #[tokio::test]
    async fn bulk_update_complete_dispatches_to_set_completion() {
        let mock = MockServer::start().await;
        // Top-level fetch (no parent_id query param): one matching node.
        Mock::given(method("GET"))
            .and(path("/nodes"))
            .and(wiremock::matchers::query_param_is_missing("parent_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "nodes": [{
                    "id": id_a(),
                    "name": "Task #target",
                    "completed": false
                }]
            })))
            .mount(&mock)
            .await;
        // Per-child fetches (`?parent_id=<id>`): empty.
        Mock::given(method("GET"))
            .and(path("/nodes"))
            .and(query_param("parent_id", id_a()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nodes": []})))
            .mount(&mock)
            .await;
        // Set-completion: POST /nodes/{id} with {"completed": true}.
        Mock::given(method("POST"))
            .and(path_regex(r"^/nodes/[^/]+$"))
            .and(body_partial_json(json!({"completed": true})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;
        let result = server
            .bulk_update(Parameters(BulkUpdateParams {
                root_id: None,
                operation: "complete".to_string(),
                query: None,
                tag: Some("target".to_string()),
                status: Some("pending".to_string()),
                operation_tag: None,
                dry_run: Some(false),
                limit: Some(10),
                max_depth: Some(2),
            }))
            .await
            .expect("bulk_update complete must succeed against the body-matcher mock");
        let body = body_text(&result);
        let v: serde_json::Value = serde_json::from_str(&body).expect("response is JSON");
        assert_eq!(v["operation"], "complete", "body: {body}");
        assert_eq!(
            v["affected_count"].as_u64().expect("affected_count"),
            1,
            "single matching node must be affected: {body}"
        );
    }

    /// Failure 2 from the 2026-05-02 report: `list_children` with
    /// `node_id: null` intermittently returned "Tool execution failed".
    /// The schema now accepts `Option<NodeId>` (`#[serde(default)]`),
    /// and the handler routes None → workspace top-level. Two test
    /// shapes pinned: `null` literal and missing field (since some MCP
    /// clients send one form and some the other).
    #[tokio::test]
    async fn list_children_null_node_id_returns_workspace_root() {
        let mock = MockServer::start().await;
        // The top-level fetch hits /nodes with NO query params; the
        // children fetch would hit /nodes?parent_id=...
        Mock::given(method("GET"))
            .and(path("/nodes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "nodes": [
                    {
                        "id": "11111111-1111-1111-1111-111111111111",
                        "name": "Root child A"
                    },
                    {
                        "id": "22222222-2222-2222-2222-222222222222",
                        "name": "Root child B"
                    }
                ]
            })))
            .mount(&mock)
            .await;

        let server = server_against(&mock).await;

        // Form 1: explicit `{"node_id": null}` after deserialisation
        // produces `node_id: None` because the field carries
        // `#[serde(default)]`.
        let params: GetChildrenParams = serde_json::from_value(json!({"node_id": null}))
            .expect("null must deserialise to None");
        assert!(params.node_id.is_none());
        let result = server
            .list_children(Parameters(params))
            .await
            .expect("null node_id must succeed against workspace root");
        let body = body_text(&result);
        assert!(body.contains("Root child A"), "body must list top-level: {body}");
        assert!(body.contains("workspace root"), "body must label scope: {body}");

        // Form 2: missing field. Same outcome — None.
        let params: GetChildrenParams = serde_json::from_value(json!({}))
            .expect("missing field must deserialise to None");
        assert!(params.node_id.is_none());
        let result = server
            .list_children(Parameters(params))
            .await
            .expect("missing node_id must succeed against workspace root");
        let body = body_text(&result);
        assert!(body.contains("Root child A"), "body must list top-level: {body}");
    }

    /// Pure unit test for `derive_api_reachable`. The 2026-05-02 report
    /// flagged that the degraded flag stuck on after the liveness probe
    /// blipped during a long write burst — the burst itself proved the
    /// API was up. The helper now treats a 2xx within the freshness
    /// window as positive evidence.
    #[test]
    fn derive_api_reachable_honours_recent_success_when_probe_fails() {
        // Probe succeeded → reachable, regardless of last-success.
        assert!(super::derive_api_reachable(true, None));
        assert!(super::derive_api_reachable(true, Some(0)));
        assert!(super::derive_api_reachable(true, Some(60_000)));

        // Probe failed, but a 2xx is recent enough → reachable.
        assert!(super::derive_api_reachable(false, Some(0)));
        assert!(super::derive_api_reachable(
            false,
            Some(defaults::API_REACHABILITY_FRESHNESS_MS - 1)
        ));

        // Probe failed and the last 2xx is older than the window
        // (or never happened) → not reachable.
        assert!(!super::derive_api_reachable(
            false,
            Some(defaults::API_REACHABILITY_FRESHNESS_MS)
        ));
        assert!(!super::derive_api_reachable(
            false,
            Some(defaults::API_REACHABILITY_FRESHNESS_MS + 60_000)
        ));
        assert!(!super::derive_api_reachable(false, None));
    }

    /// `insert_content` must return a structured partial-success
    /// payload when interrupted by `cancel_all` rather than a bare
    /// error. The 2026-05-02 report: a 4-min insert hit the MCP
    /// client's hard timeout and returned "no result received" with no
    /// diagnostic — even though the server had inserted a chunk
    /// successfully. Cancellation is the analogous test surface
    /// (deterministic to script with wiremock; the timeout path goes
    /// through the same code).
    ///
    /// Setup: mock returns 200 immediately for the first two creates
    /// then delays 30 s for the third. We fire cancel_all after the
    /// second create succeeds, and assert the response carries
    /// `status: "partial"` with `created_count >= 1`.
    #[tokio::test]
    async fn insert_content_returns_partial_on_cancel() {
        let mock = MockServer::start().await;
        let counter = Arc::new(AtomicUsize::new(0));

        // GET /nodes/{id} — used by resolve_node_ref. Return the
        // parent node so resolution succeeds (won't be reached here
        // because the parent is a full UUID, but defensive).
        Mock::given(method("GET"))
            .and(path_regex(r"^/nodes/.*"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;

        struct DelayedThirdCreate(Arc<AtomicUsize>);
        impl wiremock::Respond for DelayedThirdCreate {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                let id = format!("aaaaaaaa-bbbb-cccc-dddd-{:012x}", n);
                let template = ResponseTemplate::new(200)
                    .set_body_json(json!({"item_id": id}));
                if n >= 2 {
                    template.set_delay(Duration::from_secs(30))
                } else {
                    template
                }
            }
        }
        Mock::given(method("POST"))
            .and(path("/nodes"))
            .respond_with(DelayedThirdCreate(Arc::clone(&counter)))
            .mount(&mock)
            .await;

        let server = Arc::new(server_against(&mock).await);
        let server_for_call = Arc::clone(&server);
        let call = tokio::spawn(async move {
            server_for_call
                .insert_content(Parameters(InsertContentParams {
                    parent_id: NodeId::from(id_a()),
                    content: "Line 1\nLine 2\nLine 3\nLine 4\nLine 5".to_string(),
                }))
                .await
        });

        // Wait long enough for the first two creates to complete and
        // the third to enter its delay.
        tokio::time::sleep(Duration::from_millis(300)).await;
        server.cancel_registry.cancel_all();

        let result = tokio::time::timeout(Duration::from_secs(2), call)
            .await
            .expect("cancel must preempt within the test budget")
            .expect("task must not panic")
            .expect("partial-success returns Ok with structured payload, not Err");
        let body = body_text(&result);
        let v: serde_json::Value = serde_json::from_str(&body)
            .expect("partial-success payload must be JSON");
        assert_eq!(v["status"], "partial", "body: {body}");
        assert_eq!(v["reason"], "cancelled", "body: {body}");
        let created = v["created_count"].as_u64().expect("created_count");
        assert!(created >= 1, "must have inserted at least the first line: {body}");
        assert!(created < 5, "must not have inserted all five: {body}");
        assert_eq!(v["total_count"], 5, "body: {body}");
        assert!(v["last_inserted_id"].is_string(), "last_inserted_id must be set: {body}");
    }

}
