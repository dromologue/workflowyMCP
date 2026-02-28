import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";
import { extractWorkflowyLinks } from "../shared/utils/text-processing.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("find_backlinks logic", () => {
  const targetId = "target-abc";

  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: targetId, name: "Target Node" }),
    createMockNode({
      id: "linker1",
      name: `See [Target](https://workflowy.com/#/${targetId}) for details`,
    }),
    createMockNode({
      id: "linker2",
      name: "Another node",
      note: `Reference: https://workflowy.com/#/${targetId}`,
    }),
    createMockNode({
      id: "linker3",
      name: `Both [here](https://workflowy.com/#/${targetId})`,
      note: `And https://workflowy.com/#/${targetId}`,
    }),
    createMockNode({ id: "no-link", name: "No link here" }),
    createMockNode({
      id: "other-link",
      name: "[Other](https://workflowy.com/#/other-id)",
    }),
  ];

  function findBacklinks(nodeId: string, allNodes: WorkflowyNode[]) {
    const backlinks: Array<{ node: WorkflowyNode; link_in: "name" | "note" | "both" }> = [];

    for (const node of allNodes) {
      if (node.id === nodeId) continue;
      const nameLinks = extractWorkflowyLinks(node.name || "");
      const noteLinks = extractWorkflowyLinks(node.note || "");
      const inName = nameLinks.includes(nodeId);
      const inNote = noteLinks.includes(nodeId);

      if (inName || inNote) {
        const link_in = inName && inNote ? "both" : inName ? "name" : "note";
        backlinks.push({ node, link_in });
      }
    }

    return backlinks;
  }

  describe("link detection", () => {
    it("finds backlinks in node name", () => {
      const backlinks = findBacklinks(targetId, mockNodes);
      expect(backlinks.map((b) => b.node.id)).toContain("linker1");
    });

    it("finds backlinks in node note", () => {
      const backlinks = findBacklinks(targetId, mockNodes);
      expect(backlinks.map((b) => b.node.id)).toContain("linker2");
    });

    it("detects links in both name and note", () => {
      const backlinks = findBacklinks(targetId, mockNodes);
      const both = backlinks.find((b) => b.node.id === "linker3");
      expect(both).not.toBeUndefined();
      expect(both!.link_in).toBe("both");
    });

    it("correctly identifies link_in location", () => {
      const backlinks = findBacklinks(targetId, mockNodes);
      const nameOnly = backlinks.find((b) => b.node.id === "linker1");
      expect(nameOnly!.link_in).toBe("name");
      const noteOnly = backlinks.find((b) => b.node.id === "linker2");
      expect(noteOnly!.link_in).toBe("note");
    });
  });

  describe("exclusions", () => {
    it("excludes the target node itself", () => {
      const backlinks = findBacklinks(targetId, mockNodes);
      expect(backlinks.map((b) => b.node.id)).not.toContain(targetId);
    });

    it("excludes nodes with no links", () => {
      const backlinks = findBacklinks(targetId, mockNodes);
      expect(backlinks.map((b) => b.node.id)).not.toContain("no-link");
    });

    it("excludes nodes linking to different targets", () => {
      const backlinks = findBacklinks(targetId, mockNodes);
      expect(backlinks.map((b) => b.node.id)).not.toContain("other-link");
    });
  });

  describe("count", () => {
    it("finds all backlinks", () => {
      const backlinks = findBacklinks(targetId, mockNodes);
      expect(backlinks).toHaveLength(3);
    });

    it("returns empty for node with no backlinks", () => {
      const backlinks = findBacklinks("no-link", mockNodes);
      expect(backlinks).toHaveLength(0);
    });
  });
});
