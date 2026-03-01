/**
 * Workflowy MCP Server
 * Main entry point - wires up modules and handles tool requests
 */

import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
  ListResourcesRequestSchema,
  ReadResourceRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import * as path from "path";
import * as fs from "fs";

// Import from shared modules
import { validateConfig } from "../shared/config/environment.js";
import { workflowyRequest } from "../shared/api/workflowy.js";
import type {
  WorkflowyNode,
  NodeWithPath,
  RelatedNode,
  CreatedNode,
  AnalysisContentNode,
  AnalysisContentResult,
} from "../shared/types/index.js";
import {
  getCachedNodesIfValid,
  updateCache,
  invalidateCache,
  invalidateNode,
  startBatch,
  endBatch,
} from "../shared/utils/cache.js";
import {
  RequestQueue,
  initializeRequestQueue,
  type OperationType,
  type BatchResult,
} from "../shared/utils/requestQueue.js";
import { QUEUE_CONFIG } from "../shared/config/environment.js";
import { buildNodePaths } from "../shared/utils/node-paths.js";
import {
  parseIndentedContent,
  formatNodesForSelection,
  generateWorkflowyLink,
  extractWorkflowyLinks,
} from "../shared/utils/text-processing.js";
import {
  extractKeywords,
  calculateRelevance,
  findMatchedKeywords,
} from "../shared/utils/keyword-extraction.js";
import {
  createOrchestrator,
  type OrchestratorProgress,
  type MergedResult,
} from "../shared/utils/orchestrator.js";
import {
  splitIntoSubtrees,
  estimateTimeSavings,
} from "../shared/utils/subtree-parser.js";
import {
  convertLargeMarkdownToWorkflowy,
  analyzeMarkdown,
  type ConversionOptions,
} from "../shared/utils/large-markdown-converter.js";
import {
  JobQueue,
  getDefaultJobQueue,
  type Job,
  type JobStatus,
  type InsertContentJobParams,
  type InsertFileJobParams,
  type BatchOperationJobParams,
  type InsertContentJobResult,
  type InsertFileJobResult,
  type BatchOperationJobResult,
} from "../shared/utils/jobQueue.js";
import { getDefaultRateLimiter } from "../shared/utils/rateLimiter.js";
import {
  filterNodesByScope,
  getSubtreeNodes,
  buildChildrenIndex,
  type ScopeType,
} from "../shared/utils/scope-utils.js";
import { parseTags, parseNodeTags, nodeHasTag, nodeHasAssignee } from "../shared/utils/tag-parser.js";
import { parseDueDateFromNode, isOverdue, isDueWithin } from "../shared/utils/date-parser.js";
import { generateInteractiveConceptMapHTML } from "../shared/utils/concept-map-html.js";
import { generateTaskMap } from "../shared/utils/task-map.js";
import { uploadToDropboxPath, isDropboxConfigured } from "../shared/api/dropbox.js";
import { insertConceptMapOutline } from "../cli/concept-map-outline.js";
import {
  buildGraphStructure,
  calculateDegreeCentrality,
  calculateBetweennessCentrality,
  calculateClosenessCentrality,
  calculateEigenvectorCentrality,
  extractRelationshipsFromData,
  formatCentralityResults,
  type GraphEdge,
} from "../shared/utils/graph-analysis.js";

// Validate configuration on startup
validateConfig();

// ============================================================================
// Node Caching with API Integration
// ============================================================================

async function getCachedNodes(): Promise<WorkflowyNode[]> {
  const cached = getCachedNodesIfValid();
  if (cached) {
    return cached;
  }

  const response = await workflowyRequest("/nodes-export");
  let nodes: WorkflowyNode[];

  // API returns { nodes: [...] } not an array directly
  if (response && typeof response === "object" && "nodes" in response) {
    nodes = (response as { nodes: WorkflowyNode[] }).nodes;
  } else if (Array.isArray(response)) {
    nodes = response as WorkflowyNode[];
  } else {
    nodes = [];
  }

  updateCache(nodes);
  return nodes;
}

// ============================================================================
// Knowledge Linking Functions
// ============================================================================
// filterNodesByScope is now imported from ../shared/utils/scope-utils.js

async function findRelatedNodes(
  sourceNode: WorkflowyNode,
  allNodes: WorkflowyNode[],
  maxResults: number = 10,
  customKeywords?: string[]
): Promise<{ keywords: string[]; relatedNodes: RelatedNode[] }> {
  // Use custom keywords if provided, otherwise extract from node content
  let keywords: string[];
  if (customKeywords && customKeywords.length > 0) {
    // Normalize custom keywords (lowercase, filter empty)
    keywords = customKeywords
      .map(k => k.toLowerCase().trim())
      .filter(k => k.length > 0);
  } else {
    const sourceText = `${sourceNode.name || ""} ${sourceNode.note || ""}`;
    keywords = extractKeywords(sourceText);
  }

  if (keywords.length === 0) {
    return { keywords: [], relatedNodes: [] };
  }

  const scoredNodes: Array<{
    node: WorkflowyNode;
    score: number;
    matchedKeywords: string[];
  }> = [];

  for (const node of allNodes) {
    const score = calculateRelevance(node, keywords, sourceNode.id);
    if (score > 0) {
      const matchedKeywords = findMatchedKeywords(node, keywords);
      scoredNodes.push({ node, score, matchedKeywords });
    }
  }

  scoredNodes.sort((a, b) => b.score - a.score);
  const topNodes = scoredNodes.slice(0, maxResults);
  const nodesWithPaths = buildNodePaths(topNodes.map((n) => n.node));
  const pathMap = new Map(nodesWithPaths.map((n) => [n.id, n.path]));

  const relatedNodes: RelatedNode[] = topNodes.map((n) => ({
    id: n.node.id,
    name: n.node.name || "",
    note: n.node.note,
    path: pathMap.get(n.node.id) || n.node.name || "",
    relevanceScore: n.score,
    matchedKeywords: n.matchedKeywords,
    link: generateWorkflowyLink(n.node.id, n.node.name || ""),
  }));

  return { keywords, relatedNodes };
}


// ============================================================================
// Hierarchical Content Insertion
// ============================================================================

/**
 * Insert hierarchical content using a staging node approach.
 *
 * To avoid nodes briefly appearing at the wrong location during creation,
 * this function:
 * 1. Creates a temporary staging node under the target parent
 * 2. Creates all hierarchical content inside the staging node
 * 3. Moves top-level children from staging to the actual parent
 * 4. Deletes the staging node
 *
 * This ensures nodes are never visible at unintended locations during the operation.
 */
async function insertHierarchicalContent(
  rootParentId: string,
  content: string,
  position?: "top" | "bottom"
): Promise<CreatedNode[]> {
  const parsedLines = parseIndentedContent(content);
  if (parsedLines.length === 0) {
    return [];
  }

  // Step 1: Create a temporary staging node under the target parent
  const stagingNode = (await workflowyRequest("/nodes", "POST", {
    name: "__staging_temp__",
    parent_id: rootParentId,
    position: "bottom", // Always at bottom to minimize visibility
  })) as CreatedNode;

  const createdNodes: CreatedNode[] = [];
  const topLevelNodeIds: string[] = []; // Track top-level nodes for moving later
  const parentStack: string[] = [stagingNode.id]; // Start with staging node as root

  try {
    // Step 2: Create all content inside the staging node
    const BATCH_SIZE = 10;
    let i = 0;

    while (i < parsedLines.length) {
      const currentLine = parsedLines[i];
      const currentParentIndex = Math.min(currentLine.indent, parentStack.length - 1);

      // Collect consecutive lines that can share the same parent (same indent level)
      const batch: Array<{ line: typeof currentLine; index: number }> = [];

      while (i < parsedLines.length && batch.length < BATCH_SIZE) {
        const line = parsedLines[i];
        const parentIndex = Math.min(line.indent, parentStack.length - 1);

        if (line.indent === currentLine.indent && parentIndex < parentStack.length) {
          batch.push({ line, index: i });
          i++;
        } else {
          break;
        }
      }

      if (batch.length === 1) {
        const { line } = batch[0];
        const parentId = parentStack[Math.min(line.indent, parentStack.length - 1)];

        const result = (await workflowyRequest("/nodes", "POST", {
          name: line.text,
          parent_id: parentId,
          position: "bottom",
        })) as CreatedNode;

        createdNodes.push(result);

        // Track top-level nodes (indent 0) for moving later
        if (line.indent === 0) {
          topLevelNodeIds.push(result.id);
        }

        parentStack[line.indent + 1] = result.id;
        parentStack.length = line.indent + 2;
      } else {
        const batchPromises = batch.map(({ line }) => {
          const parentId = parentStack[Math.min(line.indent, parentStack.length - 1)];
          return workflowyRequest("/nodes", "POST", {
            name: line.text,
            parent_id: parentId,
            position: "bottom",
          }) as Promise<CreatedNode>;
        });

        const results = await Promise.all(batchPromises);

        for (let j = 0; j < results.length; j++) {
          const result = results[j];
          const { line } = batch[j];
          createdNodes.push(result);

          // Track top-level nodes for moving later
          if (line.indent === 0) {
            topLevelNodeIds.push(result.id);
          }

          parentStack[line.indent + 1] = result.id;
          parentStack.length = line.indent + 2;
        }
      }
    }

    // Step 3: Move top-level nodes from staging to the actual parent
    // Move in reverse order if position is "top" to maintain correct order
    const nodesToMove = position === "top" ? [...topLevelNodeIds].reverse() : topLevelNodeIds;

    for (const nodeId of nodesToMove) {
      await workflowyRequest(`/nodes/${nodeId}`, "POST", {
        parent_id: rootParentId,
        position: position || "bottom",
      });
    }

    // Step 4: Delete the staging node (should now be empty)
    await workflowyRequest(`/nodes/${stagingNode.id}`, "DELETE");

  } catch (error) {
    // Clean up staging node on error
    try {
      await workflowyRequest(`/nodes/${stagingNode.id}`, "DELETE");
    } catch {
      // Ignore cleanup errors
    }
    throw error;
  }

  return createdNodes;
}

/**
 * Insert multiple independent content blocks in parallel.
 * Each content block is processed hierarchically, but different
 * blocks can be processed concurrently.
 *
 * Use this when inserting content into multiple different parent nodes.
 */
async function insertMultipleContentBlocks(
  contentBlocks: Array<{
    parentId: string;
    content: string;
    position?: "top" | "bottom";
  }>
): Promise<Array<{ parentId: string; nodes: CreatedNode[]; error?: string }>> {
  const results = await Promise.allSettled(
    contentBlocks.map(async (block) => {
      const nodes = await insertHierarchicalContent(
        block.parentId,
        block.content,
        block.position
      );
      return { parentId: block.parentId, nodes };
    })
  );

  return results.map((result, index) => {
    if (result.status === "fulfilled") {
      return result.value;
    } else {
      return {
        parentId: contentBlocks[index].parentId,
        nodes: [],
        error: result.reason instanceof Error ? result.reason.message : String(result.reason),
      };
    }
  });
}

// ============================================================================
// Parallel Insertion Helper
// ============================================================================

interface ParallelInsertResult {
  success: boolean;
  message: string;
  nodes: Array<{ id: string; name?: string }>;
  stats?: {
    total_nodes: number;
    created_nodes: number;
    workers_used: number;
    duration_ms: number;
  };
  performance?: {
    estimated_single_agent_ms: number;
    actual_parallel_ms: number;
    savings_percent: number;
  };
  mode: "single_agent" | "parallel_workers";
  errors?: Array<{ subtreeId: string; error: string }>;
  workload_analysis?: {
    total_nodes: number;
    subtree_count: number;
    recommended_workers: number;
    subtrees?: Array<{ id: string; node_count: number; root_text: string }>;
  };
}

/**
 * Insert content using parallel workers by default.
 * Falls back to single-agent mode for small workloads.
 * On error, includes workload analysis for debugging.
 */
async function parallelInsertContent(
  parentId: string,
  content: string,
  position?: "top" | "bottom"
): Promise<ParallelInsertResult> {
  // Analyze workload to determine best approach
  const splitResult = splitIntoSubtrees(content, {
    maxSubtrees: 5,
    targetNodesPerSubtree: 50,
  });

  // For small workloads or single subtrees, use simple insertion
  if (splitResult.totalNodes < 20 || splitResult.subtrees.length === 1) {
    try {
      const createdNodes = await insertHierarchicalContent(parentId, content, position);
      return {
        success: true,
        message: `Inserted ${createdNodes.length} node(s)`,
        nodes: createdNodes,
        mode: "single_agent",
      };
    } catch (error) {
      // On error, return workload analysis
      return {
        success: false,
        message: error instanceof Error ? error.message : String(error),
        nodes: [],
        mode: "single_agent",
        workload_analysis: {
          total_nodes: splitResult.totalNodes,
          subtree_count: splitResult.subtrees.length,
          recommended_workers: splitResult.recommendedAgents,
        },
      };
    }
  }

  // Use parallel insertion for larger workloads
  try {
    const orchestrator = createOrchestrator(
      async (pId, subtreeContent, pos) => {
        const nodes = await insertHierarchicalContent(pId, subtreeContent, pos);
        return nodes.map((n) => ({ id: n.id, name: n.name }));
      },
      {
        maxWorkers: 5,
        workerRateLimit: 5,
        retryOnFailure: true,
        maxRetries: 2,
        splitConfig: {
          targetNodesPerSubtree: 50,
          maxSubtrees: 5,
        },
      }
    );

    const startTime = Date.now();
    const result = await orchestrator.execute({
      parentId,
      content,
      position,
    });

    const duration = Date.now() - startTime;
    const timeSavings = estimateTimeSavings(splitResult.totalNodes, splitResult.subtrees.length, 5);

    return {
      success: result.success,
      message: result.success
        ? `Inserted ${result.createdNodes} nodes using ${splitResult.subtrees.length} parallel workers`
        : `Partial success: ${result.createdNodes} nodes created, ${result.failedSubtrees.length} subtrees failed`,
      nodes: result.allNodeIds.map((id) => ({ id })),
      stats: {
        total_nodes: splitResult.totalNodes,
        created_nodes: result.createdNodes,
        workers_used: splitResult.subtrees.length,
        duration_ms: duration,
      },
      performance: {
        estimated_single_agent_ms: timeSavings.singleAgentMs,
        actual_parallel_ms: duration,
        savings_percent: Math.round(((timeSavings.singleAgentMs - duration) / timeSavings.singleAgentMs) * 100),
      },
      mode: "parallel_workers",
      errors: result.errors.length > 0 ? result.errors : undefined,
    };
  } catch (error) {
    // On parallel error, return workload analysis for debugging
    return {
      success: false,
      message: error instanceof Error ? error.message : String(error),
      nodes: [],
      mode: "parallel_workers",
      workload_analysis: {
        total_nodes: splitResult.totalNodes,
        subtree_count: splitResult.subtrees.length,
        recommended_workers: splitResult.recommendedAgents,
        subtrees: splitResult.subtrees.map((s) => ({
          id: s.id,
          node_count: s.nodeCount,
          root_text: s.rootLine.text,
        })),
      },
    };
  }
}

// ============================================================================
// Zod Schemas
// ============================================================================

const searchNodesSchema = z.object({
  query: z.string().describe("Text to search for in node names and notes"),
  tag: z.string().optional().describe("Filter by #tag (with or without #)"),
  assignee: z.string().optional().describe("Filter by @person (with or without @)"),
  status: z.enum(["all", "pending", "completed"]).optional().describe("Filter by completion status"),
  root_id: z.string().optional().describe("Limit search to a subtree"),
  scope: z.enum(["this_node", "children", "siblings", "ancestors", "all"]).optional().describe("Scope relative to root_id"),
  modified_after: z.string().optional().describe("ISO date string — only nodes modified after this date"),
  modified_before: z.string().optional().describe("ISO date string — only nodes modified before this date"),
});

const getNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to retrieve"),
});

const getChildrenSchema = z.object({
  parent_id: z.string().optional().describe("Parent node ID. Omit to get root-level nodes"),
});

const updateNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to update"),
  name: z.string().optional().describe("New text content for the node"),
  note: z.string().optional().describe("New note for the node"),
});

const deleteNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to delete"),
});

const moveNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to move"),
  parent_id: z.string().describe("The ID of the new parent node"),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings"),
});

const completeNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to mark as complete"),
});

const uncompleteNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to mark as incomplete"),
});

const listTodosSchema = z.object({
  parent_id: z.string().optional().describe("Filter to todos under a specific parent node"),
  status: z.enum(["all", "pending", "completed"]).optional().describe("Filter by completion status (default: all)"),
  query: z.string().optional().describe("Optional text to search for within todos"),
});

const findRelatedSchema = z.object({
  node_id: z.string().describe("The ID of the node to find related content for"),
  max_results: z.number().optional().describe("Maximum number of related nodes to return (default: 10)"),
});

const createLinksSchema = z.object({
  node_id: z.string().describe("The ID of the node to add links to"),
  link_node_ids: z.array(z.string()).optional().describe("Specific node IDs to link to. If omitted, auto-discovers related nodes."),
  max_links: z.number().optional().describe("Maximum number of auto-discovered links to create (default: 5)"),
  position: z.enum(["note", "child"]).optional().describe("Where to place links: 'note' appends to node note, 'child' creates a 'Related' child node (default: child)"),
});

const insertContentSchema = z.object({
  parent_id: z.string().describe("The ID of the parent node to insert content under"),
  content: z.string().describe("The content to insert. Must be 2-space indented format. Use convert_markdown_to_workflowy first if you have markdown."),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings (default: top)"),
});

const smartInsertSchema = z.object({
  search_query: z.string().describe("Search text to find the target node for insertion"),
  content: z.string().describe("Content in 2-space indented format. Use convert_markdown_to_workflowy first if you have markdown."),
  selection: z.number().optional().describe("If multiple matches found, the number (1-based) of the node to use"),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings (default: top)"),
});

const findNodeSchema = z.object({
  name: z.string().describe("The exact name of the node to find"),
  match_mode: z.enum(["exact", "contains", "starts_with"]).optional().describe("How to match the name: 'exact' (default) for exact match, 'contains' for substring match, 'starts_with' for prefix match"),
  selection: z.number().optional().describe("If multiple matches found, the number (1-based) of the node to use. Returns the selected node's ID ready for use."),
});

// LLM-Powered Concept Map Schemas
const getNodeContentForAnalysisSchema = z.object({
  node_id: z.string().describe("The ID of the root node to analyze"),
  depth: z.number().optional().describe("Maximum depth to traverse (default: unlimited)"),
  include_notes: z.boolean().optional().describe("Include node notes in output (default: true)"),
  max_nodes: z.number().optional().describe("Maximum number of nodes to return (default: 500)"),
  follow_links: z.boolean().optional().describe("Follow Workflowy internal links to include linked content (default: true)"),
  format: z.enum(["structured", "outline"]).optional().describe("Output format: 'structured' for JSON, 'outline' for indented text (default: structured)"),
});

// Batch Operations Schema for high-load scenarios
const batchOperationsSchema = z.object({
  operations: z.array(z.object({
    type: z.enum(["create", "update", "delete", "move", "complete", "uncomplete"]).describe("Operation type"),
    params: z.record(z.unknown()).describe("Operation parameters (varies by type)"),
  })).describe("Array of operations to execute"),
  parallel: z.boolean().optional().describe("Execute operations in parallel (default: true). Set to false for sequential execution."),
});

// Multi-Agent Orchestrator Schema for heavy workloads
// Analyze workload schema (for planning before execution)
const analyzeWorkloadSchema = z.object({
  content: z.string().describe("Hierarchical content to analyze"),
  max_workers: z.number().min(1).max(10).optional().describe("Maximum workers to consider (default: 5)"),
});

// Convert Markdown to Workflowy schema
const convertMarkdownToWorkflowySchema = z.object({
  markdown: z.string().describe("The markdown content to convert"),
  options: z.object({
    preserveInlineFormatting: z.boolean().optional().describe("Keep **bold**, *italic*, etc. (default: true)"),
    convertTables: z.boolean().optional().describe("Convert tables to hierarchical lists (default: true)"),
    includeHorizontalRules: z.boolean().optional().describe("Include --- as separator nodes (default: true)"),
    maxDepth: z.number().optional().describe("Maximum nesting depth (default: 10)"),
    preserveTaskLists: z.boolean().optional().describe("Keep [x] and [ ] checkboxes (default: true)"),
  }).optional().describe("Conversion options"),
  analyze_only: z.boolean().optional().describe("Only analyze the markdown, don't convert (returns stats)"),
});

// Job Queue Schemas
const submitJobSchema = z.object({
  type: z.enum(["insert_content", "batch_operations"]).describe("Type of job to submit"),
  params: z.record(z.unknown()).describe("Job parameters (varies by type)"),
  description: z.string().optional().describe("Optional description for tracking"),
});

const getJobStatusSchema = z.object({
  job_id: z.string().describe("The ID of the job to check"),
});

const getJobResultSchema = z.object({
  job_id: z.string().describe("The ID of the job to get results for"),
});

const listJobsSchema = z.object({
  status: z.array(z.enum(["pending", "processing", "completed", "failed", "cancelled"]))
    .optional()
    .describe("Filter by job status (default: all)"),
});

const cancelJobSchema = z.object({
  job_id: z.string().describe("The ID of the job to cancel"),
});

// File insertion schema - allows Claude to pass file path without reading
const insertFileSchema = z.object({
  file_path: z.string().describe("Absolute path to the file to insert"),
  parent_id: z.string().describe("The ID of the parent node to insert content under"),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings (default: top)"),
  format: z.enum(["auto", "markdown", "plain"]).optional().describe("File format: 'auto' detects from extension, 'markdown' forces markdown conversion, 'plain' treats as pre-formatted (default: auto)"),
});

// Submit file job schema - for large files
const submitFileJobSchema = z.object({
  file_path: z.string().describe("Absolute path to the file to insert"),
  parent_id: z.string().describe("The ID of the parent node to insert content under"),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings (default: top)"),
  format: z.enum(["auto", "markdown", "plain"]).optional().describe("File format handling (default: auto)"),
  description: z.string().optional().describe("Optional description for tracking"),
});

// ============================================================================
// Task & Knowledge Management Schemas
// ============================================================================

const getProjectSummarySchema = z.object({
  node_id: z.string().describe("The ID of the project root node"),
  include_tags: z.boolean().optional().describe("Include tag frequency counts (default: true)"),
  recently_modified_days: z.number().optional().describe("Days to consider for recently modified (default: 7)"),
});

const getRecentChangesSchema = z.object({
  days: z.number().optional().describe("Number of days to look back (default: 7)"),
  root_id: z.string().optional().describe("Limit to a subtree"),
  include_completed: z.boolean().optional().describe("Include completed nodes (default: true)"),
  limit: z.number().optional().describe("Max results to return (default: 50)"),
});

const listUpcomingSchema = z.object({
  days: z.number().optional().describe("Number of days ahead to look (default: 14)"),
  root_id: z.string().optional().describe("Limit to a subtree"),
  include_no_due_date: z.boolean().optional().describe("Append incomplete todos without due dates (default: false)"),
  limit: z.number().optional().describe("Max results to return (default: 50)"),
});

const listOverdueSchema = z.object({
  root_id: z.string().optional().describe("Limit to a subtree"),
  include_completed: z.boolean().optional().describe("Include completed overdue nodes (default: false)"),
  limit: z.number().optional().describe("Max results to return (default: 50)"),
});

const findBacklinksSchema = z.object({
  node_id: z.string().describe("The ID of the target node to find backlinks for"),
  limit: z.number().optional().describe("Max results to return (default: 50)"),
});

const duplicateNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to duplicate"),
  target_parent_id: z.string().describe("The ID of the parent to copy into"),
  position: z.enum(["top", "bottom"]).optional().describe("Position in target parent (default: top)"),
  include_children: z.boolean().optional().describe("Include descendants (default: true)"),
  name_prefix: z.string().optional().describe("Prefix to add to the root node name (e.g. 'Copy of ')"),
});

const createFromTemplateSchema = z.object({
  template_node_id: z.string().describe("The ID of the template node to copy"),
  target_parent_id: z.string().describe("The ID of the parent to insert the copy into"),
  variables: z.record(z.string()).optional().describe("Key-value map for {{variable}} substitution"),
  position: z.enum(["top", "bottom"]).optional().describe("Position in target parent (default: top)"),
});

const bulkUpdateSchema = z.object({
  filter: z.object({
    query: z.string().optional().describe("Text search filter"),
    tag: z.string().optional().describe("Filter by #tag"),
    assignee: z.string().optional().describe("Filter by @person"),
    status: z.enum(["all", "pending", "completed"]).optional().describe("Filter by completion status"),
    root_id: z.string().optional().describe("Limit to a subtree"),
    scope: z.enum(["this_node", "children", "siblings", "ancestors", "all"]).optional().describe("Scope relative to root_id"),
  }).describe("Criteria to select nodes"),
  operation: z.discriminatedUnion("type", [
    z.object({ type: z.literal("complete") }),
    z.object({ type: z.literal("uncomplete") }),
    z.object({ type: z.literal("add_tag"), tag: z.string().describe("Tag to add (with or without #)") }),
    z.object({ type: z.literal("remove_tag"), tag: z.string().describe("Tag to remove (with or without #)") }),
    z.object({
      type: z.literal("move"),
      target_parent_id: z.string().describe("Parent to move nodes into"),
      position: z.enum(["top", "bottom"]).optional(),
    }),
    z.object({ type: z.literal("delete") }),
  ]).describe("Operation to apply to matched nodes"),
  dry_run: z.boolean().optional().describe("Preview matches without making changes (default: false)"),
  limit: z.number().optional().describe("Safety cap on affected nodes (default: 20)"),
});

const dailyReviewSchema = z.object({
  root_id: z.string().optional().describe("Limit review to a subtree"),
  overdue_limit: z.number().optional().describe("Max overdue items to show (default: 10)"),
  upcoming_days: z.number().optional().describe("Days ahead for upcoming items (default: 7)"),
  recent_days: z.number().optional().describe("Days back for recent changes (default: 1)"),
  pending_limit: z.number().optional().describe("Max pending todos to show (default: 20)"),
});

const renderInteractiveConceptMapSchema = z.object({
  title: z.string().describe("Title for the concept map"),
  core_concept: z.object({
    label: z.string().describe("Label for the central concept"),
    description: z.string().optional().describe("Optional description"),
    workflowy_node_id: z.string().optional().describe("Workflowy node ID for linking back to source"),
  }),
  concepts: z.array(z.object({
    id: z.string().describe("Unique identifier for this concept"),
    label: z.string().describe("Display label"),
    level: z.enum(["major", "detail"]).describe("Major concepts form the inner ring; details orbit their parent"),
    importance: z.number().optional().describe("1-10 importance score (affects node size)"),
    parent_major_id: z.string().optional().describe("Which major concept this detail belongs to (for collapse grouping)"),
    workflowy_node_id: z.string().optional().describe("Workflowy node ID for linking back to source"),
  })),
  relationships: z.array(z.object({
    from: z.string().describe("Source concept ID"),
    to: z.string().describe("Target concept ID"),
    type: z.string().describe("Relationship type (e.g. 'supports', 'contrasts with', 'requires')"),
    strength: z.number().optional().describe("1-10 relationship strength (affects line thickness)"),
  })),
});

// Graph analysis schemas
const graphEdgeSchema = z.object({
  from: z.string().describe("Source vertex name"),
  to: z.string().describe("Target vertex name"),
  weight: z.number().optional().default(1).describe("Edge weight (default: 1)"),
});

const analyzeRelationshipsSchema = z.object({
  data: z.array(z.record(z.unknown())).describe("Array of data objects to analyze for relationships"),
  relationship_fields: z.array(z.string()).describe("Fields that contain relationship references (e.g. ['parent_id', 'friends'])"),
  node_label_field: z.string().optional().default("id").describe("Field to use as node labels (default: 'id')"),
});

const createAdjacencyMatrixSchema = z.object({
  relationships: z.array(graphEdgeSchema).describe("Array of relationship objects with from/to/weight"),
  vertices: z.array(z.string()).describe("Array of vertex names"),
});

const calculateCentralitySchema = z.object({
  relationships: z.array(graphEdgeSchema).describe("Array of relationship objects with from/to/weight"),
  vertices: z.array(z.string()).describe("Array of vertex names"),
  measures: z.array(z.enum(["degree", "betweenness", "closeness", "eigenvector", "all"]))
    .optional().default(["all"]).describe("Centrality measures to calculate (default: all)"),
  top_n: z.number().optional().default(10).describe("Number of top nodes to return per measure (default: 10)"),
});

const analyzeNetworkStructureSchema = z.object({
  data: z.array(z.record(z.unknown())).describe("Array of data objects to analyze"),
  relationship_fields: z.array(z.string()).describe("Fields that contain relationship references"),
  node_label_field: z.string().optional().default("id").describe("Field to use as node labels (default: 'id')"),
  include_centrality: z.boolean().optional().default(true).describe("Whether to include centrality analysis (default: true)"),
});

const generateTaskMapSchema = z.object({
  max_details_per_tag: z.number().optional().describe("Maximum detail nodes per tag (default: 8)"),
  detail_sort_by: z.enum(["recency", "name"]).optional().describe("Sort detail nodes by recency or name (default: recency)"),
  title: z.string().optional().describe("Custom title for the task map (default: 'Task Map')"),
  exclude_completed: z.boolean().optional().describe("Exclude completed nodes from tag matching (default: false)"),
  exclude_mentions: z.boolean().optional().describe("Exclude @mention tags, only use #hashtags (default: true)"),
  insert_outline: z.boolean().optional().describe("Insert a concept map outline into Workflowy under the Tags node (default: false)"),
  force_outline: z.boolean().optional().describe("Overwrite existing task map outline if one exists (default: false)"),
});

// State for the last generated interactive map (served via MCP resources)
let lastInteractiveMapHTML: string | null = null;
let lastInteractiveMapTitle: string = "";

// ============================================================================
// Request Queue Setup
// ============================================================================

const requestQueue = new RequestQueue(QUEUE_CONFIG);
requestQueue.setApiRequestFn(workflowyRequest);

// ============================================================================
// Job Queue Setup
// ============================================================================

const jobQueue = getDefaultJobQueue();

// Register insert_content job executor
jobQueue.registerExecutor<InsertContentJobParams, InsertContentJobResult>(
  "insert_content",
  async (params, onProgress, signal) => {
    const { parentId, content, position } = params;
    const rateLimiter = getDefaultRateLimiter();

    // Parse content to count nodes
    const lines = content.split("\n").filter(line => line.trim());
    const totalNodes = lines.length;

    onProgress({ total: totalNodes, completed: 0, failed: 0, currentOperation: "Starting insertion" });

    // Check abort before starting
    if (signal.aborted) {
      throw new Error("Job cancelled");
    }

    try {
      // Use the existing parallel insert content logic but with rate limiting
      const createdNodes = await insertHierarchicalContent(parentId, content, position);
      invalidateCache();

      onProgress({
        completed: createdNodes.length,
        currentOperation: "Completed",
      });

      return {
        success: true,
        nodesCreated: createdNodes.length,
        nodeIds: createdNodes.map(n => n.id),
      };
    } catch (error) {
      return {
        success: false,
        nodesCreated: 0,
        nodeIds: [],
        errors: [error instanceof Error ? error.message : String(error)],
      };
    }
  }
);

