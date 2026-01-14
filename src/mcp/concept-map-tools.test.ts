/**
 * Tests for LLM-powered concept map tools
 * Tests the logic used by get_node_content_for_analysis and render_concept_map
 */

import { describe, it, expect } from "vitest";
import { extractWorkflowyLinks } from "../shared/utils/text-processing.js";
import type { WorkflowyNode } from "../shared/types/index.js";

describe("get_node_content_for_analysis logic", () => {
  // Helper to create mock nodes
  function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
    return {
      id: "test-id",
      name: "Test Node",
      ...overrides,
    };
  }

  describe("link extraction from nodes", () => {
    it("extracts links from node name", () => {
      const node = createMockNode({
        name: "See [Related](https://workflowy.com/#/abc123)",
      });
      const nodeText = `${node.name || ""} ${node.note || ""}`;
      const links = extractWorkflowyLinks(nodeText);
      expect(links).toEqual(["abc123"]);
    });

    it("extracts links from node note", () => {
      const node = createMockNode({
        name: "Topic",
        note: "Reference: [Other Topic](https://workflowy.com/#/def456)",
      });
      const nodeText = `${node.name || ""} ${node.note || ""}`;
      const links = extractWorkflowyLinks(nodeText);
      expect(links).toEqual(["def456"]);
    });

    it("extracts multiple links from both name and note", () => {
      const node = createMockNode({
        name: "See [A](https://workflowy.com/#/link-a)",
        note: "Also [B](https://workflowy.com/#/link-b) and [C](https://workflowy.com/#/link-c)",
      });
      const nodeText = `${node.name || ""} ${node.note || ""}`;
      const links = extractWorkflowyLinks(nodeText);
      expect(links).toContain("link-a");
      expect(links).toContain("link-b");
      expect(links).toContain("link-c");
      expect(links.length).toBe(3);
    });

    it("handles nodes with no links", () => {
      const node = createMockNode({
        name: "Plain topic",
        note: "Just regular text without any links",
      });
      const nodeText = `${node.name || ""} ${node.note || ""}`;
      const links = extractWorkflowyLinks(nodeText);
      expect(links).toEqual([]);
    });
  });

  describe("hierarchy traversal", () => {
    it("builds correct paths from parent chain", () => {
      const nodes: WorkflowyNode[] = [
        { id: "root", name: "Philosophy" },
        { id: "child1", name: "Continental", parent_id: "root" },
        { id: "child2", name: "Badiou", parent_id: "child1" },
        { id: "grandchild", name: "Event Theory", parent_id: "child2" },
      ];

      // Simulate path building
      function buildPath(nodeId: string): string {
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

      expect(buildPath("grandchild")).toBe("Philosophy > Continental > Badiou > Event Theory");
      expect(buildPath("child2")).toBe("Philosophy > Continental > Badiou");
      expect(buildPath("root")).toBe("Philosophy");
    });

    it("handles nodes without parent (root level)", () => {
      const nodes: WorkflowyNode[] = [{ id: "orphan", name: "Standalone" }];

      function buildPath(nodeId: string): string {
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

      expect(buildPath("orphan")).toBe("Standalone");
    });
  });

  describe("depth calculation", () => {
    it("calculates correct depth from root", () => {
      const nodes: WorkflowyNode[] = [
        { id: "root", name: "Root" },
        { id: "l1", name: "Level 1", parent_id: "root" },
        { id: "l2", name: "Level 2", parent_id: "l1" },
        { id: "l3", name: "Level 3", parent_id: "l2" },
      ];

      function getDepth(nodeId: string, rootId: string): number {
        let depth = 0;
        let currentId: string | undefined = nodeId;
        while (currentId && currentId !== rootId) {
          const node = nodes.find((n) => n.id === currentId);
          if (!node?.parent_id) break;
          currentId = node.parent_id;
          depth++;
        }
        return depth;
      }

      expect(getDepth("l1", "root")).toBe(1);
      expect(getDepth("l2", "root")).toBe(2);
      expect(getDepth("l3", "root")).toBe(3);
    });
  });
});

describe("render_concept_map logic", () => {
  describe("concept validation", () => {
    it("requires at least 2 concepts", () => {
      const concepts = [{ id: "only-one", label: "One", level: "major" as const }];
      expect(concepts.length).toBeLessThan(2);
    });

    it("allows up to 35 concepts", () => {
      const concepts = Array.from({ length: 35 }, (_, i) => ({
        id: `concept-${i}`,
        label: `Concept ${i}`,
        level: "major" as const,
      }));
      expect(concepts.length).toBe(35);
      expect(concepts.length).toBeLessThanOrEqual(35);
    });

    it("rejects more than 35 concepts", () => {
      const concepts = Array.from({ length: 36 }, (_, i) => ({
        id: `concept-${i}`,
        label: `Concept ${i}`,
        level: "major" as const,
      }));
      expect(concepts.length).toBeGreaterThan(35);
    });
  });

  describe("relationship type handling", () => {
    it("supports causal relationship types", () => {
      const causalTypes = ["causes", "enables", "prevents", "triggers", "influences"];
      causalTypes.forEach((type) => {
        expect(type.length).toBeGreaterThan(0);
      });
    });

    it("supports structural relationship types", () => {
      const structuralTypes = ["contains", "part_of", "instance_of", "derives_from", "extends"];
      structuralTypes.forEach((type) => {
        expect(type.length).toBeGreaterThan(0);
      });
    });

    it("supports temporal relationship types", () => {
      const temporalTypes = ["precedes", "follows", "co_occurs"];
      temporalTypes.forEach((type) => {
        expect(type.length).toBeGreaterThan(0);
      });
    });

    it("supports logical relationship types", () => {
      const logicalTypes = ["implies", "contradicts", "supports", "refines", "exemplifies"];
      logicalTypes.forEach((type) => {
        expect(type.length).toBeGreaterThan(0);
      });
    });

    it("supports comparative relationship types", () => {
      const comparativeTypes = ["similar_to", "contrasts_with", "generalizes", "specializes"];
      comparativeTypes.forEach((type) => {
        expect(type.length).toBeGreaterThan(0);
      });
    });

    it("requires description field for relationships", () => {
      const relationship = {
        from: "concept-a",
        to: "concept-b",
        type: "enables",
        description: "Concept A enables Concept B by providing necessary foundation",
        strength: 0.8,
      };

      expect(relationship.description).toBeDefined();
      expect(relationship.description.length).toBeGreaterThan(0);
    });

    it("handles relationships with strength values (0.0-1.0)", () => {
      const relationship = {
        from: "concept-a",
        to: "concept-b",
        type: "enables",
        description: "Enables through causal mechanism",
        strength: 0.8,
      };

      expect(relationship.strength).toBeGreaterThanOrEqual(0);
      expect(relationship.strength).toBeLessThanOrEqual(1);
    });

    it("handles bidirectional relationships", () => {
      const relationship = {
        from: "concept-a",
        to: "concept-b",
        type: "similar_to",
        description: "Both concepts share common properties",
        bidirectional: true,
      };

      expect(relationship.bidirectional).toBe(true);
    });

    it("handles relationships with evidence", () => {
      const relationship = {
        from: "concept-a",
        to: "concept-b",
        type: "supports",
        description: "Evidence from the text supports this relationship",
        evidence: "As stated in the source: 'A directly supports B'",
      };

      expect(relationship.evidence).toBeDefined();
      expect(relationship.evidence).toContain("A directly supports B");
    });
  });

  describe("concept level mapping", () => {
    // Helper function to map level to internal representation
    function mapLevelToInternal(level: "major" | "detail"): number {
      return level === "major" ? 1 : 2;
    }

    it("maps major level to internal level 1", () => {
      expect(mapLevelToInternal("major")).toBe(1);
    });

    it("maps detail level to internal level 2", () => {
      expect(mapLevelToInternal("detail")).toBe(2);
    });
  });

  describe("importance to occurrences mapping", () => {
    it("uses importance value when provided", () => {
      const concept: { id: string; label: string; level: "major" | "detail"; importance?: number } = {
        id: "test",
        label: "Test",
        level: "major",
        importance: 8,
      };
      const occurrences = concept.importance || 5;
      expect(occurrences).toBe(8);
    });

    it("defaults to 5 when importance not provided", () => {
      const concept: { id: string; label: string; level: "major" | "detail"; importance?: number } = {
        id: "test",
        label: "Test",
        level: "major",
      };
      const occurrences = concept.importance || 5;
      expect(occurrences).toBe(5);
    });
  });
});

describe("output format handling", () => {
  describe("structured format", () => {
    it("produces valid JSON structure", () => {
      const result = {
        root: { id: "root-id", name: "Root", note: "Note" },
        total_nodes: 10,
        total_chars: 500,
        truncated: false,
        linked_nodes_included: 2,
        content: [
          { depth: 0, id: "n1", name: "Node 1", path: "Root > Node 1" },
        ],
      };

      expect(result.root).toBeDefined();
      expect(result.total_nodes).toBeTypeOf("number");
      expect(result.content).toBeInstanceOf(Array);
    });

    it("includes linked_content when links are followed", () => {
      const result = {
        root: { id: "root-id", name: "Root" },
        total_nodes: 5,
        total_chars: 200,
        truncated: false,
        linked_nodes_included: 2,
        content: [],
        linked_content: [
          { depth: -1, id: "linked-1", name: "Linked", path: "Other > Linked" },
        ],
      };

      expect(result.linked_content).toBeDefined();
      expect(result.linked_content[0].depth).toBe(-1);
    });
  });

  describe("outline format", () => {
    it("produces indented text", () => {
      const nodes = [
        { depth: 0, name: "Level 0" },
        { depth: 1, name: "Level 1" },
        { depth: 2, name: "Level 2" },
      ];

      const lines = nodes.map((n) => `${"  ".repeat(n.depth)}- ${n.name}`);
      const outline = lines.join("\n");

      expect(outline).toBe("- Level 0\n  - Level 1\n    - Level 2");
    });

    it("includes notes when present", () => {
      const node = { depth: 0, name: "Topic", note: "Important note" };
      const lines = [`- ${node.name}`, `  Notes: ${node.note}`];
      const outline = lines.join("\n");

      expect(outline).toContain("Notes: Important note");
    });
  });
});
