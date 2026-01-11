/**
 * Text processing utilities for parsing and formatting
 */

import type { ParsedLine, NodeWithPath } from "../types/index.js";

/**
 * Parse indented content into hierarchical structure
 * Supports both spaces (2 per level) and tabs
 */
export function parseIndentedContent(content: string): ParsedLine[] {
  const lines = content.split("\n");
  const parsed: ParsedLine[] = [];

  for (const line of lines) {
    // Skip empty lines
    if (!line.trim()) continue;

    // Count leading whitespace (tabs = 2 spaces equivalent)
    const match = line.match(/^(\s*)/);
    const whitespace = match ? match[1] : "";
    // Convert tabs to 2-space equivalents for consistent indent calculation
    const normalizedWhitespace = whitespace.replace(/\t/g, "  ");
    const indent = Math.floor(normalizedWhitespace.length / 2);

    parsed.push({
      text: line.trim(),
      indent,
    });
  }

  return parsed;
}

/**
 * Format nodes for display with numbered options
 */
export function formatNodesForSelection(nodes: NodeWithPath[]): string {
  if (nodes.length === 0) {
    return "No matching nodes found.";
  }

  const lines = nodes.map((node, index) => {
    const note = node.note ? ` [note: ${node.note.substring(0, 50)}...]` : "";
    return `[${index + 1}] ${node.path}${note}\n    ID: ${node.id}`;
  });

  return `Found ${nodes.length} matching node(s):\n\n${lines.join("\n\n")}`;
}

/**
 * Escape special characters for Graphviz DOT format
 */
export function escapeForDot(str: string): string {
  return str
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n")
    .substring(0, 40); // Truncate for readability
}

/**
 * Generate Workflowy internal link
 */
export function generateWorkflowyLink(nodeId: string, nodeName: string): string {
  const cleanName = (nodeName || "Untitled").substring(0, 50);
  return `[${cleanName}](https://workflowy.com/#/${nodeId})`;
}
