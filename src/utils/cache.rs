//! Node cache with TTL and efficient subtree invalidation.
//! Addresses: "Cache O(n²) subtree invalidation" → now O(n).

use crate::config::CACHE_TTL_SECS;
use crate::types::{ChildrenIndex, CacheEntry, WorkflowyNode};
use lazy_static::lazy_static;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{debug, info};

lazy_static! {
    /// Global cache instance shared across the server
    pub static ref CACHE: Arc<NodeCache> = Arc::new(NodeCache::new(10000));
}

/// A complete, display-ordered children listing for one parent, as returned
/// by the `/nodes?parent_id=` funnel. Cached whole because "the children of
/// X" is only answerable from a listing known to be complete — a per-node
/// cache can never prove completeness.
struct ListingEntry {
    children: Vec<WorkflowyNode>,
    timestamp: SystemTime,
}

/// High-performance node cache with parent→children mapping
pub struct NodeCache {
    /// HashMap cache of nodes by ID (simple, no LRU eviction - TTL-based only)
    cache: RwLock<HashMap<String, CacheEntry>>,
    /// Maps parent IDs to children IDs for O(n) subtree invalidation
    children_index: RwLock<ChildrenIndex>,
    /// Tracks if cache is in batch mode (suppresses invalidation broadcasts)
    batch_depth: RwLock<usize>,
    /// Complete children listings keyed by parent id (`""` = workspace root).
    listings: RwLock<HashMap<String, ListingEntry>>,
    /// child id → parent-listing key, so a write to a node can drop the one
    /// listing that displays it without knowing the parent. Entries may go
    /// stale when a listing is evicted; the failure direction is a spurious
    /// extra invalidation, never a stale serve.
    member_of: RwLock<HashMap<String, String>>,
}

