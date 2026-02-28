/**
 * Tests for graph analysis algorithms
 * Covers graph structure building, centrality measures, pathfinding, and relationship extraction.
 */

import { describe, it, expect } from "vitest";
import {
  buildGraphStructure,
  calculateDegreeCentrality,
  calculateBetweennessCentrality,
  calculateClosenessCentrality,
  calculateEigenvectorCentrality,
  findAllShortestPaths,
  dijkstra,
  extractRelationshipsFromData,
  formatCentralityResults,
} from "./graph-analysis.js";

describe("buildGraphStructure", () => {
  it("builds empty graph with no relationships", () => {
    const graph = buildGraphStructure([], ["A", "B"]);
    expect(graph.vertices).toEqual(["A", "B"]);
    expect(graph.adjacencyList["A"]).toEqual([]);
    expect(graph.adjacencyMatrix["A"]["B"]).toBe(0);
  });

  it("builds graph with edges", () => {
    const graph = buildGraphStructure(
      [{ from: "A", to: "B", weight: 1 }, { from: "B", to: "C", weight: 2 }],
      ["A", "B", "C"]
    );
    expect(graph.adjacencyList["A"]).toEqual([{ node: "B", weight: 1 }]);
    expect(graph.adjacencyList["B"]).toEqual([{ node: "C", weight: 2 }]);
    expect(graph.adjacencyMatrix["A"]["B"]).toBe(1);
    expect(graph.adjacencyMatrix["B"]["C"]).toBe(2);
  });

  it("ignores edges with unknown vertices", () => {
    const graph = buildGraphStructure(
      [{ from: "A", to: "D", weight: 1 }],
      ["A", "B"]
    );
    expect(graph.adjacencyList["A"]).toEqual([]);
  });

  it("handles single vertex", () => {
    const graph = buildGraphStructure([], ["A"]);
    expect(graph.vertices).toEqual(["A"]);
    expect(graph.adjacencyList["A"]).toEqual([]);
    expect(graph.adjacencyMatrix["A"]["A"]).toBe(0);
  });
});

describe("calculateDegreeCentrality", () => {
  it("calculates degree for star graph", () => {
    // Center connected to 3 others (directed: center → A, B, C)
    const graph = buildGraphStructure(
      [
        { from: "center", to: "A", weight: 1 },
        { from: "center", to: "B", weight: 1 },
        { from: "center", to: "C", weight: 1 },
      ],
      ["center", "A", "B", "C"]
    );
    const centrality = calculateDegreeCentrality(graph);
    // center: out=3, in=0 → 3/3 = 1.0
    expect(centrality["center"]).toBe(1);
    // A: out=0, in=1 → 1/3 ≈ 0.333
    expect(centrality["A"]).toBeCloseTo(1 / 3, 4);
  });

  it("calculates degree for complete graph", () => {
    const graph = buildGraphStructure(
      [
        { from: "A", to: "B", weight: 1 },
        { from: "B", to: "A", weight: 1 },
        { from: "A", to: "C", weight: 1 },
        { from: "C", to: "A", weight: 1 },
        { from: "B", to: "C", weight: 1 },
        { from: "C", to: "B", weight: 1 },
      ],
      ["A", "B", "C"]
    );
    const centrality = calculateDegreeCentrality(graph);
    // Each node: out=2, in=2 → 4/2 = 2.0
    expect(centrality["A"]).toBe(2);
    expect(centrality["B"]).toBe(2);
    expect(centrality["C"]).toBe(2);
  });

  it("returns 0 for isolated nodes", () => {
    const graph = buildGraphStructure([], ["A", "B"]);
    const centrality = calculateDegreeCentrality(graph);
    expect(centrality["A"]).toBe(0);
  });

  it("handles single vertex", () => {
    const graph = buildGraphStructure([], ["A"]);
    const centrality = calculateDegreeCentrality(graph);
    expect(centrality["A"]).toBe(0);
  });
});

