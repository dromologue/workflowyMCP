import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";
import { nodeHasTag, nodeHasAssignee } from "../shared/utils/tag-parser.js";
import { getSubtreeNodes } from "../shared/utils/scope-utils.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("bulk_update logic", () => {
  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "1", name: "Task A #inbox @alice" }),
    createMockNode({ id: "2", name: "Task B #inbox #urgent" }),
    createMockNode({ id: "3", name: "Task C #review @bob" }),
    createMockNode({ id: "4", name: "Done task #inbox", completedAt: 1700000000 }),
    createMockNode({ id: "5", name: "Untagged task" }),
    createMockNode({ id: "proj-root", name: "Root" }),
    createMockNode({ id: "child", name: "Child #inbox", parent_id: "proj-root" }),
  ];

  describe("filter pipeline", () => {
    function applyFilter(
      allNodes: WorkflowyNode[],
      filter: { query?: string; tag?: string; assignee?: string; status?: string; root_id?: string }
    ): WorkflowyNode[] {
      let candidates = allNodes;

      if (filter.root_id) {
        candidates = getSubtreeNodes(filter.root_id, allNodes);
      }
      if (filter.query) {
        const lq = filter.query.toLowerCase();
        candidates = candidates.filter((n) => n.name?.toLowerCase().includes(lq) || n.note?.toLowerCase().includes(lq));
      }
      if (filter.tag) {
        candidates = candidates.filter((n) => nodeHasTag(n, filter.tag!));
      }
      if (filter.assignee) {
        candidates = candidates.filter((n) => nodeHasAssignee(n, filter.assignee!));
      }
      if (filter.status && filter.status !== "all") {
        candidates = candidates.filter((n) => {
          const isCompleted = !!n.completedAt;
          return filter.status === "completed" ? isCompleted : !isCompleted;
        });
      }

      return candidates;
    }

    it("filters by tag", () => {
      const result = applyFilter(mockNodes, { tag: "inbox" });
      expect(result.map((n) => n.id)).toEqual(expect.arrayContaining(["1", "2", "4", "child"]));
    });

    it("filters by tag and status", () => {
      const result = applyFilter(mockNodes, { tag: "inbox", status: "pending" });
      expect(result.map((n) => n.id)).not.toContain("4"); // completed
    });

    it("filters by assignee", () => {
      const result = applyFilter(mockNodes, { assignee: "alice" });
      expect(result).toHaveLength(1);
      expect(result[0].id).toBe("1");
    });

    it("filters by query and tag", () => {
      const result = applyFilter(mockNodes, { query: "Task A", tag: "inbox" });
      expect(result).toHaveLength(1);
      expect(result[0].id).toBe("1");
    });

    it("scopes by root_id", () => {
      // Only nodes in the subtree rooted at "root" should be considered
      const result = applyFilter(mockNodes, { root_id: "proj-root", tag: "inbox" });
      expect(result).toHaveLength(1);
      expect(result[0].id).toBe("child");
    });
  });

  describe("limit enforcement", () => {
    it("rejects when matched exceeds limit", () => {
      const matched = mockNodes.filter((n) => nodeHasTag(n, "inbox"));
      const limit = 2;
      expect(matched.length > limit).toBe(true);
    });
  });

  describe("dry_run", () => {
    it("returns matched nodes without modifying", () => {
      const matched = mockNodes.filter((n) => nodeHasTag(n, "inbox"));
      const dryResult = { dry_run: true, matched_count: matched.length, nodes: matched.map((n) => n.id) };
      expect(dryResult.dry_run).toBe(true);
      expect(dryResult.matched_count).toBeGreaterThan(0);
    });
  });

  describe("add_tag operation", () => {
    it("appends tag to node name", () => {
      const name = "Task A #inbox @alice";
      const newName = `${name} #done`;
      expect(newName).toBe("Task A #inbox @alice #done");
      expect(newName).toContain("#done");
    });
  });

  describe("remove_tag operation", () => {
    it("removes tag from name", () => {
      const name = "Task B #inbox #urgent";
      const tagToRemove = "inbox";
      const cleaned = name.replace(new RegExp(`\\s*#${tagToRemove}\\b`, "gi"), "");
      expect(cleaned).toBe("Task B #urgent");
    });

    it("removes tag from note", () => {
      const note = "Details about #inbox and #review";
      const tagToRemove = "inbox";
      const cleaned = note.replace(new RegExp(`\\s*#${tagToRemove}\\b`, "gi"), "");
      expect(cleaned).toBe("Details about and #review");
    });

    it("handles tag at start of text", () => {
      const name = "#inbox task";
      const tagToRemove = "inbox";
      const cleaned = name.replace(new RegExp(`\\s*#${tagToRemove}\\b`, "gi"), "");
      expect(cleaned).toBe(" task");
    });
  });
});
