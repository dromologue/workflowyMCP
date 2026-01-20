/**
 * Subtree Parser for Multi-Agent Orchestration
 *
 * Splits hierarchical content into independent subtrees that can be
 * processed by separate agents in parallel. Each subtree is self-contained
 * and can be inserted independently.
 */

import type { ParsedLine } from "../types/index.js";
import { parseIndentedContent } from "./text-processing.js";

/**
 * A subtree that can be processed independently
 */
export interface Subtree {
  /** Unique identifier for this subtree */
  id: string;
  /** The root line of this subtree (indent level 0 relative to subtree) */
  rootLine: ParsedLine;
  /** All lines in this subtree including the root */
  lines: ParsedLine[];
  /** Original content string for this subtree */
  content: string;
  /** Number of nodes in this subtree */
  nodeCount: number;
  /** Estimated processing time in ms (based on rate limits) */
  estimatedMs: number;
}

/**
 * Result of splitting content into subtrees
 */
export interface SubtreeSplitResult {
  /** The independent subtrees */
  subtrees: Subtree[];
  /** Total number of nodes across all subtrees */
  totalNodes: number;
  /** Recommended number of agents based on workload */
  recommendedAgents: number;
  /** Estimated total time with single agent (ms) */
  singleAgentEstimateMs: number;
  /** Estimated total time with recommended agents (ms) */
  parallelEstimateMs: number;
}

/**
 * Configuration for subtree splitting
 */
export interface SplitConfig {
  /** Target nodes per subtree (default: 50) */
  targetNodesPerSubtree: number;
  /** Maximum subtrees to create (default: 10) */
  maxSubtrees: number;
  /** Minimum nodes to justify a separate subtree (default: 5) */
  minNodesPerSubtree: number;
  /** Rate limit: requests per second (default: 5) */
  requestsPerSecond: number;
}

const DEFAULT_SPLIT_CONFIG: SplitConfig = {
  targetNodesPerSubtree: 50,
  maxSubtrees: 10,
  minNodesPerSubtree: 5,
  requestsPerSecond: 5,
};

/**
 * Split hierarchical content into independent subtrees for parallel processing.
 *
 * The algorithm:
 * 1. Parse content into lines with indent levels
 * 2. Identify top-level nodes (indent 0) as potential subtree roots
 * 3. Group nodes to achieve target subtree size
 * 4. Each subtree contains a top-level node and all its descendants
 *
 * This ensures subtrees are independent and can be inserted in parallel
 * without parent-child ordering conflicts.
 */
export function splitIntoSubtrees(
  content: string,
  config: Partial<SplitConfig> = {}
): SubtreeSplitResult {
  const cfg = { ...DEFAULT_SPLIT_CONFIG, ...config };
  const lines = parseIndentedContent(content);

  if (lines.length === 0) {
    return {
      subtrees: [],
      totalNodes: 0,
      recommendedAgents: 0,
      singleAgentEstimateMs: 0,
      parallelEstimateMs: 0,
    };
  }

  // Find top-level node boundaries
  const topLevelBoundaries: number[] = [];
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].indent === 0) {
      topLevelBoundaries.push(i);
    }
  }

  // If no top-level nodes, treat entire content as one subtree
  if (topLevelBoundaries.length === 0) {
    const subtree = createSubtree("subtree-0", lines, cfg.requestsPerSecond);
    return {
      subtrees: [subtree],
      totalNodes: lines.length,
      recommendedAgents: 1,
      singleAgentEstimateMs: subtree.estimatedMs,
      parallelEstimateMs: subtree.estimatedMs,
    };
  }

  // Extract top-level groups (each top-level node + its descendants)
  const groups: ParsedLine[][] = [];
  for (let i = 0; i < topLevelBoundaries.length; i++) {
    const start = topLevelBoundaries[i];
    const end = topLevelBoundaries[i + 1] ?? lines.length;
    groups.push(lines.slice(start, end));
  }

  // Merge small groups or split if we have too many
  const subtrees = balanceSubtrees(groups, cfg);

  // Calculate timing estimates
  const totalNodes = lines.length;
  const msPerRequest = 1000 / cfg.requestsPerSecond;
  const singleAgentEstimateMs = totalNodes * msPerRequest;

  // Parallel estimate: longest subtree determines total time
  // (with some overhead for coordination)
  const maxSubtreeTime = Math.max(...subtrees.map((s) => s.estimatedMs));
  const coordinationOverhead = subtrees.length * 100; // 100ms per subtree for setup
  const parallelEstimateMs = maxSubtreeTime + coordinationOverhead;

  // Recommended agents: balance between parallelism and overhead
  const recommendedAgents = Math.min(
    subtrees.length,
    Math.ceil(totalNodes / cfg.targetNodesPerSubtree),
    cfg.maxSubtrees
  );

  return {
    subtrees,
    totalNodes,
    recommendedAgents,
    singleAgentEstimateMs,
    parallelEstimateMs,
  };
}

