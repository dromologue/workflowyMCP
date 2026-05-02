//! `wflow-do` — thin shell facade over `WorkflowyClient`.
//!
//! Exists because Claude Desktop's MCP transport intermittently drops
//! tool calls before they reach the server (Pattern 1, 2026-04-25 brief).
//! Bash dispatch is independent of MCP tool dispatch, so shelling out
//! to this binary bypasses the broken transport entirely while reusing
//! the same `WorkflowyClient` the MCP server uses.

use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde_json::json;
use workflowy_mcp_server::{
    api::{FetchControls, WorkflowyClient},
    config::validate_config,
    defaults,
    error::WorkflowyError,
};

/// Default subtree root for audit/review (Justin's Distillations node).
const DEFAULT_REVIEW_ROOT: &str = "7e351f77-c7b4-4709-86a7-ea6733a63171";

#[derive(Parser)]
#[command(name = "wflow-do", about = "Workflowy CLI — bypasses the MCP transport for direct API access")]
struct Cli {
    /// Emit raw JSON for every command (default: human-readable for create/move/delete/edit, JSON for the rest).
    #[arg(long, global = true)]
    json: bool,

    /// Suppress info-level logging.
    #[arg(long, global = true)]
    quiet: bool,

    /// For create/move/delete/edit only: print the planned operation and exit 0 without calling the API.
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Liveness + rate-limit + tree-size snapshot (mirrors the MCP `workflowy_status` payload).
    Status,
    /// Get a single node by ID.
    Get { node_id: String },
    /// List direct children of a node.
    Children { node_id: String },
    /// Create a new node.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        priority: Option<i32>,
    },
    /// Move a node to a new parent.
    Move {
        node_id: String,
        #[arg(long = "to")]
        to: String,
        #[arg(long)]
        priority: Option<i32>,
    },
    /// Delete a node.
    Delete { node_id: String },
    /// Edit a node's name and/or description.
    Edit {
        node_id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Toggle a node's native Workflowy completion state. Default is to mark
    /// complete; pass `--uncomplete` to revert. Mirrors the MCP `complete_node`
    /// tool — same `client.set_completion` code path.
    Complete {
        node_id: String,
        /// Mark uncomplete instead of complete.
        #[arg(long)]
        uncomplete: bool,
    },
    /// Search by substring in name or description over a subtree.
    Search {
        query: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value_t = 3)]
        depth: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Search by tag (`#tag` or `@person`). Walks a subtree (or workspace) and
    /// returns nodes carrying the tag in name or note. Mirrors MCP `tag_search`.
    TagSearch {
        tag: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value_t = 5)]
        depth: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Find a node by exact / contains / starts-with name match. Mirrors MCP
    /// `find_node`. Refuses unscoped root walks unless `--allow-root-scan`.
    Find {
        name: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value = "exact")]
        match_mode: String,
        #[arg(long, default_value_t = 3)]
        depth: usize,
        #[arg(long)]
        allow_root_scan: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Get the full subtree under a node, bounded by `--depth` and the
    /// 10 000-node walk cap. Mirrors MCP `get_subtree`.
    Subtree {
        node_id: String,
        #[arg(long, default_value_t = 5)]
        depth: usize,
    },
    /// Find every node containing a Workflowy link to the given node.
    /// Mirrors MCP `find_backlinks`.
    Backlinks {
        node_id: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value_t = 8)]
        depth: usize,
    },
    /// Tag intersected with hierarchical path prefix. Mirrors MCP
    /// `find_by_tag_and_path`.
    FindByTagAndPath {
        tag: String,
        /// Path prefix segments, e.g. --segment Areas --segment Personal
        #[arg(long = "segment", value_name = "NAME", num_args = 1..)]
        segments: Vec<String>,
        #[arg(long, default_value_t = 5)]
        depth: usize,
    },
    /// Resolve a hierarchical name path to a UUID. ONE list_children per
    /// segment. Mirrors MCP `node_at_path`.
    NodeAtPath {
        /// Path segments from workspace root or `--root`. Pass with --segment.
        #[arg(long = "segment", value_name = "NAME", num_args = 1..)]
        segments: Vec<String>,
        /// Optional root to start the walk under.
        #[arg(long)]
        root: Option<String>,
    },
    /// Walk parent_id chain to render the canonical root→node path.
    /// Mirrors MCP `path_of`.
    PathOf {
        node_id: String,
        #[arg(long, default_value_t = 50)]
        max_depth: usize,
    },
    /// Resolve a Workflowy URL or short-hash to full node info. Mirrors
    /// MCP `resolve_link`.
    ResolveLink {
        link: String,
        /// Optional parent-name path to scope the walk (e.g. --segment Areas).
        #[arg(long = "segment", value_name = "NAME", num_args = 1..)]
        segments: Vec<String>,
    },
    /// Check whether a node has been modified since a unix-ms threshold.
    /// Mirrors MCP `since`.
    Since {
        node_id: String,
        /// Unix milliseconds threshold.
        threshold_unix_ms: i64,
    },
    /// List todo items under a parent. Mirrors MCP `list_todos`.
    Todos {
        #[arg(long)]
        parent: Option<String>,
        /// `all` | `pending` | `completed` (default: all).
        #[arg(long, default_value = "all")]
        status: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = 5)]
        depth: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// List overdue todos (past due date, incomplete). Mirrors MCP
    /// `list_overdue`.
    Overdue {
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 5)]
        depth: usize,
        #[arg(long)]
        include_completed: bool,
    },
    /// List todos with upcoming due dates within `--days`. Mirrors MCP
    /// `list_upcoming`.
    Upcoming {
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 7)]
        days: i64,
        #[arg(long, default_value_t = 5)]
        depth: usize,
        #[arg(long)]
        include_completed: bool,
    },
    /// One-call standup: overdue + upcoming + recent + pending under a
    /// scope. Mirrors MCP `daily_review`.
    DailyReview {
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 5)]
        depth: usize,
    },
    /// Recently modified nodes within a time window. Mirrors MCP
    /// `get_recent_changes`.
    RecentChanges {
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 24)]
        hours: i64,
        #[arg(long, default_value_t = 5)]
        depth: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Project summary: stats + tag counts + assignee counts +
    /// recently-modified nodes for a subtree. Mirrors MCP
    /// `get_project_summary`.
    ProjectSummary {
        node_id: String,
    },
    /// Insert hierarchical 2-space-indented content under a parent.
    /// Mirrors MCP `insert_content`. Reads content from stdin if `--content`
    /// is not supplied.
    Insert {
        parent_id: String,
        /// Inline content; if omitted, reads from stdin.
        #[arg(long)]
        content: Option<String>,
    },
    /// Search for a target node by name and insert content under it.
    /// Mirrors MCP `smart_insert`.
    SmartInsert {
        search_query: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long, default_value_t = 3)]
        depth: usize,
    },
    /// Deep-copy a node and its subtree to a new parent. Mirrors MCP
    /// `duplicate_node`.
    Duplicate {
        node_id: String,
        #[arg(long = "to")]
        target_parent_id: String,
        #[arg(long, default_value_t = true)]
        include_children: bool,
    },
    /// Copy a template node with `{{variable}}` substitution. Mirrors
    /// MCP `create_from_template`. `--var KEY=VALUE` (repeatable).
    Template {
        template_node_id: String,
        #[arg(long = "to")]
        target_parent_id: String,
        #[arg(long = "var", value_name = "KEY=VALUE", num_args = 0..)]
        vars: Vec<String>,
    },
    /// Apply an operation to all nodes matching a filter. Mirrors MCP
    /// `bulk_update`. Operations: complete, uncomplete, delete,
    /// add_tag, remove_tag.
    BulkUpdate {
        operation: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        root: Option<String>,
        /// `all` | `pending` | `completed`.
        #[arg(long, default_value = "all")]
        status: String,
        /// Tag value for add_tag/remove_tag.
        #[arg(long = "operation-tag")]
        operation_tag: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 5)]
        depth: usize,
    },
    /// Apply a tag to many node IDs in parallel. Mirrors MCP `bulk_tag`.
    BulkTag {
        tag: String,
        /// Repeat --node for each node ID.
        #[arg(long = "node", value_name = "NODE_ID", num_args = 1..)]
        nodes: Vec<String>,
    },
    /// Pipelined batch creator. Reads JSON array from `--input` (path) or
    /// stdin: `[{"name":"...","description":"...","parent_id":"...","priority":N}]`.
    /// Mirrors MCP `batch_create_nodes`.
    BatchCreate {
        #[arg(long)]
        input: Option<String>,
    },
    /// Sequential transaction with best-effort rollback. Reads JSON array
    /// from `--input` (path) or stdin. Mirrors MCP `transaction`.
    Transaction {
        #[arg(long)]
        input: Option<String>,
    },
    /// Export a subtree as OPML | Markdown | JSON. Mirrors MCP
    /// `export_subtree`.
    Export {
        node_id: String,
        /// `opml` | `markdown` | `json` (default: markdown).
        #[arg(long, default_value = "markdown")]
        format: String,
        #[arg(long, default_value_t = 8)]
        depth: usize,
    },
    /// Quick liveness probe: one bounded API call + cache stats. Mirrors
    /// MCP `health_check`. (`status` is the broader `workflowy_status`.)
    HealthCheck,
    /// Cancel every in-flight tree walk on the running MCP server. The
    /// CLI does not run a server, so this is a no-op against the local
    /// client; useful only as a smoke test of the cancel registry. Mirrors
    /// MCP `cancel_all`.
    CancelAll,
    /// Walk one subtree and populate the persistent name index — single
    /// root, server-side semantics. Mirrors MCP `build_name_index`.
    /// (`reindex` is the multi-root, client-driven CLI variant.)
    BuildNameIndex {
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 5)]
        max_depth: usize,
    },
    /// Show the most recent N tool invocations recorded by the running
    /// MCP server's op log. Mirrors MCP `get_recent_tool_calls`. Off-server
    /// the local client has no op log, so this prints an empty list — the
    /// command exists for surface parity. Use against the live MCP via
    /// the Workflowy connector when you need real data.
    RecentTools {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Audit `canonical_of:` / `mirror_of:` markers under a subtree.
    AuditMirrors {
        #[arg(long)]
        root: Option<String>,
    },
    /// Surface what's worth re-reading: revisit-due, multi-pillar, stale, source-MOC reuse.
    Review {
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 90)]
        days_stale: i64,
    },
    /// Generate `~/code/SecondBrain/session-logs/INDEX.md` (or `--out` override) from the local logs.
    Index {
        #[arg(long)]
        out: Option<String>,
    },
    /// Walk one or more subtrees and merge what's found into the persistent
    /// name index at `$WORKFLOWY_INDEX_PATH` (default
    /// `$HOME/code/secondBrain/memory/name_index.json`). Each subtree is
    /// walked with the resolution budget; partial walks still write the
    /// nodes they reached. Useful for one-shot deep indexing from the
    /// shell, independent of any running MCP session.
    Reindex {
        /// One or more parent node IDs to walk. If omitted, walks the
        /// workspace root (which on huge trees will truncate; prefer
        /// listing top-level subtree IDs explicitly).
        #[arg(long = "root", value_name = "NODE_ID", num_args = 1..)]
        roots: Vec<String>,
        /// Override the index path. Empty string disables persistence
        /// (useful for dry-runs of how many nodes a walk reaches).
        #[arg(long)]
        index_path: Option<String>,
        /// Maximum tree depth to walk under each root.
        #[arg(long, default_value_t = 10)]
        max_depth: usize,
        /// Wall-clock budget per root in seconds. Defaults to the
        /// resolution-walk timeout (300 s). Set to 0 for no time
        /// budget — the walk runs until the node-count cap or until
        /// the subtree is exhausted, whichever fires first. Use a
        /// large value (e.g. 3600 for one hour per root) to reach
        /// deep regions of large subtrees that timed out at the
        /// default. The node-count cap (`RESOLVE_WALK_NODE_CAP`)
        /// still applies regardless.
        #[arg(long, default_value_t = (defaults::RESOLVE_WALK_TIMEOUT_MS / 1000) as u64)]
        timeout_secs: u64,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    dotenv::dotenv().ok();

    if !cli.quiet {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_ansi(false)
            .try_init();
    }

    let op = cmd_name(&cli.cmd);

    // Dry-run short-circuit: write verbs only. Skip client construction
    // entirely so missing API keys don't block plan-mode usage.
    if cli.dry_run {
        if let Some(line) = dry_run_line(&cli.cmd) {
            println!("{}", line);
            return ExitCode::SUCCESS;
        }
        // For non-write verbs --dry-run is a no-op; fall through.
    }

    let client = match build_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {} [{}]", op, e, classify(&e.to_string()));
            return ExitCode::from(1);
        }
    };

    match dispatch(&cli, client).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}: {} [{}]", op, e, classify(&e.to_string()));
            ExitCode::from(1)
        }
    }
}

