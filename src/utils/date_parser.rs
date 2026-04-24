//! Due date extraction from node text.
//! Parses due:YYYY-MM-DD, #due-YYYY-MM-DD, and bare YYYY-MM-DD patterns.

use chrono::NaiveDate;
use regex::Regex;
use lazy_static::lazy_static;

use crate::types::WorkflowyNode;

lazy_static! {
    // Priority 1: due:YYYY-MM-DD
    static ref DUE_COLON: Regex = Regex::new(r"due:(\d{4}-\d{2}-\d{2})").unwrap();
    // Priority 2: #due-YYYY-MM-DD
    static ref DUE_TAG: Regex = Regex::new(r"#due-(\d{4}-\d{2}-\d{2})").unwrap();
    // Priority 3: bare YYYY-MM-DD
    static ref BARE_DATE: Regex = Regex::new(r"\b(\d{4}-\d{2}-\d{2})\b").unwrap();
}

/// Parse a due date from arbitrary text, checking patterns in priority order.
pub fn parse_due_date(text: &str) -> Option<NaiveDate> {
    // Priority 1: due:YYYY-MM-DD
    if let Some(cap) = DUE_COLON.captures(text) {
        if let Ok(d) = NaiveDate::parse_from_str(&cap[1], "%Y-%m-%d") {
            return Some(d);
        }
    }
    // Priority 2: #due-YYYY-MM-DD
    if let Some(cap) = DUE_TAG.captures(text) {
        if let Ok(d) = NaiveDate::parse_from_str(&cap[1], "%Y-%m-%d") {
            return Some(d);
        }
    }
    // Priority 3: bare YYYY-MM-DD
    if let Some(cap) = BARE_DATE.captures(text) {
        if let Ok(d) = NaiveDate::parse_from_str(&cap[1], "%Y-%m-%d") {
            return Some(d);
        }
    }
    None
}

/// Parse due date from a node's name and description.
pub fn parse_due_date_from_node(node: &WorkflowyNode) -> Option<NaiveDate> {
    // Check name first (higher priority)
    if let Some(d) = parse_due_date(&node.name) {
        return Some(d);
    }
    // Then description
    if let Some(desc) = &node.description {
        return parse_due_date(desc);
    }
    None
}

/// Check if a node is overdue (incomplete + due date < today).
pub fn is_overdue(node: &WorkflowyNode, today: NaiveDate) -> bool {
    if node.completed_at.is_some() {
        return false;
    }
    match parse_due_date_from_node(node) {
        Some(due) => due < today,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_due_colon_format() {
        assert_eq!(
            parse_due_date("Task due:2026-03-15 something"),
            Some(NaiveDate::from_ymd_opt(2026, 3, 15).unwrap())
        );
    }

    #[test]
    fn test_parse_due_tag_format() {
        assert_eq!(
            parse_due_date("Task #due-2026-04-01"),
            Some(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap())
        );
    }

    #[test]
    fn test_parse_bare_date() {
        assert_eq!(
            parse_due_date("Meeting on 2026-06-15"),
            Some(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap())
        );
    }

    #[test]
    fn test_priority_order() {
        // due: takes priority over #due- and bare
        let text = "Task due:2026-01-01 #due-2026-02-02 2026-03-03";
        assert_eq!(
            parse_due_date(text),
            Some(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap())
        );
    }

    #[test]
    fn test_tag_over_bare() {
        let text = "Task #due-2026-02-02 also 2026-03-03";
        assert_eq!(
            parse_due_date(text),
            Some(NaiveDate::from_ymd_opt(2026, 2, 2).unwrap())
        );
    }

    #[test]
    fn test_no_date() {
        assert_eq!(parse_due_date("No date here"), None);
    }

    #[test]
    fn test_invalid_date() {
        assert_eq!(parse_due_date("due:2026-13-45"), None);
    }

    #[test]
    fn test_parse_from_node_name() {
        let node = make_node("Task due:2026-05-01", None);
        assert_eq!(
            parse_due_date_from_node(&node),
            Some(NaiveDate::from_ymd_opt(2026, 5, 1).unwrap())
        );
    }

    #[test]
    fn test_parse_from_node_description() {
        let node = make_node("Task", Some("Notes due:2026-06-15"));
        assert_eq!(
            parse_due_date_from_node(&node),
            Some(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap())
        );
    }

    #[test]
    fn test_name_takes_priority_over_description() {
        let node = make_node("Task due:2026-01-01", Some("due:2026-12-31"));
        assert_eq!(
            parse_due_date_from_node(&node),
            Some(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap())
        );
    }

    #[test]
    fn test_is_overdue_true() {
        let node = make_node("Task due:2026-01-15", None);
        let today = NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
        assert!(is_overdue(&node, today));
    }

    #[test]
    fn test_is_overdue_false_future() {
        let node = make_node("Task due:2026-12-01", None);
        let today = NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
        assert!(!is_overdue(&node, today));
    }

    #[test]
    fn test_is_overdue_false_completed() {
        let mut node = make_node("Task due:2026-01-15", None);
        node.completed_at = Some(1700000000000);
        let today = NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
        assert!(!is_overdue(&node, today));
    }

    #[test]
    fn test_is_overdue_false_no_date() {
        let node = make_node("Task without date", None);
        let today = NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
        assert!(!is_overdue(&node, today));
    }

    fn make_node(name: &str, desc: Option<&str>) -> WorkflowyNode {
        WorkflowyNode {
            id: "test-id".to_string(),
            name: name.to_string(),
            description: desc.map(String::from),
            ..Default::default()
        }
    }
}