impl NodeCache {
    pub fn new(_max_size: usize) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            children_index: RwLock::new(ChildrenIndex::new()),
            batch_depth: RwLock::new(0),
            listings: RwLock::new(HashMap::new()),
            member_of: RwLock::new(HashMap::new()),
        }
    }

    /// Insert or update a node in the cache
    pub fn insert(&self, node: WorkflowyNode) {
        let entry = CacheEntry {
            node: node.clone(),
            timestamp: SystemTime::now(),
        };

        // Update children index
        if let Some(parent_id) = &node.parent_id {
            let mut index = self.children_index.write();
            index
                .entry(parent_id.clone())
                .or_default()
                .push(node.id.clone());
        }

        self.cache.write().insert(node.id.clone(), entry);
    }

    /// Insert multiple nodes (batch operation)
    pub fn insert_batch(&self, nodes: Vec<WorkflowyNode>) {
        *self.batch_depth.write() += 1;

        for node in nodes {
            self.insert(node);
        }

        *self.batch_depth.write() -= 1;
    }

    /// Get a node by ID
    pub fn get(&self, id: &str) -> Option<WorkflowyNode> {
        let cache = self.cache.read();
        let entry = cache.get(id)?.clone();
        drop(cache); // Release lock before checking timestamp

        // Check if entry is expired
        if let Ok(elapsed) = entry.timestamp.elapsed() {
            if elapsed.as_secs() > CACHE_TTL_SECS {
                return None;
            }
        }

        Some(entry.node.clone())
    }

    /// Get all nodes from cache (for validation/debugging)
    pub fn get_all(&self) -> Vec<WorkflowyNode> {
        let cache = self.cache.read();
        cache
            .values()
            .filter_map(|entry| {
                if let Ok(elapsed) = entry.timestamp.elapsed() {
                    if elapsed.as_secs() <= CACHE_TTL_SECS {
                        return Some(entry.node.clone());
                    }
                }
                None
            })
            .collect()
    }

    /// Serve a complete children listing for `parent_key` (`""` = workspace
    /// root) if one was cached within the TTL. Returns the listing in the
    /// same display order it was inserted in.
    pub fn children_listing(&self, parent_key: &str) -> Option<Vec<WorkflowyNode>> {
        let listings = self.listings.read();
        let entry = listings.get(parent_key)?;
        if let Ok(elapsed) = entry.timestamp.elapsed() {
            if elapsed.as_secs() > CACHE_TTL_SECS {
                return None;
            }
        }
        Some(entry.children.clone())
    }

    /// Cache a complete, display-ordered children listing for `parent_key`.
    /// Bounded at [`crate::defaults::MAX_CACHED_LISTINGS`] entries: on
    /// overflow, expired entries are purged first; if still over, the map is
    /// cleared (a cold cache is always correct, an unbounded one is a leak
    /// on 250k-node walks).
    pub fn insert_children_listing(&self, parent_key: &str, children: &[WorkflowyNode]) {
        let mut listings = self.listings.write();
        if listings.len() >= crate::defaults::MAX_CACHED_LISTINGS {
            listings.retain(|_, e| {
                e.timestamp
                    .elapsed()
                    .map(|el| el.as_secs() <= CACHE_TTL_SECS)
                    .unwrap_or(false)
            });
            if listings.len() >= crate::defaults::MAX_CACHED_LISTINGS {
                listings.clear();
                self.member_of.write().clear();
            }
        }
        listings.insert(
            parent_key.to_string(),
            ListingEntry {
                children: children.to_vec(),
                timestamp: SystemTime::now(),
            },
        );
        let mut member = self.member_of.write();
        for child in children {
            member.insert(child.id.clone(), parent_key.to_string());
        }
    }

    /// Drop the cached children listing for `parent_key`, if any.
    pub fn invalidate_listing(&self, parent_key: &str) {
        self.listings.write().remove(parent_key);
    }

    /// Drop whichever cached listing displays `id` as a child. Used when a
    /// node is edited/completed/deleted/moved and the caller doesn't know
    /// its parent — the listing showing the old state must go.
    fn invalidate_listing_containing(&self, id: &str) {
        let parent_key = self.member_of.read().get(id).cloned();
        if let Some(key) = parent_key {
            self.listings.write().remove(&key);
        }
    }

    /// Invalidate a single node and all descendants - O(n) with parent map
    pub fn invalidate_subtree(&self, root_id: &str) {
        if *self.batch_depth.read() > 0 {
            return; // Suppress during batch operations
        }

        let mut to_invalidate = vec![root_id.to_string()];
        let index = self.children_index.read();

        while let Some(node_id) = to_invalidate.pop() {
            // Remove from cache
            self.cache.write().remove(&node_id);
            self.listings.write().remove(&node_id);

            // Find children and queue for invalidation (O(1) lookup)
            if let Some(children) = index.get(&node_id) {
                to_invalidate.extend(children.iter().cloned());
            }
        }
        self.invalidate_listing_containing(root_id);

        debug!(root_id = root_id, "Invalidated subtree");
    }

    /// Invalidate a single node. Also drops the node's own children
    /// listing (its children may have changed) and the listing that
    /// displays it (its name/completion may have changed there).
    pub fn invalidate_node(&self, id: &str) {
        self.cache.write().remove(id);
        self.listings.write().remove(id);
        self.invalidate_listing_containing(id);
    }

    /// Clear entire cache
    pub fn clear(&self) {
        self.cache.write().clear();
        self.children_index.write().clear();
        self.listings.write().clear();
        self.member_of.write().clear();
        info!("Cache cleared");
    }

    /// Update children index (call after rebuilding cache)
    pub fn rebuild_children_index(&self, nodes: &[WorkflowyNode]) {
        let mut index = self.children_index.write();
        index.clear();

        for node in nodes {
            if let Some(parent_id) = &node.parent_id {
                index
                    .entry(parent_id.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }

        debug!(
            node_count = nodes.len(),
            parent_count = index.len(),
            "Rebuilt children index"
        );
    }

    /// Get cache stats for monitoring
    pub fn stats(&self) -> CacheStats {
        let cache = self.cache.read();
        let index = self.children_index.read();

        CacheStats {
            node_count: cache.len(),
            parent_count: index.len(),
            listing_count: self.listings.read().len(),
        }
    }
}

#[derive(Debug)]
pub struct CacheStats {
    pub node_count: usize,
    pub parent_count: usize,
    pub listing_count: usize,
}

/// Get the global cache instance
pub fn get_cache() -> Arc<NodeCache> {
    CACHE.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_insert_and_get() {
        let cache = NodeCache::new(100);
        let node = WorkflowyNode {
            id: "node1".to_string(),
            name: "test".to_string(),
            ..Default::default()
        };

        cache.insert(node.clone());
        assert!(cache.get("node1").is_some());
    }

    #[test]
    fn children_listing_roundtrips_and_write_invalidation_drops_it() {
        let cache = NodeCache::new(100);
        let child = |id: &str| WorkflowyNode {
            id: id.to_string(),
            name: id.to_string(),
            parent_id: Some("p".to_string()),
            ..Default::default()
        };
        cache.insert_children_listing("p", &[child("a"), child("b")]);

        let served = cache.children_listing("p").expect("listing cached");
        assert_eq!(served.len(), 2);
        assert_eq!(served[0].id, "a", "display order preserved");

        // Editing a child (caller knows only the child id) drops the
        // listing that displays it.
        cache.invalidate_node("a");
        assert!(cache.children_listing("p").is_none(), "containing listing dropped");

        // Re-cache, then invalidate the parent itself (a create under p).
        cache.insert_children_listing("p", &[child("a")]);
        cache.invalidate_listing("p");
        assert!(cache.children_listing("p").is_none());

        // A node's own listing goes when the node is invalidated.
        cache.insert_children_listing("a", &[child("x")]);
        cache.invalidate_node("a");
        assert!(cache.children_listing("a").is_none());
    }

    #[test]
    fn test_subtree_invalidation_performance() {
        let cache = NodeCache::new(10000);

        // Create a tree: parent -> 100 children -> 100 grandchildren each = 10,100 nodes
        let parent = WorkflowyNode {
            id: "parent".to_string(),
            name: "parent".to_string(),
            ..Default::default()
        };
        cache.insert(parent);

        // Insert children
        for i in 0..100 {
            let child_id = format!("child_{}", i);
            let child = WorkflowyNode {
                id: child_id.clone(),
                name: child_id.clone(),
                parent_id: Some("parent".to_string()),
                ..Default::default()
            };
            cache.insert(child);

            // Insert grandchildren
            for j in 0..100 {
                let grandchild_id = format!("grandchild_{}_{}", i, j);
                let grandchild = WorkflowyNode {
                    id: grandchild_id.clone(),
                    name: grandchild_id.clone(),
                    parent_id: Some(child_id.clone()),
                    ..Default::default()
                };
                cache.insert(grandchild);
            }
        }

        // Now invalidate the parent - should be O(n), not O(n²)
        let start = std::time::Instant::now();
        cache.invalidate_subtree("parent");
        let elapsed = start.elapsed();

        // Should complete in < 100ms even with 10k nodes
        assert!(elapsed.as_millis() < 100, "Subtree invalidation took too long");
        assert!(cache.get("parent").is_none());
    }
}
