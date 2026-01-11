/**
 * Workflowy API client with retry logic
 */

import { WORKFLOWY_API_KEY, WORKFLOWY_BASE_URL } from "../config/environment.js";
import { withRetry, HttpError } from "./retry.js";

/**
 * Make a request to the Workflowy API with automatic retry
 */
export async function workflowyRequest(
  endpoint: string,
  method: string = "GET",
  body?: object
): Promise<unknown> {
  return withRetry(async () => {
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
      throw new HttpError(
        `Workflowy API error: ${response.status} - ${errorText}`,
        response.status
      );
    }

    return response.json();
  }, `${method} ${endpoint}`);
}

/**
 * Create a new node
 */
export async function createNode(params: {
  name: string;
  note?: string;
  parent_id?: string;
  priority?: number;
}): Promise<{ id: string }> {
  return workflowyRequest("/nodes", "POST", params) as Promise<{ id: string }>;
}

/**
 * Update an existing node
 */
export async function updateNode(
  nodeId: string,
  params: { name?: string; note?: string }
): Promise<unknown> {
  return workflowyRequest(`/nodes/${nodeId}`, "POST", params);
}

/**
 * Delete a node
 */
export async function deleteNode(nodeId: string): Promise<unknown> {
  return workflowyRequest(`/nodes/${nodeId}`, "DELETE");
}

/**
 * Move a node to a new parent
 */
export async function moveNode(
  nodeId: string,
  newParentId: string,
  priority?: number
): Promise<unknown> {
  return workflowyRequest(`/nodes/${nodeId}`, "POST", {
    parent_id: newParentId,
    priority,
  });
}

/**
 * Complete a node (mark as done)
 */
export async function completeNode(nodeId: string): Promise<unknown> {
  return workflowyRequest(`/nodes/${nodeId}/complete`, "POST");
}

/**
 * Uncomplete a node (mark as not done)
 */
export async function uncompleteNode(nodeId: string): Promise<unknown> {
  return workflowyRequest(`/nodes/${nodeId}/uncomplete`, "POST");
}

/**
 * Get a single node by ID
 */
export async function getNode(nodeId: string): Promise<unknown> {
  return workflowyRequest(`/nodes/${nodeId}`);
}

/**
 * Get children of a node (or root nodes if no parent)
 */
export async function getChildren(parentId?: string): Promise<unknown> {
  const endpoint = parentId ? `/nodes?parent_id=${parentId}` : "/nodes";
  return workflowyRequest(endpoint);
}

/**
 * Export all nodes (rate limited: 1 req/min)
 */
export async function exportAll(): Promise<unknown> {
  return workflowyRequest("/nodes-export");
}

/**
 * Get available targets/shortcuts
 */
export async function getTargets(): Promise<unknown> {
  return workflowyRequest("/targets");
}