// Register batch_operations job executor
jobQueue.registerExecutor<BatchOperationJobParams, BatchOperationJobResult>(
  "batch_operations",
  async (params, onProgress, signal) => {
    const { operations } = params;
    const rateLimiter = getDefaultRateLimiter();
    const results: BatchOperationJobResult["results"] = [];
    let totalSucceeded = 0;
    let totalFailed = 0;

    onProgress({
      total: operations.length,
      completed: 0,
      failed: 0,
      currentOperation: "Starting batch operations",
    });

    startBatch();

    for (let i = 0; i < operations.length; i++) {
      // Check for abort
      if (signal.aborted) {
        endBatch();
        throw new Error("Job cancelled");
      }

      const op = operations[i];
      onProgress({
        currentOperation: `Processing operation ${i + 1}/${operations.length}: ${op.type}`,
      });

      // Wait for rate limiter
      await rateLimiter.acquire();

      try {
        let result: unknown;

        switch (op.type) {
          case "create":
            result = await workflowyRequest("/nodes", "POST", op.params as object);
            break;
          case "update": {
            const { node_id, ...updateParams } = op.params as { node_id: string; [key: string]: unknown };
            result = await workflowyRequest(`/nodes/${node_id}`, "POST", updateParams);
            break;
          }
          case "delete": {
            const nodeId = (op.params as { node_id: string }).node_id;
            result = await workflowyRequest(`/nodes/${nodeId}`, "DELETE");
            break;
          }
          case "move": {
            const { node_id: moveId, ...moveParams } = op.params as { node_id: string; [key: string]: unknown };
            result = await workflowyRequest(`/nodes/${moveId}`, "POST", moveParams);
            break;
          }
          case "complete": {
            const completeId = (op.params as { node_id: string }).node_id;
            result = await workflowyRequest(`/nodes/${completeId}/complete`, "POST");
            break;
          }
          case "uncomplete": {
            const uncompleteId = (op.params as { node_id: string }).node_id;
            result = await workflowyRequest(`/nodes/${uncompleteId}/uncomplete`, "POST");
            break;
          }
        }

        results.push({ index: i, status: "success", result });
        totalSucceeded++;
        onProgress({ completed: totalSucceeded, failed: totalFailed });
      } catch (error) {
        results.push({
          index: i,
          status: "failed",
          error: error instanceof Error ? error.message : String(error),
        });
        totalFailed++;
        onProgress({ completed: totalSucceeded, failed: totalFailed });
      }
    }

    endBatch();
    invalidateCache();

    return {
      success: totalFailed === 0,
      results,
      totalSucceeded,
      totalFailed,
    };
  }
);

// Helper function to read and process a file for insertion
async function readAndProcessFile(
  filePath: string,
  format: "auto" | "markdown" | "plain" = "auto"
): Promise<{ content: string; format: string; fileName: string; fileSize: number }> {
  // Check file exists
  if (!fs.existsSync(filePath)) {
    throw new Error(`File not found: ${filePath}`);
  }

  const stats = fs.statSync(filePath);
  if (!stats.isFile()) {
    throw new Error(`Path is not a file: ${filePath}`);
  }

  const fileName = path.basename(filePath);
  const fileSize = stats.size;
  const ext = path.extname(filePath).toLowerCase();

  // Read file content
  const rawContent = fs.readFileSync(filePath, "utf-8");

  // Determine format
  let actualFormat = format;
  if (format === "auto") {
    if (ext === ".md" || ext === ".markdown") {
      actualFormat = "markdown";
    } else {
      actualFormat = "plain";
    }
  }

  // Process content based on format
  let processedContent: string;
  if (actualFormat === "markdown") {
    // Convert markdown to Workflowy format
    const result = convertLargeMarkdownToWorkflowy(rawContent);
    processedContent = result.content;
  } else {
    // Plain format - assume already in 2-space indented format or single-level content
    processedContent = rawContent;
  }

  return {
    content: processedContent,
    format: actualFormat,
    fileName,
    fileSize,
  };
}

// Register insert_file job executor
jobQueue.registerExecutor<InsertFileJobParams, InsertFileJobResult>(
  "insert_file",
  async (params, onProgress, signal) => {
    const { filePath, parentId, position, format } = params;

    onProgress({ total: 1, completed: 0, failed: 0, currentOperation: "Reading file" });

    if (signal.aborted) {
      throw new Error("Job cancelled");
    }

    try {
      // Read and process the file
      const { content, format: actualFormat, fileName, fileSize } = await readAndProcessFile(
        filePath,
        format
      );

      onProgress({ currentOperation: "Processing content" });

      // Count nodes
      const lines = content.split("\n").filter((line: string) => line.trim());
      const totalNodes = lines.length;

      onProgress({ total: totalNodes, currentOperation: "Inserting content" });

      if (signal.aborted) {
        throw new Error("Job cancelled");
      }

      // Insert content
      const createdNodes = await insertHierarchicalContent(parentId, content, position);
      invalidateCache();

      onProgress({ completed: createdNodes.length, currentOperation: "Completed" });

      return {
        success: true,
        nodesCreated: createdNodes.length,
        nodeIds: createdNodes.map((n) => n.id),
        fileName,
        fileSize,
        format: actualFormat,
      };
    } catch (error) {
      return {
        success: false,
        nodesCreated: 0,
        nodeIds: [],
        fileName: path.basename(filePath),
        fileSize: 0,
        format: format || "auto",
        errors: [error instanceof Error ? error.message : String(error)],
      };
    }
  }
);

// ============================================================================
// MCP Server Setup
// ============================================================================

const server = new Server(
  { name: "workflowy-mcp-server", version: "1.0.0" },
  { capabilities: { tools: {}, resources: {} } }
);

