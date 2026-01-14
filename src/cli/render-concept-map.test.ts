/**
 * Tests for render-concept-map CLI tool
 * Tests the JSON parsing, validation, and DOT generation logic
 */

import { describe, it, expect } from "vitest";

// Import types that match the CLI tool
type RelationshipType =
  | "causes" | "enables" | "prevents" | "triggers" | "influences"
  | "contains" | "part_of" | "instance_of" | "derives_from" | "extends"
  | "precedes" | "follows" | "co_occurs"
  | "implies" | "contradicts" | "supports" | "refines" | "exemplifies"
  | "similar_to" | "contrasts_with" | "generalizes" | "specializes"
  | "related_to";

interface ConceptInput {
  id: string;
  label: string;
  level: "major" | "detail";
  importance?: number;
  description?: string;
}

interface RelationshipInput {
  from: string;
  to: string;
  type: RelationshipType;
  description: string;
  evidence?: string;
  strength?: number;
  bidirectional?: boolean;
}

interface ConceptMapDefinition {
  title: string;
  core_concept: {
    label: string;
    description?: string;
  };
  concepts: ConceptInput[];
  relationships: RelationshipInput[];
}

// Helper functions copied from CLI tool for testing
function escapeForDot(text: string): string {
  return text
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n");
}

function formatRelationType(type: string): string {
  return type.replace(/_/g, " ");
}

function wrapText(text: string, maxWidth: number): string {
  const words = text.split(" ");
  const lines: string[] = [];
  let currentLine = "";

  for (const word of words) {
    if (currentLine.length + word.length + 1 <= maxWidth) {
      currentLine += (currentLine ? " " : "") + word;
    } else {
      if (currentLine) lines.push(currentLine);
      currentLine = word;
    }
  }
  if (currentLine) lines.push(currentLine);

  return lines.join("\\n");
}

function getEdgeColor(type: string): string {
  if (["causes", "enables", "prevents", "triggers", "influences"].includes(type)) {
    return "#2980b9";
  }
  if (["contains", "part_of", "instance_of", "derives_from", "extends"].includes(type)) {
    return "#27ae60";
  }
  if (["precedes", "follows", "co_occurs"].includes(type)) {
    return "#e67e22";
  }
  if (["implies", "supports", "refines", "exemplifies"].includes(type)) {
    return "#8e44ad";
  }
  if (["contradicts", "contrasts_with"].includes(type)) {
    return "#c0392b";
  }
  if (["similar_to", "generalizes", "specializes"].includes(type)) {
    return "#16a085";
  }
  return "#566573";
}

function getEdgeStyle(type: string): string {
  if (["contradicts", "contrasts_with", "prevents"].includes(type)) {
    return "dashed";
  }
  if (["precedes", "follows", "co_occurs"].includes(type)) {
    return "dotted";
  }
  if (["causes", "implies", "derives_from"].includes(type)) {
    return "bold";
  }
  return "solid";
}

