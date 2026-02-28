/**
 * Due date parsing from Workflowy node text.
 * Supports three patterns (priority order):
 * 1. due:YYYY-MM-DD (field-colon)
 * 2. #due-YYYY-MM-DD (hashtag)
 * 3. Bare YYYY-MM-DD (lowest priority, only if unambiguous)
 */

import type { WorkflowyNode } from "../types/index.js";

export interface DueDateInfo {
  date: Date;
  rawMatch: string;
}

// Priority-ordered patterns
const DUE_COLON_REGEX = /due:(\d{4}-\d{2}-\d{2})/i;
const DUE_HASHTAG_REGEX = /#due-(\d{4}-\d{2}-\d{2})/i;
const BARE_DATE_REGEX = /(?<!\w)(\d{4}-\d{2}-\d{2})(?!\w)/;

/**
 * Validate that a date string represents a real date.
 * Constructs Date and checks components round-trip.
 */
function isValidDate(dateStr: string): Date | null {
  const parts = dateStr.split("-");
  const y = parseInt(parts[0], 10);
  const m = parseInt(parts[1], 10);
  const d = parseInt(parts[2], 10);

  const date = new Date(y, m - 1, d);
  if (
    date.getFullYear() === y &&
    date.getMonth() === m - 1 &&
    date.getDate() === d
  ) {
    return date;
  }
  return null;
}

/**
 * Try to extract a due date from a single text string.
 */
function parseDueDateFromText(text: string): DueDateInfo | null {
  if (!text) return null;

  // Priority 1: due:YYYY-MM-DD
  let match = DUE_COLON_REGEX.exec(text);
  if (match) {
    const date = isValidDate(match[1]);
    if (date) return { date, rawMatch: match[0] };
  }

  // Priority 2: #due-YYYY-MM-DD
  match = DUE_HASHTAG_REGEX.exec(text);
  if (match) {
    const date = isValidDate(match[1]);
    if (date) return { date, rawMatch: match[0] };
  }

  // Priority 3: bare YYYY-MM-DD
  match = BARE_DATE_REGEX.exec(text);
  if (match) {
    const date = isValidDate(match[1]);
    if (date) return { date, rawMatch: match[0] };
  }

  return null;
}

/**
 * Parse due date from a Workflowy node (checks name first, then note).
 */
export function parseDueDateFromNode(node: WorkflowyNode): DueDateInfo | null {
  return parseDueDateFromText(node.name || "") || parseDueDateFromText(node.note || "");
}

/**
 * Check if a node is overdue (has a due date in the past and is not completed).
 */
export function isOverdue(node: WorkflowyNode, now?: Date): boolean {
  if (node.completedAt) return false;
  const dueDate = parseDueDateFromNode(node);
  if (!dueDate) return false;

  const reference = now || new Date();
  const refDay = new Date(reference.getFullYear(), reference.getMonth(), reference.getDate());
  return dueDate.date < refDay;
}

/**
 * Check if a node is due within the next N days (inclusive).
 */
export function isDueWithin(node: WorkflowyNode, days: number, now?: Date): boolean {
  const dueDate = parseDueDateFromNode(node);
  if (!dueDate) return false;

  const reference = now || new Date();
  const refDay = new Date(reference.getFullYear(), reference.getMonth(), reference.getDate());
  const cutoff = new Date(refDay);
  cutoff.setDate(cutoff.getDate() + days);

  return dueDate.date >= refDay && dueDate.date <= cutoff;
}
