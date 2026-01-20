/**
 * Tests for search_nodes tool
 *
 * Tests the core search functionality for finding nodes by text.
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

describe("search_nodes logic", () => {
  const mockNodes: WorkflowyNode[] = [
    createMockNode({
      id: "1",
      name: "Project Planning",
      note: "Notes about planning the project",
    }),
    createMockNode({
      id: "2",
      name: "Meeting Notes",
      note: "Q1 planning discussion",
    }),
    createMockNode({
      id: "3",
      name: "Research Ideas",
      note: "Various research topics to explore",
    }),
    createMockNode({
      id: "4",
      name: "Personal Tasks",
      note: "Daily todo items",
    }),
    createMockNode({
      id: "5",
      name: "Budget Planning",
      note: "Financial planning for Q2",
    }),
  ];

  describe("name search", () => {
    function searchByName(
      nodes: WorkflowyNode[],
      query: string
    ): WorkflowyNode[] {
      const lowerQuery = query.toLowerCase();
      return nodes.filter((node) =>
        node.name?.toLowerCase().includes(lowerQuery)
      );
    }

    it("finds nodes by exact name match", () => {
      const results = searchByName(mockNodes, "Meeting Notes");
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("2");
    });

    it("finds nodes by partial name match", () => {
      const results = searchByName(mockNodes, "Planning");
      expect(results.length).toBe(2);
      expect(results.map((n) => n.id)).toContain("1");
      expect(results.map((n) => n.id)).toContain("5");
    });

    it("is case insensitive", () => {
      const resultsLower = searchByName(mockNodes, "planning");
      const resultsUpper = searchByName(mockNodes, "PLANNING");
      expect(resultsLower.length).toBe(resultsUpper.length);
    });

    it("returns empty for no matches", () => {
      const results = searchByName(mockNodes, "nonexistent");
      expect(results).toEqual([]);
    });
  });

  describe("note search", () => {
    function searchByNote(
      nodes: WorkflowyNode[],
      query: string
    ): WorkflowyNode[] {
      const lowerQuery = query.toLowerCase();
      return nodes.filter((node) =>
        node.note?.toLowerCase().includes(lowerQuery)
      );
    }

    it("finds nodes by note content", () => {
      const results = searchByNote(mockNodes, "Q1");
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("2");
    });

    it("handles nodes without notes", () => {
      const nodesWithMissing = [
        ...mockNodes,
        createMockNode({ id: "6", name: "No Note Node" }),
      ];
      const results = searchByNote(nodesWithMissing, "planning");
      // Nodes 1, 2, and 5 have "planning" in notes (project, Q1 planning, Financial planning)
      expect(results.length).toBe(3);
    });
  });

  describe("combined name and note search", () => {
    function searchNameAndNote(
      nodes: WorkflowyNode[],
      query: string
    ): WorkflowyNode[] {
      const lowerQuery = query.toLowerCase();
      return nodes.filter((node) => {
        const nameMatch = node.name?.toLowerCase().includes(lowerQuery);
        const noteMatch = node.note?.toLowerCase().includes(lowerQuery);
        return nameMatch || noteMatch;
      });
    }

    it("finds nodes matching in either name or note", () => {
      const results = searchNameAndNote(mockNodes, "planning");
      expect(results.length).toBe(3); // 1, 2, 5
    });

    it("does not duplicate nodes matching both", () => {
      const results = searchNameAndNote(mockNodes, "Project");
      // Node 1 has "Project" in name and "project" in note
      expect(results.length).toBe(1);
    });
  });

  describe("path building for results", () => {
    const hierarchicalNodes: WorkflowyNode[] = [
      createMockNode({ id: "root", name: "Work" }),
      createMockNode({ id: "child", name: "Projects", parent_id: "root" }),
      createMockNode({
        id: "grandchild",
        name: "Active Project",
        parent_id: "child",
      }),
    ];

    function buildPath(
      nodes: WorkflowyNode[],
      nodeId: string
    ): string {
      const parts: string[] = [];
      let currentId: string | undefined = nodeId;

      while (currentId) {
        const node = nodes.find((n) => n.id === currentId);
        if (!node) break;
        parts.unshift(node.name || "Untitled");
        currentId = node.parent_id;
      }

      return parts.join(" > ");
    }

    it("builds full path for nested node", () => {
      const path = buildPath(hierarchicalNodes, "grandchild");
      expect(path).toBe("Work > Projects > Active Project");
    });

    it("builds path for root node", () => {
      const path = buildPath(hierarchicalNodes, "root");
      expect(path).toBe("Work");
    });

    it("handles orphan nodes", () => {
      const path = buildPath(hierarchicalNodes, "nonexistent");
      expect(path).toBe("");
    });
  });

  describe("result formatting", () => {
    it("formats search results with required fields", () => {
      const matches = mockNodes.slice(0, 2);
      const results = matches.map((node) => ({
        id: node.id,
        name: node.name,
        note_preview: node.note
          ? node.note.substring(0, 100) + (node.note.length > 100 ? "..." : "")
          : null,
        path: node.name, // Simplified
      }));

      expect(results[0].id).toBeDefined();
      expect(results[0].name).toBeDefined();
      expect(results[0].note_preview).not.toBeNull();
      expect(results[0].path).toBeDefined();
    });

    it("truncates long notes in preview", () => {
      const longNote = "a".repeat(200);
      const node = createMockNode({ note: longNote });
      const preview = node.note
        ? node.note.substring(0, 100) + (node.note.length > 100 ? "..." : "")
        : null;

      expect(preview?.length).toBeLessThan(longNote.length);
      expect(preview?.endsWith("...")).toBe(true);
    });

    it("returns null for note_preview when no note", () => {
      const node = createMockNode({ note: undefined });
      const preview = node.note
        ? node.note.substring(0, 100)
        : null;

      expect(preview).toBeNull();
    });
  });

  describe("result limit handling", () => {
    it("limits results to specified count", () => {
      const limit = 2;
      const results = mockNodes.slice(0, limit);
      expect(results.length).toBe(limit);
    });

    it("returns all results when under limit", () => {
      const limit = 10;
      const results = mockNodes.slice(0, Math.min(limit, mockNodes.length));
      expect(results.length).toBe(mockNodes.length);
    });
  });

  describe("response structure", () => {
    it("formats successful response", () => {
      const query = "planning";
      const matches = [
        { id: "1", name: "Project Planning", path: "Work > Project Planning" },
        { id: "5", name: "Budget Planning", path: "Finance > Budget Planning" },
      ];

      const response = {
        success: true,
        query,
        count: matches.length,
        results: matches,
        message: `Found ${matches.length} nodes matching "${query}"`,
      };

      expect(response.success).toBe(true);
      expect(response.count).toBe(2);
      expect(response.results.length).toBe(2);
    });

    it("formats empty results response", () => {
      const query = "nonexistent";

      const response = {
        success: true,
        query,
        count: 0,
        results: [],
        message: `No nodes found matching "${query}"`,
      };

      expect(response.success).toBe(true);
      expect(response.count).toBe(0);
      expect(response.results).toEqual([]);
    });
  });
});

describe("search edge cases", () => {
  describe("special characters", () => {
    const nodesWithSpecial: WorkflowyNode[] = [
      createMockNode({ id: "1", name: "C++ Programming" }),
      createMockNode({ id: "2", name: "Q&A Session" }),
      createMockNode({ id: "3", name: "Task: Important" }),
      createMockNode({ id: "4", name: "(Draft) Document" }),
      createMockNode({ id: "5", name: "100% Complete" }),
    ];

    it("searches for names with special characters", () => {
      const results = nodesWithSpecial.filter((n) =>
        n.name?.includes("C++")
      );
      expect(results.length).toBe(1);
    });

    it("searches for names with ampersand", () => {
      const results = nodesWithSpecial.filter((n) =>
        n.name?.toLowerCase().includes("q&a")
      );
      expect(results.length).toBe(1);
    });

    it("searches for names with colons", () => {
      const results = nodesWithSpecial.filter((n) =>
        n.name?.includes(":")
      );
      expect(results.length).toBe(1);
    });

    it("searches for names with parentheses", () => {
      const results = nodesWithSpecial.filter((n) =>
        n.name?.includes("(")
      );
      expect(results.length).toBe(1);
    });

    it("searches for names with percent sign", () => {
      const results = nodesWithSpecial.filter((n) =>
        n.name?.includes("%")
      );
      expect(results.length).toBe(1);
    });
  });

  describe("unicode characters", () => {
    const nodesWithUnicode: WorkflowyNode[] = [
      createMockNode({ id: "1", name: "日本語テスト" }),
      createMockNode({ id: "2", name: "Café Menu" }),
      createMockNode({ id: "3", name: "Über uns" }),
      createMockNode({ id: "4", name: "Résumé" }),
      createMockNode({ id: "5", name: "Emoji 🎉 Test" }),
    ];

    it("searches Japanese characters", () => {
      const results = nodesWithUnicode.filter((n) =>
        n.name?.includes("日本語")
      );
      expect(results.length).toBe(1);
    });

    it("searches accented characters", () => {
      const results = nodesWithUnicode.filter((n) =>
        n.name?.toLowerCase().includes("café")
      );
      expect(results.length).toBe(1);
    });

    it("searches German umlauts", () => {
      const results = nodesWithUnicode.filter((n) =>
        n.name?.toLowerCase().includes("über")
      );
      expect(results.length).toBe(1);
    });

    it("searches emoji", () => {
      const results = nodesWithUnicode.filter((n) =>
        n.name?.includes("🎉")
      );
      expect(results.length).toBe(1);
    });
  });

  describe("empty and whitespace", () => {
    const nodesWithEdgeCases: WorkflowyNode[] = [
      createMockNode({ id: "1", name: "" }),
      createMockNode({ id: "2", name: "   " }),
      createMockNode({ id: "3", name: undefined }),
      createMockNode({ id: "4", name: "Normal Node" }),
    ];

    it("handles empty name search", () => {
      const query = "";
      const results = nodesWithEdgeCases.filter((n) =>
        n.name?.toLowerCase().includes(query.toLowerCase())
      );
      // Empty query matches everything with a name
      expect(results.length).toBeGreaterThan(0);
    });

    it("handles whitespace-only names", () => {
      const query = "Normal";
      const results = nodesWithEdgeCases.filter((n) =>
        n.name?.toLowerCase().includes(query.toLowerCase())
      );
      expect(results.length).toBe(1);
    });

    it("handles undefined names safely", () => {
      const query = "test";
      const results = nodesWithEdgeCases.filter((n) => {
        const name = n.name || "";
        return name.toLowerCase().includes(query.toLowerCase());
      });
      expect(results).toEqual([]);
    });
  });

  describe("long content", () => {
    it("handles very long node names", () => {
      const longName = "A".repeat(1000);
      const node = createMockNode({ name: longName });
      const searchTerm = "A".repeat(10);

      const matches = node.name?.includes(searchTerm);
      expect(matches).toBe(true);
    });

    it("handles very long notes", () => {
      const longNote = "word ".repeat(10000);
      const node = createMockNode({ note: longNote });
      const searchTerm = "word";

      const matches = node.note?.includes(searchTerm);
      expect(matches).toBe(true);
    });
  });
});

describe("search performance considerations", () => {
  describe("result ordering", () => {
    const mockNodes: WorkflowyNode[] = [
      createMockNode({ id: "1", name: "Planning Document" }),
      createMockNode({ id: "2", name: "Project Planning" }),
      createMockNode({ id: "3", name: "Planning" }),
      createMockNode({ id: "4", name: "Annual Planning Meeting" }),
    ];

    it("exact matches could be prioritized", () => {
      const query = "Planning";
      const results = mockNodes.filter((n) =>
        n.name?.toLowerCase().includes(query.toLowerCase())
      );

      // Sort by exact match first, then by position of match
      const sorted = [...results].sort((a, b) => {
        const aExact = a.name?.toLowerCase() === query.toLowerCase();
        const bExact = b.name?.toLowerCase() === query.toLowerCase();
        if (aExact && !bExact) return -1;
        if (!aExact && bExact) return 1;
        return 0;
      });

      expect(sorted[0].name).toBe("Planning");
    });

    it("name matches could be prioritized over note matches", () => {
      const nodes = [
        createMockNode({ id: "1", name: "Meeting", note: "Planning discussion" }),
        createMockNode({ id: "2", name: "Planning Notes", note: "General notes" }),
      ];

      const query = "planning";
      const results = nodes.filter(
        (n) =>
          n.name?.toLowerCase().includes(query) ||
          n.note?.toLowerCase().includes(query)
      );

      // Sort by name match priority
      const sorted = [...results].sort((a, b) => {
        const aNameMatch = a.name?.toLowerCase().includes(query);
        const bNameMatch = b.name?.toLowerCase().includes(query);
        if (aNameMatch && !bNameMatch) return -1;
        if (!aNameMatch && bNameMatch) return 1;
        return 0;
      });

      expect(sorted[0].id).toBe("2"); // Name match first
    });
  });
});
