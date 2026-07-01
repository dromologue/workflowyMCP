//! Mirror-convention auditing and review-surface bucketing.
//!
//! Pure analyses over a `&[WorkflowyNode]` slice plus a few scalar
//! parameters. No I/O, no API client, no implicit clock — `build_review`
//! takes `now_unix` so tests are deterministic.
//!
//! Two callers share these functions: the MCP `audit_mirrors` / `review`
//! tool handlers in `server.rs`, and the `wflow-do audit-mirrors` /
//! `review` subcommands in `bin/wflow_do.rs`. Keeping the heuristics in
//! one place means a fix in either surface lands in both.
//!
//! ## Mirror convention (audit_mirrors)
//!
//! Workflowy's REST API does not expose mirror creation, so the wflow
//! skill formalises a documented convention enforced by audit:
//!
//! - **Canonical** notes carry `canonical_of: <pillar>` in their
//!   description. The pillar token is opaque to this module; the audit
//!   only checks for the marker's presence.
//! - **Mirror** notes carry `mirror_of: <canonical-uuid>` in their
//!   description. The mirror's name should be a verbatim copy of the
//!   canonical's name at creation time.
//!
//! `audit_mirrors_with_external` walks the supplied node set and emits
//! findings of four kinds:
//!
//! - **BROKEN** — `mirror_of:<uuid>` does not resolve in the walked
//!   scope **and** is not present in the supplied `external_canonicals`
//!   map (UUID typo, target deleted from the entire graph). Resolution
//!   accepts a full UUID, the 12-char URL-suffix short hash, or the
//!   8-char doc-form prefix short hash (`id_match`, matching
//!   `link_parser.rs`'s recognised short-hash shapes) — pre-2026-07-01
//!   `id_match` only checked suffixes, so an 8-char prefix marker
//!   never resolved even against an in-scope target.
//! - **DRIFTED** — mirror name has diverged from the canonical's name
//!   (substring-match in either direction; the canonical's name has
//!   probably been edited and the mirror was missed).
//! - **ORPHAN** — the claimed canonical is found in scope but lacks a
//!   `canonical_of:` marker, so it doesn't acknowledge being canonical.
//!   Often a one-way reference that was never set up symmetrically.
//! - **LONELY** — a canonical with `canonical_of:` set but no mirrors
//!   pointing at it. May be intentional (some canonicals genuinely
//!   live in one place); reported so the user can confirm.
//!
//! ### Walk scope vs canonical-resolution scope
//!
//! The classifier separates the *walk scope* (which subtree was
//! traversed to find mirrors) from the *canonical-resolution scope*
//! (where the canonical UUIDs are looked up). Cross-pillar mirroring —
//! a mirror under pillar A pointing at a canonical under pillar B — is
//! the standard pattern under Mirror Discipline, so resolving the
//! canonical must reach outside the walk. The caller supplies an
//! `external_canonicals` map (typically built from the persistent name
//! index + a few live `get_node` calls); the classifier consults it
//! before declaring BROKEN. The ORPHAN check is skipped for external
//! canonicals because the resolver path doesn't carry the canonical's
//! description — absence of evidence is not evidence of absence.
//!
//! Surfaced 2026-05-16 by the weekly synthesis report:
//! cross-pillar audits returned five false-positive BROKEN findings
//! because the original implementation conflated the two scopes. The
//! `audit_mirrors(nodes)` shim preserves the old single-scope
//! behaviour for callers (and tests) that only care about a closed
//! subtree.
//!
//! ## Review surface (build_review)
//!
//! Four buckets the review surface (Workflow 14 in the wflow skill)
//! cares about:
//!
//! - (a) **Revisit-due**: nodes tagged `#revisit` whose description
//!   contains `revisit_due: YYYY-MM-DD` past the supplied `today` date.
//! - (b) **Multi-pillar**: nodes whose `mirror_of:` count or distinct
//!   pillar-tag count is ≥ 3 (whichever is greater — sum would
//!   double-count nodes that use both conventions).
//! - (c) **Stale cross-pillar**: cross-pillar concept maps with no
//!   `last_modified` change in `days_stale` days.
//! - (d) **Source-MOC re-cited**: source MOCs (heuristic: name contains
//!   ` — ` or a 4-digit year) whose description includes a URL or DOI
//!   that also appears in the supplied `source_moc_blob` (the caller
//!   loads recent session-log text and passes it in; pass `""` to skip
//!   this bucket).

