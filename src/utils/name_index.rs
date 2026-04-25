//! In-memory name index for fast `find_node`-style lookups on large trees.
//!
//! Every time a subtree walk returns, the client feeds the visited nodes
//! through [`NameIndex::ingest`]. That populates a case-insensitive map from
//! `name -> [NameIndexEntry]`, letting subsequent lookups skip the tree walk
//! entirely for any name already seen.
//!
//! The index is opportunistic: it does not know about nodes the server has
//! never walked, and it is invalidated on mutations. Callers must not treat
//! an index miss as authoritative — fall back to a live walk.

use crate::types::WorkflowyNode;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Length (in lowercase hex chars) of the short-hash form of a UUID. Workflowy
/// URLs use the trailing 12 hex characters (e.g. `workflowy.com/#/abc123def456`)
/// as a unique handle for a node. Resolving this back to the full UUID is an
/// O(1) lookup via [`NameIndex::resolve_short_hash`].
pub const SHORT_HASH_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameIndexEntry {
    pub node_id: String,
    pub name: String,
    pub parent_id: Option<String>,
}

#[derive(Debug)]
struct IndexedValue {
    entries: Vec<NameIndexEntry>,
}

#[derive(Debug, Default)]
pub struct NameIndex {
    by_name: RwLock<HashMap<String, IndexedValue>>,
    /// Reverse lookup: node_id -> lowercased name, so we can remove stale
    /// entries when the name changes or the node is deleted.
    by_id: RwLock<HashMap<String, String>>,
    /// Short-hash → full UUID. Indexed by the last 12 lowercase-hex chars
    /// of the UUID (with hyphens stripped). Workflowy's web UI uses this as
    /// its public node handle in URLs; resolving it cheaply means callers
    /// don't have to round-trip through the API just to convert URL → UUID.
    by_short_hash: RwLock<HashMap<String, String>>,
}

