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

// Import from shared modules
import { validateConfig } from "../shared/config/environment.js";
import { workflowyRequest } from "../shared/api/workflowy.js";
import { uploadToDropbox } from "../shared/api/dropbox.js";
import type {
  WorkflowyNode,
  NodeWithPath,
  RelatedNode,
  ConceptMapScope,
  ConceptMapNode as LegacyConceptMapNode,
  ConceptMapEdge as LegacyConceptMapEdge,
  CreatedNode,
  AnalysisContentNode,
  AnalysisContentResult,
} from "../shared/types/index.js";
import {
  getCachedNodesIfValid,
  updateCache,
  invalidateCache,
} from "../shared/utils/cache.js";
import { buildNodePaths } from "../shared/utils/node-paths.js";
import {
  parseIndentedContent,
  formatNodesForSelection,
  escapeForDot,
  generateWorkflowyLink,
  validateConceptMapInput,
  extractWorkflowyLinks,
  CONCEPT_MAP_LIMITS,
} from "../shared/utils/text-processing.js";
import {
  extractKeywords,
  calculateRelevance,
  findMatchedKeywords,
} from "../shared/utils/keyword-extraction.js";

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
// Concept Map Generation
// ============================================================================

// Relationship words to look for when concepts co-occur
const RELATIONSHIP_PATTERNS: Array<{ pattern: RegExp; label: string }> = [
  { pattern: /\b(leads?\s+to|results?\s+in|causes?|produces?)\b/i, label: "leads to" },
  { pattern: /\b(influences?|affects?|impacts?)\b/i, label: "influences" },
  { pattern: /\b(is\s+part\s+of|belongs?\s+to|within)\b/i, label: "is part of" },
  { pattern: /\b(includes?|contains?|comprises?)\b/i, label: "includes" },
  { pattern: /\b(requires?|needs?|depends?\s+on)\b/i, label: "requires" },
  { pattern: /\b(contrasts?\s+with|differs?\s+from|versus|vs\.?|unlike)\b/i, label: "contrasts with" },
  { pattern: /\b(similar\s+to|like|resembles?)\b/i, label: "similar to" },
  { pattern: /\b(defines?|means?|is\s+defined\s+as)\b/i, label: "defines" },
  { pattern: /\b(examples?\s+of|such\s+as|e\.g\.|for\s+instance)\b/i, label: "example of" },
  { pattern: /\b(types?\s+of|kinds?\s+of|forms?\s+of)\b/i, label: "type of" },
  { pattern: /\b(supports?|reinforces?|strengthens?)\b/i, label: "supports" },
  { pattern: /\b(opposes?|contradicts?|challenges?)\b/i, label: "opposes" },
  { pattern: /\b(precedes?|before|prior\s+to)\b/i, label: "precedes" },
  { pattern: /\b(follows?|after|subsequent\s+to)\b/i, label: "follows" },
  { pattern: /\b(enables?|allows?|permits?)\b/i, label: "enables" },
  { pattern: /\b(prevents?|blocks?|inhibits?)\b/i, label: "prevents" },
  { pattern: /\b(creates?|generates?|builds?)\b/i, label: "creates" },
  { pattern: /\b(uses?|utilizes?|employs?)\b/i, label: "uses" },
  { pattern: /\b(extends?|expands?|builds?\s+on)\b/i, label: "extends" },
  { pattern: /\b(criticizes?|critiques?|questions?)\b/i, label: "critiques" },
];

interface ConceptMapNode {
  id: string;
  label: string;
  level: number; // 0 = core, 1 = major concept, 2 = detail
  occurrences: number;
  depth: number; // average depth in Workflowy hierarchy where found
}

interface ConceptMapEdge {
  from: string;
  to: string;
  type: string; // relationship type from vocabulary
  description: string; // natural language explanation of the relationship
  weight: number; // strength of connection (0.0-1.0)
  evidence?: string; // excerpt showing the relationship
  bidirectional: boolean; // whether the relationship is mutual
}

function extractRelationshipLabel(text: string, concept1: string, concept2: string): string {
  // Find the text between or around the two concepts
  const lowerText = text.toLowerCase();
  const pos1 = lowerText.indexOf(concept1.toLowerCase());
  const pos2 = lowerText.indexOf(concept2.toLowerCase());

  if (pos1 === -1 || pos2 === -1) return "relates to";

  // Get the text between the concepts (with some buffer)
  const start = Math.max(0, Math.min(pos1, pos2) - 20);
  const end = Math.min(text.length, Math.max(pos1 + concept1.length, pos2 + concept2.length) + 20);
  const context = text.substring(start, end);

  // Look for relationship patterns
  for (const { pattern, label } of RELATIONSHIP_PATTERNS) {
    if (pattern.test(context)) {
      return label;
    }
  }

  return "relates to";
}

