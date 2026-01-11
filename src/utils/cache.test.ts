import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  isCacheValid,
  getCachedNodesIfValid,
  updateCache,
  invalidateCache,
  getCacheAge,
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
});
