#!/usr/bin/env node
/**
 * CLI tool for generating interactive concept maps from Workflowy content.
 * Uses Claude API for semantic analysis and generates self-contained HTML.
 *
 * Usage:
 *   npm run concept-map -- --search "Topic" --auto
 *   npm run concept-map -- --search "Topic" --auto --depth 3
 *   npm run concept-map -- --search "Topic" --auto --depth 3 --insert
 *   npm run concept-map -- --node-id <id> --auto
 *   npm run concept-map -- --search "Topic" --core "Center" --concepts "A,B,C"
 *   npm run concept-map -- --setup
 */

import "dotenv/config";
import { Command } from "commander";
import Anthropic from "@anthropic-ai/sdk";
import { writeFileSync } from "fs";
import { join } from "path";
import { workflowyRequest, createNode } from "../shared/api/workflowy.js";
import { generateInteractiveConceptMapHTML } from "../shared/utils/concept-map-html.js";
import type { InteractiveConcept, InteractiveRelationship } from "../shared/utils/concept-map-html.js";
import { uploadToDropboxPath, isDropboxConfigured } from "../shared/api/dropbox.js";
import { findTasksNode } from "../shared/utils/task-map.js";
import { ensureCredentials, runSetup } from "./setup.js";
import { insertConceptMapOutline } from "./concept-map-outline.js";
import type { WorkflowyNode, ClaudeAnalysis } from "../shared/types/index.js";

const program = new Command();

program
  .name("concept-map")
  .description("Generate interactive concept maps from Workflowy content using Claude AI")
  .version("2.0.0")
  .option("-n, --node-id <id>", "Workflowy node ID to analyze")
  .option("-s, --search <query>", "Search for node by name instead of ID")
  .option("-d, --depth <number>", "Maximum depth of children to include (default: unlimited)")
  .option("-c, --core <concept>", "Core concept label (default: auto-detected)")
  .option("-C, --concepts <list>", "Comma-separated list of concepts (skips Claude analysis)")
  .option("-a, --auto", "Use Claude to automatically discover concepts and relationships")
  .option("-o, --output <filename>", "Output filename (default: concept-map-<slug>-<timestamp>.html)")
  .option("-i, --insert", "Insert concept map outline as Workflowy nodes (sibling of analyzed node)")
  .option("--force", "Overwrite existing concept map outline (use with --insert)")
  .option("--setup", "Run interactive credential setup")
  .parse(process.argv);

const options = program.opts();

function findNodeBySearch(query: string, nodes: WorkflowyNode[]): WorkflowyNode | null {
  const lowerQuery = query.toLowerCase();
  // Strip HTML for comparison
  const clean = (s: string) => (s || "").replace(/<[^>]*>/g, "").trim().toLowerCase();

  // Exact match first (ignoring HTML tags)
  let match = nodes.find(n => clean(n.name) === lowerQuery);
  if (match) return match;

  // Contains match, but skip "Concept Map - ..." outline nodes
  match = nodes.find(n => {
    const name = clean(n.name);
    if (name.startsWith("concept map - ")) return false;
    return name.includes(lowerQuery);
  });
  return match || null;
}

function getDescendants(
  parentId: string,
  nodes: WorkflowyNode[],
  currentDepth: number,
  maxDepth?: number
): Array<WorkflowyNode & { depth: number }> {
  if (maxDepth !== undefined && currentDepth > maxDepth) return [];
  const children = nodes.filter(n => n.parent_id === parentId);
  const result: Array<WorkflowyNode & { depth: number }> = [];
  for (const child of children) {
    result.push({ ...child, depth: currentDepth });
    result.push(...getDescendants(child.id, nodes, currentDepth + 1, maxDepth));
  }
  return result;
}

function buildOutlineContent(
  root: WorkflowyNode,
  descendants: Array<WorkflowyNode & { depth: number }>
): string {
  const lines: string[] = [`# ${root.name || "Root"}`];
  if (root.note) lines.push(root.note);
  lines.push("");

  for (const d of descendants) {
    const indent = "  ".repeat(d.depth);
    lines.push(`${indent}- ${d.name || "Untitled"}`);
    if (d.note) {
      lines.push(`${indent}  ${d.note}`);
    }
  }
  return lines.join("\n");
}

