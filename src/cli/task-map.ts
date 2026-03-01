#!/usr/bin/env node
/**
 * CLI tool for generating task maps from Workflowy Tags.
 * Finds the root-level Tags node, matches tagged nodes, and generates
 * an interactive concept map visualization.
 *
 * Usage:
 *   npm run task-map
 *   npm run task-map -- --exclude-completed
 *   npm run task-map -- --insert --force
 *   npm run task-map -- --max-details 5 --sort name
 */

import "dotenv/config";
import { Command } from "commander";
import { writeFileSync } from "fs";
import { join } from "path";
import { workflowyRequest, createNode } from "../shared/api/workflowy.js";
import { generateInteractiveConceptMapHTML } from "../shared/utils/concept-map-html.js";
import { generateTaskMap } from "../shared/utils/task-map.js";
import { uploadToDropboxPath, isDropboxConfigured } from "../shared/api/dropbox.js";
import { insertConceptMapOutline } from "./concept-map-outline.js";
import { ensureCredentials } from "./setup.js";
import type { WorkflowyNode } from "../shared/types/index.js";

const program = new Command();

program
  .name("task-map")
  .description("Generate interactive task maps from Workflowy Tags")
  .version("1.0.0")
  .option("-m, --max-details <number>", "Max detail nodes per tag (default: 8)")
  .option("-s, --sort <order>", "Sort details by: recency, name (default: recency)")
  .option("-t, --title <title>", "Custom map title")
  .option("-x, --exclude-completed", "Exclude completed nodes")
  .option("-o, --output <filename>", "Output filename")
  .option("-i, --insert", "Insert outline into Workflowy under Tags node")
  .option("--force", "Overwrite existing outline (use with --insert)")
  .parse(process.argv);

const options = program.opts();

async function main() {
  const hasCredentials = await ensureCredentials();
  if (!hasCredentials) {
    console.error("\nRun concept-map --setup to configure credentials");
    process.exit(1);
  }

  console.log("Fetching nodes from Workflowy...");
  const response = await workflowyRequest("/nodes-export", "GET") as { nodes: WorkflowyNode[] };
  const allNodes = response.nodes;
  console.log(`Found ${allNodes.length} total nodes`);

  // Generate task map data
  const maxDetails = options.maxDetails ? parseInt(options.maxDetails, 10) : undefined;
  const sortBy = options.sort === "name" ? "name" as const : "recency" as const;

  const taskMapData = generateTaskMap(allNodes, {
    maxDetailsPerTag: maxDetails,
    detailSortBy: sortBy,
    title: options.title,
    excludeCompleted: !!options.excludeCompleted,
  });

  const majors = taskMapData.concepts.filter(c => c.level === "major");
  const details = taskMapData.concepts.filter(c => c.level === "detail");

  console.log(`\nTags node: "${taskMapData.tagsNode.name}" (${taskMapData.tagsNode.id})`);
  console.log(`Tag definitions: ${taskMapData.tagDefinitions.length}`);
  for (const def of taskMapData.tagDefinitions) {
    const count = taskMapData.taggedNodes.filter(
      tn => tn.matchedTags.some(mt => mt.normalized === def.normalized)
    ).length;
    console.log(`  ${def.raw} (${def.type}) — ${count} matches`);
  }
  console.log(`Total tagged nodes: ${taskMapData.taggedNodes.length}`);

  // Generate HTML
  console.log("\nGenerating interactive task map...");
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

  // Save to ~/Downloads/
  const timestamp = Date.now();
  const dateStr = new Date().toISOString().slice(0, 10);
  const slug = taskMapData.title.toLowerCase().replace(/[^a-z0-9]+/g, "-").slice(0, 40);
  const downloadsDir = join(process.env.HOME || "~", "Downloads");
  const filename = options.output || `task-map-${slug}-${timestamp}.html`;
  const outputPath = join(downloadsDir, filename);
  writeFileSync(outputPath, html);

  console.log(`\nTask map saved to: ${outputPath}`);
  console.log(`  Tags: ${majors.map(c => c.label).join(", ")}`);
  console.log(`  Detail nodes: ${details.length}`);
  console.log(`  Relationships: ${taskMapData.relationships.length}`);

  // Upload to Dropbox
  let dropboxUrl: string | undefined;
  if (isDropboxConfigured()) {
    console.log("\nUploading to Dropbox...");
    const dropboxFilename = `task-map-${dateStr}.html`;
    const dropboxPath = `/Workflowy/TaskMaps/${dropboxFilename}`;
    const result = await uploadToDropboxPath(html, dropboxPath);
    if (result.success && result.url) {
      dropboxUrl = result.url;
      console.log(`  Dropbox: ${dropboxUrl}`);
    } else {
      console.error(`  Dropbox upload failed: ${result.error}`);
    }
  } else {
    console.log("\nDropbox not configured — skipping upload");
  }

  // Add link node under Tasks node in Workflowy
  if (dropboxUrl) {
    const linkParent = taskMapData.tasksNode || taskMapData.tagsNode;
    const linkParentName = taskMapData.tasksNode ? "Tasks" : "Tags";
    console.log(`\nAdding link to Workflowy (under ${linkParentName} node)...`);
    const linkName = `Task Map ${dateStr}`;
    const linkNote = dropboxUrl;
    await createNode({
      name: linkName,
      note: linkNote,
      parent_id: linkParent.id,
    });
    console.log(`  Added "${linkName}" under ${linkParentName} node`);
  }

  console.log(`\nOpen in any browser for interactive force-directed graph.`);
  console.log(`Click tags to expand matching nodes, drag to rearrange, scroll to zoom.`);

  // Insert outline into Workflowy if --insert flag is set
  if (options.insert) {
    console.log("\nInserting task map outline into Workflowy...");
    try {
      const nodeIdMap = new Map<string, string>();
      for (const n of allNodes) {
        if (n.name) nodeIdMap.set(n.name.toLowerCase(), n.id);
      }

      const result = await insertConceptMapOutline(
        taskMapData.analysis,
        taskMapData.tagsNode,
        allNodes,
        undefined,
        nodeIdMap,
        !!options.force
      );
      console.log(`  Created ${result.nodesCreated} nodes`);
      console.log(`  Outline node: https://workflowy.com/#/${result.outlineNodeId}`);
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