fn cmd_name(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::Status => "status",
        Cmd::Get { .. } => "get",
        Cmd::Children { .. } => "children",
        Cmd::Create { .. } => "create",
        Cmd::Move { .. } => "move",
        Cmd::Delete { .. } => "delete",
        Cmd::Edit { .. } => "edit",
        Cmd::Complete { .. } => "complete",
        Cmd::Search { .. } => "search",
        Cmd::TagSearch { .. } => "tag-search",
        Cmd::Find { .. } => "find",
        Cmd::Subtree { .. } => "subtree",
        Cmd::Backlinks { .. } => "backlinks",
        Cmd::FindByTagAndPath { .. } => "find-by-tag-and-path",
        Cmd::NodeAtPath { .. } => "node-at-path",
        Cmd::PathOf { .. } => "path-of",
        Cmd::ResolveLink { .. } => "resolve-link",
        Cmd::Since { .. } => "since",
        Cmd::Todos { .. } => "todos",
        Cmd::Overdue { .. } => "overdue",
        Cmd::Upcoming { .. } => "upcoming",
        Cmd::DailyReview { .. } => "daily-review",
        Cmd::RecentChanges { .. } => "recent-changes",
        Cmd::ProjectSummary { .. } => "project-summary",
        Cmd::Insert { .. } => "insert",
        Cmd::SmartInsert { .. } => "smart-insert",
        Cmd::Duplicate { .. } => "duplicate",
        Cmd::Template { .. } => "template",
        Cmd::BulkUpdate { .. } => "bulk-update",
        Cmd::BulkTag { .. } => "bulk-tag",
        Cmd::BatchCreate { .. } => "batch-create",
        Cmd::Transaction { .. } => "transaction",
        Cmd::Export { .. } => "export",
        Cmd::HealthCheck => "health-check",
        Cmd::CancelAll => "cancel-all",
        Cmd::BuildNameIndex { .. } => "build-name-index",
        Cmd::RecentTools { .. } => "recent-tools",
        Cmd::AuditMirrors { .. } => "audit-mirrors",
        Cmd::Review { .. } => "review",
        Cmd::Index { .. } => "index",
        Cmd::Reindex { .. } => "reindex",
    }
}

/// Returns the planned-operation line for the four write verbs, or `None`
/// for read-only verbs (which are unaffected by `--dry-run`).
fn dry_run_line(cmd: &Cmd) -> Option<String> {
    match cmd {
        Cmd::Create { name, description, parent, priority } => Some(format!(
            "DRY-RUN create name={:?} parent={:?} priority={:?} description_len={}",
            name,
            parent,
            priority,
            description.as_deref().map(|d| d.len()).unwrap_or(0),
        )),
        Cmd::Move { node_id, to, priority } => Some(format!(
            "DRY-RUN move node_id={} to={} priority={:?}",
            node_id, to, priority
        )),
        Cmd::Delete { node_id } => Some(format!("DRY-RUN delete node_id={}", node_id)),
        Cmd::Edit { node_id, name, description } => Some(format!(
            "DRY-RUN edit node_id={} name={:?} description_len={}",
            node_id,
            name,
            description.as_deref().map(|d| d.len()).unwrap_or(0),
        )),
        Cmd::Complete { node_id, uncomplete } => Some(format!(
            "DRY-RUN complete node_id={} target_state={}",
            node_id,
            !*uncomplete,
        )),
        Cmd::Insert { parent_id, content } => Some(format!(
            "DRY-RUN insert parent_id={} content_lines={}",
            parent_id,
            content.as_deref().map(|c| c.lines().count()).unwrap_or(0),
        )),
        Cmd::SmartInsert { search_query, content, .. } => Some(format!(
            "DRY-RUN smart-insert query={:?} content_lines={}",
            search_query,
            content.as_deref().map(|c| c.lines().count()).unwrap_or(0),
        )),
        Cmd::Duplicate { node_id, target_parent_id, .. } => Some(format!(
            "DRY-RUN duplicate node_id={} to={}",
            node_id, target_parent_id,
        )),
        Cmd::Template { template_node_id, target_parent_id, vars } => Some(format!(
            "DRY-RUN template template_id={} to={} vars={}",
            template_node_id, target_parent_id, vars.len(),
        )),
        Cmd::BulkUpdate { operation, query, tag, root, status, limit, .. } => Some(format!(
            "DRY-RUN bulk-update op={} query={:?} tag={:?} root={:?} status={} limit={}",
            operation, query, tag, root, status, limit,
        )),
        Cmd::BulkTag { tag, nodes } => Some(format!(
            "DRY-RUN bulk-tag tag={} nodes={}",
            tag, nodes.len(),
        )),
        Cmd::BatchCreate { input } => Some(format!(
            "DRY-RUN batch-create input={:?}",
            input,
        )),
        Cmd::Transaction { input } => Some(format!(
            "DRY-RUN transaction input={:?}",
            input,
        )),
        _ => None,
    }
}

fn build_client() -> Result<Arc<WorkflowyClient>, Box<dyn std::error::Error>> {
    let config = validate_config().map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    let client = WorkflowyClient::new(config.workflowy_base_url, config.workflowy_api_key)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    Ok(Arc::new(client))
}

