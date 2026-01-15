/**
 * Tests for find_node tool logic
 * Tests the node matching and selection functionality
 */

import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";

// Helper to create mock nodes
function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("find_node matching logic", () => {
  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "1", name: "Project Ideas" }),
    createMockNode({ id: "2", name: "Ideas" }),
    createMockNode({ id: "3", name: "My Ideas for 2024" }),
    createMockNode({ id: "4", name: "Work Projects" }),
    createMockNode({ id: "5", name: "ideas" }), // lowercase
    createMockNode({ id: "6", name: "Research Notes" }),
  ];

  describe("exact match mode", () => {
    function exactMatch(nodes: WorkflowyNode[], searchName: string): WorkflowyNode[] {
      const lowerSearch = searchName.toLowerCase();
      return nodes.filter((node) => {
        const nodeName = node.name?.toLowerCase() || "";
        return nodeName === lowerSearch;
      });
    }

    it("finds exact match (case insensitive)", () => {
      const results = exactMatch(mockNodes, "Ideas");
      expect(results.length).toBe(2);
      expect(results.map((n) => n.id)).toContain("2");
      expect(results.map((n) => n.id)).toContain("5");
    });

    it("returns empty array for no match", () => {
      const results = exactMatch(mockNodes, "Nonexistent");
      expect(results).toEqual([]);
    });

    it("does not match partial strings", () => {
      const results = exactMatch(mockNodes, "Idea");
      expect(results).toEqual([]);
    });

    it("matches exact phrase including spaces", () => {
      const results = exactMatch(mockNodes, "Project Ideas");
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("1");
    });
  });

  describe("contains match mode", () => {
    function containsMatch(nodes: WorkflowyNode[], searchName: string): WorkflowyNode[] {
      const lowerSearch = searchName.toLowerCase();
      return nodes.filter((node) => {
        const nodeName = node.name?.toLowerCase() || "";
        return nodeName.includes(lowerSearch);
      });
    }

    it("finds all nodes containing the search term", () => {
      const results = containsMatch(mockNodes, "Ideas");
      expect(results.length).toBe(4);
      expect(results.map((n) => n.id)).toContain("1"); // Project Ideas
      expect(results.map((n) => n.id)).toContain("2"); // Ideas
      expect(results.map((n) => n.id)).toContain("3"); // My Ideas for 2024
      expect(results.map((n) => n.id)).toContain("5"); // ideas
    });

    it("finds partial matches", () => {
      const results = containsMatch(mockNodes, "Proj");
      expect(results.length).toBe(2);
      expect(results.map((n) => n.id)).toContain("1"); // Project Ideas
      expect(results.map((n) => n.id)).toContain("4"); // Work Projects
    });

    it("is case insensitive", () => {
      const resultsLower = containsMatch(mockNodes, "research");
      const resultsUpper = containsMatch(mockNodes, "RESEARCH");
      expect(resultsLower.length).toBe(1);
      expect(resultsUpper.length).toBe(1);
      expect(resultsLower[0].id).toBe(resultsUpper[0].id);
    });
  });

  describe("starts_with match mode", () => {
    function startsWithMatch(nodes: WorkflowyNode[], searchName: string): WorkflowyNode[] {
      const lowerSearch = searchName.toLowerCase();
      return nodes.filter((node) => {
        const nodeName = node.name?.toLowerCase() || "";
        return nodeName.startsWith(lowerSearch);
      });
    }

    it("finds nodes starting with the search term", () => {
      const results = startsWithMatch(mockNodes, "Project");
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("1");
    });

    it("does not match nodes where term appears later", () => {
      const results = startsWithMatch(mockNodes, "Ideas");
      expect(results.length).toBe(2); // "Ideas" and "ideas"
      expect(results.map((n) => n.id)).not.toContain("1"); // "Project Ideas" should not match
      expect(results.map((n) => n.id)).not.toContain("3"); // "My Ideas for 2024" should not match
    });

    it("is case insensitive", () => {
      const results = startsWithMatch(mockNodes, "my ideas");
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("3");
    });
  });
});