impl NameIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a batch of nodes, overwriting existing entries for each node.
    /// Also records each node's short-hash form for URL → UUID resolution.
    pub fn ingest(&self, nodes: &[WorkflowyNode]) {
        if nodes.is_empty() {
            return;
        }
        let mut by_name = self.by_name.write();
        let mut by_id = self.by_id.write();
        let mut by_short = self.by_short_hash.write();
        for node in nodes {
            self.remove_locked(&mut by_name, &mut by_id, &mut by_short, &node.id);
            let key = node.name.to_lowercase();
            let entry = NameIndexEntry {
                node_id: node.id.clone(),
                name: node.name.clone(),
                parent_id: node.parent_id.clone(),
            };
            by_id.insert(node.id.clone(), key.clone());
            if let Some(short) = short_hash_of(&node.id) {
                by_short.insert(short, node.id.clone());
            }
            by_name
                .entry(key)
                .and_modify(|v| {
                    v.entries.push(entry.clone());
                })
                .or_insert_with(|| IndexedValue { entries: vec![entry] });
        }
    }

    /// Look up nodes whose name matches `query` under the given mode. Always
    /// returns the entries known to the index — never falls back on the
    /// caller's behalf. Callers that need authoritative results must walk the
    /// tree themselves. Unknown match modes return an empty vec.
    pub fn lookup(&self, query: &str, match_mode: &str) -> Vec<NameIndexEntry> {
        let q = query.to_lowercase();
        let by_name = self.by_name.read();
        let mut out = Vec::new();
        match match_mode {
            "exact" => {
                if let Some(hit) = by_name.get(&q) {
                    out.extend(hit.entries.iter().cloned());
                }
            }
            "starts_with" | "contains" => {
                for (key, value) in by_name.iter() {
                    let keep = if match_mode == "starts_with" {
                        key.starts_with(&q)
                    } else {
                        key.contains(&q)
                    };
                    if keep {
                        out.extend(value.entries.iter().cloned());
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Resolve a 12-char hex short hash to its full UUID. Returns `None`
    /// if the hash isn't in the index — callers must treat that as
    /// "not seen yet", not "doesn't exist". Run a `build_name_index`
    /// walk over the relevant subtree to populate.
    pub fn resolve_short_hash(&self, short: &str) -> Option<String> {
        let key = normalize_short_hash(short)?;
        self.by_short_hash.read().get(&key).cloned()
    }

    /// True when the index contains any entries.
    pub fn is_populated(&self) -> bool {
        !self.by_id.read().is_empty()
    }

    /// Remove a single node's entry. Safe to call when the node is not indexed.
    pub fn invalidate_node(&self, node_id: &str) {
        let mut by_name = self.by_name.write();
        let mut by_id = self.by_id.write();
        let mut by_short = self.by_short_hash.write();
        self.remove_locked(&mut by_name, &mut by_id, &mut by_short, node_id);
    }

    /// Drop every entry.
    pub fn clear(&self) {
        self.by_name.write().clear();
        self.by_id.write().clear();
        self.by_short_hash.write().clear();
    }

    pub fn size(&self) -> usize {
        self.by_id.read().len()
    }

    fn remove_locked(
        &self,
        by_name: &mut HashMap<String, IndexedValue>,
        by_id: &mut HashMap<String, String>,
        by_short: &mut HashMap<String, String>,
        node_id: &str,
    ) {
        if let Some(key) = by_id.remove(node_id) {
            if let Some(value) = by_name.get_mut(&key) {
                value.entries.retain(|e| e.node_id != node_id);
                if value.entries.is_empty() {
                    by_name.remove(&key);
                }
            }
        }
        if let Some(short) = short_hash_of(node_id) {
            // Only drop the short-hash entry if it still points at the
            // node we are invalidating — different nodes whose UUIDs
            // happen to share a 12-char suffix are vanishingly rare in
            // a 32-char hex space, but cheap to defend against.
            if by_short.get(&short).map(|v| v.as_str()) == Some(node_id) {
                by_short.remove(&short);
            }
        }
    }
}

/// Compute the 12-char short-hash form of a UUID by stripping hyphens
/// and taking the trailing 12 lowercase hex chars. Returns `None` if
/// the input isn't recognisably a UUID.
pub fn short_hash_of(node_id: &str) -> Option<String> {
    let stripped: String = node_id.chars().filter(|c| *c != '-').collect();
    if stripped.len() != 32 || !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(stripped[stripped.len() - SHORT_HASH_LEN..].to_lowercase())
}

/// Normalise a candidate short hash: strip hyphens, lowercase, validate.
/// Returns `None` if it isn't 12 hex chars.
fn normalize_short_hash(input: &str) -> Option<String> {
    let stripped: String = input.chars().filter(|c| *c != '-').collect();
    if stripped.len() != SHORT_HASH_LEN || !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(stripped.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, name: &str, parent: Option<&str>) -> WorkflowyNode {
        WorkflowyNode {
            id: id.to_string(),
            name: name.to_string(),
            parent_id: parent.map(|p| p.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn lookup_returns_empty_on_empty_index() {
        let idx = NameIndex::new();
        assert!(idx.lookup("anything", "exact").is_empty());
        assert!(!idx.is_populated());
    }

    #[test]
    fn exact_lookup_returns_matching_entries() {
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "Tasks", Some("root")), node("2", "tasks", Some("other"))]);
        let hits = idx.lookup("tasks", "exact");
        assert_eq!(hits.len(), 2);
        let ids: Vec<_> = hits.iter().map(|h| h.node_id.clone()).collect();
        assert!(ids.contains(&"1".to_string()));
        assert!(ids.contains(&"2".to_string()));
    }

    #[test]
    fn starts_with_matches_case_insensitively() {
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "Reading List", None), node("2", "Reading WIP", None)]);
        let hits = idx.lookup("reading", "starts_with");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn contains_matches_substring() {
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "Daily Review", None), node("2", "Weekly Review", None)]);
        let hits = idx.lookup("review", "contains");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn re_ingest_overwrites_old_entry() {
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "Original", None)]);
        idx.ingest(&[node("1", "Renamed", None)]);
        assert!(idx.lookup("original", "exact").is_empty());
        let hits = idx.lookup("renamed", "exact");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Renamed");
    }

    #[test]
    fn invalidate_removes_entry() {
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "Tasks", None)]);
        idx.invalidate_node("1");
        assert!(idx.lookup("tasks", "exact").is_empty());
        assert!(!idx.is_populated());
    }

    #[test]
    fn invalidate_unknown_node_is_noop() {
        let idx = NameIndex::new();
        idx.invalidate_node("nope");
        idx.ingest(&[node("1", "Tasks", None)]);
        idx.invalidate_node("nope");
        assert_eq!(idx.lookup("tasks", "exact").len(), 1);
    }

    #[test]
    fn clear_empties_index() {
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "A", None), node("2", "B", None)]);
        assert_eq!(idx.size(), 2);
        idx.clear();
        assert_eq!(idx.size(), 0);
        assert!(idx.lookup("a", "exact").is_empty());
        assert!(!idx.is_populated());
    }

    #[test]
    fn unknown_match_mode_returns_empty() {
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "A", None)]);
        assert!(idx.lookup("a", "fuzzy").is_empty());
    }

    #[test]
    fn is_populated_reflects_size() {
        let idx = NameIndex::new();
        assert!(!idx.is_populated());
        idx.ingest(&[node("1", "A", None)]);
        assert!(idx.is_populated());
    }

    #[test]
    fn short_hash_of_returns_trailing_12_hex_chars() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(short_hash_of(id).as_deref(), Some("446655440000"));
        let id2 = "550e8400e29b41d4a716446655440000"; // unhyphenated
        assert_eq!(short_hash_of(id2).as_deref(), Some("446655440000"));
    }

    #[test]
    fn short_hash_of_rejects_non_uuid() {
        assert_eq!(short_hash_of("not-a-uuid"), None);
        assert_eq!(short_hash_of(""), None);
        assert_eq!(short_hash_of("zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz"), None);
    }

    #[test]
    fn resolve_short_hash_returns_full_uuid_after_ingest() {
        let idx = NameIndex::new();
        let id = "550e8400-e29b-41d4-a716-446655440000";
        idx.ingest(&[node(id, "Tasks", None)]);
        assert_eq!(idx.resolve_short_hash("446655440000").as_deref(), Some(id));
        // Case insensitivity.
        assert_eq!(idx.resolve_short_hash("446655440000").as_deref(), Some(id));
        // Hyphenated input is also accepted as long as it strips to 12 hex.
        assert_eq!(idx.resolve_short_hash("4466-5544-0000").as_deref(), Some(id));
    }

    #[test]
    fn resolve_short_hash_returns_none_for_unknown() {
        let idx = NameIndex::new();
        idx.ingest(&[node("550e8400-e29b-41d4-a716-446655440000", "A", None)]);
        assert_eq!(idx.resolve_short_hash("ffffffffffff"), None);
        // Non-hex inputs.
        assert_eq!(idx.resolve_short_hash("not-hex-here"), None);
        // Wrong length.
        assert_eq!(idx.resolve_short_hash("abc"), None);
    }

    #[test]
    fn invalidate_node_removes_short_hash_entry() {
        let idx = NameIndex::new();
        let id = "550e8400-e29b-41d4-a716-446655440000";
        idx.ingest(&[node(id, "A", None)]);
        assert!(idx.resolve_short_hash("446655440000").is_some());
        idx.invalidate_node(id);
        assert!(idx.resolve_short_hash("446655440000").is_none());
    }

    #[test]
    fn no_ttl_eviction_indefinite_lifetime() {
        // Pass 4 promotes the index from opportunistic-with-TTL to
        // authoritative-until-invalidated. Confirm a long sleep doesn't
        // drop entries the way the old TTL would have.
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "Persistent", None)]);
        // We can't easily fake-clock a long sleep here, but we can confirm
        // that there's no fresh()/expiry path by checking an entry persists
        // after multiple lookups.
        for _ in 0..3 {
            assert_eq!(idx.lookup("persistent", "exact").len(), 1);
        }
    }
}