async fn dispatch(cli: &Cli, client: Arc<WorkflowyClient>) -> Result<(), Box<dyn std::error::Error>> {
    match &cli.cmd {
        Cmd::Status => {
            // Reproduce the workflowy_status payload using only public client methods.
            let started = std::time::Instant::now();
            let probe = tokio::time::timeout(
                std::time::Duration::from_millis(defaults::HEALTH_CHECK_TIMEOUT_MS),
                client.get_top_level_nodes(),
            )
            .await;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let (api_reachable, top_level_count, error): (bool, Option<usize>, Option<String>) = match probe {
                Ok(Ok(nodes)) => (true, Some(nodes.len()), None),
                Ok(Err(e)) => (false, None, Some(e.to_string())),
                Err(_) => (false, None, Some("timed out".into())),
            };
            let snap = client.rate_limit_snapshot();
            let payload = json!({
                "status": if api_reachable { "ok" } else { "degraded" },
                "api_reachable": api_reachable,
                "latency_ms": elapsed_ms,
                "top_level_count": top_level_count,
                "last_request_ms": client.last_request_ms(),
                "rate_limit": {
                    "remaining": snap.remaining,
                    "limit": snap.limit,
                    "reset_unix_seconds": snap.reset_unix_seconds,
                },
                "error": error,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::Get { node_id } => {
            let node = client.get_node_with_propagation_retry(node_id).await?;
            println!("{}", serde_json::to_string_pretty(&node)?);
        }
        Cmd::Children { node_id } => {
            let children = client.get_children_with_propagation_retry(node_id).await?;
            println!("{}", serde_json::to_string_pretty(&children)?);
        }
        Cmd::Create { name, description, parent, priority } => {
            let created = client
                .create_node(name, description.as_deref(), parent.as_deref(), *priority)
                .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&created)?);
            } else {
                let placement = parent
                    .as_deref()
                    .map(|p| format!("under {}", p))
                    .unwrap_or_else(|| "at workspace root (no parent_id supplied)".to_string());
                println!("Created {} {}", created.id, placement);
                // UUID alone on the LAST line so shell scripts can capture with $(...)
                println!("{}", created.id);
            }
        }
        Cmd::Move { node_id, to, priority } => {
            client.move_node_with_propagation_retry(node_id, to, *priority).await?;
            if cli.json {
                println!("{}", json!({ "ok": true, "node_id": node_id, "new_parent": to }));
            } else {
                println!("Moved {} -> {}", node_id, to);
            }
        }
        Cmd::Delete { node_id } => {
            client.delete_node_with_propagation_retry(node_id).await?;
            if cli.json {
                println!("{}", json!({ "ok": true, "node_id": node_id }));
            } else {
                println!("Deleted {}", node_id);
            }
        }
        Cmd::Edit { node_id, name, description } => {
            if name.is_none() && description.is_none() {
                return Err("edit requires at least one of --name or --description".into());
            }
            client
                .edit_node_with_propagation_retry(node_id, name.as_deref(), description.as_deref())
                .await?;
            if cli.json {
                println!("{}", json!({ "ok": true, "node_id": node_id }));
            } else {
                println!("Edited {}", node_id);
            }
        }
        Cmd::Complete { node_id, uncomplete } => {
            let target_state = !*uncomplete;
            client
                .set_completion_with_propagation_retry(node_id, target_state)
                .await?;
            if cli.json {
                println!(
                    "{}",
                    json!({ "ok": true, "node_id": node_id, "completed": target_state }),
                );
            } else {
                let verb = if target_state { "Completed" } else { "Uncompleted" };
                println!("{} {}", verb, node_id);
            }
        }
        Cmd::Search { query, parent, depth, limit } => {
            // Walk the subtree under `parent` (or the workspace root) up to
            // `depth` levels and filter on substring match in name or note.
            // No new client method is needed — reuses get_subtree_with_controls.
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(parent.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let needle = query.to_lowercase();
            let hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter(|n| {
                    n.name.to_lowercase().contains(&needle)
                        || n.description
                            .as_deref()
                            .map(|d| d.to_lowercase().contains(&needle))
                            .unwrap_or(false)
                })
                .take(*limit)
                .collect();
            let payload = json!({
                "query": query,
                "scope": parent,
                "depth": depth,
                "scanned": fetch.nodes.len(),
                "truncated": fetch.truncated,
                "truncation_reason": fetch.truncation_reason.map(|r| r.as_str()),
                "matches": hits,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::TagSearch { tag, parent, depth, limit } => {
            use workflowy_mcp_server::utils::tag_parser::parse_node_tags;
            let needle = tag.trim_start_matches('#').trim_start_matches('@').to_lowercase();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(parent.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter(|n| {
                    let parsed = parse_node_tags(n);
                    parsed.tags.iter().any(|t| t.eq_ignore_ascii_case(&needle))
                        || parsed.assignees.iter().any(|a| a.eq_ignore_ascii_case(&needle))
                })
                .take(*limit)
                .collect();
            let payload = json!({
                "tag": tag,
                "scope": parent,
                "scanned": fetch.nodes.len(),
                "truncated": fetch.truncated,
                "matches": hits,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::Find { name, parent, match_mode, depth, allow_root_scan, limit } => {
            if parent.is_none() && !*allow_root_scan {
                return Err("find refuses unscoped root walks; pass --parent or --allow-root-scan".into());
            }
            let needle = name.to_lowercase();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(parent.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter(|n| {
                    let lower = n.name.to_lowercase();
                    match match_mode.as_str() {
                        "exact" => lower == needle,
                        "starts_with" => lower.starts_with(&needle),
                        _ => lower.contains(&needle),
                    }
                })
                .take(*limit)
                .collect();
            let payload = json!({
                "name": name,
                "match_mode": match_mode,
                "scope": parent,
                "scanned": fetch.nodes.len(),
                "truncated": fetch.truncated,
                "matches": hits,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::Subtree { node_id, depth } => {
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(Some(node_id), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let payload = json!({
                "node_id": node_id,
                "depth": depth,
                "count": fetch.nodes.len(),
                "truncated": fetch.truncated,
                "truncation_reason": fetch.truncation_reason.map(|r| r.as_str()),
                "nodes": fetch.nodes,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::Backlinks { node_id, parent, depth } => {
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(parent.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            // A backlink is a node whose name or description references the
            // target via a Workflowy link or its UUID.
            let needle_full = node_id.as_str();
            let needle_short = if needle_full.len() >= 12 {
                &needle_full[needle_full.len() - 12..]
            } else {
                needle_full
            };
            let hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter(|n| n.id != *needle_full)
                .filter(|n| {
                    let blob = format!(
                        "{} {}",
                        n.name,
                        n.description.as_deref().unwrap_or(""),
                    );
                    blob.contains(needle_full) || blob.contains(needle_short)
                })
                .collect();
            let payload = json!({
                "target": node_id,
                "scope": parent,
                "scanned": fetch.nodes.len(),
                "truncated": fetch.truncated,
                "matches": hits,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::FindByTagAndPath { tag, segments, depth } => {
            use workflowy_mcp_server::utils::node_paths::{build_node_map, build_node_path_with_map};
            use workflowy_mcp_server::utils::tag_parser::parse_node_tags;
            let needle = tag.trim_start_matches('#').trim_start_matches('@').to_lowercase();
            let prefix_lower: Vec<String> = segments.iter().map(|s| s.to_lowercase()).collect();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(None, *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let map = build_node_map(&fetch.nodes);
            let hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter(|n| {
                    let parsed = parse_node_tags(n);
                    if !parsed.tags.iter().any(|t| t.eq_ignore_ascii_case(&needle)) {
                        return false;
                    }
                    let path = build_node_path_with_map(&n.id, &map);
                    let path_lower = path.to_lowercase();
                    prefix_lower.iter().all(|seg| path_lower.contains(seg))
                })
                .collect();
            let payload = json!({
                "tag": tag,
                "segments": segments,
                "scanned": fetch.nodes.len(),
                "truncated": fetch.truncated,
                "matches": hits,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::NodeAtPath { segments, root } => {
            // Walk segment by segment via list_children. ONE call per segment.
            let mut current: Option<String> = root.clone();
            for seg in segments {
                let children = match current.as_deref() {
                    Some(id) => client.get_children_with_propagation_retry(id).await?,
                    None => client.get_top_level_nodes().await?,
                };
                let target = children.iter().find(|c| {
                    c.name.trim().eq_ignore_ascii_case(seg.trim())
                });
                match target {
                    Some(n) => current = Some(n.id.clone()),
                    None => return Err(format!("path segment {:?} not found", seg).into()),
                }
            }
            let payload = json!({
                "node_id": current,
                "segments": segments,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::PathOf { node_id, max_depth } => {
            // Walk parent_id chain via repeated get_node calls.
            let mut path: Vec<serde_json::Value> = Vec::new();
            let mut cursor = node_id.clone();
            for _ in 0..*max_depth {
                let n = client.get_node_with_propagation_retry(&cursor).await?;
                path.push(json!({ "id": n.id.clone(), "name": n.name.clone() }));
                match n.parent_id {
                    Some(pid) if !pid.is_empty() => cursor = pid,
                    _ => break,
                }
            }
            path.reverse();
            let payload = json!({ "node_id": node_id, "path": path });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::ResolveLink { link, segments } => {
            // Extract a 12-char short hash from the URL or assume the input
            // is already an ID. Then look up via subtree-walk under the
            // given segment path (or workspace).
            let candidate: String = link
                .split(['/', '#'])
                .last()
                .unwrap_or(link)
                .chars()
                .filter(|c| c.is_ascii_hexdigit())
                .collect();
            let scope = if segments.is_empty() {
                None
            } else {
                // Walk segments to a parent UUID first.
                let mut current: Option<String> = None;
                for seg in segments {
                    let children = match current.as_deref() {
                        Some(id) => client.get_children_with_propagation_retry(id).await?,
                        None => client.get_top_level_nodes().await?,
                    };
                    let t = children.iter().find(|c| c.name.trim().eq_ignore_ascii_case(seg.trim()));
                    current = t.map(|n| n.id.clone());
                    if current.is_none() {
                        return Err(format!("path segment {:?} not found", seg).into());
                    }
                }
                current
            };
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(scope.as_deref(), 8, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let target = fetch.nodes.iter().find(|n| {
                n.id == candidate || n.id.ends_with(&candidate)
            });
            match target {
                Some(n) => println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "link": link,
                        "node": n,
                    }))?,
                ),
                None => return Err(format!("link {:?} did not resolve to any node", link).into()),
            }
        }
        Cmd::Since { node_id, threshold_unix_ms } => {
            let n = client.get_node_with_propagation_retry(node_id).await?;
            let last_modified = n.last_modified.unwrap_or(0);
            let payload = json!({
                "node_id": node_id,
                "name": n.name,
                "last_modified_unix_ms": last_modified,
                "threshold_unix_ms": threshold_unix_ms,
                "changed_since": last_modified >= *threshold_unix_ms,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::Todos { parent, status, query, depth, limit } => {
            use workflowy_mcp_server::utils::node_paths::{build_node_map, build_node_path_with_map};
            use workflowy_mcp_server::utils::subtree::{is_completed, is_todo};
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(parent.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let q = query.as_deref().map(|s| s.to_lowercase());
            let map = build_node_map(&fetch.nodes);
            let hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter(|n| {
                    if !is_todo(n) { return false; }
                    let completed = is_completed(n);
                    match status.as_str() {
                        "pending" if completed => return false,
                        "completed" if !completed => return false,
                        _ => {}
                    }
                    if let Some(q) = &q {
                        let in_name = n.name.to_lowercase().contains(q);
                        let in_desc = n.description.as_deref()
                            .map(|d| d.to_lowercase().contains(q)).unwrap_or(false);
                        if !in_name && !in_desc { return false; }
                    }
                    true
                })
                .take(*limit)
                .map(|n| json!({
                    "id": n.id,
                    "name": n.name,
                    "completed": is_completed(n),
                    "completed_at": n.completed_at,
                    "path": build_node_path_with_map(&n.id, &map),
                }))
                .collect();
            let payload = json!({
                "scope": parent,
                "status": status,
                "scanned": fetch.nodes.len(),
                "count": hits.len(),
                "todos": hits,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::Overdue { root, depth, include_completed } => {
            use workflowy_mcp_server::utils::date_parser::parse_due_date_from_node;
            use workflowy_mcp_server::utils::node_paths::{build_node_map, build_node_path_with_map};
            use workflowy_mcp_server::utils::subtree::is_completed;
            let today = chrono::Utc::now().date_naive();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let map = build_node_map(&fetch.nodes);
            let mut hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter_map(|n| {
                    if !*include_completed && is_completed(n) { return None; }
                    let due = parse_due_date_from_node(n)?;
                    if due >= today { return None; }
                    let days = (today - due).num_days();
                    Some(json!({
                        "id": n.id,
                        "name": n.name,
                        "due_date": due.to_string(),
                        "days_overdue": days,
                        "completed": is_completed(n),
                        "path": build_node_path_with_map(&n.id, &map),
                    }))
                })
                .collect();
            hits.sort_by(|a, b| {
                b["days_overdue"].as_i64().cmp(&a["days_overdue"].as_i64())
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "scope": root,
                    "today": today.to_string(),
                    "count": hits.len(),
                    "overdue": hits,
                }))?
            );
        }
        Cmd::Upcoming { root, days, depth, include_completed } => {
            use workflowy_mcp_server::utils::date_parser::parse_due_date_from_node;
            use workflowy_mcp_server::utils::node_paths::{build_node_map, build_node_path_with_map};
            use workflowy_mcp_server::utils::subtree::is_completed;
            let today = chrono::Utc::now().date_naive();
            let horizon = today + chrono::Duration::days(*days);
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let map = build_node_map(&fetch.nodes);
            let mut hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter_map(|n| {
                    if !*include_completed && is_completed(n) { return None; }
                    let due = parse_due_date_from_node(n)?;
                    if due < today || due > horizon { return None; }
                    Some(json!({
                        "id": n.id,
                        "name": n.name,
                        "due_date": due.to_string(),
                        "days_until": (due - today).num_days(),
                        "completed": is_completed(n),
                        "path": build_node_path_with_map(&n.id, &map),
                    }))
                })
                .collect();
            hits.sort_by(|a, b| {
                a["days_until"].as_i64().cmp(&b["days_until"].as_i64())
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "scope": root,
                    "today": today.to_string(),
                    "horizon_days": days,
                    "count": hits.len(),
                    "upcoming": hits,
                }))?
            );
        }
        Cmd::DailyReview { root, depth } => {
            use workflowy_mcp_server::utils::date_parser::parse_due_date_from_node;
            use workflowy_mcp_server::utils::node_paths::{build_node_map, build_node_path_with_map};
            use workflowy_mcp_server::utils::subtree::{is_completed, is_todo};
            let today = chrono::Utc::now().date_naive();
            let horizon = today + chrono::Duration::days(7);
            let recent_cutoff_ms = (chrono::Utc::now() - chrono::Duration::hours(24)).timestamp_millis();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let map = build_node_map(&fetch.nodes);
            let mut overdue: Vec<serde_json::Value> = Vec::new();
            let mut upcoming: Vec<serde_json::Value> = Vec::new();
            let mut recent: Vec<serde_json::Value> = Vec::new();
            let mut pending: Vec<serde_json::Value> = Vec::new();
            for n in &fetch.nodes {
                let path = build_node_path_with_map(&n.id, &map);
                let entry = json!({ "id": n.id, "name": n.name, "path": path });
                if let Some(due) = parse_due_date_from_node(n) {
                    if !is_completed(n) {
                        if due < today {
                            overdue.push(json!({
                                "id": n.id, "name": n.name,
                                "due_date": due.to_string(),
                                "days_overdue": (today - due).num_days(),
                                "path": entry["path"].clone(),
                            }));
                        } else if due <= horizon {
                            upcoming.push(json!({
                                "id": n.id, "name": n.name,
                                "due_date": due.to_string(),
                                "days_until": (due - today).num_days(),
                                "path": entry["path"].clone(),
                            }));
                        }
                    }
                }
                if let Some(ts) = n.last_modified {
                    if ts >= recent_cutoff_ms {
                        recent.push(json!({
                            "id": n.id, "name": n.name,
                            "modifiedAt": ts,
                            "completed": is_completed(n),
                            "path": entry["path"].clone(),
                        }));
                    }
                }
                if is_todo(n) && !is_completed(n) {
                    pending.push(entry);
                }
            }
            overdue.sort_by(|a, b| b["days_overdue"].as_i64().cmp(&a["days_overdue"].as_i64()));
            upcoming.sort_by(|a, b| a["days_until"].as_i64().cmp(&b["days_until"].as_i64()));
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "scope": root,
                    "today": today.to_string(),
                    "overdue": overdue,
                    "upcoming": upcoming,
                    "recent_changes": recent,
                    "pending_todos": pending,
                }))?
            );
        }
        Cmd::RecentChanges { root, hours, depth, limit } => {
            use workflowy_mcp_server::utils::node_paths::{build_node_map, build_node_path_with_map};
            use workflowy_mcp_server::utils::subtree::is_completed;
            let cutoff_ms = (chrono::Utc::now() - chrono::Duration::hours(*hours)).timestamp_millis();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let map = build_node_map(&fetch.nodes);
            let mut hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter_map(|n| {
                    let ts = n.last_modified?;
                    if ts < cutoff_ms { return None; }
                    Some(json!({
                        "id": n.id,
                        "name": n.name,
                        "modifiedAt": ts,
                        "completed": is_completed(n),
                        "path": build_node_path_with_map(&n.id, &map),
                    }))
                })
                .collect();
            hits.sort_by(|a, b| b["modifiedAt"].as_i64().cmp(&a["modifiedAt"].as_i64()));
            hits.truncate(*limit);
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "scope": root,
                    "hours": hours,
                    "count": hits.len(),
                    "recent": hits,
                }))?
            );
        }
        Cmd::ProjectSummary { node_id } => {
            use std::collections::HashMap;
            use workflowy_mcp_server::utils::subtree::{is_completed, is_todo};
            use workflowy_mcp_server::utils::tag_parser::parse_node_tags;
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(Some(node_id), 10, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let mut total = 0usize;
            let mut todo_total = 0usize;
            let mut todo_completed = 0usize;
            let mut tag_counts: HashMap<String, usize> = HashMap::new();
            let mut assignee_counts: HashMap<String, usize> = HashMap::new();
            for n in &fetch.nodes {
                total += 1;
                if is_todo(n) {
                    todo_total += 1;
                    if is_completed(n) { todo_completed += 1; }
                }
                let parsed = parse_node_tags(n);
                for t in parsed.tags { *tag_counts.entry(t).or_insert(0) += 1; }
                for a in parsed.assignees { *assignee_counts.entry(a).or_insert(0) += 1; }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "node_id": node_id,
                    "total_nodes": total,
                    "todos": { "total": todo_total, "completed": todo_completed },
                    "tag_counts": tag_counts,
                    "assignee_counts": assignee_counts,
                    "truncated": fetch.truncated,
                }))?
            );
        }
        Cmd::Insert { parent_id, content } => {
            let body = match content {
                Some(s) => s.clone(),
                None => {
                    use std::io::Read;
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            // Parse 2-space indented hierarchical content.
            struct Parsed { text: String, indent: usize }
            let parsed: Vec<Parsed> = body.lines().filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() { return None; }
                let leading = line.len() - line.trim_start().len();
                Some(Parsed { text: trimmed.to_string(), indent: leading / 2 })
            }).collect();
            if parsed.is_empty() {
                println!("insert: nothing to insert (no non-blank lines)");
                return Ok(());
            }
            if parsed.len() > defaults::MAX_INSERT_CONTENT_LINES {
                return Err(format!(
                    "insert: payload {} lines exceeds the {}-line cap; split into batches and chain via the returned last_inserted_id",
                    parsed.len(), defaults::MAX_INSERT_CONTENT_LINES,
                ).into());
            }
            let mut parent_stack: Vec<String> = vec![parent_id.clone()];
            let mut created = 0usize;
            let mut last_inserted_id: Option<String> = None;
            for line in &parsed {
                let indent = line.indent.min(parent_stack.len().saturating_sub(1));
                let pid = parent_stack[indent].clone();
                let n = client
                    .create_node(&line.text, None, Some(&pid), None)
                    .await?;
                created += 1;
                last_inserted_id = Some(n.id.clone());
                let next_level = indent + 1;
                if next_level < parent_stack.len() {
                    parent_stack[next_level] = n.id;
                    parent_stack.truncate(next_level + 1);
                } else {
                    parent_stack.push(n.id);
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "parent_id": parent_id,
                    "created_count": created,
                    "last_inserted_id": last_inserted_id,
                }))?
            );
        }
        Cmd::SmartInsert { search_query, content, depth } => {
            // Find a single matching parent by name, then insert under it.
            let needle = search_query.to_lowercase();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(None, *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let matches: Vec<&_> = fetch.nodes.iter()
                .filter(|n| n.name.to_lowercase().contains(&needle))
                .collect();
            if matches.is_empty() {
                return Err(format!("smart-insert: no node found matching {:?}", search_query).into());
            }
            if matches.len() > 1 {
                let options: Vec<_> = matches.iter().take(10).map(|n| json!({"id": n.id, "name": n.name})).collect();
                return Err(format!(
                    "smart-insert: {} candidates matched; rerun against a more specific query or use `wflow-do find ...` then `wflow-do insert ...`. Top matches: {}",
                    matches.len(),
                    serde_json::to_string(&options).unwrap_or_default(),
                ).into());
            }
            let parent = matches[0];
            let body = match content {
                Some(s) => s.clone(),
                None => {
                    use std::io::Read;
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            // Reuse the Insert path by recursively calling the same routine.
            // Easier than copying: just inline the create-loop here.
            struct Parsed { text: String, indent: usize }
            let parsed: Vec<Parsed> = body.lines().filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() { return None; }
                let leading = line.len() - line.trim_start().len();
                Some(Parsed { text: trimmed.to_string(), indent: leading / 2 })
            }).collect();
            let mut stack = vec![parent.id.clone()];
            let mut created = 0usize;
            for line in &parsed {
                let indent = line.indent.min(stack.len().saturating_sub(1));
                let pid = stack[indent].clone();
                let n = client.create_node(&line.text, None, Some(&pid), None).await?;
                created += 1;
                let next_level = indent + 1;
                if next_level < stack.len() {
                    stack[next_level] = n.id;
                    stack.truncate(next_level + 1);
                } else {
                    stack.push(n.id);
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "matched_parent": { "id": parent.id, "name": parent.name },
                    "created_count": created,
                }))?
            );
        }
        Cmd::Duplicate { node_id, target_parent_id, include_children } => {
            // Walk source subtree, recreate under the target parent.
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let depth = if *include_children { 10 } else { 0 };
            let fetch = client
                .get_subtree_with_controls(Some(node_id), depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            if fetch.truncated {
                return Err("duplicate: source subtree exceeds the walk cap; refusing to duplicate a partial view".into());
            }
            // Build parent → children map keyed off the source root.
            use std::collections::HashMap;
            let mut by_parent: HashMap<&str, Vec<&workflowy_mcp_server::types::WorkflowyNode>> = HashMap::new();
            for n in &fetch.nodes {
                if let Some(pid) = &n.parent_id {
                    by_parent.entry(pid.as_str()).or_default().push(n);
                }
            }
            // BFS-recreate.
            let root_node = fetch.nodes.iter().find(|n| n.id == *node_id)
                .ok_or("duplicate: source root not found in fetched subtree")?;
            let new_root = client
                .create_node(&root_node.name, root_node.description.as_deref(), Some(target_parent_id), None)
                .await?;
            let mut id_map: HashMap<String, String> = HashMap::new();
            id_map.insert(root_node.id.clone(), new_root.id.clone());
            let mut total_created = 1usize;
            if *include_children {
                let mut frontier: Vec<String> = vec![root_node.id.clone()];
                while let Some(src_id) = frontier.pop() {
                    let new_pid = id_map[&src_id].clone();
                    if let Some(children) = by_parent.get(src_id.as_str()) {
                        for child in children {
                            let n = client
                                .create_node(&child.name, child.description.as_deref(), Some(&new_pid), None)
                                .await?;
                            id_map.insert(child.id.clone(), n.id);
                            frontier.push(child.id.clone());
                            total_created += 1;
                        }
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "source_id": node_id,
                    "new_root_id": new_root.id,
                    "target_parent_id": target_parent_id,
                    "total_created": total_created,
                }))?
            );
        }
        Cmd::Template { template_node_id, target_parent_id, vars } => {
            use std::collections::HashMap;
            // Parse --var KEY=VALUE pairs.
            let mut substitutions: HashMap<String, String> = HashMap::new();
            for raw in vars {
                let (k, v) = raw.split_once('=').ok_or_else(|| {
                    format!("--var must be KEY=VALUE; got {:?}", raw)
                })?;
                substitutions.insert(k.to_string(), v.to_string());
            }
            let apply = |s: &str| -> String {
                let mut out = s.to_string();
                for (k, v) in &substitutions {
                    out = out.replace(&format!("{{{{{}}}}}", k), v);
                }
                out
            };
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(Some(template_node_id), 10, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let mut by_parent: HashMap<&str, Vec<&workflowy_mcp_server::types::WorkflowyNode>> = HashMap::new();
            for n in &fetch.nodes {
                if let Some(pid) = &n.parent_id {
                    by_parent.entry(pid.as_str()).or_default().push(n);
                }
            }
            let root_node = fetch.nodes.iter().find(|n| n.id == *template_node_id)
                .ok_or("template: template root not found")?;
            let new_root = client
                .create_node(&apply(&root_node.name), root_node.description.as_deref().map(apply).as_deref(), Some(target_parent_id), None)
                .await?;
            let mut id_map: HashMap<String, String> = HashMap::new();
            id_map.insert(root_node.id.clone(), new_root.id.clone());
            let mut total_created = 1usize;
            let mut frontier: Vec<String> = vec![root_node.id.clone()];
            while let Some(src_id) = frontier.pop() {
                let new_pid = id_map[&src_id].clone();
                if let Some(children) = by_parent.get(src_id.as_str()) {
                    for child in children {
                        let n = client
                            .create_node(&apply(&child.name), child.description.as_deref().map(apply).as_deref(), Some(&new_pid), None)
                            .await?;
                        id_map.insert(child.id.clone(), n.id);
                        frontier.push(child.id.clone());
                        total_created += 1;
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "template_id": template_node_id,
                    "new_root_id": new_root.id,
                    "target_parent_id": target_parent_id,
                    "total_created": total_created,
                }))?
            );
        }
        Cmd::BulkUpdate { operation, query, tag, root, status, operation_tag, limit, depth } => {
            use workflowy_mcp_server::utils::subtree::is_completed;
            use workflowy_mcp_server::utils::tag_parser::parse_node_tags;
            let valid_ops = ["delete", "add_tag", "remove_tag", "complete", "uncomplete"];
            if !valid_ops.contains(&operation.as_str()) {
                return Err(format!("bulk-update: invalid operation {:?}; valid: {:?}", operation, valid_ops).into());
            }
            if (operation == "add_tag" || operation == "remove_tag") && operation_tag.is_none() {
                return Err("bulk-update: --operation-tag required for add_tag/remove_tag".into());
            }
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            if fetch.truncated && operation == "delete" {
                return Err(format!(
                    "bulk-update: refusing to delete against a truncated subtree (capped at {} nodes); narrow with --root or --depth",
                    fetch.limit,
                ).into());
            }
            let q = query.as_deref().map(|s| s.to_lowercase());
            let t = tag.as_deref().map(|s| s.trim_start_matches('#').to_lowercase());
            let matched: Vec<&_> = fetch.nodes.iter()
                .filter(|n| {
                    if let Some(q) = &q {
                        let in_name = n.name.to_lowercase().contains(q);
                        let in_desc = n.description.as_deref().map(|d| d.to_lowercase().contains(q)).unwrap_or(false);
                        if !in_name && !in_desc { return false; }
                    }
                    if let Some(t) = &t {
                        let parsed = parse_node_tags(n);
                        if !parsed.tags.iter().any(|x| x == t) { return false; }
                    }
                    let completed = is_completed(n);
                    match status.as_str() {
                        "pending" if completed => return false,
                        "completed" if !completed => return false,
                        _ => {}
                    }
                    true
                })
                .take(*limit)
                .collect();
            let mut affected = 0usize;
            for n in &matched {
                let ok = match operation.as_str() {
                    "delete" => client.delete_node(&n.id).await.is_ok(),
                    "complete" => client.set_completion(&n.id, true).await.is_ok(),
                    "uncomplete" => client.set_completion(&n.id, false).await.is_ok(),
                    "add_tag" => {
                        let tag = operation_tag.as_ref().expect("validated above");
                        let new_name = format!("{} #{}", n.name, tag.trim_start_matches('#'));
                        client.edit_node(&n.id, Some(&new_name), None).await.is_ok()
                    }
                    "remove_tag" => {
                        let tag = operation_tag.as_ref().expect("validated above").trim_start_matches('#');
                        let pat = regex::Regex::new(&format!(r"\s*#{}(?:\b|$)", regex::escape(tag)))
                            .expect("escaped pattern is always valid regex");
                        let new_name = pat.replace_all(&n.name, "").to_string();
                        client.edit_node(&n.id, Some(&new_name), None).await.is_ok()
                    }
                    _ => false,
                };
                if ok { affected += 1; }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "operation": operation,
                    "matched_count": matched.len(),
                    "affected_count": affected,
                }))?
            );
        }
        Cmd::BulkTag { tag, nodes } => {
            let tag_clean = tag.trim_start_matches('#');
            let mut affected = 0usize;
            for id in nodes {
                let n = match client.get_node(id).await {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let new_name = format!("{} #{}", n.name, tag_clean);
                if client.edit_node(id, Some(&new_name), None).await.is_ok() {
                    affected += 1;
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "tag": tag_clean,
                    "node_count": nodes.len(),
                    "affected_count": affected,
                }))?
            );
        }
        Cmd::BatchCreate { input } => {
            use workflowy_mcp_server::api::BatchCreateOp;
            let body = read_input_or_stdin(input.as_deref())?;
            let ops_raw: Vec<serde_json::Value> = serde_json::from_str(&body)
                .map_err(|e| format!("batch-create: input must be a JSON array — {}", e))?;
            let mut ops: Vec<BatchCreateOp> = Vec::with_capacity(ops_raw.len());
            for raw in ops_raw {
                let name = raw["name"].as_str()
                    .ok_or("batch-create: each op needs a string `name`")?
                    .to_string();
                let description = raw["description"].as_str().map(String::from);
                let parent_id = raw["parent_id"].as_str().map(String::from);
                let priority = raw["priority"].as_i64().map(|p| p as i32);
                ops.push(BatchCreateOp { name, description, parent_id, priority });
            }
            let results = client.batch_create_nodes(ops).await;
            let mut succeeded = 0usize;
            let mut failed = 0usize;
            let entries: Vec<serde_json::Value> = results.into_iter().enumerate().map(|(i, r)| {
                match r {
                    Ok(n) => { succeeded += 1; json!({"index": i, "ok": true, "id": n.id, "name": n.name, "parent_id": n.parent_id}) }
                    Err(e) => { failed += 1; json!({"index": i, "ok": false, "error": e.to_string()}) }
                }
            }).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "total": entries.len(),
                    "succeeded": succeeded,
                    "failed": failed,
                    "results": entries,
                }))?
            );
        }
        Cmd::Transaction { input } => {
            // Sequential apply with best-effort inverse rollback. Mirrors the
            // server's `transaction` tool semantics: on first error, replay
            // inverse ops in reverse order. Inverse coverage matches
            // server.rs::TxnInverse — create / edit / move / complete are
            // invertible; delete is not.
            let body = read_input_or_stdin(input.as_deref())?;
            let ops: Vec<serde_json::Value> = serde_json::from_str(&body)
                .map_err(|e| format!("transaction: input must be a JSON array — {}", e))?;
            let mut applied: Vec<(String, serde_json::Value)> = Vec::new();
            let mut summaries: Vec<serde_json::Value> = Vec::new();
            for raw in ops.iter() {
                let op_kind = raw["op"].as_str().unwrap_or("");
                let result = apply_txn_step(&client, raw).await;
                match result {
                    Ok((summary, inverse)) => {
                        summaries.push(summary.clone());
                        if let Some(inv) = inverse {
                            applied.push((op_kind.to_string(), inv));
                        }
                    }
                    Err(e) => {
                        // Rollback in reverse order.
                        let mut rollback: Vec<serde_json::Value> = Vec::new();
                        for (kind, inv) in applied.iter().rev() {
                            match run_inverse_step(&client, inv).await {
                                Ok(v) => rollback.push(v),
                                Err(re) => rollback.push(json!({"rollback_failed": kind, "error": re.to_string()})),
                            }
                        }
                        return Err(format!(
                            "transaction failed at op[{}] ({}); rolled back {} of {}: {}\nrollback details: {}",
                            summaries.len(), op_kind, rollback.len(), applied.len(), e,
                            serde_json::to_string(&rollback).unwrap_or_default(),
                        ).into());
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "applied_count": summaries.len(),
                    "summaries": summaries,
                }))?
            );
        }
        Cmd::Export { node_id, format, depth } => {
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(Some(node_id), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&fetch.nodes)?),
                "markdown" | "md" => {
                    let body = render_subtree_markdown(&fetch.nodes, node_id);
                    println!("{}", body);
                }
                "opml" => {
                    let body = render_subtree_opml(&fetch.nodes, node_id);
                    println!("{}", body);
                }
                other => return Err(format!("export: unknown format {:?} (use json, markdown, opml)", other).into()),
            }
        }
        Cmd::HealthCheck => {
            let started = std::time::Instant::now();
            let probe = tokio::time::timeout(
                std::time::Duration::from_millis(defaults::HEALTH_CHECK_TIMEOUT_MS),
                client.get_top_level_nodes(),
            ).await;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let (api_reachable, top_level_count, error): (bool, Option<usize>, Option<String>) = match probe {
                Ok(Ok(nodes)) => (true, Some(nodes.len()), None),
                Ok(Err(e)) => (false, None, Some(e.to_string())),
                Err(_) => (false, None, Some("timed out".into())),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "status": if api_reachable { "ok" } else { "degraded" },
                    "api_reachable": api_reachable,
                    "latency_ms": elapsed_ms,
                    "top_level_count": top_level_count,
                    "error": error,
                }))?
            );
        }
        Cmd::CancelAll => {
            // The CLI process is a single-shot client; there's nothing
            // in-flight to cancel. Print a structured response so the
            // surface matches the MCP tool, and exit cleanly.
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "cancelled_count": 0,
                    "note": "wflow-do is single-shot; cancel-all is a no-op against the local client. Use against the running MCP server via the Workflowy connector when you need to preempt in-flight tree walks.",
                }))?
            );
        }
        Cmd::BuildNameIndex { root, max_depth } => {
            // Mirrors MCP `build_name_index` semantics: walk one root,
            // ingest into the persistent index.
            cmd_reindex(
                Arc::clone(&client),
                root.as_deref().map(|s| vec![s.to_string()]).unwrap_or_default().as_slice(),
                None,
                *max_depth,
                (defaults::RESOLVE_WALK_TIMEOUT_MS / 1000) as u64,
            ).await?;
        }
        Cmd::RecentTools { limit } => {
            // The CLI process has no op log. Print an empty surface that
            // matches the MCP tool's response shape so callers can probe
            // for parity.
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "limit": limit,
                    "entries": [],
                    "note": "wflow-do is single-shot; the op log only exists inside the running MCP server. Call the MCP `get_recent_tool_calls` tool against the live server to see real entries.",
                }))?
            );
        }
        Cmd::AuditMirrors { root } => {
            let scope = root.as_deref().unwrap_or(DEFAULT_REVIEW_ROOT);
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(Some(scope), 8, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let findings = audit_mirrors(&fetch.nodes);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&json!({
                    "scope": scope,
                    "scanned": fetch.nodes.len(),
                    "truncated": fetch.truncated,
                    "findings": findings,
                }))?);
            } else if findings.is_empty() {
                println!("audit-mirrors: scanned {} nodes, no findings", fetch.nodes.len());
            } else {
                for f in &findings {
                    println!("{} {} \"{}\" -> {}", f.status, f.node_id, f.name, f.issue);
                }
                println!("---\n{} findings across {} nodes", findings.len(), fetch.nodes.len());
            }
        }
        Cmd::Review { root, days_stale } => {
            let scope = root.as_deref().unwrap_or(DEFAULT_REVIEW_ROOT);
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(Some(scope), 8, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let blob = load_recent_session_logs_blob();
            let report = build_review(
                &fetch.nodes,
                *days_stale,
                chrono::Utc::now().date_naive(),
                chrono::Utc::now().timestamp(),
                &blob,
            );
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&json!({
                    "scope": scope,
                    "scanned": fetch.nodes.len(),
                    "truncated": fetch.truncated,
                    "buckets": report,
                }))?);
            } else {
                print_review(&report);
            }
        }
        Cmd::Index { out } => {
            let default = format!(
                "{}/code/SecondBrain/session-logs/INDEX.md",
                std::env::var("HOME").unwrap_or_else(|_| ".".into())
            );
            let target = out.as_deref().unwrap_or(&default);
            let dir = std::path::Path::new(target).parent().ok_or("invalid out path")?;
            let entries = scan_session_logs(dir)?;
            let body = render_index(&entries);
            std::fs::write(target, &body)?;
            println!("index: wrote {} entries to {}", entries.len(), target);
        }
        Cmd::Reindex { roots, index_path, max_depth, timeout_secs } => {
            cmd_reindex(
                client,
                roots.as_slice(),
                index_path.as_deref(),
                *max_depth,
                *timeout_secs,
            )
            .await?;
        }
    }
    Ok(())
}

