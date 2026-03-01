/**
 * Interactive concept map HTML generator.
 * Produces a self-contained HTML string with SVG + vanilla JS force-directed layout.
 * Full labels (word-wrapped, no truncation), physics sliders, Workflowy node links.
 * Click any node to see details popup with Workflowy link or "inferred concept" indicator.
 * Detail nodes are hidden by default — click a major concept's expand badge to show children.
 */

export interface InteractiveConcept {
  id: string;
  label: string;
  level: "major" | "detail";
  importance: number;
  parentMajorId?: string;
  workflowyNodeId?: string;
}

export interface InteractiveRelationship {
  from: string;
  to: string;
  type: string;
  strength: number;
}

function escapeHtml(str: string): string {
  return str.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function escapeJsonString(str: string): string {
  return str.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/</g, "\\u003c");
}

function getEdgeColor(type: string): string {
  const t = type.toLowerCase();
  if (t.includes("contrast") || t.includes("oppose")) return "#c0392b";
  if (t.includes("support") || t.includes("extend")) return "#27ae60";
  if (t.includes("require") || t.includes("depend")) return "#8e44ad";
  return "#566573";
}

// Color palettes
const CORE_COLOR = "#1a5276";
const MAJOR_COLORS = ["#2874a6", "#1e8449", "#b9770e", "#6c3483", "#1abc9c"];
const DETAIL_COLORS = ["#5dade2", "#58d68d", "#f4d03f", "#bb8fce", "#76d7c4"];

/**
 * Generate a self-contained interactive HTML concept map with force-directed layout.
 */
export interface ConceptMapHTMLOptions {
  /** Show the legend panel (default: true) */
  showLegend?: boolean;
}

