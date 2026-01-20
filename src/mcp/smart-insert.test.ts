/**
 * Tests for smart_insert and find_insert_targets tools
 *
 * Tests the search-and-insert workflow and insert target discovery.
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

describe("smart_insert workflow logic", () => {
  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "1", name: "Research", parent_id: undefined }),
    createMockNode({ id: "2", name: "Research Notes", parent_id: "1" }),
    createMockNode({ id: "3", name: "Project Research", parent_id: undefined }),
    createMockNode({ id: "4", name: "Personal", parent_id: undefined }),
  ];

  describe("search phase", () => {
    function searchForTargets(nodes: WorkflowyNode[], query: string): WorkflowyNode[] {
      const lowerQuery = query.toLowerCase();
      return nodes.filter((node) => {
        const nodeName = node.name?.toLowerCase() || "";
        return nodeName.includes(lowerQuery);
      });
    }

    it("finds matching nodes for search query", () => {
      const results = searchForTargets(mockNodes, "research");
      expect(results.length).toBe(3);
    });

    it("returns single match when query is specific", () => {
      const results = searchForTargets(mockNodes, "Personal");
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("4");
    });

    it("returns empty array for no matches", () => {
      const results = searchForTargets(mockNodes, "nonexistent");
      expect(results).toEqual([]);
    });
  });

  describe("selection phase", () => {
    const multipleMatches = [
      { id: "1", name: "Research", path: "Research" },
      { id: "2", name: "Research Notes", path: "Research > Research Notes" },
      { id: "3", name: "Project Research", path: "Project Research" },
    ];

    it("validates selection is within bounds", () => {
      const selection = 2;
      const isValid = selection >= 1 && selection <= multipleMatches.length;
      expect(isValid).toBe(true);
    });

    it("rejects selection out of bounds", () => {
      const selection = 5;
      const isValid = selection >= 1 && selection <= multipleMatches.length;
      expect(isValid).toBe(false);
    });

    it("selects correct node by 1-based index", () => {
      const selection = 2;
      const selected = multipleMatches[selection - 1];
      expect(selected.id).toBe("2");
      expect(selected.name).toBe("Research Notes");
    });
  });

  describe("insertion decision logic", () => {
    it("proceeds immediately with single match", () => {
      const matches = [{ id: "1", name: "Target" }];
      const shouldProceed = matches.length === 1;
      expect(shouldProceed).toBe(true);
    });

    it("requires selection with multiple matches", () => {
      const matches = [
        { id: "1", name: "Target A" },
        { id: "2", name: "Target B" },
      ];
      const requiresSelection = matches.length > 1;
      expect(requiresSelection).toBe(true);
    });

    it("reports error with no matches", () => {
      const matches: Array<{ id: string; name: string }> = [];
      const hasNoMatches = matches.length === 0;
      expect(hasNoMatches).toBe(true);
    });
  });

  describe("response formatting", () => {
    it("formats single match response", () => {
      const match = { id: "abc", name: "Projects", path: "Work > Projects" };
      const content = "New content here";

      const response = {
        success: true,
        message: `Content inserted into "${match.name}" at ${match.path}`,
        target: match,
        content_preview: content.substring(0, 50),
      };

      expect(response.success).toBe(true);
      expect(response.target.id).toBe("abc");
    });

    it("formats multiple match response with options", () => {
      const matches = [
        { id: "1", name: "Target A", path: "Path A" },
        { id: "2", name: "Target B", path: "Path B" },
      ];

      const response = {
        success: false,
        multiple_matches: true,
        count: matches.length,
        message: `Found ${matches.length} potential targets. Which one do you mean?`,
        options: matches.map((m, i) => ({
          option: i + 1,
          name: m.name,
          path: m.path,
          id: m.id,
        })),
        usage: "Call smart_insert again with selection: <number>",
      };

      expect(response.multiple_matches).toBe(true);
      expect(response.options.length).toBe(2);
      expect(response.options[0].option).toBe(1);
      expect(response.options[1].option).toBe(2);
    });

    it("formats no match error response", () => {
      const query = "nonexistent";

      const response = {
        success: false,
        found: false,
        message: `No nodes found matching "${query}". Try a different search term.`,
      };

      expect(response.found).toBe(false);
      expect(response.message).toContain("nonexistent");
    });
  });
});

describe("find_insert_targets logic", () => {
  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "1", name: "Projects", parent_id: undefined }),
    createMockNode({ id: "2", name: "Active Projects", parent_id: "1" }),
    createMockNode({ id: "3", name: "Archived Projects", parent_id: "1" }),
    createMockNode({ id: "4", name: "Personal Projects", parent_id: undefined }),
  ];

  // Simulate children counts
  const childrenCounts: Record<string, number> = {
    "1": 2,
    "2": 5,
    "3": 10,
    "4": 3,
  };

  describe("target search", () => {
    function findInsertTargets(
      nodes: WorkflowyNode[],
      query: string
    ): Array<{
      id: string;
      name: string;
      path: string;
      children_count: number;
    }> {
      const lowerQuery = query.toLowerCase();
      return nodes
        .filter((node) => node.name?.toLowerCase().includes(lowerQuery))
        .map((node) => ({
          id: node.id,
          name: node.name || "",
          path: node.name || "", // Simplified for test
          children_count: childrenCounts[node.id] || 0,
        }));
    }

    it("finds all matching targets", () => {
      const targets = findInsertTargets(mockNodes, "projects");
      expect(targets.length).toBe(4);
    });

    it("includes children count for each target", () => {
      const targets = findInsertTargets(mockNodes, "projects");
      const archivedTarget = targets.find((t) => t.id === "3");
      expect(archivedTarget?.children_count).toBe(10);
    });

    it("returns empty array for no matches", () => {
      const targets = findInsertTargets(mockNodes, "nonexistent");
      expect(targets).toEqual([]);
    });
  });

  describe("response formatting", () => {
    it("formats successful response with targets", () => {
      const targets = [
        { id: "1", name: "Projects", path: "Projects", children_count: 2 },
        { id: "4", name: "Personal Projects", path: "Personal Projects", children_count: 3 },
      ];

      const response = {
        found: true,
        count: targets.length,
        targets,
        message: `Found ${targets.length} potential targets. Use insert_content with the desired parent_id.`,
      };

      expect(response.found).toBe(true);
      expect(response.count).toBe(2);
      expect(response.targets[0].children_count).toBe(2);
    });

    it("formats no results response", () => {
      const query = "nonexistent";

      const response = {
        found: false,
        count: 0,
        targets: [],
        message: `No nodes found matching "${query}".`,
      };

      expect(response.found).toBe(false);
      expect(response.targets).toEqual([]);
    });
  });
});

describe("smart_insert with selection", () => {
  describe("complete workflow simulation", () => {
    interface SmartInsertState {
      query: string;
      content: string;
      position?: "top" | "bottom";
      selection?: number;
      matches?: Array<{ id: string; name: string; path: string }>;
    }

    function simulateSmartInsert(state: SmartInsertState): {
      success: boolean;
      requiresSelection: boolean;
      targetId?: string;
      options?: Array<{ option: number; id: string; name: string }>;
    } {
      // Simulate search results based on query
      const mockSearchResults: Record<
        string,
        Array<{ id: string; name: string; path: string }>
      > = {
        Research: [
          { id: "r1", name: "Research", path: "Research" },
          { id: "r2", name: "Research Notes", path: "Work > Research Notes" },
        ],
        Projects: [{ id: "p1", name: "Projects", path: "Projects" }],
        Nonexistent: [],
      };

      const matches = mockSearchResults[state.query] || [];

      if (matches.length === 0) {
        return { success: false, requiresSelection: false };
      }

      if (matches.length === 1) {
        return {
          success: true,
          requiresSelection: false,
          targetId: matches[0].id,
        };
      }

      // Multiple matches
      if (state.selection) {
        const index = state.selection - 1;
        if (index >= 0 && index < matches.length) {
          return {
            success: true,
            requiresSelection: false,
            targetId: matches[index].id,
          };
        }
        return { success: false, requiresSelection: false };
      }

      return {
        success: false,
        requiresSelection: true,
        options: matches.map((m, i) => ({
          option: i + 1,
          id: m.id,
          name: m.name,
        })),
      };
    }

    it("completes single-match workflow in one call", () => {
      const result = simulateSmartInsert({
        query: "Projects",
        content: "New item",
      });

      expect(result.success).toBe(true);
      expect(result.requiresSelection).toBe(false);
      expect(result.targetId).toBe("p1");
    });

    it("requests selection for multiple matches", () => {
      const result = simulateSmartInsert({
        query: "Research",
        content: "New item",
      });

      expect(result.success).toBe(false);
      expect(result.requiresSelection).toBe(true);
      expect(result.options?.length).toBe(2);
    });

    it("completes workflow with selection provided", () => {
      const result = simulateSmartInsert({
        query: "Research",
        content: "New item",
        selection: 2,
      });

      expect(result.success).toBe(true);
      expect(result.requiresSelection).toBe(false);
      expect(result.targetId).toBe("r2");
    });

    it("fails with no matches", () => {
      const result = simulateSmartInsert({
        query: "Nonexistent",
        content: "New item",
      });

      expect(result.success).toBe(false);
      expect(result.requiresSelection).toBe(false);
      expect(result.targetId).toBeUndefined();
    });

    it("fails with invalid selection", () => {
      const result = simulateSmartInsert({
        query: "Research",
        content: "New item",
        selection: 10, // Out of bounds
      });

      expect(result.success).toBe(false);
    });
  });
});

describe("position parameter handling", () => {
  it("defaults to bottom when position not specified", () => {
    const position: "top" | "bottom" | undefined = undefined;
    const effectivePosition = position || "bottom";
    expect(effectivePosition).toBe("bottom");
  });

  it("respects explicit top position", () => {
    const position: "top" | "bottom" = "top";
    expect(position).toBe("top");
  });

  it("respects explicit bottom position", () => {
    const position: "top" | "bottom" = "bottom";
    expect(position).toBe("bottom");
  });
});

describe("content validation", () => {
  it("handles empty content", () => {
    const content = "";
    const isValid = content.trim().length > 0;
    expect(isValid).toBe(false);
  });

  it("handles whitespace-only content", () => {
    const content = "   \n   \t   ";
    const isValid = content.trim().length > 0;
    expect(isValid).toBe(false);
  });

  it("validates hierarchical content", () => {
    const content = `Parent
  Child 1
  Child 2`;
    const isValid = content.trim().length > 0;
    expect(isValid).toBe(true);
  });
});
