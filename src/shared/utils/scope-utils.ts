/**
 * Scope filtering utilities for Workflowy nodes.
 * Extracted from server.ts for reuse across tools.
 */

import type { WorkflowyNode } from "../types/index.js";

export type ScopeType = "this_node" | "children" | "siblings" | "ancestors" | "all";

/**
 * Build a parent_id → children lookup index.
 */
export function buildChildrenIndex(nodes: WorkflowyNode[]): Map<string, WorkflowyNode[]> {
  const childrenMap = new Map<string, WorkflowyNode[]>();
  for (const node of nodes) {
    const parentId = node.parent_id || "root";
    if (!childrenMap.has(parentId)) {
      childrenMap.set(parentId, []);
    }
    childrenMap.get(parentId)!.push(node);
  }
  return childrenMap;
}

/**
 * Get a node and all its descendants.
 */
export function getSubtreeNodes(rootId: string, allNodes: WorkflowyNode[]): WorkflowyNode[] {
  const childrenMap = buildChildrenIndex(allNodes);
  const nodeMap = new Map(allNodes.map((n) => [n.id, n]));

  const result: WorkflowyNode[] = [];
  const root = nodeMap.get(rootId);
  if (root) result.push(root);

  const collectChildren = (parentId: string, depth = 0) => {
    if (depth > 100) return;
    const children = childrenMap.get(parentId) || [];
    for (const child of children) {
      result.push(child);
      collectChildren(child.id, depth + 1);
    }
  };
  collectChildren(rootId);

  return result;
}

/**
 * Filter nodes by scope relative to a source node.
 * Extracted from server.ts for reuse.
 */
export function filterNodesByScope(
  sourceNode: WorkflowyNode,
  allNodes: WorkflowyNode[],
  scope: ScopeType
): WorkflowyNode[] {
  if (!Array.isArray(allNodes)) {
    return [];
  }

  const nodeMap = new Map<string, WorkflowyNode>();
  const childrenMap = buildChildrenIndex(allNodes);

  for (const node of allNodes) {
    nodeMap.set(node.id, node);
  }

  switch (scope) {
    case "this_node":
      return [];

    case "children": {
      const result: WorkflowyNode[] = [];
      const collectChildren = (parentId: string, depth = 0) => {
        if (depth > 100) return;
        const children = childrenMap.get(parentId) || [];
        for (const child of children) {
          result.push(child);
          collectChildren(child.id, depth + 1);
        }
      };
      collectChildren(sourceNode.id);
      return result;
    }

    case "siblings": {
      if (!sourceNode.parent_id) {
        return (childrenMap.get("root") || []).filter((n) => n.id !== sourceNode.id);
      }
      return (childrenMap.get(sourceNode.parent_id) || []).filter(
        (n) => n.id !== sourceNode.id
      );
    }

    case "ancestors": {
      const ancestors: WorkflowyNode[] = [];
      let currentId = sourceNode.parent_id;
      let depth = 0;
      while (currentId && depth < 100) {
        const parent = nodeMap.get(currentId);
        if (parent) {
          ancestors.push(parent);
          currentId = parent.parent_id;
        } else {
          break;
        }
        depth++;
      }
      return ancestors;
    }

    case "all":
    default:
      return allNodes.filter((n) => n.id !== sourceNode.id);
  }
}
