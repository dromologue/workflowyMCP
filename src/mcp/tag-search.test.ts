import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";
import { nodeHasTag, nodeHasAssignee, parseNodeTags } from "../shared/utils/tag-parser.js";
import { parseDueDateFromNode } from "../shared/utils/date-parser.js";
import { getSubtreeNodes, filterNodesByScope } from "../shared/utils/scope-utils.js";
import { buildNodePaths } from "../shared/utils/node-paths.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("enhanced search_nodes filter pipeline", () => {
  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "1", name: "Project #inbox @alice", parent_id: "root" }),
    createMockNode({ id: "2", name: "Task #review @bob due:2026-03-15", parent_id: "root" }),
    createMockNode({ id: "3", name: "Done item #review", completedAt: 1700000000, parent_id: "root" }),
    createMockNode({ id: "4", name: "Child task #inbox", parent_id: "1" }),
    createMockNode({ id: "5", name: "Untagged node", parent_id: "root", modifiedAt: Date.now() }),
    createMockNode({ id: "root", name: "Root" }),
  ];

  // Inline the filter pipeline matching server.ts logic
  function filterNodes(
    nodes: WorkflowyNode[],
    options: {
      query: string;
      tag?: string;
      assignee?: string;
      status?: "all" | "pending" | "completed";
      root_id?: string;
      modified_after?: string;
      modified_before?: string;
    }
  ) {
    let candidates = nodes;
    if (options.root_id) {
      candidates = getSubtreeNodes(options.root_id, nodes);
    }

    const lowerQuery = options.query.toLowerCase();
    let results = candidates.filter((node) => {
      const nameMatch = node.name?.toLowerCase().includes(lowerQuery);
      const noteMatch = node.note?.toLowerCase().includes(lowerQuery);
      return nameMatch || noteMatch;
    });

    if (options.tag) {
      results = results.filter((n) => nodeHasTag(n, options.tag!));
    }
    if (options.assignee) {
      results = results.filter((n) => nodeHasAssignee(n, options.assignee!));
    }
    if (options.status && options.status !== "all") {
      results = results.filter((n) => {
        const isCompleted = !!n.completedAt;
        return options.status === "completed" ? isCompleted : !isCompleted;
      });
    }
    if (options.modified_after) {
      const afterTs = new Date(options.modified_after).getTime();
      results = results.filter((n) => n.modifiedAt && n.modifiedAt > afterTs);
    }
    if (options.modified_before) {
      const beforeTs = new Date(options.modified_before).getTime();
      results = results.filter((n) => n.modifiedAt && n.modifiedAt < beforeTs);
    }

    return results;
  }

  describe("text query filter", () => {
    it("matches nodes by name", () => {
      const results = filterNodes(mockNodes, { query: "project" });
      expect(results).toHaveLength(1);
      expect(results[0].id).toBe("1");
    });

    it("matches multiple nodes", () => {
      const results = filterNodes(mockNodes, { query: "task" });
      expect(results).toHaveLength(2);
    });
  });

  describe("tag filter", () => {
    it("filters by tag", () => {
      const results = filterNodes(mockNodes, { query: "", tag: "#inbox" });
      // All nodes match "" query, then filter by inbox
      const allWithInbox = mockNodes.filter((n) => nodeHasTag(n, "inbox"));
      expect(results).toHaveLength(allWithInbox.length);
    });

    it("filters by tag without hash", () => {
      const results = filterNodes(mockNodes, { query: "", tag: "review" });
      const allWithReview = mockNodes.filter((n) => nodeHasTag(n, "review"));
      expect(results).toHaveLength(allWithReview.length);
    });
  });

  describe("assignee filter", () => {
    it("filters by assignee", () => {
      const results = filterNodes(mockNodes, { query: "", assignee: "@alice" });
      expect(results).toHaveLength(1);
      expect(results[0].id).toBe("1");
    });
  });

  describe("status filter", () => {
    it("filters pending only", () => {
      const results = filterNodes(mockNodes, { query: "", status: "pending" });
      expect(results.every((n) => !n.completedAt)).toBe(true);
    });

    it("filters completed only", () => {
      const results = filterNodes(mockNodes, { query: "", status: "completed" });
      expect(results.every((n) => !!n.completedAt)).toBe(true);
      expect(results).toHaveLength(1);
      expect(results[0].id).toBe("3");
    });

    it("all status returns everything", () => {
      const results = filterNodes(mockNodes, { query: "", status: "all" });
      expect(results).toHaveLength(mockNodes.length);
    });
  });

  describe("scope filter", () => {
    it("limits to subtree with root_id", () => {
      const results = filterNodes(mockNodes, { query: "", root_id: "1" });
      // getSubtreeNodes returns root + descendants
      expect(results.map((n) => n.id)).toContain("1");
      expect(results.map((n) => n.id)).toContain("4");
      expect(results.map((n) => n.id)).not.toContain("2");
    });
  });

  describe("combined filters", () => {
    it("applies tag and status together", () => {
      const results = filterNodes(mockNodes, { query: "", tag: "review", status: "pending" });
      expect(results).toHaveLength(1);
      expect(results[0].id).toBe("2");
    });

    it("applies query and tag together", () => {
      const results = filterNodes(mockNodes, { query: "task", tag: "inbox" });
      expect(results).toHaveLength(1);
      expect(results[0].id).toBe("4");
    });
  });

  describe("enriched output", () => {
    it("includes tags and due dates", () => {
      const node = mockNodes.find((n) => n.id === "2")!;
      const tags = parseNodeTags(node);
      const dueInfo = parseDueDateFromNode(node);
      expect(tags.tags).toContain("review");
      expect(tags.assignees).toContain("bob");
      expect(dueInfo).not.toBeNull();
      expect(dueInfo!.date.getFullYear()).toBe(2026);
    });
  });
});
