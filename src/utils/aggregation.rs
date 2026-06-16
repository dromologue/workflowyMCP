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

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};

use crate::types::WorkflowyNode;
use crate::utils::date_parser::{is_overdue, parse_due_date_from_node};
use crate::utils::node_paths::{build_node_map, build_node_path_with_map};
use crate::utils::subtree::{is_completed, is_todo};
use crate::utils::tag_parser::parse_node_tags;

/// Build the per-node JSON entry that overdue / upcoming / todos-list
/// responses embed. Centralised so the entry shape (id / name / path /
/// completed flag etc.) doesn't drift between callers.
fn node_entry(
    node: &WorkflowyNode,
    map: &std::collections::HashMap<&str, &WorkflowyNode>,
    extras: Vec<(&str, Value)>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(node.id));
    obj.insert("name".into(), json!(node.name));
    obj.insert("path".into(), json!(build_node_path_with_map(&node.id, map)));
    // Move the extra values in (owned `json!(...)` temporaries the callers
    // build per node) rather than cloning each.
    for (k, v) in extras {
        obj.insert(k.to_string(), v);
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
                vec![
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
///
/// Per-entry field `days_until_due` matches the name the MCP
/// `list_upcoming` and `daily_review` handlers have always emitted —
/// the helper field used to be `days_until`, but the 2026-05-16
/// refactor renamed it so adopting the helper inside the MCP handler
/// is shape-preserving rather than a wire-break.
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
                vec![
                    ("due_date", json!(due.to_string())),
                    ("days_until_due", json!((due - today).num_days())),
                    ("completed", json!(is_completed(n))),
                ],
            ))
        })
        .collect();
    hits.sort_by(|a, b| a["days_until_due"].as_i64().cmp(&b["days_until_due"].as_i64()));
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
                vec![
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
/// at `limit`. JSON shape includes `completed`, `completed_at`, and
/// `note` (the node's description) alongside the standard entry
/// fields so callers can render the completion state and any
/// attached note. The `note` field was added 2026-05-16 to make
/// helper adoption shape-preserving for the MCP `list_todos`
/// handler — pre-refactor that handler emitted `note` inline; the
/// helper now emits it uniformly so both surfaces stay aligned.
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
                vec![
                    ("note", json!(n.description)),
                    ("completed", json!(is_completed(n))),
                    ("completed_at", json!(n.completed_at)),
                ],
            )
        })
        .collect()
}

/// Select the candidate nodes a `bulk_update` will act on, applying the
/// same filter both surfaces inlined before 2026-06-16.
///
/// - `query` (optional): case-insensitive `contains` against the node's
///   name OR description.
/// - `tag` (optional): whole-tag match via `node_has_tag` (so `lead`
///   does not match `#leadership`). The leading `#`/`@` is optional —
///   `node_has_tag` normalises it.
/// - `status`: `"pending"` drops completed nodes, `"completed"` drops
///   pending nodes, anything else (`"all"`) keeps both.
/// - `limit`: caps the returned slice (the caller enforces its own
///   over-limit error separately for the MCP surface).
///
/// Pure — no clock, no client. Both the MCP `bulk_update` handler and
/// the `wflow-do bulk-update` subcommand route through this so the
/// candidate selection cannot drift; the apply step is already shared
/// via `workflows::apply_bulk_op`.
pub fn filter_bulk_candidates<'a>(
    nodes: &'a [WorkflowyNode],
    query: Option<&str>,
    tag: Option<&str>,
    status: &str,
    limit: usize,
) -> Vec<&'a WorkflowyNode> {
    let q = query.map(|s| s.to_lowercase());
    nodes
        .iter()
        .filter(|n| {
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
            if let Some(tag) = tag {
                if !crate::utils::tag_parser::node_has_tag(n, tag) {
                    return false;
                }
            }
            let completed = is_completed(n);
            match status {
                "pending" if completed => return false,
                "completed" if !completed => return false,
                _ => {}
            }
            true
        })
        .take(limit)
        .collect()
}

// ---------------------------------------------------------------------
// project_summary
// ---------------------------------------------------------------------

