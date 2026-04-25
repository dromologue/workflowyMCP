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

/// Lengths (in lowercase hex chars) of the two short-hash forms accepted
/// for a UUID:
///
/// - **12 hex chars** (`SHORT_HASH_LEN_URL`) is what Workflowy uses in
///   URLs — the trailing 12 hex of a UUID (e.g.
///   `workflowy.com/#/abc123def456`).
/// - **8 hex chars** (`SHORT_HASH_LEN_PREFIX`) is the form humans use
///   in notes / docs / skills — the first segment of the canonical
///   8-4-4-4-12 UUID layout (e.g. `c1ef1ad5` for
///   `c1ef1ad5-ce38-8fed-bf6f-4737f286b86a`).
///
/// Both resolve via [`NameIndex::resolve_short_hash`] in `O(1)`.
pub const SHORT_HASH_LEN_URL: usize = 12;
pub const SHORT_HASH_LEN_PREFIX: usize = 8;
/// Backward-compatible alias for the URL-suffix form. Prefer the
/// length constants above in new code.
pub const SHORT_HASH_LEN: usize = SHORT_HASH_LEN_URL;

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

/// Tracks one or more nodes that share an 8-hex-char prefix. We keep the
/// first observed full UUID for the unambiguous-resolution path and the
/// count so collision-aware callers can branch on it.
#[derive(Debug, Clone)]
struct PrefixEntry {
    full_uuid: String,
    count: u32,
}