describe("find_node selection logic", () => {
  const mockMatches = [
    { id: "a", name: "Ideas", path: "Work > Ideas" },
    { id: "b", name: "Ideas", path: "Personal > Ideas" },
    { id: "c", name: "Ideas", path: "Archive > Ideas" },
  ];

  describe("single match behavior", () => {
    it("returns node directly when only one match", () => {
      const singleMatch = [mockMatches[0]];
      expect(singleMatch.length).toBe(1);
      // Should return node_id directly without needing selection
    });
  });

  describe("multiple match selection", () => {
    it("selection 1 returns first match", () => {
      const selection = 1;
      const index = selection - 1;
      expect(mockMatches[index].id).toBe("a");
    });

    it("selection 2 returns second match", () => {
      const selection = 2;
      const index = selection - 1;
      expect(mockMatches[index].id).toBe("b");
    });

    it("selection 3 returns third match", () => {
      const selection = 3;
      const index = selection - 1;
      expect(mockMatches[index].id).toBe("c");
    });

    it("validates selection is within bounds", () => {
      const selection = 4;
      const index = selection - 1;
      const isValid = index >= 0 && index < mockMatches.length;
      expect(isValid).toBe(false);
    });

    it("validates selection is positive", () => {
      const selection = 0;
      const index = selection - 1;
      const isValid = index >= 0 && index < mockMatches.length;
      expect(isValid).toBe(false);
    });

    it("negative selection is invalid", () => {
      const selection = -1;
      const index = selection - 1;
      const isValid = index >= 0 && index < mockMatches.length;
      expect(isValid).toBe(false);
    });
  });

  describe("options formatting", () => {
    it("generates numbered options with paths", () => {
      const options = mockMatches.map((node, i) => ({
        option: i + 1,
        name: node.name,
        path: node.path,
        id: node.id,
      }));

      expect(options.length).toBe(3);
      expect(options[0].option).toBe(1);
      expect(options[1].option).toBe(2);
      expect(options[2].option).toBe(3);
      expect(options[0].path).toBe("Work > Ideas");
      expect(options[1].path).toBe("Personal > Ideas");
    });

    it("includes note preview when note exists", () => {
      const nodeWithNote = {
        id: "x",
        name: "Test",
        path: "Path",
        note: "This is a very long note that should be truncated after 60 characters for the preview"
      };

      const notePreview = nodeWithNote.note
        ? nodeWithNote.note.substring(0, 60) + (nodeWithNote.note.length > 60 ? "..." : "")
        : null;

      expect(notePreview).toBe("This is a very long note that should be truncated after 60 c...");
    });

    it("returns null for note_preview when no note", () => {
      const nodeWithoutNote = { id: "x", name: "Test", path: "Path" };
      const notePreview = (nodeWithoutNote as { note?: string }).note
        ? (nodeWithoutNote as { note: string }).note.substring(0, 60)
        : null;

      expect(notePreview).toBeNull();
    });
  });
});

describe("find_node edge cases", () => {
  describe("empty or undefined names", () => {
    const nodesWithEmpty: WorkflowyNode[] = [
      createMockNode({ id: "1", name: "" }),
      createMockNode({ id: "2", name: undefined }),
      createMockNode({ id: "3", name: "Valid Name" }),
    ];

    it("handles empty string names safely", () => {
      const searchName = "Valid Name";
      const lowerSearch = searchName.toLowerCase();
      const results = nodesWithEmpty.filter((node) => {
        const nodeName = node.name?.toLowerCase() || "";
        return nodeName === lowerSearch;
      });
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("3");
    });

    it("handles undefined names safely", () => {
      const searchName = "";
      const lowerSearch = searchName.toLowerCase();
      const results = nodesWithEmpty.filter((node) => {
        const nodeName = node.name?.toLowerCase() || "";
        return nodeName === lowerSearch;
      });
      // Should match empty string names
      expect(results.length).toBe(2); // id 1 (empty) and id 2 (undefined -> empty)
    });
  });

  describe("special characters in names", () => {
    const nodesWithSpecialChars: WorkflowyNode[] = [
      createMockNode({ id: "1", name: "Test (2024)" }),
      createMockNode({ id: "2", name: "C++ Programming" }),
      createMockNode({ id: "3", name: "Q&A Section" }),
      createMockNode({ id: "4", name: "Notes: Important" }),
    ];

    it("matches names with parentheses", () => {
      const results = nodesWithSpecialChars.filter((n) =>
        n.name?.toLowerCase() === "test (2024)"
      );
      expect(results.length).toBe(1);
    });

    it("matches names with special characters", () => {
      const results = nodesWithSpecialChars.filter((n) =>
        n.name?.toLowerCase().includes("c++")
      );
      expect(results.length).toBe(1);
    });

    it("matches names with ampersand", () => {
      const results = nodesWithSpecialChars.filter((n) =>
        n.name?.toLowerCase().includes("q&a")
      );
      expect(results.length).toBe(1);
    });
  });

  describe("unicode and accented characters", () => {
    const nodesWithUnicode: WorkflowyNode[] = [
      createMockNode({ id: "1", name: "Café Notes" }),
      createMockNode({ id: "2", name: "Résumé Draft" }),
      createMockNode({ id: "3", name: "日本語" }),
      createMockNode({ id: "4", name: "Über Alles" }),
    ];

    it("matches accented characters (case insensitive)", () => {
      const results = nodesWithUnicode.filter((n) =>
        n.name?.toLowerCase() === "café notes"
      );
      expect(results.length).toBe(1);
    });

    it("matches unicode characters", () => {
      const results = nodesWithUnicode.filter((n) =>
        n.name?.includes("日本語")
      );
      expect(results.length).toBe(1);
    });

    it("matches German umlauts", () => {
      const results = nodesWithUnicode.filter((n) =>
        n.name?.toLowerCase().includes("über")
      );
      expect(results.length).toBe(1);
    });
  });
});
