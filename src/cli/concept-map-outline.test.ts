import { describe, it, expect, vi, beforeEach } from "vitest";
import { buildOutlineNodeName, insertConceptMapOutline, _setThrottleMs } from "./concept-map-outline.js";
import type { ClaudeAnalysis, WorkflowyNode } from "../shared/types/index.js";

// Disable throttle for tests
_setThrottleMs(0);

// Mock the API module
vi.mock("../shared/api/workflowy.js", () => ({
  createNode: vi.fn(),
  deleteNode: vi.fn(),
}));

import { createNode, deleteNode } from "../shared/api/workflowy.js";

const mockCreateNode = vi.mocked(createNode);
const mockDeleteNode = vi.mocked(deleteNode);

// ── Test fixtures ──

function makeAnalysis(overrides?: Partial<ClaudeAnalysis>): ClaudeAnalysis {
  return {
    title: "Test Map",
    core_label: "Central Idea",
    concepts: [
      { id: "m1", label: "Major One", level: "major", importance: 8 },
      { id: "m2", label: "Major Two", level: "major", importance: 6 },
      { id: "d1", label: "Detail Alpha", level: "detail", importance: 4, parent_major_id: "m1", workflowy_node_id: "wf-detail-1" },
      { id: "d2", label: "Detail Beta", level: "detail", importance: 3, parent_major_id: "m2" },
    ],
    relationships: [
      { from: "m1", to: "m2", type: "enables", strength: 7 },
    ],
    ...overrides,
  };
}

const targetNode: WorkflowyNode = {
  id: "target-123",
  name: "Organisational Prompts",
  note: "Existing note content",
  parent_id: "parent-456",
};

const allNodes: WorkflowyNode[] = [
  { id: "parent-456", name: "Parent", parent_id: undefined },
  targetNode,
  { id: "sibling-789", name: "Other Sibling", parent_id: "parent-456" },
];

const nodeIdMap = new Map<string, string>([
  ["major one", "wf-major-1"],
]);

// ── Tests ──

describe("buildOutlineNodeName", () => {
  it("includes depth when specified", () => {
    expect(buildOutlineNodeName("Prompts", 3)).toBe("Concept Map - Prompts - Level 3");
  });

  it("uses 'all levels' when depth is undefined", () => {
    expect(buildOutlineNodeName("Prompts", undefined)).toBe("Concept Map - Prompts - all levels");
  });

  it("strips HTML tags from node name", () => {
    expect(buildOutlineNodeName('<b><span class="colored">My Node</span></b>', 2))
      .toBe("Concept Map - My Node - Level 2");
  });
});

