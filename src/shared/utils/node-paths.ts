/**
 * Node path building utilities for hierarchical display
 */

import type { WorkflowyNode, NodeWithPath } from "../types/index.js";

/**
 * Build full paths for nodes showing their hierarchical location
 * Paths are truncated to 40 chars per segment for readability
 */
export function buildNodePaths(nodes: WorkflowyNode[]): NodeWithPath[] {
  const nodeMap = new Map<string, WorkflowyNode>();
  nodes.forEach((node) => nodeMap.set(node.id, node));

  function getPath(
    node: WorkflowyNode
  ): { path: string; depth: number } {
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

/**
 * Get the path for a single node
 */
export function getNodePath(
  node: WorkflowyNode,
  allNodes: WorkflowyNode[]
): string {
  const nodeMap = new Map<string, WorkflowyNode>();
  allNodes.forEach((n) => nodeMap.set(n.id, n));

  const parts: string[] = [];
  let current: WorkflowyNode | undefined = node;

  while (current) {
    const displayName = current.name?.substring(0, 40) || "(untitled)";
    parts.unshift(displayName);
    if (current.parent_id) {
      current = nodeMap.get(current.parent_id);
    } else {
      break;
    }
  }

  return parts.join(" > ");
}