/**
 * Format a relationship type for display (e.g., "part_of" -> "part of")
 */
function formatRelationType(type: string): string {
  return type.replace(/_/g, " ");
}

/**
 * Wrap text to fit within a maximum width (for edge labels)
 */
function wrapText(text: string, maxWidth: number): string {
  const words = text.split(" ");
  const lines: string[] = [];
  let currentLine = "";

  for (const word of words) {
    if (currentLine.length + word.length + 1 <= maxWidth) {
      currentLine += (currentLine ? " " : "") + word;
    } else {
      if (currentLine) lines.push(currentLine);
      currentLine = word;
    }
  }
  if (currentLine) lines.push(currentLine);

  return lines.join("\\n");
}

/**
 * Build a rich edge label from type and description
 */
function buildEdgeLabel(type: string, description: string): string {
  const formattedType = formatRelationType(type);
  // Allow longer descriptions for better context
  const maxDescLen = 60;
  const truncatedDesc = description.length > maxDescLen
    ? description.substring(0, maxDescLen - 3) + "..."
    : description;

  // Wrap with more width for larger output
  return wrapText(`${formattedType}: ${truncatedDesc}`, 35);
}

/**
 * Get edge color based on relationship type category
 */
function getEdgeColor(type: string): string {
  // Causal relationships - blue
  if (["causes", "enables", "prevents", "triggers", "influences"].includes(type)) {
    return "#2980b9";
  }
  // Structural relationships - green
  if (["contains", "part_of", "instance_of", "derives_from", "extends"].includes(type)) {
    return "#27ae60";
  }
  // Temporal relationships - orange
  if (["precedes", "follows", "co_occurs"].includes(type)) {
    return "#e67e22";
  }
  // Logical relationships - purple
  if (["implies", "supports", "refines", "exemplifies"].includes(type)) {
    return "#8e44ad";
  }
  // Contradictory/contrastive - red
  if (["contradicts", "contrasts_with"].includes(type)) {
    return "#c0392b";
  }
  // Comparative relationships - teal
  if (["similar_to", "generalizes", "specializes"].includes(type)) {
    return "#16a085";
  }
  // Default - gray
  return "#566573";
}

/**
 * Get edge style based on relationship type
 */
function getEdgeStyle(type: string): string {
  // Contradictory relationships are dashed
  if (["contradicts", "contrasts_with", "prevents"].includes(type)) {
    return "dashed";
  }
  // Temporal relationships are dotted
  if (["precedes", "follows", "co_occurs"].includes(type)) {
    return "dotted";
  }
  // Strong causal/logical relationships are bold
  if (["causes", "implies", "derives_from"].includes(type)) {
    return "bold";
  }
  return "solid";
}