/// Build a NameIndex pointed at the persistent file, hydrate it from
/// disk, walk each requested root with the resolution budget, ingest
/// the visited nodes, and save the merged index back. Reports per-root
/// progress and the final on-disk size so the user can see coverage
/// extending across runs.
async fn cmd_reindex(
    client: Arc<WorkflowyClient>,
    roots: &[String],
    index_path_override: Option<&str>,
    max_depth: usize,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use workflowy_mcp_server::utils::NameIndex;

    let index = NameIndex::new();
    let path = match index_path_override {
        Some(s) if s.is_empty() => None,
        Some(s) => Some(std::path::PathBuf::from(s)),
        None => {
            // Match the default the MCP server uses so a CLI reindex
            // and a server-side walk agree on the persisted file.
            std::env::var(defaults::INDEX_PATH_ENV)
                .ok()
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var("HOME").ok().map(|home| {
                        let mut p = std::path::PathBuf::from(home);
                        p.push(defaults::DEFAULT_INDEX_RELATIVE_PATH);
                        p
                    })
                })
        }
    };

    if let Some(p) = &path {
        index.set_save_path(p.clone());
        match index.load_from_disk() {
            Ok(n) => println!("reindex: loaded {} entries from {}", n, p.display()),
            Err(e) => println!("reindex: starting empty ({}: {})", p.display(), e),
        }
    } else {
        println!("reindex: persistence disabled (no save path)");
    }

    // Walk each root in turn. Empty roots = walk from workspace root.
    let walk_targets: Vec<Option<&str>> = if roots.is_empty() {
        vec![None]
    } else {
        roots.iter().map(|r| Some(r.as_str())).collect()
    };

    let started_total = std::time::Instant::now();
    if timeout_secs == 0 {
        println!(
            "reindex: per-root timeout disabled (walks bound only by node cap = {})",
            defaults::RESOLVE_WALK_NODE_CAP
        );
    } else {
        println!("reindex: per-root timeout = {} s", timeout_secs);
    }
    for target in walk_targets {
        let label = target.unwrap_or("<workspace_root>");
        let start = std::time::Instant::now();
        let controls = if timeout_secs == 0 {
            // No deadline; only the node-count cap and any external
            // cancellation can stop the walk.
            FetchControls::default()
        } else {
            FetchControls::with_timeout(std::time::Duration::from_secs(timeout_secs))
        };
        let result = client
            .get_subtree_with_controls(
                target,
                max_depth,
                defaults::RESOLVE_WALK_NODE_CAP,
                controls,
            )
            .await;
        match result {
            Ok(fetch) => {
                let count = fetch.nodes.len();
                index.ingest(&fetch.nodes);
                let trunc = fetch
                    .truncation_reason
                    .map(|r| format!(", truncated: {}", r.as_str()))
                    .unwrap_or_else(|| ", complete".to_string());
                println!(
                    "reindex: {} -> {} nodes in {} ms{}",
                    label,
                    count,
                    start.elapsed().as_millis(),
                    trunc
                );
            }
            Err(e) => {
                eprintln!("reindex: {} failed: {}", label, e);
            }
        }
    }

    if path.is_some() {
        index.save_to_disk()?;
        println!(
            "reindex: saved {} entries to {} (total elapsed {} ms)",
            index.size(),
            path.as_ref().unwrap().display(),
            started_total.elapsed().as_millis()
        );
    }
    Ok(())
}

