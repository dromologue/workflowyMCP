/**
 * Workflowy MCP Server
 * Main entry point - wires up modules and handles tool requests
 */

import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import * as path from "path";
import * as fs from "fs";
import { Graphviz } from "@hpcc-js/wasm-graphviz";
import sharp from "sharp";

// Import from modules
import { validateConfig } from "./config/environment.js";
import { workflowyRequest } from "./api/workflowy.js";
import { uploadToDropbox } from "./api/dropbox.js";
import type {
  WorkflowyNode,
  NodeWithPath,
  RelatedNode,
  ConceptMapScope,
  ConceptMapNode,
  ConceptMapEdge,
  CreatedNode,
} from "./types/index.js";
import {
  getCachedNodesIfValid,
  updateCache,
  invalidateCache,
} from "./utils/cache.js";
import { buildNodePaths } from "./utils/node-paths.js";
import {
  parseIndentedContent,
  formatNodesForSelection,
  escapeForDot,
  generateWorkflowyLink,
} from "./utils/text-processing.js";
import {
  extractKeywords,
  calculateRelevance,
  findMatchedKeywords,
} from "./utils/keyword-extraction.js";

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

function filterNodesByScope(
  sourceNode: WorkflowyNode,
  allNodes: WorkflowyNode[],
  scope: ConceptMapScope
): WorkflowyNode[] {
  if (!Array.isArray(allNodes)) {
    return [];
  }

  switch (scope) {
    case "this_node":
      return [];

    case "children": {
      const childIds = new Set<string>();
      const findChildren = (parentId: string, depth: number = 0) => {
        if (depth > 100) return;
        for (const node of allNodes) {
          if (node.parent_id === parentId && !childIds.has(node.id)) {
            childIds.add(node.id);
            findChildren(node.id, depth + 1);
          }
        }
      };
      findChildren(sourceNode.id);
      return allNodes.filter((n) => childIds.has(n.id));
    }

    case "siblings": {
      if (!sourceNode.parent_id) {
        return allNodes.filter((n) => !n.parent_id && n.id !== sourceNode.id);
      }
      return allNodes.filter(
        (n) => n.parent_id === sourceNode.parent_id && n.id !== sourceNode.id
      );
    }

    case "ancestors": {
      const ancestorIds = new Set<string>();
      let currentId = sourceNode.parent_id;
      let depth = 0;
      while (currentId && depth < 100) {
        ancestorIds.add(currentId);
        const parent = allNodes.find((n) => n.id === currentId);
        currentId = parent?.parent_id;
        depth++;
      }
      return allNodes.filter((n) => ancestorIds.has(n.id));
    }

    case "all":
    default:
      return allNodes.filter((n) => n.id !== sourceNode.id);
  }
}

