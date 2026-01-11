import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import * as dotenv from "dotenv";
import * as path from "path";
import * as fs from "fs";
import { fileURLToPath } from "url";
import { Graphviz } from "@hpcc-js/wasm-graphviz";
import sharp from "sharp";

// Load environment variables from .env file in the project directory
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
dotenv.config({ path: path.join(__dirname, "..", ".env") });

const WORKFLOWY_API_KEY = process.env.WORKFLOWY_API_KEY;
const WORKFLOWY_BASE_URL = "https://workflowy.com/api/v1";

if (!WORKFLOWY_API_KEY) {
  console.error("Error: WORKFLOWY_API_KEY environment variable is not set");
  process.exit(1);
}

// Workflowy API helper functions
async function workflowyRequest(
  endpoint: string,
  method: string = "GET",
  body?: object
): Promise<unknown> {
  const url = `${WORKFLOWY_BASE_URL}${endpoint}`;
  const headers: Record<string, string> = {
    Authorization: `Bearer ${WORKFLOWY_API_KEY}`,
    "Content-Type": "application/json",
  };

  const options: RequestInit = {
    method,
    headers,
  };

  if (body) {
    options.body = JSON.stringify(body);
  }

  const response = await fetch(url, options);

  if (!response.ok) {
    const errorText = await response.text();
    throw new Error(`Workflowy API error: ${response.status} - ${errorText}`);
  }

  return response.json();
}

// Node interface based on Workflowy API
interface WorkflowyNode {
  id: string;
  name: string;
  note?: string;
  priority?: number;
  layoutMode?: string;
  createdAt?: number;
  modifiedAt?: number;
  completedAt?: number;
  parent_id?: string;
}

interface NodeWithPath extends WorkflowyNode {
  path: string;
  depth: number;
}

// Helper function to build node paths for better identification
function buildNodePaths(nodes: WorkflowyNode[]): NodeWithPath[] {
  const nodeMap = new Map<string, WorkflowyNode>();
  nodes.forEach((node) => nodeMap.set(node.id, node));

  function getPath(node: WorkflowyNode, depth: number = 0): { path: string; depth: number } {
    const parts: string[] = [];
    let current: WorkflowyNode | undefined = node;
    let currentDepth = 0;

    while (current) {
      const displayName = current.name?.substring(0, 40) || "(untitled)";
      parts.unshift(displayName);
      currentDepth++;
      if (current.parent_id) {
        current = nodeMap.get(current.parent_id);
      } else {
        break;
      }
    }

    return { path: parts.join(" > "), depth: currentDepth };
  }

  return nodes.map((node) => {
    const { path, depth } = getPath(node);
    return { ...node, path, depth };
  });
}

// Format nodes for display with numbered options
function formatNodesForSelection(nodes: NodeWithPath[]): string {
  if (nodes.length === 0) {
    return "No matching nodes found.";
  }

  const lines = nodes.map((node, index) => {
    const note = node.note ? ` [note: ${node.note.substring(0, 50)}...]` : "";
    return `[${index + 1}] ${node.path}${note}\n    ID: ${node.id}`;
  });

  return `Found ${nodes.length} matching node(s):\n\n${lines.join("\n\n")}`;
}

// Parse indented content into hierarchical structure
interface ParsedLine {
  text: string;
  indent: number;
}