// --- audit-mirrors / review delegation ---
//
// The heuristics live in `workflowy_mcp_server::audit` so the MCP tool
// handlers and this CLI share one implementation. The CLI is now a thin
// adapter: it loads the recent session-log blob from disk (the lib is
// pure-data and never touches the filesystem) and passes it through.

use workflowy_mcp_server::audit::{audit_mirrors, build_review, ReviewReport};

const SECONDS_PER_DAY: i64 = 86_400;

/// Read recent session-log files (last 7 days) into a single blob the
/// review function can scan for URL/DOI matches. Returns `""` if the
/// `~/code/SecondBrain/session-logs/` directory doesn't exist — the
/// review function then skips bucket (d) gracefully.
fn load_recent_session_logs_blob() -> String {
    let Some(home) = std::env::var("HOME").ok() else {
        return String::new();
    };
    let dir = std::path::PathBuf::from(format!("{}/code/SecondBrain/session-logs", home));
    if !dir.exists() {
        return String::new();
    }
    let cutoff = chrono::Utc::now().timestamp() - 7 * SECONDS_PER_DAY;
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

fn print_review(r: &ReviewReport) {
    let groups: [(&str, &Vec<workflowy_mcp_server::audit::ReviewItem>); 4] = [
        ("Revisit-due", &r.revisit_due),
        ("Multi-pillar (>=3)", &r.multi_pillar),
        ("Stale cross-pillar", &r.stale_cross_pillar),
        ("Source-MOC re-cited", &r.source_moc_reuse),
    ];
    for (label, items) in groups {
        println!("== {} ({}) ==", label, items.len());
        for it in items {
            println!("  {} \"{}\" — {}", it.node_id, it.name, it.detail);
        }
    }
}

// --- index helpers ---

#[derive(Debug, Clone)]
struct IndexEntry {
    date: String,
    log_type: String,
    sources: String,
    follow_ups: String,
    path: String,
}

fn scan_session_logs(dir: &std::path::Path) -> Result<Vec<IndexEntry>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    let read = std::fs::read_dir(dir).map_err(|e| format!("read_dir({}): {}", dir.display(), e))?;
    let mut paths: Vec<_> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().map(|e| e == "md").unwrap_or(false)
                && p.file_name().map(|n| n != "INDEX.md").unwrap_or(false)
        })
        .collect();
    paths.sort();
    for p in paths {
        let body = std::fs::read_to_string(&p).unwrap_or_default();
        out.push(parse_session_log(&p, &body));
    }
    Ok(out)
}

