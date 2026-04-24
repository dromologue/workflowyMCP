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

/// High-performance node cache with parent→children mapping
pub struct NodeCache {
    /// HashMap cache of nodes by ID (simple, no LRU eviction - TTL-based only)
    cache: RwLock<HashMap<String, CacheEntry>>,
    /// Maps parent IDs to children IDs for O(n) subtree invalidation
    children_index: RwLock<ChildrenIndex>,
    /// Tracks if cache is in batch mode (suppresses invalidation broadcasts)
    batch_depth: RwLock<usize>,
}

impl NodeCache {
    pub fn new(_max_size: usize) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            children_index: RwLock::new(ChildrenIndex::new()),
            batch_depth: RwLock::new(0),
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

            // Find children and queue for invalidation (O(1) lookup)
            if let Some(children) = index.get(&node_id) {
                to_invalidate.extend(children.iter().cloned());
            }
        }

        debug!(root_id = root_id, "Invalidated subtree");
    }

    /// Invalidate a single node
    pub fn invalidate_node(&self, id: &str) {
        self.cache.write().remove(id);
    }

    /// Clear entire cache
    pub fn clear(&self) {
        self.cache.write().clear();
        self.children_index.write().clear();
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
        }
    }
}

#[derive(Debug)]
pub struct CacheStats {
    pub node_count: usize,
    pub parent_count: usize,
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