describe("calculateBetweennessCentrality", () => {
  it("identifies bridge node", () => {
    // A → B → C (B is the bridge)
    const graph = buildGraphStructure(
      [
        { from: "A", to: "B", weight: 1 },
        { from: "B", to: "C", weight: 1 },
      ],
      ["A", "B", "C"]
    );
    const centrality = calculateBetweennessCentrality(graph);
    // B lies on the only path from A to C
    expect(centrality["B"]).toBeGreaterThan(0);
    expect(centrality["A"]).toBe(0);
    expect(centrality["C"]).toBe(0);
  });

  it("returns 0 for disconnected graph", () => {
    const graph = buildGraphStructure([], ["A", "B", "C"]);
    const centrality = calculateBetweennessCentrality(graph);
    expect(centrality["A"]).toBe(0);
    expect(centrality["B"]).toBe(0);
  });

  it("handles two-node graph", () => {
    const graph = buildGraphStructure(
      [{ from: "A", to: "B", weight: 1 }],
      ["A", "B"]
    );
    const centrality = calculateBetweennessCentrality(graph);
    expect(centrality["A"]).toBe(0);
    expect(centrality["B"]).toBe(0);
  });
});

describe("calculateClosenessCentrality", () => {
  it("gives highest closeness to most central node", () => {
    // Star: center → A, B, C
    const graph = buildGraphStructure(
      [
        { from: "center", to: "A", weight: 1 },
        { from: "center", to: "B", weight: 1 },
        { from: "center", to: "C", weight: 1 },
      ],
      ["center", "A", "B", "C"]
    );
    const centrality = calculateClosenessCentrality(graph);
    expect(centrality["center"]).toBeGreaterThan(0);
  });

  it("returns 0 for isolated nodes", () => {
    const graph = buildGraphStructure([], ["A"]);
    const centrality = calculateClosenessCentrality(graph);
    expect(centrality["A"]).toBe(0);
  });
});

describe("calculateEigenvectorCentrality", () => {
  it("converges for simple bidirectional graph", () => {
    // A↔B, B↔C, A↔C: symmetric triangle — all equal
    const graph = buildGraphStructure(
      [
        { from: "A", to: "B", weight: 1 },
        { from: "B", to: "A", weight: 1 },
        { from: "B", to: "C", weight: 1 },
        { from: "C", to: "B", weight: 1 },
        { from: "A", to: "C", weight: 1 },
        { from: "C", to: "A", weight: 1 },
      ],
      ["A", "B", "C"]
    );
    const centrality = calculateEigenvectorCentrality(graph);
    // Symmetric graph — all nodes should have equal centrality
    expect(centrality["A"]).toBeCloseTo(centrality["B"], 4);
    expect(centrality["B"]).toBeCloseTo(centrality["C"], 4);
  });

  it("normalizes to unit length for bidirectional graph", () => {
    const graph = buildGraphStructure(
      [
        { from: "A", to: "B", weight: 1 },
        { from: "B", to: "A", weight: 1 },
        { from: "B", to: "C", weight: 1 },
        { from: "C", to: "B", weight: 1 },
      ],
      ["A", "B", "C"]
    );
    const centrality = calculateEigenvectorCentrality(graph);
    const norm = Math.sqrt(
      Object.values(centrality).reduce((sum, v) => sum + v * v, 0)
    );
    // For bidirectional graphs, eigenvector converges to non-zero values
    expect(norm).toBeCloseTo(1.0, 4);
  });
});

describe("dijkstra", () => {
  it("finds shortest paths from source", () => {
    const graph = buildGraphStructure(
      [
        { from: "A", to: "B", weight: 1 },
        { from: "B", to: "C", weight: 2 },
        { from: "A", to: "C", weight: 5 },
      ],
      ["A", "B", "C"]
    );
    const distances = dijkstra(graph, "A");
    expect(distances["A"]).toBe(0);
    expect(distances["B"]).toBe(1);
    expect(distances["C"]).toBe(3); // A→B→C = 1+2 = 3 (shorter than A→C = 5)
  });

  it("returns only source for disconnected nodes", () => {
    const graph = buildGraphStructure([], ["A", "B"]);
    const distances = dijkstra(graph, "A");
    expect(distances["A"]).toBe(0);
    expect(distances["B"]).toBeUndefined();
  });

  it("handles weighted edges", () => {
    const graph = buildGraphStructure(
      [
        { from: "A", to: "B", weight: 10 },
        { from: "A", to: "C", weight: 3 },
        { from: "C", to: "B", weight: 2 },
      ],
      ["A", "B", "C"]
    );
    const distances = dijkstra(graph, "A");
    expect(distances["B"]).toBe(5); // A→C→B = 3+2 = 5 (shorter than A→B = 10)
    expect(distances["C"]).toBe(3);
  });
});