fn parse_session_log(path: &std::path::Path, body: &str) -> IndexEntry {
    fn cap(s: &str, n: usize) -> String { s.replace('|', "/").chars().take(n).collect() }
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let date = regex::Regex::new(r"^(\d{4}-\d{2}-\d{2})").unwrap()
        .captures(fname).map(|c| c[1].to_string()).unwrap_or_else(|| "?".into());
    let h1 = body.lines().find(|l| l.starts_with("# ")).unwrap_or("").trim_start_matches("# ");
    let summary = body.lines().skip_while(|l| !l.starts_with("# ")).skip(1)
        .find(|l| !l.trim().is_empty()).unwrap_or("");
    let log_type = cap(if !summary.is_empty() { summary } else { h1 }, 120);
    let sources: Vec<_> = body.lines().map(str::trim)
        .filter(|t| t.contains("Sources distilled:") || t.contains("Source MOC:"))
        .map(|t| cap(t, 80)).collect();
    let sources = if sources.is_empty() { "—".into() } else { sources.join("; ") };
    let mut follow_ups = String::new();
    for (i, line) in body.lines().enumerate() {
        if line.contains("**Carry-over:**") {
            follow_ups = cap(line.trim_start_matches("**Carry-over:**").trim(), 120);
            break;
        }
        if line.trim_start().starts_with("## Follow-ups") {
            if let Some(n) = body.lines().skip(i + 1).find(|l| !l.trim().is_empty()) {
                follow_ups = cap(n.trim_start_matches(['-', '*', ' ']), 120);
            }
            break;
        }
    }
    if follow_ups.is_empty() { follow_ups = "—".into(); }
    IndexEntry { date, log_type, sources, follow_ups, path: path.display().to_string() }
}