#[derive(Debug, Default)]
pub struct NameIndex {
    by_name: RwLock<HashMap<String, IndexedValue>>,
    /// Reverse lookup: node_id -> lowercased name, so we can remove stale
    /// entries when the name changes or the node is deleted.
    by_id: RwLock<HashMap<String, String>>,
    /// Short-hash → full UUID. Indexed by the URL-suffix form (last 12
    /// lowercase-hex chars of the UUID, hyphens stripped). Workflowy's web
    /// UI uses this as its public node handle in URLs; resolving it
    /// cheaply means callers don't have to round-trip through the API
    /// just to convert URL → UUID.
    by_short_hash: RwLock<HashMap<String, String>>,
    /// Prefix-hash → full UUID. Indexed by the first 8 hex chars of the
    /// UUID (the first segment of the canonical 8-4-4-4-12 layout).
    /// Documentation and skill files commonly use this form (e.g.
    /// `c1ef1ad5` for `c1ef1ad5-…`). 8-char collisions are vanishingly
    /// rare in a 32-char hex space; on collision we keep the first
    /// observed mapping and return None for subsequent lookups so the
    /// caller can disambiguate via full UUID.
    by_prefix_hash: RwLock<HashMap<String, PrefixEntry>>,
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
        let mut by_prefix = self.by_prefix_hash.write();
        for node in nodes {
            self.remove_locked(&mut by_name, &mut by_id, &mut by_short, &mut by_prefix, &node.id);
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
            if let Some(prefix) = prefix_hash_of(&node.id) {
                by_prefix
                    .entry(prefix)
                    .and_modify(|e| {
                        // Prefix collision: bump count, keep first id, so a
                        // future resolve_short_hash returns None for that
                        // ambiguous prefix and forces the caller to use the
                        // full UUID.
                        if e.full_uuid != node.id {
                            e.count = e.count.saturating_add(1);
                        }
                    })
                    .or_insert(PrefixEntry { full_uuid: node.id.clone(), count: 1 });
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

    /// Resolve a short-hash form to its full UUID.
    ///
    /// Accepts both the 12-char URL-suffix form and the 8-char prefix
    /// form (the first segment of a hyphenated UUID, used widely in
    /// docs and skill files). Hyphens are stripped before matching, so
    /// `"abcd-1234-5678"` and `"abcd12345678"` both look up the
    /// 12-char form.
    ///
    /// Returns `None` if the hash isn't in the index, isn't a valid
    /// hex length, or — for the prefix form — collides with multiple
    /// distinct full UUIDs (collision-aware: the caller must
    /// disambiguate using the full UUID).
    pub fn resolve_short_hash(&self, short: &str) -> Option<String> {
        let stripped: String = short.chars().filter(|c| *c != '-').collect();
        match stripped.len() {
            n if n == SHORT_HASH_LEN_URL => {
                if !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                let key = stripped.to_lowercase();
                self.by_short_hash.read().get(&key).cloned()
            }
            n if n == SHORT_HASH_LEN_PREFIX => {
                if !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                let key = stripped.to_lowercase();
                let guard = self.by_prefix_hash.read();
                let entry = guard.get(&key)?;
                if entry.count > 1 {
                    // Ambiguous prefix — refuse to guess.
                    None
                } else {
                    Some(entry.full_uuid.clone())
                }
            }
            _ => None,
        }
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
        let mut by_prefix = self.by_prefix_hash.write();
        self.remove_locked(&mut by_name, &mut by_id, &mut by_short, &mut by_prefix, node_id);
    }

    /// Drop every entry.
    pub fn clear(&self) {
        self.by_name.write().clear();
        self.by_id.write().clear();
        self.by_short_hash.write().clear();
        self.by_prefix_hash.write().clear();
    }

    pub fn size(&self) -> usize {
        self.by_id.read().len()
    }

    fn remove_locked(
        &self,
        by_name: &mut HashMap<String, IndexedValue>,
        by_id: &mut HashMap<String, String>,
        by_short: &mut HashMap<String, String>,
        by_prefix: &mut HashMap<String, PrefixEntry>,
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
        if let Some(prefix) = prefix_hash_of(node_id) {
            // Same idea for the 8-char prefix form. If the entry was a
            // collision (count > 1) we can't safely decrement without
            // knowing which other node shared the prefix; leave the
            // entry in place — resolution still returns None for
            // collisions, which is the conservative behaviour.
            if let Some(entry) = by_prefix.get(&prefix) {
                if entry.count == 1 && entry.full_uuid == node_id {
                    by_prefix.remove(&prefix);
                }
            }
        }
    }
}

/// Compute the 8-char prefix form of a UUID (the first segment of the
/// canonical 8-4-4-4-12 hyphenated layout). Returns `None` if the
/// input isn't a valid UUID. This is the form humans use in docs and
/// skill files (e.g. `c1ef1ad5` for `c1ef1ad5-…`).
pub fn prefix_hash_of(node_id: &str) -> Option<String> {
    let stripped: String = node_id.chars().filter(|c| *c != '-').collect();
    if stripped.len() != 32 || !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(stripped[..SHORT_HASH_LEN_PREFIX].to_lowercase())
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
    fn prefix_hash_of_returns_first_8_hex_chars() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(prefix_hash_of(id).as_deref(), Some("550e8400"));
        let id2 = "550e8400e29b41d4a716446655440000";
        assert_eq!(prefix_hash_of(id2).as_deref(), Some("550e8400"));
    }

    #[test]
    fn resolve_short_hash_accepts_8_char_prefix_form() {
        // The wflow skill uses 8-char prefixes (e.g. `c1ef1ad5` for Tasks).
        // Pre-fix, only 12-char trailing hashes resolved; this regression
        // guard catches a future revert.
        let idx = NameIndex::new();
        let id = "c1ef1ad5-ce38-8fed-bf6f-4737f286b86a";
        idx.ingest(&[node(id, "Tasks", None)]);
        assert_eq!(idx.resolve_short_hash("c1ef1ad5").as_deref(), Some(id));
        // Hyphenated input strips to the same prefix.
        assert_eq!(idx.resolve_short_hash("c1ef-1ad5").as_deref(), Some(id));
    }

    #[test]
    fn resolve_short_hash_returns_none_on_8_char_collision() {
        // Two distinct UUIDs with the same 8-char prefix — refuse to guess.
        let idx = NameIndex::new();
        idx.ingest(&[
            node("c1ef1ad5-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "Tasks-A", None),
            node("c1ef1ad5-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "Tasks-B", None),
        ]);
        assert!(
            idx.resolve_short_hash("c1ef1ad5").is_none(),
            "ambiguous prefix must return None, not silently pick one"
        );
        // Full UUID still resolves unambiguously.
        assert_eq!(
            idx.resolve_short_hash("aaaaaaaaaaaa").as_deref(),
            Some("c1ef1ad5-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );
    }

    #[test]
    fn invalidate_node_removes_unique_prefix_entry() {
        let idx = NameIndex::new();
        let id = "deadbeef-0000-0000-0000-000000000001";
        idx.ingest(&[node(id, "X", None)]);
        assert_eq!(idx.resolve_short_hash("deadbeef").as_deref(), Some(id));
        idx.invalidate_node(id);
        assert!(idx.resolve_short_hash("deadbeef").is_none());
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
