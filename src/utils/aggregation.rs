//! Shared aggregation helpers for tree-walk results.
//!
//! Both the MCP server's tool handlers (`src/server/mod.rs`) and the
//! `wflow-do` CLI (`src/bin/wflow_do.rs`) compute the same shapes from
//! the same `Vec<WorkflowyNode>` walks: overdue items sorted by
//! days-overdue, upcoming items in a date window, recent changes by
//! `last_modified`, and a per-todo entry filtered by status and
//! optional query. Pre-2026-05-03 each surface implemented these
//! independently — same logic, two definitions, no enforcement that
//! they stay in sync. The architecture review surfaced this as a
//! latent-divergence risk: the build-time CLI parity test catches the
//! surface (every MCP tool has a CLI subcommand), but it can't catch
//! semantic drift between two parallel implementations.
//!
//! These helpers are the single source of truth. They take `&[WorkflowyNode]`
//! and produce `Vec<serde_json::Value>` for direct embedding in JSON
//! responses — the JSON shape is the contract the MCP responses pin
//! and the CLI mirrors. Keeping the helpers in `utils/` (rather than
//! `server/`) lets the CLI use them without dragging in any of the
//! server's MCP machinery.

use serde_json::{json, Value};

use crate::types::WorkflowyNode;
use crate::utils::date_parser::parse_due_date_from_node;
use crate::utils::node_paths::{build_node_map, build_node_path_with_map};
use crate::utils::subtree::{is_completed, is_todo};

/// Build the per-node JSON entry that overdue / upcoming / todos-list
/// responses embed. Centralised so the entry shape (id / name / path /
/// completed flag etc.) doesn't drift between callers.
fn node_entry(
    node: &WorkflowyNode,
    map: &std::collections::HashMap<&str, &WorkflowyNode>,
    extras: &[(&str, Value)],
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(node.id));
    obj.insert("name".into(), json!(node.name));
    obj.insert("path".into(), json!(build_node_path_with_map(&node.id, map)));
    for (k, v) in extras {
        obj.insert((*k).to_string(), v.clone());
    }
    Value::Object(obj)
}

/// Overdue todos, sorted by most-overdue first.
///
/// `nodes`: the full walk result. The function builds its own parent
/// map for path reconstruction. `today`: the comparison date — taking
/// it as a parameter rather than reading the system clock keeps the
/// function pure and lets tests pass arbitrary dates.
/// `include_completed`: when false (the default for `list_overdue`),
/// nodes with `completed_at` set are dropped before the date check.
pub fn compute_overdue(
    nodes: &[WorkflowyNode],
    today: chrono::NaiveDate,
    include_completed: bool,
) -> Vec<Value> {
    let map = build_node_map(nodes);
    let mut hits: Vec<Value> = nodes
        .iter()
        .filter_map(|n| {
            if !include_completed && is_completed(n) {
                return None;
            }
            let due = parse_due_date_from_node(n)?;
            if due >= today {
                return None;
            }
            let days = (today - due).num_days();
            Some(node_entry(
                n,
                &map,
                &[
                    ("due_date", json!(due.to_string())),
                    ("days_overdue", json!(days)),
                    ("completed", json!(is_completed(n))),
                ],
            ))
        })
        .collect();
    hits.sort_by(|a, b| b["days_overdue"].as_i64().cmp(&a["days_overdue"].as_i64()));
    hits
}

/// Upcoming todos within `[today, today + horizon_days]`, sorted by
/// nearest deadline first. Same purity contract as `compute_overdue`.
pub fn compute_upcoming(
    nodes: &[WorkflowyNode],
    today: chrono::NaiveDate,
    horizon_days: i64,
    include_completed: bool,
) -> Vec<Value> {
    let map = build_node_map(nodes);
    let horizon = today + chrono::Duration::days(horizon_days);
    let mut hits: Vec<Value> = nodes
        .iter()
        .filter_map(|n| {
            if !include_completed && is_completed(n) {
                return None;
            }
            let due = parse_due_date_from_node(n)?;
            if due < today || due > horizon {
                return None;
            }
            Some(node_entry(
                n,
                &map,
                &[
                    ("due_date", json!(due.to_string())),
                    ("days_until", json!((due - today).num_days())),
                    ("completed", json!(is_completed(n))),
                ],
            ))
        })
        .collect();
    hits.sort_by(|a, b| a["days_until"].as_i64().cmp(&b["days_until"].as_i64()));
    hits
}

/// Recently-modified nodes within the last `hours_ago` window, newest
/// first, capped at `limit`. `now_ms` taken as a parameter so tests
/// can pass arbitrary timestamps; production passes
/// `chrono::Utc::now().timestamp_millis()`.
pub fn compute_recent_changes(
    nodes: &[WorkflowyNode],
    now_ms: i64,
    hours_ago: i64,
    limit: usize,
) -> Vec<Value> {
    let map = build_node_map(nodes);
    let cutoff_ms = now_ms - hours_ago * 60 * 60 * 1_000;
    let mut hits: Vec<Value> = nodes
        .iter()
        .filter_map(|n| {
            let ts = n.last_modified?;
            if ts < cutoff_ms {
                return None;
            }
            Some(node_entry(
                n,
                &map,
                &[
                    ("modifiedAt", json!(ts)),
                    ("completed", json!(is_completed(n))),
                ],
            ))
        })
        .collect();
    hits.sort_by(|a, b| b["modifiedAt"].as_i64().cmp(&a["modifiedAt"].as_i64()));
    hits.truncate(limit);
    hits
}