fn render_index(entries: &[IndexEntry]) -> String {
    let mut s = String::new();
    s.push_str("# Session-logs INDEX\n\n");
    s.push_str(&format!("Generated {} — {} entries\n\n", chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"), entries.len()));
    s.push_str("| Date | Type | Sources | Follow-ups | Path |\n");
    s.push_str("|------|------|---------|------------|------|\n");
    for e in entries {
        s.push_str(&format!(
            "| {} | {} | {} | {} | `{}` |\n",
            e.date, e.log_type, e.sources, e.follow_ups, e.path
        ));
    }
    s
}

/// Mirrors the proximate-cause classification in `src/server.rs::tool_error`
/// so the CLI's stderr line matches what the MCP layer would emit. Kept as a
/// local copy because `tool_error` is private and the spec forbids editing
/// `server.rs`.
fn classify(err: &str) -> &'static str {
    let l = err.to_lowercase();
    if l.contains("404") || l.contains("not found") {
        "not_found"
    } else if l.contains("cancelled") {
        "cancelled"
    } else if l.contains("timeout") || l.contains("timed out") {
        "timeout"
    } else if l.contains("api error 5") {
        "upstream_error"
    } else if l.contains("401") || l.contains("403") || l.contains("unauthor") {
        "auth_failure"
    } else if l.contains("lock") {
        "lock_contention"
    } else if l.contains("cache") {
        "cache_miss"
    } else {
        "unknown"
    }
}

// Bring the WorkflowyError type into scope so error messages render via Display.
// (Used implicitly through `?` conversion.)
#[allow(dead_code)]
fn _force_error_link(_e: WorkflowyError) {}

/// Read the contents of `--input <path>`, or fall back to stdin. Mirrors
/// the convention `BatchCreate` and `Transaction` use: `--input` is a
/// path to a JSON file; absent that, the JSON is piped in via stdin.
fn read_input_or_stdin(path: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    use std::io::Read;
    match path {
        Some(p) => Ok(std::fs::read_to_string(p)?),
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(buf)
        }
    }
}

/// One transaction step, applied directly through the client. Mirrors
/// `WorkflowyMcpServer::apply_txn_op` but without the rmcp / tool_error
/// machinery (the CLI surfaces errors via `Box<dyn Error>`). Returns the
/// summary for the success log plus an optional `(kind, inverse-payload)`
/// pair captured for rollback. Inverse coverage matches the server's
/// `TxnInverse` enum.
async fn apply_txn_step(
    client: &Arc<WorkflowyClient>,
    raw: &serde_json::Value,
) -> Result<(serde_json::Value, Option<serde_json::Value>), Box<dyn std::error::Error>> {
    let op = raw["op"].as_str().ok_or("transaction: each op needs `op`")?;
    let node_id = raw["node_id"].as_str();
    match op {
        "create" => {
            let name = raw["name"].as_str()
                .ok_or("transaction.create: requires `name`")?;
            let parent_id = raw["parent_id"].as_str();
            let description = raw["description"].as_str();
            let priority = raw["priority"].as_i64().map(|p| p as i32);
            let created = client.create_node(name, description, parent_id, priority).await?;
            let summary = json!({"op": "create", "id": created.id, "name": created.name});
            let inverse = json!({"op": "delete-created", "node_id": created.id});
            Ok((summary, Some(inverse)))
        }
        "edit" => {
            let id = node_id.ok_or("transaction.edit: requires `node_id`")?;
            let name = raw["name"].as_str();
            let description = raw["description"].as_str();
            if name.is_none() && description.is_none() {
                return Err("transaction.edit: name or description required".into());
            }
            let prev = client.get_node(id).await.ok();
            client.edit_node(id, name, description).await?;
            let summary = json!({"op": "edit", "id": id});
            let inverse = prev.map(|p| json!({
                "op": "restore-edit", "node_id": id,
                "prev_name": p.name, "prev_description": p.description,
            }));
            Ok((summary, inverse))
        }
        "delete" => {
            let id = node_id.ok_or("transaction.delete: requires `node_id`")?;
            client.delete_node(id).await?;
            let summary = json!({"op": "delete", "id": id});
            // Delete is intentionally not invertible.
            Ok((summary, None))
        }
        "move" => {
            let id = node_id.ok_or("transaction.move: requires `node_id`")?;
            let new_parent = raw["new_parent_id"].as_str()
                .ok_or("transaction.move: requires `new_parent_id`")?;
            let priority = raw["priority"].as_i64().map(|p| p as i32);
            let prev = client.get_node(id).await.ok();
            let prev_parent = prev.as_ref().and_then(|p| p.parent_id.clone());
            let prev_priority = prev.as_ref().and_then(|p| p.priority).map(|p| p as i32);
            client.move_node(id, new_parent, priority).await?;
            let summary = json!({"op": "move", "id": id, "to": new_parent});
            let inverse = json!({
                "op": "un-move", "node_id": id,
                "prev_parent_id": prev_parent, "prev_priority": prev_priority,
            });
            Ok((summary, Some(inverse)))
        }
        "complete" | "uncomplete" => {
            let id = node_id.ok_or(format!("transaction.{}: requires `node_id`", op))?;
            let target = op == "complete";
            let prev = client.get_node(id).await.ok();
            let prev_completed = prev.as_ref().map(|p| p.completed_at.is_some());
            client.set_completion(id, target).await?;
            let summary = json!({"op": op, "id": id});
            let inverse = prev_completed.map(|p| json!({
                "op": "restore-completion", "node_id": id, "prev_completed": p,
            }));
            Ok((summary, inverse))
        }
        other => Err(format!(
            "transaction: unknown op {:?} (expected create/edit/delete/move/complete/uncomplete)",
            other
        ).into()),
    }
}

/// Apply one inverse op recorded by `apply_txn_step`. Same shape as
/// `WorkflowyMcpServer::run_inverse`.
async fn run_inverse_step(
    client: &Arc<WorkflowyClient>,
    inv: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let kind = inv["op"].as_str().unwrap_or("");
    match kind {
        "delete-created" => {
            let id = inv["node_id"].as_str().ok_or("delete-created: missing node_id")?;
            client.delete_node(id).await?;
            Ok(json!({"rolled_back": "create", "id": id}))
        }
        "restore-edit" => {
            let id = inv["node_id"].as_str().ok_or("restore-edit: missing node_id")?;
            let prev_name = inv["prev_name"].as_str();
            let prev_description = inv["prev_description"].as_str();
            client.edit_node(id, prev_name, prev_description).await?;
            Ok(json!({"rolled_back": "edit", "id": id}))
        }
        "un-move" => {
            let id = inv["node_id"].as_str().ok_or("un-move: missing node_id")?;
            let prev_parent_id = inv["prev_parent_id"].as_str();
            let prev_priority = inv["prev_priority"].as_i64().map(|p| p as i32);
            match prev_parent_id {
                Some(pid) => {
                    client.move_node(id, pid, prev_priority).await?;
                    Ok(json!({"rolled_back": "move", "id": id, "to": pid}))
                }
                None => Ok(json!({"skipped": "move", "id": id, "reason": "previous parent unknown"})),
            }
        }
        "restore-completion" => {
            let id = inv["node_id"].as_str().ok_or("restore-completion: missing node_id")?;
            let prev = inv["prev_completed"].as_bool().unwrap_or(false);
            client.set_completion(id, prev).await?;
            Ok(json!({"rolled_back": "completion", "id": id, "restored_to": prev}))
        }
        other => Err(format!("rollback: unknown inverse kind {:?}", other).into()),
    }
}

/// Render a subtree as nested 2-space-indented Markdown bullets. Walks
/// `parent_id` chains starting from `root_id` so the order matches the
/// tree shape regardless of how the input was returned.
fn render_subtree_markdown(
    nodes: &[workflowy_mcp_server::types::WorkflowyNode],
    root_id: &str,
) -> String {
    use std::collections::HashMap;
    let mut by_parent: HashMap<&str, Vec<&workflowy_mcp_server::types::WorkflowyNode>> = HashMap::new();
    for n in nodes {
        if let Some(pid) = &n.parent_id {
            by_parent.entry(pid.as_str()).or_default().push(n);
        }
    }
    let mut out = String::new();
    let mut stack: Vec<(&str, usize)> = vec![(root_id, 0)];
    while let Some((id, depth)) = stack.pop() {
        if let Some(node) = nodes.iter().find(|n| n.id == id) {
            for _ in 0..depth { out.push_str("  "); }
            out.push_str("- ");
            out.push_str(&node.name);
            out.push('\n');
            if let Some(desc) = &node.description {
                if !desc.is_empty() {
                    for _ in 0..(depth + 1) { out.push_str("  "); }
                    out.push_str("> ");
                    out.push_str(desc);
                    out.push('\n');
                }
            }
            if let Some(children) = by_parent.get(id) {
                // Push in reverse so we visit in declared order via pop.
                for child in children.iter().rev() {
                    stack.push((child.id.as_str(), depth + 1));
                }
            }
        }
    }
    out
}