async function analyzeWithClaude(
  content: string,
  rootName: string,
  coreLabel?: string,
  nodeIdMap?: Map<string, string>
): Promise<ClaudeAnalysis> {
  const apiKey = process.env.ANTHROPIC_API_KEY;
  if (!apiKey) {
    throw new Error("ANTHROPIC_API_KEY not set. Run with --setup or set the environment variable.");
  }

  const client = new Anthropic({ apiKey });
  console.log("Asking Claude to analyze content and discover concepts...");

  const response = await client.messages.create({
    model: "claude-sonnet-4-20250514",
    max_tokens: 4096,
    messages: [{
      role: "user",
      content: `Analyze the following Workflowy content and produce a concept map analysis as JSON.

The root topic is: "${rootName}"
${coreLabel ? `The core concept should be labeled: "${coreLabel}"` : "Choose the best label for the central concept."}

Identify:
1. **Major concepts** (5-8): Main themes, categories, or pillars
2. **Detail concepts** (2-5 per major): Specific ideas or sub-themes under each major
3. **Relationships** (10-25): Meaningful connections between concepts

For each relationship, use a specific verb phrase:
- Causal: produces, enables, requires, leads to, depends on
- Evaluative: critiques, extends, develops, refines, challenges
- Comparative: contrasts with, differs from, parallels, complements
- Hierarchical: includes, is a type of, exemplifies, generalizes
- Influence: influences, shapes, informs, draws from

Prioritize non-obvious connections. Every concept needs at least one relationship.

Return ONLY valid JSON matching this schema:
{
  "title": "Descriptive map title",
  "core_label": "Central concept label",
  "concepts": [
    {"id": "kebab-case-id", "label": "Display Label", "level": "major", "importance": 8},
    {"id": "detail-id", "label": "Detail Label", "level": "detail", "importance": 5, "parent_major_id": "kebab-case-id"}
  ],
  "relationships": [
    {"from": "concept-id", "to": "other-id", "type": "enables", "strength": 7}
  ]
}

Content to analyze:
${content.substring(0, 20000)}`
    }]
  });

  const text = response.content[0].type === "text" ? response.content[0].text : "";
  const jsonMatch = text.match(/\{[\s\S]*\}/);
  if (!jsonMatch) {
    throw new Error("Claude did not return valid JSON");
  }
  return JSON.parse(jsonMatch[0]) as ClaudeAnalysis;
}

function buildManualAnalysis(
  rootName: string,
  coreLabel: string,
  conceptNames: string[],
  descendants: Array<WorkflowyNode & { depth: number }>
): ClaudeAnalysis {
  // Split concepts: first 8 are major, rest are detail
  const majors = conceptNames.slice(0, 8);
  const details = conceptNames.slice(8, 24);

  const concepts: ClaudeAnalysis["concepts"] = [];
  for (const name of majors) {
    concepts.push({
      id: name.toLowerCase().replace(/\s+/g, "-").replace(/[^a-z0-9-]/g, ""),
      label: name,
      level: "major",
      importance: 6,
    });
  }
  for (const name of details) {
    const id = name.toLowerCase().replace(/\s+/g, "-").replace(/[^a-z0-9-]/g, "");
    concepts.push({
      id,
      label: name,
      level: "detail",
      importance: 3,
      parent_major_id: concepts[0]?.id,
    });
  }

  // Build co-occurrence relationships
  const relationships: ClaudeAnalysis["relationships"] = [];
  const allIds = concepts.map(c => c.id);
  const conceptLower = conceptNames.map(n => n.toLowerCase());

  for (const node of descendants) {
    const text = `${node.name || ""} ${node.note || ""}`.toLowerCase();
    const present: number[] = [];
    for (let i = 0; i < conceptLower.length; i++) {
      if (text.includes(conceptLower[i])) present.push(i);
    }
    for (let i = 0; i < present.length; i++) {
      for (let j = i + 1; j < present.length; j++) {
        const fromId = allIds[present[i]];
        const toId = allIds[present[j]];
        if (!fromId || !toId) continue;
        const existing = relationships.find(
          r => (r.from === fromId && r.to === toId) || (r.from === toId && r.to === fromId)
        );
        if (existing) {
          existing.strength = Math.min(10, existing.strength + 1);
        } else {
          relationships.push({ from: fromId, to: toId, type: "relates to", strength: 3 });
        }
      }
    }
  }

  return {
    title: `${rootName}: Concept Map`,
    core_label: coreLabel,
    concepts,
    relationships: relationships.slice(0, 30),
  };
}

