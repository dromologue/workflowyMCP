/**
 * Core type definitions for the Workflowy MCP Server
 */

/** Workflowy node as returned by the API */
export interface WorkflowyNode {
  id: string;
  name: string;
  note?: string;
  priority?: number;
  layoutMode?: string;
  createdAt?: number;
  modifiedAt?: number;
  completedAt?: number;
  parent_id?: string;
}

/** Node with computed path for display */
export interface NodeWithPath extends WorkflowyNode {
  path: string;
  depth: number;
}

/** Parsed line from indented content */
export interface ParsedLine {
  text: string;
  indent: number;
}

/** Node with relevance scoring for knowledge linking */
export interface RelatedNode {
  id: string;
  name: string;
  note?: string;
  path: string;
  relevanceScore: number;
  matchedKeywords: string[];
  link: string;
}

/** Created node result */
export interface CreatedNode {
  id: string;
  name: string;
  parent_id: string;
}

/** Concept map node for visualization */
export interface ConceptMapNode {
  id: string;
  label: string;
  isCenter: boolean;
}

/** Concept map edge for visualization */
export interface ConceptMapEdge {
  from: string;
  to: string;
  keywords: string[];
  weight: number;
}

/** Scope options for concept map generation */
export type ConceptMapScope =
  | "this_node"
  | "children"
  | "siblings"
  | "ancestors"
  | "all";

/** Dropbox token response */
export interface DropboxTokenResponse {
  access_token: string;
  token_type: string;
  expires_in: number;
}

/** Dropbox upload result */
export interface DropboxUploadResult {
  success: boolean;
  url?: string;
  error?: string;
}
