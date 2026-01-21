/**
 * Large Markdown to Workflowy Converter
 *
 * Converts large markdown documents to 2-space indented format suitable for Workflowy.
 * Handles complex markdown structures including:
 * - Nested headers (H1-H6)
 * - Nested lists (ordered/unordered)
 * - Task lists with checkboxes
 * - Code blocks (fenced and indented)
 * - Blockquotes (including nested)
 * - Tables (converted to hierarchical format)
 * - Horizontal rules (as separators)
 * - Links and images (preserved inline)
 * - Bold, italic, and other inline formatting
 */

export interface LargeMarkdownConversionResult {
  /** Converted content in 2-space indented format */
  content: string;
  /** Estimated number of nodes that will be created */
  nodeCount: number;
  /** Statistics about the conversion */
  stats: ConversionStats;
  /** Any warnings generated during conversion */
  warnings: string[];
}

export interface ConversionStats {
  /** Number of headers found */
  headers: number;
  /** Number of list items found */
  listItems: number;
  /** Number of code blocks found */
  codeBlocks: number;
  /** Number of tables found */
  tables: number;
  /** Number of blockquotes found */
  blockquotes: number;
  /** Number of task items found */
  taskItems: number;
  /** Number of paragraphs found */
  paragraphs: number;
  /** Original line count */
  originalLines: number;
  /** Output line count */
  outputLines: number;
}

export interface ConversionOptions {
  /** Preserve inline formatting like **bold** and *italic* (default: true) */
  preserveInlineFormatting?: boolean;
  /** Convert tables to hierarchical lists (default: true) */
  convertTables?: boolean;
  /** Include horizontal rules as separator nodes (default: true) */
  includeHorizontalRules?: boolean;
  /** Maximum depth for nested structures (default: 10) */
  maxDepth?: number;
  /** Collapse consecutive empty lines (default: true) */
  collapseEmptyLines?: boolean;
  /** Preserve task list checkboxes as [x] or [ ] (default: true) */
  preserveTaskLists?: boolean;
}

interface ConversionState {
  lines: string[];
  currentHeaderLevel: number;
  currentListBaseIndent: number;
  inCodeBlock: boolean;
  codeBlockContent: string[];
  codeBlockIndent: number;
  codeBlockLang: string;
  inBlockquote: boolean;
  blockquoteLevel: number;
  inTable: boolean;
  tableHeaders: string[];
  tableRows: string[][];
  stats: ConversionStats;
  warnings: string[];
  options: Required<ConversionOptions>;
  skipNextLine: boolean; // Used to skip setext underlines
}

const DEFAULT_OPTIONS: Required<ConversionOptions> = {
  preserveInlineFormatting: true,
  convertTables: true,
  includeHorizontalRules: true,
  maxDepth: 10,
  collapseEmptyLines: true,
  preserveTaskLists: true,
};

/**
 * Convert a large markdown document to Workflowy-compatible indented format
 */
export function convertLargeMarkdownToWorkflowy(
  markdown: string,
  options: ConversionOptions = {}
): LargeMarkdownConversionResult {
  const mergedOptions: Required<ConversionOptions> = { ...DEFAULT_OPTIONS, ...options };

  const state: ConversionState = {
    lines: [],
    currentHeaderLevel: 0,
    currentListBaseIndent: 1,
    inCodeBlock: false,
    codeBlockContent: [],
    codeBlockIndent: 0,
    codeBlockLang: "",
    inBlockquote: false,
    blockquoteLevel: 0,
    inTable: false,
    tableHeaders: [],
    tableRows: [],
    stats: {
      headers: 0,
      listItems: 0,
      codeBlocks: 0,
      tables: 0,
      blockquotes: 0,
      taskItems: 0,
      paragraphs: 0,
      originalLines: 0,
      outputLines: 0,
    },
    warnings: [],
    options: mergedOptions,
    skipNextLine: false,
  };

  const inputLines = markdown.split("\n");
  state.stats.originalLines = inputLines.length;

  for (let i = 0; i < inputLines.length; i++) {
    const line = inputLines[i];
    const nextLine = inputLines[i + 1];
    processLine(state, line, nextLine, i);
  }

  // Flush any pending content
  flushCodeBlock(state);
  flushTable(state);

  // Final warnings
  if (state.lines.length === 0) {
    state.warnings.push("No content was converted - input may be empty or unrecognized format");
  }

  if (state.stats.headers === 0 && state.lines.length > 0) {
    state.warnings.push("No headers found - content will be inserted at root level");
  }

  const content = state.lines.join("\n");
  state.stats.outputLines = state.lines.length;

  return {
    content,
    nodeCount: state.lines.length,
    stats: state.stats,
    warnings: state.warnings,
  };
}