// Tool definitions
server.setRequestHandler(ListToolsRequestSchema, async () => {
  return {
    tools: [
      {
        name: "search_nodes",
        description: "Search for nodes in Workflowy by text with optional filters. When filters are applied, returns structured JSON with tags, assignees, and due dates.",
        inputSchema: {
          type: "object",
          properties: {
            query: { type: "string", description: "Text to search for in node names and notes" },
            tag: { type: "string", description: "Filter by #tag (with or without #)" },
            assignee: { type: "string", description: "Filter by @person (with or without @)" },
            status: { type: "string", enum: ["all", "pending", "completed"], description: "Filter by completion status" },
            root_id: { type: "string", description: "Limit search to a subtree" },
            scope: { type: "string", enum: ["this_node", "children", "siblings", "ancestors", "all"], description: "Scope relative to root_id" },
            modified_after: { type: "string", description: "ISO date — only nodes modified after this" },
            modified_before: { type: "string", description: "ISO date — only nodes modified before this" },
          },
          required: ["query"],
        },
      },
      {
        name: "get_node",
        description: "Get a specific node by its ID, including its full content and metadata.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to retrieve" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "get_children",
        description: "Get child nodes of a parent node. Omit parent_id to get root-level nodes.",
        inputSchema: {
          type: "object",
          properties: {
            parent_id: { type: "string", description: "Parent node ID. Omit to get root-level nodes" },
          },
        },
      },
      {
        name: "update_node",
        description: "Update an existing node's name and/or note.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to update" },
            name: { type: "string", description: "New text content for the node" },
            note: { type: "string", description: "New note for the node" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "delete_node",
        description: "Permanently delete a node from Workflowy.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to delete" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "move_node",
        description: "Move a node to a new parent location.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to move" },
            parent_id: { type: "string", description: "The ID of the new parent node" },
            position: { type: "string", enum: ["top", "bottom"], description: "Position relative to siblings" },
          },
          required: ["node_id", "parent_id"],
        },
      },
      {
        name: "complete_node",
        description: "Mark a node as completed.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to mark as complete" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "uncomplete_node",
        description: "Mark a node as incomplete.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to mark as incomplete" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "list_todos",
        description: "List all todos with optional filtering by status, parent, and search text.",
        inputSchema: {
          type: "object",
          properties: {
            parent_id: { type: "string", description: "Filter to todos under a specific parent node" },
            status: { type: "string", enum: ["all", "pending", "completed"], description: "Filter by completion status" },
            query: { type: "string", description: "Optional text to search for within todos" },
          },
        },
      },
      {
        name: "find_related",
        description: "Find nodes related to a given node based on keyword analysis.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to find related content for" },
            max_results: { type: "number", description: "Maximum number of related nodes to return" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "create_links",
        description: "Create internal Workflowy links from a node to related content.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to add links to" },
            link_node_ids: { type: "array", items: { type: "string" }, description: "Specific node IDs to link to" },
            max_links: { type: "number", description: "Maximum number of auto-discovered links" },
            position: { type: "string", enum: ["note", "child"], description: "Where to place links" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "insert_content",
        description: "THE PRIMARY TOOL for inserting nodes into Workflowy. Use this for ALL node creation - single nodes, bulk content, todos, any hierarchical structure. Content MUST be in 2-space indented format. For markdown, first use convert_markdown_to_workflowy. For todos, use [ ] or [x] prefix. Auto-optimizes for any workload size (1 to 1000+ nodes).",
        inputSchema: {
          type: "object",
          properties: {
            parent_id: { type: "string", description: "The ID of the parent node to insert content under" },
            content: { type: "string", description: "Content in 2-space indented format. Single line = single node. Multiple indented lines = hierarchy. Use [ ] for todos, [x] for completed todos. Use convert_markdown_to_workflowy first if you have markdown." },
            position: { type: "string", enum: ["top", "bottom"], description: "Position relative to siblings (default: top)" },
          },
          required: ["parent_id", "content"],
        },
      },
      {
        name: "smart_insert",
        description: "Search for a node by name and insert content. Content MUST be in 2-space indented format. For markdown, first use convert_markdown_to_workflowy to convert it. If multiple matches found, returns options for selection.",
        inputSchema: {
          type: "object",
          properties: {
            search_query: { type: "string", description: "Search text to find the target node" },
            content: { type: "string", description: "Content in 2-space indented format. Use convert_markdown_to_workflowy first if you have markdown." },
            selection: { type: "number", description: "If multiple matches found, the number (1-based) of the node to use" },
            position: { type: "string", enum: ["top", "bottom"], description: "Position relative to siblings" },
          },
          required: ["search_query", "content"],
        },
      },
      {
        name: "find_node",
        description: "Fast node lookup by name. Returns the node ID ready for use with other tools. Handles duplicates by presenting options for selection. Use this when you need to find a specific node by its exact name.",
        inputSchema: {
          type: "object",
          properties: {
            name: { type: "string", description: "The name of the node to find" },
            match_mode: { type: "string", enum: ["exact", "contains", "starts_with"], description: "How to match: 'exact' (default), 'contains', or 'starts_with'" },
            selection: { type: "number", description: "If multiple matches, the number (1-based) to select. Returns that node's ID." },
          },
          required: ["name"],
        },
      },
      // LLM-Powered Concept Map Tools
      {
        name: "get_node_content_for_analysis",
        description: "Extract content from a Workflowy subtree formatted for LLM semantic analysis. Returns all descendant nodes with their names, notes, hierarchy depth, and paths. Automatically follows internal Workflowy links to include connected content. Use this tool to get content that you can then analyze to discover concepts and relationships for concept mapping.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the root node to analyze" },
            depth: { type: "number", description: "Maximum depth to traverse (default: unlimited)" },
            include_notes: { type: "boolean", description: "Include node notes in output (default: true)" },
            max_nodes: { type: "number", description: "Maximum number of nodes to return (default: 500)" },
            follow_links: { type: "boolean", description: "Follow Workflowy internal links to include linked content (default: true)" },
            format: { type: "string", enum: ["structured", "outline"], description: "Output format: 'structured' for JSON with metadata, 'outline' for indented text (default: structured)" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "batch_operations",
        description: "Execute multiple operations in a single call with controlled concurrency. Use this for bulk inserts, updates, or mixed operations to improve performance under high load. Operations are processed with rate limiting to avoid overwhelming the Workflowy API.",
        inputSchema: {
          type: "object",
          properties: {
            operations: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  type: {
                    type: "string",
                    enum: ["create", "update", "delete", "move", "complete", "uncomplete"],
                    description: "Operation type",
                  },
                  params: {
                    type: "object",
                    description: "Operation parameters. For create: {name, note?, parent_id?, position?}. For update: {node_id, name?, note?}. For delete/complete/uncomplete: {node_id}. For move: {node_id, parent_id, position?}",
                  },
                },
                required: ["type", "params"],
              },
              description: "Array of operations to execute",
            },
            parallel: {
              type: "boolean",
              description: "Execute operations in parallel (default: true). Set to false for strict sequential execution.",
            },
          },
          required: ["operations"],
        },
      },
      {
        name: "analyze_workload",
        description: "Analyze hierarchical content to estimate insertion performance. Returns subtree breakdown, node count, and estimated time. Use this to understand workload before insert_content.",
        inputSchema: {
          type: "object",
          properties: {
            content: {
              type: "string",
              description: "Hierarchical content to analyze (indented with 2 spaces per level)",
            },
            max_workers: {
              type: "number",
              minimum: 1,
              maximum: 10,
              description: "Maximum workers to consider for estimation (default: 5)",
            },
          },
          required: ["content"],
        },
      },
      {
        name: "convert_markdown_to_workflowy",
        description: "REQUIRED for any markdown content. Converts markdown to Workflowy's 2-space indented format. This is the ONLY way to format markdown for Workflowy - other tools (insert_content, smart_insert) require pre-formatted content. Handles: headers (H1-H6, ATX and setext), nested lists, task lists [x]/[ ], fenced code blocks, tables, blockquotes, inline formatting. Returns converted content ready for insert_content or direct paste into Workflowy.",
        inputSchema: {
          type: "object",
          properties: {
            markdown: {
              type: "string",
              description: "The markdown content to convert. Supports headers, lists, code blocks, tables, blockquotes, task lists, and inline formatting.",
            },
            options: {
              type: "object",
              properties: {
                preserveInlineFormatting: {
                  type: "boolean",
                  description: "Keep **bold**, *italic*, `code`, and links (default: true)",
                },
                convertTables: {
                  type: "boolean",
                  description: "Convert markdown tables to hierarchical lists (default: true)",
                },
                includeHorizontalRules: {
                  type: "boolean",
                  description: "Include --- horizontal rules as separator nodes (default: true)",
                },
                maxDepth: {
                  type: "number",
                  description: "Maximum nesting depth for output (default: 10)",
                },
                preserveTaskLists: {
                  type: "boolean",
                  description: "Keep [x] and [ ] checkbox markers for task lists (default: true)",
                },
              },
              description: "Optional conversion settings",
            },
            analyze_only: {
              type: "boolean",
              description: "If true, only analyze the markdown and return statistics without converting",
            },
          },
          required: ["markdown"],
        },
      },
      // ========================================================================
      // Async Job Queue Tools - For handling large workloads without hitting API limits
      // ========================================================================
      {
        name: "submit_job",
        description: "Submit a large workload for background processing. The MCP server queues and processes operations respecting API rate limits. Returns a job ID to track progress. Use this for large insert_content or batch_operations to avoid timeouts and rate limit errors.",
        inputSchema: {
          type: "object",
          properties: {
            type: {
              type: "string",
              enum: ["insert_content", "batch_operations"],
              description: "Type of job: 'insert_content' for hierarchical content insertion, 'batch_operations' for multiple operations",
            },
            params: {
              type: "object",
              description: "Job parameters. For insert_content: {parentId, content, position?}. For batch_operations: {operations: [{type, params}...]}",
            },
            description: {
              type: "string",
              description: "Optional human-readable description for tracking",
            },
          },
          required: ["type", "params"],
        },
      },
      {
        name: "get_job_status",
        description: "Check the progress of a submitted job. Returns status (pending/processing/completed/failed/cancelled) and progress information.",
        inputSchema: {
          type: "object",
          properties: {
            job_id: {
              type: "string",
              description: "The job ID returned from submit_job",
            },
          },
          required: ["job_id"],
        },
      },
      {
        name: "get_job_result",
        description: "Get the result of a completed job. Only available for jobs with status 'completed' or 'failed'. Returns the operation results or error details.",
        inputSchema: {
          type: "object",
          properties: {
            job_id: {
              type: "string",
              description: "The job ID returned from submit_job",
            },
          },
          required: ["job_id"],
        },
      },
      {
        name: "list_jobs",
        description: "List all jobs with optional status filtering. Shows job IDs, types, status, and progress for tracking multiple operations.",
        inputSchema: {
          type: "object",
          properties: {
            status: {
              type: "array",
              items: {
                type: "string",
                enum: ["pending", "processing", "completed", "failed", "cancelled"],
              },
              description: "Filter by status (default: all)",
            },
          },
        },
      },
      {
        name: "cancel_job",
        description: "Cancel a pending or processing job. Cannot cancel already completed/failed/cancelled jobs.",
        inputSchema: {
          type: "object",
          properties: {
            job_id: {
              type: "string",
              description: "The job ID to cancel",
            },
          },
          required: ["job_id"],
        },
      },
      // ========================================================================
      // File Insertion Tools - Claude can pass file paths without reading
      // ========================================================================
      {
        name: "insert_file",
        description: "Insert a file's contents into Workflowy. The server reads the file, converts markdown if needed, and inserts. Claude does NOT need to read the file first. Supports .md, .markdown, .txt, and plain text files.",
        inputSchema: {
          type: "object",
          properties: {
            file_path: {
              type: "string",
              description: "Absolute path to the file to insert",
            },
            parent_id: {
              type: "string",
              description: "The ID of the parent node to insert content under",
            },
            position: {
              type: "string",
              enum: ["top", "bottom"],
              description: "Position relative to siblings (default: top)",
            },
            format: {
              type: "string",
              enum: ["auto", "markdown", "plain"],
              description: "How to process the file: 'auto' detects from extension (.md/.markdown → markdown), 'markdown' forces markdown conversion, 'plain' treats as pre-formatted 2-space indented content (default: auto)",
            },
          },
          required: ["file_path", "parent_id"],
        },
      },
      {
        name: "submit_file_job",
        description: "Submit a large file for background insertion. Use this for large files to avoid timeouts. The server reads, converts, and inserts the file while respecting API rate limits. Returns a job ID to track progress.",
        inputSchema: {
          type: "object",
          properties: {
            file_path: {
              type: "string",
              description: "Absolute path to the file to insert",
            },
            parent_id: {
              type: "string",
              description: "The ID of the parent node to insert content under",
            },
            position: {
              type: "string",
              enum: ["top", "bottom"],
              description: "Position relative to siblings (default: top)",
            },
            format: {
              type: "string",
              enum: ["auto", "markdown", "plain"],
              description: "How to process the file (default: auto)",
            },
            description: {
              type: "string",
              description: "Optional description for tracking",
            },
          },
          required: ["file_path", "parent_id"],
        },
      },
      // ======== Task & Knowledge Management Tools ========
      {
        name: "get_project_summary",
        description: "Get a comprehensive status overview of a project subtree: node counts, todo stats, tag frequencies, assignees, overdue items, and recently modified nodes.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the project root node" },
            include_tags: { type: "boolean", description: "Include tag frequency counts (default: true)" },
            recently_modified_days: { type: "number", description: "Days to consider for recently modified (default: 7)" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "get_recent_changes",
        description: "List nodes modified within a time window, sorted by most recent first.",
        inputSchema: {
          type: "object",
          properties: {
            days: { type: "number", description: "Number of days to look back (default: 7)" },
            root_id: { type: "string", description: "Limit to a subtree" },
            include_completed: { type: "boolean", description: "Include completed nodes (default: true)" },
            limit: { type: "number", description: "Max results (default: 50)" },
          },
        },
      },
      {
        name: "list_upcoming",
        description: "List todos with due dates in the next N days, sorted by urgency (overdue first, then nearest due date).",
        inputSchema: {
          type: "object",
          properties: {
            days: { type: "number", description: "Days ahead to look (default: 14)" },
            root_id: { type: "string", description: "Limit to a subtree" },
            include_no_due_date: { type: "boolean", description: "Append incomplete todos without due dates at end (default: false)" },
            limit: { type: "number", description: "Max results (default: 50)" },
          },
        },
      },
      {
        name: "list_overdue",
        description: "List all past-due items sorted by most overdue first.",
        inputSchema: {
          type: "object",
          properties: {
            root_id: { type: "string", description: "Limit to a subtree" },
            include_completed: { type: "boolean", description: "Include completed overdue nodes (default: false)" },
            limit: { type: "number", description: "Max results (default: 50)" },
          },
        },
      },
      {
        name: "find_backlinks",
        description: "Find all nodes that contain a Workflowy link to the specified node.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the target node" },
            limit: { type: "number", description: "Max results (default: 50)" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "duplicate_node",
        description: "Deep-copy a node and its subtree to a new location.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the node to duplicate" },
            target_parent_id: { type: "string", description: "The ID of the parent to copy into" },
            position: { type: "string", enum: ["top", "bottom"], description: "Position in target parent" },
            include_children: { type: "boolean", description: "Include descendants (default: true)" },
            name_prefix: { type: "string", description: "Prefix for the root node name (e.g. 'Copy of ')" },
          },
          required: ["node_id", "target_parent_id"],
        },
      },
      {
        name: "create_from_template",
        description: "Copy a template subtree with {{variable}} substitution in node names and notes.",
        inputSchema: {
          type: "object",
          properties: {
            template_node_id: { type: "string", description: "The ID of the template node" },
            target_parent_id: { type: "string", description: "The ID of the parent to insert into" },
            variables: { type: "object", description: "Key-value map for {{variable}} substitution", additionalProperties: { type: "string" } },
            position: { type: "string", enum: ["top", "bottom"], description: "Position in target parent" },
          },
          required: ["template_node_id", "target_parent_id"],
        },
      },
      {
        name: "bulk_update",
        description: "Apply an operation (complete, uncomplete, add_tag, remove_tag, move, delete) to all nodes matching a filter. Use dry_run to preview.",
        inputSchema: {
          type: "object",
          properties: {
            filter: {
              type: "object",
              description: "Criteria to select nodes",
              properties: {
                query: { type: "string", description: "Text search" },
                tag: { type: "string", description: "Filter by #tag" },
                assignee: { type: "string", description: "Filter by @person" },
                status: { type: "string", enum: ["all", "pending", "completed"] },
                root_id: { type: "string", description: "Limit to subtree" },
                scope: { type: "string", enum: ["this_node", "children", "siblings", "ancestors", "all"] },
              },
            },
            operation: {
              type: "object",
              description: "Operation to apply. Must include 'type' field: complete, uncomplete, add_tag, remove_tag, move, delete",
              properties: {
                type: { type: "string", enum: ["complete", "uncomplete", "add_tag", "remove_tag", "move", "delete"] },
                tag: { type: "string", description: "For add_tag/remove_tag" },
                target_parent_id: { type: "string", description: "For move" },
                position: { type: "string", enum: ["top", "bottom"], description: "For move" },
              },
              required: ["type"],
            },
            dry_run: { type: "boolean", description: "Preview without making changes (default: false)" },
            limit: { type: "number", description: "Safety cap on affected nodes (default: 20)" },
          },
          required: ["filter", "operation"],
        },
      },
      {
        name: "daily_review",
        description: "One-call daily standup summary: overdue items, upcoming deadlines, recent changes, and top pending todos.",
        inputSchema: {
          type: "object",
          properties: {
            root_id: { type: "string", description: "Limit review to a subtree" },
            overdue_limit: { type: "number", description: "Max overdue items (default: 10)" },
            upcoming_days: { type: "number", description: "Days ahead for upcoming (default: 7)" },
            recent_days: { type: "number", description: "Days back for recent changes (default: 1)" },
            pending_limit: { type: "number", description: "Max pending todos (default: 20)" },
          },
        },
      },
      {
        name: "render_interactive_concept_map",
        description: "Render an interactive, collapsible concept map. Click major concepts to expand/collapse detail nodes. Supports zoom/pan.",
        inputSchema: {
          type: "object",
          properties: {
            title: { type: "string", description: "Title for the concept map" },
            core_concept: {
              type: "object",
              properties: {
                label: { type: "string", description: "Label for the central concept" },
                description: { type: "string", description: "Optional description" },
                workflowy_node_id: { type: "string", description: "Workflowy node ID for linking back to source" },
              },
              required: ["label"],
            },
            concepts: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  id: { type: "string", description: "Unique ID" },
                  label: { type: "string", description: "Display label" },
                  level: { type: "string", enum: ["major", "detail"], description: "Major or detail level" },
                  importance: { type: "number", description: "1-10 importance (affects size)" },
                  parent_major_id: { type: "string", description: "Parent major concept ID (for detail nodes)" },
                  workflowy_node_id: { type: "string", description: "Workflowy node ID for linking back to source" },
                },
                required: ["id", "label", "level"],
              },
            },
            relationships: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  from: { type: "string", description: "Source concept ID" },
                  to: { type: "string", description: "Target concept ID" },
                  type: { type: "string", description: "Relationship type" },
                  strength: { type: "number", description: "1-10 strength" },
                },
                required: ["from", "to", "type"],
              },
            },
          },
          required: ["title", "core_concept", "concepts", "relationships"],
        },
        _meta: {
          ui: {
            resourceUri: "ui://concept-map/interactive",
          },
        },
      },
      // ── Task Map ──
      {
        name: "generate_task_map",
        description: "Generate an interactive concept map from Workflowy's Tags node. Finds the root-level 'Tags' node, reads its children as #tags and @mentions, searches all nodes for matches, and produces a visual map showing tag relationships via co-occurrence. Optionally inserts an outline into Workflowy.",
        inputSchema: {
          type: "object",
          properties: {
            max_details_per_tag: { type: "number", description: "Maximum detail nodes per tag (default: 8)" },
            detail_sort_by: { type: "string", enum: ["recency", "name"], description: "Sort order for detail nodes (default: recency)" },
            title: { type: "string", description: "Custom title (default: 'Task Map')" },
            exclude_completed: { type: "boolean", description: "Exclude completed nodes (default: false)" },
            exclude_mentions: { type: "boolean", description: "Exclude @mention tags, only use #hashtags (default: true)" },
            insert_outline: { type: "boolean", description: "Insert outline into Workflowy under Tags node (default: false)" },
            force_outline: { type: "boolean", description: "Overwrite existing outline (default: false)" },
          },
        },
      },
      // ── Graph Analysis Tools ──
      {
        name: "analyze_relationships",
        description: "Extract relationships from data objects and compute graph density. Detects one-to-one and one-to-many relationships from arrays of data objects.",
        inputSchema: {
          type: "object",
          properties: {
            data: {
              type: "array",
              description: "Array of data objects to analyze for relationships",
              items: { type: "object" },
            },
            relationship_fields: {
              type: "array",
              description: "Fields that contain relationship references (e.g. ['parent_id', 'friends'])",
              items: { type: "string" },
            },
            node_label_field: {
              type: "string",
              description: "Field to use as node labels (default: 'id')",
            },
          },
          required: ["data", "relationship_fields"],
        },
      },
      {
        name: "create_adjacency_matrix",
        description: "Build adjacency matrix from explicit relationship pairs and vertex list. Returns the matrix as formatted text.",
        inputSchema: {
          type: "object",
          properties: {
            relationships: {
              type: "array",
              description: "Array of relationship objects with from/to/weight",
              items: {
                type: "object",
                properties: {
                  from: { type: "string" },
                  to: { type: "string" },
                  weight: { type: "number" },
                },
                required: ["from", "to"],
              },
            },
            vertices: {
              type: "array",
              description: "Array of vertex names",
              items: { type: "string" },
            },
          },
          required: ["relationships", "vertices"],
        },
      },
      {
        name: "calculate_centrality",
        description: "Calculate centrality measures (degree, betweenness, closeness, eigenvector) for graph nodes. Identifies the most important/central nodes in a network.",
        inputSchema: {
          type: "object",
          properties: {
            relationships: {
              type: "array",
              description: "Array of relationship objects with from/to/weight",
              items: {
                type: "object",
                properties: {
                  from: { type: "string" },
                  to: { type: "string" },
                  weight: { type: "number" },
                },
                required: ["from", "to"],
              },
            },
            vertices: {
              type: "array",
              description: "Array of vertex names",
              items: { type: "string" },
            },
            measures: {
              type: "array",
              description: "Centrality measures to calculate: degree, betweenness, closeness, eigenvector, all (default: all)",
              items: { type: "string", enum: ["degree", "betweenness", "closeness", "eigenvector", "all"] },
            },
            top_n: {
              type: "number",
              description: "Number of top nodes to return per measure (default: 10)",
            },
          },
          required: ["relationships", "vertices"],
        },
      },
      {
        name: "analyze_network_structure",
        description: "Comprehensive network analysis: extracts relationships from data, builds graph, and optionally computes all centrality measures. Combines analyze_relationships + calculate_centrality in one step.",
        inputSchema: {
          type: "object",
          properties: {
            data: {
              type: "array",
              description: "Array of data objects to analyze",
              items: { type: "object" },
            },
            relationship_fields: {
              type: "array",
              description: "Fields that contain relationship references",
              items: { type: "string" },
            },
            node_label_field: {
              type: "string",
              description: "Field to use as node labels (default: 'id')",
            },
            include_centrality: {
              type: "boolean",
              description: "Whether to include centrality analysis (default: true)",
            },
          },
          required: ["data", "relationship_fields"],
        },
      },
    ],
  };
});

