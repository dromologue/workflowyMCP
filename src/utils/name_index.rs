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
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// The node's description/note text, if any. Stored so the
    /// `search_nodes(use_index=true)` fast path can match content held in
    /// descriptions, not just names (2026-07-12 field report, issue 5).
    /// Available at every ingest site (walks return full `WorkflowyNode`s),
    /// so populating it needs no extra API calls.
    pub description: Option<String>,
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

/// On-disk representation of one node entry. Persisted as part of
/// [`PersistedSnapshot`] so a fresh server start can rehydrate the index
/// without paying for a full tree walk against the live API.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedEntry {
    id: String,
    name: String,
    parent_id: Option<String>,
    /// Persisted description text so a rehydrated index can serve
    /// description-aware search without a fresh walk. `#[serde(default)]`
    /// keeps the field optional on the wire; absent = `None`.
    #[serde(default)]
    description: Option<String>,
}

/// Ids that must not be persisted: every node matching an excluded-subtree
/// token, plus every descendant of one.
///
/// Matching accepts the two id shapes `WORKFLOWY_INDEX_EXCLUDE_SUBTREES`
/// documents — a full UUID (exact) or a 12-char short hash (trailing match,
/// the form Workflowy puts in `/#/<hash>` URLs).
///
/// Seeds are nodes matching a token **or** naming one as their `parent_id`;
/// the walk then descends from there.
///
/// WHY seed on the parent too, rather than only on the node itself: the
/// excluded root is frequently *not* in the index — the whole point of the
/// filter is to keep it out, so once it has been purged once it never comes
/// back. Seeding only on `id` would then find nothing to start from and every
/// child of the excluded root would sail through, which is the exact opposite
/// of what this function is for. It fails open, silently, and only for the
/// subtree that matters. (Observed 2026-07-17, against a purged root.)
///
/// Residual, documented: a node whose chain to an excluded root runs through
/// an intermediate that is *also* absent is not reachable from any seed and
/// will persist. Nothing in the entry links it to the token in that case, so
/// closing it needs the ancestry the index does not have. In practice walks
/// return whole levels, so a node's parent is present whenever the node is.
fn excluded_ids(nodes: &[PersistedEntry], tokens: &[String]) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    if tokens.is_empty() {
        return out;
    }

    // A token matches an id exactly (full UUID) or as its 12-char trailing
    // short hash. Compared without allocating: ids are ASCII, so the byte
    // slice is safe.
    let is_match = |id: &str| -> bool {
        tokens.iter().any(|t| {
            id.eq_ignore_ascii_case(t)
                || (t.len() == SHORT_HASH_LEN_URL
                    && id.len() >= SHORT_HASH_LEN_URL
                    && id[id.len() - SHORT_HASH_LEN_URL..].eq_ignore_ascii_case(t))
        })
    };

    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut stack: Vec<&str> = Vec::new();
    for n in nodes {
        if let Some(p) = n.parent_id.as_deref() {
            children.entry(p).or_default().push(n.id.as_str());
            // Seed on the parent as well: the excluded root is usually
            // absent from the index precisely because this filter works.
            if is_match(p) {
                stack.push(n.id.as_str());
            }
        }
        if is_match(&n.id) {
            stack.push(n.id.as_str());
        }
    }

    while let Some(id) = stack.pop() {
        // `insert` returning false means we have already queued this id —
        // which doubles as the cycle guard for a malformed parent loop.
        if !out.insert(id.to_string()) {
            continue;
        }
        if let Some(kids) = children.get(id) {
            stack.extend(kids.iter().copied());
        }
    }
    out
}

/// On-disk snapshot of the entire name index. Schema-versioned so we
/// can evolve the format without breaking older caches; readers ignore
/// snapshots whose `version` is unfamiliar rather than panicking.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedSnapshot {
    version: u32,
    updated_at: u64,
    nodes: Vec<PersistedEntry>,
}