/// Aggregated summary for `get_project_summary`. Single shape both
/// the MCP handler and the `wflow-do project-summary` CLI emit, so
/// the surfaces cannot drift — pre-2026-05-16 the MCP emitted a
/// nested `stats` object with a conditional `tags`/`assignees` pair
/// while the CLI flat-printed counts and stripped tag prefixes, the
/// drift the architecture review on 2026-05-16 surfaced as the
/// most-divergent of the three orchestrations not yet lifted.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummary {
    pub root: ProjectSummaryRoot,
    pub stats: ProjectSummaryStats,
    /// 20-most-recent-first nodes whose `last_modified` is newer than
    /// `now_ms - recent_days * 86_400_000`. Capped at 20 because the
    /// MCP wire surface has carried that cap since the original
    /// handler and consumers depend on a bounded list.
    pub recently_modified: Vec<Value>,
    /// Tag counts as `#tag → count`. `None` when the caller passed
    /// `include_tags = false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<BTreeMap<String, usize>>,
    /// Assignee counts as `@person → count`. `None` when
    /// `include_tags = false` (the flag governs both — they are the
    /// same parse pass).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignees: Option<BTreeMap<String, usize>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummaryRoot {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummaryStats {
    pub total_nodes: usize,
    pub todo_total: usize,
    pub todo_pending: usize,
    pub todo_completed: usize,
    pub completion_percent: usize,
    pub has_due_dates: bool,
    pub overdue_count: usize,
}

