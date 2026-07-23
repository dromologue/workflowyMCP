//! Best-effort durable usage log for cross-surface comparison.
//!
//! WHY (2026-07-22): the official WorkFlowy desktop-app MCP (`workflowy-desktop`)
//! arrived as a third surface onto the same tree. To decide, with data rather
//! than vibes, whether/when this REST-backed server can be retired for a given
//! kind of work, we record what this surface actually does over time: every
//! tool call appends one JSONL line to `<WORKFLOWY_USAGE_LOG_DIR>/<date>.jsonl`
//! with `{ts, surface, tool, ok, ms, cause}`. The report script in
//! dromologue-sync tallies it (volume, error rate, rate-limit incidence per
//! tool per day). We cannot instrument the desktop MCP's own server, so the
//! skill records its routing decisions into the same directory as a
//! `surface:"desktop-mcp"` line — the two together give a comparable picture.
//!
//! Contract: a no-op when the env var is unset or empty (so the public build
//! ships silent); never fails or slows the caller (all errors swallowed, one
//! append per call). Not domain logic, so reading the wall clock here is fine.

use std::io::Write;

/// Env var naming the directory the usage log is written to. Unset/empty
/// disables logging entirely (no file is created).
pub const USAGE_LOG_DIR_ENV: &str = "WORKFLOWY_USAGE_LOG_DIR";

/// Append one usage record. `surface` is the caller's own name for itself
/// (`"mcp"` for the stdio server, `"cli"` for `wflow-do`); `cause` is the
/// proximate-cause slug on failure, `None` on success.
pub fn record(surface: &str, tool: &str, ok: bool, duration_ms: u64, cause: Option<&str>) {
    let dir = match std::env::var(USAGE_LOG_DIR_ENV) {
        Ok(d) if !d.trim().is_empty() => d,
        _ => return,
    };
    record_to_dir(&dir, surface, tool, ok, duration_ms, cause);
}

/// Testable core: append to an explicit directory regardless of env.
pub fn record_to_dir(
    dir: &str,
    surface: &str,
    tool: &str,
    ok: bool,
    duration_ms: u64,
    cause: Option<&str>,
) {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let date = chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let line = serde_json::json!({
        "ts": secs,
        "surface": surface,
        "tool": tool,
        "ok": ok,
        "ms": duration_ms,
        "cause": cause,
    });
    let path = std::path::Path::new(dir).join(format!("{date}.jsonl"));
    // Best-effort: create the dir if missing, append one line, swallow errors —
    // a telemetry write must never break a tool call.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_to_dir_appends_one_jsonl_line_per_call() {
        let dir = std::env::temp_dir().join(format!("wflow-usage-test-{}", std::process::id()));
        let dir_s = dir.to_str().unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        record_to_dir(dir_s, "mcp", "search_nodes", true, 42, None);
        record_to_dir(dir_s, "cli", "create", false, 7, Some("rate_limited"));

        // Exactly one file (today's), two lines, each valid JSON with the
        // fields the report tallies.
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .collect();
        assert_eq!(files.len(), 1, "one daily file");
        let body = std::fs::read_to_string(&files[0]).unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let a: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(a["surface"], "mcp");
        assert_eq!(a["tool"], "search_nodes");
        assert_eq!(a["ok"], true);
        assert_eq!(a["cause"], serde_json::Value::Null);
        let b: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(b["surface"], "cli");
        assert_eq!(b["ok"], false);
        assert_eq!(b["cause"], "rate_limited");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_is_a_noop_when_env_unset() {
        let _guard = crate::defaults::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(USAGE_LOG_DIR_ENV).ok();
        std::env::remove_var(USAGE_LOG_DIR_ENV);
        // Must not panic and must write nothing (no dir to check — just that
        // the unset path returns before touching the filesystem).
        record("mcp", "get_node", true, 1, None);
        match prev {
            Some(v) => std::env::set_var(USAGE_LOG_DIR_ENV, v),
            None => std::env::remove_var(USAGE_LOG_DIR_ENV),
        }
    }
}
