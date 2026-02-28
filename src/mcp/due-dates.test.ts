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

describe("list_upcoming logic", () => {
  const today = new Date(2026, 1, 28); // Feb 28, 2026
  const dayMs = 24 * 60 * 60 * 1000;

  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "overdue", name: "Overdue due:2026-02-15" }),
    createMockNode({ id: "today", name: "Due today due:2026-02-28" }),
    createMockNode({ id: "soon", name: "Due in 5 days due:2026-03-05" }),
    createMockNode({ id: "later", name: "Due in 30 days due:2026-03-30" }),
    createMockNode({ id: "no-date", name: "No due date" }),
    createMockNode({ id: "completed", name: "Completed due:2026-02-20", completedAt: 1700000000 }),
    createMockNode({ id: "proj-root", name: "Root" }),
    createMockNode({ id: "child", name: "Child due:2026-03-01", parent_id: "proj-root" }),
  ];

  function listUpcoming(
    allNodes: WorkflowyNode[],
    options: { days?: number; root_id?: string; include_no_due_date?: boolean; limit?: number } = {}
  ) {
    const { days = 14, root_id, include_no_due_date = false, limit = 50 } = options;
    const cutoff = new Date(today);
    cutoff.setDate(cutoff.getDate() + days);

    let candidates = allNodes;
    if (root_id) {
      candidates = getSubtreeNodes(root_id, allNodes);
    }

    const incomplete = candidates.filter((n) => !n.completedAt);
    const upcoming: Array<{ node: WorkflowyNode; dueDate: Date; daysUntilDue: number; overdue: boolean }> = [];
    const noDueDate: WorkflowyNode[] = [];

    for (const node of incomplete) {
      const dueInfo = parseDueDateFromNode(node);
      if (dueInfo) {
        const daysUntil = Math.floor((dueInfo.date.getTime() - today.getTime()) / dayMs);
        if (dueInfo.date <= cutoff) {
          upcoming.push({ node, dueDate: dueInfo.date, daysUntilDue: daysUntil, overdue: daysUntil < 0 });
        }
      } else if (include_no_due_date) {
        noDueDate.push(node);
      }
    }

    upcoming.sort((a, b) => a.dueDate.getTime() - b.dueDate.getTime());

    let allResults = upcoming.map((u) => ({
      id: u.node.id, due_date: u.dueDate.toISOString().split("T")[0],
      days_until_due: u.daysUntilDue, overdue: u.overdue,
    }));

    if (include_no_due_date) {
      const noDueMapped = noDueDate.map((n) => ({
        id: n.id, due_date: null as string | null, days_until_due: null as number | null, overdue: false,
      }));
      allResults = [...allResults, ...noDueMapped] as typeof allResults;
    }

    return allResults.slice(0, limit);
  }

  describe("basic filtering", () => {
    it("includes overdue items", () => {
      const results = listUpcoming(mockNodes);
      expect(results.map((r) => r.id)).toContain("overdue");
    });

    it("includes items due today", () => {
      const results = listUpcoming(mockNodes);
      expect(results.map((r) => r.id)).toContain("today");
    });

    it("includes items within range", () => {
      const results = listUpcoming(mockNodes, { days: 14 });
      expect(results.map((r) => r.id)).toContain("soon");
    });

    it("excludes items beyond range", () => {
      const results = listUpcoming(mockNodes, { days: 14 });
      expect(results.map((r) => r.id)).not.toContain("later");
    });

    it("excludes completed items", () => {
      const results = listUpcoming(mockNodes);
      expect(results.map((r) => r.id)).not.toContain("completed");
    });

    it("excludes items with no due date by default", () => {
      const results = listUpcoming(mockNodes);
      expect(results.map((r) => r.id)).not.toContain("no-date");
    });
  });

  describe("sorting", () => {
    it("sorts by due date ascending (overdue first)", () => {
      const results = listUpcoming(mockNodes);
      const withDates = results.filter((r) => r.due_date);
      for (let i = 1; i < withDates.length; i++) {
        expect(withDates[i - 1].due_date! <= withDates[i].due_date!).toBe(true);
      }
    });

    it("marks overdue flag correctly", () => {
      const results = listUpcoming(mockNodes);
      const overdueResult = results.find((r) => r.id === "overdue");
      expect(overdueResult?.overdue).toBe(true);
      const todayResult = results.find((r) => r.id === "today");
      expect(todayResult?.overdue).toBe(false);
    });
  });

  describe("include_no_due_date option", () => {
    it("appends no-due-date items at end", () => {
      const results = listUpcoming(mockNodes, { include_no_due_date: true });
      expect(results.map((r) => r.id)).toContain("no-date");
      // No-due-date items should be at the end
      const noDueDateIdx = results.findIndex((r) => r.id === "no-date");
      const lastDueDateIdx = results.reduce((max, r, i) => r.due_date ? i : max, -1);
      expect(noDueDateIdx).toBeGreaterThan(lastDueDateIdx);
    });
  });

  describe("scoping", () => {
    it("limits to subtree", () => {
      const results = listUpcoming(mockNodes, { root_id: "proj-root" });
      expect(results.map((r) => r.id)).toContain("child");
      expect(results.map((r) => r.id)).not.toContain("overdue");
    });
  });

  describe("limit", () => {
    it("respects limit", () => {
      const results = listUpcoming(mockNodes, { limit: 2 });
      expect(results).toHaveLength(2);
    });
  });
});