use crate::types::WorkflowyNode;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Pillar tag tokens recognised by the multi-pillar review bucket.
/// Public so the same list can be referenced from docs and from
/// `wflow-do review`'s own CLI surface without drift.
pub const PILLAR_TAGS: &[&str] = &[
    "#leadership",
    "#ethics",
    "#building",
    "#learning",
    "#decide",
];

const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MirrorFinding {
    pub status: String,
    pub node_id: String,
    pub name: String,
    pub issue: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewReport {
    pub revisit_due: Vec<ReviewItem>,
    pub multi_pillar: Vec<ReviewItem>,
    pub stale_cross_pillar: Vec<ReviewItem>,
    pub source_moc_reuse: Vec<ReviewItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewItem {
    pub node_id: String,
    pub name: String,
    pub detail: String,
}

/// Extract a marker like `mirror_of:<uuid>` or `canonical_of:<pillar>`
/// from a description string. Returns the lowercased capture or None.
///
/// Two value shapes are supported because the convention uses both:
/// - `mirror_of:<uuid>` — full UUID or short hash, hex+hyphen
/// - `canonical_of:<pillar>` — opaque token like `lead`, `build`, `decide`
///
/// The capture class `[\w-]{3,40}` covers both: hex characters are a
/// subset of `\w` (alphanumeric + underscore), and hyphens are
/// explicitly allowed for UUID segments. The 3-char floor allows
/// test-mode IDs (`aaa`, `bbb`) alongside production values.
pub fn extract_marker(text: &str, prefix: &str) -> Option<String> {
    let pattern = format!(r"(?i){}\s*([\w-]{{3,40}})", regex::escape(prefix));
    let re = regex::Regex::new(&pattern).ok()?;
    re.captures(text).map(|c| c[1].to_lowercase())
}

/// A canonical resolved from outside the walked scope. The handler
/// builds this map by consulting the persistent name index (and, for
/// the MCP path, an optional live `get_node` fallback) for every
/// `mirror_of:` UUID encountered in scope that the walk itself didn't
/// resolve.
///
/// `has_canonical_marker` is `None` when the resolution path only
/// recovered the name (the common case — the name index does not
/// store descriptions). When `Some(true)`, the canonical is known to
/// acknowledge its role; when `Some(false)`, it does not. Only
/// `Some(false)` triggers an ORPHAN finding for an external mirror —
/// `None` is treated as "unknown, do not classify."
#[derive(Debug, Clone)]
pub struct ExternalCanonical {
    pub id: String,
    pub name: String,
    pub has_canonical_marker: Option<bool>,
}

/// Backward-compatible shim: audit a node set treating the slice as
/// both the walk scope **and** the resolution scope. Equivalent to
/// `audit_mirrors_with_external(nodes, &HashMap::new())`. Cross-scope
/// mirrors will all classify as BROKEN — use
/// [`audit_mirrors_with_external`] when the walk doesn't cover the
/// graph the mirrors point into.
pub fn audit_mirrors(nodes: &[WorkflowyNode]) -> Vec<MirrorFinding> {
    audit_mirrors_with_external(nodes, &HashMap::new())
}

/// Audit a node set against the canonical/mirror convention with
/// canonical resolution widened beyond the walk scope. See
/// module-level docs for the four finding kinds and the walk-vs-
/// resolution split.
///
/// Keys of `external_canonicals` are lowercased UUIDs (or short hashes
/// — the resolver uses the same `id_match` rule the in-scope lookup
/// uses, so end-matching on either side works). Values carry the
/// canonical's name and optionally a `canonical_of:` marker flag.
pub fn audit_mirrors_with_external(
    nodes: &[WorkflowyNode],
    external_canonicals: &HashMap<String, ExternalCanonical>,
) -> Vec<MirrorFinding> {
    // Matches a full UUID against either short-hash form the codebase
    // recognises (link_parser.rs): the 12-char URL-suffix hash (trailing
    // end of the UUID) and the 8-char doc-form prefix hash (leading end).
    // Pre-fix this only checked suffixes, so a `mirror_of:<8-char-prefix>`
    // marker never resolved even when its target was in scope.
    const MIN_SHORT_HASH_LEN: usize = 8;
    let id_match = |a: &str, b: &str| -> bool {
        let (a, b) = (a.to_lowercase(), b.to_lowercase());
        a == b
            || (a.len() >= MIN_SHORT_HASH_LEN && (a.ends_with(&b) || a.starts_with(&b)))
            || (b.len() >= MIN_SHORT_HASH_LEN && (b.ends_with(&a) || b.starts_with(&a)))
    };
    let mk = |status: &str, n: &WorkflowyNode, issue: String| MirrorFinding {
        status: status.into(),
        node_id: n.id.clone(),
        name: n.name.clone(),
        issue,
    };
    let by_id: HashMap<&str, &WorkflowyNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut canonical_targets: HashSet<String> = HashSet::new();
    let mut mirrors_by_target: HashMap<String, Vec<String>> = HashMap::new();
    let mut findings = Vec::new();
    for n in nodes {
        let desc = n.description.as_deref().unwrap_or("");
        if extract_marker(desc, "canonical_of:").is_some() {
            canonical_targets.insert(n.id.to_lowercase());
        }
        if let Some(target) = extract_marker(desc, "mirror_of:") {
            mirrors_by_target
                .entry(target.clone())
                .or_default()
                .push(n.id.clone());
            let canon = by_id.values().find(|c| id_match(&c.id, &target));
            match canon {
                Some(canon) => {
                    let (mn, cn) = (n.name.to_lowercase(), canon.name.to_lowercase());
                    if !mn.contains(&cn) && !cn.contains(&mn) {
                        findings.push(mk(
                            "DRIFTED",
                            n,
                            format!("name diverges from canonical \"{}\"", canon.name),
                        ));
                    }
                    let canon_desc = canon.description.as_deref().unwrap_or("");
                    if extract_marker(canon_desc, "canonical_of:").is_none() {
                        findings.push(mk(
                            "ORPHAN",
                            n,
                            format!("canonical {} has no canonical_of: marker", canon.id),
                        ));
                    }
                }
                None => {
                    // Not in walk scope — consult the external
                    // resolver (typically the persistent name index)
                    // before declaring BROKEN. Mirror Discipline is
                    // designed around cross-pillar references, so the
                    // common case is a canonical living elsewhere in
                    // the graph.
                    let external = external_canonicals
                        .values()
                        .find(|c| id_match(&c.id, &target));
                    match external {
                        None => findings.push(mk(
                            "BROKEN",
                            n,
                            format!(
                                "mirror_of:{} not found in scope or in name index",
                                target
                            ),
                        )),
                        Some(ec) => {
                            let (mn, cn) = (n.name.to_lowercase(), ec.name.to_lowercase());
                            if !mn.contains(&cn) && !cn.contains(&mn) {
                                findings.push(mk(
                                    "DRIFTED",
                                    n,
                                    format!(
                                        "name diverges from canonical \"{}\" (resolved outside scope)",
                                        ec.name
                                    ),
                                ));
                            }
                            // Only emit ORPHAN for an external
                            // canonical when the resolver positively
                            // knows the marker is absent. `None`
                            // means "the resolver didn't fetch the
                            // description" and must not classify.
                            if ec.has_canonical_marker == Some(false) {
                                findings.push(mk(
                                    "ORPHAN",
                                    n,
                                    format!("canonical {} has no canonical_of: marker", ec.id),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    for cid in &canonical_targets {
        if !mirrors_by_target.keys().any(|t| id_match(t, cid)) {
            if let Some(canon) = nodes.iter().find(|n| &n.id.to_lowercase() == cid) {
                findings.push(mk(
                    "LONELY",
                    canon,
                    "canonical has no mirrors (may be intentional)".into(),
                ));
            }
        }
    }
    findings
}

/// Build the four review-surface buckets. `today` and `now_unix` are
/// passed in to keep the function deterministic (tests can supply a
/// fixed clock). `source_moc_blob` is the concatenated text of recent
/// session-log files; pass `""` to skip bucket (d).
pub fn build_review(
    nodes: &[WorkflowyNode],
    days_stale: i64,
    today: chrono::NaiveDate,
    now_unix: i64,
    source_moc_blob: &str,
) -> ReviewReport {
    let stale_cutoff = now_unix - days_stale * SECONDS_PER_DAY;
    let mut report = ReviewReport {
        revisit_due: vec![],
        multi_pillar: vec![],
        stale_cross_pillar: vec![],
        source_moc_reuse: vec![],
    };
    let date_re = regex::Regex::new(r"revisit_due:\s*(\d{4}-\d{2}-\d{2})").unwrap();
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
        // (b) multi-pillar: max of mirror_of count and pillar-tag count
        let mirror_of_count =
            desc.matches("mirror_of:").count() + desc.matches("#mirrored_in:").count();
        let pillar_count = PILLAR_TAGS.iter().filter(|t| combined.contains(*t)).count();
        let max_signal = mirror_of_count.max(pillar_count);
        if max_signal >= 3 {
            report.multi_pillar.push(ReviewItem {
                node_id: n.id.clone(),
                name: n.name.clone(),
                detail: format!(
                    "signal={} (mirror_of={}, pillars={})",
                    max_signal, mirror_of_count, pillar_count
                ),
            });
        }
        // (c) stale cross-pillar concept maps
        let is_cross_pillar = pillar_count >= 2
            || combined.contains("cross-pillar")
            || combined.contains("concept map");
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
    // (d) Source-MOC re-cited: scan descriptions for URL/DOI strings
    // that appear in the supplied recent-logs blob. Caller controls
    // what counts as "recent" by what they include in the blob.
    if !source_moc_blob.is_empty() {
        let url_re = regex::Regex::new(r"https?://\S+|10\.\d{4,9}/\S+").unwrap();
        let year_re = regex::Regex::new(r"\b(19|20)\d{2}\b").unwrap();
        for n in nodes {
            let name_lower = n.name.to_lowercase();
            let looks_like_source_moc = name_lower.contains(" — ") || year_re.is_match(&n.name);
            if !looks_like_source_moc {
                continue;
            }
            let desc = n.description.as_deref().unwrap_or("");
            for m in url_re.find_iter(desc) {
                if source_moc_blob.contains(m.as_str()) {
                    report.source_moc_reuse.push(ReviewItem {
                        node_id: n.id.clone(),
                        name: n.name.clone(),
                        detail: format!("re-cited recently: {}", m.as_str()),
                    });
                    break;
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, name: &str, desc: Option<&str>) -> WorkflowyNode {
        WorkflowyNode {
            id: id.to_string(),
            name: name.to_string(),
            description: desc.map(String::from),
            ..Default::default()
        }
    }

    fn node_with_modified(id: &str, name: &str, desc: Option<&str>, last_modified: i64) -> WorkflowyNode {
        WorkflowyNode {
            id: id.to_string(),
            name: name.to_string(),
            description: desc.map(String::from),
            last_modified: Some(last_modified),
            ..Default::default()
        }
    }

    #[test]
    fn extract_marker_returns_lowercased_capture() {
        let text = "mirror_of: 550E8400-e29b-41d4-A716-446655440000 plus other stuff";
        assert_eq!(
            extract_marker(text, "mirror_of:"),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn extract_marker_returns_none_when_absent() {
        assert!(extract_marker("nothing here", "canonical_of:").is_none());
    }

    // Test IDs must be hex-only — `extract_marker`'s regex is
    // `[0-9a-f-]{3,40}` so the fake UUIDs need to live in that
    // alphabet. `aaa`, `bbb`, `dad`, `bee`, `cab` are all valid hex
    // and short enough to make the test data readable.

    #[test]
    fn audit_flags_broken_mirror_when_target_not_in_scope() {
        let nodes = vec![node("aaa", "Mirror", Some("mirror_of:dad"))];
        let findings = audit_mirrors(&nodes);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, "BROKEN");
        assert_eq!(findings[0].node_id, "aaa");
    }

    #[test]
    fn audit_flags_drifted_when_mirror_name_differs_from_canonical() {
        let nodes = vec![
            node("aaa", "Original Title", Some("canonical_of:lead")),
            node("bbb", "Completely Different Name", Some("mirror_of:aaa")),
        ];
        let findings = audit_mirrors(&nodes);
        assert!(findings.iter().any(|f| f.status == "DRIFTED" && f.node_id == "bbb"),
                "expected DRIFTED on bbb, got: {:?}", findings);
    }

    #[test]
    fn audit_flags_orphan_when_canonical_lacks_marker() {
        let nodes = vec![
            node("aaa", "Title", None), // no canonical_of:
            node("bbb", "Title", Some("mirror_of:aaa")),
        ];
        let findings = audit_mirrors(&nodes);
        assert!(findings.iter().any(|f| f.status == "ORPHAN" && f.node_id == "bbb"),
                "expected ORPHAN on bbb, got: {:?}", findings);
    }

    #[test]
    fn audit_flags_lonely_when_canonical_has_no_mirrors() {
        let nodes = vec![node("aaa", "Title", Some("canonical_of:lead"))];
        let findings = audit_mirrors(&nodes);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, "LONELY");
    }

    #[test]
    fn mirror_of_8_char_doc_prefix_hash_resolves_in_scope() {
        // link_parser.rs documents the 8-char doc-form prefix hash as a
        // first-class short-hash shape alongside the 12-char URL-suffix
        // form. Pre-fix `id_match` only checked suffixes (`ends_with`),
        // so a `mirror_of:<8-char-prefix>` marker never resolved even
        // when its target was in the walked scope.
        let nodes = vec![
            node("aaaabbbbccccdddd", "Title", Some("canonical_of:lead")),
            node("eeee", "Title", Some("mirror_of:aaaabbbb")),
        ];
        let findings = audit_mirrors(&nodes);
        assert!(
            !findings.iter().any(|f| f.status == "BROKEN"),
            "8-char doc-prefix mirror_of must resolve against an in-scope canonical: {:?}",
            findings
        );
    }

    #[test]
    fn mirror_of_8_char_doc_prefix_hash_resolves_via_external_map() {
        let nodes = vec![node("eeee", "Title", Some("mirror_of:aaaabbbb"))];
        let mut external = HashMap::new();
        external.insert(
            "aaaabbbbccccdddd".to_string(),
            ExternalCanonical {
                id: "aaaabbbbccccdddd".to_string(),
                name: "Title".to_string(),
                has_canonical_marker: Some(true),
            },
        );
        let findings = audit_mirrors_with_external(&nodes, &external);
        assert!(
            !findings.iter().any(|f| f.status == "BROKEN"),
            "8-char doc-prefix mirror_of must resolve against an external canonical: {:?}",
            findings
        );
    }

    #[test]
    fn cross_pillar_mirror_classifies_ok_when_canonical_present_in_external_map() {
        // The scope only contains the mirror; the canonical lives in
        // some other pillar and is supplied through the external map.
        // Pre-2026-05-16 this was a false-positive BROKEN. After Fix
        // A it must be silent.
        let nodes = vec![node("bbb", "Distillation Title", Some("mirror_of:aaa"))];
        let mut external = HashMap::new();
        external.insert(
            "aaa".to_string(),
            ExternalCanonical {
                id: "aaa".to_string(),
                name: "Distillation Title".to_string(),
                has_canonical_marker: Some(true),
            },
        );
        let findings = audit_mirrors_with_external(&nodes, &external);
        assert!(
            findings.is_empty(),
            "cross-pillar mirror with resolvable canonical must not classify BROKEN: {:?}",
            findings
        );
    }

    #[test]
    fn cross_pillar_mirror_drifts_when_name_diverges_from_external() {
        let nodes = vec![node(
            "bbb",
            "Completely Different Name",
            Some("mirror_of:aaa"),
        )];
        let mut external = HashMap::new();
        external.insert(
            "aaa".to_string(),
            ExternalCanonical {
                id: "aaa".to_string(),
                name: "Original Title".to_string(),
                has_canonical_marker: Some(true),
            },
        );
        let findings = audit_mirrors_with_external(&nodes, &external);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, "DRIFTED");
        assert!(findings[0].issue.contains("resolved outside scope"));
    }

    #[test]
    fn cross_pillar_mirror_orphan_only_when_marker_known_absent() {
        // marker_known_absent → ORPHAN
        let nodes = vec![node("bbb", "Title", Some("mirror_of:aaa"))];
        let mut external = HashMap::new();
        external.insert(
            "aaa".to_string(),
            ExternalCanonical {
                id: "aaa".to_string(),
                name: "Title".to_string(),
                has_canonical_marker: Some(false),
            },
        );
        let findings = audit_mirrors_with_external(&nodes, &external);
        assert!(
            findings.iter().any(|f| f.status == "ORPHAN" && f.node_id == "bbb"),
            "expected ORPHAN, got: {:?}", findings
        );

        // marker unknown (None) → no classification
        external.get_mut("aaa").unwrap().has_canonical_marker = None;
        let findings = audit_mirrors_with_external(&nodes, &external);
        assert!(
            findings.is_empty(),
            "unknown marker state must not classify: {:?}",
            findings
        );
    }

    #[test]
    fn unresolvable_mirror_still_classifies_broken_with_external_map() {
        let nodes = vec![node("bbb", "Title", Some("mirror_of:dad"))];
        let mut external = HashMap::new();
        external.insert(
            "cab".to_string(),
            ExternalCanonical {
                id: "cab".to_string(),
                name: "Other".to_string(),
                has_canonical_marker: Some(true),
            },
        );
        let findings = audit_mirrors_with_external(&nodes, &external);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, "BROKEN");
        assert!(findings[0].issue.contains("not found in scope or in name index"));
    }

    #[test]
    fn audit_clean_pair_produces_no_findings() {
        let nodes = vec![
            node("aaa", "Distillation Title", Some("canonical_of:lead")),
            node("bbb", "Distillation Title", Some("mirror_of:aaa")),
        ];
        let findings = audit_mirrors(&nodes);
        assert!(findings.is_empty(), "expected no findings, got: {:?}", findings);
    }

    #[test]
    fn review_buckets_revisit_node_when_due_date_past() {
        let nodes = vec![node(
            "n1",
            "old note #revisit",
            Some("revisit_due: 2020-01-01"),
        )];
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let report = build_review(&nodes, 90, today, 1_745_000_000, "");
        assert_eq!(report.revisit_due.len(), 1);
        assert_eq!(report.revisit_due[0].node_id, "n1");
    }

    #[test]
    fn review_skips_revisit_node_when_due_date_in_future() {
        let nodes = vec![node(
            "n1",
            "future note #revisit",
            Some("revisit_due: 2999-01-01"),
        )];
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let report = build_review(&nodes, 90, today, 1_745_000_000, "");
        assert!(report.revisit_due.is_empty());
    }

    #[test]
    fn review_buckets_multi_pillar_when_three_pillar_tags_present() {
        let nodes = vec![node(
            "n1",
            "synthesis touching #leadership #ethics #building",
            None,
        )];
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let report = build_review(&nodes, 90, today, 1_745_000_000, "");
        assert_eq!(report.multi_pillar.len(), 1);
        assert!(report.multi_pillar[0].detail.contains("pillars=3"));
    }

    #[test]
    fn review_takes_max_signal_not_sum_to_avoid_double_counting() {
        // Node has 2 pillar tags AND 2 mirror_of markers — should NOT be flagged
        // (max(2, 2) = 2, below the threshold of 3).
        let nodes = vec![node(
            "n1",
            "synthesis touching #leadership #ethics",
            Some("mirror_of:x mirror_of:y"),
        )];
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let report = build_review(&nodes, 90, today, 1_745_000_000, "");
        assert!(
            report.multi_pillar.is_empty(),
            "max(2,2)=2 should not flag: {:?}",
            report.multi_pillar
        );
    }

    #[test]
    fn review_buckets_stale_cross_pillar_when_last_modified_past_cutoff() {
        let now_unix: i64 = 1_745_000_000;
        let stale_at = now_unix - 100 * SECONDS_PER_DAY;
        let nodes = vec![node_with_modified(
            "n1",
            "Cross-pillar concept map: AI as cognitive participant",
            None,
            stale_at,
        )];
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let report = build_review(&nodes, 90, today, now_unix, "");
        assert_eq!(report.stale_cross_pillar.len(), 1);
        assert!(report.stale_cross_pillar[0].detail.contains("100 days ago"));
    }

    #[test]
    fn review_source_moc_reuse_matches_url_against_supplied_blob() {
        let nodes = vec![node(
            "n1",
            "Horaguchi 2025 — Organization philosophy",
            Some("citation: https://link.springer.com/article/10.1007/s00146-024-01980-6"),
        )];
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let blob =
            "yesterday's session referenced https://link.springer.com/article/10.1007/s00146-024-01980-6";
        let report = build_review(&nodes, 90, today, 1_745_000_000, blob);
        assert_eq!(report.source_moc_reuse.len(), 1);
        assert!(report.source_moc_reuse[0].detail.contains("re-cited"));
    }

    #[test]
    fn review_source_moc_reuse_skipped_when_blob_empty() {
        let nodes = vec![node(
            "n1",
            "Horaguchi 2025 — Organization philosophy",
            Some("citation: https://example.com/paper"),
        )];
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 26).unwrap();
        let report = build_review(&nodes, 90, today, 1_745_000_000, "");
        assert!(report.source_moc_reuse.is_empty());
    }
}