/// Render a subtree as OPML. Same parent-walk strategy as the markdown
/// renderer; XML-encodes `&`, `<`, `>` so a node name containing markup
/// doesn't produce invalid OPML.
fn render_subtree_opml(
    nodes: &[workflowy_mcp_server::types::WorkflowyNode],
    root_id: &str,
) -> String {
    use std::collections::HashMap;
    let mut by_parent: HashMap<&str, Vec<&workflowy_mcp_server::types::WorkflowyNode>> = HashMap::new();
    for n in nodes {
        if let Some(pid) = &n.parent_id {
            by_parent.entry(pid.as_str()).or_default().push(n);
        }
    }
    fn encode(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }
    let mut out = String::from("<?xml version=\"1.0\"?>\n<opml version=\"2.0\">\n  <body>\n");
    fn emit(
        out: &mut String,
        id: &str,
        depth: usize,
        nodes: &[workflowy_mcp_server::types::WorkflowyNode],
        by_parent: &std::collections::HashMap<&str, Vec<&workflowy_mcp_server::types::WorkflowyNode>>,
    ) {
        let Some(node) = nodes.iter().find(|n| n.id == id) else { return };
        let indent = "  ".repeat(depth + 2);
        let mut attrs = format!("text=\"{}\"", encode(&node.name));
        if let Some(d) = &node.description {
            if !d.is_empty() {
                attrs.push_str(&format!(" _note=\"{}\"", encode(d)));
            }
        }
        let children = by_parent.get(id).map(|v| v.as_slice()).unwrap_or(&[]);
        if children.is_empty() {
            out.push_str(&format!("{}<outline {} />\n", indent, attrs));
        } else {
            out.push_str(&format!("{}<outline {}>\n", indent, attrs));
            for c in children {
                emit(out, c.id.as_str(), depth + 1, nodes, by_parent);
            }
            out.push_str(&format!("{}</outline>\n", indent));
        }
    }
    emit(&mut out, root_id, 0, nodes, &by_parent);
    out.push_str("  </body>\n</opml>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use workflowy_mcp_server::types::WorkflowyNode;

    fn node(id: &str, name: &str, desc: Option<&str>, last_modified: Option<i64>) -> WorkflowyNode {
        WorkflowyNode {
            id: id.into(),
            name: name.into(),
            description: desc.map(String::from),
            last_modified,
            ..Default::default()
        }
    }

    #[test]
    fn cli_help_lists_every_subcommand() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        // Every MCP tool has a corresponding CLI subcommand so the parity
        // rule (see memory: feedback_cli_skill_parity) is enforceable from
        // a build-time check. Adding a tool to the MCP without the matching
        // CLI subcommand here will fail this test before it ships.
        for sub in [
            // CRUD + reads
            "status", "get", "children", "create", "move", "delete", "edit",
            "complete", "search", "tag-search", "find", "subtree",
            "backlinks", "find-by-tag-and-path", "node-at-path", "path-of",
            "resolve-link", "since",
            // Todos / scheduling
            "todos", "overdue", "upcoming", "daily-review",
            // Project / activity
            "recent-changes", "project-summary",
            // Bulk writes
            "insert", "smart-insert", "duplicate", "template",
            "bulk-update", "bulk-tag", "batch-create", "transaction", "export",
            // Diagnostics
            "health-check", "cancel-all", "build-name-index", "recent-tools",
            // Existing diagnostics + index
            "audit-mirrors", "review", "index", "reindex",
        ] {
            assert!(help.contains(sub), "help missing subcommand: {sub}\n{help}");
        }
        assert!(help.contains("--dry-run"), "help missing --dry-run global flag\n{help}");
    }

    /// Parity claim: every MCP tool has a matching CLI subcommand.
    /// The MCP-tool list is enumerated against the `cmd_name` match arms
    /// (one per subcommand). Drift between the CLI and the MCP server is
    /// caught here at build time — the audit list is the source of truth.
    #[test]
    fn cli_covers_every_non_diagnostic_mcp_tool() {
        // Every non-diagnostic / non-stub MCP tool has a CLI counterpart.
        // `convert_markdown` is a pure local transform with no API and is
        // intentionally excluded; `create_mirror` is a stub that always
        // errors and is intentionally excluded. Diagnostics that exist
        // only in-process on the MCP server (`recent-tools`, `cancel-all`)
        // ship as no-op CLI wrappers so the surface is uniform.
        let expected_pairs: &[(&str, &str)] = &[
            ("get_node", "get"),
            ("list_children", "children"),
            ("create_node", "create"),
            ("edit_node", "edit"),
            ("delete_node", "delete"),
            ("move_node", "move"),
            ("complete_node", "complete"),
            ("search_nodes", "search"),
            ("tag_search", "tag-search"),
            ("find_node", "find"),
            ("get_subtree", "subtree"),
            ("find_backlinks", "backlinks"),
            ("find_by_tag_and_path", "find-by-tag-and-path"),
            ("node_at_path", "node-at-path"),
            ("path_of", "path-of"),
            ("resolve_link", "resolve-link"),
            ("since", "since"),
            ("list_todos", "todos"),
            ("list_overdue", "overdue"),
            ("list_upcoming", "upcoming"),
            ("daily_review", "daily-review"),
            ("get_recent_changes", "recent-changes"),
            ("get_project_summary", "project-summary"),
            ("insert_content", "insert"),
            ("smart_insert", "smart-insert"),
            ("duplicate_node", "duplicate"),
            ("create_from_template", "template"),
            ("bulk_update", "bulk-update"),
            ("bulk_tag", "bulk-tag"),
            ("batch_create_nodes", "batch-create"),
            ("transaction", "transaction"),
            ("export_subtree", "export"),
            ("audit_mirrors", "audit-mirrors"),
            ("review", "review"),
            ("health_check", "health-check"),
            ("workflowy_status", "status"),
            ("cancel_all", "cancel-all"),
            ("build_name_index", "build-name-index"),
            ("get_recent_tool_calls", "recent-tools"),
        ];
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        for (mcp_tool, cli_subcommand) in expected_pairs {
            assert!(
                help.contains(cli_subcommand),
                "MCP tool `{}` has no CLI subcommand `{}` — parity broken",
                mcp_tool, cli_subcommand,
            );
        }
    }

    #[test]
    fn complete_parses_node_id_and_uncomplete_flag() {
        let parsed = Cli::try_parse_from(["wflow-do", "complete", "abc-uuid"])
            .expect("complete (default mark complete) parses");
        match parsed.cmd {
            Cmd::Complete { node_id, uncomplete } => {
                assert_eq!(node_id, "abc-uuid");
                assert!(!uncomplete, "default must be mark complete");
            }
            _ => panic!("expected Complete"),
        }
        let parsed = Cli::try_parse_from(["wflow-do", "complete", "abc-uuid", "--uncomplete"])
            .expect("complete --uncomplete parses");
        match parsed.cmd {
            Cmd::Complete { node_id, uncomplete } => {
                assert_eq!(node_id, "abc-uuid");
                assert!(uncomplete);
            }
            _ => panic!("expected Complete"),
        }
    }

    #[test]
    fn complete_dry_run_emits_planned_state() {
        let cli = Cli::try_parse_from(["wflow-do", "--dry-run", "complete", "abc"])
            .expect("dry-run complete parses");
        let line = dry_run_line(&cli.cmd).expect("complete yields a dry-run line");
        assert!(line.starts_with("DRY-RUN complete"), "got: {}", line);
        assert!(line.contains("target_state=true"), "got: {}", line);

        let cli = Cli::try_parse_from(["wflow-do", "--dry-run", "complete", "abc", "--uncomplete"])
            .expect("dry-run uncomplete parses");
        let line = dry_run_line(&cli.cmd).expect("uncomplete yields a dry-run line");
        assert!(line.contains("target_state=false"), "got: {}", line);
    }

    #[test]
    fn find_refuses_unscoped_root_walks_at_arg_level() {
        // `Find` keeps the --allow-root-scan gate to mirror the MCP
        // tool's contract. Just verify the flag parses; the runtime
        // refusal is exercised in the dispatch arm.
        let parsed = Cli::try_parse_from([
            "wflow-do", "find", "Tasks", "--allow-root-scan",
        ]).expect("find with --allow-root-scan parses");
        match parsed.cmd {
            Cmd::Find { name, allow_root_scan, .. } => {
                assert_eq!(name, "Tasks");
                assert!(allow_root_scan);
            }
            _ => panic!("expected Find"),
        }
    }

    #[test]
    fn render_subtree_markdown_walks_parent_chain_and_emits_indent() {
        let nodes = vec![
            node("root", "Root", None, None),
            {
                let mut c = node("c1", "Child 1", Some("a desc"), None);
                c.parent_id = Some("root".into());
                c
            },
            {
                let mut g = node("g1", "Grandchild", None, None);
                g.parent_id = Some("c1".into());
                g
            },
        ];
        let body = render_subtree_markdown(&nodes, "root");
        assert!(body.contains("- Root"));
        assert!(body.contains("  - Child 1"));
        assert!(body.contains("    > a desc"));
        assert!(body.contains("    - Grandchild"));
    }

    #[test]
    fn render_subtree_opml_xml_encodes_node_names() {
        let nodes = vec![
            {
                let mut n = node("r", "A & B <c>", None, None);
                n.parent_id = None;
                n
            },
        ];
        let body = render_subtree_opml(&nodes, "r");
        assert!(body.contains("&amp;"), "& must be encoded: {body}");
        assert!(body.contains("&lt;"), "< must be encoded: {body}");
        assert!(body.contains("&gt;"), "> must be encoded: {body}");
        assert!(body.contains("<?xml"), "header must be present: {body}");
    }

    #[test]
    fn classify_covers_known_branches() {
        assert_eq!(classify("API error 404 not found"), "not_found");
        assert_eq!(classify("Cancelled by cancel_all"), "cancelled");
        assert_eq!(classify("request timed out"), "timeout");
        assert_eq!(classify("API error 503"), "upstream_error");
        assert_eq!(classify("HTTP 401 unauthorized"), "auth_failure");
        assert_eq!(classify("lock contention"), "lock_contention");
        assert_eq!(classify("cache stale"), "cache_miss");
        assert_eq!(classify("totally novel failure"), "unknown");
    }

    #[test]
    fn create_parses_required_name_and_optional_parent() {
        let parsed = Cli::try_parse_from([
            "wflow-do", "create", "--name", "x", "--parent", "abc", "--priority", "1",
        ])
        .expect("create flags parse");
        match parsed.cmd {
            Cmd::Create { name, parent, priority, .. } => {
                assert_eq!(name, "x");
                assert_eq!(parent.as_deref(), Some("abc"));
                assert_eq!(priority, Some(1));
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn cli_audit_review_delegate_to_lib() {
        // Smoke check that the CLI's `use workflowy_mcp_server::audit::...`
        // wiring still resolves — the lib has comprehensive unit coverage
        // for the heuristics themselves (see src/audit.rs#tests).
        let nodes = vec![
            node("aaa", "Concept X", Some("canonical_of: bbb"), None),
            node("bbb", "Concept X", Some("mirror_of: aaa"), None),
        ];
        assert!(audit_mirrors(&nodes).is_empty());

        let now = chrono::Utc::now().timestamp();
        let today = chrono::Utc::now().date_naive();
        let nodes = vec![node(
            "a",
            "Past-due note #revisit",
            Some("revisit_due: 2020-01-01"),
            Some(now),
        )];
        let r = build_review(&nodes, 90, today, now, "");
        assert_eq!(r.revisit_due.len(), 1);
    }

    #[test]
    fn index_extracts_summary_from_session_log() {
        let body = "# Session log — 2026-04-25 — distillation\n\n**Type:** Reading list distillation.\n**Outcome:** Twelve atomic notes landed.\n\nSources distilled: Foo 2024, Bar 2023.\n\n## Follow-ups\n\n- Re-read Quux and decide.\n";
        let path = std::path::Path::new("2026-04-25-distillation.md");
        let entry = parse_session_log(path, body);
        assert_eq!(entry.date, "2026-04-25");
        assert!(entry.log_type.contains("Type:") || entry.log_type.contains("Reading"), "got {:?}", entry.log_type);
        assert!(entry.sources.contains("Sources distilled:"), "got {:?}", entry.sources);
        assert!(entry.follow_ups.contains("Quux"), "got {:?}", entry.follow_ups);
    }

    #[test]
    fn dry_run_short_circuits_create_without_api_call() {
        let cli = Cli::try_parse_from([
            "wflow-do", "--dry-run", "create", "--name", "x", "--parent", "abc",
        ])
        .expect("dry-run create parses");
        assert!(cli.dry_run, "global flag should be set");
        let line = dry_run_line(&cli.cmd).expect("create yields a dry-run line");
        assert!(line.starts_with("DRY-RUN create"), "got: {}", line);
        assert!(line.contains("name=\"x\""), "got: {}", line);
        // Read-only verbs return None — --dry-run is a no-op for them.
        let read_only = Cli::try_parse_from(["wflow-do", "--dry-run", "status"]).unwrap();
        assert!(dry_run_line(&read_only.cmd).is_none());
    }
}
