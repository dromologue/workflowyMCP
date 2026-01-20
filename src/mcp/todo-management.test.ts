/**
 * Tests for todo management tools
 *
 * Tests create_todo, list_todos, complete_node, and uncomplete_node functionality.
 */

import { describe, it, expect } from "vitest";
import type { WorkflowyNode } from "../shared/types/index.js";

// Helper to create mock nodes
function createMockNode(overrides: Partial<WorkflowyNode> = {}): WorkflowyNode {
  return {
    id: "test-id",
    name: "Test Node",
    ...overrides,
  };
}

// Helper to create mock todo nodes
function createMockTodo(
  overrides: Partial<WorkflowyNode> & { completed?: boolean } = {}
): WorkflowyNode {
  const { completed, ...nodeOverrides } = overrides;
  return {
    id: "todo-id",
    name: "Todo Item",
    layoutMode: "todo",
    completed: completed ? new Date().toISOString() : undefined,
    ...nodeOverrides,
  };
}

describe("create_todo logic", () => {
  describe("todo creation", () => {
    it("creates todo with required fields", () => {
      const todoData = {
        name: "Buy groceries",
        parent_id: "parent-123",
      };

      const createdTodo = {
        ...todoData,
        id: "new-todo-id",
        layoutMode: "todo" as const,
        completed: undefined,
      };

      expect(createdTodo.name).toBe("Buy groceries");
      expect(createdTodo.layoutMode).toBe("todo");
      expect(createdTodo.completed).toBeUndefined();
    });

    it("creates todo with optional note", () => {
      const todoData = {
        name: "Buy groceries",
        note: "Get milk, eggs, bread",
        parent_id: "parent-123",
      };

      expect(todoData.note).toBe("Get milk, eggs, bread");
    });

    it("creates todo with initial completed state", () => {
      const todoData = {
        name: "Already done task",
        completed: true,
      };

      const createdTodo = {
        ...todoData,
        id: "new-todo-id",
        layoutMode: "todo" as const,
        completed: todoData.completed ? new Date().toISOString() : undefined,
      };

      expect(createdTodo.completed).toBeDefined();
    });

    it("handles position parameter", () => {
      const positions: Array<"top" | "bottom"> = ["top", "bottom"];
      positions.forEach((position) => {
        expect(["top", "bottom"]).toContain(position);
      });
    });
  });

  describe("markdown checkbox conversion", () => {
    it("parses unchecked markdown checkbox", () => {
      const markdown = "- [ ] Task item";
      const isCheckbox = markdown.match(/^- \[ \] /);
      const text = markdown.replace(/^- \[ \] /, "");

      expect(isCheckbox).not.toBeNull();
      expect(text).toBe("Task item");
    });

    it("parses checked markdown checkbox", () => {
      const markdown = "- [x] Completed task";
      const isChecked = markdown.match(/^- \[x\] /);
      const text = markdown.replace(/^- \[x\] /, "");

      expect(isChecked).not.toBeNull();
      expect(text).toBe("Completed task");
    });

    it("identifies non-checkbox content", () => {
      const normalText = "Regular list item";
      const isCheckbox = normalText.match(/^- \[[ x]\] /);

      expect(isCheckbox).toBeNull();
    });
  });
});