describe("insertConceptMapOutline", () => {
  let callIndex: number;

  beforeEach(() => {
    vi.clearAllMocks();
    callIndex = 0;

    mockCreateNode.mockImplementation(async () => {
      callIndex++;
      return { id: `created-${callIndex}` };
    });
    mockDeleteNode.mockResolvedValue(undefined);
  });

  // ── Placement ──

  it("creates root outline node under target node itself", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    const rootCall = mockCreateNode.mock.calls[0][0];
    expect(rootCall.name).toBe("Concept Map - Organisational Prompts - Level 3");
    expect(rootCall.parent_id).toBe("target-123");
  });

  // ── Core concept ──

  it("creates core concept node under outline root", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    const coreCall = mockCreateNode.mock.calls[1][0];
    expect(coreCall.name).toBe("Central Idea");
    expect(coreCall.parent_id).toBe("created-1");
  });

  it("creates source link as child of core concept", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    const linkCall = mockCreateNode.mock.calls[2][0];
    expect(linkCall.name).toContain("https://workflowy.com/#/target-123");
    expect(linkCall.parent_id).toBe("created-2"); // under core node
  });

  // ── Major Concepts ──

  it("creates Major Concepts section under outline root", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    const sectionCall = mockCreateNode.mock.calls[3][0];
    expect(sectionCall.name).toBe("Major Concepts");
    expect(sectionCall.parent_id).toBe("created-1");
  });

  it("creates major concept nodes without importance", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    const major1Call = mockCreateNode.mock.calls[4][0];
    expect(major1Call.name).toBe("Major One");
    expect(major1Call.parent_id).toBe("created-4"); // Major Concepts section
    expect(major1Call.note).toBeUndefined();
  });

  it("creates link child for concepts with mapped nodeId", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    // Major One is mapped via nodeIdMap to wf-major-1
    // Call 4 = Major One, Call 5 = link child
    const linkCall = mockCreateNode.mock.calls[5][0];
    expect(linkCall.name).toContain("https://workflowy.com/#/wf-major-1");
    expect(linkCall.parent_id).toBe("created-5"); // under Major One
  });

  it("creates link child for concepts with workflowy_node_id", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    // Detail Alpha has workflowy_node_id = "wf-detail-1"
    // Find the Detail Alpha node and its link child
    const detailCall = mockCreateNode.mock.calls.find(
      call => call[0].name === "Detail Alpha"
    );
    expect(detailCall).toBeDefined();

    // The next call after Detail Alpha should be its link child
    const detailIdx = mockCreateNode.mock.calls.indexOf(detailCall!);
    const linkCall = mockCreateNode.mock.calls[detailIdx + 1][0];
    expect(linkCall.name).toContain("https://workflowy.com/#/wf-detail-1");
  });

  it("does not create link child for unmapped concepts", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    // Major Two has no mapping — its next sibling should NOT be a link
    const major2Call = mockCreateNode.mock.calls.find(
      call => call[0].name === "Major Two"
    );
    expect(major2Call).toBeDefined();
    const major2Idx = mockCreateNode.mock.calls.indexOf(major2Call!);
    const nextCall = mockCreateNode.mock.calls[major2Idx + 1][0];
    // Next call should be Detail Beta (child concept), not a link
    expect(nextCall.name).toBe("Detail Beta");
  });

  // ── Relationships ──

  it("creates Relationships section under root", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    const relsSectionCall = mockCreateNode.mock.calls.find(
      call => call[0].name === "Relationships"
    );
    expect(relsSectionCall).toBeDefined();
    expect(relsSectionCall![0].parent_id).toBe("created-1");
  });

  it("creates relationship nodes with arrow notation", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    const relCall = mockCreateNode.mock.calls.find(
      call => call[0].name?.includes("--enables-->")
    );
    expect(relCall).toBeDefined();
    expect(relCall![0].name).toBe("Major One --enables--> Major Two");
    expect(relCall![0].note).toBeUndefined();
  });

  it("creates link children under relationship nodes", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    const relCall = mockCreateNode.mock.calls.find(
      call => call[0].name?.includes("--enables-->")
    );
    const relIdx = mockCreateNode.mock.calls.indexOf(relCall!);

    // Next two calls should be link children for from and to
    const fromLink = mockCreateNode.mock.calls[relIdx + 1][0];
    const toLink = mockCreateNode.mock.calls[relIdx + 2][0];
    expect(fromLink.name).toContain("workflowy.com/#/created-");
    expect(toLink.name).toContain("workflowy.com/#/created-");
  });

  // ── Existing outline handling ──

  it("throws when existing outline found without --force", async () => {
    const nodesWithExisting: WorkflowyNode[] = [
      ...allNodes,
      {
        id: "existing-outline",
        name: "Concept Map - Organisational Prompts - Level 3",
        parent_id: "target-123",
      },
    ];

    await expect(
      insertConceptMapOutline(makeAnalysis(), targetNode, nodesWithExisting, 3, nodeIdMap, false)
    ).rejects.toThrow("already exists");
  });

  it("deletes and recreates with --force", async () => {
    const nodesWithExisting: WorkflowyNode[] = [
      ...allNodes,
      {
        id: "existing-outline",
        name: "Concept Map - Organisational Prompts - Level 3",
        parent_id: "target-123",
      },
    ];

    await insertConceptMapOutline(makeAnalysis(), targetNode, nodesWithExisting, 3, nodeIdMap, true);

    expect(mockDeleteNode).toHaveBeenCalledWith("existing-outline");
    expect(mockCreateNode).toHaveBeenCalled();
  });

  // ── Return values ──

  it("uses 'all levels' in name when depth is undefined", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, undefined, nodeIdMap, false);

    const rootCall = mockCreateNode.mock.calls[0][0];
    expect(rootCall.name).toBe("Concept Map - Organisational Prompts - all levels");
  });

  it("resolves 'core' ID in relationships to core_label", async () => {
    const analysis = makeAnalysis({
      relationships: [
        { from: "core", to: "m1", type: "organizes", strength: 9 },
      ],
    });
    await insertConceptMapOutline(analysis, targetNode, allNodes, 3, nodeIdMap, false);

    const relCall = mockCreateNode.mock.calls.find(
      call => call[0].name?.includes("--organizes-->")
    );
    expect(relCall).toBeDefined();
    expect(relCall![0].name).toBe("Central Idea --organizes--> Major One");
  });

  // ── No importance in output ──

  it("does not include importance in any node", async () => {
    await insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false);

    for (const call of mockCreateNode.mock.calls) {
      const name = call[0].name || "";
      const note = call[0].note || "";
      expect(name).not.toContain("Importance");
      expect(note).not.toContain("Importance");
    }
  });

  // ── Error handling ──

  it("cleans up outline root on error", async () => {
    let callCount = 0;
    mockCreateNode.mockImplementation(async () => {
      callCount++;
      if (callCount === 3) throw new Error("API failure");
      return { id: `created-${callCount}` };
    });

    await expect(
      insertConceptMapOutline(makeAnalysis(), targetNode, allNodes, 3, nodeIdMap, false)
    ).rejects.toThrow("API failure");

    expect(mockDeleteNode).toHaveBeenCalledWith("created-1");
  });
});
