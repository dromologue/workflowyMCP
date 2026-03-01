/**
 * Concept map outline insertion into Workflowy.
 * Creates a structured outline of the concept map analysis as Workflowy nodes
 * with internal links as child nodes and backlinks.
 *
 * Creates outline directly under the target node's parent. On error, cleans up
 * the partially-created outline root node.
 */

import { createNode, deleteNode } from "../shared/api/workflowy.js";
import { generateWorkflowyLink } from "../shared/utils/text-processing.js";
import type { ClaudeAnalysis, WorkflowyNode } from "../shared/types/index.js";

/** Throttle between API calls to avoid Workflowy rate limits */
let throttleMs = 1000;
export function _setThrottleMs(ms: number): void { throttleMs = ms; }
function throttle(): Promise<void> {
  if (throttleMs <= 0) return Promise.resolve();
  return new Promise(resolve => setTimeout(resolve, throttleMs));
}

/**
 * Build the name for the outline root node.
 */
export function buildOutlineNodeName(
  analyzedNodeName: string,
  depth: number | undefined
): string {
  const cleanName = analyzedNodeName.replace(/<[^>]*>/g, "").trim();
  const depthLabel = depth !== undefined ? `Level ${depth}` : "all levels";
  return `Concept Map - ${cleanName} - ${depthLabel}`;
}

/**
 * Create a child link node under a parent if a Workflowy node ID is available.
 */
async function createLinkChild(
  parentId: string,
  wfNodeId: string | undefined,
  label: string
): Promise<boolean> {
  if (!wfNodeId) return false;
  await throttle();
  await createNode({
    name: generateWorkflowyLink(wfNodeId, label),
    parent_id: parentId,
  });
  return true;
}

/**
 * Insert a concept map outline into Workflowy as a child of the analyzed node's parent.
 *
 * Structure:
 *   Concept Map - [Name] - Level [N]
 *     [Core Label]
 *       → link to source node
 *     Major Concepts
 *       [Major A]
 *         → link to WF node
 *         [Detail A1]
 *           → link to WF node
 *     Relationships
 *       [From] --type--> [To]
 *         → link to from outline node
 *         → link to to outline node
 */
export async function insertConceptMapOutline(
  analysis: ClaudeAnalysis,
  targetNode: WorkflowyNode,
  allNodes: WorkflowyNode[],
  maxDepth: number | undefined,
  nodeIdMap: Map<string, string>,
  force: boolean
): Promise<{ outlineNodeId: string; nodesCreated: number }> {
  const parentId = targetNode.id;
  const outlineName = buildOutlineNodeName(targetNode.name, maxDepth);

  // Check for existing outline child
  const children = allNodes.filter(n => n.parent_id === parentId);
  const existing = children.find(n => {
    const clean = (n.name || "").replace(/<[^>]*>/g, "").trim();
    return clean === outlineName;
  });

  if (existing && !force) {
    throw new Error(
      `Outline node already exists: "${outlineName}" (${existing.id}). ` +
      `Use --force to overwrite.`
    );
  }

  if (existing && force) {
    await deleteNode(existing.id);
  }

  let nodesCreated = 0;
  let outlineRootId: string | undefined;

  try {
    const sourceLink = generateWorkflowyLink(
      targetNode.id,
      targetNode.name.replace(/<[^>]*>/g, "").trim()
    );

    // 1. Create root outline node under target node
    // Wait extra before first call to let rate limit recover after /nodes-export
    if (throttleMs > 0) await new Promise(resolve => setTimeout(resolve, 5000));
    await throttle();
    const outlineRoot = await createNode({
      name: outlineName,
      note: `Source: ${sourceLink}`,
      parent_id: parentId,
    });
    outlineRootId = outlineRoot.id;
    nodesCreated++;

    // 2. Create core concept node with link as child
    await throttle();
    const coreNode = await createNode({
      name: analysis.core_label,
      parent_id: outlineRoot.id,
    });
    nodesCreated++;

    await throttle();
    await createNode({
      name: sourceLink,
      parent_id: coreNode.id,
    });
    nodesCreated++;

    // 3. Create "Major Concepts" section
    await throttle();
    const majorsSection = await createNode({
      name: "Major Concepts",
      parent_id: outlineRoot.id,
    });
    nodesCreated++;

    const majorConcepts = analysis.concepts.filter(c => c.level === "major");
    const detailConcepts = analysis.concepts.filter(c => c.level === "detail");

    // Track created node IDs for relationship links
    const conceptNodeMap = new Map<string, { createdId: string; label: string }>();

    for (const major of majorConcepts) {
      const wfNodeId = major.workflowy_node_id || nodeIdMap.get(major.label.toLowerCase());

      await throttle();
      const majorNode = await createNode({
        name: major.label,
        parent_id: majorsSection.id,
      });
      nodesCreated++;
      conceptNodeMap.set(major.id, { createdId: majorNode.id, label: major.label });

      // Add link as child node
      if (await createLinkChild(majorNode.id, wfNodeId, major.label)) {
        nodesCreated++;
      }

      // Insert child detail concepts
      const children = detailConcepts.filter(d => d.parent_major_id === major.id);
      for (const detail of children) {
        const detailWfId = detail.workflowy_node_id || nodeIdMap.get(detail.label.toLowerCase());

        await throttle();
        const detailNode = await createNode({
          name: detail.label,
          parent_id: majorNode.id,
        });
        nodesCreated++;
        conceptNodeMap.set(detail.id, { createdId: detailNode.id, label: detail.label });

        // Add link as child node
        if (await createLinkChild(detailNode.id, detailWfId, detail.label)) {
          nodesCreated++;
        }
      }
    }

    // 4. Create "Relationships" section
    await throttle();
    const relsSection = await createNode({
      name: "Relationships",
      parent_id: outlineRoot.id,
    });
    nodesCreated++;

    for (const rel of analysis.relationships) {
      const fromLabel = rel.from === "core"
        ? analysis.core_label
        : analysis.concepts.find(c => c.id === rel.from)?.label || rel.from;
      const toLabel = rel.to === "core"
        ? analysis.core_label
        : analysis.concepts.find(c => c.id === rel.to)?.label || rel.to;

      await throttle();
      const relNode = await createNode({
        name: `${fromLabel} --${rel.type}--> ${toLabel}`,
        parent_id: relsSection.id,
      });
      nodesCreated++;

      // Add links to from/to outline nodes as children
      const fromOutline = conceptNodeMap.get(rel.from);
      const toOutline = conceptNodeMap.get(rel.to);
      if (fromOutline) {
        await throttle();
        await createNode({
          name: generateWorkflowyLink(fromOutline.createdId, fromLabel),
          parent_id: relNode.id,
        });
        nodesCreated++;
      }
      if (toOutline) {
        await throttle();
        await createNode({
          name: generateWorkflowyLink(toOutline.createdId, toLabel),
          parent_id: relNode.id,
        });
        nodesCreated++;
      }
    }

    return { outlineNodeId: outlineRoot.id, nodesCreated };

  } catch (error) {
    // On error, delete the outline root (cascades to all children)
    if (outlineRootId) {
      console.error("  Error during outline insertion, cleaning up...");
      try {
        await deleteNode(outlineRootId);
      } catch {
        // If cleanup fails, there's nothing more we can do
      }
    }
    throw error;
  }
}
