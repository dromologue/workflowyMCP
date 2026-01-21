/**
 * Tests for markdown-to-indented converter
 */

import { describe, it, expect } from "vitest";
import {
  convertMarkdownToIndented,
  looksLikeMarkdown,
} from "./markdown-converter.js";

describe("convertMarkdownToIndented", () => {
  describe("headers", () => {
    it("converts H1 to indent level 0", () => {
      const result = convertMarkdownToIndented("# Main Title");
      expect(result.content).toBe("Main Title");
      expect(result.nodeCount).toBe(1);
    });

    it("converts H2 to indent level 1", () => {
      const result = convertMarkdownToIndented("## Subtitle");
      expect(result.content).toBe("  Subtitle");
      expect(result.nodeCount).toBe(1);
    });

    it("converts multiple header levels", () => {
      const markdown = `# Title
## Section 1
### Subsection 1.1
## Section 2`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`Title
  Section 1
    Subsection 1.1
  Section 2`);
      expect(result.nodeCount).toBe(4);
    });

    it("handles H1 through H6", () => {
      const markdown = `# H1
## H2
### H3
#### H4
##### H5
###### H6`;

      const result = convertMarkdownToIndented(markdown);
      const lines = result.content.split("\n");
      expect(lines[0]).toBe("H1");
      expect(lines[1]).toBe("  H2");
      expect(lines[2]).toBe("    H3");
      expect(lines[3]).toBe("      H4");
      expect(lines[4]).toBe("        H5");
      expect(lines[5]).toBe("          H6");
    });
  });

  describe("lists", () => {
    it("converts unordered list items", () => {
      const markdown = `# Topic
- Item 1
- Item 2
- Item 3`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`Topic
  Item 1
  Item 2
  Item 3`);
      expect(result.nodeCount).toBe(4);
    });

    it("converts nested lists", () => {
      const markdown = `# Topic
- Item 1
  - Nested 1.1
  - Nested 1.2
- Item 2`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`Topic
  Item 1
    Nested 1.1
    Nested 1.2
  Item 2`);
      expect(result.nodeCount).toBe(5);
    });

    it("handles ordered lists", () => {
      const markdown = `# Steps
1. First step
2. Second step
3. Third step`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`Steps
  First step
  Second step
  Third step`);
    });

    it("handles mixed list markers", () => {
      const markdown = `# List
- Dash item
* Star item
+ Plus item`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`List
  Dash item
  Star item
  Plus item`);
    });
  });

  describe("paragraphs", () => {
    it("converts paragraphs as children of headers", () => {
      const markdown = `# Title
This is a paragraph under the title.
## Section
Another paragraph here.`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`Title
  This is a paragraph under the title.
  Section
    Another paragraph here.`);
    });

    it("handles content without headers", () => {
      const markdown = `Just some text
More text`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`  Just some text
  More text`);
      expect(result.warnings).toContain(
        "No markdown headers found - content will be inserted at root level"
      );
    });
  });

  describe("blockquotes", () => {
    it("preserves blockquote marker", () => {
      const markdown = `# Quote Section
> This is a quote`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`Quote Section
  > This is a quote`);
    });
  });

  describe("code blocks", () => {
    it("converts code blocks to single nodes", () => {
      const markdown = `# Code Example
\`\`\`javascript
const x = 1;
const y = 2;
\`\`\``;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toContain("Code Example");
      expect(result.content).toContain("[javascript]");
      expect(result.nodeCount).toBe(2);
    });

    it("handles code blocks without language", () => {
      const markdown = `# Example
\`\`\`
plain code
\`\`\``;

      const result = convertMarkdownToIndented(markdown);
      expect(result.nodeCount).toBe(2);
    });

    it("warns about unclosed code blocks", () => {
      const markdown = `# Example
\`\`\`
unclosed block`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.warnings).toContain(
        "Unclosed code block detected - content was included"
      );
    });
  });

  describe("empty and edge cases", () => {
    it("handles empty content", () => {
      const result = convertMarkdownToIndented("");
      expect(result.content).toBe("");
      expect(result.nodeCount).toBe(0);
    });

    it("handles only whitespace", () => {
      const result = convertMarkdownToIndented("   \n\n   ");
      expect(result.content).toBe("");
      expect(result.nodeCount).toBe(0);
    });

    it("skips blank lines", () => {
      const markdown = `# Title

## Section

Content`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.content).toBe(`Title
  Section
    Content`);
      expect(result.nodeCount).toBe(3);
    });
  });

  describe("complex documents", () => {
    it("converts a realistic document", () => {
      const markdown = `# Project Plan

## Overview
This project aims to improve efficiency.

## Goals
- Reduce latency
- Increase throughput
  - By 50%
  - Within Q2

## Timeline
### Phase 1
Initial setup and planning.
### Phase 2
Implementation begins.`;

      const result = convertMarkdownToIndented(markdown);
      expect(result.nodeCount).toBe(13); // 4 headers + 4 list items + 5 paragraphs
      expect(result.warnings).toBeUndefined();

      // Verify structure
      const lines = result.content.split("\n");
      expect(lines[0]).toBe("Project Plan"); // H1 at level 0
      expect(lines[1]).toBe("  Overview"); // H2 at level 1
      expect(lines[2]).toBe("    This project aims to improve efficiency."); // Paragraph at level 2
    });
  });
});

describe("looksLikeMarkdown", () => {
  it("detects headers", () => {
    expect(looksLikeMarkdown("# Title\n## Section")).toBe(true);
  });

  it("detects lists with other markdown", () => {
    // Lists alone are weak indicators, need 2+ patterns
    expect(looksLikeMarkdown("- Item 1\n- Item 2\n> quote")).toBe(true);
  });

  it("detects code blocks", () => {
    // Code blocks are strong indicators
    expect(looksLikeMarkdown("```\ncode\n```")).toBe(true);
  });

  it("detects links and formatting", () => {
    expect(looksLikeMarkdown("[link](url) and **bold**")).toBe(true);
  });

  it("returns false for plain indented content", () => {
    expect(looksLikeMarkdown("Item\n  Child\n    Grandchild")).toBe(false);
  });

  it("returns false for simple text", () => {
    expect(looksLikeMarkdown("Just some text")).toBe(false);
  });
});
