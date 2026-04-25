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
use std::time::{Duration, SystemTime};

/// TTL for opportunistic index entries. Matches the node cache so a stale
/// index can never outlive a stale node entry.
pub const NAME_INDEX_TTL_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameIndexEntry {
    pub node_id: String,
    pub name: String,
    pub parent_id: Option<String>,
}

#[derive(Debug)]
struct IndexedValue {
    entries: Vec<NameIndexEntry>,
    inserted_at: SystemTime,
}

#[derive(Debug, Default)]
pub struct NameIndex {
    by_name: RwLock<HashMap<String, IndexedValue>>,
    /// Reverse lookup: node_id -> lowercased name, so we can remove stale
    /// entries when the name changes or the node is deleted.
    by_id: RwLock<HashMap<String, String>>,
}

impl NameIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a batch of nodes, overwriting existing entries for each node.
    pub fn ingest(&self, nodes: &[WorkflowyNode]) {
        if nodes.is_empty() {
            return;
        }
        let now = SystemTime::now();
        let mut by_name = self.by_name.write();
        let mut by_id = self.by_id.write();
        for node in nodes {
            self.remove_locked(&mut by_name, &mut by_id, &node.id);
            let key = node.name.to_lowercase();
            let entry = NameIndexEntry {
                node_id: node.id.clone(),
                name: node.name.clone(),
                parent_id: node.parent_id.clone(),
            };
            by_id.insert(node.id.clone(), key.clone());
            by_name
                .entry(key)
                .and_modify(|v| {
                    v.entries.push(entry.clone());
                    v.inserted_at = now;
                })
                .or_insert_with(|| IndexedValue { entries: vec![entry], inserted_at: now });
        }
    }

    /// Look up nodes whose name matches `query` under the given mode. Always
    /// returns the entries known to the index — never falls back on the
    /// caller's behalf. Callers that need authoritative results must walk the
    /// tree themselves. Unknown match modes return an empty vec.
    pub fn lookup(&self, query: &str, match_mode: &str) -> Vec<NameIndexEntry> {
        let q = query.to_lowercase();
        let by_name = self.by_name.read();
        let ttl = Duration::from_secs(NAME_INDEX_TTL_SECS);
        let now = SystemTime::now();
        let fresh = |entry: &IndexedValue| {
            now.duration_since(entry.inserted_at).unwrap_or_default() <= ttl
        };

        let mut out = Vec::new();
        match match_mode {
            "exact" => {
                if let Some(hit) = by_name.get(&q) {
                    if fresh(hit) {
                        out.extend(hit.entries.iter().cloned());
                    }
                }
            }
            "starts_with" | "contains" => {
                for (key, value) in by_name.iter() {
                    if !fresh(value) {
                        continue;
                    }
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

    /// True when the index contains any entries (fresh or stale).
    pub fn is_populated(&self) -> bool {
        !self.by_id.read().is_empty()
    }

    /// Remove a single node's entry. Safe to call when the node is not indexed.
    pub fn invalidate_node(&self, node_id: &str) {
        let mut by_name = self.by_name.write();
        let mut by_id = self.by_id.write();
        self.remove_locked(&mut by_name, &mut by_id, node_id);
    }

    /// Drop every entry.
    pub fn clear(&self) {
        self.by_name.write().clear();
        self.by_id.write().clear();
    }

    pub fn size(&self) -> usize {
        self.by_id.read().len()
    }

    fn remove_locked(
        &self,
        by_name: &mut HashMap<String, IndexedValue>,
        by_id: &mut HashMap<String, String>,
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
    }
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
}
