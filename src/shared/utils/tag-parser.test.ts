import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../types/index.js";
import {
  parseTags,
  parseNodeTags,
  nodeHasTag,
  nodeHasAssignee,
} from "./tag-parser.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("tag-parser", () => {
  describe("parseTags", () => {
    it("extracts tags from text", () => {
      const result = parseTags("Working on #project-alpha and #review");
      expect(result.tags).toEqual(["project-alpha", "review"]);
      expect(result.assignees).toHaveLength(0);
    });

    it("extracts assignees from text", () => {
      const result = parseTags("Assigned to @alice and @bob-smith");
      expect(result.assignees).toEqual(["alice", "bob-smith"]);
      expect(result.tags).toHaveLength(0);
    });

    it("extracts both tags and assignees", () => {
      const result = parseTags("#inbox task for @alice #urgent");
      expect(result.tags).toEqual(["inbox", "urgent"]);
      expect(result.assignees).toEqual(["alice"]);
    });

    it("deduplicates tags", () => {
      const result = parseTags("#review something #review again");
      expect(result.tags).toEqual(["review"]);
    });

    it("lowercases all tags and assignees", () => {
      const result = parseTags("#URGENT task for @Alice");
      expect(result.tags).toEqual(["urgent"]);
      expect(result.assignees).toEqual(["alice"]);
    });

    it("returns empty arrays for empty text", () => {
      const result = parseTags("");
      expect(result.tags).toHaveLength(0);
      expect(result.assignees).toHaveLength(0);
    });

    it("returns empty arrays for text with no tags", () => {
      const result = parseTags("Just a regular sentence");
      expect(result.tags).toHaveLength(0);
      expect(result.assignees).toHaveLength(0);
    });

    it("handles tags with hyphens and underscores", () => {
      const result = parseTags("#my-tag #my_tag #simple");
      expect(result.tags).toEqual(["my-tag", "my_tag", "simple"]);
    });
  });

  describe("parseNodeTags", () => {
    it("merges tags from name and note", () => {
      const node = createMockNode({
        name: "#inbox task",
        note: "#review notes for @bob",
      });
      const result = parseNodeTags(node);
      expect(result.tags).toContain("inbox");
      expect(result.tags).toContain("review");
      expect(result.assignees).toContain("bob");
    });

    it("deduplicates across name and note", () => {
      const node = createMockNode({
        name: "#urgent task",
        note: "This is #urgent",
      });
      const result = parseNodeTags(node);
      expect(result.tags).toEqual(["urgent"]);
    });

    it("handles node with no note", () => {
      const node = createMockNode({ name: "#inbox item" });
      const result = parseNodeTags(node);
      expect(result.tags).toEqual(["inbox"]);
    });
  });

  describe("nodeHasTag", () => {
    const node = createMockNode({
      name: "#inbox task #urgent",
      note: "Details about #review",
    });

    it("finds tag without # prefix", () => {
      expect(nodeHasTag(node, "inbox")).toBe(true);
    });

    it("finds tag with # prefix", () => {
      expect(nodeHasTag(node, "#inbox")).toBe(true);
    });

    it("finds tag in note", () => {
      expect(nodeHasTag(node, "review")).toBe(true);
    });

    it("returns false for missing tag", () => {
      expect(nodeHasTag(node, "missing")).toBe(false);
    });

    it("is case-insensitive", () => {
      expect(nodeHasTag(node, "URGENT")).toBe(true);
    });
  });

  describe("nodeHasAssignee", () => {
    const node = createMockNode({
      name: "Task for @alice",
      note: "Also @bob-smith",
    });

    it("finds assignee without @ prefix", () => {
      expect(nodeHasAssignee(node, "alice")).toBe(true);
    });

    it("finds assignee with @ prefix", () => {
      expect(nodeHasAssignee(node, "@alice")).toBe(true);
    });

    it("finds assignee in note", () => {
      expect(nodeHasAssignee(node, "bob-smith")).toBe(true);
    });

    it("returns false for missing assignee", () => {
      expect(nodeHasAssignee(node, "charlie")).toBe(false);
    });

    it("is case-insensitive", () => {
      expect(nodeHasAssignee(node, "ALICE")).toBe(true);
    });
  });
});
