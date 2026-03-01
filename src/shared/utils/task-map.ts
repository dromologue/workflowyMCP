/**
 * Task Map: builds a concept map from Workflowy's Tags node.
 *
 * Finds the root-level "Tags" node, reads children as #tag / @mention
 * definitions, scans all nodes for matches, and produces structured data
 * for rendering as an interactive concept map.
 *
 * Pure functions — no API calls. Receives pre-fetched nodes.
 */

import { parseNodeTags } from "./tag-parser.js";
import type { ParsedTags } from "./tag-parser.js";
import type { InteractiveConcept, InteractiveRelationship } from "./concept-map-html.js";
import type { ClaudeAnalysis, WorkflowyNode } from "../types/index.js";

// ── Types ──

export interface TagDefinition {
  /** Raw text from the Tags child node (e.g. "#inbox", "@alice") */
  raw: string;
  /** Normalized label without prefix, lowercase */
  normalized: string;
  /** Whether this is a #tag or @mention */
  type: "tag" | "mention";
  /** Workflowy node ID of the tag definition node */
  definitionNodeId: string;
}

export interface TaggedNode {
  node: WorkflowyNode;
  matchedTags: TagDefinition[];
}

export interface TaskMapOptions {
  maxDetailsPerTag?: number;
  detailSortBy?: "recency" | "name";
  title?: string;
  excludeCompleted?: boolean;
  /** Exclude @mention tags, only use #hashtags (default: true) */
  excludeMentions?: boolean;
}

export interface TaskMapData {
  title: string;
  tagsNode: WorkflowyNode;
  tagDefinitions: TagDefinition[];
  taggedNodes: TaggedNode[];
  concepts: InteractiveConcept[];
  relationships: InteractiveRelationship[];
  analysis: ClaudeAnalysis;
}

// ── Helpers ──

function cleanName(name: string): string {
  return (name || "")
    .replace(/<[^>]*>/g, "")           // strip HTML tags
    .replace(/&nbsp;/gi, " ")          // HTML entities
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#\d+;/g, "")           // numeric HTML entities
    .replace(/[\u200B-\u200F\uFEFF]/g, "") // zero-width chars
    .replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/g, "") // control chars
    .replace(/\s+/g, " ")             // collapse whitespace
    .trim();
}

// ── Core functions ──

/**
 * Find the root-level "Tags" node.
 * Root-level = parent_id is undefined/null or not present in the node set.
 */
export function findTagsNode(allNodes: WorkflowyNode[]): WorkflowyNode | null {
  const nodeIds = new Set(allNodes.map(n => n.id));
  return allNodes.find(n => {
    const isRoot = !n.parent_id || !nodeIds.has(n.parent_id);
    if (!isRoot) return false;
    const name = cleanName(n.name).toLowerCase();
    return name === "tags" || name === "#tags";
  }) || null;
}

/**
 * Extract tag/mention definitions from children of the Tags node.
 */
export function extractTagDefinitions(
  tagsNode: WorkflowyNode,
  allNodes: WorkflowyNode[]
): TagDefinition[] {
  const children = allNodes.filter(n => n.parent_id === tagsNode.id);
  const definitions: TagDefinition[] = [];

  for (const child of children) {
    const text = cleanName(child.name);
    const parsed = parseNodeTags(child);

    for (const tag of parsed.tags) {
      definitions.push({
        raw: `#${tag}`,
        normalized: tag,
        type: "tag",
        definitionNodeId: child.id,
      });
    }
    for (const assignee of parsed.assignees) {
      definitions.push({
        raw: `@${assignee}`,
        normalized: assignee,
        type: "mention",
        definitionNodeId: child.id,
      });
    }

    // Fallback: if no # or @ found, treat cleaned name as a tag
    if (parsed.tags.length === 0 && parsed.assignees.length === 0 && text) {
      definitions.push({
        raw: text,
        normalized: text.toLowerCase(),
        type: "tag",
        definitionNodeId: child.id,
      });
    }
  }

  return definitions;
}

/**
 * Find all nodes matching any of the tag definitions.
 * Pre-computes parsed tags per node for performance.
 * Excludes the Tags node and its direct children (tag definitions themselves).
 */
export function findTaggedNodes(
  tagDefinitions: TagDefinition[],
  allNodes: WorkflowyNode[],
  options: TaskMapOptions = {},
  excludeNodeIds?: Set<string>
): TaggedNode[] {
  // Pre-compute parsed tags for every node
  const parsedCache = new Map<string, ParsedTags>();
  for (const node of allNodes) {
    parsedCache.set(node.id, parseNodeTags(node));
  }

  const results: TaggedNode[] = [];

  for (const node of allNodes) {
    if (options.excludeCompleted && node.completedAt) continue;
    if (excludeNodeIds?.has(node.id)) continue;

    const parsed = parsedCache.get(node.id)!;
    const matched: TagDefinition[] = [];

    for (const def of tagDefinitions) {
      if (def.type === "tag" && parsed.tags.some(t => t === def.normalized || t.startsWith(def.normalized))) {
        matched.push(def);
      } else if (def.type === "mention" && parsed.assignees.some(a => a === def.normalized || a.startsWith(def.normalized))) {
        matched.push(def);
      }
    }

    if (matched.length > 0) {
      results.push({ node, matchedTags: matched });
    }
  }

  return results;
}

