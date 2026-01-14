import { describe, it, expect } from "vitest";
import {
  parseIndentedContent,
  formatNodesForSelection,
  escapeForDot,
  generateWorkflowyLink,
  extractWorkflowyLinks,
  validateConceptMapInput,
  CONCEPT_MAP_LIMITS,
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

  it("preserves Unicode characters (accents, umlauts)", () => {
    expect(escapeForDot("Détournement")).toBe("Détournement");
    expect(escapeForDot("Übermensch")).toBe("Übermensch");
    expect(escapeForDot("phénoménologie")).toBe("phénoménologie");
  });

  it("safely truncates strings with Unicode characters", () => {
    // 45 characters with accents - should truncate to 37 + "..."
    const unicodeString = "é".repeat(45);
    const result = escapeForDot(unicodeString);
    expect(result.length).toBe(40);
    expect(result.endsWith("...")).toBe(true);
    // Should not have broken characters
    expect(result).not.toContain("\uFFFD"); // replacement character
  });

  it("removes DOT special characters", () => {
    expect(escapeForDot("test<>{}|value")).toBe("testvalue");
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

describe("CONCEPT_MAP_LIMITS", () => {
  it("has correct minimum concepts", () => {
    expect(CONCEPT_MAP_LIMITS.MIN_CONCEPTS).toBe(2);
  });

  it("has correct maximum concepts", () => {
    expect(CONCEPT_MAP_LIMITS.MAX_CONCEPTS).toBe(35);
  });

  it("has correct max label length", () => {
    expect(CONCEPT_MAP_LIMITS.MAX_LABEL_LENGTH).toBe(40);
  });

  it("has square image dimensions", () => {
    expect(CONCEPT_MAP_LIMITS.IMAGE_SIZE).toBe(2000);
  });
});

describe("validateConceptMapInput", () => {
  it("returns valid for array with 2+ concepts", () => {
    const result = validateConceptMapInput(["concept1", "concept2"]);
    expect(result.valid).toBe(true);
  });

  it("returns valid for array at max limit", () => {
    const concepts = Array.from({ length: 35 }, (_, i) => `concept${i}`);
    const result = validateConceptMapInput(concepts);
    expect(result.valid).toBe(true);
  });

  it("returns error for undefined concepts", () => {
    const result = validateConceptMapInput(undefined);
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.error).toContain("at least 2 concepts");
    }
  });

  it("returns error for empty array", () => {
    const result = validateConceptMapInput([]);
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.error).toContain("at least 2 concepts");
    }
  });

  it("returns error for single concept", () => {
    const result = validateConceptMapInput(["only-one"]);
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.error).toContain("at least 2 concepts");
    }
  });

  it("returns error for too many concepts", () => {
    const concepts = Array.from({ length: 70 }, (_, i) => `concept${i}`);
    const result = validateConceptMapInput(concepts);
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.error).toContain("Too many concepts");
      expect(result.provided).toBe(70);
      expect(result.maximum).toBe(35);
    }
  });

  it("returns error at exactly max + 1", () => {
    const concepts = Array.from({ length: 36 }, (_, i) => `concept${i}`);
    const result = validateConceptMapInput(concepts);
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.provided).toBe(36);
    }
  });

  it("includes helpful tip in error", () => {
    const result = validateConceptMapInput([]);
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.tip).toBeDefined();
      expect(result.tip.length).toBeGreaterThan(0);
    }
  });
});

describe("extractWorkflowyLinks", () => {
  it("extracts node ID from markdown link", () => {
    const text = "See [Related Topic](https://workflowy.com/#/abc123-def456)";
    const result = extractWorkflowyLinks(text);
    expect(result).toEqual(["abc123-def456"]);
  });

  it("extracts multiple links from text", () => {
    const text = `
      Check out [Topic A](https://workflowy.com/#/node-aaa) and
      also [Topic B](https://workflowy.com/#/node-bbb) for more info.
    `;
    const result = extractWorkflowyLinks(text);
    expect(result).toEqual(["node-aaa", "node-bbb"]);
  });

  it("extracts plain Workflowy URLs", () => {
    const text = "See https://workflowy.com/#/plain-node-id for details";
    const result = extractWorkflowyLinks(text);
    expect(result).toEqual(["plain-node-id"]);
  });

  it("handles mixed markdown and plain URLs", () => {
    const text = `
      Link: [Named](https://workflowy.com/#/named-id)
      Also: https://workflowy.com/#/plain-id
    `;
    const result = extractWorkflowyLinks(text);
    expect(result).toContain("named-id");
    expect(result).toContain("plain-id");
    expect(result.length).toBe(2);
  });

  it("returns empty array for text without links", () => {
    const text = "No Workflowy links here, just regular text.";
    const result = extractWorkflowyLinks(text);
    expect(result).toEqual([]);
  });

  it("deduplicates repeated links", () => {
    const text = `
      First: [Topic](https://workflowy.com/#/same-id)
      Second: [Topic Again](https://workflowy.com/#/same-id)
    `;
    const result = extractWorkflowyLinks(text);
    expect(result).toEqual(["same-id"]);
  });

  it("handles UUIDs as node IDs", () => {
    const text = "[Note](https://workflowy.com/#/550e8400-e29b-41d4-a716-446655440000)";
    const result = extractWorkflowyLinks(text);
    expect(result).toEqual(["550e8400-e29b-41d4-a716-446655440000"]);
  });

  it("ignores non-Workflowy URLs", () => {
    const text = `
      [Google](https://google.com)
      [Workflowy](https://workflowy.com/#/valid-id)
      [Other](https://example.com/#/fake-id)
    `;
    const result = extractWorkflowyLinks(text);
    expect(result).toEqual(["valid-id"]);
  });
});
