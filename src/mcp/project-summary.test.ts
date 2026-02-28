import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";
import { getSubtreeNodes } from "../shared/utils/scope-utils.js";
import { parseNodeTags } from "../shared/utils/tag-parser.js";
import { parseDueDateFromNode, isOverdue } from "../shared/utils/date-parser.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("get_project_summary logic", () => {
  const now = new Date(2026, 1, 28);
  const dayMs = 24 * 60 * 60 * 1000;

  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "proj", name: "My Project #active" }),
    createMockNode({ id: "t1", name: "Task 1 #inbox @alice", parent_id: "proj", layoutMode: "todo" }),
    createMockNode({ id: "t2", name: "Task 2 #review due:2026-02-20", parent_id: "proj", layoutMode: "todo" }),
    createMockNode({ id: "t3", name: "Done task #review", parent_id: "proj", layoutMode: "todo", completedAt: 1700000000 }),
    createMockNode({ id: "t4", name: "Sub task", parent_id: "t1", modifiedAt: now.getTime() - dayMs }),
    createMockNode({ id: "t5", name: "Old task", parent_id: "proj", modifiedAt: now.getTime() - 30 * dayMs }),
  ];

  function getProjectSummary(nodeId: string, allNodes: WorkflowyNode[], options: { include_tags?: boolean; recently_modified_days?: number } = {}) {
    const { include_tags = true, recently_modified_days = 7 } = options;
    const subtreeNodes = getSubtreeNodes(nodeId, allNodes);
    if (subtreeNodes.length === 0) return null;

    const rootNode = subtreeNodes[0];
    let todoPending = 0;
    let todoCompleted = 0;
    let overdueCount = 0;
    let hasDueDates = false;
    const tagCounts: Record<string, number> = {};
    const assigneeCounts: Record<string, number> = {};
    const cutoffMs = now.getTime() - recently_modified_days * dayMs;
    const recentlyModified: WorkflowyNode[] = [];

    for (const node of subtreeNodes) {
      const isTodo = node.layoutMode === "todo" || /^\[[ x]\]/.test(node.name || "");
      if (isTodo) {
        if (node.completedAt) todoCompleted++;
        else todoPending++;
      }

      const dueInfo = parseDueDateFromNode(node);
      if (dueInfo) {
        hasDueDates = true;
        if (!node.completedAt && isOverdue(node, now)) overdueCount++;
      }

      if (include_tags) {
        const parsed = parseNodeTags(node);
        for (const t of parsed.tags) tagCounts[`#${t}`] = (tagCounts[`#${t}`] || 0) + 1;
        for (const a of parsed.assignees) assigneeCounts[`@${a}`] = (assigneeCounts[`@${a}`] || 0) + 1;
      }

      if (node.modifiedAt && node.modifiedAt > cutoffMs) recentlyModified.push(node);
    }

    const todoTotal = todoPending + todoCompleted;
    return {
      root: { id: rootNode.id, name: rootNode.name },
      stats: {
        total_nodes: subtreeNodes.length,
        todo_total: todoTotal,
        todo_pending: todoPending,
        todo_completed: todoCompleted,
        completion_percent: todoTotal > 0 ? Math.round((todoCompleted / todoTotal) * 100) : 0,
        has_due_dates: hasDueDates,
        overdue_count: overdueCount,
      },
      tags: include_tags ? tagCounts : undefined,
      assignees: include_tags ? assigneeCounts : undefined,
      recently_modified: recentlyModified,
    };
  }

  describe("subtree identification", () => {
    it("includes root and all descendants", () => {
      const summary = getProjectSummary("proj", mockNodes);
      expect(summary).not.toBeNull();
      expect(summary!.stats.total_nodes).toBe(6); // proj + 5 children/grandchildren
    });

    it("returns null for non-existent node", () => {
      const summary = getProjectSummary("nonexistent", mockNodes);
      expect(summary).toBeNull();
    });
  });

  describe("todo statistics", () => {
    it("counts pending todos", () => {
      const summary = getProjectSummary("proj", mockNodes)!;
      expect(summary.stats.todo_pending).toBe(2); // t1, t2
    });

    it("counts completed todos", () => {
      const summary = getProjectSummary("proj", mockNodes)!;
      expect(summary.stats.todo_completed).toBe(1); // t3
    });

    it("calculates completion percentage", () => {
      const summary = getProjectSummary("proj", mockNodes)!;
      expect(summary.stats.completion_percent).toBe(33); // 1/3
    });
  });

  describe("due date awareness", () => {
    it("detects due dates exist", () => {
      const summary = getProjectSummary("proj", mockNodes)!;
      expect(summary.stats.has_due_dates).toBe(true);
    });

    it("counts overdue items", () => {
      const summary = getProjectSummary("proj", mockNodes)!;
      expect(summary.stats.overdue_count).toBe(1); // t2 is due 2026-02-20, before Feb 28
    });
  });

  describe("tag aggregation", () => {
    it("counts tags across subtree", () => {
      const summary = getProjectSummary("proj", mockNodes)!;
      expect(summary.tags!["#review"]).toBe(2); // t2, t3
      expect(summary.tags!["#inbox"]).toBe(1); // t1
    });

    it("counts assignees", () => {
      const summary = getProjectSummary("proj", mockNodes)!;
      expect(summary.assignees!["@alice"]).toBe(1);
    });

    it("omits tags when include_tags is false", () => {
      const summary = getProjectSummary("proj", mockNodes, { include_tags: false })!;
      expect(summary.tags).toBeUndefined();
      expect(summary.assignees).toBeUndefined();
    });
  });

  describe("recently modified", () => {
    it("includes recently modified nodes", () => {
      const summary = getProjectSummary("proj", mockNodes, { recently_modified_days: 7 })!;
      expect(summary.recently_modified.map((n) => n.id)).toContain("t4");
    });

    it("excludes old nodes", () => {
      const summary = getProjectSummary("proj", mockNodes, { recently_modified_days: 7 })!;
      expect(summary.recently_modified.map((n) => n.id)).not.toContain("t5");
    });
  });
});