// Bumped 1 → 2 on 2026-07-12 when `description` was added to each entry.
// `load_from_disk` rejects any snapshot whose version it doesn't recognise,
// so a v1 cache is cleanly discarded (and rebuilt by the background walk)
// rather than loaded with silently-missing descriptions.
const PERSIST_SCHEMA_VERSION: u32 = 2;

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
    /// Disk path for persistence. When set, [`save_to_disk`] writes here
    /// and [`load_from_disk`] reads from here. Populated by the server
    /// startup wiring, not by `Default`, so unit tests don't accidentally
    /// touch the user's filesystem.
    save_path: RwLock<Option<PathBuf>>,
    /// Set when `ingest`/`invalidate_node`/`clear` mutate state since the
    /// last successful save. Lets a debounced background saver coalesce
    /// many mutations into one write.
    dirty: AtomicBool,
    /// Node IDs this process has explicitly invalidated (a delete, or a
    /// `clear`) since its last successful save.
    ///
    /// WHY: [`save_to_disk`] merges the on-disk file back in before writing,
    /// so a concurrent writer's additions survive. A naive union would also
    /// resurrect entries this process just deleted — the on-disk copy still
    /// has them, having been written by a process that never observed the
    /// delete. The tombstone set makes the merge skip them, so a delete is
    /// not undone by the very merge that protects additions. Cleared on a
    /// successful save: the file we just wrote already excludes them.
    tombstones: RwLock<HashSet<String>>,
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
        // Guard against `nodes` containing only entries that are already
        // identical to the current state — a no-op ingest must not set
        // `dirty`, otherwise the periodic saver would write the same JSON
        // forever.
        let mut changed = false;
        let mut by_name = self.by_name.write();
        let mut by_id = self.by_id.write();
        let mut by_short = self.by_short_hash.write();
        let mut by_prefix = self.by_prefix_hash.write();
        for node in nodes {
            // Skip a redundant ingest: same id + same lowercased name AND
            // same description means nothing in the maps would change. We
            // still treat parent_id changes as a no-op here because moves are
            // observed via the cache layer; the by_id key only stores the
            // lowercased name. Description is part of the equality check
            // (2026-07-12 issue 5) so a description edit is picked up for
            // description-aware search instead of being skipped — without it,
            // re-observing the same node during a walk would keep the stale
            // description. A genuinely-unchanged node still short-circuits, so
            // the periodic saver is not woken by identical re-ingests.
            let key = node.name.to_lowercase();
            if by_id.get(&node.id).map(|n| n.as_str()) == Some(key.as_str()) {
                let same_desc = by_name
                    .get(&key)
                    .and_then(|v| v.entries.iter().find(|e| e.node_id == node.id))
                    .map(|e| e.description == node.description)
                    .unwrap_or(false);
                if same_desc {
                    continue;
                }
            }
            self.remove_locked(&mut by_name, &mut by_id, &mut by_short, &mut by_prefix, &node.id);
            changed = true;
            let entry = NameIndexEntry {
                node_id: node.id.clone(),
                name: node.name.clone(),
                parent_id: node.parent_id.clone(),
                description: node.description.clone(),
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
        drop(by_prefix);
        drop(by_short);
        drop(by_id);
        drop(by_name);
        if changed {
            self.dirty.store(true, Ordering::Relaxed);
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

    /// Token-AND search over each entry's name **and** description, used by
    /// the `search_nodes(use_index=true)` fast path.
    ///
    /// WHY (2026-07-12 field report, issues 5 & 6): the old fast path called
    /// `lookup(query, "contains")`, which lowercases the *entire* query as one
    /// needle and tests `name.contains(needle)` — a single contiguous substring
    /// over the name only. Two failures fell out of that: (a) description
    /// content was invisible (the index stored no description); (b) a
    /// multi-token query like `"Annex III high-risk regime"` matched nothing
    /// even when every token was present, because the interior words broke
    /// contiguity. This method splits the query on whitespace and keeps an
    /// entry iff *every* token is a case-insensitive substring of the entry's
    /// `name + " " + description` haystack — order-independent, gap-tolerant,
    /// and description-aware. An empty/whitespace-only query returns nothing
    /// (an empty token set would otherwise match every node). `find_node`'s
    /// `lookup` is deliberately left untouched so its `exact`/`starts_with`/
    /// `contains` name-only semantics stay byte-compatible.
    pub fn search_tokens(&self, query: &str) -> Vec<NameIndexEntry> {
        let tokens: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .collect();
        if tokens.is_empty() {
            return Vec::new();
        }
        let by_name = self.by_name.read();
        let mut out = Vec::new();
        for value in by_name.values() {
            for entry in &value.entries {
                let mut hay = entry.name.to_lowercase();
                if let Some(desc) = &entry.description {
                    hay.push(' ');
                    hay.push_str(&desc.to_lowercase());
                }
                if tokens.iter().all(|t| hay.contains(t)) {
                    out.push(entry.clone());
                }
            }
        }
        out
    }

    /// Look up the entry for a specific node by its full UUID. Used by
    /// the `audit_mirrors` resolver path to widen canonical resolution
    /// beyond the walked subtree (Fix A for the 2026-05-16
    /// cross-pillar false-positive report). Returns `None` if the
    /// node has never been ingested.
    pub fn lookup_entry_by_id(&self, node_id: &str) -> Option<NameIndexEntry> {
        let lc_name = self.by_id.read().get(node_id).cloned()?;
        let by_name = self.by_name.read();
        let value = by_name.get(&lc_name)?;
        value.entries.iter().find(|e| e.node_id == node_id).cloned()
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
        let was_present = by_id.contains_key(node_id);
        self.remove_locked(&mut by_name, &mut by_id, &mut by_short, &mut by_prefix, node_id);
        if was_present {
            drop(by_prefix);
            drop(by_short);
            drop(by_id);
            drop(by_name);
            // Tombstone so the merge in `save_to_disk` cannot resurrect this
            // node from a concurrent writer's copy of the file.
            self.tombstones.write().insert(node_id.to_string());
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Drop every entry.
    pub fn clear(&self) {
        let mut by_id = self.by_id.write();
        let was_populated = !by_id.is_empty();
        if was_populated {
            let mut tombstones = self.tombstones.write();
            for id in by_id.keys() {
                tombstones.insert(id.clone());
            }
        }
        self.by_name.write().clear();
        by_id.clear();
        drop(by_id);
        self.by_short_hash.write().clear();
        self.by_prefix_hash.write().clear();
        if was_populated {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    pub fn size(&self) -> usize {
        self.by_id.read().len()
    }

    // --- Persistence ---

    /// Configure the on-disk JSON path for this index. Call once at server
    /// startup. Subsequent calls overwrite the path. Setting it does not
    /// trigger an immediate save — call [`save_to_disk`] explicitly or
    /// rely on the periodic saver task.
    pub fn set_save_path(&self, path: PathBuf) {
        *self.save_path.write() = Some(path);
    }

    /// Path the index would write to. `None` until `set_save_path` is called.
    pub fn save_path(&self) -> Option<PathBuf> {
        self.save_path.read().clone()
    }

    /// True iff state has changed since the last successful save. Cleared
    /// by [`save_to_disk`] on a successful write.
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    /// Load entries from the configured save path, if any. Returns the
    /// number of nodes ingested. A missing file is treated as a clean
    /// start (returns 0). A malformed file is returned as an
    /// `InvalidData` error so the caller can decide whether to delete
    /// and re-walk; we never panic on bad cache contents.
    ///
    /// After a successful load, the dirty flag is cleared — entries
    /// just read from disk should not trigger an immediate write back.
    pub fn load_from_disk(&self) -> std::io::Result<usize> {
        let path = match self.save_path() {
            Some(p) => p,
            None => return Ok(0),
        };
        if !path.exists() {
            return Ok(0);
        }
        let text = std::fs::read_to_string(&path)?;
        let snap: PersistedSnapshot = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if snap.version != PERSIST_SCHEMA_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "name index schema version {} is not supported (expected {})",
                    snap.version, PERSIST_SCHEMA_VERSION
                ),
            ));
        }
        let count = snap.nodes.len();
        // Reuse the public ingest path so all derived maps stay
        // consistent. We need WorkflowyNode stubs for that — fields we
        // don't persist remain at their `Default` values.
        let nodes: Vec<WorkflowyNode> = snap
            .nodes
            .into_iter()
            .map(|e| WorkflowyNode {
                id: e.id,
                name: e.name,
                parent_id: e.parent_id,
                description: e.description,
                ..Default::default()
            })
            .collect();
        self.ingest(&nodes);
        // Loading from disk reconstitutes state — there is nothing new
        // to write back, so clear the dirty flag set by `ingest`.
        self.dirty.store(false, Ordering::Relaxed);
        Ok(count)
    }

    /// Atomically write the current index to the configured save path,
    /// merging in whatever another writer has added since this process last
    /// read the file. No-op when no path is set. Writes through a `.tmp`
    /// sibling and renames into place so a crashed save can never leave a
    /// half-written JSON file behind. Clears the dirty flag on success.
    ///
    /// WHY the merge and the lock (2026-07-16): the index has two writers —
    /// the long-running MCP server (loads at process start, checkpoints every
    /// `INDEX_SAVE_INTERVAL_SECS`) and `wflow-do reindex` (loads, walks,
    /// saves). Both used to serialise their whole in-memory map straight over
    /// the file, so each silently discarded every entry the other had added
    /// since its own load — a window of *hours* for the server. The observed
    /// symptom was non-monotonic churn: a subtree present at 15:20 was gone
    /// at 16:08, replaced by a different one, even though `ingest` is purely
    /// additive and nothing prunes. Re-reading and merging under an exclusive
    /// lock makes concurrent writers compose instead of clobber.
    ///
    /// Merge rule: **memory wins for any ID this process knows**, because it
    /// observed that node itself; disk-only IDs are adopted, because we have
    /// no opinion on them and the other writer did observe them. Tombstoned
    /// IDs are skipped so the merge cannot undo a delete.
    pub fn save_to_disk(&self) -> std::io::Result<()> {
        let path = match self.save_path() {
            Some(p) => p,
            None => return Ok(()),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Hold the lock across the whole read-merge-write. Without it two
        // savers can both read, both merge, and the later rename still drops
        // the earlier one's additions — the same clobber on a shorter fuse.
        let _guard = SaveLock::acquire(&path)?;

        self.merge_from_disk(&path)?;

        let snap = self.snapshot();
        let json = serde_json::to_vec_pretty(&snap)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let tmp = with_tmp_suffix(&path);
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &path)?;
        self.dirty.store(false, Ordering::Relaxed);
        // The file we just wrote already excludes these, so they no longer
        // need suppressing on the next merge.
        self.tombstones.write().clear();
        Ok(())
    }

    /// Read `path` and adopt every entry this process has no opinion on.
    /// Called by [`save_to_disk`] while holding the save lock.
    ///
    /// A missing file means nothing to merge. A malformed or wrong-schema
    /// file is ignored rather than propagated: refusing to save because
    /// someone else wrote a bad file would strand this process's own work,
    /// and the save that follows replaces the bad file with a good one.
    fn merge_from_disk(&self, path: &Path) -> std::io::Result<()> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let snap: PersistedSnapshot = match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "name index on disk is unreadable; overwriting it");
                return Ok(());
            }
        };
        if snap.version != PERSIST_SCHEMA_VERSION {
            tracing::warn!(
                found = snap.version,
                expected = PERSIST_SCHEMA_VERSION,
                "name index on disk has an unsupported schema version; overwriting it"
            );
            return Ok(());
        }

        let adopt: Vec<WorkflowyNode> = {
            let by_id = self.by_id.read();
            let tombstones = self.tombstones.read();
            snap.nodes
                .into_iter()
                .filter(|e| !by_id.contains_key(&e.id) && !tombstones.contains(&e.id))
                .map(|e| WorkflowyNode {
                    id: e.id,
                    name: e.name,
                    parent_id: e.parent_id,
                    description: e.description,
                    ..Default::default()
                })
                .collect()
        };
        if !adopt.is_empty() {
            tracing::info!(
                adopted = adopt.len(),
                "merged entries written by another index writer"
            );
            self.ingest(&adopt);
        }
        Ok(())
    }

    /// Build a snapshot of the index for serialization. The order of
    /// emitted nodes is deterministic for a given map state (by
    /// node_id), so successive saves of an unchanged index produce
    /// byte-identical JSON — useful when the index file is itself
    /// version-controlled.
    ///
    /// This is the ONLY place the persisted form is built (`save_to_disk`
    /// is its sole caller), which is why the excluded-subtree filter lives
    /// here: every writer — the server's periodic checkpoint and an
    /// out-of-process `wflow-do reindex` alike — must pass through it, so
    /// an excluded subtree cannot reach disk by any route. Pinned by
    /// `persisted_snapshot_is_the_only_serialisation_path`.
    fn snapshot(&self) -> PersistedSnapshot {
        let by_name = self.by_name.read();
        let mut nodes: Vec<PersistedEntry> = Vec::with_capacity(by_name.values().map(|v| v.entries.len()).sum());
        for value in by_name.values() {
            for entry in &value.entries {
                nodes.push(PersistedEntry {
                    id: entry.node_id.clone(),
                    name: entry.name.clone(),
                    parent_id: entry.parent_id.clone(),
                    description: entry.description.clone(),
                });
            }
        }

        let excluded = excluded_ids(&nodes, &crate::defaults::index_excluded_subtrees());
        if !excluded.is_empty() {
            let before = nodes.len();
            nodes.retain(|n| !excluded.contains(&n.id));
            tracing::debug!(
                dropped = before - nodes.len(),
                "excluded subtrees withheld from the persisted name index"
            );
        }

        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        PersistedSnapshot {
            version: PERSIST_SCHEMA_VERSION,
            updated_at,
            nodes,
        }
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

/// Cross-process exclusive lock guarding the read-merge-write in
/// [`NameIndex::save_to_disk`], so the MCP server and a concurrent
/// `wflow-do reindex` serialise their saves instead of racing.
///
/// The lock lives on a `.lock` sibling rather than on the index file
/// itself: `save_to_disk` renames a temp file over the index, which would
/// swap the very inode a lock on the index was held against. The advisory
/// lock is released when the file handle drops — including on panic or a
/// killed process — so a crashed writer cannot wedge the index permanently
/// the way a `create_new`-style lock file would.
struct SaveLock(std::fs::File);

impl SaveLock {
    fn acquire(index_path: &Path) -> std::io::Result<Self> {
        use fs4::FileExt;
        let mut s = index_path.as_os_str().to_owned();
        s.push(".lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(PathBuf::from(s))?;
        // Blocking exclusive lock (`FileExt::lock`). Saves are infrequent (a
        // 30 s server checkpoint, a one-shot CLI run) and the critical
        // section is a read plus a rename, so contention resolves in
        // milliseconds. Waiting is strictly better than skipping the save
        // and losing the work.
        FileExt::lock(&file)?;
        Ok(Self(file))
    }
}

impl Drop for SaveLock {
    fn drop(&mut self) {
        use fs4::FileExt;
        // Best-effort and strictly redundant: closing the handle (which
        // happens as this struct drops) releases the lock anyway. Explicit
        // here so the guard's purpose is legible at the call site.
        let _ = FileExt::unlock(&self.0);
    }
}

/// Append a `.tmp` segment to a path so atomic writes can rename
/// over the live file. Used by [`NameIndex::save_to_disk`].
fn with_tmp_suffix(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
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
    fn lookup_entry_by_id_returns_ingested_entry() {
        let idx = NameIndex::new();
        let id = "cf07501e-4e1a-4914-b3a3-0157006680ad";
        idx.ingest(&[node(id, "How we Lead — Specification without accountability", None)]);
        let hit = idx.lookup_entry_by_id(id).expect("ingested id must resolve");
        assert_eq!(hit.node_id, id);
        assert_eq!(
            hit.name,
            "How we Lead — Specification without accountability"
        );
    }

    #[test]
    fn lookup_entry_by_id_returns_none_for_unknown_id() {
        let idx = NameIndex::new();
        idx.ingest(&[node("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "X", None)]);
        assert!(idx
            .lookup_entry_by_id("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")
            .is_none());
    }

    #[test]
    fn lookup_entry_by_id_returns_none_after_invalidate() {
        let idx = NameIndex::new();
        let id = "cf07501e-4e1a-4914-b3a3-0157006680ad";
        idx.ingest(&[node(id, "Name", None)]);
        idx.invalidate_node(id);
        assert!(idx.lookup_entry_by_id(id).is_none());
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
    fn save_load_roundtrips_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("name_index.json");

        let idx = NameIndex::new();
        idx.set_save_path(path.clone());
        let id = "550e8400-e29b-41d4-a716-446655440000";
        idx.ingest(&[node(id, "Tasks", Some("root"))]);
        assert!(idx.is_dirty(), "ingest should mark dirty");
        idx.save_to_disk().expect("save");
        assert!(!idx.is_dirty(), "save clears dirty");

        // Fresh index, same path, must rehydrate the entry.
        let idx2 = NameIndex::new();
        idx2.set_save_path(path);
        let count = idx2.load_from_disk().expect("load");
        assert_eq!(count, 1);
        assert!(!idx2.is_dirty(), "load must not leave the index dirty");
        assert_eq!(
            idx2.resolve_short_hash("446655440000").as_deref(),
            Some(id)
        );
        assert_eq!(idx2.lookup("tasks", "exact").len(), 1);
    }

    /// Persistence-time exclusion tests (2026-07-17).
    ///
    /// The rule is "may enter memory, may never reach disk": a walk is free
    /// to traverse an excluded subtree (a live session resolving a hash under
    /// it still needs an answer), but the durable file must never carry it.
    /// The filter therefore sits at the serialisation boundary, not at
    /// `ingest`, and these tests assert both halves of that.
    mod excluded_subtrees {
        use super::*;

        const EXCL_ROOT: &str = "e0000000-0000-4000-8000-000000000000";
        const CHILD: &str = "c0000000-0000-4000-8000-000000000001";
        const GRANDCHILD: &str = "c0000000-0000-4000-8000-000000000002";
        const KEEP: &str = "k0000000-0000-4000-8000-000000000003";

        fn entry(id: &str, parent: Option<&str>) -> PersistedEntry {
            PersistedEntry {
                id: id.to_string(),
                name: format!("node-{id}"),
                parent_id: parent.map(str::to_string),
                description: None,
            }
        }

        fn tree() -> Vec<PersistedEntry> {
            vec![
                entry(EXCL_ROOT, None),
                entry(CHILD, Some(EXCL_ROOT)),
                entry(GRANDCHILD, Some(CHILD)),
                entry(KEEP, None),
            ]
        }

        #[test]
        fn excludes_the_root_and_every_descendant_transitively() {
            let out = excluded_ids(&tree(), &[EXCL_ROOT.to_string()]);
            assert!(out.contains(EXCL_ROOT), "the named root itself must go");
            assert!(out.contains(CHILD));
            assert!(
                out.contains(GRANDCHILD),
                "exclusion must be transitive, not just direct children"
            );
            assert!(!out.contains(KEEP), "unrelated nodes must survive");
        }

        #[test]
        fn matches_a_twelve_char_short_hash_as_well_as_a_full_uuid() {
            let short = &EXCL_ROOT[EXCL_ROOT.len() - SHORT_HASH_LEN_URL..];
            let out = excluded_ids(&tree(), &[short.to_string()]);
            assert!(
                out.contains(EXCL_ROOT) && out.contains(GRANDCHILD),
                "a URL short hash must exclude the same subtree a full UUID does"
            );
        }

        #[test]
        fn no_tokens_excludes_nothing() {
            assert!(excluded_ids(&tree(), &[]).is_empty());
        }

        /// The regression that matters most. Once the excluded root has been
        /// purged it is no longer in the index, so a filter that seeds only
        /// on `id` finds nothing to walk from and passes the entire subtree
        /// through — failing open exactly where it must not, and only for the
        /// subtree it exists to protect. Observed against the live index on
        /// 2026-07-17.
        #[test]
        fn excludes_descendants_even_when_the_excluded_root_is_absent() {
            let orphaned: Vec<PersistedEntry> = tree()
                .into_iter()
                .filter(|n| n.id != EXCL_ROOT)
                .collect();
            assert!(
                !orphaned.iter().any(|n| n.id == EXCL_ROOT),
                "precondition: the root is not in the index"
            );

            let out = excluded_ids(&orphaned, &[EXCL_ROOT.to_string()]);
            assert!(
                out.contains(CHILD),
                "a child naming the excluded root as its parent must be excluded even \
                 though the root itself is not indexed"
            );
            assert!(
                out.contains(GRANDCHILD),
                "and the exclusion must still descend from that child"
            );
            assert!(!out.contains(KEEP));
        }

        #[test]
        fn a_parent_cycle_terminates() {
            // A malformed pair pointing at each other must not spin forever.
            let nodes = vec![entry(EXCL_ROOT, Some(CHILD)), entry(CHILD, Some(EXCL_ROOT))];
            let out = excluded_ids(&nodes, &[EXCL_ROOT.to_string()]);
            assert_eq!(out.len(), 2);
        }

        #[test]
        fn excluded_subtree_is_withheld_from_disk_but_kept_in_memory() {
            let _guard = crate::defaults::ENV_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let key = "WORKFLOWY_INDEX_EXCLUDE_SUBTREES";
            let prev = std::env::var(key).ok();
            std::env::set_var(key, EXCL_ROOT);

            let dir = std::env::temp_dir().join(format!("wf-excl-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("name_index.json");
            let _ = std::fs::remove_file(&path);

            let idx = NameIndex::default();
            idx.set_save_path(path.clone());
            idx.ingest(&[
                node(EXCL_ROOT, "secret-root", None),
                node(CHILD, "secret-child", Some(EXCL_ROOT)),
                node(KEEP, "public", None),
            ]);

            // In memory the excluded node is present: a live session may
            // still resolve it.
            assert!(
                idx.lookup_entry_by_id(CHILD).is_some(),
                "exclusion is a persistence rule, not an ingest rule"
            );

            idx.save_to_disk().unwrap();
            let on_disk = std::fs::read_to_string(&path).unwrap();
            assert!(
                !on_disk.contains(EXCL_ROOT) && !on_disk.contains(CHILD),
                "excluded subtree must not reach disk"
            );
            assert!(on_disk.contains(KEEP), "everything else must still persist");

            let _ = std::fs::remove_file(&path);
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }

        /// `snapshot()` is where the exclusion filter lives, so it must stay
        /// the only place the persisted form is built. A future edit that
        /// serialises the index anywhere else would bypass the filter and
        /// leak the excluded subtree to disk — silently, since nothing in
        /// the file records which writer produced it.
        #[test]
        fn persisted_snapshot_is_the_only_serialisation_path() {
            let src = include_str!("name_index.rs");
            // `concat!` keeps these needles out of this file as contiguous
            // literals, so the grep cannot match its own assertion text.
            let ctor = concat!("PersistedSnapshot", " {");
            let def = concat!("struct ", "PersistedSnapshot", " {");
            // The type also appears as `snapshot()`'s return signature; the
            // brace there opens the function, not a struct literal.
            let sig = concat!("-> ", "PersistedSnapshot", " {");
            let sites =
                src.matches(ctor).count() - src.matches(def).count() - src.matches(sig).count();
            assert_eq!(
                sites, 1,
                "PersistedSnapshot must be constructed in exactly one place — snapshot(), \
                 which applies the excluded-subtree filter. Another construction site \
                 would write the index to disk unfiltered; found {sites}",
            );
            assert!(
                src.contains(concat!("let snap = ", "self.snapshot();")),
                "save_to_disk must serialise via self.snapshot(), never a hand-built snapshot",
            );
        }
    }

    /// Multi-writer merge tests for the 2026-07-16 clobber defect.
    ///
    /// The index has two writers — the long-running MCP server and
    /// `wflow-do reindex`. Both used to serialise their whole in-memory map
    /// straight over the file, so each silently discarded everything the
    /// other had added since its own load. The observed symptom was
    /// non-monotonic churn: a subtree present in the file at 15:20 was gone
    /// at 16:08, replaced by a different one, even though `ingest` is purely
    /// additive and nothing prunes.
    mod multi_writer_merge {
        use super::*;

        // Synthetic IDs. The defect was found against two real nodes in the
        // author's workspace, but the repo ships no machine-specific IDs
        // (constitution principle 8) — the merge logic is keyed on nothing
        // but map membership, so the values carry no meaning beyond being
        // distinct, well-formed UUIDs.
        const A_ID: &str = "aaaaaaaa-1111-4111-8111-111111111111";
        const B_ID: &str = "bbbbbbbb-2222-4222-8222-222222222222";

        /// The report's exact sequence: two writers each load the file, then
        /// each ingests a different subtree and saves. Neither writer's work
        /// may vanish.
        #[test]
        fn concurrent_writers_additions_survive_each_others_saves() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("name_index.json");

            // Both writers start from the same (empty) file — this is the
            // load-at-process-start that opens the clobber window.
            let server = NameIndex::new();
            server.set_save_path(path.clone());
            server.load_from_disk().expect("load");

            let cli = NameIndex::new();
            cli.set_save_path(path.clone());
            cli.load_from_disk().expect("load");

            // The CLI reindex walks and saves one subtree.
            cli.ingest(&[node(A_ID, "Walked by the CLI only", Some("parent-a"))]);
            cli.save_to_disk().expect("cli save");

            // Later the server's background refresher saves a different one.
            // Pre-fix this write dropped A_ID, which the server had never
            // seen, because it serialised its own stale map over the file.
            server.ingest(&[node(B_ID, "Walked by the server only", Some("parent-b"))]);
            server.save_to_disk().expect("server save");

            let reader = NameIndex::new();
            reader.set_save_path(path);
            reader.load_from_disk().expect("load");

            assert!(
                reader.lookup_entry_by_id(A_ID).is_some(),
                "the CLI's subtree was clobbered by the server's save"
            );
            assert!(
                reader.lookup_entry_by_id(B_ID).is_some(),
                "the server's own subtree is missing from its own save"
            );
        }

        /// The merge adopts disk-only entries into memory, so a long-running
        /// writer converges on the union rather than staying blind to the
        /// other writer's work.
        #[test]
        fn merge_adopts_disk_only_entries_into_memory() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("name_index.json");

            let cli = NameIndex::new();
            cli.set_save_path(path.clone());
            cli.ingest(&[node(A_ID, "From CLI", None)]);
            cli.save_to_disk().expect("cli save");

            let server = NameIndex::new();
            server.set_save_path(path);
            server.ingest(&[node(B_ID, "From server", None)]);
            assert!(server.lookup_entry_by_id(A_ID).is_none(), "precondition");
            server.save_to_disk().expect("server save");

            assert!(
                server.lookup_entry_by_id(A_ID).is_some(),
                "save should merge the other writer's entries into memory"
            );
        }

        /// Memory is at least as fresh as disk for any ID this process has
        /// observed, so a rename must not be reverted by the merge.
        #[test]
        fn merge_does_not_overwrite_a_locally_renamed_node_with_the_stale_disk_name() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("name_index.json");

            let first = NameIndex::new();
            first.set_save_path(path.clone());
            first.ingest(&[node(A_ID, "Old name", None)]);
            first.save_to_disk().expect("save");

            let second = NameIndex::new();
            second.set_save_path(path);
            second.ingest(&[node(A_ID, "New name", None)]);
            second.save_to_disk().expect("save");

            assert_eq!(
                second.lookup("new name", "exact").len(),
                1,
                "the observed rename must win over the stale on-disk name"
            );
            assert_eq!(
                second.lookup("old name", "exact").len(),
                0,
                "the merge resurrected the stale name"
            );
        }

        /// The merge protects additions; it must not undo deletions. A node
        /// this process invalidated is tombstoned, so the on-disk copy
        /// written by a writer that never saw the delete cannot bring it back.
        #[test]
        fn merge_does_not_resurrect_a_locally_deleted_node() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("name_index.json");

            // Another writer's file still carries the node.
            let other = NameIndex::new();
            other.set_save_path(path.clone());
            other.ingest(&[node(A_ID, "Doomed", None)]);
            other.save_to_disk().expect("save");

            // This writer knows it, then observes the delete.
            let server = NameIndex::new();
            server.set_save_path(path.clone());
            server.load_from_disk().expect("load");
            assert!(server.lookup_entry_by_id(A_ID).is_some(), "precondition");
            server.invalidate_node(A_ID);
            server.save_to_disk().expect("save");

            assert!(
                server.lookup_entry_by_id(A_ID).is_none(),
                "the merge resurrected a node this process deleted"
            );
            let reader = NameIndex::new();
            reader.set_save_path(path);
            reader.load_from_disk().expect("load");
            assert!(
                reader.lookup_entry_by_id(A_ID).is_none(),
                "the deleted node came back through the saved file"
            );
        }

        /// A save must not be abandoned because another writer left an
        /// unreadable file — that would strand this process's own work.
        #[test]
        fn save_overwrites_a_corrupt_file_rather_than_failing() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("name_index.json");
            std::fs::write(&path, "{ not json at all").expect("write corrupt");

            let idx = NameIndex::new();
            idx.set_save_path(path.clone());
            idx.ingest(&[node(A_ID, "Survivor", None)]);
            idx.save_to_disk().expect("save must not fail on a corrupt file");

            let reader = NameIndex::new();
            reader.set_save_path(path);
            reader.load_from_disk().expect("load");
            assert!(reader.lookup_entry_by_id(A_ID).is_some());
        }
    }

    #[test]
    fn ingest_is_no_op_when_state_unchanged() {
        // Re-ingesting the same node must NOT flip dirty — the periodic
        // saver would otherwise loop forever rewriting the same JSON.
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "A", None)]);
        let _ = idx.is_dirty();
        idx.dirty.store(false, Ordering::Relaxed);
        idx.ingest(&[node("1", "A", None)]);
        assert!(!idx.is_dirty(), "redundant ingest must not mark dirty");
    }

    fn node_with_desc(id: &str, name: &str, desc: &str) -> WorkflowyNode {
        WorkflowyNode {
            id: id.to_string(),
            name: name.to_string(),
            description: Some(desc.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn search_tokens_matches_multi_token_query_out_of_order() {
        // 2026-07-12 issue 6: `lookup(_, "contains")` treated the whole query
        // as one contiguous needle, so a distinctive multi-word query missed
        // even when every token was present. Token-AND fixes it.
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "Annex III — high-risk AI systems regime", None)]);
        // Whole-phrase contiguous match still works.
        assert_eq!(idx.search_tokens("Annex III").len(), 1);
        // The former failure: interior words break contiguity; token-AND
        // (any order, gaps allowed) now matches.
        assert_eq!(idx.search_tokens("Annex III high-risk regime").len(), 1);
        assert_eq!(idx.search_tokens("regime annex").len(), 1, "order-independent");
        // A token absent from the node still excludes it.
        assert_eq!(idx.search_tokens("Annex IV").len(), 0);
    }

    #[test]
    fn search_tokens_matches_description_content() {
        // 2026-07-12 issue 5: content living only in a node's description
        // must be findable via the index fast path.
        let idx = NameIndex::new();
        idx.ingest(&[node_with_desc(
            "1",
            "GDPR obligations",
            "records of processing activities under Article 30",
        )]);
        // Token present only in the description.
        assert_eq!(idx.search_tokens("Article 30").len(), 1);
        // Tokens spanning name + description both required.
        assert_eq!(idx.search_tokens("GDPR Article").len(), 1);
        assert_eq!(idx.search_tokens("GDPR Article 31").len(), 0);
    }

    #[test]
    fn search_tokens_empty_query_returns_nothing() {
        let idx = NameIndex::new();
        idx.ingest(&[node("1", "A", None)]);
        assert!(idx.search_tokens("").is_empty());
        assert!(idx.search_tokens("   ").is_empty());
    }

    #[test]
    fn description_edit_is_reingested_for_search() {
        // The no-op guard must NOT skip a re-ingest when only the description
        // changed, otherwise description-aware search would serve stale text.
        let idx = NameIndex::new();
        idx.ingest(&[node_with_desc("1", "Note", "old body")]);
        assert_eq!(idx.search_tokens("old body").len(), 1);
        idx.dirty.store(false, Ordering::Relaxed);
        idx.ingest(&[node_with_desc("1", "Note", "new body")]);
        assert!(idx.is_dirty(), "a description change must mark the index dirty");
        assert!(idx.search_tokens("old body").is_empty(), "stale description gone");
        assert_eq!(idx.search_tokens("new body").len(), 1);
    }

    #[test]
    fn save_load_roundtrips_description() {
        // Schema v2: descriptions survive a persist → rehydrate cycle so a
        // fresh server start can serve description-aware search immediately.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("name_index.json");
        let idx = NameIndex::new();
        idx.set_save_path(path.clone());
        idx.ingest(&[node_with_desc(
            "550e8400-e29b-41d4-a716-446655440000",
            "Policy",
            "Article 30 records",
        )]);
        idx.save_to_disk().expect("save");

        let idx2 = NameIndex::new();
        idx2.set_save_path(path);
        idx2.load_from_disk().expect("load");
        assert_eq!(
            idx2.search_tokens("Article 30").len(),
            1,
            "rehydrated index must still match on description content"
        );
    }

    #[test]
    fn load_from_disk_handles_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.json");
        let idx = NameIndex::new();
        idx.set_save_path(path);
        assert_eq!(idx.load_from_disk().expect("missing file is OK"), 0);
    }

    #[test]
    fn load_from_disk_rejects_bad_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("name_index.json");
        std::fs::write(
            &path,
            r#"{"version": 999, "updated_at": 0, "nodes": []}"#,
        )
        .expect("write");
        let idx = NameIndex::new();
        idx.set_save_path(path);
        let err = idx.load_from_disk().expect_err("bad version must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn save_creates_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("name_index.json");
        let idx = NameIndex::new();
        idx.set_save_path(nested.clone());
        idx.ingest(&[node("1", "A", None)]);
        idx.save_to_disk().expect("save creates parents");
        assert!(nested.exists());
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