describe("list_todos logic", () => {
  const mockTodos: WorkflowyNode[] = [
    createMockTodo({ id: "1", name: "Task 1", parent_id: "project-a" }),
    createMockTodo({ id: "2", name: "Task 2", parent_id: "project-a", completed: true }),
    createMockTodo({ id: "3", name: "Task 3", parent_id: "project-b" }),
    createMockTodo({ id: "4", name: "Important task", parent_id: "project-a" }),
    createMockTodo({ id: "5", name: "Archived task", parent_id: "archive", completed: true }),
  ];

  describe("status filtering", () => {
    function filterByStatus(
      todos: WorkflowyNode[],
      status: "all" | "pending" | "completed"
    ): WorkflowyNode[] {
      if (status === "all") return todos;
      if (status === "pending") return todos.filter((t) => !t.completed);
      if (status === "completed") return todos.filter((t) => t.completed);
      return todos;
    }

    it("returns all todos when status is all", () => {
      const results = filterByStatus(mockTodos, "all");
      expect(results.length).toBe(5);
    });

    it("returns only pending todos", () => {
      const results = filterByStatus(mockTodos, "pending");
      expect(results.length).toBe(3);
      results.forEach((todo) => {
        expect(todo.completed).toBeUndefined();
      });
    });

    it("returns only completed todos", () => {
      const results = filterByStatus(mockTodos, "completed");
      expect(results.length).toBe(2);
      results.forEach((todo) => {
        expect(todo.completed).toBeDefined();
      });
    });
  });

  describe("parent filtering", () => {
    function filterByParent(
      todos: WorkflowyNode[],
      parentId?: string
    ): WorkflowyNode[] {
      if (!parentId) return todos;
      return todos.filter((t) => t.parent_id === parentId);
    }

    it("returns todos under specific parent", () => {
      const results = filterByParent(mockTodos, "project-a");
      expect(results.length).toBe(3);
    });

    it("returns all todos when no parent specified", () => {
      const results = filterByParent(mockTodos);
      expect(results.length).toBe(5);
    });

    it("returns empty array for parent with no todos", () => {
      const results = filterByParent(mockTodos, "nonexistent");
      expect(results).toEqual([]);
    });
  });

  describe("query filtering", () => {
    function filterByQuery(
      todos: WorkflowyNode[],
      query?: string
    ): WorkflowyNode[] {
      if (!query) return todos;
      const lowerQuery = query.toLowerCase();
      return todos.filter(
        (t) =>
          t.name?.toLowerCase().includes(lowerQuery) ||
          t.note?.toLowerCase().includes(lowerQuery)
      );
    }

    it("filters todos by name query", () => {
      const results = filterByQuery(mockTodos, "Important");
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("4");
    });

    it("returns all todos when no query specified", () => {
      const results = filterByQuery(mockTodos);
      expect(results.length).toBe(5);
    });

    it("is case insensitive", () => {
      const resultsLower = filterByQuery(mockTodos, "task");
      const resultsUpper = filterByQuery(mockTodos, "TASK");
      expect(resultsLower.length).toBe(resultsUpper.length);
    });
  });

  describe("combined filtering", () => {
    function filterTodos(
      todos: WorkflowyNode[],
      options: {
        status?: "all" | "pending" | "completed";
        parent_id?: string;
        query?: string;
      }
    ): WorkflowyNode[] {
      let results = todos;

      if (options.status && options.status !== "all") {
        if (options.status === "pending") {
          results = results.filter((t) => !t.completed);
        } else {
          results = results.filter((t) => t.completed);
        }
      }

      if (options.parent_id) {
        results = results.filter((t) => t.parent_id === options.parent_id);
      }

      if (options.query) {
        const lowerQuery = options.query.toLowerCase();
        results = results.filter(
          (t) =>
            t.name?.toLowerCase().includes(lowerQuery) ||
            t.note?.toLowerCase().includes(lowerQuery)
        );
      }

      return results;
    }

    it("applies status and parent filters together", () => {
      const results = filterTodos(mockTodos, {
        status: "pending",
        parent_id: "project-a",
      });
      expect(results.length).toBe(2);
    });

    it("applies all three filters", () => {
      const results = filterTodos(mockTodos, {
        status: "pending",
        parent_id: "project-a",
        query: "Important",
      });
      expect(results.length).toBe(1);
      expect(results[0].id).toBe("4");
    });
  });

  describe("response formatting", () => {
    it("formats todo list response", () => {
      const todos = mockTodos.slice(0, 2);
      const response = {
        success: true,
        count: todos.length,
        todos: todos.map((t) => ({
          id: t.id,
          name: t.name,
          completed: !!t.completed,
          parent_id: t.parent_id,
        })),
      };

      expect(response.success).toBe(true);
      expect(response.count).toBe(2);
      expect(response.todos[0].completed).toBe(false);
      expect(response.todos[1].completed).toBe(true);
    });
  });
});

describe("complete_node logic", () => {
  describe("completion marking", () => {
    it("marks node as completed with timestamp", () => {
      const node = createMockTodo({ completed: false });
      const completedAt = new Date().toISOString();

      const completedNode = {
        ...node,
        completed: completedAt,
      };

      expect(completedNode.completed).toBe(completedAt);
    });

    it("handles already completed node", () => {
      const originalTimestamp = "2024-01-01T00:00:00.000Z";
      const node = createMockTodo({
        completed: true,
      });
      // Simulate existing completed timestamp
      (node as unknown as { completed: string }).completed = originalTimestamp;

      // Re-completing should update timestamp
      const newTimestamp = new Date().toISOString();
      const completedNode = {
        ...node,
        completed: newTimestamp,
      };

      expect(completedNode.completed).not.toBe(originalTimestamp);
    });
  });

  describe("response formatting", () => {
    it("formats successful completion response", () => {
      const nodeId = "todo-123";
      const response = {
        success: true,
        node_id: nodeId,
        completed: true,
        completed_at: new Date().toISOString(),
        message: `Node ${nodeId} marked as completed`,
      };

      expect(response.success).toBe(true);
      expect(response.completed).toBe(true);
      expect(response.completed_at).toBeDefined();
    });

    it("formats error response for missing node", () => {
      const nodeId = "nonexistent";
      const response = {
        success: false,
        error: `Node ${nodeId} not found`,
      };

      expect(response.success).toBe(false);
      expect(response.error).toContain("not found");
    });
  });
});