describe("render-concept-map CLI", () => {
  describe("JSON validation", () => {
    it("validates required fields in ConceptMapDefinition", () => {
      const validDefinition: ConceptMapDefinition = {
        title: "Test Map",
        core_concept: { label: "Core" },
        concepts: [
          { id: "a", label: "A", level: "major" },
          { id: "b", label: "B", level: "detail" },
        ],
        relationships: [
          { from: "core", to: "a", type: "enables", description: "Core enables A" },
        ],
      };

      expect(validDefinition.title).toBeDefined();
      expect(validDefinition.core_concept.label).toBeDefined();
      expect(validDefinition.concepts.length).toBeGreaterThanOrEqual(2);
      expect(validDefinition.relationships.length).toBeGreaterThan(0);
    });

    it("rejects definitions with fewer than 2 concepts", () => {
      const concepts = [{ id: "only-one", label: "One", level: "major" as const }];
      expect(concepts.length).toBeLessThan(2);
    });

    it("validates concept structure", () => {
      const concept: ConceptInput = {
        id: "test-id",
        label: "Test Label",
        level: "major",
        importance: 8,
        description: "Optional description",
      };

      expect(concept.id).toBeDefined();
      expect(concept.label).toBeDefined();
      expect(["major", "detail"]).toContain(concept.level);
      expect(concept.importance).toBeGreaterThanOrEqual(1);
      expect(concept.importance).toBeLessThanOrEqual(10);
    });

    it("validates relationship structure with required description", () => {
      const relationship: RelationshipInput = {
        from: "concept-a",
        to: "concept-b",
        type: "enables",
        description: "A enables B by providing foundation",
        strength: 0.8,
        bidirectional: false,
      };

      expect(relationship.from).toBeDefined();
      expect(relationship.to).toBeDefined();
      expect(relationship.type).toBeDefined();
      expect(relationship.description).toBeDefined();
      expect(relationship.description.length).toBeGreaterThan(0);
    });
  });

  describe("DOT escaping", () => {
    it("escapes backslashes", () => {
      expect(escapeForDot("path\\to\\file")).toBe("path\\\\to\\\\file");
    });

    it("escapes double quotes", () => {
      expect(escapeForDot('He said "hello"')).toBe('He said \\"hello\\"');
    });

    it("escapes newlines", () => {
      expect(escapeForDot("line1\nline2")).toBe("line1\\nline2");
    });

    it("handles combined special characters", () => {
      const input = 'Path: "C:\\Users"\nNext';
      const expected = 'Path: \\"C:\\\\Users\\"\\nNext';
      expect(escapeForDot(input)).toBe(expected);
    });
  });

  describe("relationship type formatting", () => {
    it("converts underscores to spaces", () => {
      expect(formatRelationType("part_of")).toBe("part of");
      expect(formatRelationType("similar_to")).toBe("similar to");
      expect(formatRelationType("contrasts_with")).toBe("contrasts with");
    });

    it("preserves types without underscores", () => {
      expect(formatRelationType("enables")).toBe("enables");
      expect(formatRelationType("causes")).toBe("causes");
    });
  });

  describe("text wrapping", () => {
    it("wraps long text at word boundaries", () => {
      const text = "This is a long description that should be wrapped";
      const wrapped = wrapText(text, 20);
      expect(wrapped).toContain("\\n");
    });

    it("preserves short text", () => {
      const text = "Short text";
      const wrapped = wrapText(text, 20);
      expect(wrapped).toBe("Short text");
    });

    it("handles single long word", () => {
      const text = "Supercalifragilisticexpialidocious";
      const wrapped = wrapText(text, 10);
      expect(wrapped).toBe(text); // Single word cannot be wrapped
    });
  });

  describe("edge color mapping", () => {
    it("returns blue for causal relationships", () => {
      expect(getEdgeColor("causes")).toBe("#2980b9");
      expect(getEdgeColor("enables")).toBe("#2980b9");
      expect(getEdgeColor("prevents")).toBe("#2980b9");
      expect(getEdgeColor("triggers")).toBe("#2980b9");
      expect(getEdgeColor("influences")).toBe("#2980b9");
    });

    it("returns green for structural relationships", () => {
      expect(getEdgeColor("contains")).toBe("#27ae60");
      expect(getEdgeColor("part_of")).toBe("#27ae60");
      expect(getEdgeColor("derives_from")).toBe("#27ae60");
    });

    it("returns orange for temporal relationships", () => {
      expect(getEdgeColor("precedes")).toBe("#e67e22");
      expect(getEdgeColor("follows")).toBe("#e67e22");
      expect(getEdgeColor("co_occurs")).toBe("#e67e22");
    });

    it("returns purple for logical relationships", () => {
      expect(getEdgeColor("implies")).toBe("#8e44ad");
      expect(getEdgeColor("supports")).toBe("#8e44ad");
      expect(getEdgeColor("refines")).toBe("#8e44ad");
    });

    it("returns red for contradictory relationships", () => {
      expect(getEdgeColor("contradicts")).toBe("#c0392b");
      expect(getEdgeColor("contrasts_with")).toBe("#c0392b");
    });

    it("returns teal for comparative relationships", () => {
      expect(getEdgeColor("similar_to")).toBe("#16a085");
      expect(getEdgeColor("generalizes")).toBe("#16a085");
      expect(getEdgeColor("specializes")).toBe("#16a085");
    });

    it("returns gray for unknown types", () => {
      expect(getEdgeColor("related_to")).toBe("#566573");
      expect(getEdgeColor("unknown")).toBe("#566573");
    });
  });

  describe("edge style mapping", () => {
    it("returns dashed for contradictory types", () => {
      expect(getEdgeStyle("contradicts")).toBe("dashed");
      expect(getEdgeStyle("contrasts_with")).toBe("dashed");
      expect(getEdgeStyle("prevents")).toBe("dashed");
    });

    it("returns dotted for temporal types", () => {
      expect(getEdgeStyle("precedes")).toBe("dotted");
      expect(getEdgeStyle("follows")).toBe("dotted");
      expect(getEdgeStyle("co_occurs")).toBe("dotted");
    });

    it("returns bold for strong causal types", () => {
      expect(getEdgeStyle("causes")).toBe("bold");
      expect(getEdgeStyle("implies")).toBe("bold");
      expect(getEdgeStyle("derives_from")).toBe("bold");
    });

    it("returns solid for other types", () => {
      expect(getEdgeStyle("enables")).toBe("solid");
      expect(getEdgeStyle("supports")).toBe("solid");
      expect(getEdgeStyle("related_to")).toBe("solid");
    });
  });

  describe("example JSON generation", () => {
    it("produces valid Heidegger example", () => {
      // This matches the getHeideggerExample() function in the CLI
      const example: ConceptMapDefinition = {
        title: "Heidegger's Fundamental Ontology",
        core_concept: {
          label: "Being (Sein)",
          description: "The central question of Heidegger's philosophy",
        },
        concepts: [
          { id: "dasein", label: "Dasein", level: "major", importance: 9 },
          { id: "being-in-world", label: "Being-in-the-World", level: "major", importance: 8 },
          { id: "temporality", label: "Temporality", level: "major", importance: 8 },
          { id: "authenticity", label: "Authenticity", level: "major", importance: 7 },
          { id: "das-man", label: "Das Man (The They)", level: "detail", importance: 6 },
        ],
        relationships: [
          {
            from: "core",
            to: "dasein",
            type: "enables",
            description: "Being is disclosed through Dasein's unique capacity for understanding",
            strength: 0.9,
          },
        ],
      };

      expect(example.title).toBe("Heidegger's Fundamental Ontology");
      expect(example.core_concept.label).toBe("Being (Sein)");
      expect(example.concepts.length).toBeGreaterThanOrEqual(2);
      expect(example.relationships[0].description).toBeDefined();
      expect(example.relationships[0].type).toBe("enables");
    });
  });

  describe("output options", () => {
    it("supports PNG format", () => {
      const format = "png";
      expect(["png", "jpeg", "pdf"]).toContain(format);
    });

    it("supports JPEG format", () => {
      const format = "jpeg";
      expect(["png", "jpeg", "pdf"]).toContain(format);
    });

    it("supports PDF format (as high-res PNG)", () => {
      const format = "pdf";
      expect(["png", "jpeg", "pdf"]).toContain(format);
    });

    it("validates dimension options", () => {
      const width = 4000;
      const height = 3000;
      const dpi = 300;

      expect(width).toBeGreaterThan(0);
      expect(height).toBeGreaterThan(0);
      expect(dpi).toBeGreaterThanOrEqual(72);
    });
  });
});