/**
 * Balance groups into subtrees of roughly equal size
 */
function balanceSubtrees(
  groups: ParsedLine[][],
  cfg: SplitConfig
): Subtree[] {
  const subtrees: Subtree[] = [];
  let currentGroup: ParsedLine[] = [];
  let subtreeIndex = 0;

  for (const group of groups) {
    const potentialSize = currentGroup.length + group.length;

    // If adding this group would exceed target, finalize current subtree
    if (
      currentGroup.length >= cfg.minNodesPerSubtree &&
      potentialSize > cfg.targetNodesPerSubtree * 1.5
    ) {
      subtrees.push(
        createSubtree(`subtree-${subtreeIndex++}`, currentGroup, cfg.requestsPerSecond)
      );
      currentGroup = [];
    }

    // Add group to current accumulator
    currentGroup.push(...group);

    // If we've hit max subtrees, stop splitting
    if (subtrees.length >= cfg.maxSubtrees - 1 && groups.indexOf(group) < groups.length - 1) {
      // Merge remaining groups into last subtree
      const remainingIndex = groups.indexOf(group) + 1;
      for (let i = remainingIndex; i < groups.length; i++) {
        currentGroup.push(...groups[i]);
      }
      break;
    }
  }

  // Don't forget the last group
  if (currentGroup.length > 0) {
    subtrees.push(
      createSubtree(`subtree-${subtreeIndex}`, currentGroup, cfg.requestsPerSecond)
    );
  }

  return subtrees;
}

/**
 * Create a subtree from parsed lines
 */
function createSubtree(
  id: string,
  lines: ParsedLine[],
  requestsPerSecond: number
): Subtree {
  // Normalize indent levels relative to the subtree root
  const minIndent = Math.min(...lines.map((l) => l.indent));
  const normalizedLines = lines.map((line) => ({
    ...line,
    indent: line.indent - minIndent,
  }));

  // Reconstruct content string with proper indentation
  const content = normalizedLines
    .map((line) => "  ".repeat(line.indent) + line.text)
    .join("\n");

  const msPerRequest = 1000 / requestsPerSecond;

  return {
    id,
    rootLine: normalizedLines[0],
    lines: normalizedLines,
    content,
    nodeCount: lines.length,
    estimatedMs: lines.length * msPerRequest,
  };
}

/**
 * Estimate time savings from parallel processing
 */
export function estimateTimeSavings(
  totalNodes: number,
  agentCount: number,
  requestsPerSecond: number = 5
): {
  singleAgentMs: number;
  parallelMs: number;
  savingsPercent: number;
  savingsSeconds: number;
} {
  const msPerRequest = 1000 / requestsPerSecond;
  const singleAgentMs = totalNodes * msPerRequest;

  // Each agent can process at the rate limit independently
  // But coordination adds overhead
  const nodesPerAgent = Math.ceil(totalNodes / agentCount);
  const agentProcessingMs = nodesPerAgent * msPerRequest;
  const coordinationOverheadMs = agentCount * 100 + 500; // Setup + coordination

  const parallelMs = agentProcessingMs + coordinationOverheadMs;

  const savingsMs = singleAgentMs - parallelMs;
  const savingsPercent = Math.max(0, (savingsMs / singleAgentMs) * 100);
  const savingsSeconds = Math.max(0, savingsMs / 1000);

  return {
    singleAgentMs,
    parallelMs,
    savingsPercent: Math.round(savingsPercent),
    savingsSeconds: Math.round(savingsSeconds * 10) / 10,
  };
}

/**
 * Merge subtree results back into a single result
 */
export interface SubtreeResult {
  subtreeId: string;
  success: boolean;
  nodeIds: string[];
  error?: string;
  durationMs: number;
}

export interface MergedResult {
  success: boolean;
  totalNodes: number;
  createdNodes: number;
  failedSubtrees: string[];
  allNodeIds: string[];
  totalDurationMs: number;
  errors: Array<{ subtreeId: string; error: string }>;
}

export function mergeSubtreeResults(results: SubtreeResult[]): MergedResult {
  const allNodeIds: string[] = [];
  const failedSubtrees: string[] = [];
  const errors: Array<{ subtreeId: string; error: string }> = [];
  let totalDurationMs = 0;

  for (const result of results) {
    if (result.success) {
      allNodeIds.push(...result.nodeIds);
    } else {
      failedSubtrees.push(result.subtreeId);
      if (result.error) {
        errors.push({ subtreeId: result.subtreeId, error: result.error });
      }
    }
    totalDurationMs = Math.max(totalDurationMs, result.durationMs);
  }

  return {
    success: failedSubtrees.length === 0,
    totalNodes: results.reduce((sum, r) => sum + (r.success ? r.nodeIds.length : 0), 0),
    createdNodes: allNodeIds.length,
    failedSubtrees,
    allNodeIds,
    totalDurationMs,
    errors,
  };
}
