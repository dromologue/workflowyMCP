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

#[derive(Parser)]
#[command(name = "wflow-do", about = "Workflowy CLI — bypasses the MCP transport for direct API access")]
struct Cli {
    /// Emit raw JSON for every command (default: human-readable for create/move/delete/edit, JSON for the rest).
    #[arg(long, global = true)]
    json: bool,

    /// Suppress info-level logging.
    #[arg(long, global = true)]
    quiet: bool,

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
    }
    Ok(())
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

    #[test]
    fn cli_help_lists_every_subcommand() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        for sub in ["status", "get", "children", "create", "move", "delete", "edit", "search"] {
            assert!(help.contains(sub), "help missing subcommand: {sub}\n{help}");
        }
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
}