export function generateInteractiveConceptMapHTML(
  title: string,
  coreNode: { id: string; label: string; workflowyNodeId?: string },
  concepts: InteractiveConcept[],
  relationships: InteractiveRelationship[],
  options?: ConceptMapHTMLOptions,
): string {
  const majors = concepts.filter((c) => c.level === "major");
  const details = concepts.filter((c) => c.level === "detail");

  // Build node data for the browser
  interface NodeData {
    id: string;
    label: string;
    level: string;
    importance: number;
    parentMajorId: string | null;
    color: string;
    textColor: string;
    radius: number;
    workflowyNodeId: string | null;
  }

  const nodes: NodeData[] = [];

  // Core
  nodes.push({
    id: coreNode.id,
    label: coreNode.label,
    level: "core",
    importance: 10,
    parentMajorId: null,
    color: CORE_COLOR,
    textColor: "#fff",
    radius: 40,
    workflowyNodeId: coreNode.workflowyNodeId || null,
  });

  // Majors
  majors.forEach((m, i) => {
    nodes.push({
      id: m.id,
      label: m.label,
      level: "major",
      importance: m.importance || 5,
      parentMajorId: null,
      color: MAJOR_COLORS[i % MAJOR_COLORS.length],
      textColor: "#fff",
      radius: Math.max(28, Math.min(28 + (m.importance || 5) * 1.5, 42)),
      workflowyNodeId: m.workflowyNodeId || null,
    });
  });

  // Details
  details.forEach((d, i) => {
    const pid = d.parentMajorId || majors[0]?.id || coreNode.id;
    nodes.push({
      id: d.id,
      label: d.label,
      level: "detail",
      importance: d.importance || 3,
      parentMajorId: pid,
      color: DETAIL_COLORS[i % DETAIL_COLORS.length],
      textColor: "#1a1a1a",
      radius: Math.max(20, Math.min(20 + (d.importance || 3) * 1.2, 32)),
      workflowyNodeId: d.workflowyNodeId || null,
    });
  });

  // Build edge data — explicit relationships + implicit parent-child links
  interface EdgeData {
    from: string;
    to: string;
    type: string;
    strength: number;
    color: string;
    dashed: boolean;
    isParentLink: boolean;
  }

  const edges: EdgeData[] = [];
  const addedEdges = new Set<string>();

  // Explicit relationships
  for (const rel of relationships) {
    const key = [rel.from, rel.to].sort().join("|||");
    if (addedEdges.has(key)) continue;
    addedEdges.add(key);
    const fromExists = nodes.some((n) => n.id === rel.from);
    const toExists = nodes.some((n) => n.id === rel.to);
    if (!fromExists || !toExists) continue;
    edges.push({
      from: rel.from,
      to: rel.to,
      type: rel.type,
      strength: rel.strength || 5,
      color: getEdgeColor(rel.type),
      dashed: rel.type.toLowerCase().includes("contrast") || rel.type.toLowerCase().includes("oppose"),
      isParentLink: false,
    });
  }

  // Implicit parent links (detail → parent major) if not already present
  for (const d of details) {
    const pid = d.parentMajorId || majors[0]?.id || coreNode.id;
    const key = [d.id, pid].sort().join("|||");
    if (!addedEdges.has(key)) {
      addedEdges.add(key);
      edges.push({
        from: pid,
        to: d.id,
        type: "",
        strength: 3,
        color: "#ccc",
        dashed: false,
        isParentLink: true,
      });
    }
  }

  // Core → major links if not already present
  for (const m of majors) {
    const key = [coreNode.id, m.id].sort().join("|||");
    if (!addedEdges.has(key)) {
      addedEdges.add(key);
      edges.push({
        from: coreNode.id,
        to: m.id,
        type: "",
        strength: 4,
        color: "#aaa",
        dashed: false,
        isParentLink: true,
      });
    }
  }

  // Serialize to JSON for embedding — escape < to prevent XSS in <script> context
  const safeJson = (obj: unknown) => JSON.stringify(obj).replace(/</g, "\\u003c");
  const nodesJson = safeJson(nodes);
  const edgesJson = safeJson(edges);

  return `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<title>${escapeHtml(title)}</title>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
body { background: #f8f9fa; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; overflow: hidden; }
.container { width: 100%; height: 100vh; display: flex; flex-direction: column; }
.title-bar { padding: 10px 16px; background: #fff; border-bottom: 1px solid #e0e0e0; display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: 8px; }
.title-bar h1 { font-size: 16px; color: #1a1a1a; font-weight: 600; }
.controls { display: flex; gap: 8px; align-items: center; }
.controls button { padding: 4px 10px; border: 1px solid #ccc; border-radius: 4px; background: #fff; cursor: pointer; font-size: 12px; }
.controls button:hover { background: #f0f0f0; }
.controls button.active { background: #e8f0fe; border-color: #4285f4; color: #1a73e8; }
.slider-panel {
  position: absolute; top: 46px; right: 10px; background: #fff; border: 1px solid #ddd;
  border-radius: 8px; padding: 14px 18px; box-shadow: 0 4px 12px rgba(0,0,0,0.12);
  z-index: 60; display: none; min-width: 260px;
}
.slider-panel h3 { font-size: 12px; font-weight: 600; color: #555; margin-bottom: 10px; text-transform: uppercase; letter-spacing: 0.5px; }
.slider-group { margin-bottom: 10px; }
.slider-group:last-child { margin-bottom: 0; }
.slider-group label { display: flex; justify-content: space-between; font-size: 12px; color: #333; margin-bottom: 3px; }
.slider-group .val { color: #888; font-variant-numeric: tabular-nums; min-width: 40px; text-align: right; }
.slider-group input[type="range"] { width: 100%; height: 6px; -webkit-appearance: none; appearance: none; background: #e0e0e0; border-radius: 3px; outline: none; }
.slider-group input[type="range"]::-webkit-slider-thumb { -webkit-appearance: none; width: 14px; height: 14px; border-radius: 50%; background: #4285f4; cursor: pointer; border: 2px solid #fff; box-shadow: 0 1px 3px rgba(0,0,0,0.3); }
.node-popup {
  position: absolute; background: #fff; border: 1px solid #ddd; border-radius: 8px;
  padding: 12px 16px; box-shadow: 0 4px 16px rgba(0,0,0,0.18); z-index: 80;
  max-width: 320px; display: none; font-size: 13px; line-height: 1.5;
}
.node-popup .popup-label { font-weight: 600; color: #1a1a1a; margin-bottom: 6px; font-size: 14px; }
.node-popup .popup-level { font-size: 11px; color: #888; text-transform: uppercase; letter-spacing: 0.5px; margin-bottom: 8px; }
.node-popup .popup-actions { display: flex; flex-direction: column; gap: 6px; }
.node-popup .popup-btn {
  display: inline-flex; align-items: center; gap: 6px; padding: 6px 12px;
  border: 1px solid #ddd; border-radius: 5px; background: #fff; cursor: pointer;
  font-size: 12px; color: #333; text-decoration: none; transition: background 0.15s;
}
.node-popup .popup-btn:hover { background: #f0f4ff; border-color: #4285f4; }
.node-popup .popup-btn.wf-btn { color: #1a73e8; border-color: #a8c7fa; }
.node-popup .popup-btn.wf-btn:hover { background: #e8f0fe; }
.node-popup .popup-inferred {
  font-size: 11px; color: #999; font-style: italic; padding: 4px 0;
}
.svg-container { flex: 1; overflow: hidden; cursor: grab; position: relative; }
.svg-container:active { cursor: grabbing; }
svg { width: 100%; height: 100%; }
.node-group { cursor: pointer; }
.node-group:hover circle:first-child { filter: brightness(1.15); stroke-width: 3; }
.node-group.dragging { cursor: grabbing; }
.node-label { pointer-events: none; user-select: none; }
.edge-group { pointer-events: none; }
.edge-label { font-size: 9px; pointer-events: none; user-select: none; }
.detail-hidden { display: none; }
.collapsible .expand-badge { cursor: pointer; }
.tooltip {
  position: absolute; padding: 8px 12px; background: rgba(0,0,0,0.88); color: #fff;
  border-radius: 6px; font-size: 12px; pointer-events: none; max-width: 300px;
  display: none; z-index: 100; line-height: 1.4;
}
.legend { position: absolute; bottom: 10px; left: 10px; background: rgba(255,255,255,0.92); border: 1px solid #ddd; border-radius: 6px; padding: 8px 12px; font-size: 11px; }
.legend-item { display: flex; align-items: center; gap: 6px; margin: 3px 0; }
.legend-dot { width: 10px; height: 10px; border-radius: 50%; }
.legend-line { width: 20px; height: 2px; }
</style>
</head>
<body>
<div class="container">
  <div class="title-bar">
    <h1>${escapeHtml(title)}</h1>
    <div class="controls">
      <button onclick="resetView()">Reset</button>
      <button onclick="expandAll()">Expand All</button>
      <button onclick="collapseAll()">Collapse All</button>
      <button id="physicsBtn" onclick="togglePhysics()">Physics</button>
    </div>
  </div>
  <div class="svg-container" id="svgContainer">
    <svg id="mapSvg" xmlns="http://www.w3.org/2000/svg">
      <g id="zoomGroup">
        <g id="edgeLayer"></g>
        <g id="nodeLayer"></g>
      </g>
    </svg>
    <div class="slider-panel" id="sliderPanel">
      <h3>Force Parameters</h3>
      <div class="slider-group">
        <label>Charge (repulsion) <span class="val" id="chargeVal">800</span></label>
        <input type="range" id="chargeSlider" min="100" max="3000" value="800" step="50">
      </div>
      <div class="slider-group">
        <label>Link Distance <span class="val" id="linkDistVal">200</span></label>
        <input type="range" id="linkDistSlider" min="50" max="600" value="200" step="10">
      </div>
      <div class="slider-group">
        <label>Center Gravity <span class="val" id="gravityVal">0.003</span></label>
        <input type="range" id="gravitySlider" min="1" max="30" value="3" step="1">
      </div>
      <div class="slider-group">
        <label>Damping <span class="val" id="dampingVal">0.60</span></label>
        <input type="range" id="dampingSlider" min="10" max="95" value="60" step="1">
      </div>
      <div class="slider-group">
        <label>Overlap Repulsion <span class="val" id="overlapVal">0.8</span></label>
        <input type="range" id="overlapSlider" min="0" max="30" value="8" step="1">
      </div>
    </div>
    <div class="node-popup" id="nodePopup"></div>
    <div class="tooltip" id="tooltip"></div>
    ${(options?.showLegend ?? true) ? `<div class="legend">
      <div class="legend-item"><div class="legend-dot" style="background:${CORE_COLOR}"></div> Core concept</div>
      <div class="legend-item"><div class="legend-dot" style="background:${MAJOR_COLORS[0]}"></div> Major concept (click to expand)</div>
      <div class="legend-item"><div class="legend-dot" style="background:${DETAIL_COLORS[0]}"></div> Detail concept</div>
      <div class="legend-item"><div class="legend-line" style="background:#566573"></div> Relationship</div>
    </div>` : ""}
  </div>
</div>
<script>
(function() {
  // ── Data ──
  var graphNodes = ${nodesJson};
  var graphEdges = ${edgesJson};

  // ── Configurable force parameters ──
  var forceParams = {
    charge: 800,
    linkDist: 200,
    gravity: 0.003,
    damping: 0.6,
    overlap: 0.8
  };

  // ── State ──
  var expanded = {};   // majorId → true/false
  var activeNodes = []; // currently visible nodes
  var activeEdges = []; // currently visible edges
  var scale = 1, panX = 0, panY = 0;
  var isDragging = false, dragNode = null, dragOffsetX = 0, dragOffsetY = 0;
  var isPanning = false, panStartX = 0, panStartY = 0;
  var width = 0, height = 0;
  var animFrame = null;
  var popupNodeId = null; // currently shown popup node

  // ── DOM refs ──
  var svg = document.getElementById('mapSvg');
  var zoomGroup = document.getElementById('zoomGroup');
  var edgeLayer = document.getElementById('edgeLayer');
  var nodeLayer = document.getElementById('nodeLayer');
  var container = document.getElementById('svgContainer');
  var tooltip = document.getElementById('tooltip');
  var nodePopup = document.getElementById('nodePopup');

  function resize() {
    width = container.clientWidth;
    height = container.clientHeight;
    svg.setAttribute('viewBox', '0 0 ' + width + ' ' + height);
  }
  resize();
  window.addEventListener('resize', resize);

  // ── Word-wrap helper ──
  function wrapLabel(label, maxCharsPerLine) {
    if (label.length <= maxCharsPerLine) return [label];
    var words = label.split(/\\s+/);
    var lines = [];
    var current = '';
    for (var i = 0; i < words.length; i++) {
      var test = current ? current + ' ' + words[i] : words[i];
      if (test.length > maxCharsPerLine && current) {
        lines.push(current);
        current = words[i];
      } else {
        current = test;
      }
    }
    if (current) lines.push(current);
    return lines;
  }

  // ── Workflowy URL helper ──
  function workflowyUrl(nodeId) {
    if (!nodeId) return null;
    var parts = nodeId.split('-');
    return 'https://workflowy.com/#/' + parts[parts.length - 1];
  }

  // ── Node popup ──
  function showNodePopup(n, clientX, clientY) {
    popupNodeId = n.id;
    var html = '<div class="popup-label">' + escapeStr(n.label) + '</div>';
    html += '<div class="popup-level">' + n.level + ' concept</div>';
    html += '<div class="popup-actions">';

    if (n.workflowyNodeId) {
      var url = workflowyUrl(n.workflowyNodeId);
      html += '<a class="popup-btn wf-btn" href="' + url + '" target="_blank">Open in Workflowy \\u2197</a>';
    } else {
      html += '<div class="popup-inferred">Inferred concept — no direct Workflowy link</div>';
    }

    if (n.level === 'major') {
      var hasChildren = graphNodes.some(function(d) { return d.level === 'detail' && d.parentMajorId === n.id; });
      if (hasChildren) {
        var action = expanded[n.id] ? 'Collapse' : 'Expand';
        html += '<button class="popup-btn" onclick="toggleExpand(\\'' + n.id + '\\')">' + action + ' details</button>';
      }
    }

    html += '</div>';
    nodePopup.innerHTML = html;
    nodePopup.style.display = 'block';

    // Position popup near click, but keep it in viewport
    var popW = 280, popH = 120;
    var left = clientX + 16;
    var top = clientY - 10;
    if (left + popW > window.innerWidth) left = clientX - popW - 16;
    if (top + popH > window.innerHeight) top = window.innerHeight - popH - 10;
    if (top < 10) top = 10;
    nodePopup.style.left = left + 'px';
    nodePopup.style.top = top + 'px';
  }

  function hideNodePopup() {
    nodePopup.style.display = 'none';
    popupNodeId = null;
  }

  function escapeStr(s) {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  window.toggleExpand = function(majorId) {
    expanded[majorId] = !expanded[majorId];
    if (expanded[majorId]) {
      var parent = nodeById(majorId);
      graphNodes.forEach(function(n) {
        if (n.level === 'detail' && n.parentMajorId === majorId) {
          n.x = parent.x + (Math.random() - 0.5) * 60;
          n.y = parent.y + (Math.random() - 0.5) * 60;
          n.vx = 0; n.vy = 0;
        }
      });
    }
    hideNodePopup();
    rebuildActive();
    render();
    runSimulation(150);
  };

  // ── Initialize node positions ──
  var cx = width / 2, cy = height / 2;
  graphNodes.forEach(function(n, i) {
    if (n.level === 'core') {
      n.x = cx; n.y = cy; n.fx = cx; n.fy = cy;
    } else if (n.level === 'major') {
      var majors = graphNodes.filter(function(m) { return m.level === 'major'; });
      var idx = majors.indexOf(n);
      var angle = (2 * Math.PI * idx) / Math.max(majors.length, 1) - Math.PI / 2;
      var r = Math.min(width, height) * 0.35;
      n.x = cx + r * Math.cos(angle);
      n.y = cy + r * Math.sin(angle);
    } else {
      var parent = graphNodes.find(function(p) { return p.id === n.parentMajorId; });
      if (parent) {
        n.x = (parent.x || cx) + (Math.random() - 0.5) * 80;
        n.y = (parent.y || cy) + (Math.random() - 0.5) * 80;
      } else {
        n.x = cx + (Math.random() - 0.5) * 200;
        n.y = cy + (Math.random() - 0.5) * 200;
      }
    }
    n.vx = 0; n.vy = 0;
  });

  // ── Force simulation ──
  function simulate(iterations) {
    var alpha = 1.0;
    var decay = 1 - Math.pow(0.001, 1 / iterations);
    for (var iter = 0; iter < iterations; iter++) {
      alpha *= (1 - decay);
      if (alpha < 0.001) break;
      applyForces(alpha);
    }
  }

  function applyForces(alpha) {
    var nodes = activeNodes;
    var edges = activeEdges;
    var i, j, n1, n2, dx, dy, dist, force, ex, ey;

    // Charge repulsion (all pairs)
    for (i = 0; i < nodes.length; i++) {
      for (j = i + 1; j < nodes.length; j++) {
        n1 = nodes[i]; n2 = nodes[j];
        dx = n2.x - n1.x; dy = n2.y - n1.y;
        dist = Math.sqrt(dx * dx + dy * dy) || 1;
        var minDist = n1.radius + n2.radius + 80;
        force = -forceParams.charge * alpha / (dist * dist);
        if (dist < minDist) force -= (minDist - dist) * forceParams.overlap * alpha;
        ex = dx / dist * force; ey = dy / dist * force;
        if (!n1.fx) { n1.vx += ex; n1.vy += ey; }
        if (!n2.fx) { n2.vx -= ex; n2.vy -= ey; }
      }
    }

    // Link attraction
    for (i = 0; i < edges.length; i++) {
      var edge = edges[i];
      n1 = nodeById(edge.from); n2 = nodeById(edge.to);
      if (!n1 || !n2) continue;
      dx = n2.x - n1.x; dy = n2.y - n1.y;
      dist = Math.sqrt(dx * dx + dy * dy) || 1;
      var idealDist = edge.isParentLink ? forceParams.linkDist : forceParams.linkDist * 1.5;
      force = (dist - idealDist) * 0.015 * alpha;
      ex = dx / dist * force; ey = dy / dist * force;
      if (!n1.fx) { n1.vx += ex; n1.vy += ey; }
      if (!n2.fx) { n2.vx -= ex; n2.vy -= ey; }
    }

    // Center gravity
    for (i = 0; i < nodes.length; i++) {
      n1 = nodes[i];
      if (n1.fx) continue;
      n1.vx += (cx - n1.x) * forceParams.gravity * alpha;
      n1.vy += (cy - n1.y) * forceParams.gravity * alpha;
    }

    // Apply velocities with damping
    for (i = 0; i < nodes.length; i++) {
      n1 = nodes[i];
      if (n1.fx) { n1.x = n1.fx; n1.y = n1.fy; continue; }
      n1.vx *= forceParams.damping; n1.vy *= forceParams.damping;
      n1.x += n1.vx; n1.y += n1.vy;
      var pad = n1.radius + 20;
      if (n1.x < pad) n1.x = pad;
      if (n1.x > width - pad) n1.x = width - pad;
      if (n1.y < pad) n1.y = pad;
      if (n1.y > height - pad) n1.y = height - pad;
    }
  }

  function nodeById(id) {
    return graphNodes.find(function(n) { return n.id === id; });
  }

  // ── Rebuild active sets ──
  function rebuildActive() {
    activeNodes = graphNodes.filter(function(n) {
      if (n.level === 'core' || n.level === 'major') return true;
      return n.parentMajorId && expanded[n.parentMajorId];
    });
    activeEdges = graphEdges.filter(function(e) {
      var fromNode = nodeById(e.from);
      var toNode = nodeById(e.to);
      if (!fromNode || !toNode) return false;
      return activeNodes.indexOf(fromNode) >= 0 && activeNodes.indexOf(toNode) >= 0;
    });
  }

  // ── Rendering ──
  var NS = 'http://www.w3.org/2000/svg';

  function render() {
    edgeLayer.innerHTML = '';
    nodeLayer.innerHTML = '';

    // Draw edges
    activeEdges.forEach(function(e) {
      var n1 = nodeById(e.from), n2 = nodeById(e.to);
      if (!n1 || !n2) return;
      var g = document.createElementNS(NS, 'g');
      g.setAttribute('class', 'edge-group');

      var mx = (n1.x + n2.x) / 2;
      var my = (n1.y + n2.y) / 2;
      var dx = n2.x - n1.x, dy = n2.y - n1.y;
      var ctrlX = mx - dy * 0.12, ctrlY = my + dx * 0.12;

      var path = document.createElementNS(NS, 'path');
      path.setAttribute('d', 'M' + n1.x + ',' + n1.y + ' Q' + ctrlX + ',' + ctrlY + ' ' + n2.x + ',' + n2.y);
      path.setAttribute('fill', 'none');
      path.setAttribute('stroke', e.color);
      path.setAttribute('stroke-width', String(Math.max(1, Math.min(1 + e.strength * 0.3, 4))));
      if (e.dashed) path.setAttribute('stroke-dasharray', '6,3');
      path.setAttribute('opacity', e.isParentLink ? '0.3' : '0.6');
      g.appendChild(path);

      // Full edge label — no truncation
      if (e.type && !e.isParentLink) {
        var label = document.createElementNS(NS, 'text');
        label.setAttribute('x', String(ctrlX));
        label.setAttribute('y', String(ctrlY));
        label.setAttribute('text-anchor', 'middle');
        label.setAttribute('class', 'edge-label');
        label.setAttribute('fill', e.color);
        label.setAttribute('opacity', '0.8');
        label.textContent = e.type;
        g.appendChild(label);
      }
      edgeLayer.appendChild(g);
    });

    // Draw nodes
    activeNodes.forEach(function(n) {
      var g = document.createElementNS(NS, 'g');
      g.setAttribute('class', 'node-group');
      g.setAttribute('data-id', n.id);
      g.setAttribute('data-level', n.level);
      if (n.parentMajorId) g.setAttribute('data-parent-major', n.parentMajorId);

      var circle = document.createElementNS(NS, 'circle');
      circle.setAttribute('cx', String(n.x));
      circle.setAttribute('cy', String(n.y));
      circle.setAttribute('r', String(n.radius));
      circle.setAttribute('fill', n.color);
      circle.setAttribute('stroke', n.level === 'core' ? '#0d2f3e' : '#fff');
      circle.setAttribute('stroke-width', n.level === 'core' ? '3' : '2');
      g.appendChild(circle);

      // Full label with word wrapping via tspan elements
      var text = document.createElementNS(NS, 'text');
      text.setAttribute('text-anchor', 'middle');
      text.setAttribute('class', 'node-label');
      text.setAttribute('fill', n.textColor);
      var fontSize = n.level === 'core' ? 14 : n.level === 'major' ? 11 : 9;
      text.setAttribute('font-size', String(fontSize));
      if (n.level !== 'detail') text.setAttribute('font-weight', 'bold');

      var maxCharsPerLine = n.level === 'core' ? 20 : n.level === 'major' ? 18 : 16;
      var lines = wrapLabel(n.label, maxCharsPerLine);
      var lineHeight = fontSize * 1.3;
      var startY = n.y - ((lines.length - 1) * lineHeight) / 2;

      for (var li = 0; li < lines.length; li++) {
        var tspan = document.createElementNS(NS, 'tspan');
        tspan.setAttribute('x', String(n.x));
        tspan.setAttribute('y', String(startY + li * lineHeight));
        tspan.textContent = lines[li];
        text.appendChild(tspan);
      }
      g.appendChild(text);

      // Expand badge for majors with children
      if (n.level === 'major') {
        var hasChildren = graphNodes.some(function(d) { return d.level === 'detail' && d.parentMajorId === n.id; });
        if (hasChildren) {
          g.setAttribute('class', 'node-group collapsible');
          var badge = document.createElementNS(NS, 'circle');
          badge.setAttribute('cx', String(n.x + n.radius * 0.7));
          badge.setAttribute('cy', String(n.y - n.radius * 0.7));
          badge.setAttribute('r', '8');
          badge.setAttribute('fill', expanded[n.id] ? '#e74c3c' : '#2ecc71');
          badge.setAttribute('stroke', '#fff');
          badge.setAttribute('stroke-width', '1.5');
          badge.setAttribute('class', 'expand-badge');
          g.appendChild(badge);

          var badgeText = document.createElementNS(NS, 'text');
          badgeText.setAttribute('x', String(n.x + n.radius * 0.7));
          badgeText.setAttribute('y', String(n.y - n.radius * 0.7));
          badgeText.setAttribute('text-anchor', 'middle');
          badgeText.setAttribute('dominant-baseline', 'central');
          badgeText.setAttribute('fill', '#fff');
          badgeText.setAttribute('font-size', '10');
          badgeText.setAttribute('font-weight', 'bold');
          badgeText.setAttribute('class', 'node-label');
          badgeText.textContent = expanded[n.id] ? '\\u2212' : '+';
          g.appendChild(badgeText);
        }
      }

      // Workflowy link indicator dot (small dot bottom-left: blue=linked, grey=inferred)
      var indicatorColor = n.workflowyNodeId ? '#3498db' : '#bbb';
      var indicator = document.createElementNS(NS, 'circle');
      indicator.setAttribute('cx', String(n.x - n.radius * 0.65));
      indicator.setAttribute('cy', String(n.y + n.radius * 0.65));
      indicator.setAttribute('r', '4');
      indicator.setAttribute('fill', indicatorColor);
      indicator.setAttribute('stroke', '#fff');
      indicator.setAttribute('stroke-width', '1');
      indicator.setAttribute('class', 'wf-indicator');
      g.appendChild(indicator);

      nodeLayer.appendChild(g);
    });
  }

  function updatePositions() {
    nodeLayer.querySelectorAll('.node-group').forEach(function(g) {
      var id = g.getAttribute('data-id');
      var n = nodeById(id);
      if (!n) return;

      // Main circle
      var circle = g.querySelector(':scope > circle');
      if (circle) { circle.setAttribute('cx', n.x); circle.setAttribute('cy', n.y); }

      // Word-wrapped label tspans
      var text = g.querySelector('text.node-label');
      if (text) {
        var tspans = text.querySelectorAll('tspan');
        if (tspans.length > 0) {
          var fontSize = parseInt(text.getAttribute('font-size')) || 11;
          var lineHeight = fontSize * 1.3;
          var startY = n.y - ((tspans.length - 1) * lineHeight) / 2;
          for (var ti = 0; ti < tspans.length; ti++) {
            tspans[ti].setAttribute('x', String(n.x));
            tspans[ti].setAttribute('y', String(startY + ti * lineHeight));
          }
        } else {
          text.setAttribute('x', n.x);
          text.setAttribute('y', n.y);
        }
      }

      // Expand badge
      var badge = g.querySelector('.expand-badge');
      if (badge) {
        badge.setAttribute('cx', n.x + n.radius * 0.7);
        badge.setAttribute('cy', n.y - n.radius * 0.7);
      }
      var badgeTexts = g.querySelectorAll('text.node-label');
      if (badgeTexts.length > 1) {
        var last = badgeTexts[badgeTexts.length - 1];
        if (last.textContent && last.textContent.length <= 1) {
          last.setAttribute('x', n.x + n.radius * 0.7);
          last.setAttribute('y', n.y - n.radius * 0.7);
        }
      }

      // Workflowy indicator dot
      var indicator = g.querySelector('.wf-indicator');
      if (indicator) {
        indicator.setAttribute('cx', n.x - n.radius * 0.65);
        indicator.setAttribute('cy', n.y + n.radius * 0.65);
      }
    });

    // Update edges
    var edgeGs = edgeLayer.querySelectorAll('.edge-group');
    activeEdges.forEach(function(e, i) {
      if (i >= edgeGs.length) return;
      var n1 = nodeById(e.from), n2 = nodeById(e.to);
      if (!n1 || !n2) return;
      var g = edgeGs[i];
      var path = g.querySelector('path');
      var mx = (n1.x + n2.x) / 2, my = (n1.y + n2.y) / 2;
      var dx = n2.x - n1.x, dy = n2.y - n1.y;
      var ctrlX = mx - dy * 0.12, ctrlY = my + dx * 0.12;
      if (path) path.setAttribute('d', 'M' + n1.x + ',' + n1.y + ' Q' + ctrlX + ',' + ctrlY + ' ' + n2.x + ',' + n2.y);
      var label = g.querySelector('.edge-label');
      if (label) { label.setAttribute('x', ctrlX); label.setAttribute('y', ctrlY); }
    });
  }

  // ── Animated simulation ──
  function runSimulation(iterations, callback) {
    var iter = 0;
    var alpha = 1.0;
    var decay = 1 - Math.pow(0.001, 1 / iterations);
    function step() {
      if (iter >= iterations || alpha < 0.001) {
        if (callback) callback();
        return;
      }
      alpha *= (1 - decay);
      applyForces(alpha);
      updatePositions();
      iter++;
      animFrame = requestAnimationFrame(step);
    }
    if (animFrame) cancelAnimationFrame(animFrame);
    step();
  }

  // ── Event handling ──
  function applyTransform() {
    zoomGroup.setAttribute('transform', 'translate(' + panX + ',' + panY + ') scale(' + scale + ')');
  }

  container.addEventListener('wheel', function(e) {
    e.preventDefault();
    var delta = e.deltaY > 0 ? 0.9 : 1.1;
    scale = Math.max(0.2, Math.min(scale * delta, 5));
    applyTransform();
  });

  container.addEventListener('mousedown', function(e) {
    // Don't start drag if clicking inside popup or slider panel
    if (e.target.closest('.node-popup')) return;
    if (e.target.closest('.slider-panel')) return;
    var nodeEl = e.target.closest('.node-group');
    if (nodeEl) {
      var id = nodeEl.getAttribute('data-id');
      var n = nodeById(id);
      if (!n) return;
      isDragging = true;
      dragNode = n;
      var pt = svgPoint(e);
      dragOffsetX = pt.x - n.x;
      dragOffsetY = pt.y - n.y;
      e.preventDefault();
      return;
    }
    // Clicking on background hides popup
    if (!e.target.closest('.node-popup') && !e.target.closest('.slider-panel')) {
      hideNodePopup();
    }
    isPanning = true;
    panStartX = e.clientX - panX;
    panStartY = e.clientY - panY;
  });

  container.addEventListener('mousemove', function(e) {
    if (isDragging && dragNode) {
      var pt = svgPoint(e);
      dragNode.x = pt.x - dragOffsetX;
      dragNode.y = pt.y - dragOffsetY;
      if (dragNode.fx !== undefined && dragNode.fx !== null) {
        dragNode.fx = dragNode.x;
        dragNode.fy = dragNode.y;
      }
      updatePositions();
      return;
    }
    if (isPanning) {
      panX = e.clientX - panStartX;
      panY = e.clientY - panStartY;
      applyTransform();
      return;
    }
    // Tooltip
    var nodeEl = e.target.closest('.node-group');
    if (nodeEl) {
      var id = nodeEl.getAttribute('data-id');
      var n = nodeById(id);
      if (n) {
        tooltip.textContent = n.label;
        tooltip.style.display = 'block';
        tooltip.style.left = (e.clientX + 12) + 'px';
        tooltip.style.top = (e.clientY - 10) + 'px';
      }
    } else {
      tooltip.style.display = 'none';
    }
  });

  container.addEventListener('mouseup', function(e) {
    if (isDragging && dragNode) {
      isDragging = false;
      runSimulation(50);
      dragNode = null;
      return;
    }
    isPanning = false;
  });
  container.addEventListener('mouseleave', function() { isPanning = false; isDragging = false; dragNode = null; tooltip.style.display = 'none'; });

  // Click to show node popup
  container.addEventListener('click', function(e) {
    if (isDragging) return;
    // Ignore clicks inside popup
    if (e.target.closest('.node-popup')) return;

    var nodeEl = e.target.closest('.node-group');
    if (!nodeEl) {
      hideNodePopup();
      return;
    }

    var id = nodeEl.getAttribute('data-id');
    var n = nodeById(id);
    if (!n) return;

    // If clicking the same node, toggle popup off
    if (popupNodeId === id) {
      hideNodePopup();
      return;
    }

    showNodePopup(n, e.clientX, e.clientY);
  });

  function svgPoint(e) {
    var rect = svg.getBoundingClientRect();
    return {
      x: (e.clientX - rect.left - panX) / scale,
      y: (e.clientY - rect.top - panY) / scale
    };
  }

  // ── Slider controls ──
  var sliderPanel = document.getElementById('sliderPanel');
  var physicsBtn = document.getElementById('physicsBtn');

  window.togglePhysics = function() {
    var visible = sliderPanel.style.display !== 'none';
    sliderPanel.style.display = visible ? 'none' : 'block';
    physicsBtn.classList.toggle('active', !visible);
  };

  function setupSlider(sliderId, valId, paramKey, divisor) {
    var slider = document.getElementById(sliderId);
    var valEl = document.getElementById(valId);
    if (!slider || !valEl) return;
    slider.addEventListener('input', function() {
      var raw = parseFloat(slider.value);
      var val = divisor ? raw / divisor : raw;
      forceParams[paramKey] = val;
      valEl.textContent = divisor ? val.toFixed(divisor > 100 ? 3 : 2) : String(Math.round(val));
      rebuildActive();
      render();
      runSimulation(150);
    });
  }

  setupSlider('chargeSlider', 'chargeVal', 'charge', 0);
  setupSlider('linkDistSlider', 'linkDistVal', 'linkDist', 0);
  setupSlider('gravitySlider', 'gravityVal', 'gravity', 1000);
  setupSlider('dampingSlider', 'dampingVal', 'damping', 100);
  setupSlider('overlapSlider', 'overlapVal', 'overlap', 10);

  // ── Global controls ──
  window.resetView = function() {
    scale = 1; panX = 0; panY = 0;
    applyTransform();
  };
  window.expandAll = function() {
    graphNodes.forEach(function(n) {
      if (n.level === 'major') {
        var hasChildren = graphNodes.some(function(d) { return d.level === 'detail' && d.parentMajorId === n.id; });
        if (hasChildren) {
          if (!expanded[n.id]) {
            expanded[n.id] = true;
            graphNodes.forEach(function(d) {
              if (d.level === 'detail' && d.parentMajorId === n.id) {
                d.x = n.x + (Math.random() - 0.5) * 60;
                d.y = n.y + (Math.random() - 0.5) * 60;
                d.vx = 0; d.vy = 0;
              }
            });
          }
        }
      }
    });
    rebuildActive();
    render();
    runSimulation(200);
  };
  window.collapseAll = function() {
    Object.keys(expanded).forEach(function(k) { expanded[k] = false; });
    rebuildActive();
    render();
    runSimulation(100);
  };

  // ── Initial render ──
  rebuildActive();
  simulate(200);
  render();
})();
</script>
</body>
</html>`;
}
