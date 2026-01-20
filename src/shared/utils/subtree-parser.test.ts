/**
 * Tests for subtree parser - multi-agent content splitting
 */

import { describe, it, expect } from "vitest";
import {
  splitIntoSubtrees,
  estimateTimeSavings,
  mergeSubtreeResults,
  type SubtreeResult,
} from "./subtree-parser.js";

describe("splitIntoSubtrees", () => {
  it("handles empty content", () => {
    const result = splitIntoSubtrees("");
    expect(result.subtrees).toHaveLength(0);
    expect(result.totalNodes).toBe(0);
    expect(result.recommendedAgents).toBe(0);
  });

  it("handles single top-level node", () => {
    const content = "Single node";
    const result = splitIntoSubtrees(content);
    expect(result.subtrees).toHaveLength(1);
    expect(result.totalNodes).toBe(1);
    expect(result.subtrees[0].nodeCount).toBe(1);
  });

  it("splits multiple top-level nodes into separate subtrees", () => {
    const content = `First item
  Child of first
Second item
  Child of second
Third item`;

    const result = splitIntoSubtrees(content, {
      targetNodesPerSubtree: 2,
      minNodesPerSubtree: 1,
    });

    expect(result.totalNodes).toBe(5);
    // Should create multiple subtrees
    expect(result.subtrees.length).toBeGreaterThanOrEqual(1);
  });

  it("groups small subtrees together to meet target size", () => {
    const content = `Item 1
Item 2
Item 3
Item 4
Item 5`;

    const result = splitIntoSubtrees(content, {
      targetNodesPerSubtree: 10,
      minNodesPerSubtree: 3,
    });

    // Should group all 5 items into one subtree since target is 10
    expect(result.subtrees.length).toBeLessThanOrEqual(2);
  });

  it("normalizes indent levels within subtrees", () => {
    const content = `Parent
  Child 1
    Grandchild
  Child 2`;

    const result = splitIntoSubtrees(content);
    const subtree = result.subtrees[0];

    // First line should have indent 0 (normalized)
    expect(subtree.lines[0].indent).toBe(0);
    // Child should have indent 1
    expect(subtree.lines[1].indent).toBe(1);
    // Grandchild should have indent 2
    expect(subtree.lines[2].indent).toBe(2);
  });

  it("respects max subtrees limit", () => {
    const content = Array(20).fill("Item").map((s, i) => s + ` ${i + 1}`).join("\n");

    const result = splitIntoSubtrees(content, {
      maxSubtrees: 3,
      targetNodesPerSubtree: 2,
      minNodesPerSubtree: 1,
    });

    expect(result.subtrees.length).toBeLessThanOrEqual(3);
  });

  it("estimates processing time correctly", () => {
    const content = `Item 1
Item 2
Item 3
Item 4
Item 5`;

    const result = splitIntoSubtrees(content, {
      requestsPerSecond: 5,
    });

    // 5 nodes at 5 req/sec = 1000ms total
    expect(result.singleAgentEstimateMs).toBe(1000);
  });

  it("calculates parallel estimate including overhead", () => {
    const content = Array(100).fill(0).map((_, i) => `Item ${i + 1}`).join("\n");

    const result = splitIntoSubtrees(content, {
      targetNodesPerSubtree: 25,
      maxSubtrees: 5,
    });

    // Parallel should be faster than single agent
    expect(result.parallelEstimateMs).toBeLessThan(result.singleAgentEstimateMs);
  });
});

describe("estimateTimeSavings", () => {
  it("calculates single agent time correctly", () => {
    const result = estimateTimeSavings(100, 1, 5);
    // 100 nodes / 5 req per sec = 20 seconds = 20000ms
    expect(result.singleAgentMs).toBe(20000);
  });

  it("shows savings with multiple agents", () => {
    const result = estimateTimeSavings(100, 5, 5);

    expect(result.savingsPercent).toBeGreaterThan(0);
    expect(result.parallelMs).toBeLessThan(result.singleAgentMs);
  });

  it("accounts for coordination overhead", () => {
    const result = estimateTimeSavings(100, 5, 5);

    // Parallel time should include overhead (not just 1/5 of single agent)
    const theoreticalParallel = result.singleAgentMs / 5;
    expect(result.parallelMs).toBeGreaterThan(theoreticalParallel);
  });

  it("handles single agent case", () => {
    const result = estimateTimeSavings(50, 1, 5);

    expect(result.savingsPercent).toBe(0);
    expect(result.savingsSeconds).toBe(0);
  });
});

describe("mergeSubtreeResults", () => {
  it("merges successful results", () => {
    const results: SubtreeResult[] = [
      { subtreeId: "s1", success: true, nodeIds: ["a", "b"], durationMs: 100 },
      { subtreeId: "s2", success: true, nodeIds: ["c", "d"], durationMs: 150 },
    ];

    const merged = mergeSubtreeResults(results);

    expect(merged.success).toBe(true);
    expect(merged.createdNodes).toBe(4);
    expect(merged.allNodeIds).toEqual(["a", "b", "c", "d"]);
    expect(merged.failedSubtrees).toHaveLength(0);
  });

  it("tracks failed subtrees", () => {
    const results: SubtreeResult[] = [
      { subtreeId: "s1", success: true, nodeIds: ["a"], durationMs: 100 },
      { subtreeId: "s2", success: false, nodeIds: [], error: "API error", durationMs: 50 },
    ];

    const merged = mergeSubtreeResults(results);

    expect(merged.success).toBe(false);
    expect(merged.createdNodes).toBe(1);
    expect(merged.failedSubtrees).toEqual(["s2"]);
    expect(merged.errors).toHaveLength(1);
    expect(merged.errors[0].error).toBe("API error");
  });

  it("uses max duration for total time", () => {
    const results: SubtreeResult[] = [
      { subtreeId: "s1", success: true, nodeIds: ["a"], durationMs: 100 },
      { subtreeId: "s2", success: true, nodeIds: ["b"], durationMs: 200 },
      { subtreeId: "s3", success: true, nodeIds: ["c"], durationMs: 150 },
    ];

    const merged = mergeSubtreeResults(results);

    // Parallel execution means total time is max of individual times
    expect(merged.totalDurationMs).toBe(200);
  });

  it("handles all failures", () => {
    const results: SubtreeResult[] = [
      { subtreeId: "s1", success: false, nodeIds: [], error: "Error 1", durationMs: 50 },
      { subtreeId: "s2", success: false, nodeIds: [], error: "Error 2", durationMs: 50 },
    ];

    const merged = mergeSubtreeResults(results);

    expect(merged.success).toBe(false);
    expect(merged.createdNodes).toBe(0);
    expect(merged.failedSubtrees).toHaveLength(2);
  });
});
