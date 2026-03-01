import { describe, it, expect } from "vitest";
import {
  findTagsNode,
  extractTagDefinitions,
  findTaggedNodes,
  buildTaskMapData,
  generateTaskMap,
} from "./task-map.js";
import type { WorkflowyNode } from "../types/index.js";

// ── Fixtures ──

const mockNodes: WorkflowyNode[] = [
  { id: "tags-node", name: "Tags" },
  { id: "tag-inbox", name: "#inbox", parent_id: "tags-node" },
  { id: "tag-review", name: "#review", parent_id: "tags-node" },
  { id: "tag-alice", name: "@alice", parent_id: "tags-node" },
  { id: "proj-1", name: "Project Alpha" },
  { id: "task-1", name: "Fix login #inbox @alice", parent_id: "proj-1", modifiedAt: 3000 },
  { id: "task-2", name: "Review PR #review @alice", parent_id: "proj-1", modifiedAt: 2000 },
  { id: "task-3", name: "Triage bug #inbox #review", parent_id: "proj-1", modifiedAt: 1000 },
  { id: "task-4", name: "Write docs", parent_id: "proj-1" },
  { id: "task-5", name: "Done thing #inbox", parent_id: "proj-1", completedAt: 1700000000, modifiedAt: 500 },
];

// ── findTagsNode ──

describe("findTagsNode", () => {
  it("finds root-level Tags node", () => {
    const result = findTagsNode(mockNodes);
    expect(result?.id).toBe("tags-node");
  });

  it("returns null when no Tags node exists", () => {
    const nodes = mockNodes.filter(n => n.id !== "tags-node");
    expect(findTagsNode(nodes)).toBeNull();
  });

  it("ignores non-root node named Tags", () => {
    const nodes: WorkflowyNode[] = [
      { id: "root", name: "Root" },
      { id: "child-tags", name: "Tags", parent_id: "root" },
    ];
    expect(findTagsNode(nodes)).toBeNull();
  });

  it("handles HTML-wrapped name", () => {
    const nodes: WorkflowyNode[] = [
      { id: "html-tags", name: "<b>Tags</b>" },
    ];
    expect(findTagsNode(nodes)?.id).toBe("html-tags");
  });

  it("finds case-insensitive match", () => {
    const nodes: WorkflowyNode[] = [
      { id: "upper", name: "TAGS" },
    ];
    expect(findTagsNode(nodes)?.id).toBe("upper");
  });

  it("finds #tags variant", () => {
    const nodes: WorkflowyNode[] = [
      { id: "hash-tags", name: "#Tags" },
    ];
    // #tags gets parsed as a tag, but the clean name is "#Tags" → "tags" won't match
    // Actually: cleanName strips HTML only, so "#Tags" stays. lowercase = "#tags".
    // We check for "tags" or "#tags" — this should match.
    expect(findTagsNode(nodes)?.id).toBe("hash-tags");
  });
});

// ── extractTagDefinitions ──

describe("extractTagDefinitions", () => {
  const tagsNode = mockNodes.find(n => n.id === "tags-node")!;

  it("extracts #tags from children", () => {
    const defs = extractTagDefinitions(tagsNode, mockNodes);
    const tagDefs = defs.filter(d => d.type === "tag");
    expect(tagDefs.map(d => d.normalized)).toContain("inbox");
    expect(tagDefs.map(d => d.normalized)).toContain("review");
  });

  it("extracts @mentions from children", () => {
    const defs = extractTagDefinitions(tagsNode, mockNodes);
    const mentionDefs = defs.filter(d => d.type === "mention");
    expect(mentionDefs.map(d => d.normalized)).toContain("alice");
  });

  it("sets definitionNodeId correctly", () => {
    const defs = extractTagDefinitions(tagsNode, mockNodes);
    const inboxDef = defs.find(d => d.normalized === "inbox");
    expect(inboxDef?.definitionNodeId).toBe("tag-inbox");
  });

  it("uses fallback for plain text children", () => {
    const nodes: WorkflowyNode[] = [
      { id: "tags", name: "Tags" },
      { id: "plain", name: "Leadership", parent_id: "tags" },
    ];
    const defs = extractTagDefinitions(nodes[0], nodes);
    expect(defs).toHaveLength(1);
    expect(defs[0].normalized).toBe("leadership");
    expect(defs[0].type).toBe("tag");
  });

  it("handles child with both tag and mention", () => {
    const nodes: WorkflowyNode[] = [
      { id: "tags", name: "Tags" },
      { id: "mixed", name: "#project @bob", parent_id: "tags" },
    ];
    const defs = extractTagDefinitions(nodes[0], nodes);
    expect(defs).toHaveLength(2);
    expect(defs.find(d => d.normalized === "project")?.type).toBe("tag");
    expect(defs.find(d => d.normalized === "bob")?.type).toBe("mention");
  });
});

