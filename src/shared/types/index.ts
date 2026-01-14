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

// ============================================================================
// LLM-Powered Concept Map Types
// ============================================================================

/** A node formatted for LLM analysis */
export interface AnalysisContentNode {
  depth: number;
  id: string;
  name: string;
  note?: string;
  path: string;
  /** IDs of nodes this node links to (via Workflowy internal links) */
  links_to?: string[];
}

/** Result from get_node_content_for_analysis */
export interface AnalysisContentResult {
  root: {
    id: string;
    name: string;
    note?: string;
  };
  total_nodes: number;
  total_chars: number;
  truncated: boolean;
  /** Nodes that were linked but outside the initial scope */
  linked_nodes_included: number;
  content: AnalysisContentNode[];
}

/** Concept input for render_concept_map */
export interface ConceptInput {
  id: string;
  label: string;
  level: "major" | "detail";
  importance?: number;
  description?: string;
}

/** Relationship input for render_concept_map */
export interface RelationshipInput {
  from: string;
  to: string;
  type: string;
  strength?: number;
  evidence?: string;
}

/** Core concept for render_concept_map */
export interface CoreConceptInput {
  label: string;
  description?: string;
}

/** Output options for render_concept_map */
export interface RenderOutputOptions {
  format?: "png" | "jpeg";
  insert_into_workflowy?: string;
  output_path?: string;
}

/** Result from render_concept_map */
export interface RenderConceptMapResult {
  success: boolean;
  image_url?: string;
  file_path?: string;
  inserted_into?: {
    id: string;
    name: string;
  };
  error?: string;
  stats?: {
    concepts_rendered: number;
    major_concepts: number;
    detail_concepts: number;
    relationships_rendered: number;
  };
}
