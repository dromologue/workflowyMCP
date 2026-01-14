#!/usr/bin/env node
/**
 * CLI tool for rendering concept maps from JSON definitions
 *
 * This tool takes a JSON file or stdin containing concept definitions and relationships,
 * then renders them as a visual concept map in PNG, JPEG, or PDF format.
 *
 * Usage:
 *   npx tsx src/cli/render-concept-map.ts --input concepts.json --output map.png
 *   npx tsx src/cli/render-concept-map.ts --input concepts.json --format pdf
 *   cat concepts.json | npx tsx src/cli/render-concept-map.ts --output map.pdf
 *   npx tsx src/cli/render-concept-map.ts --example > heidegger.json
 *
 * JSON format:
 * {
 *   "title": "Map Title",
 *   "core_concept": { "label": "Central Concept" },
 *   "concepts": [
 *     { "id": "concept-id", "label": "Display Label", "level": "major", "importance": 8 }
 *   ],
 *   "relationships": [
 *     {
 *       "from": "core", "to": "concept-id", "type": "enables",
 *       "description": "Explanation of why this relationship exists"
 *     }
 *   ]
 * }
 */

import { Command } from "commander";
import { Graphviz } from "@hpcc-js/wasm-graphviz";
import sharp from "sharp";
import * as fs from "fs";
import * as path from "path";

// ============================================================================
// Types
// ============================================================================

type RelationshipType =
  | "causes" | "enables" | "prevents" | "triggers" | "influences"
  | "contains" | "part_of" | "instance_of" | "derives_from" | "extends"
  | "precedes" | "follows" | "co_occurs"
  | "implies" | "contradicts" | "supports" | "refines" | "exemplifies"
  | "similar_to" | "contrasts_with" | "generalizes" | "specializes"
  | "related_to";

interface ConceptInput {
  id: string;
  label: string;
  level: "major" | "detail";
  importance?: number;
  description?: string;
}

interface RelationshipInput {
  from: string;
  to: string;
  type: RelationshipType;
  description: string;
  evidence?: string;
  strength?: number;
  bidirectional?: boolean;
}

interface ConceptMapDefinition {
  title: string;
  core_concept: {
    label: string;
    description?: string;
  };
  concepts: ConceptInput[];
  relationships: RelationshipInput[];
}

interface ConceptMapNode {
  id: string;
  label: string;
  level: number;
  occurrences: number;
}

interface ConceptMapEdge {
  from: string;
  to: string;
  type: string;
  description: string;
  weight: number;
  evidence?: string;
  bidirectional: boolean;
}

// ============================================================================
// DOT Generation Helpers
// ============================================================================

function escapeForDot(text: string): string {
  return text
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n");
}

function formatRelationType(type: string): string {
  return type.replace(/_/g, " ");
}

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

function buildEdgeLabel(_type: string, description: string): string {
  // Use the description directly - it should already be a short, complete sentence
  // The relationship type is conveyed through edge color and style
  return description;
}

function getEdgeColor(type: string): string {
  if (["causes", "enables", "prevents", "triggers", "influences"].includes(type)) {
    return "#2980b9";
  }
  if (["contains", "part_of", "instance_of", "derives_from", "extends"].includes(type)) {
    return "#27ae60";
  }
  if (["precedes", "follows", "co_occurs"].includes(type)) {
    return "#e67e22";
  }
  if (["implies", "supports", "refines", "exemplifies"].includes(type)) {
    return "#8e44ad";
  }
  if (["contradicts", "contrasts_with"].includes(type)) {
    return "#c0392b";
  }
  if (["similar_to", "generalizes", "specializes"].includes(type)) {
    return "#16a085";
  }
  return "#566573";
}

function getEdgeStyle(type: string): string {
  if (["contradicts", "contrasts_with", "prevents"].includes(type)) {
    return "dashed";
  }
  if (["precedes", "follows", "co_occurs"].includes(type)) {
    return "dotted";
  }
  if (["causes", "implies", "derives_from"].includes(type)) {
    return "bold";
  }
  return "solid";
}

// ============================================================================
// DOT Graph Generation
// ============================================================================

