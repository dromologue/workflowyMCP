//! Best-effort idempotency for `create_node`.
//!
//! Workflowy's `POST /nodes` assigns the node ID server-side and exposes
//! neither a client-supplied ID nor an `Idempotency-Key` header, so true
//! upstream-deduped idempotency is not available. This is the next best
//! thing: an in-memory map keyed by a caller-supplied `idempotency_key`,
//! living on the long-running MCP server process. When the same key is
//! presented twice, the second `create_node` returns the node from the
//! original call instead of writing a duplicate.
//!
//! **What this covers** (the common retry shapes):
//! - A retry after a recorded success — the first call landed and we stored
//!   `key → item_id`; the retry replays the stored result, no second write.
//! - A retry after a failure that happened *before* the upstream write
//!   (e.g. the in-window 429 fail-fast) — no entry exists, so re-issuing is
//!   correct and creates exactly one node.
//!
//! **What this does NOT cover** (documented honestly so callers don't rely
//! on a guarantee that isn't there):
//! - An *ambiguous timeout* AFTER the POST was sent but before we saw the
//!   response — we never recorded success, so a retry with the same key
//!   re-issues and double-writes. Only upstream idempotency could close
//!   this; the residual is why a single-node create lost to a timeout still
//!   needs a read-back before retry.
//! - Concurrent same-key calls racing through the check before either
//!   records — best-effort, both may write. Sequential retry (the dominant
//!   real case) is handled.
//!
//! Methods take `now_ms` explicitly so TTL behaviour is deterministic under
//! test, mirroring the `utils::aggregation` clock-injection pattern.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock milliseconds since the Unix epoch. Used by the server to drive
/// [`IdempotencyStore::check`] / [`IdempotencyStore::record`]; tests pass a
/// fixed value instead.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
struct Entry {
    item_id: String,
    name: String,
    parent_id: Option<String>,
    recorded_unix_ms: u64,
}

/// A hit returned from [`IdempotencyStore::check`] — the original call's
/// result plus how long ago it was recorded, so the caller-facing message
/// can say "created N seconds ago".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdempotencyHit {
    pub item_id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub age_ms: u64,
}

/// In-memory, TTL-bounded idempotency-key → create-result map. Server-only
/// state (a stateless CLI invocation has no prior call to dedupe against, so
/// the CLI deliberately does not expose the key — see CLAUDE.md).
pub struct IdempotencyStore {
    inner: RwLock<HashMap<String, Entry>>,
    ttl_ms: u64,
    max_entries: usize,
}

impl IdempotencyStore {
    pub fn new(ttl_ms: u64, max_entries: usize) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            ttl_ms,
            max_entries,
        }
    }

    /// Look up a key. Returns the original call's result if a non-expired
    /// entry exists. Prunes expired entries as a side effect so the map
    /// self-cleans on access without a background task.
    pub fn check(&self, key: &str, now_ms: u64) -> Option<IdempotencyHit> {
        let mut map = self.inner.write();
        self.prune_expired(&mut map, now_ms);
        map.get(key).map(|e| IdempotencyHit {
            item_id: e.item_id.clone(),
            name: e.name.clone(),
            parent_id: e.parent_id.clone(),
            age_ms: now_ms.saturating_sub(e.recorded_unix_ms),
        })
    }

    /// Record a successful create under `key`. Overwrites any existing entry
    /// (a fresh success for a reused key supersedes the old one). Enforces
    /// the entry cap by evicting the oldest entries after insertion.
    pub fn record(
        &self,
        key: String,
        item_id: String,
        name: String,
        parent_id: Option<String>,
        now_ms: u64,
    ) {
        let mut map = self.inner.write();
        map.insert(
            key,
            Entry {
                item_id,
                name,
                parent_id,
                recorded_unix_ms: now_ms,
            },
        );
        self.prune_expired(&mut map, now_ms);
        self.enforce_cap(&mut map);
    }

    fn prune_expired(&self, map: &mut HashMap<String, Entry>, now_ms: u64) {
        map.retain(|_, e| now_ms.saturating_sub(e.recorded_unix_ms) < self.ttl_ms);
    }

    /// Evict oldest-first until under the cap. Only runs when the map exceeds
    /// `max_entries` (rare — the TTL prune usually keeps it small).
    fn enforce_cap(&self, map: &mut HashMap<String, Entry>) {
        if map.len() <= self.max_entries {
            return;
        }
        let mut by_age: Vec<(String, u64)> = map
            .iter()
            .map(|(k, e)| (k.clone(), e.recorded_unix_ms))
            .collect();
        by_age.sort_by_key(|(_, ts)| *ts);
        let to_remove = map.len() - self.max_entries;
        for (k, _) in by_age.into_iter().take(to_remove) {
            map.remove(&k);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> IdempotencyStore {
        IdempotencyStore::new(1000, 3)
    }

    #[test]
    fn miss_then_record_then_hit() {
        let s = store();
        assert!(s.check("k1", 0).is_none(), "fresh key misses");
        s.record("k1".into(), "id-1".into(), "Node".into(), Some("p".into()), 100);
        let hit = s.check("k1", 150).expect("recorded key hits");
        assert_eq!(hit.item_id, "id-1");
        assert_eq!(hit.name, "Node");
        assert_eq!(hit.parent_id.as_deref(), Some("p"));
        assert_eq!(hit.age_ms, 50, "age is now - recorded");
    }

    #[test]
    fn entry_expires_after_ttl() {
        let s = store(); // ttl 1000 ms
        s.record("k1".into(), "id-1".into(), "Node".into(), None, 100);
        assert!(s.check("k1", 900).is_some(), "within TTL still hits");
        assert!(
            s.check("k1", 1200).is_none(),
            "past TTL (100 + 1000 = 1100) misses and is pruned"
        );
        assert_eq!(s.len(), 0, "expired entry pruned on access");
    }

    #[test]
    fn cap_evicts_oldest_first() {
        let s = store(); // max 3
        s.record("a".into(), "ia".into(), "A".into(), None, 10);
        s.record("b".into(), "ib".into(), "B".into(), None, 20);
        s.record("c".into(), "ic".into(), "C".into(), None, 30);
        s.record("d".into(), "id".into(), "D".into(), None, 40); // over cap → evict oldest ("a")
        assert_eq!(s.len(), 3);
        assert!(s.check("a", 40).is_none(), "oldest evicted");
        assert!(s.check("d", 40).is_some(), "newest retained");
    }

    #[test]
    fn record_overwrites_same_key() {
        let s = store();
        s.record("k".into(), "old".into(), "Old".into(), None, 10);
        s.record("k".into(), "new".into(), "New".into(), None, 20);
        let hit = s.check("k", 25).expect("hit");
        assert_eq!(hit.item_id, "new", "fresh success supersedes stale entry");
    }
}