// ── findTaggedNodes ──

describe("findTaggedNodes", () => {
  const tagsNode = mockNodes.find(n => n.id === "tags-node")!;
  const defs = extractTagDefinitions(tagsNode, mockNodes);

  it("finds nodes matching #tag", () => {
    const tagged = findTaggedNodes(defs, mockNodes);
    const inboxNodes = tagged.filter(tn =>
      tn.matchedTags.some(mt => mt.normalized === "inbox")
    );
    // task-1, task-3, task-5 have #inbox
    expect(inboxNodes.map(tn => tn.node.id)).toContain("task-1");
    expect(inboxNodes.map(tn => tn.node.id)).toContain("task-3");
    expect(inboxNodes.map(tn => tn.node.id)).toContain("task-5");
  });

  it("finds nodes matching @mention", () => {
    const tagged = findTaggedNodes(defs, mockNodes);
    const aliceNodes = tagged.filter(tn =>
      tn.matchedTags.some(mt => mt.normalized === "alice")
    );
    // task-1, task-2 have @alice
    expect(aliceNodes.map(tn => tn.node.id)).toContain("task-1");
    expect(aliceNodes.map(tn => tn.node.id)).toContain("task-2");
  });

  it("returns multiple matched tags per node", () => {
    const tagged = findTaggedNodes(defs, mockNodes);
    const task3 = tagged.find(tn => tn.node.id === "task-3");
    // task-3 has #inbox and #review
    expect(task3?.matchedTags.length).toBe(2);
  });

  it("excludes completed nodes when option set", () => {
    const tagged = findTaggedNodes(defs, mockNodes, { excludeCompleted: true });
    expect(tagged.find(tn => tn.node.id === "task-5")).toBeUndefined();
  });

  it("includes completed nodes by default", () => {
    const tagged = findTaggedNodes(defs, mockNodes);
    expect(tagged.find(tn => tn.node.id === "task-5")).toBeDefined();
  });

  it("does not match untagged nodes", () => {
    const tagged = findTaggedNodes(defs, mockNodes);
    expect(tagged.find(tn => tn.node.id === "task-4")).toBeUndefined();
  });

  it("matches by prefix (e.g. #action_ matches #action_review)", () => {
    const prefixDefs: TagDefinition[] = [
      { raw: "#action_", normalized: "action_", type: "tag", definitionNodeId: "def-1" },
    ];
    const nodes: WorkflowyNode[] = [
      { id: "n1", name: "Do thing #action_review" },
      { id: "n2", name: "Other #action_plan" },
      { id: "n3", name: "No tag here" },
    ];
    const tagged = findTaggedNodes(prefixDefs, nodes);
    expect(tagged.map(tn => tn.node.id)).toEqual(["n1", "n2"]);
  });
});

// ── buildTaskMapData ──

