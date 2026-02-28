import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";
import { getSubtreeNodes } from "../shared/utils/scope-utils.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("get_recent_changes logic", () => {
  const now = new Date(2026, 1, 28);
  const dayMs = 24 * 60 * 60 * 1000;

  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "root", name: "Root" }),
    createMockNode({ id: "recent1", name: "Modified yesterday", parent_id: "root", modifiedAt: now.getTime() - 1 * dayMs }),
    createMockNode({ id: "recent2", name: "Modified 3 days ago", parent_id: "root", modifiedAt: now.getTime() - 3 * dayMs }),
    createMockNode({ id: "old", name: "Modified 30 days ago", parent_id: "root", modifiedAt: now.getTime() - 30 * dayMs }),
    createMockNode({ id: "completed", name: "Completed recently", parent_id: "root", modifiedAt: now.getTime() - 2 * dayMs, completedAt: 1700000000 }),
    createMockNode({ id: "no-mod", name: "No modifiedAt", parent_id: "root" }),
    createMockNode({ id: "sub", name: "Sub node", parent_id: "recent1", modifiedAt: now.getTime() - 1 * dayMs }),
  ];

  function getRecentChanges(
    allNodes: WorkflowyNode[],
    options: { days?: number; root_id?: string; include_completed?: boolean; limit?: number } = {}
  ) {
    const { days = 7, root_id, include_completed = true, limit = 50 } = options;
    const cutoffMs = now.getTime() - days * dayMs;

    let candidates = allNodes;
    if (root_id) {
      candidates = getSubtreeNodes(root_id, allNodes);
    }

    let results = candidates.filter((n) => n.modifiedAt && n.modifiedAt > cutoffMs);

    if (!include_completed) {
      results = results.filter((n) => !n.completedAt);
    }

    results.sort((a, b) => (b.modifiedAt || 0) - (a.modifiedAt || 0));
    return results.slice(0, limit);
  }

  describe("time window filtering", () => {
    it("returns nodes modified within window", () => {
      const results = getRecentChanges(mockNodes, { days: 7 });
      expect(results.map((n) => n.id)).toContain("recent1");
      expect(results.map((n) => n.id)).toContain("recent2");
    });

    it("excludes nodes older than window", () => {
      const results = getRecentChanges(mockNodes, { days: 7 });
      expect(results.map((n) => n.id)).not.toContain("old");
    });

    it("excludes nodes without modifiedAt", () => {
      const results = getRecentChanges(mockNodes, { days: 7 });
      expect(results.map((n) => n.id)).not.toContain("no-mod");
    });

    it("narrows window correctly", () => {
      const results = getRecentChanges(mockNodes, { days: 2 });
      expect(results.map((n) => n.id)).toContain("recent1");
      expect(results.map((n) => n.id)).not.toContain("recent2");
    });
  });

  describe("completion filter", () => {
    it("includes completed nodes by default", () => {
      const results = getRecentChanges(mockNodes, { days: 7 });
      expect(results.map((n) => n.id)).toContain("completed");
    });

    it("excludes completed nodes when requested", () => {
      const results = getRecentChanges(mockNodes, { days: 7, include_completed: false });
      expect(results.map((n) => n.id)).not.toContain("completed");
    });
  });

  describe("sorting", () => {
    it("sorts by most recent first", () => {
      const results = getRecentChanges(mockNodes, { days: 7 });
      for (let i = 1; i < results.length; i++) {
        expect(results[i - 1].modifiedAt! >= results[i].modifiedAt!).toBe(true);
      }
    });
  });

  describe("scoping", () => {
    it("limits to subtree when root_id provided", () => {
      const results = getRecentChanges(mockNodes, { days: 7, root_id: "recent1" });
      expect(results.map((n) => n.id)).toContain("recent1");
      expect(results.map((n) => n.id)).toContain("sub");
      expect(results.map((n) => n.id)).not.toContain("recent2");
    });
  });

  describe("limit", () => {
    it("respects limit", () => {
      const results = getRecentChanges(mockNodes, { days: 7, limit: 2 });
      expect(results).toHaveLength(2);
    });
  });
});
