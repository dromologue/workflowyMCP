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
        Cmd::Search { .. } => "search",
        Cmd::AuditMirrors { .. } => "audit-mirrors",
        Cmd::Review { .. } => "review",
        Cmd::Index { .. } => "index",
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
            let report = build_review(&fetch.nodes, *days_stale, chrono::Utc::now().timestamp());
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
    }
    Ok(())
}

// --- audit-mirrors helpers ---

#[derive(Debug, Clone, serde::Serialize)]
struct MirrorFinding {
    status: String,
    node_id: String,
    name: String,
    issue: String,
}

/// Extract the first UUID-shaped capture for the given marker prefix in `text`.
/// Returns `None` if no marker matches. UUIDs may be the full 36-char form or
/// the 12-char short hash (matches Workflowy URL format).
fn extract_marker(text: &str, prefix: &str) -> Option<String> {
    // Accept full UUIDs (36 chars), 12-char short hashes, and shorter
    // synthetic IDs used in tests. Lower bound at 3 so tests with `aaa`/`bbb`
    // markers still parse; production IDs are always longer.
    let pattern = format!(r"(?i){}\s*([0-9a-f-]{{3,40}})", regex::escape(prefix));
    let re = regex::Regex::new(&pattern).ok()?;
    re.captures(text).map(|c| c[1].to_lowercase())
}

fn audit_mirrors(nodes: &[workflowy_mcp_server::types::WorkflowyNode]) -> Vec<MirrorFinding> {
    use std::collections::{HashMap, HashSet};
    let id_match = |a: &str, b: &str| -> bool {
        let (a, b) = (a.to_lowercase(), b.to_lowercase());
        a == b || a.ends_with(&b) || b.ends_with(&a)
    };
    let mk = |status: &str, n: &workflowy_mcp_server::types::WorkflowyNode, issue: String| MirrorFinding {
        status: status.into(), node_id: n.id.clone(), name: n.name.clone(), issue,
    };
    let by_id: HashMap<&str, &workflowy_mcp_server::types::WorkflowyNode> =
        nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut canonical_targets: HashSet<String> = HashSet::new();
    let mut mirrors_by_target: HashMap<String, Vec<String>> = HashMap::new();
    let mut findings = Vec::new();
    for n in nodes {
        let desc = n.description.as_deref().unwrap_or("");
        if extract_marker(desc, "canonical_of:").is_some() {
            canonical_targets.insert(n.id.to_lowercase());
        }
        if let Some(target) = extract_marker(desc, "mirror_of:") {
            mirrors_by_target.entry(target.clone()).or_default().push(n.id.clone());
            let canon = by_id.values().find(|c| id_match(&c.id, &target));
            match canon {
                None => findings.push(mk("BROKEN", n, format!("mirror_of:{} not found in scope", target))),
                Some(canon) => {
                    let (mn, cn) = (n.name.to_lowercase(), canon.name.to_lowercase());
                    if !mn.contains(&cn) && !cn.contains(&mn) {
                        findings.push(mk("DRIFTED", n, format!("name diverges from canonical \"{}\"", canon.name)));
                    }
                    let canon_desc = canon.description.as_deref().unwrap_or("");
                    if extract_marker(canon_desc, "canonical_of:").is_none() {
                        findings.push(mk("ORPHAN", n, format!("canonical {} has no canonical_of: marker", canon.id)));
                    }
                }
            }
        }
    }
    for cid in &canonical_targets {
        if !mirrors_by_target.keys().any(|t| id_match(t, cid)) {
            if let Some(canon) = nodes.iter().find(|n| &n.id.to_lowercase() == cid) {
                findings.push(mk("LONELY", canon, "canonical has no mirrors (may be intentional)".into()));
            }
        }
    }
    findings
}

// --- review helpers ---

