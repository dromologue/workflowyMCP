import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import * as dotenv from "dotenv";
import * as path from "path";
import { fileURLToPath } from "url";

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
    cachedNodes = (await workflowyRequest("/nodes-export")) as WorkflowyNode[];
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