function generateHierarchicalConceptMap(
  coreNode: ConceptMapNode,
  conceptNodes: ConceptMapNode[],
  edges: ConceptMapEdge[],
  title: string
): string {
  const lines: string[] = [
    "digraph ConceptMap {",
    '  charset="UTF-8";',
    // Use sfdp layout - scales well and produces cleaner results
    '  layout=sfdp;',
    // Prevent node overlap with scaling
    '  overlap=prism;',
    '  overlap_scaling=4;',
    // Use polyline splines - cleaner than ortho for complex graphs
    '  splines=polyline;',
    // Increase separation significantly
    '  sep="+80,80";',
    '  K=3;',
    '  repulsiveforce=2.0;',
    // Large graph dimensions for high-res output
    '  size="40,30";',
    '  ratio=fill;',
    '  bgcolor="white";',
    '  pad="1.0";',
    '  margin="1.0";',
    // Title
    `  label="${escapeForDot(title)}";`,
    '  labelloc="t";',
    '  fontsize=28;',
    '  fontname="Helvetica Bold";',
    "",
    "  // Global node styling",
    '  node [shape=box, style="rounded,filled", fontname="Helvetica", margin="0.3,0.15"];',
    "",
    "  // Global edge styling - larger labels for readability",
    '  edge [fontname="Helvetica", fontsize=11, labelfloat=false, decorate=true, labeldistance=2];',
    "",
  ];

  // Core concept - largest, distinctive color
  lines.push("  // Core concept (center)");
  lines.push(
    `  "${coreNode.id}" [label="${escapeForDot(coreNode.label)}", fillcolor="#1a5276", fontcolor="white", fontsize=20, penwidth=4, width=4, height=1.2];`
  );
  lines.push("");

  // Group concepts by level
  const level1 = conceptNodes.filter(n => n.level === 1);
  const level2 = conceptNodes.filter(n => n.level === 2);

  // Level 1 - Major concepts
  if (level1.length > 0) {
    lines.push("  // Major concepts");
    const majorColors = ["#2874a6", "#1e8449", "#b9770e", "#6c3483", "#1abc9c", "#c0392b", "#2c3e50", "#7d3c98"];
    level1.forEach((node, index) => {
      const color = majorColors[index % majorColors.length];
      const width = Math.max(2.5, Math.min(2.5 + node.occurrences * 0.15, 3.5));
      lines.push(
        `  "${node.id}" [label="${escapeForDot(node.label)}", fillcolor="${color}", fontcolor="white", fontsize=16, width=${width}, height=1];`
      );
    });
    lines.push("");
  }

  // Level 2 - Detail concepts
  if (level2.length > 0) {
    lines.push("  // Detail concepts");
    const detailColors = ["#5dade2", "#58d68d", "#f4d03f", "#bb8fce", "#76d7c4", "#f1948a", "#85929e", "#aed6f1"];
    level2.forEach((node, index) => {
      const color = detailColors[index % detailColors.length];
      const width = Math.max(2.0, Math.min(2.0 + node.occurrences * 0.1, 2.8));
      lines.push(
        `  "${node.id}" [label="${escapeForDot(node.label)}", fillcolor="${color}", fontcolor="#1a1a1a", fontsize=14, width=${width}, height=0.8];`
      );
    });
    lines.push("");
  }

  // Edges with enriched relationship labels
  lines.push("  // Relationships with semantic labels");
  const addedEdges = new Set<string>();

  edges.forEach((edge) => {
    // For bidirectional, use sorted key; for directional, use ordered key
    const edgeKey = edge.bidirectional
      ? [edge.from, edge.to].sort().join("|||")
      : `${edge.from}|||${edge.to}`;
    if (addedEdges.has(edgeKey)) return;
    addedEdges.add(edgeKey);

    // Scale weight from 0-1 to reasonable penwidth (1.5-5)
    const penwidth = Math.max(1.5, Math.min(1.5 + edge.weight * 4, 5));
    const color = getEdgeColor(edge.type);
    const style = getEdgeStyle(edge.type);

    // Build the enriched label with more space
    const label = buildEdgeLabel(edge.type, edge.description);

    // Build edge attributes
    const attrs: string[] = [
      `label="${escapeForDot(label)}"`,
      `fontsize=12`,
      `penwidth=${penwidth}`,
      `color="${color}"`,
      `fontcolor="${color}"`,
      `style="${style}"`,
      `len=3`, // Preferred edge length for sfdp
    ];

    // Handle bidirectional edges
    if (edge.bidirectional) {
      attrs.push(`dir=both`);
      attrs.push(`arrowhead=normal`);
      attrs.push(`arrowtail=normal`);
    }

    // Add tooltip with full description and evidence
    const tooltipParts = [edge.description];
    if (edge.evidence) {
      tooltipParts.push(`Evidence: "${edge.evidence}"`);
    }
    attrs.push(`tooltip="${escapeForDot(tooltipParts.join(" | "))}"`);

    lines.push(`  "${edge.from}" -> "${edge.to}" [${attrs.join(", ")}];`);
  });

  lines.push("}");
  return lines.join("\n");
}