/**
 * Build the full task map data from tag definitions and matched nodes.
 */
export function buildTaskMapData(
  tagsNode: WorkflowyNode,
  tagDefinitions: TagDefinition[],
  taggedNodes: TaggedNode[],
  options: TaskMapOptions = {}
): TaskMapData {
  const maxDetails = options.maxDetailsPerTag ?? 8;
  const sortBy = options.detailSortBy ?? "recency";
  const title = options.title ?? "Task Map";

  // Count matches per tag for importance
  const matchCounts = new Map<string, number>();
  for (const def of tagDefinitions) {
    matchCounts.set(def.normalized, 0);
  }
  for (const tn of taggedNodes) {
    for (const mt of tn.matchedTags) {
      matchCounts.set(mt.normalized, (matchCounts.get(mt.normalized) || 0) + 1);
    }
  }

  const maxCount = Math.max(1, ...matchCounts.values());

  // Major concepts: one per tag definition
  const concepts: InteractiveConcept[] = [];
  const analysisConcepts: ClaudeAnalysis["concepts"] = [];

  for (const def of tagDefinitions) {
    const count = matchCounts.get(def.normalized) || 0;
    const importance = Math.max(1, Math.round((count / maxCount) * 10));
    const majorId = `tag-${def.normalized}`;

    concepts.push({
      id: majorId,
      label: def.raw,
      level: "major",
      importance,
      workflowyNodeId: def.definitionNodeId,
    });

    analysisConcepts.push({
      id: majorId,
      label: def.raw,
      level: "major",
      importance,
      workflowy_node_id: def.definitionNodeId,
    });

    // Detail concepts: matched nodes for this tag, capped
    const nodesForTag = taggedNodes.filter(
      tn => tn.matchedTags.some(mt => mt.normalized === def.normalized)
    );

    const sorted = [...nodesForTag].sort((a, b) => {
      if (sortBy === "recency") {
        return (b.node.modifiedAt || 0) - (a.node.modifiedAt || 0);
      }
      return cleanName(a.node.name).localeCompare(cleanName(b.node.name));
    });

    for (const tn of sorted.slice(0, maxDetails)) {
      const detailId = `${def.normalized}-${tn.node.id}`;
      concepts.push({
        id: detailId,
        label: cleanName(tn.node.name),
        level: "detail",
        importance: 3,
        parentMajorId: majorId,
        workflowyNodeId: tn.node.id,
      });

      analysisConcepts.push({
        id: detailId,
        label: cleanName(tn.node.name),
        level: "detail",
        importance: 3,
        parent_major_id: majorId,
        workflowy_node_id: tn.node.id,
      });
    }
  }

  // Relationships: co-occurrence (nodes matching 2+ tags)
  const pairCounts = new Map<string, number>();
  for (const tn of taggedNodes) {
    if (tn.matchedTags.length < 2) continue;
    const tagIds = tn.matchedTags.map(mt => `tag-${mt.normalized}`).sort();
    for (let i = 0; i < tagIds.length; i++) {
      for (let j = i + 1; j < tagIds.length; j++) {
        const key = `${tagIds[i]}|${tagIds[j]}`;
        pairCounts.set(key, (pairCounts.get(key) || 0) + 1);
      }
    }
  }

  const relationships: InteractiveRelationship[] = [];
  const analysisRelationships: ClaudeAnalysis["relationships"] = [];

  for (const [key, count] of pairCounts) {
    const [from, to] = key.split("|");
    const strength = Math.min(10, count);
    relationships.push({ from, to, type: "co-occurs with", strength });
    analysisRelationships.push({ from, to, type: "co-occurs with", strength });
  }

  const analysis: ClaudeAnalysis = {
    title,
    core_label: title,
    concepts: analysisConcepts,
    relationships: analysisRelationships,
  };

  return {
    title,
    tagsNode,
    tagDefinitions,
    taggedNodes,
    concepts,
    relationships,
    analysis,
  };
}

/**
 * Top-level convenience: generate a full task map from all nodes.
 */
export function generateTaskMap(
  allNodes: WorkflowyNode[],
  options: TaskMapOptions = {}
): TaskMapData {
  const tagsNode = findTagsNode(allNodes);
  if (!tagsNode) {
    throw new Error("No root-level 'Tags' node found in Workflowy");
  }

  let tagDefinitions = extractTagDefinitions(tagsNode, allNodes);

  // Exclude @mentions by default
  const excludeMentions = options.excludeMentions ?? true;
  if (excludeMentions) {
    tagDefinitions = tagDefinitions.filter(d => d.type === "tag");
  }

  // Exclude the Tags node and its children from search results
  const excludeIds = new Set<string>([tagsNode.id]);
  for (const n of allNodes) {
    if (n.parent_id === tagsNode.id) excludeIds.add(n.id);
  }

  const taggedNodes = findTaggedNodes(tagDefinitions, allNodes, options, excludeIds);
  return buildTaskMapData(tagsNode, tagDefinitions, taggedNodes, options);
}