async function findRelatedNodes(
  sourceNode: WorkflowyNode,
  allNodes: WorkflowyNode[],
  maxResults: number = 10
): Promise<{ keywords: string[]; relatedNodes: RelatedNode[] }> {
  const sourceText = `${sourceNode.name || ""} ${sourceNode.note || ""}`;
  const keywords = extractKeywords(sourceText);

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
// Concept Map Generation
// ============================================================================

function generateDotGraph(
  centerNode: ConceptMapNode,
  relatedNodes: Array<{ node: ConceptMapNode; keywords: string[]; weight: number }>,
  title: string
): string {
  const lines: string[] = [
    "digraph ConceptMap {",
    '  rankdir=LR;',
    '  bgcolor="white";',
    `  label="${escapeForDot(title)}";`,
    '  labelloc="t";',
    '  fontsize=24;',
    '  fontname="Arial";',
    "",
    "  // Node styling",
    '  node [shape=box, style="rounded,filled", fontname="Arial", fontsize=12];',
    "",
    "  // Center node",
    `  "${centerNode.id}" [label="${escapeForDot(centerNode.label)}", fillcolor="#4A90D9", fontcolor="white", penwidth=2];`,
    "",
    "  // Related nodes",
  ];

  const colors = ["#7CB342", "#F9A825", "#EF6C00", "#AB47BC", "#26A69A"];

  relatedNodes.forEach((item, index) => {
    const color = colors[index % colors.length];
    lines.push(
      `  "${item.node.id}" [label="${escapeForDot(item.node.label)}", fillcolor="${color}", fontcolor="white"];`
    );
  });

  lines.push("");
  lines.push("  // Edges");

  relatedNodes.forEach((item) => {
    const keywordLabel = item.keywords.slice(0, 2).join(", ");
    const penwidth = Math.min(1 + item.weight / 3, 4);
    lines.push(
      `  "${centerNode.id}" -> "${item.node.id}" [label="${escapeForDot(keywordLabel)}", penwidth=${penwidth}, color="#666666"];`
    );
  });

  lines.push("}");
  return lines.join("\n");
}

async function generateConceptMapImage(
  centerNode: { id: string; name: string },
  relatedNodes: RelatedNode[],
  title: string,
  format: "png" | "jpeg" = "png"
): Promise<{ success: boolean; buffer?: Buffer; error?: string }> {
  try {
    const graphviz = await Graphviz.load();

    const center: ConceptMapNode = {
      id: centerNode.id,
      label: centerNode.name || "Center",
      isCenter: true,
    };

    const related = relatedNodes.map((n) => ({
      node: { id: n.id, label: n.name || "Node", isCenter: false },
      keywords: n.matchedKeywords,
      weight: n.relevanceScore,
    }));

    const dotGraph = generateDotGraph(center, related, title);
    const svg = graphviz.dot(dotGraph, "svg");

    const imageBuffer = await sharp(Buffer.from(svg), { density: 300 })
      .resize(2400, null, {
        fit: "inside",
        withoutEnlargement: false,
      })
      .flatten({ background: "#ffffff" })
      [format]({
        quality: format === "jpeg" ? 95 : undefined,
      })
      .toBuffer();

    return { success: true, buffer: imageBuffer };
  } catch (err) {
    return {
      success: false,
      error: `Failed to generate concept map: ${err instanceof Error ? err.message : String(err)}`,
    };
  }
}

// ============================================================================
// Hierarchical Content Insertion
// ============================================================================

async function insertHierarchicalContent(
  rootParentId: string,
  content: string,
  position?: "top" | "bottom"
): Promise<CreatedNode[]> {
  const parsedLines = parseIndentedContent(content);
  const createdNodes: CreatedNode[] = [];
  const parentStack: string[] = [rootParentId];
  let firstTopLevelInserted = false;

  for (const line of parsedLines) {
    const parentIndex = Math.min(line.indent, parentStack.length - 1);
    const parentId = parentStack[parentIndex];

    let nodePosition: "top" | "bottom" = "bottom";
    if (position === "top" && line.indent === 0 && !firstTopLevelInserted) {
      nodePosition = "top";
      firstTopLevelInserted = true;
    }

    const body: Record<string, unknown> = {
      name: line.text,
      parent_id: parentId,
      position: nodePosition,
    };

    const result = (await workflowyRequest("/nodes", "POST", body)) as CreatedNode;
    createdNodes.push(result);

    parentStack[line.indent + 1] = result.id;
    parentStack.length = line.indent + 2;
  }

  return createdNodes;
}

// ============================================================================
// Zod Schemas
// ============================================================================

const searchNodesSchema = z.object({
  query: z.string().describe("Text to search for in node names and notes"),
});

const getNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to retrieve"),
});

const getChildrenSchema = z.object({
  parent_id: z.string().optional().describe("Parent node ID. Omit to get root-level nodes"),
});