#[derive(Debug, Clone, serde::Serialize)]
struct ReviewReport {
    revisit_due: Vec<ReviewItem>,
    multi_pillar: Vec<ReviewItem>,
    stale_cross_pillar: Vec<ReviewItem>,
    source_moc_reuse: Vec<ReviewItem>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReviewItem {
    node_id: String,
    name: String,
    detail: String,
}

const PILLAR_TAGS: &[&str] = &["#leadership", "#ethics", "#building", "#learning", "#decide"];
const SECONDS_PER_DAY: i64 = 86_400;

fn build_review(
    nodes: &[workflowy_mcp_server::types::WorkflowyNode],
    days_stale: i64,
    now_unix: i64,
) -> ReviewReport {
    let stale_cutoff = now_unix - days_stale * SECONDS_PER_DAY;
    let mut report = ReviewReport {
        revisit_due: vec![],
        multi_pillar: vec![],
        stale_cross_pillar: vec![],
        source_moc_reuse: vec![],
    };
    let date_re = regex::Regex::new(r"revisit_due:\s*(\d{4}-\d{2}-\d{2})").unwrap();
    let today = chrono::Utc::now().date_naive();
    for n in nodes {
        let desc = n.description.as_deref().unwrap_or("");
        let combined = format!("{} {}", n.name, desc).to_lowercase();
        // (a) revisit-due past today
        if combined.contains("#revisit") {
            if let Some(cap) = date_re.captures(desc) {
                if let Ok(d) = chrono::NaiveDate::parse_from_str(&cap[1], "%Y-%m-%d") {
                    if d < today {
                        report.revisit_due.push(ReviewItem {
                            node_id: n.id.clone(),
                            name: n.name.clone(),
                            detail: format!("revisit_due:{} (past today)", &cap[1]),
                        });
                    }
                }
            }
        }
        // (b) multi-pillar: count mirror_of markers OR distinct pillar tags
        let mirror_of_count = desc.matches("mirror_of:").count() + desc.matches("#mirrored_in:").count();
        let pillar_count = PILLAR_TAGS.iter().filter(|t| combined.contains(*t)).count();
        let max_signal = mirror_of_count.max(pillar_count);
        if max_signal >= 3 {
            report.multi_pillar.push(ReviewItem {
                node_id: n.id.clone(),
                name: n.name.clone(),
                detail: format!("signal={} (mirror_of={}, pillars={})", max_signal, mirror_of_count, pillar_count),
            });
        }
        // (c) stale cross-pillar concept maps
        let is_cross_pillar = pillar_count >= 2 || combined.contains("cross-pillar") || combined.contains("concept map");
        if is_cross_pillar {
            if let Some(lm) = n.last_modified {
                if lm < stale_cutoff {
                    let days = (now_unix - lm) / SECONDS_PER_DAY;
                    report.stale_cross_pillar.push(ReviewItem {
                        node_id: n.id.clone(),
                        name: n.name.clone(),
                        detail: format!("last_modified {} days ago", days),
                    });
                }
            }
        }
    }
    // (d) Source-MOC reuse: scan recent session-logs for URL/DOI references in source MOC descriptions.
    report.source_moc_reuse = source_moc_reuse(nodes).unwrap_or_default();
    report
}

fn source_moc_reuse(nodes: &[workflowy_mcp_server::types::WorkflowyNode]) -> Option<Vec<ReviewItem>> {
    let logs_dir = format!(
        "{}/code/SecondBrain/session-logs",
        std::env::var("HOME").ok()?
    );
    let dir = std::path::Path::new(&logs_dir);
    if !dir.exists() {
        return None;
    }
    let cutoff = chrono::Utc::now().timestamp() - 7 * SECONDS_PER_DAY;
    let mut recent_blob = String::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for ent in read.flatten() {
            if let Ok(meta) = ent.metadata() {
                let mtime = meta.modified().ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if mtime >= cutoff {
                    if let Ok(c) = std::fs::read_to_string(ent.path()) {
                        recent_blob.push_str(&c);
                        recent_blob.push('\n');
                    }
                }
            }
        }
    }
    let url_re = regex::Regex::new(r"https?://\S+|10\.\d{4,9}/\S+").unwrap();
    let mut hits = Vec::new();
    for n in nodes {
        let name_lower = n.name.to_lowercase();
        let looks_like_source_moc = name_lower.contains(" — ")
            || regex::Regex::new(r"\b(19|20)\d{2}\b").unwrap().is_match(&n.name);
        if !looks_like_source_moc {
            continue;
        }
        let desc = n.description.as_deref().unwrap_or("");
        for m in url_re.find_iter(desc) {
            if recent_blob.contains(m.as_str()) {
                hits.push(ReviewItem {
                    node_id: n.id.clone(),
                    name: n.name.clone(),
                    detail: format!("re-cited recently: {}", m.as_str()),
                });
                break;
            }
        }
    }
    Some(hits)
}

fn print_review(r: &ReviewReport) {
    let groups: [(&str, &Vec<ReviewItem>); 4] = [
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
        for sub in [
            "status", "get", "children", "create", "move", "delete", "edit",
            "search", "audit-mirrors", "review", "index",
        ] {
            assert!(help.contains(sub), "help missing subcommand: {sub}\n{help}");
        }
        assert!(help.contains("--dry-run"), "help missing --dry-run global flag\n{help}");
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
    fn audit_parses_mirror_of_marker_from_description() {
        // Mirror name must contain (or be contained by) the canonical name —
        // that's the convention the audit enforces.
        let nodes = vec![
            node("aaa", "Concept X", Some("canonical_of: bbb"), None),
            node("bbb", "Concept X", Some("mirror_of: aaa"), None),
        ];
        let findings = audit_mirrors(&nodes);
        // Both ends well-marked, names align; should report no DRIFTED/BROKEN/ORPHAN/LONELY.
        assert!(findings.is_empty(), "expected clean audit, got {:?}", findings);

        let drifted = vec![
            node("aaa", "Canonical X", Some("canonical_of: bbb"), None),
            node("bbb", "Totally Different Name", Some("mirror_of: aaa"), None),
        ];
        let f2 = audit_mirrors(&drifted);
        assert!(f2.iter().any(|f| f.status == "DRIFTED"), "expected DRIFTED finding, got {:?}", f2);

        let broken = vec![node("bbb", "Lonely Mirror", Some("mirror_of: 99999999"), None)];
        let f3 = audit_mirrors(&broken);
        assert!(f3.iter().any(|f| f.status == "BROKEN"), "expected BROKEN finding, got {:?}", f3);
    }

    #[test]
    fn review_buckets_a_revisit_node_correctly() {
        let now = chrono::Utc::now().timestamp();
        let nodes = vec![
            node("a", "Past-due note #revisit", Some("revisit_due: 2020-01-01"), Some(now)),
            node("b", "Cross-pillar map #leadership #ethics #building", Some("mirror_of: x\nmirror_of: y\nmirror_of: z"), Some(now - 200 * SECONDS_PER_DAY)),
            node("c", "Plain note", None, Some(now)),
        ];
        let r = build_review(&nodes, 90, now);
        assert_eq!(r.revisit_due.len(), 1, "expected 1 revisit-due, got {:?}", r.revisit_due);
        assert!(r.multi_pillar.iter().any(|i| i.node_id == "b"), "expected multi-pillar hit on b");
        assert!(r.stale_cross_pillar.iter().any(|i| i.node_id == "b"), "expected stale cross-pillar hit on b");
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