function parseIndentedContent(content: string): ParsedLine[] {
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

// Common stop words to filter out when extracting keywords
const STOP_WORDS = new Set([
  "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of",
  "with", "by", "from", "as", "is", "was", "are", "were", "been", "be", "have",
  "has", "had", "do", "does", "did", "will", "would", "could", "should", "may",
  "might", "must", "can", "this", "that", "these", "those", "i", "you", "he",
  "she", "it", "we", "they", "what", "which", "who", "whom", "when", "where",
  "why", "how", "all", "each", "every", "both", "few", "more", "most", "other",
  "some", "such", "no", "nor", "not", "only", "own", "same", "so", "than",
  "too", "very", "just", "also", "now", "here", "there", "then", "once",
  "if", "about", "into", "through", "during", "before", "after", "above",
  "below", "between", "under", "again", "further", "any", "your", "my", "our",
  "their", "its", "his", "her", "up", "down", "out", "off", "over", "under",
  "get", "got", "make", "made", "take", "took", "see", "saw", "know", "knew",
  "think", "thought", "come", "came", "go", "went", "want", "need", "use",
  "used", "like", "new", "first", "last", "long", "great", "little", "own",
  "good", "bad", "right", "left", "being", "thing", "things", "way", "ways",
  "work", "well", "even", "back", "still", "while", "since", "much", "many"
]);

// Extract significant keywords from text
function extractKeywords(text: string): string[] {
  if (!text) return [];

  // Normalize text: lowercase, remove special chars except hyphens in words
  const normalized = text
    .toLowerCase()
    .replace(/[^\w\s-]/g, " ")
    .replace(/\s+/g, " ")
    .trim();

  // Split into words
  const words = normalized.split(" ");

  // Filter and score keywords
  const keywords: string[] = [];
  const seen = new Set<string>();

  for (const word of words) {
    // Skip short words, stop words, and duplicates
    if (word.length < 3) continue;
    if (STOP_WORDS.has(word)) continue;
    if (seen.has(word)) continue;

    // Skip pure numbers
    if (/^\d+$/.test(word)) continue;

    seen.add(word);
    keywords.push(word);
  }

  return keywords;
}

// Calculate relevance score between a node and keywords
function calculateRelevance(
  node: WorkflowyNode,
  keywords: string[],
  sourceNodeId: string
): number {
  // Don't match the source node itself
  if (node.id === sourceNodeId) return 0;

  const nodeText = `${node.name || ""} ${node.note || ""}`.toLowerCase();
  let score = 0;

  for (const keyword of keywords) {
    // Count occurrences of keyword in node text
    const regex = new RegExp(`\\b${keyword}\\b`, "gi");
    const matches = nodeText.match(regex);
    if (matches) {
      // Boost score for title matches vs note matches
      const titleMatches = (node.name || "").toLowerCase().match(regex);
      score += matches.length;
      if (titleMatches) {
        score += titleMatches.length * 2; // Title matches worth 3x total
      }
    }
  }

  return score;
}

// Generate Workflowy internal link
function generateWorkflowyLink(nodeId: string, nodeName: string): string {
  // Workflowy internal links format: [text](https://workflowy.com/#/nodeid)
  const cleanName = (nodeName || "Untitled").substring(0, 50);
  return `[${cleanName}](https://workflowy.com/#/${nodeId})`;
}

interface RelatedNode {
  id: string;
  name: string;
  note?: string;
  path: string;
  relevanceScore: number;
  matchedKeywords: string[];
  link: string;
}

// Filter nodes by scope relative to a source node
type ConceptMapScope = "this_node" | "children" | "siblings" | "ancestors" | "all";

function filterNodesByScope(
  sourceNode: WorkflowyNode,
  allNodes: WorkflowyNode[],
  scope: ConceptMapScope
): WorkflowyNode[] {
  // Defensive check: ensure allNodes is an array
  if (!Array.isArray(allNodes)) {
    return [];
  }

  switch (scope) {
    case "this_node":
      // Only the source node itself (no related nodes possible)
      return [];

    case "children": {
      // Source node and all its descendants
      const childIds = new Set<string>();
      const findChildren = (parentId: string, depth: number = 0) => {
        // Prevent infinite recursion
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
      // Nodes with the same parent as the source node
      if (!sourceNode.parent_id) {
        // Root-level nodes are siblings
        return allNodes.filter(
          (n) => !n.parent_id && n.id !== sourceNode.id
        );
      }
      return allNodes.filter(
        (n) => n.parent_id === sourceNode.parent_id && n.id !== sourceNode.id
      );
    }

    case "ancestors": {
      // Walk up the parent chain
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
      // All nodes except the source
      return allNodes.filter((n) => n.id !== sourceNode.id);
  }
}

// Find nodes related to a source node based on keyword matching
async function findRelatedNodes(
  sourceNode: WorkflowyNode,
  allNodes: WorkflowyNode[],
  maxResults: number = 10
): Promise<{ keywords: string[]; relatedNodes: RelatedNode[] }> {
  // Extract keywords from source node
  const sourceText = `${sourceNode.name || ""} ${sourceNode.note || ""}`;
  const keywords = extractKeywords(sourceText);

  if (keywords.length === 0) {
    return { keywords: [], relatedNodes: [] };
  }

  // Score all nodes
  const scoredNodes: Array<{
    node: WorkflowyNode;
    score: number;
    matchedKeywords: string[];
  }> = [];

  for (const node of allNodes) {
    const score = calculateRelevance(node, keywords, sourceNode.id);
    if (score > 0) {
      // Find which keywords matched
      const nodeText = `${node.name || ""} ${node.note || ""}`.toLowerCase();
      const matchedKeywords = keywords.filter((kw) =>
        new RegExp(`\\b${kw}\\b`, "i").test(nodeText)
      );
      scoredNodes.push({ node, score, matchedKeywords });
    }
  }

  // Sort by score descending
  scoredNodes.sort((a, b) => b.score - a.score);

  // Take top results
  const topNodes = scoredNodes.slice(0, maxResults);

  // Build paths for context
  const nodesWithPaths = buildNodePaths(topNodes.map((n) => n.node));
  const pathMap = new Map(nodesWithPaths.map((n) => [n.id, n.path]));

  // Format results
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

// Concept map generation
interface ConceptMapNode {
  id: string;
  label: string;
  isCenter: boolean;
}

interface ConceptMapEdge {
  from: string;
  to: string;
  keywords: string[];
  weight: number;
}

// Escape special characters for DOT format
function escapeForDot(str: string): string {
  return str
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n")
    .substring(0, 40); // Truncate for readability
}

// Generate DOT format graph for Graphviz
function generateDotGraph(
  centerNode: { id: string; name: string },
  relatedNodes: RelatedNode[],
  title: string
): string {
  const nodes: ConceptMapNode[] = [
    { id: centerNode.id, label: centerNode.name || "Center", isCenter: true },
  ];

  const edges: ConceptMapEdge[] = [];

  for (const related of relatedNodes) {
    nodes.push({
      id: related.id,
      label: related.name || "Untitled",
      isCenter: false,
    });
    edges.push({
      from: centerNode.id,
      to: related.id,
      keywords: related.matchedKeywords,
      weight: related.relevanceScore,
    });
  }

  // Build DOT format
  let dot = `digraph ConceptMap {
    // Graph settings for readability
    graph [
      rankdir=TB
      bgcolor="#ffffff"
      fontname="Arial"
      fontsize=14
      pad=0.5
      nodesep=0.8
      ranksep=1.0
      label="${escapeForDot(title)}"
      labelloc=t
      labeljust=c
      fontcolor="#333333"
    ];

    // Default node settings
    node [
      shape=box
      style="rounded,filled"
      fontname="Arial"
      fontsize=11
      margin=0.2
      width=0
      height=0
    ];

    // Default edge settings
    edge [
      fontname="Arial"
      fontsize=9
      color="#666666"
      fontcolor="#888888"
    ];

`;

  // Add nodes
  for (const node of nodes) {
    const escapedLabel = escapeForDot(node.label);
    if (node.isCenter) {
      // Center node - larger, different color
      dot += `    "${node.id}" [
      label="${escapedLabel}"
      fillcolor="#4A90D9"
      fontcolor="#ffffff"
      fontsize=13
      penwidth=2
    ];\n`;
    } else {
      // Related nodes - lighter color
      dot += `    "${node.id}" [
      label="${escapedLabel}"
      fillcolor="#E8F4FD"
      fontcolor="#333333"
      penwidth=1
    ];\n`;
    }
  }

  dot += "\n";

  // Add edges with keyword labels
  for (const edge of edges) {
    const keywordLabel =
      edge.keywords.length > 0
        ? edge.keywords.slice(0, 3).join(", ")
        : "";
    const penWidth = Math.min(1 + edge.weight * 0.3, 4);
    dot += `    "${edge.from}" -> "${edge.to}" [
      label="${escapeForDot(keywordLabel)}"
      penwidth=${penWidth.toFixed(1)}
    ];\n`;
  }

  dot += "}\n";

  return dot;
}

// Generate concept map image
async function generateConceptMapImage(
  centerNode: { id: string; name: string },
  relatedNodes: RelatedNode[],
  title: string,
  format: "png" | "jpeg" = "png"
): Promise<{ success: boolean; buffer?: Buffer; error?: string }> {
  try {
    // Generate DOT graph
    const dot = generateDotGraph(centerNode, relatedNodes, title);

    // Initialize Graphviz WASM
    const graphviz = await Graphviz.load();

    // Render to SVG
    const svg = graphviz.dot(dot, "svg");

    // Convert SVG to PNG/JPEG using sharp at high resolution
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
  } catch (error) {
    return {
      success: false,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

// Insert hierarchical content respecting indentation
interface CreatedNode {
  id: string;
  name: string;
  [key: string]: unknown;
}

async function insertHierarchicalContent(
  rootParentId: string,
  content: string,
  position?: "top" | "bottom"
): Promise<CreatedNode[]> {
  const parsedLines = parseIndentedContent(content);
  const createdNodes: CreatedNode[] = [];

  // Stack to track parent IDs at each indent level
  // Index 0 = root parent, Index 1 = first level children, etc.
  const parentStack: string[] = [rootParentId];

  // Track if we've inserted the first top-level node (for "top" positioning)
  let firstTopLevelInserted = false;

  for (const line of parsedLines) {
    // Determine the parent for this node
    // If indent is 0, parent is rootParentId
    // If indent is N, parent is the node at level N-1
    const parentIndex = Math.min(line.indent, parentStack.length - 1);
    const parentId = parentStack[parentIndex];

    // Position logic:
    // - Default to "bottom" to preserve content order
    // - If "top" requested: only first top-level node goes to top,
    //   all subsequent nodes use "bottom" to maintain order
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

    // Update the parent stack for subsequent nodes
    // Set this node as the parent at indent level + 1
    parentStack[line.indent + 1] = result.id;
    // Trim the stack to remove any deeper levels (they're now invalid)
    parentStack.length = line.indent + 2;
  }

  return createdNodes;
}

// Tool schemas
const searchNodesSchema = z.object({
  query: z.string().describe("Text to search for in node names and notes"),
});

const getNodeSchema = z.object({
  node_id: z.string().describe("The ID of the node to retrieve"),
});

const getChildrenSchema = z.object({
  parent_id: z
    .string()
    .optional()
    .describe("Parent node ID. Omit to get root-level nodes"),
});

const createNodeSchema = z.object({
  name: z.string().describe("The text content of the new node"),
  note: z.string().optional().describe("Optional note for the node"),
  parent_id: z
    .string()
    .optional()
    .describe(
      'Parent node ID, target key (e.g., "inbox"), or omit for root level'
    ),
  position: z
    .enum(["top", "bottom"])
    .optional()
    .describe("Position relative to siblings (default: top)"),
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
  position: z
    .enum(["top", "bottom"])
    .optional()
    .describe("Position relative to siblings"),
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
  parent_id: z
    .string()
    .optional()
    .describe(
      'Parent node ID, target key (e.g., "inbox"), or omit for root level'
    ),
  completed: z
    .boolean()
    .optional()
    .describe("Whether the todo starts as completed (default: false)"),
  position: z
    .enum(["top", "bottom"])
    .optional()
    .describe("Position relative to siblings (default: bottom)"),
});

const listTodosSchema = z.object({
  parent_id: z
    .string()
    .optional()
    .describe("Filter to todos under a specific parent node"),
  status: z
    .enum(["all", "pending", "completed"])
    .optional()
    .describe("Filter by completion status (default: all)"),
  query: z
    .string()
    .optional()
    .describe("Optional text to search for within todos"),
});

const findRelatedSchema = z.object({
  node_id: z.string().describe("The ID of the node to find related content for"),
  max_results: z
    .number()
    .optional()
    .describe("Maximum number of related nodes to return (default: 10)"),
});

const createLinksSchema = z.object({
  node_id: z.string().describe("The ID of the node to add links to"),
  link_node_ids: z
    .array(z.string())
    .optional()
    .describe("Specific node IDs to link to. If omitted, auto-discovers related nodes."),
  max_links: z
    .number()
    .optional()
    .describe("Maximum number of auto-discovered links to create (default: 5)"),
  position: z
    .enum(["note", "child"])
    .optional()
    .describe("Where to place links: 'note' appends to node note, 'child' creates a 'Related' child node (default: child)"),
});

const generateConceptMapSchema = z.object({
  node_id: z.string().describe("The ID of the center node for the concept map"),
  scope: z
    .enum(["this_node", "children", "siblings", "ancestors", "all"])
    .optional()
    .describe("Search scope: 'children' (descendants only), 'siblings' (peer nodes), 'ancestors' (parent chain), 'all' (entire Workflowy). Default: 'all'"),
  max_related: z
    .number()
    .optional()
    .describe("Maximum number of related nodes to include in the map (default: 15)"),
  output_path: z
    .string()
    .optional()
    .describe("Output file path. Defaults to ~/Downloads/concept-map-{timestamp}.png"),
  format: z
    .enum(["png", "jpeg"])
    .optional()
    .describe("Image format (default: png)"),
  title: z
    .string()
    .optional()
    .describe("Title for the concept map (defaults to node name)"),
});

const insertContentSchema = z.object({
  parent_id: z
    .string()
    .describe("The ID of the parent node to insert content under"),
  content: z.string().describe("The content to insert (can be multiline)"),
  position: z
    .enum(["top", "bottom"])
    .optional()
    .describe("Position relative to siblings (default: top)"),
});

const findInsertTargetsSchema = z.object({
  query: z.string().describe("Search text to find potential target nodes"),
});

const smartInsertSchema = z.object({
  search_query: z
    .string()
    .describe("Search text to find the target node for insertion"),
  content: z.string().describe("The content to insert"),
  selection: z
    .number()
    .optional()
    .describe("If multiple matches found, the number (1-based) of the node to use"),
  position: z
    .enum(["top", "bottom"])
    .optional()
    .describe("Position relative to siblings (default: top)"),
});

// Cache for nodes to avoid repeated API calls
let cachedNodes: WorkflowyNode[] | null = null;
let cacheTimestamp: number = 0;
const CACHE_TTL = 30000; // 30 seconds

async function getCachedNodes(): Promise<WorkflowyNode[]> {
  const now = Date.now();
  if (!cachedNodes || now - cacheTimestamp > CACHE_TTL) {
    const response = await workflowyRequest("/nodes-export");
    // API returns { nodes: [...] } not an array directly
    if (response && typeof response === "object" && "nodes" in response) {
      cachedNodes = (response as { nodes: WorkflowyNode[] }).nodes;
    } else if (Array.isArray(response)) {
      cachedNodes = response as WorkflowyNode[];
    } else {
      cachedNodes = [];
    }
    cacheTimestamp = now;
  }
  return cachedNodes;
}

function invalidateCache() {
  cachedNodes = null;
  cacheTimestamp = 0;
}

// Initialize MCP server
const server = new Server(
  { name: "workflowy-mcp-server", version: "1.0.0" },
  { capabilities: { tools: {} } }
);

// List available tools
server.setRequestHandler(ListToolsRequestSchema, async () => {
  return {
    tools: [
      {
        name: "search_nodes",
        description:
          "Search for nodes in Workflowy by text. Returns all nodes matching the query in their name or note, with full paths for identification.",
        inputSchema: {
          type: "object",
          properties: {
            query: {
              type: "string",
              description: "Text to search for in node names and notes",
            },
          },
          required: ["query"],
        },
      },
      {
        name: "get_node",
        description:
          "Get a specific node by its ID, including its full content and metadata.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: {
              type: "string",
              description: "The ID of the node to retrieve",
            },
          },
          required: ["node_id"],
        },
      },
      {
        name: "get_children",
        description:
          "Get child nodes of a parent node. Omit parent_id to get root-level nodes.",
        inputSchema: {
          type: "object",
          properties: {
            parent_id: {
              type: "string",
              description: "Parent node ID. Omit to get root-level nodes",
            },
          },
        },
      },
      {
        name: "create_node",
        description:
          "Create a new node in Workflowy. Supports markdown formatting for headers, todos, code blocks, etc.",
        inputSchema: {
          type: "object",
          properties: {
            name: {
              type: "string",
              description: "The text content of the new node",
            },
            note: {
              type: "string",
              description: "Optional note for the node",
            },
            parent_id: {
              type: "string",
              description:
                'Parent node ID, target key (e.g., "inbox"), or omit for root level',
            },
            position: {
              type: "string",
              enum: ["top", "bottom"],
              description: "Position relative to siblings (default: top)",
            },
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
            node_id: {
              type: "string",
              description: "The ID of the node to update",
            },
            name: {
              type: "string",
              description: "New text content for the node",
            },
            note: {
              type: "string",
              description: "New note for the node",
            },
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
            node_id: {
              type: "string",
              description: "The ID of the node to delete",
            },
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
            node_id: {
              type: "string",
              description: "The ID of the node to move",
            },
            parent_id: {
              type: "string",
              description: "The ID of the new parent node",
            },
            position: {
              type: "string",
              enum: ["top", "bottom"],
              description: "Position relative to siblings",
            },
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
            node_id: {
              type: "string",
              description: "The ID of the node to mark as complete",
            },
          },
          required: ["node_id"],
        },
      },
      {
        name: "uncomplete_node",
        description: "Mark a node as not completed.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: {
              type: "string",
              description: "The ID of the node to mark as incomplete",
            },
          },
          required: ["node_id"],
        },
      },
      {
        name: "create_todo",
        description:
          "Create a new todo item in Workflowy. Todo items have a checkbox and can be marked complete/incomplete.",
        inputSchema: {
          type: "object",
          properties: {
            name: {
              type: "string",
              description: "The text content of the todo item",
            },
            note: {
              type: "string",
              description: "Optional note for the todo",
            },
            parent_id: {
              type: "string",
              description:
                'Parent node ID, target key (e.g., "inbox"), or omit for root level',
            },
            completed: {
              type: "boolean",
              description:
                "Whether the todo starts as completed (default: false)",
            },
            position: {
              type: "string",
              enum: ["top", "bottom"],
              description: "Position relative to siblings (default: bottom)",
            },
          },
          required: ["name"],
        },
      },
      {
        name: "list_todos",
        description:
          "List all todo items in Workflowy. Can filter by parent, completion status, and search text.",
        inputSchema: {
          type: "object",
          properties: {
            parent_id: {
              type: "string",
              description: "Filter to todos under a specific parent node",
            },
            status: {
              type: "string",
              enum: ["all", "pending", "completed"],
              description: "Filter by completion status (default: all)",
            },
            query: {
              type: "string",
              description: "Optional text to search for within todos",
            },
          },
        },
      },
      {
        name: "find_related",
        description:
          "Find nodes related to a given node based on keyword analysis. Extracts significant keywords from the source node and finds matching content across the knowledge base.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: {
              type: "string",
              description: "The ID of the node to find related content for",
            },
            max_results: {
              type: "number",
              description:
                "Maximum number of related nodes to return (default: 10)",
            },
          },
          required: ["node_id"],
        },
      },
      {
        name: "create_links",
        description:
          "Create internal links from a node to related nodes in the knowledge base. Can auto-discover related nodes or link to specific node IDs.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: {
              type: "string",
              description: "The ID of the node to add links to",
            },
            link_node_ids: {
              type: "array",
              items: { type: "string" },
              description:
                "Specific node IDs to link to. If omitted, auto-discovers related nodes.",
            },
            max_links: {
              type: "number",
              description:
                "Maximum number of auto-discovered links to create (default: 5)",
            },
            position: {
              type: "string",
              enum: ["note", "child"],
              description:
                "Where to place links: 'note' appends to node note, 'child' creates a 'Related' child node (default: child)",
            },
          },
          required: ["node_id"],
        },
      },
      {
        name: "generate_concept_map",
        description:
          "Generate a visual concept map showing conceptual links between a node and related content. Saves a high-resolution PNG/JPEG to Downloads that can be dragged into Workflowy.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: {
              type: "string",
              description: "The ID of the center node for the concept map",
            },
            scope: {
              type: "string",
              enum: ["children", "siblings", "ancestors", "all"],
              description:
                "Search scope: 'children' (descendants), 'siblings' (peer nodes), 'ancestors' (parent chain), 'all' (entire Workflowy). Default: 'all'",
            },
            max_related: {
              type: "number",
              description:
                "Maximum number of related nodes to include (default: 15)",
            },
            output_path: {
              type: "string",
              description:
                "Output file path. Defaults to ~/Downloads/concept-map-{timestamp}.png",
            },
            format: {
              type: "string",
              enum: ["png", "jpeg"],
              description: "Image format (default: png)",
            },
            title: {
              type: "string",
              description: "Title for the concept map (defaults to node name)",
            },
          },
          required: ["node_id"],
        },
      },
      {
        name: "insert_content",
        description:
          "Insert content into a Workflowy node by ID. Use find_insert_targets first to locate the node ID.",
        inputSchema: {
          type: "object",
          properties: {
            parent_id: {
              type: "string",
              description: "The ID of the parent node to insert content under",
            },
            content: {
              type: "string",
              description: "The content to insert (can be multiline)",
            },
            position: {
              type: "string",
              enum: ["top", "bottom"],
              description: "Position relative to siblings (default: top)",
            },
          },
          required: ["parent_id", "content"],
        },
      },
      {
        name: "find_insert_targets",
        description:
          "Search for potential target nodes to insert content into. Returns a numbered list with full paths and IDs. Use this before insert_content to find the right node.",
        inputSchema: {
          type: "object",
          properties: {
            query: {
              type: "string",
              description: "Search text to find potential target nodes",
            },
          },
          required: ["query"],
        },
      },
      {
        name: "smart_insert",
        description:
          "Search for a node and insert content. If one match is found, inserts immediately. If multiple matches, returns numbered options - call again with selection number to complete insertion.",
        inputSchema: {
          type: "object",
          properties: {
            search_query: {
              type: "string",
              description: "Search text to find the target node for insertion",
            },
            content: {
              type: "string",
              description: "The content to insert",
            },
            selection: {
              type: "number",
              description:
                "If multiple matches were found, the number (1-based) of the node to use",
            },
            position: {
              type: "string",
              enum: ["top", "bottom"],
              description: "Position relative to siblings (default: top)",
            },
          },
          required: ["search_query", "content"],
        },
      },
      {
        name: "list_targets",
        description:
          "List all available targets (shortcuts) including inbox and user-defined shortcuts.",
        inputSchema: {
          type: "object",
          properties: {},
        },
      },
      {
        name: "export_all",
        description:
          "Export all nodes from Workflowy. Rate limited to 1 request per minute.",
        inputSchema: {
          type: "object",
          properties: {},
        },
      },
    ],
  };
});

