import { describe, it, expect } from "vitest";
import {
  parseIndentedContent,
  formatNodesForSelection,
  escapeForDot,
  generateWorkflowyLink,
} from "./text-processing.js";
import type { NodeWithPath } from "../types/index.js";

describe("parseIndentedContent", () => {
  it("parses flat content with no indentation", () => {
    const content = "Line 1\nLine 2\nLine 3";
    const result = parseIndentedContent(content);
    expect(result).toEqual([
      { text: "Line 1", indent: 0 },
      { text: "Line 2", indent: 0 },
      { text: "Line 3", indent: 0 },
    ]);
  });

  it("parses content with 2-space indentation", () => {
    const content = "Parent\n  Child\n    Grandchild";
    const result = parseIndentedContent(content);
    expect(result).toEqual([
      { text: "Parent", indent: 0 },
      { text: "Child", indent: 1 },
      { text: "Grandchild", indent: 2 },
    ]);
  });

  it("parses content with tab indentation", () => {
    const content = "Parent\n\tChild\n\t\tGrandchild";
    const result = parseIndentedContent(content);
    expect(result).toEqual([
      { text: "Parent", indent: 0 },
      { text: "Child", indent: 1 },
      { text: "Grandchild", indent: 2 },
    ]);
  });

  it("skips empty lines", () => {
    const content = "Line 1\n\nLine 2\n   \nLine 3";
    const result = parseIndentedContent(content);
    expect(result).toEqual([
      { text: "Line 1", indent: 0 },
      { text: "Line 2", indent: 0 },
      { text: "Line 3", indent: 0 },
    ]);
  });

  it("handles deep nesting (10+ levels)", () => {
    const lines = [];
    for (let i = 0; i < 12; i++) {
      lines.push("  ".repeat(i) + `Level ${i}`);
    }
    const result = parseIndentedContent(lines.join("\n"));
    expect(result.length).toBe(12);
    expect(result[11]).toEqual({ text: "Level 11", indent: 11 });
  });

  it("handles mixed indentation (spaces and tabs)", () => {
    const content = "Root\n  Space child\n\tTab child";
    const result = parseIndentedContent(content);
    expect(result).toEqual([
      { text: "Root", indent: 0 },
      { text: "Space child", indent: 1 },
      { text: "Tab child", indent: 1 },
    ]);
  });
});

describe("formatNodesForSelection", () => {
  it("returns message for empty array", () => {
    const result = formatNodesForSelection([]);
    expect(result).toBe("No matching nodes found.");
  });

  it("formats single node", () => {
    const nodes: NodeWithPath[] = [
      { id: "abc123", name: "Test Node", path: "Parent > Test Node", depth: 2 },
    ];
    const result = formatNodesForSelection(nodes);
    expect(result).toContain("[1]");
    expect(result).toContain("Parent > Test Node");
    expect(result).toContain("ID: abc123");
  });

  it("includes truncated note if present", () => {
    const nodes: NodeWithPath[] = [
      {
        id: "abc123",
        name: "Test",
        note: "This is a very long note that should be truncated after fifty characters for display",
        path: "Test",
        depth: 1,
      },
    ];
    const result = formatNodesForSelection(nodes);
    expect(result).toContain("[note:");
    expect(result).toContain("...]");
  });
});

describe("escapeForDot", () => {
  it("escapes backslashes", () => {
    expect(escapeForDot("path\\to\\file")).toBe("path\\\\to\\\\file");
  });

  it("escapes double quotes", () => {
    expect(escapeForDot('say "hello"')).toBe('say \\"hello\\"');
  });

  it("escapes newlines", () => {
    expect(escapeForDot("line1\nline2")).toBe("line1\\nline2");
  });

  it("truncates long strings to 40 chars", () => {
    const longString = "a".repeat(50);
    expect(escapeForDot(longString).length).toBe(40);
  });
});

describe("generateWorkflowyLink", () => {
  it("generates valid Workflowy link", () => {
    const link = generateWorkflowyLink("abc123", "My Node");
    expect(link).toBe("[My Node](https://workflowy.com/#/abc123)");
  });

  it("uses 'Untitled' for empty name", () => {
    const link = generateWorkflowyLink("xyz789", "");
    expect(link).toBe("[Untitled](https://workflowy.com/#/xyz789)");
  });

  it("truncates long names to 50 chars", () => {
    const longName = "a".repeat(60);
    const link = generateWorkflowyLink("id", longName);
    expect(link).toContain("[" + "a".repeat(50) + "]");
  });
});
