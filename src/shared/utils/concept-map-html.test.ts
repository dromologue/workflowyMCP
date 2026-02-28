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

  it("embeds node data as JSON with all concept labels", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("graphNodes");
    expect(html).toContain("Central Topic");
    expect(html).toContain("Major One");
    expect(html).toContain("Major Two");
    expect(html).toContain("Detail Alpha");
    expect(html).toContain("Detail Beta");
    expect(html).toContain("Detail Gamma");
  });

  it("embeds edge data as JSON", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("graphEdges");
    expect(html).toContain("supports");
    expect(html).toContain("extends");
    expect(html).toContain("requires");
    expect(html).toContain("contrasts with");
  });

  it("includes force simulation code", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("applyForces");
    expect(html).toContain("simulate");
    expect(html).toContain("activeNodes");
  });

  it("includes collapsible class for major nodes with children", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("collapsible");
    expect(html).toContain("expand-badge");
  });

  it("marks detail nodes with parent data in JSON", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("parentMajorId");
    expect(html).toContain('"m1"');
    expect(html).toContain('"m2"');
  });

  it("uses dashed stroke for contrast edges", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    // Edge data includes dashed:true for contrast relationships
    expect(html).toContain('"dashed":true');
  });

  it("handles empty relationships", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, []);
    expect(html).toContain("<!DOCTYPE html>");
    expect(html).toContain("Major One");
    // Should still have implicit parent links
    expect(html).toContain("isParentLink");
  });

  it("handles single concept (no majors, no details)", () => {
    const html = generateInteractiveConceptMapHTML("Minimal", coreNode, [], []);
    expect(html).toContain("Central Topic");
    expect(html).toContain("graphNodes");
  });

  it("escapes HTML in labels", () => {
    const xssCore = { id: "xss", label: '<script>alert("xss")</script>' };
    const html = generateInteractiveConceptMapHTML("Safe", xssCore, [], []);
    expect(html).not.toContain('<script>alert("xss")</script>');
    // The label is embedded in JSON, so angle brackets are escaped as unicode
    expect(html).toContain("\\u003c");
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

  it("includes tooltip element", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("tooltip");
  });

  it("includes drag handling", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("isDragging");
    expect(html).toContain("dragNode");
  });
});
