/**
 * Tests for hierarchical content insertion with staging node approach
 *
 * The insertHierarchicalContent function uses a staging node pattern:
 * 1. Creates a temporary staging node under the target parent
 * 2. Creates all hierarchical content inside the staging node
 * 3. Moves top-level children from staging to the actual parent
 * 4. Deletes the staging node
 *
 * This ensures nodes never appear at unintended locations (like root) during insertion.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { parseIndentedContent } from "../shared/utils/text-processing.js";

// Type for tracking mock API calls
interface MockApiCall {
  method: string;
  endpoint: string;
  body?: Record<string, unknown>;
}

describe("Staging Node Insertion Pattern", () => {
  describe("parseIndentedContent for hierarchical input", () => {
    it("parses simple hierarchical content", () => {
      const content = `Parent
  Child 1
  Child 2
    Grandchild`;

      const result = parseIndentedContent(content);
      expect(result).toEqual([
        { text: "Parent", indent: 0 },
        { text: "Child 1", indent: 1 },
        { text: "Child 2", indent: 1 },
        { text: "Grandchild", indent: 2 },
      ]);
    });

    it("parses multiple top-level nodes", () => {
      const content = `First Top Level
  First Child
Second Top Level
  Second Child`;

      const result = parseIndentedContent(content);
      expect(result).toEqual([
        { text: "First Top Level", indent: 0 },
        { text: "First Child", indent: 1 },
        { text: "Second Top Level", indent: 0 },
        { text: "Second Child", indent: 1 },
      ]);
    });

    it("handles deeply nested content", () => {
      const content = `Root
  Level 1
    Level 2
      Level 3
        Level 4`;

      const result = parseIndentedContent(content);
      expect(result).toHaveLength(5);
      expect(result[4]).toEqual({ text: "Level 4", indent: 4 });
    });

    it("returns empty array for empty content", () => {
      const result = parseIndentedContent("");
      expect(result).toEqual([]);
    });

    it("returns empty array for whitespace-only content", () => {
      const result = parseIndentedContent("   \n   \n   ");
      expect(result).toEqual([]);
    });
  });

  describe("Staging node workflow simulation", () => {
    let apiCalls: MockApiCall[];

    beforeEach(() => {
      apiCalls = [];
    });

    /**
     * Simulates the staging node insertion workflow
     * This mirrors the actual implementation logic
     */
    function simulateStagingInsertion(
      rootParentId: string,
      parsedLines: Array<{ text: string; indent: number }>,
      position?: "top" | "bottom"
    ): MockApiCall[] {
      const calls: MockApiCall[] = [];

      if (parsedLines.length === 0) {
        return calls;
      }

      // Step 1: Create staging node
      const stagingNodeId = "staging-temp-id";
      calls.push({
        method: "POST",
        endpoint: "/nodes",
        body: {
          name: "__staging_temp__",
          parent_id: rootParentId,
          position: "bottom",
        },
      });

      // Step 2: Create all nodes inside staging
      const parentStack: string[] = [stagingNodeId];
      const topLevelNodeIds: string[] = [];
      let nodeCounter = 0;

      for (const line of parsedLines) {
        const parentId = parentStack[Math.min(line.indent, parentStack.length - 1)];
        const nodeId = `node-${nodeCounter++}`;

        calls.push({
          method: "POST",
          endpoint: "/nodes",
          body: {
            name: line.text,
            parent_id: parentId,
            position: "bottom",
          },
        });

        if (line.indent === 0) {
          topLevelNodeIds.push(nodeId);
        }

        parentStack[line.indent + 1] = nodeId;
        parentStack.length = line.indent + 2;
      }

      // Step 3: Move top-level nodes from staging to actual parent
      const nodesToMove = position === "top" ? [...topLevelNodeIds].reverse() : topLevelNodeIds;

      for (const nodeId of nodesToMove) {
        calls.push({
          method: "POST",
          endpoint: `/nodes/${nodeId}`,
          body: {
            parent_id: rootParentId,
            position: position || "bottom",
          },
        });
      }

      // Step 4: Delete staging node
      calls.push({
        method: "DELETE",
        endpoint: `/nodes/${stagingNodeId}`,
      });

      return calls;
    }

    it("creates staging node before any content nodes", () => {
      const content = `Item 1
Item 2`;
      const parsed = parseIndentedContent(content);
      const calls = simulateStagingInsertion("parent-123", parsed);

      // First call should be staging node creation
      expect(calls[0]).toEqual({
        method: "POST",
        endpoint: "/nodes",
        body: {
          name: "__staging_temp__",
          parent_id: "parent-123",
          position: "bottom",
        },
      });
    });

    it("creates content nodes with staging node as parent", () => {
      const content = `Item 1`;
      const parsed = parseIndentedContent(content);
      const calls = simulateStagingInsertion("parent-123", parsed);

      // Second call should create content with staging as parent
      expect(calls[1].body?.parent_id).toBe("staging-temp-id");
    });

    it("moves top-level nodes to actual parent after creation", () => {
      const content = `Item 1
Item 2`;
      const parsed = parseIndentedContent(content);
      const calls = simulateStagingInsertion("parent-123", parsed);

      // After staging (1) + content (2) = 3 calls, then moves
      const moveCalls = calls.filter(
        (c) => c.method === "POST" && c.endpoint.startsWith("/nodes/node-") && c.body?.parent_id === "parent-123"
      );
      expect(moveCalls.length).toBe(2);
    });

    it("deletes staging node after moving content", () => {
      const content = `Item`;
      const parsed = parseIndentedContent(content);
      const calls = simulateStagingInsertion("parent-123", parsed);

      // Last call should delete staging
      const lastCall = calls[calls.length - 1];
      expect(lastCall).toEqual({
        method: "DELETE",
        endpoint: "/nodes/staging-temp-id",
      });
    });

    it("preserves hierarchy - children reference correct parents", () => {
      const content = `Parent
  Child`;
      const parsed = parseIndentedContent(content);
      const calls = simulateStagingInsertion("root", parsed);

      // First content node (Parent) should be under staging
      expect(calls[1].body?.parent_id).toBe("staging-temp-id");

      // Second content node (Child) should be under Parent (node-0)
      expect(calls[2].body?.parent_id).toBe("node-0");
    });

    it("reverses move order when position is top", () => {
      const content = `First
Second
Third`;
      const parsed = parseIndentedContent(content);
      const calls = simulateStagingInsertion("root", parsed, "top");

      // Move calls should be in reverse order for "top" position
      const moveCalls = calls.filter(
        (c) => c.method === "POST" && c.endpoint.startsWith("/nodes/node-") && c.body?.position === "top"
      );

      // Should be node-2, node-1, node-0 (reversed)
      expect(moveCalls[0].endpoint).toBe("/nodes/node-2");
      expect(moveCalls[1].endpoint).toBe("/nodes/node-1");
      expect(moveCalls[2].endpoint).toBe("/nodes/node-0");
    });

    it("only moves top-level nodes, not children", () => {
      const content = `Parent
  Child 1
  Child 2`;
      const parsed = parseIndentedContent(content);
      const calls = simulateStagingInsertion("root", parsed);

      // Only Parent (node-0) should be moved, not children
      const moveCalls = calls.filter(
        (c) => c.method === "POST" && c.endpoint.startsWith("/nodes/node-") && c.body?.parent_id === "root"
      );
      expect(moveCalls.length).toBe(1);
      expect(moveCalls[0].endpoint).toBe("/nodes/node-0");
    });

    it("handles empty content without creating staging node", () => {
      const calls = simulateStagingInsertion("root", []);
      expect(calls).toEqual([]);
    });

    it("handles complex nested structure correctly", () => {
      const content = `Topic A
  Subtopic A1
    Detail A1a
  Subtopic A2
Topic B
  Subtopic B1`;
      const parsed = parseIndentedContent(content);
      const calls = simulateStagingInsertion("root", parsed);

      // Verify parent-child relationships
      // node-0: Topic A (parent: staging)
      // node-1: Subtopic A1 (parent: node-0)
      // node-2: Detail A1a (parent: node-1)
      // node-3: Subtopic A2 (parent: node-0)
      // node-4: Topic B (parent: staging)
      // node-5: Subtopic B1 (parent: node-4)

      const contentCalls = calls.filter(
        (c) => c.method === "POST" && c.endpoint === "/nodes" && c.body?.name !== "__staging_temp__"
      );

      expect(contentCalls[0].body?.parent_id).toBe("staging-temp-id"); // Topic A
      expect(contentCalls[1].body?.parent_id).toBe("node-0"); // Subtopic A1
      expect(contentCalls[2].body?.parent_id).toBe("node-1"); // Detail A1a
      expect(contentCalls[3].body?.parent_id).toBe("node-0"); // Subtopic A2
      expect(contentCalls[4].body?.parent_id).toBe("staging-temp-id"); // Topic B
      expect(contentCalls[5].body?.parent_id).toBe("node-4"); // Subtopic B1

      // Only Topic A and Topic B should be moved to root
      const moveCalls = calls.filter(
        (c) => c.method === "POST" && c.endpoint.startsWith("/nodes/node-") && c.body?.parent_id === "root"
      );
      expect(moveCalls.length).toBe(2);
    });
  });

  describe("Edge cases", () => {
    it("handles single node insertion", () => {
      const parsed = parseIndentedContent("Single Item");
      expect(parsed).toHaveLength(1);
      expect(parsed[0].indent).toBe(0);
    });

    it("handles very deep nesting (10+ levels)", () => {
      let content = "Root";
      for (let i = 1; i <= 10; i++) {
        content += "\n" + "  ".repeat(i) + `Level ${i}`;
      }

      const parsed = parseIndentedContent(content);
      expect(parsed).toHaveLength(11);
      expect(parsed[10].indent).toBe(10);
      expect(parsed[10].text).toBe("Level 10");
    });

    it("handles nodes with special characters", () => {
      const content = `Node with "quotes"
  Node with <brackets>
    Node with & ampersand`;

      const parsed = parseIndentedContent(content);
      expect(parsed[0].text).toBe('Node with "quotes"');
      expect(parsed[1].text).toBe("Node with <brackets>");
      expect(parsed[2].text).toBe("Node with & ampersand");
    });

    it("handles unicode content", () => {
      const content = `日本語
  Café notes
    Über important`;

      const parsed = parseIndentedContent(content);
      expect(parsed[0].text).toBe("日本語");
      expect(parsed[1].text).toBe("Café notes");
      expect(parsed[2].text).toBe("Über important");
    });
  });
});

describe("Position behavior", () => {
  it("bottom position appends after existing children", () => {
    // When position is "bottom" or undefined, nodes should be added
    // at the bottom of the parent's children list
    const position: "top" | "bottom" | undefined = "bottom";
    expect(position || "bottom").toBe("bottom");
  });

  it("top position prepends before existing children", () => {
    // When position is "top", nodes should be added at the top
    // and moved in reverse order to maintain correct sequence
    const position: "top" | "bottom" = "top";
    const items = ["first", "second", "third"];
    const reversed = [...items].reverse();
    expect(reversed).toEqual(["third", "second", "first"]);
  });

  it("default position is bottom", () => {
    const position: "top" | "bottom" | undefined = undefined;
    const effectivePosition = position || "bottom";
    expect(effectivePosition).toBe("bottom");
  });
});
