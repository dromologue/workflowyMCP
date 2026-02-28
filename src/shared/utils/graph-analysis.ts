/**
 * Graph analysis algorithms for network centrality and relationship extraction.
 * Ported from ~/code/Graph-Tools/mcp-graph-server/index.js (pure TypeScript, no external deps).
 */

// ============================================================================
// Types
// ============================================================================

export interface GraphEdge {
  from: string;
  to: string;
  weight: number;
}

export interface GraphStructure {
  adjacencyList: Record<string, Array<{ node: string; weight: number }>>;
  adjacencyMatrix: Record<string, Record<string, number>>;
  vertices: string[];
}

export interface CentralityResults {
  [measure: string]: Record<string, number>;
}

export interface RelationshipExtractionResult {
  relationships: GraphEdge[];
  vertices: string[];
  density: number;
}

// ============================================================================
// Graph Structure
// ============================================================================

export function buildGraphStructure(
  relationships: GraphEdge[],
  vertices: string[]
): GraphStructure {
  const adjacencyList: Record<string, Array<{ node: string; weight: number }>> = {};
  const adjacencyMatrix: Record<string, Record<string, number>> = {};

  for (const vertex of vertices) {
    adjacencyList[vertex] = [];
    adjacencyMatrix[vertex] = {};
    for (const otherVertex of vertices) {
      adjacencyMatrix[vertex][otherVertex] = 0;
    }
  }

  for (const rel of relationships) {
    const { from, to, weight = 1 } = rel;
    if (from && to && vertices.includes(from) && vertices.includes(to)) {
      adjacencyList[from].push({ node: to, weight });
      adjacencyMatrix[from][to] = weight;
    }
  }

  return { adjacencyList, adjacencyMatrix, vertices };
}

// ============================================================================
// Centrality Measures
// ============================================================================

export function calculateDegreeCentrality(
  graph: GraphStructure
): Record<string, number> {
  const { adjacencyList, vertices } = graph;
  const centrality: Record<string, number> = {};

  for (const vertex of vertices) {
    const outDegree = adjacencyList[vertex].length;
    const inDegree = vertices.filter((v) =>
      adjacencyList[v].some((neighbor) => neighbor.node === vertex)
    ).length;
    centrality[vertex] =
      vertices.length > 1 ? (outDegree + inDegree) / (vertices.length - 1) : 0;
  }

  return centrality;
}

export function calculateBetweennessCentrality(
  graph: GraphStructure
): Record<string, number> {
  const { vertices } = graph;
  const centrality: Record<string, number> = {};
  vertices.forEach((v) => (centrality[v] = 0));

  for (let s = 0; s < vertices.length; s++) {
    for (let t = s + 1; t < vertices.length; t++) {
      const source = vertices[s];
      const target = vertices[t];

      const paths = findAllShortestPaths(graph, source, target);
      if (paths.length === 0) continue;

      for (const vertex of vertices) {
        if (vertex === source || vertex === target) continue;
        const pathsThrough = paths.filter((path) => path.includes(vertex)).length;
        centrality[vertex] += pathsThrough / paths.length;
      }
    }
  }

  // Normalize
  const n = vertices.length;
  const normalizationFactor = ((n - 1) * (n - 2)) / 2;
  if (normalizationFactor > 0) {
    vertices.forEach((v) => (centrality[v] /= normalizationFactor));
  }

  return centrality;
}

export function calculateClosenessCentrality(
  graph: GraphStructure
): Record<string, number> {
  const { vertices } = graph;
  const centrality: Record<string, number> = {};

  for (const vertex of vertices) {
    const distances = dijkstra(graph, vertex);
    const validDistances = Object.values(distances).filter(
      (d) => d !== Infinity && d > 0
    );

    if (validDistances.length === 0) {
      centrality[vertex] = 0;
    } else {
      const avgDistance =
        validDistances.reduce((sum, d) => sum + d, 0) / validDistances.length;
      centrality[vertex] = avgDistance > 0 ? 1 / avgDistance : 0;
    }
  }

  return centrality;
}

export function calculateEigenvectorCentrality(
  graph: GraphStructure,
  maxIterations = 100,
  tolerance = 1e-6
): Record<string, number> {
  const { adjacencyMatrix, vertices } = graph;
  const n = vertices.length;

  let centrality: Record<string, number> = {};
  vertices.forEach((v) => (centrality[v] = 1 / Math.sqrt(n)));

  for (let iter = 0; iter < maxIterations; iter++) {
    const newCentrality: Record<string, number> = {};

    // Matrix-vector multiplication: A * x
    for (const vertex of vertices) {
      newCentrality[vertex] = 0;
      for (const neighbor of vertices) {
        newCentrality[vertex] +=
          adjacencyMatrix[neighbor][vertex] * centrality[neighbor];
      }
    }

    // Normalize
    const norm = Math.sqrt(
      Object.values(newCentrality).reduce((sum, val) => sum + val * val, 0)
    );
    if (norm > 0) {
      vertices.forEach((v) => (newCentrality[v] /= norm));
    }

    // Check convergence
    const diff = vertices.reduce(
      (sum, v) => sum + Math.abs(newCentrality[v] - centrality[v]),
      0
    );

    centrality = newCentrality;
    if (diff < tolerance) break;
  }

  return centrality;
}

