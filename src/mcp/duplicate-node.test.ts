import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";
import { getSubtreeNodes, buildChildrenIndex } from "../shared/utils/scope-utils.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("duplicate_node logic", () => {
  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "src", name: "Source Node", note: "Source note" }),
    createMockNode({ id: "child1", name: "Child 1", parent_id: "src" }),
    createMockNode({ id: "child2", name: "Child 2", parent_id: "src", note: "child note" }),
    createMockNode({ id: "grandchild", name: "Grandchild", parent_id: "child1" }),
    createMockNode({ id: "target", name: "Target Parent" }),
  ];

  describe("subtree traversal", () => {
    it("collects full subtree in parent-before-child order", () => {
      const subtree = getSubtreeNodes("src", mockNodes);
      const childrenIndex = buildChildrenIndex(subtree);

      const ordered: WorkflowyNode[] = [];
      const visit = (nodeId: string) => {
        const node = subtree.find((n) => n.id === nodeId);
        if (node) ordered.push(node);
        const children = childrenIndex.get(nodeId) || [];
        for (const child of children) visit(child.id);
      };
      visit("src");

      expect(ordered).toHaveLength(4);
      // Parent appears before children
      const srcIdx = ordered.findIndex((n) => n.id === "src");
      const child1Idx = ordered.findIndex((n) => n.id === "child1");
      const grandchildIdx = ordered.findIndex((n) => n.id === "grandchild");
      expect(srcIdx).toBeLessThan(child1Idx);
      expect(child1Idx).toBeLessThan(grandchildIdx);
    });

    it("handles leaf-only copy (include_children=false)", () => {
      const leafOnly = mockNodes.filter((n) => n.id === "src");
      expect(leafOnly).toHaveLength(1);
      expect(leafOnly[0].name).toBe("Source Node");
    });
  });

  describe("name prefix", () => {
    it("applies prefix only to root node", () => {
      const prefix = "Copy of ";
      const subtree = getSubtreeNodes("src", mockNodes);

      const modified = subtree.map((n) => ({
        ...n,
        name: n.id === "src" ? prefix + n.name : n.name,
      }));

      expect(modified.find((n) => n.id === "src")!.name).toBe("Copy of Source Node");
      expect(modified.find((n) => n.id === "child1")!.name).toBe("Child 1");
    });
  });

  describe("ID mapping", () => {
    it("maps old parent IDs to new IDs", () => {
      const subtree = getSubtreeNodes("src", mockNodes);
      const childrenIndex = buildChildrenIndex(subtree);

      // Simulate ID mapping
      const idMap = new Map<string, string>();
      idMap.set("src", "new-src");
      idMap.set("child1", "new-child1");

      const ordered: WorkflowyNode[] = [];
      const visit = (nodeId: string) => {
        const node = subtree.find((n) => n.id === nodeId);
        if (node) ordered.push(node);
        const children = childrenIndex.get(nodeId) || [];
        for (const child of children) visit(child.id);
      };
      visit("src");

      // For grandchild, parent should be new-child1
      const grandchild = ordered.find((n) => n.id === "grandchild")!;
      const newParent = idMap.get(grandchild.parent_id || "");
      expect(newParent).toBe("new-child1");
    });
  });
});
