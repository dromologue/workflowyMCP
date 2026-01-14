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
 * Handles Unicode characters (accents, umlauts) safely
 * Does NOT truncate - callers should handle length if needed
 */
export function escapeForDot(str: string): string {
  return str
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\r/g, "")
    .replace(/[<>{}|]/g, ""); // Remove DOT special chars that could break parsing
  // Note: actual newlines (\n) are kept as-is - Graphviz handles them in labels
}

/**
 * Generate Workflowy internal link
 */
export function generateWorkflowyLink(nodeId: string, nodeName: string): string {
  const cleanName = (nodeName || "Untitled").substring(0, 50);
  return `[${cleanName}](https://workflowy.com/#/${nodeId})`;
}

/**
 * Extract Workflowy node IDs from internal links in text
 * Matches markdown links like [text](https://workflowy.com/#/node-id)
 * Also matches plain Workflowy URLs
 */
export function extractWorkflowyLinks(text: string): string[] {
  const nodeIds: string[] = [];

  // Match markdown links: [text](https://workflowy.com/#/node-id)
  // Node IDs can contain lowercase hex chars, numbers, and dashes
  const markdownRegex = /\[([^\]]*)\]\(https:\/\/workflowy\.com\/#\/([a-z0-9-]+)\)/gi;
  let match;
  while ((match = markdownRegex.exec(text)) !== null) {
    const nodeId = match[2];
    if (nodeId && !nodeIds.includes(nodeId)) {
      nodeIds.push(nodeId);
    }
  }

  // Also match plain URLs: https://workflowy.com/#/node-id
  // Use negative lookbehind to avoid matching URLs already captured by markdown pattern
  // Match URL not preceded by '(' (which would be part of markdown link)
  const plainUrlRegex = /(?<!\()https:\/\/workflowy\.com\/#\/([a-z0-9-]+)/gi;
  while ((match = plainUrlRegex.exec(text)) !== null) {
    const nodeId = match[1];
    if (nodeId && !nodeIds.includes(nodeId)) {
      nodeIds.push(nodeId);
    }
  }

  return nodeIds;
}

/**
 * Concept map configuration constants
 */
export const CONCEPT_MAP_LIMITS = {
  MIN_CONCEPTS: 2,
  MAX_CONCEPTS: 35,
  MAX_LABEL_LENGTH: 40,
  IMAGE_SIZE: 2000,  // Square dimensions (2000x2000)
} as const;

/**
 * Validate concept map input parameters
 * Returns null if valid, or an error object if invalid
 */
export function validateConceptMapInput(concepts: string[] | undefined): {
  valid: false;
  error: string;
  tip: string;
  provided?: number;
  maximum?: number;
} | { valid: true } {
  if (!concepts || concepts.length < CONCEPT_MAP_LIMITS.MIN_CONCEPTS) {
    return {
      valid: false,
      error: `Please provide at least ${CONCEPT_MAP_LIMITS.MIN_CONCEPTS} concepts. Concepts become the nodes in the map, connected based on relationships found in your content.`,
      tip: "Example: concepts: ['phenomenology', 'pragmatism', 'experience', 'being'] - the core concept will be at the center, with others arranged hierarchically.",
    };
  }

  if (concepts.length > CONCEPT_MAP_LIMITS.MAX_CONCEPTS) {
    return {
      valid: false,
      error: `Too many concepts: ${concepts.length} provided, maximum is ${CONCEPT_MAP_LIMITS.MAX_CONCEPTS}. Large graphs become unreadable and may fail to render.`,
      tip: "Split into multiple focused concept maps, or select the most important concepts. Consider grouping related concepts under broader themes.",
      provided: concepts.length,
      maximum: CONCEPT_MAP_LIMITS.MAX_CONCEPTS,
    };
  }

  return { valid: true };
}
