import { describe, it, expect } from "vitest";
import {
  generateInteractiveConceptMapHTML,
  type InteractiveConcept,
  type InteractiveRelationship,
} from "./concept-map-html.js";

describe("generateInteractiveConceptMapHTML", () => {
  const coreNode = { id: "core", label: "Central Topic" };

  const concepts: InteractiveConcept[] = [
    { id: "m1", label: "Major One", level: "major", importance: 7 },
    { id: "m2", label: "Major Two", level: "major", importance: 5 },
    { id: "d1", label: "Detail Alpha", level: "detail", importance: 3, parentMajorId: "m1" },
    { id: "d2", label: "Detail Beta", level: "detail", importance: 4, parentMajorId: "m1" },
    { id: "d3", label: "Detail Gamma", level: "detail", importance: 2, parentMajorId: "m2" },
  ];

  const relationships: InteractiveRelationship[] = [
    { from: "core", to: "m1", type: "relates to", strength: 8 },
    { from: "core", to: "m2", type: "supports", strength: 6 },
    { from: "m1", to: "d1", type: "extends", strength: 5 },
    { from: "m1", to: "d2", type: "requires", strength: 4 },
    { from: "m1", to: "m2", type: "contrasts with", strength: 3 },
  ];

  it("returns valid HTML with svg, style, and script", () => {
    const html = generateInteractiveConceptMapHTML("Test Map", coreNode, concepts, relationships);
    expect(html).toContain("<!DOCTYPE html>");
    expect(html).toContain("<svg");
    expect(html).toContain("<style>");
    expect(html).toContain("<script>");
    expect(html).toContain("</html>");
  });

  it("includes the title", () => {
    const html = generateInteractiveConceptMapHTML("My Concept Map", coreNode, concepts, relationships);
    expect(html).toContain("My Concept Map");
  });

  it("contains all concept labels", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("Central Topic");
    expect(html).toContain("Major One");
    expect(html).toContain("Major Two");
    expect(html).toContain("Detail Alpha");
    expect(html).toContain("Detail Beta");
    expect(html).toContain("Detail Gamma");
  });

  it("includes collapse-toggle classes for major concepts with children", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("collapsible");
    expect(html).toContain("collapse-indicator");
  });

  it("marks detail nodes with parent data attribute", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain('data-parent-major="m1"');
    expect(html).toContain('data-parent-major="m2"');
  });

  it("includes edge type labels for non-generic relationships", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("supports");
    expect(html).toContain("extends");
    expect(html).toContain("requires");
    expect(html).toContain("contrasts with");
  });

  it("uses dashed stroke for contrast edges", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain('stroke-dasharray="6,3"');
  });

  it("handles empty relationships", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, []);
    expect(html).toContain("<!DOCTYPE html>");
    expect(html).toContain("Major One");
    expect(html).not.toContain("<path");
  });

  it("handles single concept (no majors, no details)", () => {
    const html = generateInteractiveConceptMapHTML("Minimal", coreNode, [], []);
    expect(html).toContain("Central Topic");
    expect(html).toContain("<circle");
  });

  it("escapes HTML in labels", () => {
    const xssCore = { id: "xss", label: '<script>alert("xss")</script>' };
    const html = generateInteractiveConceptMapHTML("Safe", xssCore, [], []);
    expect(html).not.toContain('<script>alert("xss")</script>');
    expect(html).toContain("&lt;script&gt;");
  });

  it("includes zoom and pan controls", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("zoomGroup");
    expect(html).toContain("wheel");
    expect(html).toContain("resetView");
  });

  it("includes expand/collapse all buttons", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("expandAll");
    expect(html).toContain("collapseAll");
    expect(html).toContain("Expand All");
    expect(html).toContain("Collapse All");
  });

  it("includes legend", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("Core concept");
    expect(html).toContain("Major concept");
    expect(html).toContain("Detail concept");
  });
});
