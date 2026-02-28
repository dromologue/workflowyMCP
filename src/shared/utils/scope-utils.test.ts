import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../types/index.js";
import {
  buildChildrenIndex,
  getSubtreeNodes,
  filterNodesByScope,
} from "./scope-utils.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("scope-utils", () => {
  const nodes: WorkflowyNode[] = [
    createMockNode({ id: "root1", name: "Root 1" }),
    createMockNode({ id: "root2", name: "Root 2" }),
    createMockNode({ id: "child1", name: "Child 1", parent_id: "root1" }),
    createMockNode({ id: "child2", name: "Child 2", parent_id: "root1" }),
    createMockNode({ id: "grandchild1", name: "Grandchild 1", parent_id: "child1" }),
    createMockNode({ id: "child3", name: "Child 3", parent_id: "root2" }),
  ];

  describe("buildChildrenIndex", () => {
    it("builds parent_id to children map", () => {
      const index = buildChildrenIndex(nodes);
      expect(index.get("root1")).toHaveLength(2);
      expect(index.get("child1")).toHaveLength(1);
      expect(index.get("root2")).toHaveLength(1);
    });

    it("puts root-level nodes under 'root' key", () => {
      const index = buildChildrenIndex(nodes);
      const rootChildren = index.get("root");
      expect(rootChildren).toHaveLength(2);
      expect(rootChildren!.map((n) => n.id)).toContain("root1");
      expect(rootChildren!.map((n) => n.id)).toContain("root2");
    });

    it("returns empty map for empty array", () => {
      const index = buildChildrenIndex([]);
      expect(index.size).toBe(0);
    });
  });

  describe("getSubtreeNodes", () => {
    it("returns root and all descendants", () => {
      const subtree = getSubtreeNodes("root1", nodes);
      expect(subtree).toHaveLength(4); // root1, child1, child2, grandchild1
      expect(subtree.map((n) => n.id)).toContain("root1");
      expect(subtree.map((n) => n.id)).toContain("grandchild1");
    });

    it("returns just the node if it has no children", () => {
      const subtree = getSubtreeNodes("grandchild1", nodes);
      expect(subtree).toHaveLength(1);
      expect(subtree[0].id).toBe("grandchild1");
    });

    it("returns empty array for non-existent node", () => {
      const subtree = getSubtreeNodes("nonexistent", nodes);
      expect(subtree).toHaveLength(0);
    });
  });

  describe("filterNodesByScope", () => {
    const sourceNode = nodes[0]; // root1

    it("returns empty for this_node scope", () => {
      const result = filterNodesByScope(sourceNode, nodes, "this_node");
      expect(result).toHaveLength(0);
    });

    it("returns all descendants for children scope", () => {
      const result = filterNodesByScope(sourceNode, nodes, "children");
      expect(result).toHaveLength(3); // child1, child2, grandchild1
      expect(result.map((n) => n.id)).not.toContain("root1");
    });

    it("returns siblings for siblings scope", () => {
      const result = filterNodesByScope(sourceNode, nodes, "siblings");
      expect(result).toHaveLength(1);
      expect(result[0].id).toBe("root2");
    });

    it("returns siblings of non-root node", () => {
      const child1 = nodes[2]; // child1, parent_id=root1
      const result = filterNodesByScope(child1, nodes, "siblings");
      expect(result).toHaveLength(1);
      expect(result[0].id).toBe("child2");
    });

    it("returns ancestors for ancestors scope", () => {
      const grandchild = nodes[4]; // grandchild1
      const result = filterNodesByScope(grandchild, nodes, "ancestors");
      expect(result).toHaveLength(2); // child1, root1
      expect(result[0].id).toBe("child1");
      expect(result[1].id).toBe("root1");
    });

    it("returns all other nodes for all scope", () => {
      const result = filterNodesByScope(sourceNode, nodes, "all");
      expect(result).toHaveLength(nodes.length - 1);
      expect(result.map((n) => n.id)).not.toContain("root1");
    });

    it("handles empty node array", () => {
      const result = filterNodesByScope(sourceNode, [], "children");
      expect(result).toHaveLength(0);
    });
  });
});
