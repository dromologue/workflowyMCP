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

interface WorkflowyTarget {
  id: string;
  name: string;
  type: string;
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
          "Search for nodes in Workflowy by text. Returns all nodes matching the query in their name or note.",
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
              description:
                "Parent node ID. Omit to get root-level nodes",
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
        description:
          "Update an existing node's name and/or note.",
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
        name: "insert_content",
        description:
          "Insert Claude's generated content into a specified Workflowy node. Creates a new child node with the content.",
        inputSchema: {
          type: "object",
          properties: {
            parent_id: {
              type: "string",
              description:
                "The ID of the parent node to insert content under",
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
        // Export all nodes and search locally
        const allNodes = (await workflowyRequest(
          "/nodes-export"
        )) as WorkflowyNode[];
        const queryLower = query.toLowerCase();
        const matches = allNodes.filter(
          (node) =>
            node.name?.toLowerCase().includes(queryLower) ||
            node.note?.toLowerCase().includes(queryLower)
        );
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  query,
                  total_matches: matches.length,
                  nodes: matches,
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
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                { success: true, node: result },
                null,
                2
              ),
            },
          ],
        };
      }

      case "update_node": {
        const {
          node_id,
          name: nodeName,
          note,
        } = updateNodeSchema.parse(args);
        const body: Record<string, unknown> = {};
        if (nodeName !== undefined) body.name = nodeName;
        if (note !== undefined) body.note = note;

        const result = await workflowyRequest(
          `/nodes/${node_id}`,
          "POST",
          body
        );
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                { success: true, node: result },
                null,
                2
              ),
            },
          ],
        };
      }

      case "delete_node": {
        const { node_id } = deleteNodeSchema.parse(args);
        await workflowyRequest(`/nodes/${node_id}`, "DELETE");
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
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                { success: true, node: result },
                null,
                2
              ),
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
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                { success: true, node: result },
                null,
                2
              ),
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
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                { success: true, node: result },
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

        // Split content by double newlines to create separate nodes
        // Single newlines within content are preserved
        const lines = content.split(/\n\n+/);
        const createdNodes: unknown[] = [];

        for (const line of lines) {
          if (line.trim()) {
            const body: Record<string, unknown> = {
              name: line.trim(),
              parent_id,
            };
            if (position) body.position = position;

            const result = await workflowyRequest("/nodes", "POST", body);
            createdNodes.push(result);
          }
        }

        return {
          content: [
            {
              type: "text",
              text: JSON.stringify(
                {
                  success: true,
                  message: `Inserted ${createdNodes.length} node(s)`,
                  nodes: createdNodes,
                },
                null,
                2
              ),
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