describe("buildTaskMapData", () => {
  const tagsNode = mockNodes.find(n => n.id === "tags-node")!;
  const defs = extractTagDefinitions(tagsNode, mockNodes);
  const tagged = findTaggedNodes(defs, mockNodes);

  it("creates one major concept per tag definition", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged);
    const majors = data.concepts.filter(c => c.level === "major");
    expect(majors).toHaveLength(defs.length);
  });

  it("creates detail concepts for matched nodes", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged);
    const details = data.concepts.filter(c => c.level === "detail");
    expect(details.length).toBeGreaterThan(0);
  });

  it("caps details at maxDetailsPerTag", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged, { maxDetailsPerTag: 1 });
    // Each tag should have at most 1 detail
    for (const def of defs) {
      const majorId = `tag-${def.normalized}`;
      const details = data.concepts.filter(
        c => c.level === "detail" && c.parentMajorId === majorId
      );
      expect(details.length).toBeLessThanOrEqual(1);
    }
  });

  it("sorts details by recency by default", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged);
    const inboxDetails = data.concepts.filter(
      c => c.level === "detail" && c.parentMajorId === "tag-inbox"
    );
    // task-1 (modifiedAt: 3000) should be first
    expect(inboxDetails[0]?.label).toBe("Fix login #inbox @alice");
  });

  it("sorts details by name when configured", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged, { detailSortBy: "name" });
    const inboxDetails = data.concepts.filter(
      c => c.level === "detail" && c.parentMajorId === "tag-inbox"
    );
    const labels = inboxDetails.map(c => c.label);
    const sorted = [...labels].sort();
    expect(labels).toEqual(sorted);
  });

  it("computes co-occurrence relationships", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged);
    // task-3 has #inbox + #review → relationship between tag-inbox and tag-review
    // task-1 has #inbox + @alice → relationship between tag-inbox and tag-alice
    // task-2 has #review + @alice → relationship between tag-review and tag-alice
    expect(data.relationships.length).toBe(3);
  });

  it("sets relationship strength from co-occurrence count", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged);
    // Each pair co-occurs exactly once in our fixture
    for (const rel of data.relationships) {
      expect(rel.strength).toBe(1);
    }
  });

  it("sets workflowyNodeId on major concepts", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged);
    const inboxMajor = data.concepts.find(c => c.id === "tag-inbox");
    expect(inboxMajor?.workflowyNodeId).toBe("tag-inbox");
  });

  it("sets workflowyNodeId on detail concepts", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged);
    const details = data.concepts.filter(c => c.level === "detail");
    for (const d of details) {
      expect(d.workflowyNodeId).toBeDefined();
    }
  });

  it("produces valid ClaudeAnalysis", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged);
    expect(data.analysis.title).toBe("Task Map");
    expect(data.analysis.core_label).toBe("Task Map");
    expect(data.analysis.concepts.length).toBe(data.concepts.length);
    expect(data.analysis.relationships.length).toBe(data.relationships.length);
  });

  it("uses custom title", () => {
    const data = buildTaskMapData(tagsNode, defs, tagged, { title: "My Tasks" });
    expect(data.title).toBe("My Tasks");
    expect(data.analysis.title).toBe("My Tasks");
  });

  it("handles tag with zero matches", () => {
    const extraDefs = [...defs, {
      raw: "#orphan",
      normalized: "orphan",
      type: "tag" as const,
      definitionNodeId: "orphan-node",
    }];
    const data = buildTaskMapData(tagsNode, extraDefs, tagged);
    const orphanMajor = data.concepts.find(c => c.id === "tag-orphan");
    expect(orphanMajor).toBeDefined();
    expect(orphanMajor?.importance).toBe(1);
    const orphanDetails = data.concepts.filter(
      c => c.level === "detail" && c.parentMajorId === "tag-orphan"
    );
    expect(orphanDetails).toHaveLength(0);
  });
});

// ── generateTaskMap ──

describe("generateTaskMap", () => {
  it("excludes @mentions by default", () => {
    const data = generateTaskMap(mockNodes);
    expect(data.tagsNode.id).toBe("tags-node");
    // Only #inbox and #review (not @alice)
    expect(data.tagDefinitions.length).toBe(2);
    expect(data.concepts.filter(c => c.level === "major")).toHaveLength(2);
  });

  it("includes @mentions when excludeMentions is false", () => {
    const data = generateTaskMap(mockNodes, { excludeMentions: false });
    expect(data.tagDefinitions.length).toBe(3);
    expect(data.concepts.filter(c => c.level === "major")).toHaveLength(3);
  });

  it("excludes tag definition nodes from results", () => {
    const data = generateTaskMap(mockNodes, { excludeMentions: false });
    const allDetailIds = data.concepts
      .filter(c => c.level === "detail")
      .map(c => c.workflowyNodeId);
    // Tag definition nodes should not appear as details
    expect(allDetailIds).not.toContain("tag-inbox");
    expect(allDetailIds).not.toContain("tag-review");
    expect(allDetailIds).not.toContain("tag-alice");
    expect(allDetailIds).not.toContain("tags-node");
  });

  it("throws when no Tags node found", () => {
    const nodes = mockNodes.filter(n => n.id !== "tags-node");
    expect(() => generateTaskMap(nodes)).toThrow("No root-level 'Tags' node found");
  });

  it("passes options through", () => {
    const data = generateTaskMap(mockNodes, { excludeCompleted: true, title: "Custom" });
    expect(data.title).toBe("Custom");
    // task-5 (completed) should not appear in details
    const allDetailIds = data.concepts
      .filter(c => c.level === "detail")
      .map(c => c.workflowyNodeId);
    expect(allDetailIds).not.toContain("task-5");
  });
});
