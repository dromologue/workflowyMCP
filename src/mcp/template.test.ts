import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";
import { getSubtreeNodes, buildChildrenIndex } from "../shared/utils/scope-utils.js";

function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

describe("create_from_template logic", () => {
  const templateNodes: WorkflowyNode[] = [
    createMockNode({ id: "tmpl", name: "{{project_name}} Plan", note: "Owner: {{owner}}" }),
    createMockNode({ id: "tmpl-c1", name: "Phase 1: {{phase1}}", parent_id: "tmpl" }),
    createMockNode({ id: "tmpl-c2", name: "Phase 2: {{phase2}}", parent_id: "tmpl", note: "Follow up with {{owner}}" }),
    createMockNode({ id: "tmpl-gc", name: "Task: {{task}}", parent_id: "tmpl-c1" }),
  ];

  function substituteVars(text: string, variables: Record<string, string>): string {
    return text.replace(/\{\{(\w+)\}\}/g, (match, key) => {
      return key in variables ? variables[key] : match;
    });
  }

  describe("variable substitution", () => {
    it("replaces all occurrences of a variable", () => {
      const result = substituteVars("Hello {{name}}, welcome {{name}}!", { name: "Alice" });
      expect(result).toBe("Hello Alice, welcome Alice!");
    });

    it("replaces multiple different variables", () => {
      const result = substituteVars("{{greeting}} {{name}}", { greeting: "Hi", name: "Bob" });
      expect(result).toBe("Hi Bob");
    });

    it("leaves unmatched variables as-is", () => {
      const result = substituteVars("{{known}} and {{unknown}}", { known: "replaced" });
      expect(result).toBe("replaced and {{unknown}}");
    });

    it("handles empty variables map", () => {
      const result = substituteVars("{{keep}} this", {});
      expect(result).toBe("{{keep}} this");
    });

    it("handles text with no variables", () => {
      const result = substituteVars("Plain text", { name: "unused" });
      expect(result).toBe("Plain text");
    });
  });

  describe("template traversal", () => {
    it("processes nested subtrees", () => {
      const subtree = getSubtreeNodes("tmpl", templateNodes);
      expect(subtree).toHaveLength(4);
    });

    it("applies substitution to name and note", () => {
      const variables = { project_name: "Alpha", owner: "Alice", phase1: "Research", phase2: "Build", task: "Survey" };
      const subtree = getSubtreeNodes("tmpl", templateNodes);

      const substituted = subtree.map((n) => ({
        ...n,
        name: substituteVars(n.name || "", variables),
        note: n.note ? substituteVars(n.note, variables) : undefined,
      }));

      const byId = (id: string) => substituted.find((n) => n.id === id)!;
      expect(byId("tmpl").name).toBe("Alpha Plan");
      expect(byId("tmpl").note).toBe("Owner: Alice");
      expect(byId("tmpl-c1").name).toBe("Phase 1: Research");
      expect(byId("tmpl-c2").name).toBe("Phase 2: Build");
      expect(byId("tmpl-c2").note).toBe("Follow up with Alice");
      expect(byId("tmpl-gc").name).toBe("Task: Survey");
    });

    it("preserves parent-child ordering", () => {
      const subtree = getSubtreeNodes("tmpl", templateNodes);
      const childrenIndex = buildChildrenIndex(subtree);

      const ordered: string[] = [];
      const visit = (nodeId: string) => {
        ordered.push(nodeId);
        const children = childrenIndex.get(nodeId) || [];
        for (const child of children) visit(child.id);
      };
      visit("tmpl");

      expect(ordered).toEqual(["tmpl", "tmpl-c1", "tmpl-gc", "tmpl-c2"]);
    });
  });
});
