import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  isCacheValid,
  getCachedNodesIfValid,
  getCachedNode,
  updateCache,
  invalidateCache,
  invalidateNode,
  invalidateSubtree,
  startBatch,
  endBatch,
  hasPendingInvalidations,
  getPendingInvalidationCount,
  getCacheAge,
  getCacheStats,
} from "./cache.js";
import type { WorkflowyNode } from "../types/index.js";

describe("cache", () => {
  beforeEach(() => {
    // Reset cache before each test
    invalidateCache();
  });

  describe("isCacheValid", () => {
    it("returns false when cache is empty", () => {
      expect(isCacheValid()).toBe(false);
    });

    it("returns true immediately after update", () => {
      updateCache([{ id: "1", name: "Test" }]);
      expect(isCacheValid()).toBe(true);
    });

    it("returns false after TTL expires", async () => {
      // Mock time to test TTL
      vi.useFakeTimers();

      updateCache([{ id: "1", name: "Test" }]);
      expect(isCacheValid()).toBe(true);

      // Fast forward past TTL (30 seconds)
      vi.advanceTimersByTime(31000);
      expect(isCacheValid()).toBe(false);

      vi.useRealTimers();
    });
  });

  describe("getCachedNodesIfValid", () => {
    it("returns null when cache is empty", () => {
      expect(getCachedNodesIfValid()).toBeNull();
    });

    it("returns nodes when cache is valid", () => {
      const nodes: WorkflowyNode[] = [
        { id: "1", name: "Node 1" },
        { id: "2", name: "Node 2" },
      ];
      updateCache(nodes);
      expect(getCachedNodesIfValid()).toEqual(nodes);
    });

    it("returns null after invalidation", () => {
      updateCache([{ id: "1", name: "Test" }]);
      invalidateCache();
      expect(getCachedNodesIfValid()).toBeNull();
    });
  });

  describe("updateCache", () => {
    it("stores nodes in cache", () => {
      const nodes: WorkflowyNode[] = [{ id: "1", name: "Test" }];
      updateCache(nodes);
      expect(getCachedNodesIfValid()).toEqual(nodes);
    });

    it("replaces existing cache", () => {
      updateCache([{ id: "1", name: "Old" }]);
      updateCache([{ id: "2", name: "New" }]);

      const cached = getCachedNodesIfValid();
      expect(cached).toHaveLength(1);
      expect(cached![0].name).toBe("New");
    });
  });

  describe("invalidateCache", () => {
    it("clears the cache", () => {
      updateCache([{ id: "1", name: "Test" }]);
      invalidateCache();
      expect(isCacheValid()).toBe(false);
      expect(getCachedNodesIfValid()).toBeNull();
    });

    it("is safe to call on empty cache", () => {
      expect(() => invalidateCache()).not.toThrow();
    });
  });

  describe("getCacheAge", () => {
    it("returns -1 when cache is empty", () => {
      expect(getCacheAge()).toBe(-1);
    });

    it("returns 0 immediately after update", () => {
      vi.useFakeTimers();
      updateCache([{ id: "1", name: "Test" }]);
      expect(getCacheAge()).toBe(0);
      vi.useRealTimers();
    });

    it("increases over time", () => {
      vi.useFakeTimers();
      updateCache([{ id: "1", name: "Test" }]);

      vi.advanceTimersByTime(5000);
      expect(getCacheAge()).toBe(5000);

      vi.advanceTimersByTime(5000);
      expect(getCacheAge()).toBe(10000);

      vi.useRealTimers();
    });
  });

  describe("getCachedNode", () => {
    it("returns null when cache is empty", () => {
      expect(getCachedNode("1")).toBeNull();
    });

    it("returns node by ID when cached", () => {
      const nodes: WorkflowyNode[] = [
        { id: "1", name: "Node 1" },
        { id: "2", name: "Node 2" },
      ];
      updateCache(nodes);

      const node = getCachedNode("2");
      expect(node).toEqual({ id: "2", name: "Node 2" });
    });

    it("returns null for non-existent ID", () => {
      updateCache([{ id: "1", name: "Test" }]);
      expect(getCachedNode("nonexistent")).toBeNull();
    });
  });

  describe("invalidateNode", () => {
    it("removes single node from index", () => {
      const nodes: WorkflowyNode[] = [
        { id: "1", name: "Node 1" },
        { id: "2", name: "Node 2" },
      ];
      updateCache(nodes);

      invalidateNode("1");

      expect(getCachedNode("1")).toBeNull();
      expect(getCachedNode("2")).not.toBeNull();
    });

    it("adds to pending invalidations", () => {
      updateCache([{ id: "1", name: "Test" }]);

      invalidateNode("1");

      expect(hasPendingInvalidations()).toBe(true);
      expect(getPendingInvalidationCount()).toBe(1);
    });

    it("is safe to call when cache is empty", () => {
      expect(() => invalidateNode("1")).not.toThrow();
    });
  });

  describe("invalidateSubtree", () => {
    it("invalidates node and all descendants", () => {
      const nodes: WorkflowyNode[] = [
        { id: "root", name: "Root" },
        { id: "child1", name: "Child 1", parent_id: "root" },
        { id: "child2", name: "Child 2", parent_id: "root" },
        { id: "grandchild", name: "Grandchild", parent_id: "child1" },
        { id: "other", name: "Other" },
      ];
      updateCache(nodes);

      invalidateSubtree("root");

      expect(getCachedNode("root")).toBeNull();
      expect(getCachedNode("child1")).toBeNull();
      expect(getCachedNode("child2")).toBeNull();
      expect(getCachedNode("grandchild")).toBeNull();
      expect(getCachedNode("other")).not.toBeNull();
    });

    it("only invalidates specified subtree", () => {
      const nodes: WorkflowyNode[] = [
        { id: "parent1", name: "Parent 1" },
        { id: "child1", name: "Child 1", parent_id: "parent1" },
        { id: "parent2", name: "Parent 2" },
        { id: "child2", name: "Child 2", parent_id: "parent2" },
      ];
      updateCache(nodes);

      invalidateSubtree("parent1");

      expect(getCachedNode("parent1")).toBeNull();
      expect(getCachedNode("child1")).toBeNull();
      expect(getCachedNode("parent2")).not.toBeNull();
      expect(getCachedNode("child2")).not.toBeNull();
    });

    it("falls back to full invalidation when cache empty", () => {
      expect(() => invalidateSubtree("1")).not.toThrow();
    });
  });

  describe("batch operations", () => {
    it("startBatch and endBatch clear pending invalidations", () => {
      updateCache([
        { id: "1", name: "Node 1" },
        { id: "2", name: "Node 2" },
      ]);

      startBatch();
      invalidateNode("1");
      expect(hasPendingInvalidations()).toBe(true);

      endBatch();
      expect(hasPendingInvalidations()).toBe(false);
    });

    it("endBatch triggers full invalidation when too many nodes invalid", () => {
      const nodes: WorkflowyNode[] = [];
      for (let i = 0; i < 10; i++) {
        nodes.push({ id: `${i}`, name: `Node ${i}` });
      }
      updateCache(nodes);

      startBatch();
      // Invalidate more than 20% (3+ out of 10)
      invalidateNode("0");
      invalidateNode("1");
      invalidateNode("2");
      invalidateNode("3");

      endBatch();

      // Full cache should be invalidated
      expect(isCacheValid()).toBe(false);
    });
  });

  describe("getCacheStats", () => {
    it("returns stats when cache is empty", () => {
      const stats = getCacheStats();

      expect(stats.valid).toBe(false);
      expect(stats.nodeCount).toBe(0);
      expect(stats.ageMs).toBe(-1);
      expect(stats.pendingInvalidations).toBe(0);
      expect(stats.fullInvalidationPending).toBe(false);
    });

    it("returns correct stats when cache populated", () => {
      vi.useFakeTimers();
      updateCache([
        { id: "1", name: "Node 1" },
        { id: "2", name: "Node 2" },
      ]);

      vi.advanceTimersByTime(1000);

      const stats = getCacheStats();
      expect(stats.valid).toBe(true);
      expect(stats.nodeCount).toBe(2);
      expect(stats.ageMs).toBe(1000);

      vi.useRealTimers();
    });

    it("tracks pending invalidations", () => {
      updateCache([{ id: "1", name: "Test" }]);
      invalidateNode("1");

      const stats = getCacheStats();
      expect(stats.pendingInvalidations).toBe(1);
    });
  });
});