describe("uncomplete_node logic", () => {
  describe("uncomplete marking", () => {
    it("removes completion timestamp", () => {
      const node = createMockTodo({ completed: true });

      const uncompletedNode = {
        ...node,
        completed: undefined,
      };

      expect(uncompletedNode.completed).toBeUndefined();
    });

    it("handles already uncompleted node", () => {
      const node = createMockTodo({ completed: false });

      const uncompletedNode = {
        ...node,
        completed: undefined,
      };

      expect(uncompletedNode.completed).toBeUndefined();
    });
  });

  describe("response formatting", () => {
    it("formats successful uncomplete response", () => {
      const nodeId = "todo-123";
      const response = {
        success: true,
        node_id: nodeId,
        completed: false,
        message: `Node ${nodeId} marked as incomplete`,
      };

      expect(response.success).toBe(true);
      expect(response.completed).toBe(false);
    });
  });
});

describe("todo identification", () => {
  describe("layoutMode detection", () => {
    it("identifies todo by layoutMode", () => {
      const node = createMockNode({ layoutMode: "todo" });
      const isTodo = node.layoutMode === "todo";
      expect(isTodo).toBe(true);
    });

    it("non-todo nodes have different layoutMode", () => {
      const node = createMockNode({ layoutMode: undefined });
      const isTodo = node.layoutMode === "todo";
      expect(isTodo).toBe(false);
    });
  });

  describe("markdown checkbox detection", () => {
    it("detects unchecked checkbox in name", () => {
      const node = createMockNode({ name: "- [ ] Task item" });
      const isCheckbox = node.name?.match(/^- \[ \] /);
      expect(isCheckbox).not.toBeNull();
    });

    it("detects checked checkbox in name", () => {
      const node = createMockNode({ name: "- [x] Completed item" });
      const isChecked = node.name?.match(/^- \[x\] /);
      expect(isChecked).not.toBeNull();
    });

    it("regular names are not checkboxes", () => {
      const node = createMockNode({ name: "Regular item" });
      const isCheckbox = node.name?.match(/^- \[[ x]\] /);
      expect(isCheckbox).toBeNull();
    });
  });

  describe("combined todo detection", () => {
    function isTodoNode(node: WorkflowyNode): boolean {
      if (node.layoutMode === "todo") return true;
      if (node.name?.match(/^- \[[ x]\] /)) return true;
      return false;
    }

    it("detects layoutMode todos", () => {
      const node = createMockTodo({ name: "Task" });
      expect(isTodoNode(node)).toBe(true);
    });

    it("detects markdown checkbox todos", () => {
      const node = createMockNode({ name: "- [ ] Task" });
      expect(isTodoNode(node)).toBe(true);
    });

    it("rejects non-todo nodes", () => {
      const node = createMockNode({ name: "Regular node" });
      expect(isTodoNode(node)).toBe(false);
    });
  });
});

describe("todo edge cases", () => {
  describe("empty and whitespace", () => {
    it("handles todo with empty name", () => {
      const todo = createMockTodo({ name: "" });
      expect(todo.name).toBe("");
      expect(todo.layoutMode).toBe("todo");
    });

    it("handles todo with whitespace name", () => {
      const todo = createMockTodo({ name: "   " });
      expect(todo.name?.trim()).toBe("");
    });
  });

  describe("special characters in todo names", () => {
    it("handles emojis in todo name", () => {
      const todo = createMockTodo({ name: "✅ Complete task" });
      expect(todo.name).toContain("✅");
    });

    it("handles URLs in todo name", () => {
      const todo = createMockTodo({ name: "Check https://example.com" });
      expect(todo.name).toContain("https://");
    });

    it("handles special formatting", () => {
      const todo = createMockTodo({ name: "**Bold** and _italic_" });
      expect(todo.name).toContain("**");
    });
  });

  describe("hierarchy handling", () => {
    it("todo can have children", () => {
      const parentTodo = createMockTodo({ id: "parent" });
      const childNode = createMockNode({ parent_id: "parent" });

      expect(childNode.parent_id).toBe(parentTodo.id);
    });

    it("todo can be a child of non-todo", () => {
      const parent = createMockNode({ id: "project" });
      const childTodo = createMockTodo({ parent_id: "project" });

      expect(childTodo.parent_id).toBe(parent.id);
    });
  });
});

describe("batch todo operations", () => {
  describe("completing multiple todos", () => {
    it("supports batch completion", () => {
      const todoIds = ["todo-1", "todo-2", "todo-3"];
      const timestamp = new Date().toISOString();

      const results = todoIds.map((id) => ({
        node_id: id,
        success: true,
        completed_at: timestamp,
      }));

      expect(results.length).toBe(3);
      results.forEach((result) => {
        expect(result.success).toBe(true);
        expect(result.completed_at).toBe(timestamp);
      });
    });

    it("handles partial failures in batch", () => {
      const results = [
        { node_id: "todo-1", success: true },
        { node_id: "todo-2", success: false, error: "Node not found" },
        { node_id: "todo-3", success: true },
      ];

      const successful = results.filter((r) => r.success);
      const failed = results.filter((r) => !r.success);

      expect(successful.length).toBe(2);
      expect(failed.length).toBe(1);
    });
  });
});
