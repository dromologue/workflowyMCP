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
    expect(html).toContain('"dashed":true');
  });

  it("handles empty relationships", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, []);
    expect(html).toContain("<!DOCTYPE html>");
    expect(html).toContain("Major One");
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

  it("mousedown excludes slider panel to prevent panning interference", () => {
    const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
    expect(html).toContain("e.target.closest('.slider-panel')");
  });

  // ── Full labels (no truncation) ──

  describe("full labels", () => {
    it("does not truncate long node labels", () => {
      const longConcepts: InteractiveConcept[] = [
        { id: "m1", label: "This Is A Very Long Major Concept Label That Would Be Truncated", level: "major", importance: 7 },
      ];
      const html = generateInteractiveConceptMapHTML("Test", coreNode, longConcepts, []);
      expect(html).toContain("This Is A Very Long Major Concept Label That Would Be Truncated");
    });

    it("does not truncate long edge type labels", () => {
      const rels: InteractiveRelationship[] = [
        { from: "core", to: "m1", type: "fundamentally contradicts and challenges", strength: 5 },
      ];
      const conceptsWithM1: InteractiveConcept[] = [
        { id: "m1", label: "Topic", level: "major", importance: 5 },
      ];
      const html = generateInteractiveConceptMapHTML("Test", coreNode, conceptsWithM1, rels);
      expect(html).toContain("fundamentally contradicts and challenges");
    });

    it("includes wrapLabel function for word wrapping", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("wrapLabel");
    });

    it("uses tspan elements for word-wrapped labels", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("createElementNS(NS, 'tspan')");
    });
  });

  // ── Physics sliders ──

  describe("physics sliders", () => {
    it("includes the slider panel HTML", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("slider-panel");
      expect(html).toContain("sliderPanel");
    });

    it("includes a Physics toggle button", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("physicsBtn");
      expect(html).toContain("togglePhysics");
      expect(html).toContain("Physics");
    });

    it("includes all five sliders", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("chargeSlider");
      expect(html).toContain("linkDistSlider");
      expect(html).toContain("gravitySlider");
      expect(html).toContain("dampingSlider");
      expect(html).toContain("overlapSlider");
    });

    it("uses configurable forceParams object", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("forceParams");
      expect(html).toContain("forceParams.charge");
      expect(html).toContain("forceParams.linkDist");
      expect(html).toContain("forceParams.gravity");
      expect(html).toContain("forceParams.damping");
      expect(html).toContain("forceParams.overlap");
    });

    it("includes setupSlider function for binding sliders to params", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("setupSlider");
    });

    it("slider panel is hidden by default", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain('id="sliderPanel"');
      expect(html).toMatch(/slider-panel[^}]*display:\s*none/);
    });
  });

  // ── Workflowy links & node popup ──

  describe("workflowy links and node popup", () => {
    it("embeds workflowyNodeId in node JSON when provided", () => {
      const wfConcepts: InteractiveConcept[] = [
        { id: "m1", label: "Topic", level: "major", importance: 5, workflowyNodeId: "abc-def-123-456-789abcdef" },
      ];
      const html = generateInteractiveConceptMapHTML("Test", coreNode, wfConcepts, []);
      expect(html).toContain('"workflowyNodeId":"abc-def-123-456-789abcdef"');
    });

    it("embeds core node workflowyNodeId when provided", () => {
      const wfCore = { id: "core", label: "Core", workflowyNodeId: "f81cd51b-2327-a719-08a4-87c129cf5f3e" };
      const html = generateInteractiveConceptMapHTML("Test", wfCore, [], []);
      expect(html).toContain('"workflowyNodeId":"f81cd51b-2327-a719-08a4-87c129cf5f3e"');
    });

    it("sets workflowyNodeId to null when not provided", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain('"workflowyNodeId":null');
    });

    it("includes workflowyUrl helper function", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("workflowyUrl");
      expect(html).toContain("workflowy.com/#/");
    });

    it("includes node popup HTML element", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("node-popup");
      expect(html).toContain("nodePopup");
    });

    it("includes showNodePopup function", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("showNodePopup");
      expect(html).toContain("hideNodePopup");
    });

    it("popup shows Open in Workflowy link when node has workflowyNodeId", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("Open in Workflowy");
      expect(html).toContain("wf-btn");
      expect(html).toContain("target=\"_blank\"");
    });

    it("popup shows inferred concept message when node has no workflowyNodeId", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("Inferred concept");
      expect(html).toContain("popup-inferred");
    });

    it("popup includes expand/collapse button for majors via toggleExpand", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("toggleExpand");
      expect(html).toContain("Expand");
    });

    it("includes workflowy indicator dot on nodes", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("wf-indicator");
    });

    it("backward compatible — works without workflowyNodeId", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("<!DOCTYPE html>");
      expect(html).toContain("Major One");
      expect(html).toContain("graphNodes");
    });

    it("click handler shows popup instead of directly expanding", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      // Click handler calls showNodePopup, not direct expand
      expect(html).toContain("showNodePopup(n,");
      expect(html).toContain("popupNodeId");
    });
  });

  // ── updatePositions ──

  describe("updatePositions", () => {
    it("updates tspan positions during animation", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("querySelectorAll('tspan')");
    });

    it("updates workflowy indicator dot positions", () => {
      const html = generateInteractiveConceptMapHTML("Test", coreNode, concepts, relationships);
      expect(html).toContain("querySelector('.wf-indicator')");
    });
  });
});