function processLine(
  state: ConversionState,
  line: string,
  nextLine: string | undefined,
  lineIndex: number
): void {
  // Handle code blocks (fenced)
  if (line.trim().startsWith("```")) {
    if (!state.inCodeBlock) {
      state.inCodeBlock = true;
      state.codeBlockContent = [];
      state.codeBlockIndent = state.currentHeaderLevel + 1;
      state.codeBlockLang = line.trim().slice(3).trim();
    } else {
      flushCodeBlock(state);
    }
    return;
  }

  if (state.inCodeBlock) {
    state.codeBlockContent.push(line);
    return;
  }

  // Handle tables
  if (isTableLine(line)) {
    if (!state.inTable) {
      state.inTable = true;
      state.tableHeaders = parseTableRow(line);
    } else if (isTableSeparator(line)) {
      // Skip separator line
    } else {
      state.tableRows.push(parseTableRow(line));
    }

    // Check if table ends
    if (nextLine === undefined || (!isTableLine(nextLine) && nextLine.trim() !== "")) {
      flushTable(state);
    }
    return;
  } else if (state.inTable) {
    flushTable(state);
  }

  // Skip empty lines (but track for paragraph separation)
  if (!line.trim()) {
    return;
  }

  // If we just processed a setext header, skip this line (the underline)
  if (state.skipNextLine) {
    state.skipNextLine = false;
    return;
  }

  // Check for setext-style headers (underlined with === or ---)
  // Must check BEFORE horizontal rules since --- is both
  if (nextLine && /^=+\s*$/.test(nextLine)) {
    processHeader(state, line.trim(), 1);
    state.skipNextLine = true;
    return;
  }
  if (nextLine && /^-+\s*$/.test(nextLine) && line.trim() && !line.startsWith("-") && !line.startsWith("#")) {
    processHeader(state, line.trim(), 2);
    state.skipNextLine = true;
    return;
  }

  // Check for horizontal rules (---, ***, or ___)
  if (/^(\*{3,}|-{3,}|_{3,})\s*$/.test(line.trim())) {
    if (state.options.includeHorizontalRules) {
      const indent = "  ".repeat(state.currentHeaderLevel + 1);
      state.lines.push(`${indent}---`);
    }
    return;
  }

  // Check for ATX headers
  const headerMatch = line.match(/^(#{1,6})\s+(.+?)(?:\s*#*\s*)?$/);
  if (headerMatch) {
    const level = headerMatch[1].length;
    const text = headerMatch[2].trim();
    processHeader(state, text, level);
    return;
  }

  // Check for blockquotes
  const blockquoteMatch = line.match(/^(>+)\s*(.*)$/);
  if (blockquoteMatch) {
    const level = blockquoteMatch[1].length;
    const text = blockquoteMatch[2];
    processBlockquote(state, text, level);
    return;
  }

  // Check for task list items
  const taskMatch = line.match(/^(\s*)([-*+])\s+\[([ xX])\]\s+(.+)$/);
  if (taskMatch) {
    const leadingSpaces = taskMatch[1].length;
    const checked = taskMatch[3].toLowerCase() === "x";
    const text = taskMatch[4].trim();
    processTaskItem(state, text, leadingSpaces, checked);
    return;
  }

  // Check for list items (unordered)
  const unorderedMatch = line.match(/^(\s*)([-*+])\s+(.+)$/);
  if (unorderedMatch) {
    const leadingSpaces = unorderedMatch[1].length;
    const text = unorderedMatch[3].trim();
    processListItem(state, text, leadingSpaces);
    return;
  }

  // Check for list items (ordered)
  const orderedMatch = line.match(/^(\s*)(\d+)\.\s+(.+)$/);
  if (orderedMatch) {
    const leadingSpaces = orderedMatch[1].length;
    const text = orderedMatch[3].trim();
    processListItem(state, text, leadingSpaces);
    return;
  }

  // Check for indented code block (4 spaces or 1 tab)
  if (/^(    |\t)/.test(line)) {
    const codeContent = line.replace(/^(    |\t)/, "");
    const indent = "  ".repeat(state.currentHeaderLevel + 1);
    state.lines.push(`${indent}${codeContent}`);
    return;
  }

  // Plain paragraph text
  const text = processInlineFormatting(state, line.trim());
  if (text) {
    const indent = "  ".repeat(state.currentHeaderLevel + 1);
    state.lines.push(`${indent}${text}`);
    state.stats.paragraphs++;
  }
}

function processHeader(state: ConversionState, text: string, level: number): void {
  // Clamp level to maxDepth
  const effectiveLevel = Math.min(level - 1, state.options.maxDepth - 1);
  state.currentHeaderLevel = effectiveLevel;
  state.currentListBaseIndent = effectiveLevel + 1;

  const processedText = processInlineFormatting(state, text);
  const indent = "  ".repeat(effectiveLevel);
  state.lines.push(`${indent}${processedText}`);
  state.stats.headers++;
}

function processListItem(state: ConversionState, text: string, leadingSpaces: number): void {
  const listIndentLevel = Math.floor(leadingSpaces / 2);
  const totalIndent = Math.min(
    state.currentListBaseIndent + listIndentLevel,
    state.options.maxDepth
  );

  const processedText = processInlineFormatting(state, text);
  const indent = "  ".repeat(totalIndent);
  state.lines.push(`${indent}${processedText}`);
  state.stats.listItems++;
}

function processTaskItem(
  state: ConversionState,
  text: string,
  leadingSpaces: number,
  checked: boolean
): void {
  const listIndentLevel = Math.floor(leadingSpaces / 2);
  const totalIndent = Math.min(
    state.currentListBaseIndent + listIndentLevel,
    state.options.maxDepth
  );

  const processedText = processInlineFormatting(state, text);
  const indent = "  ".repeat(totalIndent);

  if (state.options.preserveTaskLists) {
    const checkbox = checked ? "[x]" : "[ ]";
    state.lines.push(`${indent}${checkbox} ${processedText}`);
  } else {
    state.lines.push(`${indent}${processedText}`);
  }

  state.stats.taskItems++;
  state.stats.listItems++;
}

function processBlockquote(state: ConversionState, text: string, level: number): void {
  const totalIndent = Math.min(
    state.currentHeaderLevel + level,
    state.options.maxDepth
  );

  if (text.trim()) {
    const processedText = processInlineFormatting(state, text.trim());
    const indent = "  ".repeat(totalIndent);
    state.lines.push(`${indent}> ${processedText}`);
    state.stats.blockquotes++;
  }
}

function processInlineFormatting(state: ConversionState, text: string): string {
  if (!state.options.preserveInlineFormatting) {
    // Strip all inline formatting
    return text
      .replace(/\*\*(.+?)\*\*/g, "$1")
      .replace(/\*(.+?)\*/g, "$1")
      .replace(/__(.+?)__/g, "$1")
      .replace(/_(.+?)_/g, "$1")
      .replace(/~~(.+?)~~/g, "$1")
      .replace(/`(.+?)`/g, "$1")
      .replace(/\[(.+?)\]\((.+?)\)/g, "$1")
      .replace(/!\[(.+?)\]\((.+?)\)/g, "[Image: $1]");
  }

  // Preserve most formatting, but convert images to text references
  return text.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, "[Image: $1]");
}

function flushCodeBlock(state: ConversionState): void {
  if (!state.inCodeBlock) return;

  state.inCodeBlock = false;

  if (state.codeBlockContent.length > 0) {
    const indent = "  ".repeat(state.codeBlockIndent);

    // Create a header for the code block
    const langLabel = state.codeBlockLang ? `[Code: ${state.codeBlockLang}]` : "[Code]";
    state.lines.push(`${indent}${langLabel}`);

    // Add code content as children, preserving structure
    const codeIndent = "  ".repeat(state.codeBlockIndent + 1);
    for (const codeLine of state.codeBlockContent) {
      // Preserve indentation within code by using a marker
      const preservedLine = codeLine || " "; // Empty lines become single space
      state.lines.push(`${codeIndent}${preservedLine}`);
    }

    state.stats.codeBlocks++;
  }

  state.codeBlockContent = [];
  state.codeBlockLang = "";
}

function isTableLine(line: string): boolean {
  const trimmed = line.trim();
  return trimmed.startsWith("|") && trimmed.endsWith("|");
}

function isTableSeparator(line: string): boolean {
  return /^\|[\s\-:|]+\|$/.test(line.trim());
}

function parseTableRow(line: string): string[] {
  return line
    .trim()
    .slice(1, -1) // Remove leading and trailing |
    .split("|")
    .map((cell) => cell.trim());
}

function flushTable(state: ConversionState): void {
  if (!state.inTable) return;

  state.inTable = false;

  if (state.tableHeaders.length > 0 && state.options.convertTables) {
    const indent = "  ".repeat(state.currentHeaderLevel + 1);
    const rowIndent = "  ".repeat(state.currentHeaderLevel + 2);
    const cellIndent = "  ".repeat(state.currentHeaderLevel + 3);

    // Create table node
    state.lines.push(`${indent}[Table]`);

    // Add header row
    state.lines.push(`${rowIndent}[Header]`);
    for (const header of state.tableHeaders) {
      if (header.trim()) {
        state.lines.push(`${cellIndent}${header}`);
      }
    }

    // Add data rows
    for (let i = 0; i < state.tableRows.length; i++) {
      const row = state.tableRows[i];
      state.lines.push(`${rowIndent}[Row ${i + 1}]`);

      for (let j = 0; j < row.length; j++) {
        const cell = row[j];
        const header = state.tableHeaders[j] || `Col ${j + 1}`;
        if (cell.trim()) {
          state.lines.push(`${cellIndent}${header}: ${cell}`);
        }
      }
    }

    state.stats.tables++;
  }

  state.tableHeaders = [];
  state.tableRows = [];
}

/**
 * Analyze markdown content and return statistics without converting
 */
export function analyzeMarkdown(markdown: string): ConversionStats & { estimatedNodes: number } {
  const lines = markdown.split("\n");
  const stats: ConversionStats = {
    headers: 0,
    listItems: 0,
    codeBlocks: 0,
    tables: 0,
    blockquotes: 0,
    taskItems: 0,
    paragraphs: 0,
    originalLines: lines.length,
    outputLines: 0,
  };

  let inCodeBlock = false;
  let inTable = false;
  let tableRowCount = 0;

  for (const line of lines) {
    const trimmed = line.trim();

    // Code blocks
    if (trimmed.startsWith("```")) {
      if (!inCodeBlock) {
        inCodeBlock = true;
        stats.codeBlocks++;
      } else {
        inCodeBlock = false;
      }
      continue;
    }

    if (inCodeBlock) continue;

    // Tables
    if (isTableLine(line)) {
      if (!inTable) {
        inTable = true;
        stats.tables++;
      }
      if (!isTableSeparator(line)) {
        tableRowCount++;
      }
      continue;
    } else if (inTable && trimmed) {
      inTable = false;
      tableRowCount = 0;
    }

    // Skip empty lines
    if (!trimmed) continue;

    // Headers
    if (/^#{1,6}\s+/.test(line) || /^=+$/.test(trimmed) || /^-+$/.test(trimmed)) {
      stats.headers++;
      continue;
    }

    // Blockquotes
    if (/^>+/.test(trimmed)) {
      stats.blockquotes++;
      continue;
    }

    // Task items
    if (/^(\s*)([-*+])\s+\[([ xX])\]/.test(line)) {
      stats.taskItems++;
      stats.listItems++;
      continue;
    }

    // List items
    if (/^(\s*)([-*+]|\d+\.)\s+/.test(line)) {
      stats.listItems++;
      continue;
    }

    // Paragraphs
    stats.paragraphs++;
  }

  // Estimate output nodes
  const estimatedNodes =
    stats.headers +
    stats.listItems +
    stats.codeBlocks * 2 + // Code block header + content
    stats.tables * 3 + // Table + header row + data rows
    stats.blockquotes +
    stats.paragraphs;

  return { ...stats, estimatedNodes };
}
