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

use workflowy_mcp_server::defaults::default_review_root;

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
    /// Reorder a set of nodes under a given parent. Pass --node for each
    /// id in the desired head-first order: the first --node ends up at
    /// position 0, the last --node ends up after every other id in the
    /// list. Side effect: ids not currently under `--parent` are
    /// reparented as part of the reorder. Mirrors MCP `reorder_nodes`
    /// — same `crate::workflows::reorder_nodes_via_priority` workflow.
    Reorder {
        /// Parent under which to order the listed nodes.
        #[arg(long = "parent")]
        parent_id: String,
        /// Desired order, head-first. Repeat `--node` for each id.
        #[arg(long = "node", value_name = "NODE_ID", num_args = 1..)]
        nodes: Vec<String>,
    },
    /// Delete a node.
    Delete {
        node_id: String,
        /// Optional name-echo guard: the current name of the node you intend
        /// to delete. If set and it does not match the resolved node's name,
        /// the delete is refused. Mirrors the MCP `delete_node.expect_name`
        /// host-coercion defence.
        #[arg(long)]
        expect_name: Option<String>,
    },
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
        /// Serve from the persisted name index at $WORKFLOWY_INDEX_PATH
        /// (token-AND over names + descriptions) instead of a live walk.
        /// No API calls; misses nodes not yet walked/reindexed.
        #[arg(long)]
        use_index: bool,
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
        /// Serve from the persisted name index at $WORKFLOWY_INDEX_PATH
        /// instead of a live walk. Whole-tag match, no API calls.
        #[arg(long)]
        use_index: bool,
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
        /// Serve from the persisted name index at $WORKFLOWY_INDEX_PATH
        /// (name-only match) instead of a live walk. No API calls.
        #[arg(long)]
        use_index: bool,
    },
    /// Get the full subtree under a node, bounded by `--depth` and the
    /// 10 000-node walk cap. Mirrors MCP `get_subtree`.
    Subtree {
        node_id: String,
        #[arg(long, default_value_t = 5)]
        depth: usize,
        /// Wall-clock budget in seconds. Defaults to the interactive
        /// subtree timeout. Set to 0 for no time budget: the walk then
        /// runs until the node-count cap or until the subtree is
        /// exhausted, so it returns the COMPLETE subtree rather than a
        /// depth-3-in-20-seconds partial. Pair with `--patient` for a
        /// walk that also survives rate-limit windows. Used by
        /// dromologue-site's canonical drift check, which needs the true
        /// subtree max, not a partial one.
        #[arg(long, default_value_t = defaults::SUBTREE_FETCH_TIMEOUT_MS / 1000)]
        timeout_secs: u64,
        /// Trade time for coverage: wait out rate-limit windows and keep
        /// re-attempting dropped branches until they stop recovering,
        /// rather than dropping them on the first 429. Pair with
        /// `--timeout-secs 0` — a deadline that cannot cover a rate-limit
        /// wait makes the walk skip the wait and drop branches anyway.
        #[arg(long)]
        patient: bool,
    },
    /// Find every node containing a Workflowy link to the given node.
    /// Mirrors MCP `find_backlinks`.
    Backlinks {
        node_id: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value_t = 8)]
        depth: usize,
        /// Scan the persisted name index at $WORKFLOWY_INDEX_PATH instead
        /// of walking. Covers everything ever indexed, no walk budget.
        #[arg(long)]
        use_index: bool,
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
    /// is not supplied. Omit `parent_id` to insert at the workspace root —
    /// matches the null-means-root convention used by `create` and the
    /// MCP `insert_content` tool since the failure-report 2026-05-03 fix.
    Insert {
        parent_id: Option<String>,
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
        /// Prefix prepended to the ROOT copy's name (descendants
        /// unchanged). Parity with MCP `duplicate_node.name_prefix`.
        #[arg(long = "name-prefix")]
        name_prefix: Option<String>,
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
    /// Batched reads (get_node | list_children | get_subtree). Reads JSON
    /// array from `--input` (path) or stdin:
    /// `[{"op":"get_node","node_id":"..."},{"op":"list_children","node_id":null},{"op":"get_subtree","node_id":"...","max_depth":5}]`.
    /// Mirrors MCP `read_batch`.
    ReadBatch {
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
    /// command exists for surface parity. Use against the live
    /// Workflowy MCP server when you need real data.
    RecentTools {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Audit `canonical_of:` / `mirror_of:` markers under a subtree.
    /// Mirrors the MCP `audit_mirrors` tool: default scope is the
    /// Distillations root, walked in chunks (one per direct child) to
    /// avoid the 10 000-node walk cap. Canonical resolution widens to
    /// `get_node` calls for `mirror_of:` UUIDs the walk didn't catch
    /// so cross-pillar mirrors don't false-positive as BROKEN.
    AuditMirrors {
        #[arg(long)]
        root: Option<String>,
        /// Force-chunked walk (default: true when --root is omitted,
        /// false when --root is supplied). Pass false to opt out.
        #[arg(long)]
        chunked: Option<bool>,
        /// Widen canonical resolution beyond the walked scope by
        /// issuing `get_node` for `mirror_of:` UUIDs not covered by
        /// the walk. Defaults to true. Set false to restore the
        /// legacy in-scope-only classifier (any cross-scope mirror
        /// will then classify as BROKEN).
        #[arg(long)]
        cross_scope_resolve: Option<bool>,
    },
    /// Create a convention-based mirror of a canonical node under a
    /// new parent. Mirrors MCP `create_mirror`. The mirror's name is
    /// copied verbatim from the canonical, and its description carries
    /// `mirror_of: <canonical_uuid>`. Workflowy's REST API does not
    /// expose native mirror creation; this is the documented note
    /// convention `audit_mirrors` already understands. Edits to the
    /// canonical do NOT propagate to the mirror.
    CreateMirror {
        /// UUID or short hash of the canonical node to mirror.
        canonical_node_id: String,
        /// UUID, short hash, or omitted for workspace root.
        #[arg(long = "to")]
        target_parent_id: Option<String>,
        /// Position among siblings of the new mirror (lower = earlier).
        #[arg(long)]
        priority: Option<i32>,
        /// Optional pillar token to write to the canonical's
        /// `canonical_of:` marker if it lacks one. Skipped when
        /// omitted; existing markers are never overwritten.
        #[arg(long)]
        pillar: Option<String>,
        /// Resolve canonical and target without writing. Prints what
        /// the production call would create — mirror name (copied from
        /// the canonical), resolved target_parent_id, and whether the
        /// pillar annotation would be applied. Pair with the
        /// production call once the resolved scope is verified.
        #[arg(long)]
        dry_run: bool,
    },
    /// Surface what's worth re-reading: revisit-due, multi-pillar, stale, source-MOC reuse.
    Review {
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 90)]
        days_stale: i64,
    },
    /// Generate `INDEX.md` from the local logs. Default output is
    /// `$SECONDBRAIN_DIR/session-logs/INDEX.md`; pass `--out` to
    /// override. Errors if neither is provided.
    Index {
        #[arg(long)]
        out: Option<String>,
    },
    /// Walk one or more subtrees and merge what's found into the persistent
    /// name index at `$WORKFLOWY_INDEX_PATH`. Unset or empty disables
    /// persistence (the walk runs in memory). Each subtree is walked
    /// with the resolution budget; partial walks still write the nodes
    /// they reached. Useful for one-shot deep indexing from the shell,
    /// independent of any running MCP session.
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
        // The prose deliberately does not restate the default's value —
        // clap renders it from the constant below. It read "300 s" for
        // months after `RESOLVE_WALK_TIMEOUT_MS` dropped to 20 s, because
        // a number written in two places drifts in one of them.
        /// Wall-clock budget per root in seconds. The default (shown
        /// below) is the resolution-walk timeout, which is tuned for an
        /// interactive resolve rather than an indexing run — set this
        /// explicitly for a real reindex. Set to 0 for no time budget:
        /// the walk then runs until the node-count cap or until the
        /// subtree is exhausted, whichever fires first. Use a large
        /// value (e.g. 3600 for one hour per root) to reach deep regions
        /// of large subtrees. The node-count cap still applies regardless.
        #[arg(long, default_value_t = (defaults::RESOLVE_WALK_TIMEOUT_MS / 1000) as u64)]
        timeout_secs: u64,
        /// Trade time for coverage: wait out rate-limit windows and keep
        /// re-attempting dropped branches until they stop recovering,
        /// rather than dropping them on the first 429. Without this a walk
        /// under rate-limit pressure silently omits whole subtrees — one
        /// 429 opens a ~50 s window in which every remaining child fetch
        /// fails instantly, so entire levels get dropped in milliseconds.
        /// Use for any scheduled/unattended reindex, and pair it with
        /// `--timeout-secs 0`: a deadline that cannot cover a rate-limit
        /// wait makes the walk skip the wait and drop the branches anyway.
        #[arg(long)]
        patient: bool,
        /// Rebuild the WHOLE index from a single bulk `GET /nodes-export`
        /// instead of walking roots level-by-level. One request returns
        /// every node (flat, with parent_id) in ~30 s, replacing the
        /// patient-walk machinery — no truncation, no dropped branches, no
        /// 429 storm. `--root`, `--max-depth`, `--timeout-secs` and
        /// `--patient` are IGNORED (an export covers the entire tree); the
        /// `WORKFLOWY_INDEX_EXCLUDE_SUBTREES` filter still applies at the
        /// save boundary, so excluded subtrees never reach disk. Workflowy
        /// throttles this endpoint hard — do not loop it faster than the
        /// ~65 s floor. This is the recommended path for the nightly job.
        #[arg(long = "full-export")]
        full_export: bool,
    },
    /// List nodes modified since a given time, served entirely from the
    /// persisted name index at $WORKFLOWY_INDEX_PATH — zero API calls.
    /// The local half of incremental sync: the scheduled reindex records
    /// each node's upstream modifiedAt (index schema v3); this diffs it.
    /// Entries with no recorded timestamp are excluded (absence means
    /// "not observed"); for suspected gaps, re-run the reindex.
    ChangedSince {
        /// Cutoff: epoch seconds (the index's native unit), epoch
        /// milliseconds (auto-detected and divided down), or a YYYY-MM-DD
        /// date (midnight UTC).
        since: String,
        /// Restrict to descendants of this node (full UUID as stored).
        #[arg(long)]
        root: Option<String>,
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
    /// Create a NATIVE Workflowy mirror via the beta API (a real mirror
    /// whose shared content stays in sync with its origin), as opposed to
    /// the convention-based `create-mirror` (which duplicates a node and
    /// writes a `mirror_of:` note). CLI-only and experimental: native
    /// mirrors are BETA-ONLY — on the production account the created node
    /// renders with an empty name and no mirror metadata until Workflowy
    /// ships mirrors to production. Distinct mechanism from `audit-mirrors`
    /// (which tracks the note convention).
    NativeMirrorCreate {
        /// UUID or short hash of the canonical (origin) node to mirror.
        canonical_node_id: String,
        /// UUID or short hash of the parent under which the mirror appears.
        #[arg(long = "to")]
        target_parent_id: String,
        /// Position among siblings: "top" (default) or "bottom".
        #[arg(long, default_value = "top")]
        position: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    dotenvy::dotenv().ok();

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
        Cmd::Reorder { .. } => "reorder",
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
        Cmd::ReadBatch { .. } => "read-batch",
        Cmd::Transaction { .. } => "transaction",
        Cmd::Export { .. } => "export",
        Cmd::HealthCheck => "health-check",
        Cmd::CancelAll => "cancel-all",
        Cmd::BuildNameIndex { .. } => "build-name-index",
        Cmd::RecentTools { .. } => "recent-tools",
        Cmd::AuditMirrors { .. } => "audit-mirrors",
        Cmd::CreateMirror { .. } => "create-mirror",
        Cmd::Review { .. } => "review",
        Cmd::Index { .. } => "index",
        Cmd::Reindex { .. } => "reindex",
        Cmd::ChangedSince { .. } => "changed-since",
        Cmd::NativeMirrorCreate { .. } => "native-mirror-create",
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
        Cmd::Reorder { parent_id, nodes } => Some(format!(
            "DRY-RUN reorder parent_id={} count={}",
            parent_id,
            nodes.len(),
        )),
        Cmd::Delete { node_id, expect_name } => Some(format!(
            "DRY-RUN delete node_id={}{}",
            node_id,
            expect_name
                .as_deref()
                .map(|n| format!(" expect_name={:?}", n))
                .unwrap_or_default()
        )),
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
            parent_id.as_deref().unwrap_or("<workspace root>"),
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

/// Hydrate the persisted name index for a `--use-index` read. Fails loud
/// when the env var is unset or the file is empty — an index-backed query
/// against nothing would masquerade as "no results".
fn load_persistent_index(
) -> Result<workflowy_mcp_server::utils::NameIndex, Box<dyn std::error::Error>> {
    use workflowy_mcp_server::utils::NameIndex;
    let path = std::env::var(defaults::INDEX_PATH_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or("--use-index requires $WORKFLOWY_INDEX_PATH to point at the persisted name index")?;
    let index = NameIndex::new();
    index.set_save_path(std::path::PathBuf::from(&path));
    let loaded = index.load_from_disk()?;
    if loaded == 0 {
        return Err(format!(
            "the persisted name index at {} is empty; run `wflow-do reindex --timeout-secs 0 --patient --root <UUID>` first",
            path
        )
        .into());
    }
    Ok(index)
}

fn build_client() -> Result<Arc<WorkflowyClient>, Box<dyn std::error::Error>> {
    let config = validate_config().map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    // The listing cache pays off within a single CLI run too: chunked
    // audit walks and duplicate/template deep-copies revisit shared
    // branches, and writes invalidate through the same request funnel.
    let client = WorkflowyClient::new(config.workflowy_base_url, config.workflowy_api_key)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?
        .with_node_cache(workflowy_mcp_server::utils::cache::get_cache());
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
            // `client.move_node` itself embeds the propagation-retry loop
            // since the 2026-05-04 unification — see the docstring at the
            // method definition. Both this CLI surface and the MCP
            // `move_node` tool handler call this single entry point.
            client.move_node(node_id, to, *priority).await?;
            if cli.json {
                println!("{}", json!({ "ok": true, "node_id": node_id, "new_parent": to }));
            } else {
                println!("Moved {} -> {}", node_id, to);
            }
        }
        Cmd::Reorder { parent_id, nodes } => {
            // Both this CLI surface and the MCP `reorder_nodes` tool
            // delegate to the same workflow function. The CLI passes a
            // default WorkflowContext (no cancel, no deadline) — single-
            // shot processes don't need either; the MCP wraps with its
            // tool_handler! cancel + bulk-budget deadline.
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let (outcome, _footprint) =
                workflowy_mcp_server::workflows::reorder_nodes_via_priority(
                    &client, parent_id, nodes, &ctx,
                )
                .await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                use workflowy_mcp_server::workflows::ReorderOutcome;
                match &outcome {
                    ReorderOutcome::Complete {
                        attempted, succeeded, failed, ..
                    } => println!(
                        "Reordered {} node(s) under {} ({} ok, {} failed)",
                        attempted, parent_id, succeeded, failed,
                    ),
                    ReorderOutcome::Partial {
                        reason,
                        attempted,
                        succeeded,
                        failed,
                        skipped,
                        ..
                    } => println!(
                        "Reorder partial ({}): attempted={} succeeded={} failed={} skipped={} parent={}",
                        reason.as_str(),
                        attempted,
                        succeeded,
                        failed,
                        skipped,
                        parent_id,
                    ),
                }
            }
        }
        Cmd::Delete { node_id, expect_name } => {
            // Name-echo guard (host-coercion defence) — parity with MCP
            // `delete_node.expect_name`. Shared comparison helper so the
            // two surfaces cannot drift.
            if let Some(expected) = expect_name.as_deref() {
                let current = client.get_node(node_id).await?;
                if !workflowy_mcp_server::workflows::destructive_echo_matches(
                    &current.name,
                    expected,
                ) {
                    return Err(format!(
                        "delete refused: node {} is named {:?} but --expect-name was {:?} — re-resolve and confirm before retrying",
                        node_id, current.name, expected
                    )
                    .into());
                }
            }
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
        Cmd::Search { query, parent, depth, limit, use_index } => {
            if *use_index {
                // Serve from the persisted index: token-AND over names +
                // descriptions, subtree-scoped via parent-chain links —
                // the same semantics the MCP `search_nodes(use_index)`
                // path has, with zero API calls.
                let index = load_persistent_index()?;
                let mut hits = index.search_tokens(query);
                if let Some(p) = parent.as_deref() {
                    hits.retain(|e| index.is_descendant_of(&e.node_id, p));
                }
                hits.truncate(*limit);
                let payload = json!({
                    "query": query,
                    "scope": parent,
                    "served_from": "name_index",
                    "name_index_size": index.size(),
                    "count": hits.len(),
                    "matches": hits.iter().map(|e| json!({
                        "id": e.node_id,
                        "name": e.name,
                        "description": e.description,
                        "parent_id": e.parent_id,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
                return Ok(());
            }
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
        Cmd::TagSearch { tag, parent, depth, limit, use_index } => {
            use workflowy_mcp_server::utils::tag_parser::node_has_tag;
            if *use_index {
                let index = load_persistent_index()?;
                let bare = tag.trim_start_matches('#').trim_start_matches('@');
                let as_tag = format!("#{}", bare);
                let as_assignee = format!("@{}", bare);
                let mut matches: Vec<workflowy_mcp_server::utils::name_index::NameIndexEntry> =
                    Vec::new();
                index.for_each_entry(|e| {
                    let probe = workflowy_mcp_server::types::WorkflowyNode {
                        name: e.name.clone(),
                        description: e.description.clone(),
                        ..Default::default()
                    };
                    if node_has_tag(&probe, &as_tag) || node_has_tag(&probe, &as_assignee) {
                        matches.push(e.clone());
                    }
                });
                if let Some(p) = parent.as_deref() {
                    matches.retain(|e| index.is_descendant_of(&e.node_id, p));
                }
                matches.truncate(*limit);
                let payload = json!({
                    "tag": tag,
                    "scope": parent,
                    "served_from": "name_index",
                    "name_index_size": index.size(),
                    "count": matches.len(),
                    "matches": matches.iter().map(|e| json!({
                        "id": e.node_id,
                        "name": e.name,
                        "parent_id": e.parent_id,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
                return Ok(());
            }
            // Whole-tag match via the shared `node_has_tag` predicate so
            // the CLI cannot drift from MCP `tag_search`. We check both a
            // `#tag` and an `@assignee` interpretation of the needle so a
            // bare needle still matches either axis (the CLI's historic
            // tag-OR-assignee behaviour).
            let bare = tag.trim_start_matches('#').trim_start_matches('@');
            let as_tag = format!("#{}", bare);
            let as_assignee = format!("@{}", bare);
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(parent.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter(|n| node_has_tag(n, &as_tag) || node_has_tag(n, &as_assignee))
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
        Cmd::Find { name, parent, match_mode, depth, allow_root_scan, limit, use_index } => {
            if *use_index {
                let index = load_persistent_index()?;
                let mut hits = index.lookup(name, match_mode);
                if let Some(p) = parent.as_deref() {
                    hits.retain(|e| index.is_descendant_of(&e.node_id, p));
                }
                hits.truncate(*limit);
                let payload = json!({
                    "name": name,
                    "match_mode": match_mode,
                    "scope": parent,
                    "served_from": "name_index",
                    "name_index_size": index.size(),
                    "count": hits.len(),
                    "matches": hits.iter().map(|e| json!({
                        "id": e.node_id,
                        "name": e.name,
                        "parent_id": e.parent_id,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
                return Ok(());
            }
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
        Cmd::Subtree { node_id, depth, timeout_secs, patient } => {
            // A 0 budget means "no deadline" — the walk is bound only by the
            // node cap, so it returns the complete subtree rather than a
            // partial. `--patient` additionally waits out rate-limit windows.
            let controls = if *timeout_secs == 0 {
                FetchControls::default()
            } else {
                FetchControls::with_timeout(std::time::Duration::from_secs(*timeout_secs))
            };
            let controls = if *patient { controls.patient() } else { controls };
            let fetch = client
                .get_subtree_with_controls(Some(node_id), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let payload = json!({
                "node_id": node_id,
                "depth": depth,
                "count": fetch.nodes.len(),
                "truncated": fetch.truncated,
                "truncation_reason": fetch.truncation_reason.map(|r| r.as_str()),
                "skipped_branches": fetch.skipped_branches,
                "nodes": fetch.nodes,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::Backlinks { node_id, parent, depth, use_index } => {
            if *use_index {
                use workflowy_mcp_server::utils::link_parser::node_links_to;
                let index = load_persistent_index()?;
                let mut matches: Vec<workflowy_mcp_server::utils::name_index::NameIndexEntry> =
                    Vec::new();
                index.for_each_entry(|e| {
                    if e.node_id == *node_id {
                        return;
                    }
                    let probe = workflowy_mcp_server::types::WorkflowyNode {
                        name: e.name.clone(),
                        description: e.description.clone(),
                        ..Default::default()
                    };
                    if node_links_to(&probe, node_id) {
                        matches.push(e.clone());
                    }
                });
                if let Some(p) = parent.as_deref() {
                    matches.retain(|e| index.is_descendant_of(&e.node_id, p));
                }
                let payload = json!({
                    "target": node_id,
                    "scope": parent,
                    "served_from": "name_index",
                    "name_index_size": index.size(),
                    "count": matches.len(),
                    "backlinks": matches.iter().map(|e| json!({
                        "id": e.node_id,
                        "name": e.name,
                        "path": index.path_of(&e.node_id).join(" > "),
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
                return Ok(());
            }
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(parent.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            // A backlink is a node whose name or description references the
            // target. The match predicate is the shared `node_links_to`
            // (UNION of canonical-URL and 12-char short-hash forms) so the
            // CLI cannot drift from MCP `find_backlinks`.
            use workflowy_mcp_server::utils::link_parser::node_links_to;
            let needle_full = node_id.as_str();
            let hits: Vec<_> = fetch
                .nodes
                .iter()
                .filter(|n| n.id != *needle_full)
                .filter(|n| node_links_to(n, needle_full))
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
            use workflowy_mcp_server::utils::tag_parser::node_has_tag;
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
                    // Whole-tag match via the shared `node_has_tag`
                    // predicate so the CLI cannot drift from MCP
                    // `find_by_tag_and_path`.
                    if !node_has_tag(n, tag) {
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
            // Strip HTML on each child name before comparing, matching MCP
            // `node_at_path` — Workflowy node names can carry inline markup
            // (`<b>`, links) that the raw `eq_ignore_ascii_case` the CLI
            // used pre-2026-06-16 failed to see through.
            use workflowy_mcp_server::utils::html::strip_html;
            let mut current: Option<String> = root.clone();
            for seg in segments {
                let needle = seg.trim().to_lowercase();
                let children = match current.as_deref() {
                    Some(id) => client.get_children_with_propagation_retry(id).await?,
                    None => client.get_top_level_nodes().await?,
                };
                let target = children.iter().find(|c| {
                    strip_html(&c.name).to_lowercase().trim() == needle
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
            // Parent-chain walk shared with MCP `path_of` via
            // `walk_parent_chain` — the cycle guard lives there, so a
            // malformed parent loop terminates instead of spinning to the
            // depth cap (the pre-2026-06-16 CLI had no guard).
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let chain = workflowy_mcp_server::workflows::walk_parent_chain(
                &client,
                node_id,
                *max_depth,
                &ctx,
            )
            .await?;
            let path: Vec<serde_json::Value> = chain
                .segments
                .iter()
                .map(|s| json!({ "id": s.id, "name": s.name }))
                .collect();
            let payload = json!({ "node_id": node_id, "path": path });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::ResolveLink { link, segments } => {
            // Facade over the lifted `crate::workflows::resolve_link_*`
            // helpers — see the doc comment block at the top of the
            // resolve_link section in src/workflows.rs for the design.
            // The CLI omits the single-flight scope marker (no concurrent
            // caller), but since 2026-07-21 it DOES use the persistent
            // name index at `$WORKFLOWY_INDEX_PATH` when configured:
            // an O(1) preflight answers without any walk, and a
            // miss-walk's nodes are merged back into the file so every
            // walk permanently extends coverage instead of being thrown
            // away. Everything else — URL parsing, walk-and-scan,
            // hit/miss payload construction — routes through the same
            // workflow functions the MCP handler calls.
            use workflowy_mcp_server::utils::NameIndex;
            use workflowy_mcp_server::workflows::{
                build_resolve_link_hit_payload, build_resolve_link_miss_payload,
                resolve_link_via_walk_and_scan,
            };
            let candidate = match workflowy_mcp_server::utils::link_parser::extract_workflowy_short_hash(link) {
                Some(h) => h,
                None => {
                    return Err(format!(
                        "could not extract a Workflowy short hash from {:?}. Expected a workflowy.com URL (fragment, /s/<slug>/<hash>, or ?focusedItem=<hash> form) or a bare 8/12/32-char hex hash.",
                        link
                    )
                    .into())
                }
            };

            // Direct full-UUID input: skip the walk entirely. Matches
            // the MCP handler's `full_uuid_passthrough` shortcut.
            if candidate.len() == 32 {
                match client.get_node(&candidate).await {
                    Ok(node) => {
                        let payload = build_resolve_link_hit_payload(&node, "full_uuid_passthrough");
                        println!("{}", serde_json::to_string_pretty(&payload)?);
                        return Ok(());
                    }
                    Err(e) => return Err(format!("get_node({}) failed: {}", candidate, e).into()),
                }
            }

            // Persistent-index preflight (2026-07-21): hydrate from
            // `$WORKFLOWY_INDEX_PATH` and try the short hash before any
            // walk. A hit costs one `get_node` (which also verifies the
            // entry isn't stale — a 404 falls through to the walk).
            let index = NameIndex::new();
            let index_path = std::env::var(defaults::INDEX_PATH_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(std::path::PathBuf::from);
            if let Some(p) = &index_path {
                index.set_save_path(p.clone());
                let _ = index.load_from_disk();
                if let Some(full) = index.resolve_short_hash(&candidate) {
                    if let Ok(node) = client.get_node(&full).await {
                        let payload = build_resolve_link_hit_payload(&node, "cache_hit");
                        println!("{}", serde_json::to_string_pretty(&payload)?);
                        return Ok(());
                    }
                    eprintln!(
                        "resolve-link: index entry for {} is stale (get_node failed); walking",
                        candidate
                    );
                }
            }

            // Walk the scope (parent path → UUID, or workspace root).
            let scope_uuid = if segments.is_empty() {
                None
            } else {
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
            let scope_str = if segments.is_empty() {
                "the workspace root".to_string()
            } else {
                format!("path {:?}", segments)
            };

            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::RESOLVE_WALK_TIMEOUT_MS,
            ));
            let walk = match resolve_link_via_walk_and_scan(
                &client,
                &candidate,
                scope_uuid.as_deref(),
                controls,
            )
            .await
            {
                Ok(w) => w,
                Err(e) => {
                    // Walk-error miss envelope — symmetric with the MCP
                    // handler's `walk_error` branch.
                    let payload = build_resolve_link_miss_payload(
                        &candidate,
                        &scope_str,
                        0,
                        0,
                        true,
                        None,
                        "walk_error",
                        index_path.as_ref().map(|_| index.size()),
                    );
                    println!("{}", serde_json::to_string_pretty(&payload)?);
                    return Err(format!("resolve_link walk failed: {}", e).into());
                }
            };

            // Contribute the walk to the persistent index (merge-on-save,
            // so concurrent writers compose). Pre-2026-07-21 the CLI threw
            // this coverage away, and the next miss re-walked the same
            // region — the "requires walking the tree again" complaint.
            if index_path.is_some() && !walk.nodes.is_empty() {
                index.ingest(&walk.nodes);
                match index.save_to_disk() {
                    Ok(()) => eprintln!(
                        "resolve-link: contributed {} walked nodes to the persistent index",
                        walk.nodes.len()
                    ),
                    Err(e) => eprintln!("resolve-link: could not persist walked nodes: {}", e),
                }
            }

            match walk.found {
                Some(node) => {
                    // The CLI is always the primary on its own walk
                    // (no concurrent caller, no single-flight marker),
                    // so the only walk-derived hit value is `scoped_walk`.
                    let payload = build_resolve_link_hit_payload(&node, "scoped_walk");
                    println!("{}", serde_json::to_string_pretty(&payload)?);
                }
                None => {
                    // Miss envelope — same builder the MCP handler
                    // calls. `name_index_size` reflects the hydrated
                    // persistent index when `$WORKFLOWY_INDEX_PATH` is
                    // configured; the lifted hint adjusts its wording
                    // when it is not.
                    let payload = build_resolve_link_miss_payload(
                        &candidate,
                        &scope_str,
                        walk.nodes_walked,
                        walk.elapsed_ms,
                        walk.truncated,
                        walk.truncation_reason,
                        "primary_walk",
                        index_path.as_ref().map(|_| index.size()),
                    );
                    println!("{}", serde_json::to_string_pretty(&payload)?);
                    // Surface the miss to the caller via a non-zero
                    // exit code so shell pipelines can branch on it.
                    // The MCP handler always returns Ok; the CLI
                    // returns Err so an unattended script doesn't
                    // silently continue with a `resolved: null`.
                    return Err(format!("link {:?} did not resolve to any node", link).into());
                }
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
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(parent.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            // Aggregation routed through the shared helper so the CLI
            // and the MCP `list_todos` handler can't drift in semantics
            // — see `src/utils/aggregation.rs`.
            let hits = workflowy_mcp_server::utils::aggregation::filter_todos(
                &fetch.nodes, status, query.as_deref(), *limit,
            );
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
            let today = chrono::Utc::now().date_naive();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let hits = workflowy_mcp_server::utils::aggregation::compute_overdue(
                &fetch.nodes, today, *include_completed,
            );
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
            let today = chrono::Utc::now().date_naive();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let hits = workflowy_mcp_server::utils::aggregation::compute_upcoming(
                &fetch.nodes, today, *days, *include_completed,
            );
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
            // Aggregation routed through the shared helper so the CLI
            // and the MCP `daily_review` tool cannot drift —
            // see `src/utils/aggregation.rs::compute_daily_review`.
            let today = chrono::Utc::now().date_naive();
            let now_ms = chrono::Utc::now().timestamp_millis();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let review = workflowy_mcp_server::utils::aggregation::compute_daily_review(
                &fetch.nodes,
                today,
                now_ms,
                7,  // upcoming_days — historical CLI default
                1,  // recent_days — historical CLI default (24-hour window)
                10, // overdue_limit
                20, // due_soon_limit
                20, // recent_limit
                20, // pending_limit
            );
            let mut value = serde_json::to_value(&review)?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("scope".into(), json!(root));
                obj.insert("truncated".into(), json!(fetch.truncated));
                obj.insert("truncation_limit".into(), json!(fetch.limit));
                obj.insert(
                    "truncation_reason".into(),
                    json!(fetch.truncation_reason.map(|r| r.as_str())),
                );
            }
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Cmd::RecentChanges { root, hours, depth, limit } => {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(root.as_deref(), *depth, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let hits = workflowy_mcp_server::utils::aggregation::compute_recent_changes(
                &fetch.nodes, now_ms, *hours, *limit,
            );
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
            // Aggregation routed through the shared helper so the CLI
            // and the MCP `get_project_summary` tool cannot drift —
            // see `src/utils/aggregation.rs::compute_project_summary`.
            let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                defaults::SUBTREE_FETCH_TIMEOUT_MS,
            ));
            let fetch = client
                .get_subtree_with_controls(Some(node_id), 10, defaults::MAX_SUBTREE_NODES, controls)
                .await?;
            let today = chrono::Utc::now().date_naive();
            let now_ms = chrono::Utc::now().timestamp_millis();
            let summary = workflowy_mcp_server::utils::aggregation::compute_project_summary(
                &fetch.nodes, node_id, today, now_ms, true, 7,
            )
            .ok_or_else(|| {
                format!(
                    "project-summary: node {:?} not found in the {}-node walk; pass a UUID/short-hash that resolves under the workspace root",
                    node_id, fetch.nodes.len(),
                )
            })?;
            let mut value = serde_json::to_value(&summary)?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("truncated".into(), json!(fetch.truncated));
                obj.insert("truncation_limit".into(), json!(fetch.limit));
                obj.insert(
                    "truncation_reason".into(),
                    json!(fetch.truncation_reason.map(|r| r.as_str())),
                );
            }
            println!("{}", serde_json::to_string_pretty(&value)?);
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
            // Shared workflow handles parsing, cap enforcement,
            // partial-success reporting. CLI passes a default
            // (no-cancel, no-deadline) context. A hard mid-batch API
            // error no longer propagates as Err (which would discard the
            // committed-count); it returns a Partial { reason: "error" }
            // carrying created_count + last_inserted_id, same as the MCP
            // surface (write-path report Recommendation D, 2026-06-17).
            let parsed = workflowy_mcp_server::workflows::parse_indented_content(&body);
            if parsed.is_empty() {
                println!("insert: nothing to insert (no non-blank lines)");
                return Ok(());
            }
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let (outcome, _footprint) =
                workflowy_mcp_server::workflows::insert_content_via_indented(
                    &client,
                    parent_id.as_deref(),
                    parsed,
                    &ctx,
                )
                .await?;
            // The CLI surfaces both the Complete and Partial shapes —
            // unlike pre-2026-05-04, where the CLI had no partial
            // surface at all. Both shapes are JSON so shell pipelines
            // can route on `status` (and `reason` within partial).
            let is_error_partial = matches!(
                &outcome,
                workflowy_mcp_server::workflows::InsertContentOutcome::Partial {
                    reason: workflowy_mcp_server::workflows::PartialReason::Error,
                    ..
                }
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::to_value(&outcome)?)?
            );
            // Exit non-zero on a hard-error partial so shell pipelines see
            // the failure, while the full resume cursor is already on stdout.
            // Cancel/timeout partials exit 0 — they are expected resume points.
            if is_error_partial {
                std::process::exit(1);
            }
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
            // Insertion delegated to the shared workflow — same
            // code path the MCP `smart_insert` handler uses, so
            // both surfaces respect 2-space indentation uniformly.
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let (outcome, _footprint) =
                workflowy_mcp_server::workflows::smart_insert_under_target(
                    &client,
                    &parent.id,
                    &body,
                    &ctx,
                )
                .await?;
            let created = match &outcome {
                workflowy_mcp_server::workflows::InsertContentOutcome::Complete {
                    created_count,
                    ..
                } => *created_count,
                workflowy_mcp_server::workflows::InsertContentOutcome::Partial {
                    created_count,
                    ..
                } => *created_count,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "matched_parent": { "id": parent.id, "name": parent.name },
                    "created_count": created,
                    "outcome": outcome,
                }))?
            );
        }
        Cmd::Duplicate { node_id, target_parent_id, include_children, name_prefix } => {
            // Facade over the lifted `crate::workflows::duplicate_subtree`
            // — the same orchestration the MCP `duplicate_node` handler
            // uses (BFS ordering, truncated-subtree refusal, name_prefix).
            // The CLI passes the default WorkflowContext (no cancel, no
            // deadline) and discards the footprint (no cache / name index
            // out here).
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let (outcome, _footprint) = workflowy_mcp_server::workflows::duplicate_subtree(
                &client,
                node_id,
                target_parent_id,
                *include_children,
                name_prefix.as_deref(),
                &ctx,
            )
            .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "source_id": outcome.original_id,
                    "new_root_id": outcome.new_root_id,
                    "target_parent_id": target_parent_id,
                    "total_created": outcome.nodes_created,
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
            // Facade over the lifted `crate::workflows::instantiate_template`.
            // This GAINS regex `{{var}}` substitution with unmatched-variable
            // passthrough — the pre-lift CLI used a literal `str::replace`
            // per known key with no passthrough concept; the canonical
            // (MCP) regex form is strictly better (an unknown `{{x}}`
            // survives verbatim, and overlapping keys can't corrupt each
            // other). Same orchestration the MCP handler runs.
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let (outcome, _footprint) = workflowy_mcp_server::workflows::instantiate_template(
                &client,
                template_node_id,
                target_parent_id,
                &substitutions,
                &ctx,
            )
            .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "template_id": outcome.template_id,
                    "new_root_id": outcome.new_root_id,
                    "target_parent_id": target_parent_id,
                    "total_created": outcome.nodes_created,
                    "variables_applied": outcome.variables_applied,
                }))?
            );
        }
        Cmd::BulkUpdate { operation, query, tag, root, status, operation_tag, limit, depth } => {
            let valid_ops = defaults::BULK_UPDATE_VALID_OPS;
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
            // Candidate selection shared with MCP `bulk_update` via
            // `filter_bulk_candidates` (query + whole-tag + status). The
            // CLI applies `--limit` as a cap directly inside the helper.
            let matched = workflowy_mcp_server::utils::aggregation::filter_bulk_candidates(
                &fetch.nodes,
                query.as_deref(),
                tag.as_deref(),
                status.as_str(),
                *limit,
            );
            // Apply step delegated to the shared workflow — same
            // code path the MCP `bulk_update` handler uses.
            let bulk_op = workflowy_mcp_server::workflows::BulkOp::parse(operation)
                .ok_or_else(|| {
                    format!(
                        "bulk-update: unknown operation {:?} (expected delete/complete/uncomplete/add_tag/remove_tag)",
                        operation,
                    )
                })?;
            let owned_matched: Vec<workflowy_mcp_server::types::WorkflowyNode> =
                matched.iter().map(|n| (*n).clone()).collect();
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let (apply_result, _footprint) = workflowy_mcp_server::workflows::apply_bulk_op(
                &client,
                bulk_op,
                &owned_matched,
                operation_tag.as_deref(),
                &ctx,
            )
            .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "operation": bulk_op.as_str(),
                    "matched_count": apply_result.matched_count,
                    "affected_count": apply_result.affected_count,
                }))?
            );
        }
        Cmd::BulkTag { tag, nodes } => {
            let tag_clean = tag.trim_start_matches('#');
            let mut affected = 0usize;
            let mut already_tagged = 0usize;
            for id in nodes {
                let n = match client.get_node(id).await {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                // Whole-tag idempotency via the shared helper — `None` means
                // the node already carries the tag (skip the write). Pre-fix
                // the CLI blindly re-appended the tag, double-tagging on
                // re-runs and shadowing longer tags (`#lead` vs `#leadership`).
                match workflowy_mcp_server::utils::tag_parser::add_tag_to_name(&n.name, tag_clean) {
                    None => already_tagged += 1,
                    Some(new_name) => {
                        if client.edit_node(id, Some(&new_name), None).await.is_ok() {
                            affected += 1;
                        }
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "tag": tag_clean,
                    "node_count": nodes.len(),
                    "affected_count": affected,
                    "already_tagged": already_tagged,
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
        Cmd::ReadBatch { input } => {
            // Surface-parity counterpart to MCP `read_batch`. Dispatches
            // get_node / list_children / get_subtree per op, in input
            // order, with per-op status. No host-encoding hazard on the
            // CLI surface — the value here is wire-shape symmetry with
            // the MCP tool so the same JSON payload runs against either.
            let body = read_input_or_stdin(input.as_deref())?;
            let ops_raw: Vec<serde_json::Value> = serde_json::from_str(&body)
                .map_err(|e| format!("read-batch: input must be a JSON array — {}", e))?;
            if ops_raw.is_empty() {
                return Err("read-batch: operations must not be empty".into());
            }
            use workflowy_mcp_server::defaults::READ_BATCH_VALID_OPS as VALID_OPS;
            let mut succeeded = 0usize;
            let mut failed = 0usize;
            let mut entries: Vec<serde_json::Value> = Vec::with_capacity(ops_raw.len());
            for (idx, raw) in ops_raw.iter().enumerate() {
                let op_kind = raw["op"].as_str()
                    .ok_or_else(|| format!("read-batch: operations[{}] missing string `op`", idx))?
                    .to_string();
                if !VALID_OPS.contains(&op_kind.as_str()) {
                    return Err(format!(
                        "read-batch: operations[{}].op = {:?}; expected one of {:?}",
                        idx, op_kind, VALID_OPS
                    ).into());
                }
                let node_id = raw["node_id"].as_str().map(String::from);
                let max_depth = raw["max_depth"].as_u64().map(|d| d as usize);
                if matches!(op_kind.as_str(), "get_node" | "get_subtree") && node_id.is_none() {
                    return Err(format!(
                        "read-batch: operations[{}].node_id required for op={}",
                        idx, op_kind
                    ).into());
                }
                let result: std::result::Result<serde_json::Value, String> = match op_kind.as_str() {
                    "get_node" => {
                        let id = node_id.as_deref().expect("validated");
                        match client.get_node(id).await {
                            Ok(n) => Ok(json!({"node": n})),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    "list_children" => {
                        let res = match node_id.as_deref() {
                            Some(i) => client.get_children(i).await,
                            None => client.get_top_level_nodes().await,
                        };
                        match res {
                            Ok(children) => Ok(json!({"children": children})),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    "get_subtree" => {
                        let id = node_id.as_deref().expect("validated");
                        let depth = max_depth.unwrap_or(5);
                        let controls = FetchControls::with_timeout(std::time::Duration::from_millis(
                            defaults::SUBTREE_FETCH_TIMEOUT_MS,
                        ));
                        match client
                            .get_subtree_with_controls(Some(id), depth, defaults::MAX_SUBTREE_NODES, controls)
                            .await
                        {
                            Ok(fetch) => Ok(json!({
                                "nodes": fetch.nodes,
                                "truncated": fetch.truncated,
                                "truncation_limit": fetch.limit,
                                "truncation_reason": fetch.truncation_reason.map(|r| r.as_str()),
                            })),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    _ => unreachable!("validated above"),
                };
                let entry = match result {
                    Ok(data) => {
                        succeeded += 1;
                        json!({
                            "index": idx,
                            "op": op_kind,
                            "node_id": node_id,
                            "ok": true,
                            "data": data,
                        })
                    }
                    Err(e) => {
                        failed += 1;
                        json!({
                            "index": idx,
                            "op": op_kind,
                            "node_id": node_id,
                            "ok": false,
                            "error": e,
                        })
                    }
                };
                entries.push(entry);
            }
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
            // Both sequential apply and best-effort rollback live in
            // `crate::workflows::run_transaction` — same code path the
            // MCP `transaction` tool uses. The CLI just parses the
            // input JSON into TxnOps and projects the typed outcome
            // into stdout.
            let body = read_input_or_stdin(input.as_deref())?;
            let raw_ops: Vec<serde_json::Value> = serde_json::from_str(&body)
                .map_err(|e| format!("transaction: input must be a JSON array — {}", e))?;
            let mut ops: Vec<workflowy_mcp_server::workflows::TxnOp> =
                Vec::with_capacity(raw_ops.len());
            for raw in &raw_ops {
                let kind_str = raw["op"].as_str().ok_or("transaction: each op needs `op`")?;
                ops.push(workflowy_mcp_server::workflows::TxnOp {
                    op: kind_str.to_string(),
                    node_id: raw["node_id"].as_str().map(str::to_string),
                    parent_id: raw["parent_id"].as_str().map(str::to_string),
                    new_parent_id: raw["new_parent_id"].as_str().map(str::to_string),
                    name: raw["name"].as_str().map(str::to_string),
                    description: raw["description"].as_str().map(str::to_string),
                    priority: raw["priority"].as_i64().map(|p| p as i32),
                    expect_name: raw["expect_name"].as_str().map(str::to_string),
                });
            }
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let (outcome, _footprint) =
                workflowy_mcp_server::workflows::run_transaction(&client, ops, &ctx).await?;
            // Both `Applied` and `RolledBack` variants serialise via
            // serde with a `status` discriminator. Same shape as the
            // MCP envelope so callers piping JSON between the two
            // surfaces don't need a translation layer.
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::to_value(&outcome)?)?
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
                    "note": "wflow-do is single-shot; cancel-all is a no-op against the local client. Use against the running Workflowy MCP server when you need to preempt in-flight tree walks.",
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
                // Impatient: this mirrors the MCP tool, whose caller is
                // waiting. Patience belongs to the scheduled reindex.
                false,
                // Scoped walk, not a full export: build_name_index targets a
                // single root by design.
                false,
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
        Cmd::AuditMirrors { root, chunked, cross_scope_resolve } => {
            // Walk orchestration routed through the shared workflow
            // so the CLI and the MCP `audit_mirrors` tool cannot
            // drift — see `crate::workflows::audit_mirrors_walk`.
            let review_root = default_review_root();
            let scope = root.as_deref().or(review_root.as_deref()).ok_or_else(|| {
                anyhow::anyhow!(
                    "no --root provided and WORKFLOWY_REVIEW_ROOT not set; \
                     pass --root or set the env var to your review-anchor node"
                )
            })?;
            let do_chunked = chunked.unwrap_or(root.is_none());
            let do_cross_resolve = cross_scope_resolve.unwrap_or(true);
            // Use the same depth budget the MCP handler defaults to
            // (8). Pre-2026-05-16 the CLI hardcoded 7 for child
            // walks; both surfaces now flow through the workflow's
            // `saturating_sub(1)` decrement so child_depth is 7 by
            // construction when max_depth = 8 — same result, single
            // definition.
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let outcome = workflowy_mcp_server::workflows::audit_mirrors_walk(
                &client, scope, 8, do_chunked, &ctx,
            )
            .await?;
            let all_nodes = outcome.nodes;
            let chunks_json = outcome.chunks;
            let truncated_any = outcome.truncated;
            let top_truncation = outcome.truncation_reason;

            // External-canonical resolution stays per-surface: the
            // CLI is single-shot and has no persistent index, so it
            // issues live `get_node` calls (rate limiter inside the
            // client serialises). Both surfaces share the unresolved-
            // target extraction.
            let mut external: std::collections::HashMap<String, workflowy_mcp_server::audit::ExternalCanonical> =
                std::collections::HashMap::new();
            if do_cross_resolve {
                let targets =
                    workflowy_mcp_server::workflows::extract_unresolved_mirror_targets(&all_nodes);
                for t in targets {
                    if let Ok(canon) = client.get_node(&t).await {
                        let canon_desc = canon.description.as_deref().unwrap_or("");
                        let has_marker = workflowy_mcp_server::audit::extract_marker(
                            canon_desc,
                            "canonical_of:",
                        )
                        .is_some();
                        external.insert(
                            t,
                            workflowy_mcp_server::audit::ExternalCanonical {
                                id: canon.id,
                                name: canon.name,
                                has_canonical_marker: Some(has_marker),
                            },
                        );
                    }
                }
            }

            let findings = workflowy_mcp_server::audit::audit_mirrors_with_external(
                &all_nodes, &external,
            );
            if cli.json {
                let mut payload = serde_json::Map::new();
                payload.insert("scope".into(), json!(scope));
                payload.insert("scanned".into(), json!(all_nodes.len()));
                payload.insert("truncated".into(), json!(truncated_any));
                payload.insert("truncation_reason".into(), json!(top_truncation));
                payload.insert("chunked".into(), json!(do_chunked));
                payload.insert("cross_scope_resolve".into(), json!(do_cross_resolve));
                payload.insert(
                    "external_canonicals_resolved".into(),
                    json!(external.len()),
                );
                if do_chunked {
                    payload.insert("chunks".into(), json!(chunks_json));
                }
                payload.insert(
                    "findings".into(),
                    serde_json::to_value(&findings).unwrap(),
                );
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::Value::Object(payload))?
                );
            } else if findings.is_empty() {
                println!(
                    "audit-mirrors: scanned {} nodes ({} external canonicals resolved), no findings",
                    all_nodes.len(),
                    external.len()
                );
            } else {
                for f in &findings {
                    println!("{} {} \"{}\" -> {}", f.status, f.node_id, f.name, f.issue);
                }
                println!(
                    "---\n{} findings across {} nodes ({} external canonicals resolved)",
                    findings.len(),
                    all_nodes.len(),
                    external.len()
                );
            }
        }
        Cmd::CreateMirror { canonical_node_id, target_parent_id, priority, pillar, dry_run } => {
            // The orchestration lives in `crate::workflows` so this
            // CLI and the MCP `create_mirror` tool share a single
            // source of truth. The CLI just translates the typed
            // result into stdout.
            //
            // `--dry-run` delegates to `create_mirror_dry_run` so the
            // CLI and the MCP tool's `dry_run=true` path resolve
            // through the same code. The CLI wraps the typed result
            // in stdout output; the MCP wraps it in a JSON envelope
            // with `scope_resolved`. Single source of truth, no
            // duplicate orchestration.
            if *dry_run {
                let preview = workflowy_mcp_server::workflows::create_mirror_dry_run(
                    &client,
                    canonical_node_id,
                    target_parent_id.as_deref(),
                    pillar.as_deref(),
                )
                .await?;
                let scope_resolved = workflowy_mcp_server::workflows::scope_resolved_label(
                    preview.target_parent_id.as_deref(),
                );
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "status": "dry_run",
                            "scope_resolved": scope_resolved,
                            "canonical_id": preview.canonical_id,
                            "target_parent_id": preview.target_parent_id,
                            "mirror_name": preview.mirror_name,
                            "canonical_already_marked": preview.canonical_already_marked,
                            "would_annotate_canonical": preview.would_annotate_canonical,
                            "pillar": preview.pillar,
                        }))?
                    );
                } else {
                    println!(
                        "dry_run: would create mirror named \"{}\" under {} (scope_resolved={}, would_annotate_canonical={})",
                        preview.mirror_name,
                        preview.target_parent_id.as_deref().unwrap_or("workspace root"),
                        scope_resolved,
                        preview.would_annotate_canonical,
                    );
                }
                return Ok(());
            }
            let ctx = workflowy_mcp_server::workflows::WorkflowContext::default();
            let (result, _footprint) = workflowy_mcp_server::workflows::create_mirror_via_convention(
                &client,
                canonical_node_id,
                target_parent_id.as_deref(),
                *priority,
                pillar.as_deref(),
                &ctx,
            )
            .await?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "mirror_id": result.mirror_id,
                        "canonical_id": result.canonical_id,
                        "target_parent_id": result.target_parent_id,
                        "name": result.name,
                        "audit_status": result.audit_status,
                        "annotated_canonical": result.annotated_canonical,
                    }))?
                );
            } else {
                println!(
                    "Created mirror {} of canonical {} under {} (audit_status={})",
                    result.mirror_id,
                    result.canonical_id,
                    result.target_parent_id.as_deref().unwrap_or("workspace root"),
                    result.audit_status,
                );
                println!("{}", result.mirror_id);
            }
        }
        Cmd::Review { root, days_stale } => {
            let review_root = default_review_root();
            let scope = root.as_deref().or(review_root.as_deref()).ok_or_else(|| {
                anyhow::anyhow!(
                    "no --root provided and WORKFLOWY_REVIEW_ROOT not set; \
                     pass --root or set the env var to your review-anchor node"
                )
            })?;
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
            // When --out is given we honour it verbatim. Otherwise we derive the
            // default under $SECONDBRAIN_DIR — but route through the *checked*
            // accessor so a stale/missing SECONDBRAIN_DIR fails loud (naming the
            // resolved path) instead of silently writing INDEX.md to a directory
            // nobody provisioned (2026-06-02 relocation hazard).
            let derived: Option<String> = match out.as_deref() {
                Some(_) => None,
                None => workflowy_mcp_server::defaults::secondbrain_dir_checked()?
                    .map(|root| {
                        root.join("session-logs")
                            .join("INDEX.md")
                            .to_string_lossy()
                            .into_owned()
                    }),
            };
            let target = match (out.as_deref(), derived.as_deref()) {
                (Some(o), _) => o,
                (None, Some(d)) => d,
                (None, None) => {
                    return Err(
                        "no output path: pass --out or set SECONDBRAIN_DIR".into(),
                    );
                }
            };
            let dir = std::path::Path::new(target).parent().ok_or("invalid out path")?;
            let entries = scan_session_logs(dir)?;
            let body = render_index(&entries);
            std::fs::write(target, &body)?;
            println!("index: wrote {} entries to {}", entries.len(), target);
        }
        Cmd::Reindex { roots, index_path, max_depth, timeout_secs, patient, full_export } => {
            cmd_reindex(
                client,
                roots.as_slice(),
                index_path.as_deref(),
                *max_depth,
                *timeout_secs,
                *patient,
                *full_export,
            )
            .await?;
        }
        Cmd::ChangedSince { since, root, limit } => {
            // Normalise to epoch SECONDS — the unit the index stores
            // `last_modified` in. Comparing a ms cutoff against second-scale
            // stored values matched nothing (fixed 2026-07-22).
            let since_secs: i64 =
                workflowy_mcp_server::utils::date_parser::epoch_input_to_secs(since)?;
            let index = load_persistent_index()?;
            let mut hits = index.entries_modified_since(since_secs);
            if let Some(r) = root.as_deref() {
                hits.retain(|e| index.is_descendant_of(&e.node_id, r));
            }
            hits.sort_by_key(|e| std::cmp::Reverse(e.last_modified));
            let total = hits.len();
            hits.truncate(*limit);
            let payload = json!({
                "since_secs": since_secs,
                "root": root,
                "served_from": "name_index",
                "name_index_size": index.size(),
                "total_matches": total,
                "count": hits.len(),
                "nodes": hits.iter().map(|e| json!({
                    "id": e.node_id,
                    "name": e.name,
                    "parent_id": e.parent_id,
                    "last_modified": e.last_modified,
                })).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        Cmd::NativeMirrorCreate { canonical_node_id, target_parent_id, position } => {
            let resp = client
                .create_native_mirror(canonical_node_id, target_parent_id, position)
                .await?;
            let payload = json!({
                "mode": "native",
                "item_id": resp.item_id,
                "origin_id": resp.origin_id,
                "position": position,
                "caveat": "Native mirror created on the BETA API. On the production \
                           account it renders as an empty-named node with no mirror \
                           metadata until Workflowy ships mirrors to production. This is \
                           a distinct mechanism from the `create-mirror` note convention \
                           that `audit-mirrors` tracks.",
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
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
    patient: bool,
    full_export: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use workflowy_mcp_server::utils::NameIndex;

    let index = NameIndex::new();
    let path = match index_path_override {
        Some(s) if s.is_empty() => None,
        Some(s) => Some(std::path::PathBuf::from(s)),
        None => {
            // Match the env-var path the MCP server uses so a CLI
            // reindex and a server-side walk agree on the persisted
            // file. Unset / empty env means persistence is disabled —
            // the CLI keeps the index in memory for this run only.
            std::env::var(defaults::INDEX_PATH_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(std::path::PathBuf::from)
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

    // Fast path: one bulk `GET /nodes-export` covers the entire tree, so it
    // replaces the whole root-walk loop below — no truncation, no dropped
    // branches, no per-root checkpointing. The exclusion filter still runs
    // at the save boundary (`snapshot()`), so ingesting the whole tree in
    // memory and then saving yields a disk index that omits the excluded
    // subtrees exactly as the scoped walk did. `--root`/`--max-depth`/
    // `--timeout-secs`/`--patient` are meaningless here and ignored.
    if full_export {
        if !roots.is_empty() || patient {
            eprintln!(
                "reindex: --full-export covers the whole tree; ignoring --root/--patient/--timeout-secs"
            );
        }
        let started = std::time::Instant::now();
        println!("reindex: full export via GET /nodes-export (one call for the whole tree)…");
        let nodes = client.export_all().await.map_err(|e| {
            format!("full export failed: {e} (Workflowy throttles /nodes-export; wait ~65 s and retry)")
        })?;
        let fetched = nodes.len();
        index.ingest(&nodes);
        println!(
            "reindex: exported {} nodes in {} ms; index holds {} entries (pre-exclusion, in memory)",
            fetched,
            started.elapsed().as_millis(),
            index.size()
        );
        if path.is_some() {
            index.save_to_disk()?;
            println!(
                "reindex: saved to {} (excluded subtrees filtered at save; total elapsed {} ms)",
                path.as_ref().unwrap().display(),
                started.elapsed().as_millis()
            );
        }
        if fetched == 0 {
            eprintln!("reindex: WARNING export returned 0 nodes — index left unchanged on disk");
        }
        return Ok(());
    }

    // Walk each root in turn. Empty roots = walk from workspace root.
    let walk_targets: Vec<Option<&str>> = if roots.is_empty() {
        vec![None]
    } else {
        roots.iter().map(|r| Some(r.as_str())).collect()
    };

    let started_total = std::time::Instant::now();
    let mut skipped_total: Vec<String> = Vec::new();
    if timeout_secs == 0 {
        println!(
            "reindex: per-root timeout disabled (walks bound only by node cap = {})",
            defaults::RESOLVE_WALK_NODE_CAP
        );
    } else {
        println!("reindex: per-root timeout = {} s", timeout_secs);
    }
    if patient {
        println!(
            "reindex: patient mode — waiting out rate-limit windows and re-attempting \
             dropped branches until they stop recovering"
        );
        if timeout_secs != 0 {
            // Not fatal, but it undercuts the flag: the retry declines to
            // wait when the wait would outlast the deadline, and then the
            // branches are dropped exactly as they would be without it.
            eprintln!(
                "reindex: WARNING --patient with --timeout-secs {timeout_secs} — a rate-limit \
                 window that outlasts the remaining budget makes the walk skip the wait and \
                 drop the branches anyway. Use --timeout-secs 0 for a complete walk."
            );
        }
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
        let controls = if patient { controls.patient() } else { controls };
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
                // `complete` must mean "covered the subtree", not merely "did
                // not time out" — a walk that dropped branches reports them
                // here rather than claiming completion (2026-07-16).
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
                // Checkpoint after EVERY root, not once at the end
                // (2026-07-21): a multi-hour patient run that dies on root
                // 9 of 13 must keep roots 1-8. save_to_disk merges under
                // the cross-process lock, so incremental saves compose
                // exactly like the final one; a failed checkpoint is
                // reported but does not abort the walk (the final save
                // retries it with everything accumulated).
                if index.save_path().is_some() {
                    match index.save_to_disk() {
                        Ok(()) => println!(
                            "reindex: checkpointed {} entries after {}",
                            index.size(),
                            label
                        ),
                        Err(e) => eprintln!("reindex: checkpoint after {} failed: {}", label, e),
                    }
                }
                if !fetch.skipped_branches.is_empty() {
                    skipped_total.extend(fetch.skipped_branches.iter().cloned());
                    eprintln!(
                        "reindex: WARNING {} dropped {} branch(es) after retry — their subtrees are NOT indexed: {}",
                        label,
                        fetch.skipped_branches.len(),
                        fetch.skipped_branches.join(", ")
                    );
                }
            }
            Err(e) => {
                eprintln!("reindex: {} failed: {}", label, e);
            }
        }
    }

    if path.is_some() {
        // Merges the on-disk file back in first, so entries written by a
        // running MCP server since our load survive this save rather than
        // being clobbered by it.
        index.save_to_disk()?;
        println!(
            "reindex: saved {} entries to {} (total elapsed {} ms)",
            index.size(),
            path.as_ref().unwrap().display(),
            started_total.elapsed().as_millis()
        );
    }

    if !skipped_total.is_empty() {
        skipped_total.sort();
        skipped_total.dedup();
        eprintln!(
            "reindex: COVERAGE IS PARTIAL — {} branch(es) were dropped and their subtrees are absent from the index. \
             Re-run once upstream rate-limit pressure clears; the merge-on-save means a re-run adds to the index rather than replacing it. \
             Dropped branches: {}",
            skipped_total.len(),
            skipped_total.join(", ")
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

use workflowy_mcp_server::audit::{build_review, ReviewReport};
#[cfg(test)]
use workflowy_mcp_server::audit::audit_mirrors;
use workflowy_mcp_server::defaults::SECONDS_PER_DAY;

/// Read recent session-log files (last 7 days) into a single blob the
/// review function can scan for URL/DOI matches. Returns `""` if
/// `$SECONDBRAIN_DIR` is unset or its `session-logs` subdirectory
/// doesn't exist — the review function then skips bucket (d) gracefully.
fn load_recent_session_logs_blob() -> String {
    let Some(dir) = workflowy_mcp_server::defaults::session_logs_dir() else {
        return String::new();
    };
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

/// Proximate-cause string for the CLI's stderr line. Routes through the SAME
/// classifier the MCP layer uses (`ProximateCause::from_error_message` in
/// `utils::error_class`) so the CLI label cannot drift from the server's. The
/// pre-2026-06-24 local copy had already drifted: it lacked the `rate_limited`
/// (429) branch the server gained 2026-06-17, so a rate-limited CLI error
/// printed `unknown`. Sharing the classifier closes that gap by construction.
fn classify(err: &str) -> &'static str {
    workflowy_mcp_server::utils::error_class::ProximateCause::from_error_message(err).as_str()
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

// Until 2026-05-04 the CLI carried its own `apply_txn_step` and
// `run_inverse_step` helpers — JSON-blob equivalents of the server's
// `apply_txn_op` and `run_inverse`. Both are now collapsed into
// `crate::workflows::run_transaction`, which both surfaces call.

// Through 2026-05-09 the CLI carried its own copy of
// `render_subtree_markdown` / `render_subtree_opml` alongside parallel
// implementations in `server/mod.rs`. The duplication-audit lift moved
// the canonical renderers into `utils::subtree::{render_subtree_markdown,
// render_subtree_opml}` so the MCP `export_subtree` tool and the
// `wflow-do export` CLI subcommand call the same code. Re-exported
// here so the local `Cmd::Export` arm can reach the renderer with
// the same name.
use workflowy_mcp_server::utils::subtree::{render_subtree_markdown, render_subtree_opml};

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
            "status", "get", "children", "create", "move", "reorder", "delete", "edit",
            "complete", "search", "tag-search", "find", "subtree",
            "backlinks", "find-by-tag-and-path", "node-at-path", "path-of",
            "resolve-link", "since",
            // Todos / scheduling
            "todos", "overdue", "upcoming", "daily-review",
            // Project / activity
            "recent-changes", "project-summary",
            // Bulk writes
            "insert", "smart-insert", "duplicate", "template",
            "bulk-update", "bulk-tag", "batch-create", "read-batch", "transaction", "export",
            // Diagnostics
            "health-check", "cancel-all", "build-name-index", "recent-tools",
            // Existing diagnostics + index
            "audit-mirrors", "create-mirror", "review", "index", "reindex",
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
        // Every non-diagnostic MCP tool has a CLI counterpart.
        // `convert_markdown` is a pure local transform with no API and is
        // intentionally excluded. (`create_mirror` was a stub through
        // 2026-05-04; the failure-report follow-up replaced it with a
        // real implementation, and the `create-mirror` CLI subcommand
        // landed in the same commit.) Diagnostics that exist only
        // in-process on the MCP server (`recent-tools`, `cancel-all`)
        // ship as no-op CLI wrappers so the surface is uniform.
        let expected_pairs: &[(&str, &str)] = &[
            ("get_node", "get"),
            ("list_children", "children"),
            ("create_node", "create"),
            ("edit_node", "edit"),
            ("delete_node", "delete"),
            ("move_node", "move"),
            ("reorder_nodes", "reorder"),
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
            ("read_batch", "read-batch"),
            ("transaction", "transaction"),
            ("export_subtree", "export"),
            ("audit_mirrors", "audit-mirrors"),
            ("create_mirror", "create-mirror"),
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
        // Tie the pair list to the single-source non-diagnostic catalogue so the
        // two cannot drift: every non-diagnostic tool must appear here AND a new
        // non-diagnostic tool added to the catalogue must land its CLI pair.
        // `convert_markdown` is the one documented exclusion — a pure local
        // transform with no API surface, so it has no CLI subcommand (it is still
        // a non-diagnostic tool the skill's allowed-tools must list).
        for tool in workflowy_mcp_server::defaults::NON_DIAGNOSTIC_MCP_TOOLS {
            if *tool == "convert_markdown" {
                continue;
            }
            assert!(
                expected_pairs.iter().any(|(m, _)| m == tool),
                "non-diagnostic tool `{}` (defaults::NON_DIAGNOSTIC_MCP_TOOLS) has no \
                 CLI parity pair in this test",
                tool,
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
    fn reorder_parses_parent_and_repeated_node_flags() {
        let parsed = Cli::try_parse_from([
            "wflow-do",
            "reorder",
            "--parent",
            "parent-uuid",
            "--node",
            "id-1",
            "--node",
            "id-2",
            "--node",
            "id-3",
        ])
        .expect("reorder with --parent and three --node flags parses");
        match parsed.cmd {
            Cmd::Reorder { parent_id, nodes } => {
                assert_eq!(parent_id, "parent-uuid");
                assert_eq!(nodes, vec!["id-1", "id-2", "id-3"]);
            }
            _ => panic!("expected Reorder"),
        }
    }

    #[test]
    fn reorder_dry_run_emits_count() {
        let cli = Cli::try_parse_from([
            "wflow-do",
            "--dry-run",
            "reorder",
            "--parent",
            "parent-uuid",
            "--node",
            "a",
            "--node",
            "b",
        ])
        .expect("dry-run reorder parses");
        let line = dry_run_line(&cli.cmd).expect("reorder yields a dry-run line");
        assert!(line.starts_with("DRY-RUN reorder"), "got: {}", line);
        assert!(line.contains("count=2"), "got: {}", line);
        assert!(line.contains("parent_id=parent-uuid"), "got: {}", line);
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

    /// CLI smoke test for the shared renderer. The 2026-05-09
    /// duplication-audit lift moved `render_subtree_markdown` from the
    /// CLI's local module to `utils::subtree`; this test asserts that
    /// `Cmd::Export` continues to reach the same output through the
    /// re-export, matching the format the server-side test pins.
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
        // Description renders with 4-space indent (the unified format
        // the server-side renderer has shipped); the legacy CLI `> `
        // prefix was the outlier and went away with the lift.
        assert!(body.contains("a desc"));
        assert!(body.contains("    - Grandchild"));
    }

    /// Same lift smoke test for the OPML renderer. The unified header
    /// includes `encoding="UTF-8"` (the server-side renderer's; the
    /// CLI's previous header omitted it). Asserts the encoding +
    /// XML-character escaping over the shared implementation.
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
        // Now routes through the shared `ProximateCause::from_error_message`,
        // so the CLI gains the `rate_limited` (429) branch it used to lack.
        assert_eq!(classify("HTTP 429 too many requests"), "rate_limited");
        assert_eq!(classify("rate limit exceeded"), "rate_limited");
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
