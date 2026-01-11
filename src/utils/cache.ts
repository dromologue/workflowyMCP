/**
 * Node caching with TTL for Workflowy data
 */

import type { WorkflowyNode } from "../types/index.js";
import { CACHE_TTL } from "../config/environment.js";

/** Cached nodes */
let cachedNodes: WorkflowyNode[] | null = null;

/** Timestamp of last cache update */
let cacheTimestamp: number = 0;

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
 * Update the cache with new nodes
 */
export function updateCache(nodes: WorkflowyNode[]): void {
  cachedNodes = nodes;
  cacheTimestamp = Date.now();
}

/**
 * Invalidate the cache (called after write operations)
 */
export function invalidateCache(): void {
  cachedNodes = null;
  cacheTimestamp = 0;
}

/**
 * Get cache age in milliseconds (for diagnostics)
 */
export function getCacheAge(): number {
  if (!cachedNodes) return -1;
  return Date.now() - cacheTimestamp;
}