// ============================================================================
// Pathfinding
// ============================================================================

export function findAllShortestPaths(
  graph: GraphStructure,
  source: string,
  target: string
): string[][] {
  const { adjacencyList } = graph;
  const distances: Record<string, number> = {};
  const predecessors: Record<string, string[]> = {};
  const visited = new Set<string>();
  const queue: string[] = [source];

  distances[source] = 0;
  predecessors[source] = [];

  while (queue.length > 0) {
    const current = queue.shift()!;
    if (visited.has(current)) continue;
    visited.add(current);

    for (const neighbor of adjacencyList[current] || []) {
      const next = neighbor.node;
      const newDist = distances[current] + (neighbor.weight || 1);

      if (!(next in distances) || newDist < distances[next]) {
        distances[next] = newDist;
        predecessors[next] = [current];
        queue.push(next);
      } else if (newDist === distances[next]) {
        predecessors[next].push(current);
      }
    }
  }

  if (!(target in distances)) return [];

  const paths: string[][] = [];
  const buildPaths = (node: string, currentPath: string[]): void => {
    if (node === source) {
      paths.push([source, ...currentPath]);
      return;
    }
    for (const pred of predecessors[node] || []) {
      buildPaths(pred, [node, ...currentPath]);
    }
  };

  buildPaths(target, []);
  return paths;
}

export function dijkstra(
  graph: GraphStructure,
  source: string
): Record<string, number> {
  const { adjacencyList } = graph;
  const distances: Record<string, number> = {};
  const visited = new Set<string>();
  const priorityQueue: Array<{ node: string; distance: number }> = [
    { node: source, distance: 0 },
  ];

  distances[source] = 0;

  while (priorityQueue.length > 0) {
    priorityQueue.sort((a, b) => a.distance - b.distance);
    const { node: current, distance: currentDist } = priorityQueue.shift()!;

    if (visited.has(current)) continue;
    visited.add(current);

    for (const neighbor of adjacencyList[current] || []) {
      const next = neighbor.node;
      const weight = neighbor.weight || 1;
      const newDist = currentDist + weight;

      if (!(next in distances) || newDist < distances[next]) {
        distances[next] = newDist;
        priorityQueue.push({ node: next, distance: newDist });
      }
    }
  }

  return distances;
}

// ============================================================================
// Relationship Extraction
// ============================================================================

export function extractRelationshipsFromData(
  data: Array<Record<string, unknown>>,
  relationshipFields: string[],
  nodeLabelField = "id"
): RelationshipExtractionResult {
  const relationships: GraphEdge[] = [];
  const vertices = new Set<string>();

  for (const item of data) {
    const nodeId = String(item[nodeLabelField] || "");
    if (nodeId) vertices.add(nodeId);

    for (const field of relationshipFields) {
      if (item[field]) {
        if (Array.isArray(item[field])) {
          for (const target of item[field] as unknown[]) {
            const targetStr = String(target);
            if (targetStr && targetStr !== nodeId) {
              relationships.push({ from: nodeId, to: targetStr, weight: 1 });
              vertices.add(targetStr);
            }
          }
        } else if (item[field] !== null && String(item[field]) !== nodeId) {
          const targetStr = String(item[field]);
          if (targetStr) {
            relationships.push({ from: nodeId, to: targetStr, weight: 1 });
            vertices.add(targetStr);
          }
        }
      }
    }
  }

  const vertexArray = Array.from(vertices).sort();
  const density =
    vertexArray.length > 1
      ? relationships.length / (vertexArray.length * (vertexArray.length - 1))
      : 0;

  return { relationships, vertices: vertexArray, density };
}

// ============================================================================
// Result Formatting
// ============================================================================

export function formatCentralityResults(
  results: CentralityResults,
  topN = 10
): string {
  let text = "Centrality Analysis\n\n";

  for (const [measure, scores] of Object.entries(results)) {
    const sorted = Object.entries(scores)
      .map(([node, score]) => ({ node, score: Number(score) }))
      .sort((a, b) => b.score - a.score)
      .slice(0, topN);

    text += `${measure.charAt(0).toUpperCase() + measure.slice(1)} Centrality\n`;
    sorted.forEach((item, idx) => {
      text += `${idx + 1}. ${item.node}: ${item.score.toFixed(4)}\n`;
    });
    text += "\n";
  }

  return text;
}