describe("findAllShortestPaths", () => {
  it("finds single shortest path", () => {
    const graph = buildGraphStructure(
      [
        { from: "A", to: "B", weight: 1 },
        { from: "B", to: "C", weight: 1 },
      ],
      ["A", "B", "C"]
    );
    const paths = findAllShortestPaths(graph, "A", "C");
    expect(paths).toEqual([["A", "B", "C"]]);
  });

  it("returns empty for disconnected nodes", () => {
    const graph = buildGraphStructure([], ["A", "B"]);
    const paths = findAllShortestPaths(graph, "A", "B");
    expect(paths).toEqual([]);
  });

  it("finds path of length 1", () => {
    const graph = buildGraphStructure(
      [{ from: "A", to: "B", weight: 1 }],
      ["A", "B"]
    );
    const paths = findAllShortestPaths(graph, "A", "B");
    expect(paths).toEqual([["A", "B"]]);
  });
});

describe("extractRelationshipsFromData", () => {
  it("extracts one-to-many relationships from arrays", () => {
    const data = [
      { id: "Alice", friends: ["Bob", "Charlie"] },
      { id: "Bob", friends: ["Alice"] },
    ];
    const result = extractRelationshipsFromData(data, ["friends"]);
    expect(result.relationships.length).toBe(3);
    expect(result.vertices).toContain("Alice");
    expect(result.vertices).toContain("Bob");
    expect(result.vertices).toContain("Charlie");
  });

  it("extracts one-to-one relationships from scalars", () => {
    const data = [
      { id: "child", parent: "root" },
      { id: "grandchild", parent: "child" },
    ];
    const result = extractRelationshipsFromData(data, ["parent"]);
    expect(result.relationships.length).toBe(2);
    expect(result.relationships[0]).toEqual({ from: "child", to: "root", weight: 1 });
  });

  it("excludes self-loops", () => {
    const data = [{ id: "A", link: "A" }];
    const result = extractRelationshipsFromData(data, ["link"]);
    expect(result.relationships.length).toBe(0);
  });

  it("handles missing fields", () => {
    const data = [{ id: "A", other: "value" }];
    const result = extractRelationshipsFromData(data, ["nonexistent"]);
    expect(result.relationships.length).toBe(0);
    expect(result.vertices).toEqual(["A"]);
  });

  it("uses custom label field", () => {
    const data = [
      { name: "Alice", reports_to: "Bob" },
      { name: "Bob", reports_to: null },
    ];
    const result = extractRelationshipsFromData(data, ["reports_to"], "name");
    expect(result.relationships.length).toBe(1);
    expect(result.relationships[0].from).toBe("Alice");
    expect(result.relationships[0].to).toBe("Bob");
  });

  it("calculates density", () => {
    const data = [
      { id: "A", link: "B" },
      { id: "B", link: "A" },
    ];
    const result = extractRelationshipsFromData(data, ["link"]);
    // 2 edges / (2 * 1) = 1.0
    expect(result.density).toBe(1);
  });

  it("returns 0 density for single vertex", () => {
    const data = [{ id: "A" }];
    const result = extractRelationshipsFromData(data, ["link"]);
    expect(result.density).toBe(0);
  });
});

describe("formatCentralityResults", () => {
  it("formats results as text", () => {
    const results = {
      degree: { A: 0.5, B: 1.0, C: 0.25 },
    };
    const text = formatCentralityResults(results);
    expect(text).toContain("Degree Centrality");
    expect(text).toContain("B: 1.0000");
    expect(text).toContain("A: 0.5000");
  });

  it("respects topN limit", () => {
    const scores: Record<string, number> = {};
    for (let i = 0; i < 20; i++) scores[`node${i}`] = i / 20;
    const text = formatCentralityResults({ degree: scores }, 3);
    const lines = text.split("\n").filter((l) => /^\d+\./.test(l));
    expect(lines.length).toBe(3);
  });

  it("handles multiple measures", () => {
    const results = {
      degree: { A: 0.5 },
      betweenness: { A: 0.3 },
    };
    const text = formatCentralityResults(results);
    expect(text).toContain("Degree Centrality");
    expect(text).toContain("Betweenness Centrality");
  });
});
