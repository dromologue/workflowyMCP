import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";
import { parseDueDateFromNode } from "../shared/utils/date-parser.js";
import { getSubtreeNodes } from "../shared/utils/scope-utils.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("daily_review logic", () => {
  const now = new Date(2026, 1, 28);
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const dayMs = 24 * 60 * 60 * 1000;

  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "workspace", name: "Workspace" }),
    // Overdue
    createMockNode({ id: "overdue1", name: "[ ] Overdue task due:2026-02-15", parent_id: "workspace", layoutMode: "todo" }),
    createMockNode({ id: "overdue2", name: "[ ] Very overdue due:2026-01-01", parent_id: "workspace", layoutMode: "todo" }),
    // Due today
    createMockNode({ id: "due-today", name: "[ ] Due today due:2026-02-28", parent_id: "workspace", layoutMode: "todo" }),
    // Due soon
    createMockNode({ id: "due-soon", name: "[ ] Due in 3 days due:2026-03-03", parent_id: "workspace", layoutMode: "todo" }),
    // Modified recently
    createMockNode({ id: "recent", name: "Modified today", parent_id: "workspace", modifiedAt: now.getTime() - dayMs / 2 }),
    // Completed
    createMockNode({ id: "done", name: "[x] Completed", parent_id: "workspace", layoutMode: "todo", completedAt: 1700000000 }),
    // Pending todo without date
    createMockNode({ id: "pending", name: "[ ] Just pending", parent_id: "workspace", layoutMode: "todo" }),
    // Old node
    createMockNode({ id: "old", name: "Old node", parent_id: "workspace", modifiedAt: now.getTime() - 30 * dayMs }),
  ];

  function dailyReview(
    allNodes: WorkflowyNode[],
    options: { root_id?: string; overdue_limit?: number; upcoming_days?: number; recent_days?: number; pending_limit?: number } = {}
  ) {
    const { root_id, overdue_limit = 10, upcoming_days = 7, recent_days = 1, pending_limit = 20 } = options;

    let candidates = allNodes;
    if (root_id) candidates = getSubtreeNodes(root_id, allNodes);

    let pendingTodos = 0;
    let overdueCount = 0;
    let dueTodayCount = 0;
    const recentCutoffMs = now.getTime() - recent_days * dayMs;
    let modifiedTodayCount = 0;

    const overdueItems: Array<{ node: WorkflowyNode; daysOverdue: number }> = [];
    const upcomingItems: Array<{ node: WorkflowyNode; dueDate: Date; daysUntilDue: number }> = [];
    const recentChanges: WorkflowyNode[] = [];
    const pendingNodes: WorkflowyNode[] = [];

    const cutoffDate = new Date(today);
    cutoffDate.setDate(cutoffDate.getDate() + upcoming_days);

    for (const node of candidates) {
      const isIncomplete = !node.completedAt;
      const dueInfo = parseDueDateFromNode(node);

      const isTodo = node.layoutMode === "todo" || /^\[[ x]\]/.test(node.name || "");
      if (isIncomplete && isTodo) {
        pendingTodos++;
        pendingNodes.push(node);
      }

      if (dueInfo && isIncomplete) {
        const daysUntil = Math.floor((dueInfo.date.getTime() - today.getTime()) / dayMs);
        if (daysUntil < 0) {
          overdueCount++;
          overdueItems.push({ node, daysOverdue: -daysUntil });
        } else if (daysUntil === 0) {
          dueTodayCount++;
          upcomingItems.push({ node, dueDate: dueInfo.date, daysUntilDue: 0 });
        } else if (dueInfo.date <= cutoffDate) {
          upcomingItems.push({ node, dueDate: dueInfo.date, daysUntilDue: daysUntil });
        }
      }

      if (node.modifiedAt && node.modifiedAt > recentCutoffMs) {
        modifiedTodayCount++;
        recentChanges.push(node);
      }
    }

    return {
      summary: { total_nodes: candidates.length, pending_todos: pendingTodos, overdue_count: overdueCount, due_today: dueTodayCount, modified_today: modifiedTodayCount },
      overdue: overdueItems.sort((a, b) => b.daysOverdue - a.daysOverdue).slice(0, overdue_limit),
      due_soon: upcomingItems.sort((a, b) => a.dueDate.getTime() - b.dueDate.getTime()),
      recent_changes: recentChanges.sort((a, b) => (b.modifiedAt || 0) - (a.modifiedAt || 0)),
      top_pending: pendingNodes.slice(0, pending_limit),
    };
  }

  describe("summary stats", () => {
    it("counts total nodes", () => {
      const review = dailyReview(mockNodes);
      expect(review.summary.total_nodes).toBe(mockNodes.length);
    });

    it("counts pending todos", () => {
      const review = dailyReview(mockNodes);
      // overdue1, overdue2, due-today, due-soon, pending = 5
      expect(review.summary.pending_todos).toBe(5);
    });

    it("counts overdue items", () => {
      const review = dailyReview(mockNodes);
      expect(review.summary.overdue_count).toBe(2);
    });

    it("counts due today", () => {
      const review = dailyReview(mockNodes);
      expect(review.summary.due_today).toBe(1);
    });

    it("counts recently modified", () => {
      const review = dailyReview(mockNodes);
      expect(review.summary.modified_today).toBeGreaterThanOrEqual(1);
    });
  });

  describe("overdue section", () => {
    it("lists overdue items sorted by most overdue first", () => {
      const review = dailyReview(mockNodes);
      expect(review.overdue).toHaveLength(2);
      expect(review.overdue[0].node.id).toBe("overdue2"); // Jan 1 is more overdue
      expect(review.overdue[1].node.id).toBe("overdue1");
    });

    it("respects overdue_limit", () => {
      const review = dailyReview(mockNodes, { overdue_limit: 1 });
      expect(review.overdue).toHaveLength(1);
    });
  });

  describe("due_soon section", () => {
    it("includes today and upcoming items", () => {
      const review = dailyReview(mockNodes, { upcoming_days: 7 });
      const ids = review.due_soon.map((u) => u.node.id);
      expect(ids).toContain("due-today");
      expect(ids).toContain("due-soon");
    });

    it("sorts by due date ascending", () => {
      const review = dailyReview(mockNodes, { upcoming_days: 7 });
      for (let i = 1; i < review.due_soon.length; i++) {
        expect(review.due_soon[i - 1].dueDate.getTime()).toBeLessThanOrEqual(review.due_soon[i].dueDate.getTime());
      }
    });
  });

  describe("recent_changes section", () => {
    it("includes recently modified nodes", () => {
      const review = dailyReview(mockNodes, { recent_days: 1 });
      const ids = review.recent_changes.map((n) => n.id);
      expect(ids).toContain("recent");
    });

    it("excludes old nodes", () => {
      const review = dailyReview(mockNodes, { recent_days: 1 });
      const ids = review.recent_changes.map((n) => n.id);
      expect(ids).not.toContain("old");
    });
  });

  describe("scoping", () => {
    it("limits to subtree when root_id provided", () => {
      const review = dailyReview(mockNodes, { root_id: "workspace" });
      expect(review.summary.total_nodes).toBe(mockNodes.length); // all are under workspace in this case
    });
  });

  describe("pending section", () => {
    it("lists top pending todos", () => {
      const review = dailyReview(mockNodes);
      const pendingIds = review.top_pending.map((n) => n.id);
      expect(pendingIds).toContain("pending");
      expect(pendingIds).not.toContain("done");
    });

    it("respects pending_limit", () => {
      const review = dailyReview(mockNodes, { pending_limit: 2 });
      expect(review.top_pending).toHaveLength(2);
    });
  });
});
