/**
 * Node caching with TTL for Workflowy data
 *
 * Supports:
 * - Full cache for all nodes
 * - Selective invalidation (single node, subtree, or full)
 * - Deferred invalidation for batch operations
 */

import type { WorkflowyNode } from "../types/index.js";
import { CACHE_TTL } from "../config/environment.js";

/** Cached nodes */
let cachedNodes: WorkflowyNode[] | null = null;

/** Index by node ID for fast lookups */
let nodeIndex: Map<string, WorkflowyNode> | null = null;

/** Timestamp of last cache update */
let cacheTimestamp: number = 0;

/** Set of node IDs pending invalidation (for batch operations) */
const pendingInvalidations: Set<string> = new Set();

/** Whether full invalidation is pending */
let fullInvalidationPending = false;

/**
 * Check if cache is valid (not expired)
 */
export function isCacheValid(): boolean {
  return cachedNodes !== null && Date.now() - cacheTimestamp < CACHE_TTL;
}

/**
 * Get cached nodes if valid, null otherwise
 */
export function getCachedNodesIfValid(): WorkflowyNode[] | null {
  if (isCacheValid()) {
    return cachedNodes;
  }
  return null;
}

/**
 * Get a single cached node by ID
 */
export function getCachedNode(nodeId: string): WorkflowyNode | null {
  if (!isCacheValid() || !nodeIndex) return null;
  return nodeIndex.get(nodeId) || null;
}

/**
 * Update the cache with new nodes
 */
export function updateCache(nodes: WorkflowyNode[]): void {
  cachedNodes = nodes;
  cacheTimestamp = Date.now();

  // Build index for fast lookups
  nodeIndex = new Map();
  for (const node of nodes) {
    nodeIndex.set(node.id, node);
  }

  // Clear any pending invalidations since we have fresh data
  pendingInvalidations.clear();
  fullInvalidationPending = false;
}

/**
 * Invalidate the entire cache (called after write operations)
 */
export function invalidateCache(): void {
  cachedNodes = null;
  nodeIndex = null;
  cacheTimestamp = 0;
  pendingInvalidations.clear();
  fullInvalidationPending = false;
}

/**
 * Mark a single node as invalid without full cache clear
 * Useful when you know only one node changed
 */
export function invalidateNode(nodeId: string): void {
  if (!nodeIndex) return;

  // Remove from index - will be refetched on next full cache refresh
  nodeIndex.delete(nodeId);
  pendingInvalidations.add(nodeId);
}

/**
 * Mark a subtree as invalid (node and all descendants)
 */
export function invalidateSubtree(nodeId: string): void {
  if (!cachedNodes || !nodeIndex) {
    invalidateCache();
    return;
  }

  const invalidNodeIds = new Set<string>();
  invalidNodeIds.add(nodeId);

  // Find all descendants by parent_id chain
  let foundMore = true;
  while (foundMore) {
    foundMore = false;
    for (const node of cachedNodes) {
      if (node.parent_id && invalidNodeIds.has(node.parent_id)) {
        if (!invalidNodeIds.has(node.id)) {
          invalidNodeIds.add(node.id);
          foundMore = true;
        }
      }
    }
  }

  // Remove all from index
  for (const id of invalidNodeIds) {
    nodeIndex.delete(id);
    pendingInvalidations.add(id);
  }
}

/**
 * Start a batch operation - defers invalidation until batch ends
 * Returns a batch ID to use with endBatch
 */
export function startBatch(): void {
  // Currently just a marker - actual batching happens via pendingInvalidations
}

/**
 * End a batch operation and apply all pending invalidations
 * If too many nodes are invalid, triggers full cache invalidation
 */
export function endBatch(): void {
  // If more than 20% of cache is invalid, just clear everything
  if (
    cachedNodes &&
    pendingInvalidations.size > cachedNodes.length * 0.2
  ) {
    invalidateCache();
    return;
  }

  if (fullInvalidationPending) {
    invalidateCache();
    return;
  }

  // Otherwise, pending invalidations are already applied to nodeIndex
  pendingInvalidations.clear();
}

/**
 * Mark full invalidation as pending (for use during batch operations)
 */
export function markFullInvalidationPending(): void {
  fullInvalidationPending = true;
}

/**
 * Check if there are pending invalidations
 */
export function hasPendingInvalidations(): boolean {
  return pendingInvalidations.size > 0 || fullInvalidationPending;
}

/**
 * Get count of pending invalidations
 */
export function getPendingInvalidationCount(): number {
  return pendingInvalidations.size;
}

/**
 * Get cache age in milliseconds (for diagnostics)
 */
export function getCacheAge(): number {
  if (!cachedNodes) return -1;
  return Date.now() - cacheTimestamp;
}

/**
 * Get cache statistics (for monitoring)
 */
export function getCacheStats(): {
  valid: boolean;
  nodeCount: number;
  ageMs: number;
  pendingInvalidations: number;
  fullInvalidationPending: boolean;
} {
  return {
    valid: isCacheValid(),
    nodeCount: cachedNodes?.length || 0,
    ageMs: getCacheAge(),
    pendingInvalidations: pendingInvalidations.size,
    fullInvalidationPending,
  };
}
