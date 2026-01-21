import { describe, it, expect } from "vitest";
import {
  convertLargeMarkdownToWorkflowy,
  analyzeMarkdown,
} from "./large-markdown-converter.js";

describe("convertLargeMarkdownToWorkflowy", () => {
  describe("headers", () => {
    it("should convert ATX headers to indented format", () => {
      const markdown = `# Title
## Section
### Subsection`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toBe(`Title
  Section
    Subsection`);
      expect(result.stats.headers).toBe(3);
    });

    it("should handle setext-style headers", () => {
      const markdown = `Title
=====
Section
-------`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toBe(`Title
  Section`);
      expect(result.stats.headers).toBe(2);
    });
  });

  describe("lists", () => {
    it("should convert unordered lists", () => {
      const markdown = `# Items
- First item
- Second item
  - Nested item`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toBe(`Items
  First item
  Second item
    Nested item`);
      expect(result.stats.listItems).toBe(3);
    });

    it("should convert ordered lists", () => {
      const markdown = `# Steps
1. First step
2. Second step
3. Third step`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toBe(`Steps
  First step
  Second step
  Third step`);
      expect(result.stats.listItems).toBe(3);
    });

    it("should convert task lists with checkboxes", () => {
      const markdown = `# Tasks
- [x] Done task
- [ ] Pending task`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toBe(`Tasks
  [x] Done task
  [ ] Pending task`);
      expect(result.stats.taskItems).toBe(2);
    });

    it("should strip task checkboxes when option disabled", () => {
      const markdown = `# Tasks
- [x] Done task`;

      const result = convertLargeMarkdownToWorkflowy(markdown, {
        preserveTaskLists: false,
      });

      expect(result.content).toBe(`Tasks
  Done task`);
    });
  });

  describe("code blocks", () => {
    it("should convert fenced code blocks", () => {
      const markdown = `# Code
\`\`\`javascript
const x = 1;
console.log(x);
\`\`\``;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toContain("[Code: javascript]");
      expect(result.content).toContain("const x = 1;");
      expect(result.stats.codeBlocks).toBe(1);
    });

    it("should handle code blocks without language", () => {
      const markdown = `\`\`\`
plain code
\`\`\``;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toContain("[Code]");
      expect(result.content).toContain("plain code");
    });
  });

  describe("blockquotes", () => {
    it("should convert blockquotes", () => {
      const markdown = `# Quote
> This is a quote
> Another line`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toContain("> This is a quote");
      expect(result.stats.blockquotes).toBe(2);
    });

    it("should handle nested blockquotes", () => {
      const markdown = `> Level 1
>> Level 2`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toContain("> Level 1");
      expect(result.content).toContain("> Level 2");
    });
  });

  describe("tables", () => {
    it("should convert markdown tables to hierarchical lists", () => {
      const markdown = `| Name | Age |
|------|-----|
| Alice | 30 |
| Bob | 25 |`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toContain("[Table]");
      expect(result.content).toContain("[Header]");
      expect(result.content).toContain("Name");
      expect(result.content).toContain("[Row 1]");
      expect(result.content).toContain("Name: Alice");
      expect(result.stats.tables).toBe(1);
    });
  });

  describe("horizontal rules", () => {
    it("should include horizontal rules as separators", () => {
      const markdown = `# Section 1
---
# Section 2`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toContain("---");
    });

    it("should skip horizontal rules when option disabled", () => {
      const markdown = `# Section 1
---
# Section 2`;

      const result = convertLargeMarkdownToWorkflowy(markdown, {
        includeHorizontalRules: false,
      });

      expect(result.content).not.toContain("---");
    });
  });

  describe("inline formatting", () => {
    it("should preserve inline formatting by default", () => {
      const markdown = `# Text
**bold** and *italic* text`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toContain("**bold** and *italic* text");
    });

    it("should strip inline formatting when option disabled", () => {
      const markdown = `# Text
**bold** and *italic* text`;

      const result = convertLargeMarkdownToWorkflowy(markdown, {
        preserveInlineFormatting: false,
      });

      expect(result.content).toContain("bold and italic text");
    });

    it("should convert images to text references", () => {
      const markdown = `![alt text](image.png)`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.content).toContain("[Image: alt text]");
    });
  });

  describe("complex documents", () => {
    it("should handle a complex markdown document", () => {
      const markdown = `# Project README

## Overview
This is a sample project.

## Features
- Feature 1
  - Sub-feature A
  - Sub-feature B
- Feature 2

## Code Example
\`\`\`typescript
function hello() {
  console.log("Hello!");
}
\`\`\`

## Tasks
- [x] Setup
- [ ] Implementation
- [ ] Testing

## License
MIT`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.stats.headers).toBe(6);
      expect(result.stats.listItems).toBeGreaterThan(5);
      expect(result.stats.codeBlocks).toBe(1);
      expect(result.stats.taskItems).toBe(3);
      expect(result.nodeCount).toBeGreaterThan(15);
    });
  });

  describe("options", () => {
    it("should respect maxDepth option", () => {
      const markdown = `# Level 1
## Level 2
### Level 3
#### Level 4
##### Level 5`;

      const result = convertLargeMarkdownToWorkflowy(markdown, {
        maxDepth: 3,
      });

      // All headers beyond maxDepth should be clamped
      const lines = result.content.split("\n");
      const maxIndent = Math.max(
        ...lines.map((line) => {
          const match = line.match(/^(\s*)/);
          return match ? match[1].length : 0;
        })
      );
      // Max indent should be (maxDepth - 1) * 2 = 4 spaces
      expect(maxIndent).toBeLessThanOrEqual(4);
    });
  });

  describe("warnings", () => {
    it("should warn when no headers found", () => {
      const markdown = `Just some text
without any headers`;

      const result = convertLargeMarkdownToWorkflowy(markdown);

      expect(result.warnings).toContain(
        "No headers found - content will be inserted at root level"
      );
    });

    it("should warn on empty input", () => {
      const result = convertLargeMarkdownToWorkflowy("");

      expect(result.warnings.length).toBeGreaterThan(0);
    });
  });
});

describe("analyzeMarkdown", () => {
  it("should analyze markdown and return statistics", () => {
    const markdown = `# Title
## Section
- Item 1
- Item 2
\`\`\`
code
\`\`\`
> Quote`;

    const stats = analyzeMarkdown(markdown);

    expect(stats.headers).toBe(2);
    expect(stats.listItems).toBe(2);
    expect(stats.codeBlocks).toBe(1);
    expect(stats.blockquotes).toBe(1);
    expect(stats.estimatedNodes).toBeGreaterThan(0);
  });

  it("should detect task items", () => {
    const markdown = `- [x] Done
- [ ] Todo`;

    const stats = analyzeMarkdown(markdown);

    expect(stats.taskItems).toBe(2);
    expect(stats.listItems).toBe(2);
  });

  it("should count tables", () => {
    const markdown = `| A | B |
|---|---|
| 1 | 2 |`;

    const stats = analyzeMarkdown(markdown);

    expect(stats.tables).toBe(1);
  });
});