// Legacy function for backward compatibility (hub-and-spoke style)
function generateDotGraph(
  centerNode: LegacyConceptMapNode,
  relatedNodes: Array<{ node: LegacyConceptMapNode; keywords: string[]; weight: number }>,
  title: string
): string {
  const lines: string[] = [
    "digraph ConceptMap {",
    '  charset="UTF-8";',  // Ensure proper handling of accented characters
    '  layout=neato;',     // Force-directed layout for better space usage
    '  overlap=false;',    // Prevent node overlap
    '  splines=true;',     // Curved edges
    '  sep="+20";',        // Minimum separation between nodes
    '  ratio=1;',          // Force 1:1 square aspect ratio
    '  size="14,14!";',    // Force exact square dimensions (! = force)
    '  bgcolor="white";',
    `  label="${escapeForDot(title)}";`,
    '  labelloc="t";',
    '  fontsize=24;',
    '  fontname="Arial";',
    "",
    "  // Node styling",
    '  node [shape=box, style="rounded,filled", fontname="Arial", fontsize=12];',
    "",
    "  // Center node (pinned at center)",
    `  "${centerNode.id}" [label="${escapeForDot(centerNode.label)}", fillcolor="#4A90D9", fontcolor="white", penwidth=2, pos="7,7!", pin=true];`,
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

    const center: LegacyConceptMapNode = {
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
      .resize(2000, 2000, {
        fit: "inside",        // Fit within square bounds
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

async function generateHierarchicalConceptMapImage(
  coreNode: ConceptMapNode,
  conceptNodes: ConceptMapNode[],
  edges: ConceptMapEdge[],
  title: string,
  format: "png" | "jpeg" = "png"
): Promise<{ success: boolean; buffer?: Buffer; error?: string }> {
  try {
    const graphviz = await Graphviz.load();
    const dotGraph = generateHierarchicalConceptMap(coreNode, conceptNodes, edges, title);
    const svg = graphviz.dot(dotGraph, "svg");

    // Generate larger image for better readability when zoomed
    const imageBuffer = await sharp(Buffer.from(svg), { density: 300 })
      .resize(4000, 3000, {
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
  node_id: z.string().describe("The ID of the parent node whose children will be analyzed"),
  core_concept: z.string().optional().describe("The central/main concept of the map. If omitted, uses the parent node name."),
  concepts: z.array(z.string()).describe("REQUIRED: The concepts to map. These become nodes connected to the core and to each other based on relationships found in content."),
  scope: z.enum(["this_node", "children", "siblings", "ancestors", "all"]).optional().describe("Search scope for content to analyze (default: 'children')"),
  output_path: z.string().optional().describe("Output file path. Defaults to ~/Downloads/concept-map-{timestamp}.png"),
  format: z.enum(["png", "jpeg"]).optional().describe("Image format (default: png)"),
  title: z.string().optional().describe("Title for the concept map (defaults to core concept)"),
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

// LLM-Powered Concept Map Schemas
const getNodeContentForAnalysisSchema = z.object({
  node_id: z.string().describe("The ID of the root node to analyze"),
  depth: z.number().optional().describe("Maximum depth to traverse (default: unlimited)"),
  include_notes: z.boolean().optional().describe("Include node notes in output (default: true)"),
  max_nodes: z.number().optional().describe("Maximum number of nodes to return (default: 500)"),
  follow_links: z.boolean().optional().describe("Follow Workflowy internal links to include linked content (default: true)"),
  format: z.enum(["structured", "outline"]).optional().describe("Output format: 'structured' for JSON, 'outline' for indented text (default: structured)"),
});

const renderConceptMapSchema = z.object({
  title: z.string().describe("Title for the concept map"),
  core_concept: z.object({
    label: z.string().describe("The central concept label"),
    description: z.string().optional().describe("Optional description for the core concept"),
  }).describe("The central/main concept of the map"),
  concepts: z.array(z.object({
    id: z.string().describe("Unique identifier (slug form, e.g., 'truth-procedure')"),
    label: z.string().describe("Display label for the concept"),
    level: z.enum(["major", "detail"]).describe("Hierarchy level: 'major' for key concepts, 'detail' for supporting concepts"),
    importance: z.number().optional().describe("Importance score 1-10 (affects node size, default: 5)"),
    description: z.string().optional().describe("Brief description of the concept"),
  })).describe("The concepts discovered through analysis"),
  relationships: z.array(z.object({
    from: z.string().describe("Source concept ID (or 'core' for the central concept)"),
    to: z.string().describe("Target concept ID"),
    type: z.enum([
      // Causal
      "causes", "enables", "prevents", "triggers", "influences",
      // Structural
      "contains", "part_of", "instance_of", "derives_from", "extends",
      // Temporal
      "precedes", "follows", "co_occurs",
      // Logical
      "implies", "contradicts", "supports", "refines", "exemplifies",
      // Comparative
      "similar_to", "contrasts_with", "generalizes", "specializes",
      // Other
      "related_to"
    ]).describe("Relationship type from the defined vocabulary"),
    description: z.string().describe("A concise sentence explaining WHY this relationship exists in this context. Example: 'Feedback loops enable system stability by correcting deviations'"),
    evidence: z.string().optional().describe("Brief quote or paraphrase from source content evidencing this relationship"),
    strength: z.number().optional().describe("Relationship strength 0.0-1.0 (affects edge weight, default: 0.5)"),
    bidirectional: z.boolean().optional().describe("Whether the relationship is mutual/bidirectional (default: false)"),
  })).describe("Relationships between concepts with semantic descriptions"),
  output: z.object({
    format: z.enum(["png", "jpeg"]).optional().describe("Image format (default: png)"),
    insert_into_workflowy: z.string().optional().describe("Node ID to insert the concept map into"),
    output_path: z.string().optional().describe("Custom output file path"),
  }).optional().describe("Output options"),
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
        description: "Generate a hierarchical concept map following academic concept mapping principles. Places a core concept at the center, arranges major and detail concepts in hierarchy based on Workflowy structure depth, and labels relationships between concepts (influences, contrasts with, includes, etc.) extracted from content context. Visual encoding: larger nodes = more occurrences, colored edges = relationship types, dashed lines = contrasting relationships.",
        inputSchema: {
          type: "object",
          properties: {
            node_id: { type: "string", description: "The ID of the parent node whose children will be analyzed" },
            core_concept: { type: "string", description: "The central/main concept of the map (defaults to parent node name)" },
            concepts: { type: "array", items: { type: "string" }, description: "REQUIRED: The concepts to map. These become nodes arranged hierarchically around the core, connected by labeled relationships found in your content." },
            scope: { type: "string", enum: ["this_node", "children", "siblings", "ancestors", "all"], description: "Search scope for content to analyze (default: children)" },
            output_path: { type: "string", description: "Output file path" },
            format: { type: "string", enum: ["png", "jpeg"], description: "Image format" },
            title: { type: "string", description: "Title for the concept map" },
          },
          required: ["node_id", "concepts"],
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
        name: "render_concept_map",
        description: `Render a concept map from your semantic analysis. After analyzing content with get_node_content_for_analysis, use this tool to create a visual concept map.

RELATIONSHIP TYPES (grouped by category):
- Causal: causes, enables, prevents, triggers, influences
- Structural: contains, part_of, instance_of, derives_from, extends
- Temporal: precedes, follows, co_occurs
- Logical: implies, contradicts, supports, refines, exemplifies
- Comparative: similar_to, contrasts_with, generalizes, specializes
- Other: related_to

IMPORTANT: Each relationship MUST include:
1. A 'type' from the vocabulary above
2. A 'description' explaining WHY this relationship exists (e.g., "Feedback loops enable stability by correcting deviations")
3. Optionally, 'evidence' quoting the source material
4. Set 'bidirectional: true' for mutual relationships (e.g., similar_to, contrasts_with)

The edges are color-coded by category and styled by type (dashed for contradictions, dotted for temporal).`,
        inputSchema: {
          type: "object",
          properties: {
            title: { type: "string", description: "Title for the concept map" },
            core_concept: {
              type: "object",
              properties: {
                label: { type: "string", description: "The central concept label" },
                description: { type: "string", description: "Optional description" },
              },
              required: ["label"],
              description: "The central/main concept of the map",
            },
            concepts: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  id: { type: "string", description: "Unique identifier (slug form)" },
                  label: { type: "string", description: "Display label" },
                  level: { type: "string", enum: ["major", "detail"], description: "Hierarchy level" },
                  importance: { type: "number", description: "Importance 1-10 (affects size)" },
                  description: { type: "string", description: "Brief description" },
                },
                required: ["id", "label", "level"],
              },
              description: "The concepts discovered through analysis",
            },
            relationships: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  from: { type: "string", description: "Source concept ID (or 'core')" },
                  to: { type: "string", description: "Target concept ID" },
                  type: {
                    type: "string",
                    enum: ["causes", "enables", "prevents", "triggers", "influences", "contains", "part_of", "instance_of", "derives_from", "extends", "precedes", "follows", "co_occurs", "implies", "contradicts", "supports", "refines", "exemplifies", "similar_to", "contrasts_with", "generalizes", "specializes", "related_to"],
                    description: "Relationship type from the defined vocabulary",
                  },
                  description: { type: "string", description: "REQUIRED: Sentence explaining WHY this relationship exists" },
                  evidence: { type: "string", description: "Quote or paraphrase from source evidencing the relationship" },
                  strength: { type: "number", description: "Strength 0.0-1.0 (affects edge weight, default: 0.5)" },
                  bidirectional: { type: "boolean", description: "Whether relationship is mutual (default: false)" },
                },
                required: ["from", "to", "type", "description"],
              },
              description: "Relationships between concepts with semantic descriptions",
            },
            output: {
              type: "object",
              properties: {
                format: { type: "string", enum: ["png", "jpeg"], description: "Image format (default: png)" },
                insert_into_workflowy: { type: "string", description: "Node ID to insert the map into" },
                output_path: { type: "string", description: "Custom output file path" },
              },
              description: "Output options",
            },
          },
          required: ["title", "core_concept", "concepts", "relationships"],
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
      const { node_id, core_concept, concepts, scope, output_path, format, title } =
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

      // Validate concepts input (min/max limits)
      const validation = validateConceptMapInput(concepts);
      if (!validation.valid) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              message: validation.error,
              tip: validation.tip,
              ...(validation.provided !== undefined && { provided: validation.provided }),
              ...(validation.maximum !== undefined && { maximum: validation.maximum }),
            }, null, 2),
          }],
        };
      }

      // Normalize concepts (preserve original case for display, lowercase for matching)
      const conceptList = concepts.map(c => ({
        original: c.trim(),
        lower: c.toLowerCase().trim()
      })).filter(c => c.lower.length > 0);

      // The core concept - either explicitly provided or use the parent node name
      const coreConcept = core_concept?.trim() || sourceNode.name || "Core Concept";

      // Get nodes to analyze based on scope
      const searchScope = scope || "children";
      const scopedNodes = filterNodesByScope(sourceNode, allNodes, searchScope);
      const nodesToAnalyze = scopedNodes.length > 0
        ? scopedNodes
        : allNodes.filter((n) => n.id !== sourceNode.id);

      // Helper to calculate depth of a node from the source
      const getNodeDepth = (nodeId: string): number => {
        let depth = 0;
        let currentId = nodeId;
        let safety = 0;
        while (currentId && currentId !== sourceNode.id && safety < 100) {
          const node = allNodes.find(n => n.id === currentId);
          if (!node || !node.parent_id) break;
          currentId = node.parent_id;
          depth++;
          safety++;
        }
        return depth;
      };

      // Track concept occurrences with depth and context
      interface ConceptOccurrence {
        nodeId: string;
        nodeName: string;
        depth: number;
        fullText: string;
      }

      const conceptOccurrences = new Map<string, ConceptOccurrence[]>();

      for (const concept of conceptList) {
        conceptOccurrences.set(concept.lower, []);
      }

      // Analyze all nodes for concept occurrences
      for (const node of nodesToAnalyze) {
        const nodeText = `${node.name || ""} ${node.note || ""}`;
        const lowerText = nodeText.toLowerCase();
        const depth = getNodeDepth(node.id);

        for (const concept of conceptList) {
          if (lowerText.includes(concept.lower)) {
            conceptOccurrences.get(concept.lower)!.push({
              nodeId: node.id,
              nodeName: node.name || "Untitled",
              depth,
              fullText: nodeText,
            });
          }
        }
      }

      // Build concept map nodes with hierarchy based on occurrence depth and frequency
      const coreMapNode: ConceptMapNode = {
        id: "core",
        label: coreConcept,
        level: 0,
        occurrences: nodesToAnalyze.length, // Core represents the whole topic
        depth: 0,
      };

      const conceptMapNodes: ConceptMapNode[] = [];
      const conceptsWithOccurrences: Array<{
        concept: typeof conceptList[0];
        occurrences: ConceptOccurrence[];
        avgDepth: number;
      }> = [];

      for (const concept of conceptList) {
        const occs = conceptOccurrences.get(concept.lower) || [];
        if (occs.length > 0) {
          const avgDepth = occs.reduce((sum, o) => sum + o.depth, 0) / occs.length;
          conceptsWithOccurrences.push({ concept, occurrences: occs, avgDepth });
        }
      }

      if (conceptsWithOccurrences.length < 2) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              message: "Less than 2 concepts were found in the content. Need at least 2 to create a meaningful map.",
              concepts_searched: conceptList.map(c => c.original),
              concepts_found: conceptsWithOccurrences.map(c => ({
                concept: c.concept.original,
                occurrences: c.occurrences.length,
                avg_depth: c.avgDepth.toFixed(1),
              })),
              scope_used: searchScope,
              nodes_analyzed: nodesToAnalyze.length,
              tip: "Try different concepts or use scope: 'all' to search more broadly.",
            }, null, 2),
          }],
        };
      }

      // Sort by average depth to determine hierarchy level
      // Lower depth = closer to parent = major concept (level 1)
      // Higher depth = deeper nested = detail concept (level 2)
      conceptsWithOccurrences.sort((a, b) => a.avgDepth - b.avgDepth);
      const medianDepth = conceptsWithOccurrences[Math.floor(conceptsWithOccurrences.length / 2)]?.avgDepth || 1;

      for (const { concept, occurrences, avgDepth } of conceptsWithOccurrences) {
        const level = avgDepth <= medianDepth ? 1 : 2;
        conceptMapNodes.push({
          id: concept.lower,
          label: concept.original,
          level,
          occurrences: occurrences.length,
          depth: avgDepth,
        });
      }

      // Build edges with relationship labels
      const conceptMapEdges: ConceptMapEdge[] = [];
      const edgeMap = new Map<string, { weight: number; contexts: string[]; labels: string[] }>();

      // First, connect all concepts to the core (the main topic)
      for (const node of conceptMapNodes) {
        conceptMapEdges.push({
          from: "core",
          to: node.id,
          type: node.level === 1 ? "contains" : "contains",
          description: node.level === 1 ? "Major concept within this topic" : "Detail supporting the topic",
          weight: Math.min(node.occurrences / 10, 1),
          evidence: undefined,
          bidirectional: false,
        });
      }

      // Then, find relationships between concepts based on co-occurrence
      for (const node of nodesToAnalyze) {
        const nodeText = `${node.name || ""} ${node.note || ""}`;
        const lowerText = nodeText.toLowerCase();

        // Find which concepts appear in this node
        const presentConcepts = conceptList.filter(c => lowerText.includes(c.lower));

        if (presentConcepts.length >= 2) {
          // Create edges between all pairs of concepts in this node
          for (let i = 0; i < presentConcepts.length; i++) {
            for (let j = i + 1; j < presentConcepts.length; j++) {
              const c1 = presentConcepts[i].lower;
              const c2 = presentConcepts[j].lower;
              const edgeKey = [c1, c2].sort().join("|||");

              if (!edgeMap.has(edgeKey)) {
                edgeMap.set(edgeKey, { weight: 0, contexts: [], labels: [] });
              }

              const edge = edgeMap.get(edgeKey)!;
              edge.weight++;

              // Extract relationship label from context
              const relationLabel = extractRelationshipLabel(nodeText, c1, c2);
              if (!edge.labels.includes(relationLabel)) {
                edge.labels.push(relationLabel);
              }

              // Store context excerpt (limit to 100 chars)
              const contextExcerpt = nodeText.substring(0, 100) + (nodeText.length > 100 ? "..." : "");
              if (edge.contexts.length < 3 && !edge.contexts.includes(contextExcerpt)) {
                edge.contexts.push(contextExcerpt);
              }
            }
          }
        }
      }

      // Convert edge map to array (concept-to-concept edges)
      for (const [key, data] of edgeMap) {
        const [from, to] = key.split("|||");
        // Use the most common relationship label, or "related_to" if none found
        const label = data.labels.find(l => l !== "relates to") || data.labels[0] || "related_to";
        conceptMapEdges.push({
          from,
          to,
          type: label === "relates to" ? "related_to" : label,
          description: data.contexts[0] ? `Co-occurs in: ${data.contexts[0].substring(0, 50)}...` : "Concepts appear together",
          weight: Math.min(data.weight / 5, 1),
          evidence: data.contexts[0],
          bidirectional: true, // Co-occurrence is inherently bidirectional
        });
      }

      const imageFormat = format || "png";
      const mapTitle = title || `Concept Map: ${coreConcept}`;

      const result = await generateHierarchicalConceptMapImage(
        coreMapNode,
        conceptMapNodes,
        conceptMapEdges,
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

      // Build analysis summary
      const majorConcepts = conceptMapNodes.filter(n => n.level === 1);
      const detailConcepts = conceptMapNodes.filter(n => n.level === 2);
      const conceptConnections = conceptMapEdges.filter(e => e.from !== "core" && e.to !== "core");

      if (uploadResult.success && uploadResult.url) {
        const imageMarkdown = `![Concept Map](${uploadResult.url})`;
        const nodeNote = [
          `Core: ${coreConcept}`,
          `Major concepts: ${majorConcepts.map(c => c.label).join(", ")}`,
          `Detail concepts: ${detailConcepts.map(c => c.label).join(", ") || "none"}`,
          `Relationships: ${conceptConnections.length}`,
        ].join("\n");

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
                structure: {
                  core_concept: coreConcept,
                  major_concepts: majorConcepts.map(c => ({
                    concept: c.label,
                    found_in: c.occurrences,
                  })),
                  detail_concepts: detailConcepts.map(c => ({
                    concept: c.label,
                    found_in: c.occurrences,
                  })),
                },
                relationships: conceptConnections.slice(0, 10).map(e => ({
                  between: [e.from, e.to],
                  relationship: e.type,
                  description: e.description,
                  strength: e.weight,
                })),
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
              parent_node: { id: sourceNode.id, name: sourceNode.name },
              structure: {
                core_concept: coreConcept,
                major_concepts: majorConcepts.map(c => c.label),
                detail_concepts: detailConcepts.map(c => c.label),
                total_relationships: conceptConnections.length,
              },
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

    case "render_concept_map": {
      const { title, core_concept, concepts, relationships, output } =
        renderConceptMapSchema.parse(args);

      // Validate we have enough concepts
      if (concepts.length < 2) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: "At least 2 concepts are required to create a concept map",
              provided: concepts.length,
            }, null, 2),
          }],
        };
      }

      if (concepts.length > 35) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: "Too many concepts - maximum is 35 for readability",
              provided: concepts.length,
              maximum: 35,
            }, null, 2),
          }],
        };
      }

      // Build concept map nodes
      const coreMapNode: ConceptMapNode = {
        id: "core",
        label: core_concept.label,
        level: 0,
        occurrences: 10, // Core is always prominent
        depth: 0,
      };

      const conceptMapNodes: ConceptMapNode[] = concepts.map((c) => ({
        id: c.id,
        label: c.label,
        level: c.level === "major" ? 1 : 2,
        occurrences: c.importance || 5,
        depth: c.level === "major" ? 1 : 2,
      }));

      // Build edges with enriched relationship data
      const conceptMapEdges: ConceptMapEdge[] = relationships.map((r) => ({
        from: r.from,
        to: r.to,
        type: r.type,
        description: r.description,
        weight: r.strength ?? 0.5,
        evidence: r.evidence,
        bidirectional: r.bidirectional ?? false,
      }));

      // Generate the image
      const imageFormat = output?.format || "png";
      const result = await generateHierarchicalConceptMapImage(
        coreMapNode,
        conceptMapNodes,
        conceptMapEdges,
        title,
        imageFormat
      );

      if (!result.success || !result.buffer) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: false,
              error: result.error || "Failed to generate concept map image",
            }, null, 2),
          }],
          isError: true,
        };
      }

      const timestamp = Date.now();
      const filename = `concept-map-${timestamp}.${imageFormat}`;

      // Try to upload to Dropbox
      const uploadResult = await uploadToDropbox(result.buffer, filename);

      const majorConcepts = concepts.filter((c) => c.level === "major");
      const detailConcepts = concepts.filter((c) => c.level === "detail");

      // If we should insert into Workflowy
      if (uploadResult.success && uploadResult.url && output?.insert_into_workflowy) {
        const allNodes = await getCachedNodes();
        const targetNode = allNodes.find((n) => n.id === output.insert_into_workflowy);

        if (targetNode) {
          const imageMarkdown = `![Concept Map](${uploadResult.url})`;
          const nodeNote = [
            `Core: ${core_concept.label}`,
            `Major concepts: ${majorConcepts.map((c) => c.label).join(", ")}`,
            `Detail concepts: ${detailConcepts.map((c) => c.label).join(", ") || "none"}`,
            `Relationships: ${relationships.length}`,
          ].join("\n");

          try {
            await workflowyRequest("/nodes", "POST", {
              parent_id: targetNode.id,
              name: `📊 ${title}`,
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
                  image_url: uploadResult.url,
                  inserted_into: { id: targetNode.id, name: targetNode.name },
                  stats: {
                    concepts_rendered: concepts.length,
                    major_concepts: majorConcepts.length,
                    detail_concepts: detailConcepts.length,
                    relationships_rendered: relationships.length,
                  },
                }, null, 2),
              }],
            };
          } catch (insertError) {
            // Continue to return success even if insert failed
            return {
              content: [{
                type: "text",
                text: JSON.stringify({
                  success: true,
                  message: "Concept map created but failed to insert into Workflowy",
                  image_url: uploadResult.url,
                  insert_error: insertError instanceof Error ? insertError.message : String(insertError),
                  stats: {
                    concepts_rendered: concepts.length,
                    major_concepts: majorConcepts.length,
                    detail_concepts: detailConcepts.length,
                    relationships_rendered: relationships.length,
                  },
                }, null, 2),
              }],
            };
          }
        }
      }

      // If Dropbox upload succeeded but no Workflowy insert requested
      if (uploadResult.success && uploadResult.url) {
        return {
          content: [{
            type: "text",
            text: JSON.stringify({
              success: true,
              message: "Concept map created",
              image_url: uploadResult.url,
              stats: {
                concepts_rendered: concepts.length,
                major_concepts: majorConcepts.length,
                detail_concepts: detailConcepts.length,
                relationships_rendered: relationships.length,
              },
            }, null, 2),
          }],
        };
      }

      // Fallback to local file
      const defaultPath = path.join(
        process.env.HOME || "/tmp",
        "Downloads",
        filename
      );
      const finalPath = output?.output_path || defaultPath;

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
            stats: {
              concepts_rendered: concepts.length,
              major_concepts: majorConcepts.length,
              detail_concepts: detailConcepts.length,
              relationships_rendered: relationships.length,
            },
          }, null, 2),
        }],
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