describe("list_overdue logic", () => {
  const today = new Date(2026, 1, 28);
  const dayMs = 24 * 60 * 60 * 1000;

  const mockNodes: WorkflowyNode[] = [
    createMockNode({ id: "very-overdue", name: "Task due:2026-01-15" }),
    createMockNode({ id: "slightly-overdue", name: "Task due:2026-02-25" }),
    createMockNode({ id: "not-overdue", name: "Task due:2026-03-15" }),
    createMockNode({ id: "due-today", name: "Task due:2026-02-28" }),
    createMockNode({ id: "completed-overdue", name: "Task due:2026-01-01", completedAt: 1700000000 }),
    createMockNode({ id: "no-date", name: "No date" }),
  ];

  function listOverdue(
    allNodes: WorkflowyNode[],
    options: { root_id?: string; include_completed?: boolean; limit?: number } = {}
  ) {
    const { include_completed = false, limit = 50 } = options;

    const overdue: Array<{ node: WorkflowyNode; dueDate: Date; daysOverdue: number }> = [];

    for (const node of allNodes) {
      if (!include_completed && node.completedAt) continue;
      const dueInfo = parseDueDateFromNode(node);
      if (dueInfo && dueInfo.date < today) {
        const daysOver = Math.floor((today.getTime() - dueInfo.date.getTime()) / dayMs);
        overdue.push({ node, dueDate: dueInfo.date, daysOverdue: daysOver });
      }
    }

    overdue.sort((a, b) => b.daysOverdue - a.daysOverdue);
    return overdue.slice(0, limit).map((o) => ({
      id: o.node.id, due_date: o.dueDate.toISOString().split("T")[0], days_overdue: o.daysOverdue,
    }));
  }

  describe("basic filtering", () => {
    it("includes overdue items", () => {
      const results = listOverdue(mockNodes);
      expect(results.map((r) => r.id)).toContain("very-overdue");
      expect(results.map((r) => r.id)).toContain("slightly-overdue");
    });

    it("excludes items due today or later", () => {
      const results = listOverdue(mockNodes);
      expect(results.map((r) => r.id)).not.toContain("not-overdue");
      expect(results.map((r) => r.id)).not.toContain("due-today");
    });

    it("excludes completed items by default", () => {
      const results = listOverdue(mockNodes);
      expect(results.map((r) => r.id)).not.toContain("completed-overdue");
    });

    it("includes completed items when requested", () => {
      const results = listOverdue(mockNodes, { include_completed: true });
      expect(results.map((r) => r.id)).toContain("completed-overdue");
    });

    it("excludes items with no date", () => {
      const results = listOverdue(mockNodes);
      expect(results.map((r) => r.id)).not.toContain("no-date");
    });
  });

  describe("sorting", () => {
    it("sorts most overdue first", () => {
      const results = listOverdue(mockNodes);
      expect(results[0].id).toBe("very-overdue");
      expect(results[1].id).toBe("slightly-overdue");
    });

    it("calculates days_overdue correctly", () => {
      const results = listOverdue(mockNodes);
      const veryOverdue = results.find((r) => r.id === "very-overdue");
      // Jan 15 to Feb 28 = 44 days
      expect(veryOverdue?.days_overdue).toBe(44);
    });
  });

  describe("limit", () => {
    it("respects limit", () => {
      const results = listOverdue(mockNodes, { limit: 1 });
      expect(results).toHaveLength(1);
    });
  });
});
