/**
 * Markdown to indented hierarchy converter
 * Converts markdown documents to 2-space indented format for Workflowy insertion
 */

export interface MarkdownConversionResult {
  /** Converted content in 2-space indented format */
  content: string;
  /** Estimated number of nodes that will be created */
  nodeCount: number;
  /** Any warnings generated during conversion */
  warnings?: string[];
}

interface ConversionState {
  lines: string[];
  currentHeaderLevel: number;
  currentListBaseIndent: number;
  warnings: string[];
}

/**
 * Convert markdown to 2-space indented hierarchical format
 *
 * Conversion rules:
 * - # H1 → indent level 0
 * - ## H2 → indent level 1
 * - ### H3 → indent level 2 (and so on for H4-H6)
 * - - list item → indent = current header level + 1
 * - Nested lists (  - item) → additional indent per 2 spaces
 * - Plain paragraphs → same level as current header + 1
 * - Blank lines → ignored
 * - Code blocks → preserved as single node with content
 */
export function convertMarkdownToIndented(markdown: string): MarkdownConversionResult {
  const state: ConversionState = {
    lines: [],
    currentHeaderLevel: 0,
    currentListBaseIndent: 1,
    warnings: [],
  };

  const inputLines = markdown.split("\n");
  let inCodeBlock = false;
  let codeBlockContent: string[] = [];
  let codeBlockIndent = 0;

  for (let i = 0; i < inputLines.length; i++) {
    const line = inputLines[i];

    // Handle code blocks
    if (line.trim().startsWith("```")) {
      if (!inCodeBlock) {
        // Starting code block
        inCodeBlock = true;
        codeBlockContent = [];
        codeBlockIndent = state.currentHeaderLevel + 1;
        // Extract language if specified
        const lang = line.trim().slice(3).trim();
        if (lang) {
          codeBlockContent.push(`[${lang}]`);
        }
      } else {
        // Ending code block - emit as single node
        inCodeBlock = false;
        if (codeBlockContent.length > 0) {
          const indent = "  ".repeat(codeBlockIndent);
          // Join code block content, preserving internal structure
          const codeText = codeBlockContent.join("\\n");
          state.lines.push(`${indent}${codeText}`);
        }
        codeBlockContent = [];
      }
      continue;
    }

    if (inCodeBlock) {
      codeBlockContent.push(line);
      continue;
    }

    // Skip empty lines
    if (!line.trim()) {
      continue;
    }

    // Check for headers
    const headerMatch = line.match(/^(#{1,6})\s+(.+)$/);
    if (headerMatch) {
      const level = headerMatch[1].length - 1; // # = 0, ## = 1, etc.
      const text = headerMatch[2].trim();
      state.currentHeaderLevel = level;
      state.currentListBaseIndent = level + 1;

      const indent = "  ".repeat(level);
      state.lines.push(`${indent}${text}`);
      continue;
    }

    // Check for list items
    const listMatch = line.match(/^(\s*)([-*+]|\d+\.)\s+(.+)$/);
    if (listMatch) {
      const leadingSpaces = listMatch[1].length;
      const listIndentLevel = Math.floor(leadingSpaces / 2);
      const text = listMatch[3].trim();

      // List indent = header level + 1 + any additional nesting
      const totalIndent = state.currentListBaseIndent + listIndentLevel;
      const indent = "  ".repeat(totalIndent);
      state.lines.push(`${indent}${text}`);
      continue;
    }

    // Check for blockquotes
    const blockquoteMatch = line.match(/^>\s*(.*)$/);
    if (blockquoteMatch) {
      const text = blockquoteMatch[1].trim();
      if (text) {
        const indent = "  ".repeat(state.currentHeaderLevel + 1);
        state.lines.push(`${indent}> ${text}`);
      }
      continue;
    }

    // Plain paragraph text - treat as child of current header
    const text = line.trim();
    if (text) {
      const indent = "  ".repeat(state.currentHeaderLevel + 1);
      state.lines.push(`${indent}${text}`);
    }
  }

  // Handle unclosed code block
  if (inCodeBlock && codeBlockContent.length > 0) {
    state.warnings.push("Unclosed code block detected - content was included");
    const indent = "  ".repeat(codeBlockIndent);
    const codeText = codeBlockContent.join("\\n");
    state.lines.push(`${indent}${codeText}`);
  }

  // If no headers were found, warn
  if (state.lines.length > 0 && !markdown.match(/^#{1,6}\s/m)) {
    state.warnings.push(
      "No markdown headers found - content will be inserted at root level"
    );
  }

  const content = state.lines.join("\n");

  return {
    content,
    nodeCount: state.lines.length,
    warnings: state.warnings.length > 0 ? state.warnings : undefined,
  };
}

/**
 * Detect if content appears to be markdown format
 * Used to auto-detect format when not explicitly specified
 */
export function looksLikeMarkdown(content: string): boolean {
  // Strong indicators - single match is enough
  const strongPatterns = [
    /^#{1,6}\s+.+/m, // Headers (# followed by text)
    /^```/m, // Code blocks
  ];

  for (const pattern of strongPatterns) {
    if (pattern.test(content)) {
      return true;
    }
  }

  // Weak indicators - need 2+ matches
  const weakPatterns = [
    /^\s*[-*+]\s+/m, // Unordered lists
    /^\s*\d+\.\s+/m, // Ordered lists
    /^\s*>/m, // Blockquotes
    /\[.+\]\(.+\)/, // Links
    /\*\*.+\*\*/, // Bold
    /_.+_/, // Italic
  ];

  const weakMatches = weakPatterns.filter((pattern) =>
    pattern.test(content)
  ).length;

  // If 2+ weak patterns found, likely markdown
  return weakMatches >= 2;
}
