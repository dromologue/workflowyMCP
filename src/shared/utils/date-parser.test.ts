import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../types/index.js";
import { parseDueDateFromNode, isOverdue, isDueWithin } from "./date-parser.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("date-parser", () => {
  describe("parseDueDateFromNode", () => {
    it("parses due:YYYY-MM-DD from name", () => {
      const node = createMockNode({ name: "Task due:2026-03-15" });
      const result = parseDueDateFromNode(node);
      expect(result).not.toBeNull();
      expect(result!.date.getFullYear()).toBe(2026);
      expect(result!.date.getMonth()).toBe(2); // March = 2
      expect(result!.date.getDate()).toBe(15);
      expect(result!.rawMatch).toBe("due:2026-03-15");
    });

    it("parses #due-YYYY-MM-DD from name", () => {
      const node = createMockNode({ name: "Task #due-2026-04-01" });
      const result = parseDueDateFromNode(node);
      expect(result).not.toBeNull();
      expect(result!.date.getFullYear()).toBe(2026);
      expect(result!.date.getMonth()).toBe(3);
      expect(result!.rawMatch).toBe("#due-2026-04-01");
    });

    it("parses bare YYYY-MM-DD from name", () => {
      const node = createMockNode({ name: "Task 2026-05-20" });
      const result = parseDueDateFromNode(node);
      expect(result).not.toBeNull();
      expect(result!.date.getMonth()).toBe(4);
      expect(result!.rawMatch).toBe("2026-05-20");
    });

    it("prioritizes due: over #due- over bare date", () => {
      const node = createMockNode({
        name: "Task #due-2026-01-01 due:2026-02-02 2026-03-03",
      });
      const result = parseDueDateFromNode(node);
      expect(result).not.toBeNull();
      expect(result!.rawMatch).toBe("due:2026-02-02");
    });

    it("prioritizes #due- over bare date", () => {
      const node = createMockNode({
        name: "Task #due-2026-01-01 2026-03-03",
      });
      const result = parseDueDateFromNode(node);
      expect(result!.rawMatch).toBe("#due-2026-01-01");
    });

    it("checks note if name has no date", () => {
      const node = createMockNode({
        name: "A plain task",
        note: "due:2026-06-30",
      });
      const result = parseDueDateFromNode(node);
      expect(result).not.toBeNull();
      expect(result!.date.getMonth()).toBe(5);
    });

    it("prefers name date over note date", () => {
      const node = createMockNode({
        name: "Task due:2026-01-15",
        note: "due:2026-12-25",
      });
      const result = parseDueDateFromNode(node);
      expect(result!.rawMatch).toBe("due:2026-01-15");
    });

    it("returns null for no date", () => {
      const node = createMockNode({ name: "No date here" });
      expect(parseDueDateFromNode(node)).toBeNull();
    });

    it("rejects invalid dates", () => {
      const node = createMockNode({ name: "Task due:2026-02-30" });
      expect(parseDueDateFromNode(node)).toBeNull();
    });

    it("rejects month 13", () => {
      const node = createMockNode({ name: "Task due:2026-13-01" });
      expect(parseDueDateFromNode(node)).toBeNull();
    });

    it("is case-insensitive for due: prefix", () => {
      const node = createMockNode({ name: "Task DUE:2026-03-15" });
      const result = parseDueDateFromNode(node);
      expect(result).not.toBeNull();
    });
  });

  describe("isOverdue", () => {
    const now = new Date(2026, 1, 28); // Feb 28, 2026

    it("returns true for past due date", () => {
      const node = createMockNode({ name: "Task due:2026-02-15" });
      expect(isOverdue(node, now)).toBe(true);
    });

    it("returns false for future due date", () => {
      const node = createMockNode({ name: "Task due:2026-03-15" });
      expect(isOverdue(node, now)).toBe(false);
    });

    it("returns false for today's due date", () => {
      const node = createMockNode({ name: "Task due:2026-02-28" });
      expect(isOverdue(node, now)).toBe(false);
    });

    it("returns false for completed node", () => {
      const node = createMockNode({
        name: "Task due:2026-01-01",
        completedAt: 1700000000,
      });
      expect(isOverdue(node, now)).toBe(false);
    });

    it("returns false for node with no due date", () => {
      const node = createMockNode({ name: "No date" });
      expect(isOverdue(node, now)).toBe(false);
    });
  });

  describe("isDueWithin", () => {
    const now = new Date(2026, 1, 28); // Feb 28, 2026

    it("returns true for due date within range", () => {
      const node = createMockNode({ name: "Task due:2026-03-05" });
      expect(isDueWithin(node, 7, now)).toBe(true);
    });

    it("returns true for due date on boundary", () => {
      const node = createMockNode({ name: "Task due:2026-03-07" });
      expect(isDueWithin(node, 7, now)).toBe(true);
    });

    it("returns false for due date beyond range", () => {
      const node = createMockNode({ name: "Task due:2026-04-01" });
      expect(isDueWithin(node, 7, now)).toBe(false);
    });

    it("returns false for past due date", () => {
      const node = createMockNode({ name: "Task due:2026-02-20" });
      expect(isDueWithin(node, 7, now)).toBe(false);
    });

    it("returns true for today's due date", () => {
      const node = createMockNode({ name: "Task due:2026-02-28" });
      expect(isDueWithin(node, 7, now)).toBe(true);
    });

    it("returns false for no due date", () => {
      const node = createMockNode({ name: "No date" });
      expect(isDueWithin(node, 7, now)).toBe(false);
    });
  });
});
