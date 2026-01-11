#!/usr/bin/env node
/**
 * CLI tool for generating concept maps from Workflowy content
 * Uses Claude API to intelligently extract and relate concepts
 *
 * Usage:
 *   npx tsx src/cli/concept-map.ts --node-id <id> --core "Event" --concepts "Being,Truth,Subject"
 *   npx tsx src/cli/concept-map.ts --node-id <id> --core "Event" --auto  # Claude extracts concepts
 *   npx tsx src/cli/concept-map.ts --search "Conceptual Foundations" --core "Event" --auto
 *   npx tsx src/cli/concept-map.ts --setup  # Interactive credential setup
 */

import "dotenv/config";
import { Command } from "commander";
import Anthropic from "@anthropic-ai/sdk";
import { Graphviz } from "@hpcc-js/wasm-graphviz";
import sharp from "sharp";
import { writeFileSync } from "fs";
import { join } from "path";
import { workflowyRequest } from "../api/workflowy.js";
import { escapeForDot } from "../utils/text-processing.js";
import { ensureCredentials, runSetup } from "./setup.js";

interface WorkflowyNode {
  id: string;
  name: string;
  note?: string;
  parent_id?: string;
}

interface ConceptData {
  id: string;
  label: string;
  occurrences: number;
}

const program = new Command();

program
  .name("concept-map")
  .description("Generate concept maps from Workflowy content using Claude AI")
  .version("1.0.0")
  .option("-n, --node-id <id>", "Workflowy node ID to analyze")
  .option("-s, --search <query>", "Search for node by name instead of ID")
  .option("-c, --core <concept>", "Core concept for the map center", "Main Concept")
  .option("-C, --concepts <list>", "Comma-separated list of concepts to map")
  .option("-a, --auto", "Use Claude to automatically extract relevant concepts")
  .option("-o, --output <filename>", "Output filename (default: concept-map-<timestamp>.png)")
  .option("-f, --format <type>", "Output format: png or jpeg", "png")
  .option("--no-claude", "Skip Claude analysis, use provided concepts only")
  .option("--setup", "Run interactive credential setup")
  .parse(process.argv);

const options = program.opts();

async function findNodeBySearch(query: string, nodes: WorkflowyNode[]): Promise<WorkflowyNode | null> {
  const lowerQuery = query.toLowerCase();

  // Exact match first
  let match = nodes.find(n => n.name?.toLowerCase() === lowerQuery);
  if (match) return match;

  // Contains match
  match = nodes.find(n => n.name?.toLowerCase().includes(lowerQuery));
  return match || null;
}

function getDescendants(parentId: string, nodes: WorkflowyNode[]): WorkflowyNode[] {
  const children = nodes.filter(n => n.parent_id === parentId);
  const descendants: WorkflowyNode[] = [...children];
  for (const child of children) {
    descendants.push(...getDescendants(child.id, nodes));
  }
  return descendants;
}

async function extractConceptsWithClaude(
  content: string,
  coreConcept: string
): Promise<string[]> {
  const apiKey = process.env.ANTHROPIC_API_KEY;
  if (!apiKey) {
    console.error("Warning: ANTHROPIC_API_KEY not set, cannot use Claude for concept extraction");
    return [];
  }

  const client = new Anthropic({ apiKey });

  console.log("Asking Claude to extract relevant concepts...");

  const response = await client.messages.create({
    model: "claude-sonnet-4-20250514",
    max_tokens: 1024,
    messages: [{
      role: "user",
      content: `Analyze this content and extract 15-25 key philosophical/theoretical concepts that relate to "${coreConcept}".

Return ONLY a JSON array of concept names, no explanation. Focus on:
- Named theories, frameworks, or ideas
- Technical terms specific to the domain
- Key thinkers or figures mentioned
- Core abstractions and their relationships

Content to analyze:
${content.substring(0, 15000)}

Return format: ["concept1", "concept2", ...]`
    }]
  });

  try {
    const text = response.content[0].type === "text" ? response.content[0].text : "";
    // Extract JSON array from response
    const match = text.match(/\[[\s\S]*\]/);
    if (match) {
      const concepts = JSON.parse(match[0]) as string[];
      console.log(`Claude extracted ${concepts.length} concepts`);
      return concepts;
    }
  } catch (e) {
    console.error("Failed to parse Claude response:", e);
  }
  return [];
}

