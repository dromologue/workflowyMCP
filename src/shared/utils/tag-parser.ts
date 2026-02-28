/**
 * Tag and assignee extraction from Workflowy node text.
 * Parses #tags and @mentions from node name and note fields.
 */

import type { WorkflowyNode } from "../types/index.js";

export interface ParsedTags {
  tags: string[];
  assignees: string[];
}

const TAG_REGEX = /#([\w-]+)/g;
const ASSIGNEE_REGEX = /@([\w-]+)/g;

/**
 * Extract #tags and @mentions from text.
 * Returns lowercase, deduplicated arrays.
 */
export function parseTags(text: string): ParsedTags {
  if (!text) return { tags: [], assignees: [] };

  const tags = new Set<string>();
  const assignees = new Set<string>();

  let match;
  const tagRegex = new RegExp(TAG_REGEX.source, TAG_REGEX.flags);
  while ((match = tagRegex.exec(text)) !== null) {
    tags.add(match[1].toLowerCase());
  }

  const assigneeRegex = new RegExp(ASSIGNEE_REGEX.source, ASSIGNEE_REGEX.flags);
  while ((match = assigneeRegex.exec(text)) !== null) {
    assignees.add(match[1].toLowerCase());
  }

  return { tags: [...tags], assignees: [...assignees] };
}

/**
 * Parse tags from both name and note of a node, merged and deduplicated.
 */
export function parseNodeTags(node: WorkflowyNode): ParsedTags {
  const nameTags = parseTags(node.name || "");
  const noteTags = parseTags(node.note || "");

  const tags = new Set([...nameTags.tags, ...noteTags.tags]);
  const assignees = new Set([...nameTags.assignees, ...noteTags.assignees]);

  return { tags: [...tags], assignees: [...assignees] };
}

/**
 * Check if a node has a specific tag in its name or note.
 * Accepts tag with or without leading #.
 */
export function nodeHasTag(node: WorkflowyNode, tag: string): boolean {
  const normalizedTag = tag.replace(/^#/, "").toLowerCase();
  const { tags } = parseNodeTags(node);
  return tags.includes(normalizedTag);
}

/**
 * Check if a node has a specific assignee in its name or note.
 * Accepts assignee with or without leading @.
 */
export function nodeHasAssignee(node: WorkflowyNode, assignee: string): boolean {
  const normalizedAssignee = assignee.replace(/^@/, "").toLowerCase();
  const { assignees } = parseNodeTags(node);
  return assignees.includes(normalizedAssignee);
}