/// Compute the project summary over a walked subtree.
///
/// Returns `None` when `root_id` is not present in `nodes` — the
/// caller surfaces this as a `not found` validation failure rather
/// than an empty summary. `today` and `now_ms` are taken as
/// parameters (no system-clock reads) so the function is pure and
/// tests can pass deterministic values.
///
/// `recent_days` is the recently-modified window in days; the
/// resulting list is sorted newest-first and capped at 20.
/// `include_tags` controls whether `tags` / `assignees` are
/// populated — passing `false` skips the per-node tag parse, which
/// matters on big trees.
pub fn compute_project_summary(
    nodes: &[WorkflowyNode],
    root_id: &str,
    today: chrono::NaiveDate,
    now_ms: i64,
    include_tags: bool,
    recent_days: i64,
) -> Option<ProjectSummary> {
    let map = build_node_map(nodes);
    let root = nodes.iter().find(|n| n.id == root_id)?;
    let root_path = build_node_path_with_map(&root.id, &map);

    let recent_cutoff = now_ms - recent_days * 86_400_000;

    let mut total = 0usize;
    let mut todo_total = 0usize;
    let mut todo_completed = 0usize;
    let mut overdue_count = 0usize;
    let mut has_due_dates = false;
    let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut assignee_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut recent_modified: Vec<(&WorkflowyNode, i64)> = Vec::new();

    for node in nodes {
        total += 1;
        let todo = is_todo(node);
        let completed = is_completed(node);

        if todo {
            todo_total += 1;
            if completed {
                todo_completed += 1;
            }
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

    let completion_percent = if todo_total > 0 {
        ((todo_completed as f64 / todo_total as f64) * 100.0).round() as usize
    } else {
        0
    };

    let recently_modified: Vec<Value> = recent_modified
        .iter()
        .map(|(n, ts)| {
            json!({
                "id": n.id,
                "name": n.name,
                "modifiedAt": ts,
                "path": build_node_path_with_map(&n.id, &map),
            })
        })
        .collect();

    Some(ProjectSummary {
        root: ProjectSummaryRoot {
            id: root.id.clone(),
            name: root.name.clone(),
            path: root_path,
        },
        stats: ProjectSummaryStats {
            total_nodes: total,
            todo_total,
            todo_pending: todo_total - todo_completed,
            todo_completed,
            completion_percent,
            has_due_dates,
            overdue_count,
        },
        recently_modified,
        tags: if include_tags { Some(tag_counts) } else { None },
        assignees: if include_tags {
            Some(assignee_counts)
        } else {
            None
        },
    })
}

// ---------------------------------------------------------------------
// daily_review
// ---------------------------------------------------------------------

/// Aggregated four-bucket daily review over a walked subtree. Both
/// the MCP `daily_review` handler and the `wflow-do daily-review`
/// CLI emit this shape, so semantic drift between the surfaces is
/// impossible — pre-2026-05-16 the CLI hardcoded a 7-day horizon
/// and emitted `days_until` while the MCP parameterised
/// `upcoming_days` and emitted `days_until_due`.
///
/// The four buckets reuse the per-bucket aggregation helpers
/// (`compute_overdue`, `compute_upcoming`, `compute_recent_changes`,
/// `filter_todos`) so any future tweak to a bucket's shape lands
/// across all five entry points uniformly.
#[derive(Debug, Clone, Serialize)]
pub struct DailyReview {
    pub as_of: String,
    pub summary: DailyReviewSummary,
    pub overdue: Vec<Value>,
    pub due_soon: Vec<Value>,
    pub recent_changes: Vec<Value>,
    pub top_pending: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyReviewSummary {
    pub total_nodes: usize,
    pub pending_todos: usize,
    pub overdue_count: usize,
    pub due_today: usize,
    pub modified_today: usize,
}

/// Compute the daily review over a walked subtree.
///
/// Bucket parameters:
/// - `upcoming_days`: forward window (days) for `due_soon`.
/// - `recent_days`: backward window (days) for `recent_changes` —
///   converted internally to `recent_days * 24` hours so the
///   helper signature matches `compute_recent_changes`.
/// - `*_limit`: per-bucket caps applied AFTER sorting.
///
/// `today` and `now_ms` are taken as parameters so the function is
/// pure and tests can supply deterministic values.
pub fn compute_daily_review(
    nodes: &[WorkflowyNode],
    today: chrono::NaiveDate,
    now_ms: i64,
    upcoming_days: i64,
    recent_days: i64,
    overdue_limit: usize,
    due_soon_limit: usize,
    recent_limit: usize,
    pending_limit: usize,
) -> DailyReview {
    let mut overdue = compute_overdue(nodes, today, false);
    overdue.truncate(overdue_limit);

    let mut due_soon = compute_upcoming(nodes, today, upcoming_days, false);
    due_soon.truncate(due_soon_limit);

    let mut recent_changes =
        compute_recent_changes(nodes, now_ms, recent_days * 24, recent_limit);
    recent_changes.truncate(recent_limit);

    let top_pending = filter_todos(nodes, "pending", None, pending_limit);

    // Summary stats are computed in one pass over the full node set
    // — the per-bucket helpers already filter+sort+limit, but the
    // summary counts (total nodes, pending count, due-today count,
    // modified-today count) must reflect the whole walk.
    let mut total_nodes = 0usize;
    let mut pending_todos = 0usize;
    let mut due_today = 0usize;
    let mut modified_today = 0usize;
    let recent_cutoff = now_ms - recent_days * 86_400_000;

    for node in nodes {
        total_nodes += 1;
        let completed = is_completed(node);
        if is_todo(node) && !completed {
            pending_todos += 1;
        }
        if !completed {
            if let Some(due) = parse_due_date_from_node(node) {
                if due == today {
                    due_today += 1;
                }
            }
        }
        if let Some(ts) = node.last_modified {
            if ts > recent_cutoff {
                modified_today += 1;
            }
        }
    }

    DailyReview {
        as_of: today.to_string(),
        summary: DailyReviewSummary {
            total_nodes,
            pending_todos,
            overdue_count: overdue.len(),
            due_today,
            modified_today,
        },
        overdue,
        due_soon,
        recent_changes,
        top_pending,
    }
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
    fn filter_bulk_candidates_applies_query_tag_status_and_limit() {
        let mut a = node("a", "Buy milk #shop");
        a.description = Some("from the store".into());
        let b = node("b", "Read leadership book #leadership");
        let mut c = node("c", "done task #shop");
        c.completed_at = Some(1700000000000);
        let d = node("d", "unrelated note");
        let nodes = vec![a, b, c.clone(), d];

        // Query matches name OR description.
        let q = filter_bulk_candidates(&nodes, Some("store"), None, "all", 100);
        assert_eq!(q.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec!["a"]);

        // Whole-tag match: `#shop` must NOT be matched by `sho`, and
        // `#lead` must NOT match `#leadership`.
        let shop = filter_bulk_candidates(&nodes, None, Some("shop"), "all", 100);
        assert_eq!(shop.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec!["a", "c"]);
        let lead = filter_bulk_candidates(&nodes, None, Some("lead"), "all", 100);
        assert!(lead.is_empty(), "`lead` must not match `#leadership`");

        // Status gate via is_completed: pending drops c, completed keeps only c.
        let pending = filter_bulk_candidates(&nodes, None, Some("shop"), "pending", 100);
        assert_eq!(pending.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec!["a"]);
        let completed = filter_bulk_candidates(&nodes, None, Some("shop"), "completed", 100);
        assert_eq!(completed.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec!["c"]);

        // Limit caps the slice.
        let capped = filter_bulk_candidates(&nodes, None, Some("shop"), "all", 1);
        assert_eq!(capped.len(), 1);
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
        assert_eq!(
            hits[0]["days_until_due"].as_i64(),
            Some(2),
            "field name `days_until_due` is the wire contract — renamed from `days_until` on 2026-05-16 so the MCP handler can adopt the helper without breaking its existing JSON shape",
        );
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

    #[test]
    fn filter_todos_entry_carries_note_field() {
        // The MCP `list_todos` handler used to emit a `note` field
        // alongside id/name/path/completed/completed_at — pinned
        // here as part of the helper's contract so adoption inside
        // the handler is shape-preserving (no wire break).
        let mut t = node("t", "[ ] do thing");
        t.layout_mode = Some("todo".into());
        t.description = Some("some note body".into());

        let hits = filter_todos(std::slice::from_ref(&t), "all", None, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0]["note"].as_str(),
            Some("some note body"),
            "filter_todos entries must include the description as `note`",
        );
    }

    #[test]
    fn compute_project_summary_counts_todos_overdue_and_tags() {
        let mut t1 = node("t1", "[ ] pending #a @bob");
        t1.layout_mode = Some("todo".into());
        let mut t2 = node("t2", "[x] done #a #b");
        t2.layout_mode = Some("todo".into());
        t2.completed_at = Some(1700000000000);
        let mut overdue_node = node("o1", "[ ] overdue");
        overdue_node.layout_mode = Some("todo".into());
        overdue_node.description = Some("due:2026-04-25".into());
        let mut recent_node = node("r1", "modified recently");
        recent_node.last_modified = Some(1_700_000_500_000);

        let root = node("root", "Root");
        let nodes = vec![
            root.clone(),
            t1,
            t2,
            overdue_node,
            recent_node,
        ];
        let today = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        let now_ms = 1_700_001_000_000i64;

        let summary = compute_project_summary(&nodes, "root", today, now_ms, true, 7)
            .expect("root present");

        assert_eq!(summary.stats.total_nodes, 5);
        assert_eq!(summary.stats.todo_total, 3); // t1, t2, overdue_node
        assert_eq!(summary.stats.todo_completed, 1); // t2
        assert_eq!(summary.stats.todo_pending, 2);
        assert_eq!(summary.stats.completion_percent, 33);
        assert!(summary.stats.has_due_dates);
        assert_eq!(summary.stats.overdue_count, 1);
        assert_eq!(summary.root.id, "root");

        let tags = summary.tags.as_ref().expect("include_tags=true");
        assert_eq!(tags.get("#a").copied(), Some(2));
        assert_eq!(tags.get("#b").copied(), Some(1));

        let assignees = summary.assignees.as_ref().expect("include_tags=true");
        assert_eq!(assignees.get("@bob").copied(), Some(1));
    }

    #[test]
    fn compute_project_summary_returns_none_when_root_absent() {
        let other = node("other", "Other");
        let today = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        let summary =
            compute_project_summary(std::slice::from_ref(&other), "missing", today, 0, false, 7);
        assert!(summary.is_none());
    }

    #[test]
    fn compute_project_summary_skips_tag_parse_when_include_tags_false() {
        let mut t = node("t", "[ ] thing #foo");
        t.layout_mode = Some("todo".into());
        let today = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        let summary = compute_project_summary(
            std::slice::from_ref(&t),
            "t",
            today,
            0,
            false,
            7,
        )
        .expect("root present");
        assert!(summary.tags.is_none());
        assert!(summary.assignees.is_none());
    }

    #[test]
    fn compute_daily_review_routes_buckets_through_per_bucket_helpers() {
        let mut overdue_n = node("o1", "[ ] overdue");
        overdue_n.layout_mode = Some("todo".into());
        overdue_n.description = Some("due:2026-04-25".into());
        let mut soon_n = node("s1", "[ ] due soon");
        soon_n.layout_mode = Some("todo".into());
        soon_n.description = Some("due:2026-05-17".into());
        let mut pending_n = node("p1", "[ ] just a pending todo");
        pending_n.layout_mode = Some("todo".into());
        let mut recent_n = node("r1", "modified recently");
        recent_n.last_modified = Some(1_700_000_500_000);

        let today = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        let now_ms = 1_700_001_000_000i64;
        let review = compute_daily_review(
            &[overdue_n, soon_n, pending_n, recent_n],
            today,
            now_ms,
            7,    // upcoming_days
            1,    // recent_days
            10,   // overdue_limit
            20,   // due_soon_limit
            20,   // recent_limit
            20,   // pending_limit
        );

        assert_eq!(review.as_of, "2026-05-16");
        assert_eq!(review.overdue.len(), 1);
        assert_eq!(review.overdue[0]["id"].as_str(), Some("o1"));
        assert_eq!(review.due_soon.len(), 1);
        assert_eq!(review.due_soon[0]["id"].as_str(), Some("s1"));
        assert_eq!(review.recent_changes.len(), 1);
        assert_eq!(review.recent_changes[0]["id"].as_str(), Some("r1"));
        // top_pending includes every pending todo: overdue, due-soon, p1.
        assert_eq!(review.top_pending.len(), 3);
        assert_eq!(review.summary.overdue_count, 1);
        assert_eq!(review.summary.pending_todos, 3);
        assert_eq!(review.summary.modified_today, 1);
    }
}