// Tool handlers
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;

  switch (name) {
    case "search_nodes": {
      const { query, tag, assignee, status, root_id, scope, modified_after, modified_before } = searchNodesSchema.parse(args);
      const allNodes = await getCachedNodes();
      const hasFilters = tag || assignee || status || root_id || modified_after || modified_before;

      // Step 1: Scope filter
      let candidates = allNodes;
      if (root_id) {
        const rootNode = allNodes.find((n) => n.id === root_id);
        if (rootNode && scope) {
          candidates = filterNodesByScope(rootNode, allNodes, scope);
        } else if (rootNode) {
          candidates = getSubtreeNodes(root_id, allNodes);
        }
      }

      // Step 2: Text query filter
      const lowerQuery = query.toLowerCase();
      let results = candidates.filter((node) => {
        const nameMatch = node.name?.toLowerCase().includes(lowerQuery);
        const noteMatch = node.note?.toLowerCase().includes(lowerQuery);
        return nameMatch || noteMatch;
      });

      // Step 3: Tag filter
      if (tag) {
        results = results.filter((node) => nodeHasTag(node, tag));
      }

      // Step 4: Assignee filter
      if (assignee) {
        results = results.filter((node) => nodeHasAssignee(node, assignee));
      }

      // Step 5: Status filter
      if (status && status !== "all") {
        results = results.filter((node) => {
          const isCompleted = !!node.completedAt;
          return status === "completed" ? isCompleted : !isCompleted;
        });
      }

      // Step 6: Date range filters
      if (modified_after) {
        const afterTs = new Date(modified_after).getTime();
        results = results.filter((node) => node.modifiedAt && node.modifiedAt > afterTs);
      }
      if (modified_before) {
        const beforeTs = new Date(modified_before).getTime();
        results = results.filter((node) => node.modifiedAt && node.modifiedAt < beforeTs);
      }

      // Return structured JSON when filters are applied, otherwise legacy text format
      if (hasFilters) {
        const nodesWithPaths = buildNodePaths(results);
        const enriched = nodesWithPaths.map((n) => {
          const tags = parseNodeTags(n);
          const dueInfo = parseDueDateFromNode(n);
          return {
            id: n.id,
            name: n.name,
            path: n.path,
            completed: !!n.completedAt,
            tags: tags.tags,
            assignees: tags.assignees,
            due_date: dueInfo ? dueInfo.date.toISOString().split("T")[0] : null,
          };
        });
        const filtersApplied: Record<string, string> = {};
        if (tag) filtersApplied.tag = tag;
        if (assignee) filtersApplied.assignee = assignee;
        if (status) filtersApplied.status = status;
        if (root_id) filtersApplied.root_id = root_id;
        if (modified_after) filtersApplied.modified_after = modified_after;
        if (modified_before) filtersApplied.modified_before = modified_before;

        return {
          content: [{ type: "text", text: JSON.stringify({ count: enriched.length, filters_applied: filtersApplied, results: enriched }, null, 2) }],
        };
      }

      const nodesWithPaths = buildNodePaths(results);
      return {
        content: [{ type: "text", text: formatNodesForSelection(nodesWithPaths) }],
      };
    }

    case "get_node": {
      const { node_id } = getNodeSchema.parse(args);
      const result = await workflowyRequest(`/nodes/${node_id}`);
      return {
        content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
      };
    }

    case "get_children": {
      const { parent_id } = getChildrenSchema.parse(args);
      const endpoint = parent_id ? `/nodes?parent_id=${parent_id}` : "/nodes";
      const result = await workflowyRequest(endpoint);
      return {
        content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
      };
    }

    case "update_node": {
      const { node_id, name: nodeName, note } = updateNodeSchema.parse(args);
      const body: Record<string, unknown> = {};
      if (nodeName !== undefined) body.name = nodeName;
      if (note !== undefined) body.note = note;

      const result = await workflowyRequest(`/nodes/${node_id}`, "POST", body);
      invalidateCache();
      return {
        content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
      };
    }

    case "delete_node": {
      const { node_id } = deleteNodeSchema.parse(args);
      await workflowyRequest(`/nodes/${node_id}`, "DELETE");
      invalidateCache();
      return {
        content: [{ type: "text", text: `Node ${node_id} deleted successfully` }],
      };
    }

    case "move_node": {
      const { node_id, parent_id, position } = moveNodeSchema.parse(args);
      const body: Record<string, unknown> = { parent_id };
      if (position) body.position = position;

      const result = await workflowyRequest(`/nodes/${node_id}`, "POST", body);
      invalidateCache();
      return {
        content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
      };
    }

    case "complete_node": {
      const { node_id } = completeNodeSchema.parse(args);
      await workflowyRequest(`/nodes/${node_id}/complete`, "POST");
      invalidateCache();
      return {
        content: [{ type: "text", text: `Node ${node_id} marked as complete` }],
      };
    }

    case "uncomplete_node": {
      const { node_id } = uncompleteNodeSchema.parse(args);
      await workflowyRequest(`/nodes/${node_id}/uncomplete`, "POST");
      invalidateCache();
      return {
        content: [{ type: "text", text: `Node ${node_id} marked as incomplete` }],
      };
    }

    case "list_todos": {
      const { parent_id, status, query } = listTodosSchema.parse(args);
      const allNodes = await getCachedNodes();

      let todos = allNodes.filter((node) => {
        const isTodo = node.layoutMode === "todo" ||
          node.name?.match(/^- \[(x| )\]/i);
        return isTodo;
      });

      if (parent_id) {
        const isDescendant = (nodeId: string, targetParentId: string): boolean => {
          const node = allNodes.find((n) => n.id === nodeId);
          if (!node) return false;
          if (node.parent_id === targetParentId) return true;
          if (node.parent_id) return isDescendant(node.parent_id, targetParentId);
          return false;
        };
        todos = todos.filter((t) => t.id === parent_id || isDescendant(t.id, parent_id));
      }

      if (status && status !== "all") {
        todos = todos.filter((t) => {
          const isCompleted = t.completedAt !== undefined;
          return status === "completed" ? isCompleted : !isCompleted;
        });
      }

      if (query) {
        const lowerQuery = query.toLowerCase();
        todos = todos.filter((t) =>
          t.name?.toLowerCase().includes(lowerQuery) ||
          t.note?.toLowerCase().includes(lowerQuery)
        );
      }

      const todosWithPaths = buildNodePaths(todos);
      const formatted = todosWithPaths.map((t) => ({
        id: t.id,
        name: t.name,
        note: t.note,
        path: t.path,
        completed: t.completedAt !== undefined,
        completedAt: t.completedAt,
      }));

      return {
        content: [{
          type: "text",
          text: JSON.stringify({ count: formatted.length, todos: formatted }, null, 2),
        }],
      };
    }

    case "find_related": {
      const { node_id, max_results } = findRelatedSchema.parse(args);
      const allNodes = await getCachedNodes();
      const sourceNode = allNodes.find((n) => n.id === node_id);

      if (!sourceNode) {
        return {
          content: [{ type: "text", text: `Error: Node with ID "${node_id}" not found` }],
          isError: true,
        };
      }

      const { keywords, relatedNodes } = await findRelatedNodes(
        sourceNode,
        allNodes,
        max_results || 10
      );

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            source_node: { id: sourceNode.id, name: sourceNode.name },
            keywords_extracted: keywords,
            related_nodes: relatedNodes,
          }, null, 2),
        }],
      };
    }

    case "create_links": {
      const { node_id, link_node_ids, max_links, position } = createLinksSchema.parse(args);
      const allNodes = await getCachedNodes();
      const sourceNode = allNodes.find((n) => n.id === node_id);

      if (!sourceNode) {
        return {
          content: [{ type: "text", text: `Error: Node with ID "${node_id}" not found` }],
          isError: true,
        };
      }

      let nodesToLink: WorkflowyNode[];
      if (link_node_ids && link_node_ids.length > 0) {
        nodesToLink = link_node_ids
          .map((id) => allNodes.find((n) => n.id === id))
          .filter((n): n is WorkflowyNode => n !== undefined);
      } else {
        const { relatedNodes } = await findRelatedNodes(
          sourceNode,
          allNodes,
          max_links || 5
        );
        nodesToLink = relatedNodes
          .map((r) => allNodes.find((n) => n.id === r.id))
          .filter((n): n is WorkflowyNode => n !== undefined);
      }

      if (nodesToLink.length === 0) {
        return {
          content: [{ type: "text", text: "No related nodes found to link to." }],
        };
      }

      const links = nodesToLink.map((n) => generateWorkflowyLink(n.id, n.name || ""));
      const linkText = links.join("\n");

      if (position === "note") {
        const currentNote = sourceNode.note || "";
        const newNote = currentNote
          ? `${currentNote}\n\n---\n🔗 Related:\n${linkText}`
          : `🔗 Related:\n${linkText}`;
        await workflowyRequest(`/nodes/${node_id}`, "POST", { note: newNote });
      } else {
        await workflowyRequest("/nodes", "POST", {
          parent_id: node_id,
          name: "🔗 Related",
          note: linkText,
          priority: 999,
        });
      }

      invalidateCache();
      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: true,
            message: `Created ${links.length} link(s) to related content`,
            position: position || "child",
            links_created: nodesToLink.map((n) => ({ id: n.id, name: n.name })),
          }, null, 2),
        }],
      };
    }

    case "insert_content": {
      const { parent_id, content, position } = insertContentSchema.parse(args);

      // Validate parent exists
      const allNodesForInsert = await getCachedNodes();
      const parentNodeForInsert = allNodesForInsert.find((n) => n.id === parent_id);
      if (!parentNodeForInsert) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: `Parent node not found: ${parent_id}`,
            }, null, 2),
          }],
          isError: true,
        };
      }

      // Content must already be in 2-space indented format
      // Use convert_markdown_to_workflowy first if you have markdown
      const insertResult = await parallelInsertContent(parent_id, content, position);
      invalidateCache();

      return {
        content: [{
          type: "text",
          text: JSON.stringify(insertResult, null, 2),
        }],
        isError: !insertResult.success,
      };
    }

    case "smart_insert": {
      const { search_query, content, selection, position } = smartInsertSchema.parse(args);
      const allNodes = await getCachedNodes();
      const lowerQuery = search_query.toLowerCase();

      const matchingNodes = allNodes.filter((node) => {
        const nameMatch = node.name?.toLowerCase().includes(lowerQuery);
        const noteMatch = node.note?.toLowerCase().includes(lowerQuery);
        return nameMatch || noteMatch;
      });

      if (matchingNodes.length === 0) {
        return {
          content: [{
            type: "text",
            text: `No nodes found matching "${search_query}". Please try a different search term.`,
          }],
        };
      }

      const nodesWithPaths = buildNodePaths(matchingNodes);

      if (matchingNodes.length === 1) {
        const targetNode = matchingNodes[0];
        // Use parallel insertion by default
        const insertResult = await parallelInsertContent(targetNode.id, content, position);
        invalidateCache();
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              ...insertResult,
              target: { id: targetNode.id, name: targetNode.name, path: nodesWithPaths[0].path },
            }, null, 2),
          }],
          isError: !insertResult.success,
        };
      }

      if (selection !== undefined) {
        const index = selection - 1;
        if (index < 0 || index >= matchingNodes.length) {
          return {
            content: [{
              type: "text",
              text: `Invalid selection: ${selection}. Please choose a number between 1 and ${matchingNodes.length}.`,
            }],
          };
        }
        const targetNode = matchingNodes[index];
        // Use parallel insertion by default
        const insertResult = await parallelInsertContent(targetNode.id, content, position);
        invalidateCache();
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              ...insertResult,
              target: { id: targetNode.id, name: targetNode.name, path: nodesWithPaths[index].path },
            }, null, 2),
          }],
          isError: !insertResult.success,
        };
      }

      return {
        content: [{
          type: "text",
          text: `Multiple nodes match "${search_query}". Please select one:\n\n${formatNodesForSelection(nodesWithPaths)}\n\nCall smart_insert again with the selection parameter (1-${matchingNodes.length}) to insert your content.`,
        }],
      };
    }

    case "find_node": {
      const { name: searchName, match_mode, selection } = findNodeSchema.parse(args);
      const allNodes = await getCachedNodes();
      const mode = match_mode || "exact";
      const lowerSearchName = searchName.toLowerCase();

      // Filter nodes based on match mode
      const matchingNodes = allNodes.filter((node) => {
        const nodeName = node.name?.toLowerCase() || "";
        switch (mode) {
          case "exact":
            return nodeName === lowerSearchName;
          case "starts_with":
            return nodeName.startsWith(lowerSearchName);
          case "contains":
            return nodeName.includes(lowerSearchName);
          default:
            return nodeName === lowerSearchName;
        }
      });

      if (matchingNodes.length === 0) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              found: false,
              message: `No node found with name "${searchName}" (match mode: ${mode})`,
              tip: mode === "exact"
                ? "Try match_mode: 'contains' or 'starts_with' for more flexible matching"
                : "Check the exact spelling or try a different search term",
            }, null, 2),
          }],
        };
      }

      const nodesWithPaths = buildNodePaths(matchingNodes);

      // Single match - return directly
      if (matchingNodes.length === 1) {
        const node = matchingNodes[0];
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              found: true,
              node_id: node.id,
              name: node.name,
              path: nodesWithPaths[0].path,
              note: node.note || null,
              message: "Single match found. Use node_id with other tools.",
            }, null, 2),
          }],
        };
      }

      // Multiple matches - check if selection provided
      if (selection !== undefined) {
        const index = selection - 1;
        if (index < 0 || index >= matchingNodes.length) {
          return {
            content: [{
              type: "text",
              text: JSON.stringify({
                found: true,
                error: `Invalid selection: ${selection}. Choose between 1 and ${matchingNodes.length}.`,
                matches: matchingNodes.length,
              }, null, 2),
            }],
          };
        }
        const selectedNode = matchingNodes[index];
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              found: true,
              node_id: selectedNode.id,
              name: selectedNode.name,
              path: nodesWithPaths[index].path,
              note: selectedNode.note || null,
              message: `Selected match ${selection} of ${matchingNodes.length}. Use node_id with other tools.`,
            }, null, 2),
          }],
        };
      }

      // Multiple matches, no selection - ask user to choose
      const options = nodesWithPaths.map((node, i) => ({
        option: i + 1,
        name: node.name,
        path: node.path,
        note_preview: node.note ? node.note.substring(0, 60) + (node.note.length > 60 ? "..." : "") : null,
        id: node.id,
      }));

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            found: true,
            multiple_matches: true,
            count: matchingNodes.length,
            message: `Found ${matchingNodes.length} nodes named "${searchName}". Which one do you mean?`,
            options,
            usage: `Call find_node again with selection: <number> to get the node_id`,
          }, null, 2),
        }],
      };
    }

    // ========================================================================
    // LLM-Powered Concept Map Tools
    // ========================================================================

    case "get_node_content_for_analysis": {
      const {
        node_id,
        depth: maxDepth,
        include_notes = true,
        max_nodes = 500,
        follow_links = true,
        format = "structured",
      } = getNodeContentForAnalysisSchema.parse(args);

      const allNodes = await getCachedNodes();

      // Find the root node
      const rootNode = allNodes.find((n) => n.id === node_id);
      if (!rootNode) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: `Node with ID "${node_id}" not found`,
            }, null, 2),
          }],
          isError: true,
        };
      }

      // Build a map for efficient lookups
      const nodeMap = new Map<string, WorkflowyNode>();
      for (const node of allNodes) {
        nodeMap.set(node.id, node);
      }

      // Collect descendants recursively
      const visitedIds = new Set<string>();
      const contentNodes: AnalysisContentNode[] = [];
      const linkedNodeIds = new Set<string>();

      function collectDescendants(
        nodeId: string,
        currentDepth: number,
        pathParts: string[]
      ): void {
        if (visitedIds.has(nodeId)) return;
        if (maxDepth !== undefined && currentDepth > maxDepth) return;
        if (contentNodes.length >= max_nodes) return;

        visitedIds.add(nodeId);
        const node = nodeMap.get(nodeId);
        if (!node) return;

        const nodeName = node.name || "Untitled";
        const currentPath = [...pathParts, nodeName].join(" > ");

        // Extract links from this node
        const nodeText = `${node.name || ""} ${node.note || ""}`;
        const linksInNode = extractWorkflowyLinks(nodeText);

        // Track linked nodes for later inclusion
        if (follow_links) {
          for (const linkedId of linksInNode) {
            if (!visitedIds.has(linkedId)) {
              linkedNodeIds.add(linkedId);
            }
          }
        }

        const contentNode: AnalysisContentNode = {
          depth: currentDepth,
          id: node.id,
          name: nodeName,
          path: currentPath,
        };

        if (include_notes && node.note) {
          contentNode.note = node.note;
        }

        if (linksInNode.length > 0) {
          contentNode.links_to = linksInNode;
        }

        contentNodes.push(contentNode);

        // Find and process children
        const children = allNodes.filter((n) => n.parent_id === nodeId);
        for (const child of children) {
          collectDescendants(child.id, currentDepth + 1, [...pathParts, nodeName]);
        }
      }

      // Start collection from root's children (not root itself, as that's the context)
      const rootChildren = allNodes.filter((n) => n.parent_id === node_id);
      for (const child of rootChildren) {
        collectDescendants(child.id, 0, [rootNode.name || "Root"]);
      }

      // Process linked nodes that weren't in the initial tree
      const linkedContent: AnalysisContentNode[] = [];
      if (follow_links && linkedNodeIds.size > 0) {
        for (const linkedId of linkedNodeIds) {
          if (visitedIds.has(linkedId)) continue;
          if (linkedContent.length >= 50) break; // Limit linked content

          const linkedNode = nodeMap.get(linkedId);
          if (!linkedNode) continue;

          visitedIds.add(linkedId);

          // Build path for linked node
          const pathParts: string[] = [];
          let current = linkedNode;
          let safety = 0;
          while (current && safety < 20) {
            pathParts.unshift(current.name || "Untitled");
            if (!current.parent_id) break;
            current = nodeMap.get(current.parent_id)!;
            safety++;
          }

          const linkedContentNode: AnalysisContentNode = {
            depth: -1, // Indicates this is linked content, not part of hierarchy
            id: linkedNode.id,
            name: linkedNode.name || "Untitled",
            path: pathParts.join(" > "),
          };

          if (include_notes && linkedNode.note) {
            linkedContentNode.note = linkedNode.note;
          }

          linkedContent.push(linkedContentNode);
        }
      }

      // Calculate total characters
      let totalChars = 0;
      for (const node of contentNodes) {
        totalChars += (node.name || "").length + (node.note || "").length;
      }
      for (const node of linkedContent) {
        totalChars += (node.name || "").length + (node.note || "").length;
      }

      if (format === "outline") {
        // Generate outline format
        const lines: string[] = [];
        lines.push(`# ${rootNode.name || "Root"}`);
        if (rootNode.note) {
          lines.push(`Notes: ${rootNode.note}`);
        }
        lines.push("");

        for (const node of contentNodes) {
          const indent = "  ".repeat(node.depth);
          lines.push(`${indent}- ${node.name}`);
          if (node.note) {
            lines.push(`${indent}  Notes: ${node.note}`);
          }
          if (node.links_to && node.links_to.length > 0) {
            lines.push(`${indent}  [Links to ${node.links_to.length} other node(s)]`);
          }
        }

        if (linkedContent.length > 0) {
          lines.push("");
          lines.push("## Linked Content (referenced but outside hierarchy)");
          for (const node of linkedContent) {
            lines.push(`- ${node.name} (${node.path})`);
            if (node.note) {
              lines.push(`  Notes: ${node.note}`);
            }
          }
        }

        return {
          content: [{
            type: "text",
            text: lines.join("\n"),
          }],
        };
      }

      // Structured JSON format
      const result: AnalysisContentResult = {
        root: {
          id: rootNode.id,
          name: rootNode.name || "Untitled",
          note: rootNode.note,
        },
        total_nodes: contentNodes.length,
        total_chars: totalChars,
        truncated: contentNodes.length >= max_nodes,
        linked_nodes_included: linkedContent.length,
        content: contentNodes,
      };

      // Add linked content if any
      if (linkedContent.length > 0) {
        (result as AnalysisContentResult & { linked_content: AnalysisContentNode[] }).linked_content = linkedContent;
      }

      return {
        content: [{
          type: "text",
          text: JSON.stringify(result, null, 2),
        }],
      };
    }

    case "batch_operations": {
      const { operations, parallel = true } = batchOperationsSchema.parse(args);

      if (operations.length === 0) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: true,
              message: "No operations to execute",
              total: 0,
              succeeded: 0,
              failed: 0,
              results: [],
            }, null, 2),
          }],
        };
      }

      // Start batch cache operation
      startBatch();

      const results: BatchResult[] = [];

      if (parallel) {
        // Execute operations through the request queue with controlled concurrency
        const promises = operations.map(async (op, index) => {
          try {
            const result = await requestQueue.enqueue({
              type: op.type as OperationType,
              params: op.params as Record<string, unknown>,
            });
            return {
              operationId: `op-${index}`,
              status: "fulfilled" as const,
              value: result,
            };
          } catch (error) {
            return {
              operationId: `op-${index}`,
              status: "rejected" as const,
              error: error instanceof Error ? error.message : String(error),
            };
          }
        });

        const settled = await Promise.all(promises);
        results.push(...settled);
      } else {
        // Sequential execution
        for (let i = 0; i < operations.length; i++) {
          const op = operations[i];
          try {
            const result = await requestQueue.enqueue({
              type: op.type as OperationType,
              params: op.params as Record<string, unknown>,
            });
            results.push({
              operationId: `op-${i}`,
              status: "fulfilled",
              value: result,
            });
          } catch (error) {
            results.push({
              operationId: `op-${i}`,
              status: "rejected",
              error: error instanceof Error ? error.message : String(error),
            });
          }
        }
      }

      // End batch and apply cache invalidation
      endBatch();
      invalidateCache();

      const succeeded = results.filter(r => r.status === "fulfilled").length;
      const failed = results.filter(r => r.status === "rejected").length;

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: failed === 0,
            message: failed === 0
              ? `All ${succeeded} operations completed successfully`
              : `${succeeded} succeeded, ${failed} failed`,
            total: operations.length,
            succeeded,
            failed,
            results: results.map((r, i) => ({
              index: i,
              operation: operations[i],
              status: r.status,
              result: r.status === "fulfilled" ? r.value : undefined,
              error: r.status === "rejected" ? r.error : undefined,
            })),
            queue_stats: requestQueue.getStats(),
          }, null, 2),
        }],
      };
    }

    case "analyze_workload": {
      const { content, max_workers = 5 } = analyzeWorkloadSchema.parse(args);

      const splitResult = splitIntoSubtrees(content, {
        maxSubtrees: max_workers,
        targetNodesPerSubtree: 50,
      });

      if (splitResult.totalNodes === 0) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: true,
              message: "No content to analyze",
              total_nodes: 0,
            }, null, 2),
          }],
        };
      }

      const timeSavings = estimateTimeSavings(
        splitResult.totalNodes,
        Math.min(splitResult.recommendedAgents, max_workers),
        5 // requests per second
      );

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: true,
            analysis: {
              total_nodes: splitResult.totalNodes,
              subtree_count: splitResult.subtrees.length,
              recommended_workers: Math.min(splitResult.recommendedAgents, max_workers),
              subtrees: splitResult.subtrees.map((s) => ({
                id: s.id,
                node_count: s.nodeCount,
                root_text: s.rootLine.text.substring(0, 50) + (s.rootLine.text.length > 50 ? "..." : ""),
                estimated_ms: s.estimatedMs,
              })),
            },
            time_estimates: {
              single_agent_ms: timeSavings.singleAgentMs,
              single_agent_seconds: Math.round(timeSavings.singleAgentMs / 100) / 10,
              parallel_ms: timeSavings.parallelMs,
              parallel_seconds: Math.round(timeSavings.parallelMs / 100) / 10,
              savings_percent: timeSavings.savingsPercent,
              savings_seconds: timeSavings.savingsSeconds,
            },
            recommendation: splitResult.totalNodes < 20
              ? "Use insert_content for small workloads (< 20 nodes)"
              : "Use insert_content - it auto-optimizes for any workload size",
          }, null, 2),
        }],
      };
    }

    case "convert_markdown_to_workflowy": {
      const { markdown, options = {}, analyze_only = false } = convertMarkdownToWorkflowySchema.parse(args);

      // If analyze_only, just return stats
      if (analyze_only) {
        const stats = analyzeMarkdown(markdown);
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: true,
              mode: "analyze_only",
              stats: {
                headers: stats.headers,
                list_items: stats.listItems,
                code_blocks: stats.codeBlocks,
                tables: stats.tables,
                blockquotes: stats.blockquotes,
                task_items: stats.taskItems,
                paragraphs: stats.paragraphs,
                original_lines: stats.originalLines,
                estimated_nodes: stats.estimatedNodes,
              },
              recommendation: stats.estimatedNodes > 100
                ? "Large document - consider using insert_content after conversion"
                : "Standard size - can be used directly with insert_content",
            }, null, 2),
          }],
        };
      }

      // Convert the markdown
      const result = convertLargeMarkdownToWorkflowy(markdown, options as ConversionOptions);

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: true,
            content: result.content,
            node_count: result.nodeCount,
            stats: {
              headers: result.stats.headers,
              list_items: result.stats.listItems,
              code_blocks: result.stats.codeBlocks,
              tables: result.stats.tables,
              blockquotes: result.stats.blockquotes,
              task_items: result.stats.taskItems,
              paragraphs: result.stats.paragraphs,
              original_lines: result.stats.originalLines,
              output_lines: result.stats.outputLines,
            },
            warnings: result.warnings.length > 0 ? result.warnings : undefined,
            usage_hint: result.nodeCount > 100
              ? "Large output - use insert_content for best performance"
              : "Ready to use with insert_content or paste directly into Workflowy",
          }, null, 2),
        }],
      };
    }

    // ========================================================================
    // Async Job Queue Handlers
    // ========================================================================

    case "submit_job": {
      const { type, params, description } = submitJobSchema.parse(args);

      // Validate params based on job type
      if (type === "insert_content") {
        const insertParams = params as { parentId?: string; parent_id?: string; content?: string; position?: string };
        const parentId = insertParams.parentId || insertParams.parent_id;

        if (!parentId || !insertParams.content) {
          return {
            content: [{
              type: "text",
              text: JSON.stringify({
                success: false,
                error: "insert_content jobs require parentId (or parent_id) and content parameters",
              }, null, 2),
            }],
            isError: true,
          };
        }

        // Validate parent exists
        const allNodes = await getCachedNodes();
        const parentNode = allNodes.find((n) => n.id === parentId);
        if (!parentNode) {
          return {
            content: [{
              type: "text",
              text: JSON.stringify({
                success: false,
                error: `Parent node not found: ${parentId}`,
              }, null, 2),
            }],
            isError: true,
          };
        }

        // Analyze workload for description
        const lines = insertParams.content.split("\n").filter((line: string) => line.trim());
        const nodeCount = lines.length;
        const jobDescription = description || `Insert ${nodeCount} nodes under "${parentNode.name}"`;

        const job = jobQueue.submit<InsertContentJobParams, InsertContentJobResult>(
          "insert_content",
          {
            parentId,
            content: insertParams.content,
            position: (insertParams.position as "top" | "bottom") || "top",
          },
          jobDescription
        );

        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: true,
              job_id: job.id,
              type: job.type,
              status: job.status,
              description: job.description,
              estimated_nodes: nodeCount,
              message: "Job submitted for background processing. Use get_job_status to check progress.",
              tip: "The server will process this job respecting API rate limits to avoid errors.",
            }, null, 2),
          }],
        };
      }

      if (type === "batch_operations") {
        const batchParams = params as { operations?: Array<{ type: string; params: Record<string, unknown> }> };

        if (!batchParams.operations || !Array.isArray(batchParams.operations)) {
          return {
            content: [{
              type: "text",
              text: JSON.stringify({
                success: false,
                error: "batch_operations jobs require an operations array",
              }, null, 2),
            }],
            isError: true,
          };
        }

        const opCount = batchParams.operations.length;
        const jobDescription = description || `Batch of ${opCount} operations`;

        const job = jobQueue.submit<BatchOperationJobParams, BatchOperationJobResult>(
          "batch_operations",
          {
            operations: batchParams.operations as BatchOperationJobParams["operations"],
          },
          jobDescription
        );

        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: true,
              job_id: job.id,
              type: job.type,
              status: job.status,
              description: job.description,
              operation_count: opCount,
              message: "Job submitted for background processing. Use get_job_status to check progress.",
            }, null, 2),
          }],
        };
      }

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: false,
            error: `Unknown job type: ${type}`,
          }, null, 2),
        }],
        isError: true,
      };
    }

    case "get_job_status": {
      const { job_id } = getJobStatusSchema.parse(args);
      const status = jobQueue.getJobStatus(job_id);

      if (!status.found) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: `Job not found: ${job_id}`,
              tip: "Job IDs expire after 30 minutes. Use list_jobs to see active jobs.",
            }, null, 2),
          }],
          isError: true,
        };
      }

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: true,
            job_id,
            status: status.status,
            progress: status.progress,
            description: status.description,
            created_at: status.createdAt ? new Date(status.createdAt).toISOString() : undefined,
            started_at: status.startedAt ? new Date(status.startedAt).toISOString() : undefined,
            completed_at: status.completedAt ? new Date(status.completedAt).toISOString() : undefined,
            ...(status.error && { error: status.error }),
          }, null, 2),
        }],
      };
    }

    case "get_job_result": {
      const { job_id } = getJobResultSchema.parse(args);
      const result = jobQueue.getJobResult(job_id);

      if (!result.found) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: `Job not found: ${job_id}`,
            }, null, 2),
          }],
          isError: true,
        };
      }

      if (result.status === "pending" || result.status === "processing") {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              job_id,
              status: result.status,
              message: "Job is still running. Use get_job_status to check progress.",
            }, null, 2),
          }],
        };
      }

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: result.status === "completed",
            job_id,
            status: result.status,
            result: result.result,
            error: result.error,
            item_errors: result.itemErrors,
          }, null, 2),
        }],
        isError: result.status === "failed",
      };
    }

    case "list_jobs": {
      const { status } = listJobsSchema.parse(args);
      const jobs = jobQueue.listJobs(status as JobStatus[] | undefined);
      const stats = jobQueue.getStats();

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: true,
            jobs: jobs.map((j) => ({
              job_id: j.id,
              type: j.type,
              status: j.status,
              progress: j.progress,
              description: j.description,
              created_at: new Date(j.createdAt).toISOString(),
            })),
            queue_stats: stats,
          }, null, 2),
        }],
      };
    }

    case "cancel_job": {
      const { job_id } = cancelJobSchema.parse(args);
      const result = jobQueue.cancelJob(job_id);

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: result.success,
            message: result.message,
          }, null, 2),
        }],
        isError: !result.success,
      };
    }

    // ========================================================================
    // File Insertion Handlers
    // ========================================================================

    case "insert_file": {
      const { file_path, parent_id, position, format } = insertFileSchema.parse(args);

      // Validate parent exists
      const allNodes = await getCachedNodes();
      const parentNode = allNodes.find((n) => n.id === parent_id);
      if (!parentNode) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: `Parent node not found: ${parent_id}`,
            }, null, 2),
          }],
          isError: true,
        };
      }

      try {
        // Read and process the file
        const { content, format: actualFormat, fileName, fileSize } = await readAndProcessFile(
          file_path,
          format
        );

        // Count nodes for info
        const lines = content.split("\n").filter((line: string) => line.trim());
        const nodeCount = lines.length;

        // For large files, suggest using submit_file_job
        if (nodeCount > 100) {
          // Still process but warn
          const insertResult = await parallelInsertContent(parent_id, content, position);
          invalidateCache();

          return {
            content: [{
              type: "text",
              text: JSON.stringify({
                ...insertResult,
                file: {
                  name: fileName,
                  size: fileSize,
                  format: actualFormat,
                  node_count: nodeCount,
                },
                tip: nodeCount > 200
                  ? "For very large files, consider using submit_file_job to avoid timeouts"
                  : undefined,
              }, null, 2),
            }],
            isError: !insertResult.success,
          };
        }

        // For smaller files, use parallel insertion
        const insertResult = await parallelInsertContent(parent_id, content, position);
        invalidateCache();

        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              ...insertResult,
              file: {
                name: fileName,
                size: fileSize,
                format: actualFormat,
                node_count: nodeCount,
              },
            }, null, 2),
          }],
          isError: !insertResult.success,
        };
      } catch (error) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: error instanceof Error ? error.message : String(error),
            }, null, 2),
          }],
          isError: true,
        };
      }
    }

    case "submit_file_job": {
      const { file_path, parent_id, position, format, description } = submitFileJobSchema.parse(args);

      // Validate parent exists
      const allNodes = await getCachedNodes();
      const parentNode = allNodes.find((n) => n.id === parent_id);
      if (!parentNode) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: `Parent node not found: ${parent_id}`,
            }, null, 2),
          }],
          isError: true,
        };
      }

      // Validate file exists
      if (!fs.existsSync(file_path)) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: `File not found: ${file_path}`,
            }, null, 2),
          }],
          isError: true,
        };
      }

      const stats = fs.statSync(file_path);
      const fileName = path.basename(file_path);
      const jobDescription = description || `Insert file "${fileName}" under "${parentNode.name}"`;

      const job = jobQueue.submit<InsertFileJobParams, InsertFileJobResult>(
        "insert_file",
        {
          filePath: file_path,
          parentId: parent_id,
          position: position || "top",
          format: format || "auto",
        },
        jobDescription
      );

      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: true,
            job_id: job.id,
            type: job.type,
            status: job.status,
            description: job.description,
            file: {
              name: fileName,
              size: stats.size,
              path: file_path,
            },
            message: "File job submitted for background processing. Use get_job_status to check progress.",
          }, null, 2),
        }],
      };
    }

    // ======== Task & Knowledge Management Handlers ========

    case "get_project_summary": {
      const { node_id, include_tags = true, recently_modified_days = 7 } = getProjectSummarySchema.parse(args);
      const allNodes = await getCachedNodes();
      const subtreeNodes = getSubtreeNodes(node_id, allNodes);

      if (subtreeNodes.length === 0) {
        return { content: [{ type: "text", text: `Node ${node_id} not found` }], isError: true };
      }

      const rootNode = subtreeNodes[0];
      const nodesWithPaths = buildNodePaths([rootNode]);

      // Todo stats
      let todoPending = 0;
      let todoCompleted = 0;
      let overdueCount = 0;
      const now = new Date();
      const tagCounts: Record<string, number> = {};
      const assigneeCounts: Record<string, number> = {};
      let hasDueDates = false;

      const cutoffMs = now.getTime() - recently_modified_days * 24 * 60 * 60 * 1000;
      const recentlyModified: Array<{ id: string; name: string; modifiedAt: number; path: string }> = [];

      for (const node of subtreeNodes) {
        // Count todos
        const isTodo = node.layoutMode === "todo" || /^\[[ x]\]/.test(node.name || "");
        if (isTodo) {
          if (node.completedAt) {
            todoCompleted++;
          } else {
            todoPending++;
          }
        }

        // Due dates & overdue
        const dueInfo = parseDueDateFromNode(node);
        if (dueInfo) {
          hasDueDates = true;
          if (!node.completedAt && isOverdue(node, now)) {
            overdueCount++;
          }
        }

        // Tags & assignees
        if (include_tags) {
          const parsed = parseNodeTags(node);
          for (const t of parsed.tags) {
            tagCounts[`#${t}`] = (tagCounts[`#${t}`] || 0) + 1;
          }
          for (const a of parsed.assignees) {
            assigneeCounts[`@${a}`] = (assigneeCounts[`@${a}`] || 0) + 1;
          }
        }

        // Recently modified
        if (node.modifiedAt && node.modifiedAt > cutoffMs) {
          recentlyModified.push({ id: node.id, name: node.name || "", modifiedAt: node.modifiedAt, path: "" });
        }
      }

      // Build paths for recently modified
      const recentNodes = subtreeNodes.filter((n) => n.modifiedAt && n.modifiedAt > cutoffMs);
      const recentWithPaths = buildNodePaths(recentNodes);
      recentWithPaths.sort((a, b) => (b.modifiedAt || 0) - (a.modifiedAt || 0));
      const recentOutput = recentWithPaths.slice(0, 20).map((n) => ({
        id: n.id, name: n.name, modifiedAt: n.modifiedAt, path: n.path,
      }));

      const todoTotal = todoPending + todoCompleted;
      const summary = {
        root: { id: rootNode.id, name: rootNode.name, path: nodesWithPaths[0]?.path || "" },
        stats: {
          total_nodes: subtreeNodes.length,
          todo_total: todoTotal,
          todo_pending: todoPending,
          todo_completed: todoCompleted,
          completion_percent: todoTotal > 0 ? Math.round((todoCompleted / todoTotal) * 100) : 0,
          has_due_dates: hasDueDates,
          overdue_count: overdueCount,
        },
        tags: include_tags ? tagCounts : undefined,
        assignees: include_tags ? assigneeCounts : undefined,
        recently_modified: recentOutput,
      };

      return { content: [{ type: "text", text: JSON.stringify(summary, null, 2) }] };
    }

    case "get_recent_changes": {
      const { days = 7, root_id, include_completed = true, limit = 50 } = getRecentChangesSchema.parse(args);
      const allNodes = await getCachedNodes();
      const now = new Date();
      const cutoffMs = now.getTime() - days * 24 * 60 * 60 * 1000;
      const since = new Date(cutoffMs).toISOString();

      let candidates = allNodes;
      if (root_id) {
        candidates = getSubtreeNodes(root_id, allNodes);
      }

      let results = candidates.filter((n) => n.modifiedAt && n.modifiedAt > cutoffMs);

      if (!include_completed) {
        results = results.filter((n) => !n.completedAt);
      }

      results.sort((a, b) => (b.modifiedAt || 0) - (a.modifiedAt || 0));
      results = results.slice(0, limit);

      const nodesWithPaths = buildNodePaths(results);
      const changes = nodesWithPaths.map((n) => ({
        id: n.id, name: n.name, path: n.path, modifiedAt: n.modifiedAt, completed: !!n.completedAt,
      }));

      return { content: [{ type: "text", text: JSON.stringify({ since, count: changes.length, changes }, null, 2) }] };
    }

    case "list_upcoming": {
      const { days = 14, root_id, include_no_due_date = false, limit = 50 } = listUpcomingSchema.parse(args);
      const allNodes = await getCachedNodes();
      const now = new Date();
      const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
      const cutoff = new Date(today);
      cutoff.setDate(cutoff.getDate() + days);

      let candidates = allNodes;
      if (root_id) {
        candidates = getSubtreeNodes(root_id, allNodes);
      }

      // Filter to incomplete nodes
      const incomplete = candidates.filter((n) => !n.completedAt);

      const upcoming: Array<{ node: WorkflowyNode; dueDate: Date; daysUntilDue: number; overdue: boolean }> = [];
      const noDueDate: WorkflowyNode[] = [];

      for (const node of incomplete) {
        const dueInfo = parseDueDateFromNode(node);
        if (dueInfo) {
          const daysUntil = Math.floor((dueInfo.date.getTime() - today.getTime()) / (24 * 60 * 60 * 1000));
          if (dueInfo.date <= cutoff) {
            upcoming.push({ node, dueDate: dueInfo.date, daysUntilDue: daysUntil, overdue: daysUntil < 0 });
          }
        } else if (include_no_due_date) {
          noDueDate.push(node);
        }
      }

      // Sort: overdue first (most overdue first), then by nearest due date
      upcoming.sort((a, b) => a.dueDate.getTime() - b.dueDate.getTime());

      let allResults: Array<{ node: WorkflowyNode; due_date: string | null; days_until_due: number | null; overdue: boolean }> = upcoming.map((u) => ({
        node: u.node, due_date: u.dueDate.toISOString().split("T")[0], days_until_due: u.daysUntilDue, overdue: u.overdue,
      }));

      if (include_no_due_date) {
        const noDueMapped = noDueDate.map((n) => ({
          node: n, due_date: null as string | null, days_until_due: null as number | null, overdue: false,
        }));
        allResults = [...allResults, ...noDueMapped];
      }

      allResults = allResults.slice(0, limit);
      const resultNodes = allResults.map((r) => r.node);
      const nodesWithPaths = buildNodePaths(resultNodes);
      const pathMap = new Map(nodesWithPaths.map((n) => [n.id, n.path]));

      const output = allResults.map((r) => ({
        id: r.node.id, name: r.node.name, path: pathMap.get(r.node.id) || "",
        due_date: r.due_date, days_until_due: r.days_until_due, overdue: r.overdue, completed: false,
      }));

      return { content: [{ type: "text", text: JSON.stringify({ as_of: today.toISOString().split("T")[0], count: output.length, upcoming: output }, null, 2) }] };
    }

    case "list_overdue": {
      const { root_id, include_completed = false, limit = 50 } = listOverdueSchema.parse(args);
      const allNodes = await getCachedNodes();
      const now = new Date();
      const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());

      let candidates = allNodes;
      if (root_id) {
        candidates = getSubtreeNodes(root_id, allNodes);
      }

      const overdue: Array<{ node: WorkflowyNode; dueDate: Date; daysOverdue: number }> = [];

      for (const node of candidates) {
        if (!include_completed && node.completedAt) continue;
        const dueInfo = parseDueDateFromNode(node);
        if (dueInfo && dueInfo.date < today) {
          const daysOver = Math.floor((today.getTime() - dueInfo.date.getTime()) / (24 * 60 * 60 * 1000));
          overdue.push({ node, dueDate: dueInfo.date, daysOverdue: daysOver });
        }
      }

      // Most overdue first
      overdue.sort((a, b) => b.daysOverdue - a.daysOverdue);
      const limited = overdue.slice(0, limit);
      const resultNodes = limited.map((o) => o.node);
      const nodesWithPaths = buildNodePaths(resultNodes);
      const pathMap = new Map(nodesWithPaths.map((n) => [n.id, n.path]));

      const output = limited.map((o) => ({
        id: o.node.id, name: o.node.name, path: pathMap.get(o.node.id) || "",
        due_date: o.dueDate.toISOString().split("T")[0], days_overdue: o.daysOverdue, completed: !!o.node.completedAt,
      }));

      return { content: [{ type: "text", text: JSON.stringify({ as_of: today.toISOString().split("T")[0], count: output.length, overdue: output }, null, 2) }] };
    }

    case "find_backlinks": {
      const { node_id, limit = 50 } = findBacklinksSchema.parse(args);
      const allNodes = await getCachedNodes();
      const targetNode = allNodes.find((n) => n.id === node_id);

      if (!targetNode) {
        return { content: [{ type: "text", text: `Node ${node_id} not found` }], isError: true };
      }

      const backlinks: Array<{ node: WorkflowyNode; link_in: "name" | "note" | "both" }> = [];

      for (const node of allNodes) {
        if (node.id === node_id) continue;
        const nameLinks = extractWorkflowyLinks(node.name || "");
        const noteLinks = extractWorkflowyLinks(node.note || "");
        const inName = nameLinks.includes(node_id);
        const inNote = noteLinks.includes(node_id);

        if (inName || inNote) {
          const link_in = inName && inNote ? "both" : inName ? "name" : "note";
          backlinks.push({ node, link_in });
        }
      }

      const limited = backlinks.slice(0, limit);
      const resultNodes = limited.map((b) => b.node);
      const nodesWithPaths = buildNodePaths(resultNodes);
      const pathMap = new Map(nodesWithPaths.map((n) => [n.id, n.path]));

      const output = limited.map((b) => ({
        id: b.node.id, name: b.node.name, path: pathMap.get(b.node.id) || "", link_in: b.link_in,
      }));

      return {
        content: [{ type: "text", text: JSON.stringify({ target: { id: targetNode.id, name: targetNode.name }, count: output.length, backlinks: output }, null, 2) }],
      };
    }

    case "duplicate_node": {
      const { node_id, target_parent_id, position, include_children = true, name_prefix } = duplicateNodeSchema.parse(args);
      const allNodes = await getCachedNodes();

      // Get the subtree to copy
      const subtree = include_children ? getSubtreeNodes(node_id, allNodes) : allNodes.filter((n) => n.id === node_id);
      if (subtree.length === 0) {
        return { content: [{ type: "text", text: `Node ${node_id} not found` }], isError: true };
      }

      // Build parent-child order for sequential creation
      const childrenIndex = buildChildrenIndex(subtree);
      const ordered: WorkflowyNode[] = [];
      const visit = (nodeId: string) => {
        const node = subtree.find((n) => n.id === nodeId);
        if (node) ordered.push(node);
        const children = childrenIndex.get(nodeId) || [];
        for (const child of children) visit(child.id);
      };
      visit(node_id);

      // Create nodes sequentially, mapping old IDs to new IDs
      const idMap = new Map<string, string>();
      let nodesCreated = 0;

      startBatch();
      try {
        for (const node of ordered) {
          const parentId = node.id === node_id ? target_parent_id : idMap.get(node.parent_id || "");
          if (!parentId) continue;

          let nodeName = node.name || "";
          if (node.id === node_id && name_prefix) {
            nodeName = name_prefix + nodeName;
          }

          const body: Record<string, unknown> = { name: nodeName, parent_id: parentId };
          if (node.note) body.description = node.note;
          if (position && node.id === node_id) body.position = position;

          const result = await workflowyRequest("/nodes", "POST", body) as { id?: string };
          if (result?.id) {
            idMap.set(node.id, result.id);
            nodesCreated++;
          }
        }
      } finally {
        endBatch();
        invalidateCache();
      }

      const newRootId = idMap.get(node_id) || "";
      return {
        content: [{ type: "text", text: JSON.stringify({ success: true, original_id: node_id, new_root_id: newRootId, nodes_created: nodesCreated }, null, 2) }],
      };
    }

    case "create_from_template": {
      const { template_node_id, target_parent_id, variables = {}, position } = createFromTemplateSchema.parse(args);
      const allNodes = await getCachedNodes();

      const subtree = getSubtreeNodes(template_node_id, allNodes);
      if (subtree.length === 0) {
        return { content: [{ type: "text", text: `Template node ${template_node_id} not found` }], isError: true };
      }

      // Variable substitution helper
      const substituteVars = (text: string): string => {
        return text.replace(/\{\{(\w+)\}\}/g, (match, key) => {
          return key in variables ? variables[key] : match;
        });
      };

      // Build order and create
      const childrenIndex = buildChildrenIndex(subtree);
      const ordered: WorkflowyNode[] = [];
      const visit = (nodeId: string) => {
        const node = subtree.find((n) => n.id === nodeId);
        if (node) ordered.push(node);
        const children = childrenIndex.get(nodeId) || [];
        for (const child of children) visit(child.id);
      };
      visit(template_node_id);

      const idMap = new Map<string, string>();
      let nodesCreated = 0;

      startBatch();
      try {
        for (const node of ordered) {
          const parentId = node.id === template_node_id ? target_parent_id : idMap.get(node.parent_id || "");
          if (!parentId) continue;

          const body: Record<string, unknown> = {
            name: substituteVars(node.name || ""),
            parent_id: parentId,
          };
          if (node.note) body.description = substituteVars(node.note);
          if (position && node.id === template_node_id) body.position = position;

          const result = await workflowyRequest("/nodes", "POST", body) as { id?: string };
          if (result?.id) {
            idMap.set(node.id, result.id);
            nodesCreated++;
          }
        }
      } finally {
        endBatch();
        invalidateCache();
      }

      const newRootId = idMap.get(template_node_id) || "";
      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            success: true,
            template_id: template_node_id,
            new_root_id: newRootId,
            nodes_created: nodesCreated,
            variables_applied: Object.keys(variables),
          }, null, 2),
        }],
      };
    }

    case "bulk_update": {
      const { filter, operation, dry_run = false, limit = 20 } = bulkUpdateSchema.parse(args);
      const allNodes = await getCachedNodes();

      // Apply filter pipeline (same as enhanced search_nodes)
      let candidates = allNodes;
      if (filter.root_id) {
        const rootNode = allNodes.find((n) => n.id === filter.root_id);
        if (rootNode && filter.scope) {
          candidates = filterNodesByScope(rootNode, allNodes, filter.scope);
        } else if (filter.root_id) {
          candidates = getSubtreeNodes(filter.root_id, allNodes);
        }
      }

      if (filter.query) {
        const lq = filter.query.toLowerCase();
        candidates = candidates.filter((n) => n.name?.toLowerCase().includes(lq) || n.note?.toLowerCase().includes(lq));
      }
      if (filter.tag) {
        candidates = candidates.filter((n) => nodeHasTag(n, filter.tag!));
      }
      if (filter.assignee) {
        candidates = candidates.filter((n) => nodeHasAssignee(n, filter.assignee!));
      }
      if (filter.status && filter.status !== "all") {
        candidates = candidates.filter((n) => {
          const isCompleted = !!n.completedAt;
          return filter.status === "completed" ? isCompleted : !isCompleted;
        });
      }

      const matchedCount = candidates.length;

      if (matchedCount > limit) {
        return {
          content: [{ type: "text", text: JSON.stringify({
            error: `Matched ${matchedCount} nodes which exceeds limit of ${limit}. Increase limit or narrow your filter.`,
            matched_count: matchedCount,
            limit,
          }, null, 2) }],
          isError: true,
        };
      }

      if (dry_run) {
        const nodesWithPaths = buildNodePaths(candidates);
        const preview = nodesWithPaths.map((n) => ({ id: n.id, name: n.name, path: n.path }));
        return {
          content: [{ type: "text", text: JSON.stringify({
            dry_run: true, matched_count: matchedCount, operation: operation.type, nodes_matched: preview,
          }, null, 2) }],
        };
      }

      // Execute operation
      const affected: Array<{ id: string; name: string }> = [];
      startBatch();
      try {
        for (const node of candidates) {
          switch (operation.type) {
            case "complete":
              await workflowyRequest(`/nodes/${node.id}/complete`, "POST");
              break;
            case "uncomplete":
              await workflowyRequest(`/nodes/${node.id}/uncomplete`, "POST");
              break;
            case "add_tag": {
              const tagToAdd = operation.tag.replace(/^#/, "");
              const newName = `${node.name || ""} #${tagToAdd}`;
              await workflowyRequest(`/nodes/${node.id}`, "POST", { name: newName });
              break;
            }
            case "remove_tag": {
              const tagToRemove = operation.tag.replace(/^#/, "");
              const tagRegex = new RegExp(`\\s*#${tagToRemove}\\b`, "gi");
              const cleanedName = (node.name || "").replace(tagRegex, "");
              const cleanedNote = (node.note || "").replace(tagRegex, "");
              const body: Record<string, unknown> = { name: cleanedName };
              if (node.note) body.description = cleanedNote;
              await workflowyRequest(`/nodes/${node.id}`, "POST", body);
              break;
            }
            case "move": {
              const moveBody: Record<string, unknown> = { parent_id: operation.target_parent_id };
              if (operation.position) moveBody.position = operation.position;
              await workflowyRequest(`/nodes/${node.id}`, "POST", moveBody);
              break;
            }
            case "delete":
              await workflowyRequest(`/nodes/${node.id}`, "DELETE");
              break;
          }
          affected.push({ id: node.id, name: node.name || "" });
        }
      } finally {
        endBatch();
        invalidateCache();
      }

      const nodesWithPaths = buildNodePaths(candidates);
      const affectedOutput = nodesWithPaths.map((n) => ({ id: n.id, name: n.name, path: n.path }));

      return {
        content: [{ type: "text", text: JSON.stringify({
          dry_run: false, matched_count: matchedCount, affected_count: affected.length,
          operation: operation.type, nodes_affected: affectedOutput,
        }, null, 2) }],
      };
    }

    case "daily_review": {
      const { root_id, overdue_limit = 10, upcoming_days = 7, recent_days = 1, pending_limit = 20 } = dailyReviewSchema.parse(args);
      const allNodes = await getCachedNodes();
      const now = new Date();
      const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());

      let candidates = allNodes;
      if (root_id) {
        candidates = getSubtreeNodes(root_id, allNodes);
      }

      // Gather stats in a single pass
      let pendingTodos = 0;
      let overdueCount = 0;
      let dueTodayCount = 0;
      const recentCutoffMs = now.getTime() - recent_days * 24 * 60 * 60 * 1000;
      let modifiedTodayCount = 0;

      const overdueItems: Array<{ node: WorkflowyNode; dueDate: Date; daysOverdue: number }> = [];
      const upcomingItems: Array<{ node: WorkflowyNode; dueDate: Date; daysUntilDue: number }> = [];
      const recentChanges: WorkflowyNode[] = [];
      const pendingNodes: WorkflowyNode[] = [];

      const cutoffDate = new Date(today);
      cutoffDate.setDate(cutoffDate.getDate() + upcoming_days);

      for (const node of candidates) {
        const isIncomplete = !node.completedAt;
        const dueInfo = parseDueDateFromNode(node);

        // Pending todos
        if (isIncomplete) {
          const isTodo = node.layoutMode === "todo" || /^\[[ x]\]/.test(node.name || "");
          if (isTodo) {
            pendingTodos++;
            pendingNodes.push(node);
          }
        }

        // Due date analysis
        if (dueInfo && isIncomplete) {
          const daysUntil = Math.floor((dueInfo.date.getTime() - today.getTime()) / (24 * 60 * 60 * 1000));
          if (daysUntil < 0) {
            overdueCount++;
            overdueItems.push({ node, dueDate: dueInfo.date, daysOverdue: -daysUntil });
          } else if (daysUntil === 0) {
            dueTodayCount++;
            upcomingItems.push({ node, dueDate: dueInfo.date, daysUntilDue: 0 });
          } else if (dueInfo.date <= cutoffDate) {
            upcomingItems.push({ node, dueDate: dueInfo.date, daysUntilDue: daysUntil });
          }
        }

        // Recent changes
        if (node.modifiedAt && node.modifiedAt > recentCutoffMs) {
          modifiedTodayCount++;
          recentChanges.push(node);
        }
      }

      // Sort and limit
      overdueItems.sort((a, b) => b.daysOverdue - a.daysOverdue);
      upcomingItems.sort((a, b) => a.dueDate.getTime() - b.dueDate.getTime());
      recentChanges.sort((a, b) => (b.modifiedAt || 0) - (a.modifiedAt || 0));

      const limitedOverdue = overdueItems.slice(0, overdue_limit);
      const limitedUpcoming = upcomingItems.slice(0, 20);
      const limitedRecent = recentChanges.slice(0, 20);
      const limitedPending = pendingNodes.slice(0, pending_limit);

      // Build paths
      const allResultNodes = [
        ...limitedOverdue.map((o) => o.node),
        ...limitedUpcoming.map((u) => u.node),
        ...limitedRecent,
        ...limitedPending,
      ];
      const nodesWithPaths = buildNodePaths(allResultNodes);
      const pathMap = new Map(nodesWithPaths.map((n) => [n.id, n.path]));

      const review = {
        as_of: today.toISOString().split("T")[0],
        summary: {
          total_nodes: candidates.length,
          pending_todos: pendingTodos,
          overdue_count: overdueCount,
          due_today: dueTodayCount,
          modified_today: modifiedTodayCount,
        },
        overdue: limitedOverdue.map((o) => ({
          id: o.node.id, name: o.node.name, path: pathMap.get(o.node.id) || "",
          due_date: o.dueDate.toISOString().split("T")[0], days_overdue: o.daysOverdue,
        })),
        due_soon: limitedUpcoming.map((u) => ({
          id: u.node.id, name: u.node.name, path: pathMap.get(u.node.id) || "",
          due_date: u.dueDate.toISOString().split("T")[0], days_until_due: u.daysUntilDue,
        })),
        recent_changes: limitedRecent.map((n) => ({
          id: n.id, name: n.name, path: pathMap.get(n.id) || "",
          modifiedAt: n.modifiedAt, completed: !!n.completedAt,
        })),
        top_pending: limitedPending.map((n) => ({
          id: n.id, name: n.name, path: pathMap.get(n.id) || "",
        })),
      };

      return { content: [{ type: "text", text: JSON.stringify(review, null, 2) }] };
    }

    case "render_interactive_concept_map": {
      const parsed = renderInteractiveConceptMapSchema.parse(args);
      const coreId = "core";
      const coreNode = {
        id: coreId,
        label: parsed.core_concept.label,
        workflowyNodeId: parsed.core_concept.workflowy_node_id,
      };

      // Build concept list with auto-assignment of unparented details
      const majors = parsed.concepts.filter((c) => c.level === "major");
      const concepts = parsed.concepts.map((c) => {
        const concept = {
          id: c.id,
          label: c.label,
          level: c.level as "major" | "detail",
          importance: c.importance ?? (c.level === "major" ? 6 : 3),
          parentMajorId: c.parent_major_id,
          workflowyNodeId: c.workflowy_node_id,
        };

        // Auto-assign detail concepts without a parent to their most-connected major
        if (concept.level === "detail" && !concept.parentMajorId && majors.length > 0) {
          const connectionCounts = new Map<string, number>();
          for (const rel of parsed.relationships) {
            if (rel.from === c.id || rel.to === c.id) {
              const otherId = rel.from === c.id ? rel.to : rel.from;
              const otherMajor = majors.find((m) => m.id === otherId);
              if (otherMajor) {
                connectionCounts.set(otherMajor.id, (connectionCounts.get(otherMajor.id) || 0) + (rel.strength || 5));
              }
            }
          }
          if (connectionCounts.size > 0) {
            let bestId = majors[0].id;
            let bestScore = 0;
            for (const [id, score] of connectionCounts) {
              if (score > bestScore) { bestId = id; bestScore = score; }
            }
            concept.parentMajorId = bestId;
          } else {
            // Fallback: assign to first major concept
            concept.parentMajorId = majors[0].id;
          }
        }

        return concept;
      });

      const relationships = parsed.relationships.map((r) => ({
        from: r.from,
        to: r.to,
        type: r.type,
        strength: r.strength ?? 5,
      }));

      const html = generateInteractiveConceptMapHTML(parsed.title, coreNode, concepts, relationships);
      lastInteractiveMapHTML = html;
      lastInteractiveMapTitle = parsed.title;

      // Save HTML to ~/Downloads/ as fallback (always available regardless of MCP Apps support)
      const timestamp = Date.now();
      const slug = parsed.title.toLowerCase().replace(/[^a-z0-9]+/g, "-").slice(0, 40);
      const downloadsDir = path.join(process.env.HOME || "~", "Downloads");
      const filePath = path.join(downloadsDir, `concept-map-${slug}-${timestamp}.html`);
      try {
        fs.writeFileSync(filePath, html);
      } catch {
        // Fallback write failure is non-fatal
      }

      const stats = {
        success: true,
        title: parsed.title,
        file_path: filePath,
        stats: {
          major_concepts: majors.length,
          detail_concepts: concepts.length - majors.length,
          relationships: relationships.length,
        },
        instructions: "The concept map HTML file is self-contained (no server needed). Open it in any browser for an interactive force-directed graph. Click major concepts to expand details, drag nodes to rearrange, scroll to zoom, drag background to pan.",
      };

      return { content: [{ type: "text", text: JSON.stringify(stats, null, 2) }] };
    }

    // ── Task Map ──

    case "generate_task_map": {
      const parsed = generateTaskMapSchema.parse(args);
      const allNodes = await getCachedNodes();

      const taskMapData = generateTaskMap(allNodes, {
        maxDetailsPerTag: parsed.max_details_per_tag,
        detailSortBy: parsed.detail_sort_by,
        title: parsed.title,
        excludeCompleted: parsed.exclude_completed,
        excludeMentions: parsed.exclude_mentions,
      });

      const coreNode = {
        id: "core",
        label: taskMapData.title,
        workflowyNodeId: taskMapData.tagsNode.id,
      };

      const html = generateInteractiveConceptMapHTML(
        taskMapData.title,
        coreNode,
        taskMapData.concepts,
        taskMapData.relationships,
        { showLegend: false }
      );

      lastInteractiveMapHTML = html;
      lastInteractiveMapTitle = taskMapData.title;

      const timestamp = Date.now();
      const dateStr = new Date().toISOString().slice(0, 10);
      const slug = taskMapData.title.toLowerCase().replace(/[^a-z0-9]+/g, "-").slice(0, 40);
      const downloadsDir = path.join(process.env.HOME || "~", "Downloads");
      const filePath = path.join(downloadsDir, `task-map-${slug}-${timestamp}.html`);
      try {
        fs.writeFileSync(filePath, html);
      } catch {
        // Fallback write failure is non-fatal
      }

      // Upload to Dropbox
      let dropboxUrl: string | undefined;
      if (isDropboxConfigured()) {
        const dropboxFilename = `task-map-${dateStr}.html`;
        const dropboxResult = await uploadToDropboxPath(html, `/Workflowy/TaskMaps/${dropboxFilename}`);
        if (dropboxResult.success && dropboxResult.url) {
          dropboxUrl = dropboxResult.url;
        }
      }

      // Add link node under Tasks node (falls back to Tags if no Tasks node)
      if (dropboxUrl) {
        const linkParentId = taskMapData.tasksNode?.id || taskMapData.tagsNode.id;
        await workflowyRequest("/nodes", "POST", {
          name: `Task Map ${dateStr}`,
          note: dropboxUrl,
          parent_id: linkParentId,
          position: "bottom",
        });
      }

      const result: Record<string, unknown> = {
        success: true,
        title: taskMapData.title,
        file_path: filePath,
        dropbox_url: dropboxUrl,
        tags_node_id: taskMapData.tagsNode.id,
        tag_count: taskMapData.tagDefinitions.length,
        tags: taskMapData.tagDefinitions.map(t => ({
          label: t.raw,
          type: t.type,
          matched_nodes: taskMapData.taggedNodes.filter(
            tn => tn.matchedTags.some(mt => mt.normalized === t.normalized)
          ).length,
        })),
        stats: {
          major_concepts: taskMapData.concepts.filter(c => c.level === "major").length,
          detail_concepts: taskMapData.concepts.filter(c => c.level === "detail").length,
          relationships: taskMapData.relationships.length,
          total_tagged_nodes: taskMapData.taggedNodes.length,
        },
        instructions: "Task map HTML saved. Open in any browser for interactive visualization. Click tags to expand matching nodes, drag to rearrange, scroll to zoom.",
      };

      if (parsed.insert_outline) {
        const nodeIdMap = new Map<string, string>();
        for (const n of allNodes) {
          if (n.name) nodeIdMap.set(n.name.toLowerCase(), n.id);
        }
        const outlineResult = await insertConceptMapOutline(
          taskMapData.analysis,
          taskMapData.tagsNode,
          allNodes,
          undefined,
          nodeIdMap,
          !!parsed.force_outline
        );
        result.outline = {
          outline_node_id: outlineResult.outlineNodeId,
          nodes_created: outlineResult.nodesCreated,
        };
      }

      return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
    }

    // ── Graph Analysis Tools ──

    case "analyze_relationships": {
      const parsed = analyzeRelationshipsSchema.parse(args);
      const result = extractRelationshipsFromData(
        parsed.data as Array<Record<string, unknown>>,
        parsed.relationship_fields,
        parsed.node_label_field
      );

      let text = `Relationship Analysis Results\n\n`;
      text += `Vertices found: ${result.vertices.length}\n`;
      text += `Relationships found: ${result.relationships.length}\n`;
      text += `Graph density: ${result.density.toFixed(3)}\n\n`;
      text += `Vertices: ${result.vertices.join(", ")}\n\n`;
      text += `Relationships:\n`;
      for (const r of result.relationships) {
        text += `  ${r.from} -> ${r.to} (weight: ${r.weight})\n`;
      }

      return { content: [{ type: "text", text }] };
    }

    case "create_adjacency_matrix": {
      const parsed = createAdjacencyMatrixSchema.parse(args);
      const graph = buildGraphStructure(parsed.relationships as GraphEdge[], parsed.vertices);

      let text = `Adjacency Matrix (${parsed.vertices.length}x${parsed.vertices.length})\n\n`;
      // Header row
      text += `${"".padEnd(15)} ${parsed.vertices.map((v) => v.padEnd(8)).join(" ")}\n`;
      for (const from of parsed.vertices) {
        text += `${from.padEnd(15)} ${parsed.vertices.map((to) => String(graph.adjacencyMatrix[from][to]).padEnd(8)).join(" ")}\n`;
      }
      text += `\nVertices: ${parsed.vertices.length}\n`;
      text += `Edges: ${parsed.relationships.length}\n`;

      return { content: [{ type: "text", text }] };
    }

    case "calculate_centrality": {
      const parsed = calculateCentralitySchema.parse(args);
      const graph = buildGraphStructure(parsed.relationships as GraphEdge[], parsed.vertices);

      const requestedMeasures = parsed.measures.includes("all")
        ? ["degree", "betweenness", "closeness", "eigenvector"]
        : parsed.measures;

      const results: Record<string, Record<string, number>> = {};
      for (const measure of requestedMeasures) {
        switch (measure) {
          case "degree":
            results.degree = calculateDegreeCentrality(graph);
            break;
          case "betweenness":
            results.betweenness = calculateBetweennessCentrality(graph);
            break;
          case "closeness":
            results.closeness = calculateClosenessCentrality(graph);
            break;
          case "eigenvector":
            results.eigenvector = calculateEigenvectorCentrality(graph);
            break;
        }
      }

      let text = formatCentralityResults(results, parsed.top_n);
      text += `Analysis Summary\n`;
      text += `- Vertices analyzed: ${parsed.vertices.length}\n`;
      text += `- Relationships: ${parsed.relationships.length}\n`;
      text += `- Measures calculated: ${Object.keys(results).join(", ")}\n`;

      return { content: [{ type: "text", text }] };
    }

    case "analyze_network_structure": {
      const parsed = analyzeNetworkStructureSchema.parse(args);

      // Extract relationships from data
      const extraction = extractRelationshipsFromData(
        parsed.data as Array<Record<string, unknown>>,
        parsed.relationship_fields,
        parsed.node_label_field
      );

      let text = `Network Structure Analysis\n\n`;
      text += `Vertices found: ${extraction.vertices.length}\n`;
      text += `Relationships found: ${extraction.relationships.length}\n`;
      text += `Graph density: ${extraction.density.toFixed(3)}\n\n`;
      text += `Relationships:\n`;
      for (const r of extraction.relationships) {
        text += `  ${r.from} -> ${r.to} (weight: ${r.weight})\n`;
      }

      if (parsed.include_centrality && extraction.vertices.length > 0 && extraction.relationships.length > 0) {
        const graph = buildGraphStructure(extraction.relationships, extraction.vertices);
        const results: Record<string, Record<string, number>> = {
          degree: calculateDegreeCentrality(graph),
          betweenness: calculateBetweennessCentrality(graph),
          closeness: calculateClosenessCentrality(graph),
          eigenvector: calculateEigenvectorCentrality(graph),
        };

        text += `\n${formatCentralityResults(results, 5)}`;
      }

      return { content: [{ type: "text", text }] };
    }

    default:
      return {
        content: [{ type: "text", text: `Unknown tool: ${name}` }],
        isError: true,
      };
  }
});

// ============================================================================
// MCP Resource Handlers (for interactive concept map UI)
// ============================================================================

server.setRequestHandler(ListResourcesRequestSchema, async () => ({
  resources: lastInteractiveMapHTML
    ? [
        {
          uri: "ui://concept-map/interactive",
          name: lastInteractiveMapTitle || "Interactive Concept Map",
          mimeType: "text/html;profile=mcp-app",
        },
      ]
    : [],
}));

server.setRequestHandler(ReadResourceRequestSchema, async (request) => {
  if (request.params.uri === "ui://concept-map/interactive" && lastInteractiveMapHTML) {
    return {
      contents: [
        {
          uri: request.params.uri,
          mimeType: "text/html;profile=mcp-app",
          text: lastInteractiveMapHTML,
        },
      ],
    };
  }
  return { contents: [] };
});

// Start server
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error("Workflowy MCP server running on stdio");
}

main().catch((error) => {
  console.error("Server error:", error);
  process.exit(1);
});