function generateDotGraph(
  coreConcept: string,
  majorConcepts: ConceptData[],
  detailConcepts: ConceptData[],
  edges: { from: string; to: string; weight: number }[]
): string {
  const coreId = "core";

  const lines: string[] = [
    "digraph ConceptMap {",
    '  charset="UTF-8";',
    '  layout=neato;',
    '  overlap=false;',
    '  splines=true;',
    '  sep="+25";',
    '  ratio=1;',
    '  size="14,14!";',
    '  bgcolor="white";',
    `  label="${escapeForDot(coreConcept)}: Concept Map";`,
    '  labelloc="t";',
    '  fontsize=32;',
    '  fontname="Arial Bold";',
    "",
    '  node [shape=box, style="rounded,filled", fontname="Arial"];',
    "",
    `  "${coreId}" [label="${escapeForDot(coreConcept)}", fillcolor="#1a5276", fontcolor="white", fontsize=18, penwidth=3, width=2.5, pos="7,7!", pin=true];`,
    "",
  ];

  // Major concepts
  const majorColors = ["#2874a6", "#1e8449", "#b9770e", "#6c3483", "#1abc9c", "#c0392b", "#2980b9", "#27ae60"];
  majorConcepts.forEach((node, i) => {
    const color = majorColors[i % majorColors.length];
    const width = Math.max(1.8, Math.min(1.8 + node.occurrences * 0.05, 2.4));
    lines.push(`  "${node.id}" [label="${escapeForDot(node.label)}", fillcolor="${color}", fontcolor="white", fontsize=14, width=${width}];`);
  });

  // Detail concepts
  const detailColors = ["#5dade2", "#58d68d", "#f4d03f", "#bb8fce", "#76d7c4", "#f1948a", "#85c1e9", "#82e0aa"];
  detailConcepts.forEach((node, i) => {
    const color = detailColors[i % detailColors.length];
    const width = Math.max(1.2, Math.min(1.2 + node.occurrences * 0.04, 1.8));
    lines.push(`  "${node.id}" [label="${escapeForDot(node.label)}", fillcolor="${color}", fontcolor="#1a1a1a", fontsize=12, width=${width}];`);
  });

  lines.push("");

  // Add edges
  const addedEdges = new Set<string>();
  const significantEdges = edges.filter(e => e.weight >= 1).slice(0, 50);

  for (const edge of significantEdges) {
    const key = [edge.from, edge.to].sort().join("|||");
    if (addedEdges.has(key)) continue;
    addedEdges.add(key);

    const penwidth = Math.min(1 + edge.weight * 0.3, 3);
    lines.push(`  "${edge.from}" -> "${edge.to}" [penwidth=${penwidth}, color="#566573", dir=none];`);
  }

  lines.push("}");
  return lines.join("\n");
}