/// Filter nodes to todos matching the given status (`all` /
/// `pending` / `completed`) and optional substring query (case-
/// insensitive, matched against name + description). Result capped
/// at `limit`. JSON shape includes `completed` and `completed_at`
/// alongside the standard entry fields so callers can render the
/// completion state.
pub fn filter_todos(
    nodes: &[WorkflowyNode],
    status: &str,
    query: Option<&str>,
    limit: usize,
) -> Vec<Value> {
    let map = build_node_map(nodes);
    let q = query.map(|s| s.to_lowercase());
    nodes
        .iter()
        .filter(|n| {
            if !is_todo(n) {
                return false;
            }
            let completed = is_completed(n);
            match status {
                "pending" if completed => return false,
                "completed" if !completed => return false,
                _ => {}
            }
            if let Some(q) = &q {
                let in_name = n.name.to_lowercase().contains(q);
                let in_desc = n
                    .description
                    .as_deref()
                    .map(|d| d.to_lowercase().contains(q))
                    .unwrap_or(false);
                if !in_name && !in_desc {
                    return false;
                }
            }
            true
        })
        .take(limit)
        .map(|n| {
            node_entry(
                n,
                &map,
                &[
                    ("completed", json!(is_completed(n))),
                    ("completed_at", json!(n.completed_at)),
                ],
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn node(id: &str, name: &str) -> WorkflowyNode {
        WorkflowyNode {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn compute_overdue_drops_future_dates_and_sorts_most_overdue_first() {
        let mut a = node("a", "due in past");
        a.description = Some("due:2026-05-01".into());
        let mut b = node("b", "due further in past");
        b.description = Some("due:2026-04-25".into());
        let mut c = node("c", "due in future");
        c.description = Some("due:2026-06-01".into());

        let today = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
        let hits = compute_overdue(&[a, b, c], today, false);
        assert_eq!(hits.len(), 2, "future-due item must be excluded");
        assert_eq!(
            hits[0]["id"].as_str(),
            Some("b"),
            "most-overdue must come first"
        );
        assert_eq!(
            hits[0]["days_overdue"].as_i64(),
            Some(8),
            "days-overdue computed against `today` parameter"
        );
    }

    #[test]
    fn compute_overdue_excludes_completed_unless_include_completed() {
        let mut a = node("a", "completed and overdue");
        a.description = Some("due:2026-04-25".into());
        a.completed_at = Some(1700000000000);

        let today = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
        assert!(compute_overdue(&std::slice::from_ref(&a), today, false).is_empty());
        assert_eq!(
            compute_overdue(std::slice::from_ref(&a), today, true).len(),
            1
        );
    }

    #[test]
    fn compute_upcoming_filters_to_window_and_sorts_nearest_first() {
        let mut a = node("a", "due in 5 days");
        a.description = Some("due:2026-05-08".into());
        let mut b = node("b", "due in 2 days");
        b.description = Some("due:2026-05-05".into());
        let mut c = node("c", "due in 30 days (outside 7-day window)");
        c.description = Some("due:2026-06-02".into());

        let today = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
        let hits = compute_upcoming(&[a, b, c], today, 7, false);
        assert_eq!(hits.len(), 2, "out-of-window item dropped");
        assert_eq!(hits[0]["id"].as_str(), Some("b"), "nearest-first");
    }

    #[test]
    fn compute_recent_changes_respects_window_and_limit() {
        let mut now = node("now", "modified recently");
        now.last_modified = Some(1700000000000);
        let mut old = node("old", "modified long ago");
        old.last_modified = Some(1600000000000);
        let mut newer = node("newer", "modified more recently");
        newer.last_modified = Some(1700001000000);

        let now_ms = 1700002000000;
        let hits = compute_recent_changes(&[now, old, newer], now_ms, 24, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["id"].as_str(), Some("newer"), "newest first");
    }

    #[test]
    fn filter_todos_applies_status_and_query_and_limit() {
        let mut t1 = node("t1", "[ ] write tests");
        t1.layout_mode = Some("todo".into());
        let mut t2 = node("t2", "[x] write more tests");
        t2.layout_mode = Some("todo".into());
        t2.completed_at = Some(1700000000000);
        let regular = node("r1", "not a todo");

        let pending = filter_todos(&[t1.clone(), t2.clone(), regular.clone()], "pending", None, 10);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["id"].as_str(), Some("t1"));

        let completed = filter_todos(&[t1.clone(), t2.clone()], "completed", None, 10);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0]["id"].as_str(), Some("t2"));

        let all = filter_todos(&[t1.clone(), t2.clone()], "all", None, 10);
        assert_eq!(all.len(), 2);

        let by_query = filter_todos(&[t1.clone(), t2.clone()], "all", Some("more"), 10);
        assert_eq!(by_query.len(), 1);
        assert_eq!(by_query[0]["id"].as_str(), Some("t2"));

        let limited = filter_todos(&[t1.clone(), t2.clone()], "all", None, 1);
        assert_eq!(limited.len(), 1);
    }
}