async function main() {
  if (options.setup) {
    await runSetup();
    return;
  }

  const hasCredentials = await ensureCredentials();
  if (!hasCredentials) {
    console.error("\nRun with --setup to configure credentials");
    process.exit(1);
  }

  if (!options.nodeId && !options.search) {
    console.error("Error: Must provide either --node-id or --search");
    process.exit(1);
  }

  if (!options.concepts && !options.auto) {
    console.error("Error: Must provide either --concepts or --auto");
    process.exit(1);
  }

  const maxDepth = options.depth ? parseInt(options.depth, 10) : undefined;

  console.log("Fetching nodes from Workflowy...");
  const response = await workflowyRequest("/nodes-export", "GET") as { nodes: WorkflowyNode[] };
  const allNodes = response.nodes;
  console.log(`Found ${allNodes.length} total nodes`);

  // Find target node
  let targetNode: WorkflowyNode | null = null;
  if (options.nodeId) {
    targetNode = allNodes.find(n => n.id === options.nodeId) || null;
  } else if (options.search) {
    targetNode = findNodeBySearch(options.search, allNodes);
  }

  if (!targetNode) {
    console.error(`Error: Could not find node "${options.nodeId || options.search}"`);
    process.exit(1);
  }

  console.log(`Analyzing: "${targetNode.name}" (${targetNode.id})`);
  if (maxDepth !== undefined) console.log(`Depth limit: ${maxDepth}`);

  // Get descendants with depth limit
  const descendants = getDescendants(targetNode.id, allNodes, 1, maxDepth);
  console.log(`Found ${descendants.length} descendant nodes`);

  // Build content for analysis
  const content = buildOutlineContent(targetNode, descendants);

  // Build node ID lookup for Workflowy linking
  const nodeIdMap = new Map<string, string>();
  nodeIdMap.set(targetNode.name?.toLowerCase() || "", targetNode.id);
  for (const d of descendants) {
    if (d.name) nodeIdMap.set(d.name.toLowerCase(), d.id);
  }

  // Analyze
  let analysis: ClaudeAnalysis;
  if (options.auto) {
    analysis = await analyzeWithClaude(content, targetNode.name, options.core, nodeIdMap);
    console.log(`Claude discovered ${analysis.concepts.length} concepts and ${analysis.relationships.length} relationships`);
  } else {
    const conceptNames = options.concepts.split(",").map((c: string) => c.trim());
    analysis = buildManualAnalysis(
      targetNode.name,
      options.core || targetNode.name,
      conceptNames,
      descendants
    );
  }

  // Convert to interactive map format
  const coreNode = {
    id: "core",
    label: analysis.core_label,
    workflowyNodeId: targetNode.id,
  };

  const concepts: InteractiveConcept[] = analysis.concepts.map(c => ({
    id: c.id,
    label: c.label,
    level: c.level,
    importance: c.importance,
    parentMajorId: c.parent_major_id,
    workflowyNodeId: c.workflowy_node_id || nodeIdMap.get(c.label.toLowerCase()),
  }));

  const relationships: InteractiveRelationship[] = analysis.relationships.map(r => ({
    from: r.from,
    to: r.to,
    type: r.type,
    strength: r.strength,
  }));

  // Generate HTML
  console.log("\nGenerating interactive concept map...");
  const html = generateInteractiveConceptMapHTML(analysis.title, coreNode, concepts, relationships);

  // Save to ~/Downloads/
  const timestamp = Date.now();
  const slug = analysis.title.toLowerCase().replace(/[^a-z0-9]+/g, "-").slice(0, 40);
  const downloadsDir = join(process.env.HOME || "~", "Downloads");
  const filename = options.output || `concept-map-${slug}-${timestamp}.html`;
  const outputPath = join(downloadsDir, filename);
  writeFileSync(outputPath, html);

  const majors = concepts.filter(c => c.level === "major");
  const details = concepts.filter(c => c.level === "detail");

  console.log(`\nConcept map saved to: ${outputPath}`);
  console.log(`  Title: ${analysis.title}`);
  console.log(`  Major concepts (${majors.length}): ${majors.map(c => c.label).join(", ")}`);
  console.log(`  Detail concepts (${details.length}): ${details.map(c => c.label).join(", ")}`);
  console.log(`  Relationships: ${relationships.length}`);
  console.log(`\nOpen in any browser for interactive force-directed graph.`);
  console.log(`Click major concepts to expand details, drag to rearrange, scroll to zoom.`);

  // Upload to Dropbox
  if (isDropboxConfigured()) {
    console.log("\nUploading to Dropbox...");
    const dateStr = new Date().toISOString().slice(0, 10);
    const dropboxFilename = `concept-map-${slug}-${dateStr}.html`;
    const dropboxPath = `/Workflowy/ConceptMaps/${dropboxFilename}`;
    const dropboxResult = await uploadToDropboxPath(html, dropboxPath);
    if (dropboxResult.success && dropboxResult.url) {
      console.log(`  Dropbox: ${dropboxResult.url}`);

      // Add clickable link under Tasks node
      const tasksNode = findTasksNode(allNodes);
      if (tasksNode) {
        const linkName = `<a href="${dropboxResult.url}">Concept Map: ${analysis.title} ${dateStr}</a>`;
        await createNode({ name: linkName, parent_id: tasksNode.id });
        console.log(`  Added link under Tasks node`);
      }
    } else {
      console.error(`  Dropbox upload failed: ${dropboxResult.error}`);
    }
  } else {
    console.log("\nDropbox not configured — skipping upload");
  }

  // Insert outline into Workflowy if --insert flag is set
  if (options.insert) {
    console.log("\nInserting concept map outline into Workflowy...");
    try {
      const result = await insertConceptMapOutline(
        analysis,
        targetNode,
        allNodes,
        maxDepth,
        nodeIdMap,
        !!options.force
      );
      console.log(`  Created ${result.nodesCreated} nodes`);
      console.log(`  Outline node: https://workflowy.com/#/${result.outlineNodeId}`);
      console.log(`  Backlink added to source node`);
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : String(err);
      console.error(`  Insert failed: ${message}`);
      process.exit(1);
    }
  }
}

main().catch(err => {
  console.error("Error:", err.message);
  process.exit(1);
});