const createNodeSchema = z.object({
  name: z.string().describe("The text content of the new node"),
  note: z.string().optional().describe("Optional note for the node"),
  parent_id: z.string().optional().describe('Parent node ID, target key (e.g., "inbox"), or omit for root level'),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings (default: top)"),
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

const createTodoSchema = z.object({
  name: z.string().describe("The text content of the todo item"),
  note: z.string().optional().describe("Optional note for the todo"),
  parent_id: z.string().optional().describe('Parent node ID, target key (e.g., "inbox"), or omit for root level'),
  completed: z.boolean().optional().describe("Whether the todo starts as completed (default: false)"),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings (default: bottom)"),
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

const generateConceptMapSchema = z.object({
  node_id: z.string().describe("The ID of the center node for the concept map"),
  scope: z.enum(["this_node", "children", "siblings", "ancestors", "all"]).optional().describe("Search scope for related nodes (default: 'all')"),
  max_related: z.number().optional().describe("Maximum number of related nodes to include (default: 15)"),
  output_path: z.string().optional().describe("Output file path. Defaults to ~/Downloads/concept-map-{timestamp}.png"),
  format: z.enum(["png", "jpeg"]).optional().describe("Image format (default: png)"),
  title: z.string().optional().describe("Title for the concept map (defaults to node name)"),
});

const insertContentSchema = z.object({
  parent_id: z.string().describe("The ID of the parent node to insert content under"),
  content: z.string().describe("The content to insert (can be multiline)"),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings (default: top)"),
});

const findInsertTargetsSchema = z.object({
  query: z.string().describe("Search text to find potential target nodes"),
});

const smartInsertSchema = z.object({
  search_query: z.string().describe("Search text to find the target node for insertion"),
  content: z.string().describe("The content to insert"),
  selection: z.number().optional().describe("If multiple matches found, the number (1-based) of the node to use"),
  position: z.enum(["top", "bottom"]).optional().describe("Position relative to siblings (default: top)"),
});

// ============================================================================
// MCP Server Setup
// ============================================================================

const server = new Server(
  { name: "workflowy-mcp-server", version: "1.0.0" },
  { capabilities: { tools: {} } }
);

// Tool definitions
server.setRequestHandler(ListToolsRequestSchema, async () => {
  return {
    tools: [
      {
        name: "search_nodes",
        description: "Search for nodes in Workflowy by text. Returns all nodes matching the query in their name or note, with full paths for identification.",
        inputSchema: {
          type: "object",
          properties: {
            query: { type: "string", description: "Text to search for in node names and notes" },
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
        name: "create_node",
        description: "Create a new node in Workflowy.",
        inputSchema: {
          type: "object",
          properties: {
            name: { type: "string", description: "The text content of the new node" },
            note: { type: "string", description: "Optional note for the node" },
            parent_id: { type: "string", description: "Parent node ID or omit for root level" },
            position: { type: "string", enum: ["top", "bottom"], description: "Position relative to siblings" },
          },
          required: ["name"],
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
        name: "create_todo",
        description: "Create a new todo item with a checkbox.",
        inputSchema: {
          type: "object",
          properties: {
            name: { type: "string", description: "The text content of the todo item" },
            note: { type: "string", description: "Optional note for the todo" },
            parent_id: { type: "string", description: "Parent node ID or omit for root level" },
            completed: { type: "boolean", description: "Whether the todo starts as completed" },
            position: { type: "string", enum: ["top", "bottom"], description: "Position relative to siblings" },
          },
          required: ["name"],
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
        name: "generate_concept_map",
        description: "Generate a visual concept map showing relationships between a node and related content. Auto-inserts into Workflowy if Dropbox is configured.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the center node for the concept map" },
            scope: { type: "string", enum: ["this_node", "children", "siblings", "ancestors", "all"], description: "Search scope for related nodes" },
            max_related: { type: "number", description: "Maximum number of related nodes to include" },
            output_path: { type: "string", description: "Output file path" },
            format: { type: "string", enum: ["png", "jpeg"], description: "Image format" },
            title: { type: "string", description: "Title for the concept map" },
          },
          required: ["node_id"],
        },
      },
      {
        name: "insert_content",
        description: "Insert content (possibly multiline with indentation) into a specific node.",
        inputSchema: {
          type: "object",
          properties: {
            parent_id: { type: "string", description: "The ID of the parent node to insert content under" },
            content: { type: "string", description: "The content to insert (can be multiline)" },
            position: { type: "string", enum: ["top", "bottom"], description: "Position relative to siblings" },
          },
          required: ["parent_id", "content"],
        },
      },
      {
        name: "find_insert_targets",
        description: "Search for potential target nodes to insert content into.",
        inputSchema: {
          type: "object",
          properties: {
            query: { type: "string", description: "Search text to find potential target nodes" },
          },
          required: ["query"],
        },
      },
      {
        name: "smart_insert",
        description: "Search for a node and insert content. If multiple matches, returns options for selection.",
        inputSchema: {
          type: "object",
          properties: {
            search_query: { type: "string", description: "Search text to find the target node" },
            content: { type: "string", description: "The content to insert" },
            selection: { type: "number", description: "If multiple matches found, the number (1-based) of the node to use" },
            position: { type: "string", enum: ["top", "bottom"], description: "Position relative to siblings" },
          },
          required: ["search_query", "content"],
        },
      },
      {
        name: "export_all",
        description: "Export all nodes from Workflowy. Rate limited to 1 request per minute.",
        inputSchema: {
          type: "object",
          properties: {},
        },
      },
      {
        name: "list_targets",
        description: "List available Workflowy shortcuts/targets (inbox, starred nodes, etc.).",
        inputSchema: {
          type: "object",
          properties: {},
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
      const { query } = searchNodesSchema.parse(args);
      const allNodes = await getCachedNodes();
      const lowerQuery = query.toLowerCase();

      const matchingNodes = allNodes.filter((node) => {
        const nameMatch = node.name?.toLowerCase().includes(lowerQuery);
        const noteMatch = node.note?.toLowerCase().includes(lowerQuery);
        return nameMatch || noteMatch;
      });

      const nodesWithPaths = buildNodePaths(matchingNodes);
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

    case "create_node": {
      const { name: nodeName, note, parent_id, position } = createNodeSchema.parse(args);
      const body: Record<string, unknown> = { name: nodeName };
      if (note) body.note = note;
      if (parent_id) body.parent_id = parent_id;
      if (position) body.position = position;

      const result = await workflowyRequest("/nodes", "POST", body);
      invalidateCache();
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

    case "create_todo": {
      const { name: todoName, note, parent_id, completed, position } = createTodoSchema.parse(args);
      const body: Record<string, unknown> = {
        name: todoName,
        layoutMode: "todo",
      };
      if (note) body.note = note;
      if (parent_id) body.parent_id = parent_id;
      if (position) body.position = position;

      const result = (await workflowyRequest("/nodes", "POST", body)) as { id: string };

      if (completed) {
        await workflowyRequest(`/nodes/${result.id}/complete`, "POST");
      }

      invalidateCache();
      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            ...result,
            completed: completed || false,
            message: `Todo created${completed ? " and marked complete" : ""}`,
          }, null, 2),
        }],
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

    case "generate_concept_map": {
      const { node_id, scope, max_related, output_path, format, title } =
        generateConceptMapSchema.parse(args);
      const allNodes = await getCachedNodes();

      if (!Array.isArray(allNodes) || allNodes.length === 0) {
        return {
          content: [{ type: "text", text: "Error: Could not retrieve nodes from Workflowy" }],
          isError: true,
        };
      }

      const sourceNode = allNodes.find((n) => n.id === node_id);
      if (!sourceNode) {
        return {
          content: [{ type: "text", text: `Error: Node with ID "${node_id}" not found` }],
          isError: true,
        };
      }

      const searchScope = scope || "all";
      const scopedNodes = filterNodesByScope(sourceNode, allNodes, searchScope);

      const { keywords, relatedNodes } = await findRelatedNodes(
        sourceNode,
        scopedNodes.length > 0 ? scopedNodes : allNodes.filter((n) => n.id !== sourceNode.id),
        max_related || 15
      );

      if (relatedNodes.length === 0) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              message: "No related nodes found. The node may not have enough content to find conceptual links.",
              keywords_extracted: keywords,
              scope_used: searchScope,
              nodes_in_scope: scopedNodes.length,
            }, null, 2),
          }],
        };
      }

      const imageFormat = format || "png";
      const mapTitle = title || `Concept Map: ${sourceNode.name || "Node"}`;

      const result = await generateConceptMapImage(
        { id: sourceNode.id, name: sourceNode.name || "" },
        relatedNodes,
        mapTitle,
        imageFormat
      );

      if (!result.success || !result.buffer) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              message: "Failed to generate concept map image",
              error: result.error,
            }, null, 2),
          }],
          isError: true,
        };
      }

      const timestamp = Date.now();
      const filename = `concept-map-${timestamp}.${imageFormat}`;

      // Try to upload to Dropbox and insert into Workflowy
      const uploadResult = await uploadToDropbox(result.buffer, filename);

      if (uploadResult.success && uploadResult.url) {
        const imageMarkdown = `![Concept Map](${uploadResult.url})`;
        const nodeNote = `Scope: ${searchScope} | Keywords: ${keywords.slice(0, 5).join(", ")}${keywords.length > 5 ? "..." : ""} | ${relatedNodes.length} related nodes`;

        try {
          await workflowyRequest("/nodes", "POST", {
            parent_id: sourceNode.id,
            name: `📊 ${mapTitle}`,
            note: `${imageMarkdown}\n\n${nodeNote}`,
            priority: 0,
          });
          invalidateCache();

          return {
            content: [{
              type: "text",
              text: JSON.stringify({
                success: true,
                message: "Concept map created and inserted into Workflowy",
                inserted_into: { id: sourceNode.id, name: sourceNode.name },
                image_url: uploadResult.url,
                format: imageFormat,
                scope: searchScope,
                keywords_used: keywords,
                related_nodes_count: relatedNodes.length,
              }, null, 2),
            }],
          };
        } catch (insertError) {
          return {
            content: [{
              type: "text",
              text: JSON.stringify({
                success: true,
                message: "Concept map uploaded but failed to insert into Workflowy",
                image_url: uploadResult.url,
                insert_error: insertError instanceof Error ? insertError.message : String(insertError),
                tip: "You can manually add this image URL to your Workflowy node.",
              }, null, 2),
            }],
          };
        }
      } else {
        // Dropbox upload failed - save locally as fallback
        const defaultPath = path.join(process.env.HOME || "/tmp", "Downloads", filename);
        const finalPath = output_path || defaultPath;

        const dir = path.dirname(finalPath);
        if (!fs.existsSync(dir)) {
          fs.mkdirSync(dir, { recursive: true });
        }

        fs.writeFileSync(finalPath, result.buffer);

        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: true,
              message: "Concept map saved locally (Dropbox not configured)",
              file_path: finalPath,
              dropbox_error: uploadResult.error,
              format: imageFormat,
              scope: searchScope,
              center_node: { id: sourceNode.id, name: sourceNode.name },
              keywords_used: keywords,
              related_nodes_count: relatedNodes.length,
              tip: "Configure Dropbox to auto-insert concept maps into Workflowy. See README.",
            }, null, 2),
          }],
        };
      }
    }

    case "insert_content": {
      const { parent_id, content, position } = insertContentSchema.parse(args);
      const createdNodes = await insertHierarchicalContent(parent_id, content, position);
      invalidateCache();
      return {
        content: [{
          type: "text",
          text: JSON.stringify({
            message: `Inserted ${createdNodes.length} node(s)`,
            nodes: createdNodes,
          }, null, 2),
        }],
      };
    }

    case "find_insert_targets": {
      const { query } = findInsertTargetsSchema.parse(args);
      const allNodes = await getCachedNodes();
      const lowerQuery = query.toLowerCase();

      const matchingNodes = allNodes.filter((node) => {
        const nameMatch = node.name?.toLowerCase().includes(lowerQuery);
        const noteMatch = node.note?.toLowerCase().includes(lowerQuery);
        return nameMatch || noteMatch;
      });

      const nodesWithPaths = buildNodePaths(matchingNodes);
      return {
        content: [{ type: "text", text: formatNodesForSelection(nodesWithPaths) }],
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
        const createdNodes = await insertHierarchicalContent(targetNode.id, content, position);
        invalidateCache();
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              message: `Inserted ${createdNodes.length} node(s) into "${targetNode.name}"`,
              target: { id: targetNode.id, name: targetNode.name, path: nodesWithPaths[0].path },
              nodes: createdNodes,
            }, null, 2),
          }],
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
        const createdNodes = await insertHierarchicalContent(targetNode.id, content, position);
        invalidateCache();
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              message: `Inserted ${createdNodes.length} node(s) into "${targetNode.name}"`,
              target: { id: targetNode.id, name: targetNode.name, path: nodesWithPaths[index].path },
              nodes: createdNodes,
            }, null, 2),
          }],
        };
      }

      return {
        content: [{
          type: "text",
          text: `Multiple nodes match "${search_query}". Please select one:\n\n${formatNodesForSelection(nodesWithPaths)}\n\nCall smart_insert again with the selection parameter (1-${matchingNodes.length}) to insert your content.`,
        }],
      };
    }

    case "export_all": {
      const result = await workflowyRequest("/nodes-export");
      return {
        content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
      };
    }

    case "list_targets": {
      const result = await workflowyRequest("/targets");
      return {
        content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
      };
    }

    default:
      return {
        content: [{ type: "text", text: `Unknown tool: ${name}` }],
        isError: true,
      };
  }
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