async function main() {
  // Handle --setup flag
  if (options.setup) {
    await runSetup();
    return;
  }

  // Ensure credentials are configured (prompts if missing)
  const hasCredentials = await ensureCredentials();
  if (!hasCredentials) {
    console.error("\nRun with --setup to configure credentials");
    process.exit(1);
  }

  // Validate inputs
  if (!options.nodeId && !options.search) {
    console.error("Error: Must provide either --node-id or --search");
    process.exit(1);
  }

  if (!options.concepts && !options.auto) {
    console.error("Error: Must provide either --concepts or --auto");
    process.exit(1);
  }

  console.log("Fetching nodes from Workflowy...");
  const response = await workflowyRequest("/nodes-export", "GET") as { nodes: WorkflowyNode[] };
  const allNodes = response.nodes;
  console.log(`Found ${allNodes.length} total nodes`);

  // Find target node
  let targetNode: WorkflowyNode | null = null;

  if (options.nodeId) {
    targetNode = allNodes.find(n => n.id === options.nodeId) || null;
  } else if (options.search) {
    targetNode = await findNodeBySearch(options.search, allNodes);
  }

  if (!targetNode) {
    console.error(`Error: Could not find node ${options.nodeId || options.search}`);
    process.exit(1);
  }

  console.log(`Analyzing: "${targetNode.name}" (${targetNode.id})`);

  // Get descendants
  const descendants = getDescendants(targetNode.id, allNodes);
  console.log(`Found ${descendants.length} descendant nodes`);

  // Build content for analysis
  const contentParts = descendants.map(n => `${n.name || ""}\n${n.note || ""}`);
  const fullContent = contentParts.join("\n\n");

  // Get concepts
  let concepts: string[] = [];

  if (options.concepts) {
    concepts = options.concepts.split(",").map((c: string) => c.trim());
  }

  if (options.auto && options.claude !== false) {
    const claudeConcepts = await extractConceptsWithClaude(fullContent, options.core);
    concepts = [...new Set([...concepts, ...claudeConcepts])];
  }

  if (concepts.length < 2) {
    console.error("Error: Need at least 2 concepts to generate a map");
    process.exit(1);
  }

  console.log(`\nMapping ${concepts.length} concepts: ${concepts.slice(0, 10).join(", ")}${concepts.length > 10 ? "..." : ""}`);

  // Normalize and count occurrences
  const conceptList = concepts.map(c => ({
    original: c,
    lower: c.toLowerCase(),
    id: c.toLowerCase().replace(/\s+/g, "_").replace(/[^a-z0-9_]/g, "")
  }));

  const conceptOccurrences = new Map<string, number>();
  for (const c of conceptList) {
    conceptOccurrences.set(c.lower, 0);
  }

  // Count occurrences
  for (const node of descendants) {
    const text = `${node.name || ""} ${node.note || ""}`.toLowerCase();
    for (const concept of conceptList) {
      if (text.includes(concept.lower)) {
        conceptOccurrences.set(concept.lower, (conceptOccurrences.get(concept.lower) || 0) + 1);
      }
    }
  }

  // Filter to concepts with occurrences and sort
  const foundConcepts = conceptList
    .filter(c => c.lower !== options.core.toLowerCase())
    .map(c => ({
      id: c.id,
      label: c.original,
      occurrences: conceptOccurrences.get(c.lower) || 0
    }))
    .filter(c => c.occurrences > 0)
    .sort((a, b) => b.occurrences - a.occurrences);

  console.log(`\nFound ${foundConcepts.length} concepts with occurrences in content`);

  if (foundConcepts.length < 2) {
    console.error("Error: Not enough concepts found in content");
    process.exit(1);
  }

  // Split into major and detail
  const majorConcepts = foundConcepts.slice(0, 8);
  const detailConcepts = foundConcepts.slice(8, 16);

  // Build edges
  const edges: { from: string; to: string; weight: number }[] = [];
  const coreId = "core";

  // Connect all to core
  for (const c of [...majorConcepts, ...detailConcepts]) {
    edges.push({ from: coreId, to: c.id, weight: c.occurrences });
  }

  // Find co-occurrences
  for (const node of descendants) {
    const text = `${node.name || ""} ${node.note || ""}`.toLowerCase();
    const present = conceptList.filter(c => text.includes(c.lower));

    if (present.length >= 2) {
      for (let i = 0; i < present.length; i++) {
        for (let j = i + 1; j < present.length; j++) {
          const id1 = present[i].id;
          const id2 = present[j].id;
          if (id1 === coreId || id2 === coreId) continue;

          const existing = edges.find(e =>
            (e.from === id1 && e.to === id2) || (e.from === id2 && e.to === id1)
          );
          if (existing) {
            existing.weight++;
          } else {
            edges.push({ from: id1, to: id2, weight: 1 });
          }
        }
      }
    }
  }

  // Generate graph
  console.log("\nGenerating concept map...");
  const dotGraph = generateDotGraph(options.core, majorConcepts, detailConcepts, edges);

  // Render image
  console.log("Rendering graph...");
  const graphviz = await Graphviz.load();
  const svg = graphviz.dot(dotGraph, "svg");

  const format = options.format as "png" | "jpeg";
  const imageBuffer = await sharp(Buffer.from(svg), { density: 300 })
    .resize(2000, 2000, { fit: "inside", withoutEnlargement: false })
    .flatten({ background: "#ffffff" })
    [format]({ quality: format === "jpeg" ? 95 : undefined })
    .toBuffer();

  // Save to current directory
  const timestamp = Date.now();
  const filename = options.output || `concept-map-${options.core.toLowerCase().replace(/\s+/g, "-")}-${timestamp}.${format}`;
  const outputPath = join(process.cwd(), filename);

  writeFileSync(outputPath, imageBuffer);

  console.log(`\n✅ Concept map saved to: ${outputPath}`);
  console.log(`   Size: ${(imageBuffer.length / 1024).toFixed(1)} KB`);
  console.log(`   Concepts mapped: ${foundConcepts.length}`);
  console.log(`   Major: ${majorConcepts.map(c => c.label).join(", ")}`);
  if (detailConcepts.length > 0) {
    console.log(`   Detail: ${detailConcepts.map(c => c.label).join(", ")}`);
  }
}

main().catch(err => {
  console.error("Error:", err.message);
  process.exit(1);
});