// Handle tool execution
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;

  try {
    switch (name) {
      case "search_nodes": {
        const { query } = searchNodesSchema.parse(args);
        const allNodes = await getCachedNodes();
        const queryLower = query.toLowerCase();
        const matches = allNodes.filter(
          (node) =>
            node.name?.toLowerCase().includes(queryLower) ||
            node.note?.toLowerCase().includes(queryLower)
        );
        const nodesWithPaths = buildNodePaths(matches);

        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  query,
                  total_matches: matches.length,
                  nodes: nodesWithPaths.map((n) => ({
                    id: n.id,
                    name: n.name,
                    note: n.note,
                    path: n.path,
                    depth: n.depth,
                  })),
                },
                null,
                2
              ),
            },
          ],
        };
      }

      case "get_node": {
        const { node_id } = getNodeSchema.parse(args);
        const node = await workflowyRequest(`/nodes/${node_id}`);
        return {
          content: [{ type: "text", text: JSON.stringify(node, null, 2) }],
        };
      }

      case "get_children": {
        const { parent_id } = getChildrenSchema.parse(args);
        const endpoint = parent_id
          ? `/nodes?parent_id=${parent_id}`
          : "/nodes";
        const nodes = await workflowyRequest(endpoint);
        return {
          content: [{ type: "text", text: JSON.stringify(nodes, null, 2) }],
        };
      }

      case "create_node": {
        const { name: nodeName, note, parent_id, position } =
          createNodeSchema.parse(args);
        const body: Record<string, unknown> = { name: nodeName };
        if (note) body.note = note;
        if (parent_id) body.parent_id = parent_id;
        if (position) body.position = position;

        const result = await workflowyRequest("/nodes", "POST", body);
        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify({ success: true, node: result }, null, 2),
            },
          ],
        };
      }

      case "update_node": {
        const { node_id, name: nodeName, note } = updateNodeSchema.parse(args);
        const body: Record<string, unknown> = {};
        if (nodeName !== undefined) body.name = nodeName;
        if (note !== undefined) body.note = note;

        const result = await workflowyRequest(
          `/nodes/${node_id}`,
          "POST",
          body
        );
        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify({ success: true, node: result }, null, 2),
            },
          ],
        };
      }

      case "delete_node": {
        const { node_id } = deleteNodeSchema.parse(args);
        await workflowyRequest(`/nodes/${node_id}`, "DELETE");
        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                { success: true, message: `Node ${node_id} deleted` },
                null,
                2
              ),
            },
          ],
        };
      }

      case "move_node": {
        const { node_id, parent_id, position } = moveNodeSchema.parse(args);
        const body: Record<string, unknown> = { parent_id };
        if (position) body.position = position;

        const result = await workflowyRequest(
          `/nodes/${node_id}/move`,
          "POST",
          body
        );
        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify({ success: true, node: result }, null, 2),
            },
          ],
        };
      }

      case "complete_node": {
        const { node_id } = completeNodeSchema.parse(args);
        const result = await workflowyRequest(
          `/nodes/${node_id}/complete`,
          "POST"
        );
        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify({ success: true, node: result }, null, 2),
            },
          ],
        };
      }

      case "uncomplete_node": {
        const { node_id } = uncompleteNodeSchema.parse(args);
        const result = await workflowyRequest(
          `/nodes/${node_id}/uncomplete`,
          "POST"
        );
        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify({ success: true, node: result }, null, 2),
            },
          ],
        };
      }

      case "create_todo": {
        const { name: todoName, note, parent_id, completed, position } =
          createTodoSchema.parse(args);

        // Create todo using markdown syntax: - [ ] for uncompleted, - [x] for completed
        const todoPrefix = completed ? "- [x] " : "- [ ] ";
        const body: Record<string, unknown> = {
          name: todoPrefix + todoName,
        };
        if (note) body.note = note;
        if (parent_id) body.parent_id = parent_id;
        body.position = position || "bottom";

        const result = (await workflowyRequest("/nodes", "POST", body)) as CreatedNode;

        // If marked as completed, also call the complete endpoint
        if (completed) {
          await workflowyRequest(`/nodes/${result.id}/complete`, "POST");
        }

        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  success: true,
                  message: `Created todo: "${todoName}"${completed ? " (completed)" : ""}`,
                  todo: result,
                },
                null,
                2
              ),
            },
          ],
        };
      }

      case "list_todos": {
        const { parent_id, status, query } = listTodosSchema.parse(args);
        const allNodes = await getCachedNodes();

        // Filter for todo items (those with layoutMode "todo" or using checkbox syntax)
        let todos = allNodes.filter((node) => {
          // Check for todo layoutMode or checkbox markdown syntax
          const isTodo =
            node.layoutMode === "todo" ||
            node.name?.startsWith("- [ ] ") ||
            node.name?.startsWith("- [x] ");
          return isTodo;
        });

        // Filter by parent if specified
        if (parent_id) {
          // Get all descendant IDs of the parent
          const descendantIds = new Set<string>();
          const findDescendants = (pid: string) => {
            allNodes.forEach((n) => {
              if (n.parent_id === pid) {
                descendantIds.add(n.id);
                findDescendants(n.id);
              }
            });
          };
          descendantIds.add(parent_id);
          findDescendants(parent_id);
          todos = todos.filter((t) => t.parent_id && descendantIds.has(t.parent_id));
        }

        // Filter by completion status
        if (status && status !== "all") {
          todos = todos.filter((t) => {
            const isCompleted =
              t.completedAt !== undefined ||
              t.name?.startsWith("- [x] ");
            return status === "completed" ? isCompleted : !isCompleted;
          });
        }

        // Filter by query if specified
        if (query) {
          const queryLower = query.toLowerCase();
          todos = todos.filter(
            (t) =>
              t.name?.toLowerCase().includes(queryLower) ||
              t.note?.toLowerCase().includes(queryLower)
          );
        }

        // Build paths for better identification
        const todosWithPaths = buildNodePaths(todos);

        // Format output
        const formattedTodos = todosWithPaths.map((t) => {
          const isCompleted =
            t.completedAt !== undefined || t.name?.startsWith("- [x] ");
          // Clean up the name for display (remove checkbox syntax)
          const cleanName = t.name
            ?.replace(/^- \[[ x]\] /, "")
            .trim();
          return {
            id: t.id,
            name: cleanName,
            note: t.note,
            completed: isCompleted,
            path: t.path,
            completedAt: t.completedAt,
          };
        });

        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  total: formattedTodos.length,
                  pending: formattedTodos.filter((t) => !t.completed).length,
                  completed: formattedTodos.filter((t) => t.completed).length,
                  todos: formattedTodos,
                },
                null,
                2
              ),
            },
          ],
        };
      }

      case "find_related": {
        const { node_id, max_results } = findRelatedSchema.parse(args);
        const allNodes = await getCachedNodes();

        // Find the source node
        const sourceNode = allNodes.find((n) => n.id === node_id);
        if (!sourceNode) {
          return {
            content: [
              {
                type: "text",
                text: `Error: Node with ID "${node_id}" not found`,
              },
            ],
            isError: true,
          };
        }

        // Find related nodes
        const { keywords, relatedNodes } = await findRelatedNodes(
          sourceNode,
          allNodes,
          max_results || 10
        );

        if (relatedNodes.length === 0) {
          return {
            content: [
              {
                type: "text",
                text: JSON.stringify(
                  {
                    source_node: {
                      id: sourceNode.id,
                      name: sourceNode.name,
                    },
                    keywords_extracted: keywords,
                    message: "No related nodes found. Try a node with more content.",
                    related_nodes: [],
                  },
                  null,
                  2
                ),
              },
            ],
          };
        }

        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  source_node: {
                    id: sourceNode.id,
                    name: sourceNode.name,
                  },
                  keywords_extracted: keywords,
                  related_count: relatedNodes.length,
                  related_nodes: relatedNodes,
                },
                null,
                2
              ),
            },
          ],
        };
      }

      case "create_links": {
        const { node_id, link_node_ids, max_links, position } =
          createLinksSchema.parse(args);
        const allNodes = await getCachedNodes();

        // Find the source node
        const sourceNode = allNodes.find((n) => n.id === node_id);
        if (!sourceNode) {
          return {
            content: [
              {
                type: "text",
                text: `Error: Node with ID "${node_id}" not found`,
              },
            ],
            isError: true,
          };
        }

        let nodesToLink: RelatedNode[] = [];

        if (link_node_ids && link_node_ids.length > 0) {
          // Use specified node IDs
          const nodesWithPaths = buildNodePaths(allNodes);
          const pathMap = new Map(nodesWithPaths.map((n) => [n.id, n.path]));

          for (const linkId of link_node_ids) {
            const node = allNodes.find((n) => n.id === linkId);
            if (node) {
              nodesToLink.push({
                id: node.id,
                name: node.name || "",
                note: node.note,
                path: pathMap.get(node.id) || node.name || "",
                relevanceScore: 0,
                matchedKeywords: [],
                link: generateWorkflowyLink(node.id, node.name || ""),
              });
            }
          }
        } else {
          // Auto-discover related nodes
          const { relatedNodes } = await findRelatedNodes(
            sourceNode,
            allNodes,
            max_links || 5
          );
          nodesToLink = relatedNodes;
        }

        if (nodesToLink.length === 0) {
          return {
            content: [
              {
                type: "text",
                text: JSON.stringify(
                  {
                    success: false,
                    message: "No nodes to link. Node may lack sufficient content for keyword extraction.",
                  },
                  null,
                  2
                ),
              },
            ],
          };
        }

        // Generate the links content
        const linksContent = nodesToLink
          .map((n) => `• ${n.link}`)
          .join("\n");

        const linkPosition = position || "child";

        if (linkPosition === "note") {
          // Append links to the node's note
          const existingNote = sourceNode.note || "";
          const separator = existingNote ? "\n\n---\n**Related:**\n" : "**Related:**\n";
          const newNote = existingNote + separator + linksContent;

          await workflowyRequest(`/nodes/${node_id}`, "POST", { note: newNote });
        } else {
          // Create a "Related" child node with links
          const relatedNodeContent = `**Related**\n${linksContent}`;
          await workflowyRequest("/nodes", "POST", {
            name: "🔗 Related",
            note: linksContent,
            parent_id: node_id,
            position: "bottom",
          });
        }

        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  success: true,
                  message: `Created ${nodesToLink.length} link(s) ${linkPosition === "note" ? "in note" : "as child node"}`,
                  source_node: {
                    id: sourceNode.id,
                    name: sourceNode.name,
                  },
                  linked_nodes: nodesToLink.map((n) => ({
                    id: n.id,
                    name: n.name,
                    path: n.path,
                    link: n.link,
                  })),
                },
                null,
                2
              ),
            },
          ],
        };
      }

      case "generate_concept_map": {
        const { node_id, scope, max_related, output_path, format, title } =
          generateConceptMapSchema.parse(args);
        const allNodes = await getCachedNodes();

        // Defensive check
        if (!Array.isArray(allNodes) || allNodes.length === 0) {
          return {
            content: [
              {
                type: "text",
                text: "Error: Could not retrieve nodes from Workflowy",
              },
            ],
            isError: true,
          };
        }

        // Find the source node
        const sourceNode = allNodes.find((n) => n.id === node_id);
        if (!sourceNode) {
          return {
            content: [
              {
                type: "text",
                text: `Error: Node with ID "${node_id}" not found`,
              },
            ],
            isError: true,
          };
        }

        // Filter nodes by scope
        const searchScope = scope || "all";
        const scopedNodes = filterNodesByScope(sourceNode, allNodes, searchScope);

        // Find related nodes within the scoped set
        const { keywords, relatedNodes } = await findRelatedNodes(
          sourceNode,
          scopedNodes.length > 0 ? scopedNodes : allNodes.filter(n => n.id !== sourceNode.id),
          max_related || 15
        );

        if (relatedNodes.length === 0) {
          return {
            content: [
              {
                type: "text",
                text: JSON.stringify(
                  {
                    success: false,
                    message: "No related nodes found. The node may not have enough content to find conceptual links.",
                    keywords_extracted: keywords,
                    scope_used: searchScope,
                    nodes_in_scope: scopedNodes.length,
                  },
                  null,
                  2
                ),
              },
            ],
          };
        }

        // Generate the map title
        const imageFormat = format || "png";
        const mapTitle = title || `Concept Map: ${sourceNode.name || "Node"}`;

        // Generate the image
        const result = await generateConceptMapImage(
          { id: sourceNode.id, name: sourceNode.name || "" },
          relatedNodes,
          mapTitle,
          imageFormat
        );

        if (!result.success || !result.buffer) {
          return {
            content: [
              {
                type: "text",
                text: JSON.stringify(
                  {
                    success: false,
                    message: "Failed to generate concept map image",
                    error: result.error,
                  },
                  null,
                  2
                ),
              },
            ],
            isError: true,
          };
        }

        // Save to file
        const timestamp = Date.now();
        const defaultPath = path.join(
          process.env.HOME || "/tmp",
          "Downloads",
          `concept-map-${timestamp}.${imageFormat}`
        );
        const finalPath = output_path || defaultPath;

        // Ensure directory exists
        const dir = path.dirname(finalPath);
        if (!fs.existsSync(dir)) {
          fs.mkdirSync(dir, { recursive: true });
        }

        fs.writeFileSync(finalPath, result.buffer);

        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  success: true,
                  message: "Concept map generated successfully",
                  file_path: finalPath,
                  format: imageFormat,
                  scope: searchScope,
                  center_node: {
                    id: sourceNode.id,
                    name: sourceNode.name,
                  },
                  keywords_used: keywords,
                  related_nodes_count: relatedNodes.length,
                  related_nodes: relatedNodes.map((n) => ({
                    id: n.id,
                    name: n.name,
                    relevance: n.relevanceScore,
                    matched_keywords: n.matchedKeywords,
                  })),
                  tip: "Drag and drop the image file into Workflowy to insert it.",
                },
                null,
                2
              ),
            },
          ],
        };
      }

      case "insert_content": {
        const { parent_id, content, position } =
          insertContentSchema.parse(args);

        // Insert content respecting indentation hierarchy
        const createdNodes = await insertHierarchicalContent(
          parent_id,
          content,
          position
        );

        invalidateCache();
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  success: true,
                  message: `Inserted ${createdNodes.length} node(s) with hierarchy preserved`,
                  nodes: createdNodes,
                },
                null,
                2
              ),
            },
          ],
        };
      }

      case "find_insert_targets": {
        const { query } = findInsertTargetsSchema.parse(args);
        const allNodes = await getCachedNodes();
        const queryLower = query.toLowerCase();

        const matches = allNodes.filter(
          (node) =>
            node.name?.toLowerCase().includes(queryLower) ||
            node.note?.toLowerCase().includes(queryLower)
        );

        const nodesWithPaths = buildNodePaths(matches);
        const formatted = formatNodesForSelection(nodesWithPaths);

        return {
          content: [
            {
              type: "text",
              text:
                formatted +
                "\n\n---\nTo insert content, use insert_content with the ID of your chosen node, or use smart_insert with the selection number.",
            },
          ],
        };
      }

      case "smart_insert": {
        const { search_query, content, selection, position } =
          smartInsertSchema.parse(args);
        const allNodes = await getCachedNodes();
        const queryLower = search_query.toLowerCase();

        const matches = allNodes.filter(
          (node) =>
            node.name?.toLowerCase().includes(queryLower) ||
            node.note?.toLowerCase().includes(queryLower)
        );

        if (matches.length === 0) {
          return {
            content: [
              {
                type: "text",
                text: `No nodes found matching "${search_query}". Try a different search term or use get_children to browse the node structure.`,
              },
            ],
          };
        }

        const nodesWithPaths = buildNodePaths(matches);

        // If selection provided, use that node
        if (selection !== undefined) {
          if (selection < 1 || selection > matches.length) {
            return {
              content: [
                {
                  type: "text",
                  text: `Invalid selection ${selection}. Please choose a number between 1 and ${matches.length}.`,
                },
              ],
              isError: true,
            };
          }

          const targetNode = nodesWithPaths[selection - 1];
          const createdNodes = await insertHierarchicalContent(
            targetNode.id,
            content,
            position
          );

          invalidateCache();
          return {
            content: [
              {
                type: "text",
                text: JSON.stringify(
                  {
                    success: true,
                    message: `Inserted ${createdNodes.length} node(s) into "${targetNode.name}" with hierarchy preserved`,
                    target_path: targetNode.path,
                    nodes: createdNodes,
                  },
                  null,
                  2
                ),
              },
            ],
          };
        }

        // If only one match, insert directly
        if (matches.length === 1) {
          const targetNode = nodesWithPaths[0];
          const createdNodes = await insertHierarchicalContent(
            targetNode.id,
            content,
            position
          );

          invalidateCache();
          return {
            content: [
              {
                type: "text",
                text: JSON.stringify(
                  {
                    success: true,
                    message: `Inserted ${createdNodes.length} node(s) into "${targetNode.name}" with hierarchy preserved`,
                    target_path: targetNode.path,
                    nodes: createdNodes,
                  },
                  null,
                  2
                ),
              },
            ],
          };
        }

        // Multiple matches - return options for user to select
        const formatted = formatNodesForSelection(nodesWithPaths);
        return {
          content: [
            {
              type: "text",
              text:
                `Multiple nodes match "${search_query}". Please select one:\n\n` +
                formatted +
                `\n\n---\nCall smart_insert again with the same search_query and content, plus selection: <number> to insert into your chosen node.`,
            },
          ],
        };
      }

      case "list_targets": {
        const targets = await workflowyRequest("/targets");
        return {
          content: [{ type: "text", text: JSON.stringify(targets, null, 2) }],
        };
      }

      case "export_all": {
        const allNodes = await workflowyRequest("/nodes-export");
        return {
          content: [{ type: "text", text: JSON.stringify(allNodes, null, 2) }],
        };
      }

      default:
        return {
          content: [{ type: "text", text: `Unknown tool: ${name}` }],
          isError: true,
        };
    }
  } catch (error) {
    const errorMessage =
      error instanceof Error ? error.message : String(error);
    return {
      content: [{ type: "text", text: `Error: ${errorMessage}` }],
      isError: true,
    };
  }
});

// Start server
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error("Workflowy MCP server started");
}

main().catch((error) => {
  console.error("Fatal error:", error);
  process.exit(1);
});