function generateConceptMapDot(
  coreNode: ConceptMapNode,
  conceptNodes: ConceptMapNode[],
  edges: ConceptMapEdge[],
  title: string,
  options: { width: number; height: number; fontSize: number }
): string {
  const { width, height, fontSize } = options;

  const lines: string[] = [
    "digraph ConceptMap {",
    '  charset="UTF-8";',
    // Use sfdp for better scaling with larger graphs
    '  layout=sfdp;',
    // Prevent overlap with scaling - higher values push nodes apart more
    '  overlap=prism;',
    '  overlap_scaling=6;',
    // Use curved splines - better label placement than polyline
    '  splines=curved;',
    // Increase separation significantly for label space
    '  sep="+100,100";',
    '  K=4;',
    '  repulsiveforce=3.0;',
    // Force external labels to be shown even if they might overlap
    '  forcelabels=true;',
    // Much larger graph dimensions
    `  size="${width},${height}";`,
    '  ratio=fill;',
    '  bgcolor="white";',
    '  pad="1.5";',
    '  margin="1.5";',
    // Title
    `  label="${escapeForDot(title)}";`,
    '  labelloc="t";',
    `  fontsize=${fontSize * 1.5};`,
    '  fontname="Helvetica Bold";',
    "",
    "  // Global node styling",
    `  node [shape=box, style="rounded,filled", fontname="Helvetica", margin="0.4,0.2", fontsize=${fontSize}];`,
    "",
    "  // Global edge styling - use xlabel for external labels that don't overlap edges",
    `  edge [fontname="Helvetica", fontsize=${fontSize * 0.9}];`,
    "",
  ];

  // Core concept - largest, distinctive color
  lines.push("  // Core concept (center)");
  lines.push(
    `  "${coreNode.id}" [label="${escapeForDot(coreNode.label)}", fillcolor="#1a5276", fontcolor="white", fontsize=${fontSize * 1.3}, penwidth=4, width=4, height=1.2];`
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
      const nodeWidth = Math.max(2.5, Math.min(2.5 + node.occurrences * 0.15, 3.5));
      lines.push(
        `  "${node.id}" [label="${escapeForDot(node.label)}", fillcolor="${color}", fontcolor="white", fontsize=${fontSize}, width=${nodeWidth}, height=1];`
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
      const nodeWidth = Math.max(2.0, Math.min(2.0 + node.occurrences * 0.1, 2.8));
      lines.push(
        `  "${node.id}" [label="${escapeForDot(node.label)}", fillcolor="${color}", fontcolor="#1a1a1a", fontsize=${fontSize * 0.9}, width=${nodeWidth}, height=0.8];`
      );
    });
    lines.push("");
  }

  // Edges with enriched relationship labels
  lines.push("  // Relationships with semantic labels");
  const addedEdges = new Set<string>();

  edges.forEach((edge) => {
    const edgeKey = edge.bidirectional
      ? [edge.from, edge.to].sort().join("|||")
      : `${edge.from}|||${edge.to}`;
    if (addedEdges.has(edgeKey)) return;
    addedEdges.add(edgeKey);

    const penwidth = Math.max(1.5, Math.min(1.5 + edge.weight * 4, 5));
    const color = getEdgeColor(edge.type);
    const style = getEdgeStyle(edge.type);
    const label = buildEdgeLabel(edge.type, edge.description);

    // Use xlabel (external label) to position label away from the edge line
    // This prevents labels from overlapping with edges and nodes
    const attrs: string[] = [
      `xlabel="${escapeForDot(label)}"`,
      `fontsize=${fontSize * 0.9}`,
      `penwidth=${penwidth}`,
      `color="${color}"`,
      `fontcolor="${color}"`,
      `style="${style}"`,
      `len=4`, // Longer edge length for more label space
    ];

    if (edge.bidirectional) {
      attrs.push(`dir=both`);
      attrs.push(`arrowhead=normal`);
      attrs.push(`arrowtail=normal`);
    }

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

// ============================================================================
// Image Generation
// ============================================================================

async function generateImage(
  dotGraph: string,
  format: "png" | "jpeg" | "pdf",
  outputWidth: number,
  outputHeight: number,
  dpi: number
): Promise<Buffer> {
  const graphviz = await Graphviz.load();
  const svg = graphviz.dot(dotGraph, "svg");

  if (format === "pdf") {
    // For PDF, we'll create a high-res PNG first then note that PDF would need additional library
    // Sharp doesn't directly support PDF, so we output SVG for PDF use case
    const imageBuffer = await sharp(Buffer.from(svg), { density: dpi })
      .resize(outputWidth, outputHeight, {
        fit: "inside",
        withoutEnlargement: false,
      })
      .flatten({ background: "#ffffff" })
      .png()
      .toBuffer();
    return imageBuffer;
  }

  const imageBuffer = await sharp(Buffer.from(svg), { density: dpi })
    .resize(outputWidth, outputHeight, {
      fit: "inside",
      withoutEnlargement: false,
    })
    .flatten({ background: "#ffffff" })
    [format]({
      quality: format === "jpeg" ? 95 : undefined,
    })
    .toBuffer();

  return imageBuffer;
}

// ============================================================================
// Example Data
// ============================================================================

function getHeideggerExample(): ConceptMapDefinition {
  return {
    title: "Heidegger's Fundamental Ontology",
    core_concept: {
      label: "Being (Sein)",
      description: "The central question of Heidegger's philosophy",
    },
    concepts: [
      { id: "dasein", label: "Dasein", level: "major", importance: 9 },
      { id: "being-in-world", label: "Being-in-the-World", level: "major", importance: 8 },
      { id: "temporality", label: "Temporality", level: "major", importance: 8 },
      { id: "authenticity", label: "Authenticity", level: "major", importance: 7 },
      { id: "das-man", label: "Das Man (The They)", level: "detail", importance: 6 },
      { id: "thrownness", label: "Thrownness", level: "detail", importance: 5 },
      { id: "care", label: "Care (Sorge)", level: "detail", importance: 6 },
      { id: "being-toward-death", label: "Being-toward-Death", level: "detail", importance: 5 },
      { id: "aletheia", label: "Aletheia (Unconcealment)", level: "detail", importance: 5 },
    ],
    relationships: [
      {
        from: "core",
        to: "dasein",
        type: "enables",
        description: "Being is disclosed through Dasein's unique capacity for understanding",
        strength: 0.9,
      },
      {
        from: "dasein",
        to: "being-in-world",
        type: "derives_from",
        description: "Dasein's essential structure is always already being-in-the-world",
        strength: 0.85,
      },
      {
        from: "dasein",
        to: "temporality",
        type: "derives_from",
        description: "Dasein's being is fundamentally temporal, ecstatic toward past and future",
        strength: 0.85,
      },
      {
        from: "temporality",
        to: "care",
        type: "enables",
        description: "Temporality is the ontological meaning of care's threefold structure",
        strength: 0.8,
      },
      {
        from: "dasein",
        to: "authenticity",
        type: "enables",
        description: "Dasein can choose to exist authentically or fall into inauthenticity",
        strength: 0.75,
      },
      {
        from: "authenticity",
        to: "das-man",
        type: "contrasts_with",
        description: "Authenticity requires breaking free from the anonymous they-self",
        strength: 0.7,
        bidirectional: true,
      },
      {
        from: "being-toward-death",
        to: "authenticity",
        type: "enables",
        description: "Confronting finitude enables authentic self-ownership",
        strength: 0.8,
      },
      {
        from: "thrownness",
        to: "being-in-world",
        type: "part_of",
        description: "Thrownness is a fundamental existentiale of being-in-the-world",
        strength: 0.7,
      },
      {
        from: "care",
        to: "being-in-world",
        type: "contains",
        description: "Care unifies the structural moments of being-in-the-world",
        strength: 0.75,
      },
      {
        from: "core",
        to: "aletheia",
        type: "enables",
        description: "Being shows itself through unconcealment, the original meaning of truth",
        strength: 0.8,
      },
      {
        from: "das-man",
        to: "thrownness",
        type: "influences",
        description: "We are thrown into a world already interpreted by the they",
        strength: 0.6,
      },
    ],
  };
}

// ============================================================================
// CLI
// ============================================================================

const program = new Command();

program
  .name("render-concept-map")
  .description("Render concept maps from JSON definitions to PNG, JPEG, or PDF")
  .version("1.0.0")
  .option("-i, --input <file>", "Input JSON file (use - for stdin)")
  .option("-o, --output <file>", "Output file path")
  .option("-f, --format <type>", "Output format: png, jpeg, or pdf", "png")
  .option("-w, --width <pixels>", "Output width in pixels", "4000")
  .option("-h, --height <pixels>", "Output height in pixels", "3000")
  .option("-d, --dpi <number>", "DPI for rendering", "300")
  .option("--font-size <number>", "Base font size", "18")
  .option("--example", "Output example JSON to stdout")
  .option("--dot", "Output DOT graph source instead of image")
  .option("--svg", "Output SVG instead of rasterized image")
  .parse(process.argv);

const options = program.opts();

async function main() {
  // Handle --example flag
  if (options.example) {
    console.log(JSON.stringify(getHeideggerExample(), null, 2));
    return;
  }

  // Read input
  let inputJson: string;

  if (!options.input || options.input === "-") {
    // Read from stdin
    const chunks: Buffer[] = [];
    for await (const chunk of process.stdin) {
      chunks.push(chunk);
    }
    inputJson = Buffer.concat(chunks).toString("utf-8");
  } else {
    // Read from file
    if (!fs.existsSync(options.input)) {
      console.error(`Error: Input file not found: ${options.input}`);
      process.exit(1);
    }
    inputJson = fs.readFileSync(options.input, "utf-8");
  }

  // Parse JSON
  let definition: ConceptMapDefinition;
  try {
    definition = JSON.parse(inputJson) as ConceptMapDefinition;
  } catch (e) {
    console.error("Error: Invalid JSON input");
    console.error(e instanceof Error ? e.message : String(e));
    process.exit(1);
  }

  // Validate
  if (!definition.title || !definition.core_concept || !definition.concepts || !definition.relationships) {
    console.error("Error: JSON must contain title, core_concept, concepts, and relationships");
    process.exit(1);
  }

  if (definition.concepts.length < 2) {
    console.error("Error: At least 2 concepts are required");
    process.exit(1);
  }

  console.error(`Rendering: ${definition.title}`);
  console.error(`  Core: ${definition.core_concept.label}`);
  console.error(`  Concepts: ${definition.concepts.length}`);
  console.error(`  Relationships: ${definition.relationships.length}`);

  // Build internal structures
  const coreNode: ConceptMapNode = {
    id: "core",
    label: definition.core_concept.label,
    level: 0,
    occurrences: 10,
  };

  const conceptNodes: ConceptMapNode[] = definition.concepts.map((c) => ({
    id: c.id,
    label: c.label,
    level: c.level === "major" ? 1 : 2,
    occurrences: c.importance || 5,
  }));

  const edges: ConceptMapEdge[] = definition.relationships.map((r) => ({
    from: r.from,
    to: r.to,
    type: r.type,
    description: r.description,
    weight: r.strength ?? 0.5,
    evidence: r.evidence,
    bidirectional: r.bidirectional ?? false,
  }));

  // Generate DOT
  const width = parseInt(options.width, 10);
  const height = parseInt(options.height, 10);
  const fontSize = parseInt(options.fontSize, 10);
  const dpi = parseInt(options.dpi, 10);

  // Scale graph dimensions based on output size
  const graphWidth = width / 100;
  const graphHeight = height / 100;

  const dotGraph = generateConceptMapDot(coreNode, conceptNodes, edges, definition.title, {
    width: graphWidth,
    height: graphHeight,
    fontSize,
  });

  // Handle --dot output
  if (options.dot) {
    console.log(dotGraph);
    return;
  }

  // Handle --svg output
  if (options.svg) {
    const graphviz = await Graphviz.load();
    const svg = graphviz.dot(dotGraph, "svg");
    if (options.output) {
      fs.writeFileSync(options.output, svg);
      console.error(`SVG saved to: ${options.output}`);
    } else {
      console.log(svg);
    }
    return;
  }

  // Generate image
  const format = options.format as "png" | "jpeg" | "pdf";
  const imageBuffer = await generateImage(dotGraph, format, width, height, dpi);

  // Determine output path
  let outputPath: string;
  if (options.output) {
    outputPath = options.output;
  } else {
    const timestamp = Date.now();
    const safeName = definition.title.toLowerCase().replace(/[^a-z0-9]+/g, "-").substring(0, 30);
    const ext = format === "pdf" ? "png" : format; // Note: PDF outputs as high-res PNG
    outputPath = path.join(process.cwd(), `concept-map-${safeName}-${timestamp}.${ext}`);
  }

  // Ensure directory exists
  const dir = path.dirname(outputPath);
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }

  // Write output
  fs.writeFileSync(outputPath, imageBuffer);

  console.error(`\nConcept map saved to: ${outputPath}`);
  console.error(`  Size: ${(imageBuffer.length / 1024).toFixed(1)} KB`);
  console.error(`  Dimensions: ${width}x${height} @ ${dpi} DPI`);

  if (format === "pdf") {
    console.error(`  Note: PDF format outputs as high-resolution PNG. For true PDF, use --svg and convert.`);
  }
}

main().catch((err) => {
  console.error("Error:", err.message);
  process.exit(1);
});
