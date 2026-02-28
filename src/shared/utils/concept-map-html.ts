/**
 * Interactive concept map HTML generator.
 * Produces a self-contained HTML string with SVG + vanilla JS force-directed layout.
 * Nodes repel each other, connected nodes attract, and users can drag to rearrange.
 * Detail nodes are hidden by default — click a major concept to expand its children.
 */

export interface InteractiveConcept {
  id: string;
  label: string;
  level: "major" | "detail";
  importance: number;
  parentMajorId?: string;
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
export function generateInteractiveConceptMapHTML(
  title: string,
  coreNode: { id: string; label: string },
  concepts: InteractiveConcept[],
  relationships: InteractiveRelationship[],
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
.title-bar { padding: 10px 16px; background: #fff; border-bottom: 1px solid #e0e0e0; display: flex; justify-content: space-between; align-items: center; }
.title-bar h1 { font-size: 16px; color: #1a1a1a; font-weight: 600; }
.controls { display: flex; gap: 8px; }
.controls button { padding: 4px 10px; border: 1px solid #ccc; border-radius: 4px; background: #fff; cursor: pointer; font-size: 12px; }
.controls button:hover { background: #f0f0f0; }
.svg-container { flex: 1; overflow: hidden; cursor: grab; position: relative; }
.svg-container:active { cursor: grabbing; }
svg { width: 100%; height: 100%; }
.node-group { cursor: pointer; }
.node-group:hover circle { filter: brightness(1.15); stroke-width: 3; }
.node-group.dragging { cursor: grabbing; }
.node-label { pointer-events: none; user-select: none; }
.edge-group { pointer-events: none; }
.edge-label { font-size: 9px; pointer-events: none; user-select: none; }
.detail-hidden { display: none; }
.collapsible .expand-badge { cursor: pointer; }
.tooltip {
  position: absolute; padding: 6px 10px; background: rgba(0,0,0,0.85); color: #fff;
  border-radius: 4px; font-size: 12px; pointer-events: none; white-space: nowrap;
  display: none; z-index: 100;
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
    </div>
  </div>
  <div class="svg-container" id="svgContainer">
    <svg id="mapSvg" xmlns="http://www.w3.org/2000/svg">
      <g id="zoomGroup">
        <g id="edgeLayer"></g>
        <g id="nodeLayer"></g>
      </g>
    </svg>
    <div class="tooltip" id="tooltip"></div>
    <div class="legend">
      <div class="legend-item"><div class="legend-dot" style="background:${CORE_COLOR}"></div> Core concept</div>
      <div class="legend-item"><div class="legend-dot" style="background:${MAJOR_COLORS[0]}"></div> Major concept (click to expand)</div>
      <div class="legend-item"><div class="legend-dot" style="background:${DETAIL_COLORS[0]}"></div> Detail concept</div>
      <div class="legend-item"><div class="legend-line" style="background:#566573"></div> Relationship</div>
    </div>
  </div>
</div>
<script>
(function() {
  // ── Data ──
  var graphNodes = ${nodesJson};
  var graphEdges = ${edgesJson};

  // ── State ──
  var expanded = {};   // majorId → true/false
  var activeNodes = []; // currently visible nodes
  var activeEdges = []; // currently visible edges
  var scale = 1, panX = 0, panY = 0;
  var isDragging = false, dragNode = null, dragOffsetX = 0, dragOffsetY = 0;
  var isPanning = false, panStartX = 0, panStartY = 0;
  var width = 0, height = 0;
  var animFrame = null;

  // ── DOM refs ──
  var svg = document.getElementById('mapSvg');
  var zoomGroup = document.getElementById('zoomGroup');
  var edgeLayer = document.getElementById('edgeLayer');
  var nodeLayer = document.getElementById('nodeLayer');
  var container = document.getElementById('svgContainer');
  var tooltip = document.getElementById('tooltip');

  function resize() {
    width = container.clientWidth;
    height = container.clientHeight;
    svg.setAttribute('viewBox', '0 0 ' + width + ' ' + height);
  }
  resize();
  window.addEventListener('resize', resize);

  // ── Initialize node positions ──
  // Core at center, majors in a circle, details near their parent
  var cx = width / 2, cy = height / 2;
  graphNodes.forEach(function(n, i) {
    if (n.level === 'core') {
      n.x = cx; n.y = cy; n.fx = cx; n.fy = cy; // pinned
    } else if (n.level === 'major') {
      var majors = graphNodes.filter(function(m) { return m.level === 'major'; });
      var idx = majors.indexOf(n);
      var angle = (2 * Math.PI * idx) / Math.max(majors.length, 1) - Math.PI / 2;
      var r = Math.min(width, height) * 0.3;
      n.x = cx + r * Math.cos(angle);
      n.y = cy + r * Math.sin(angle);
    } else {
      // Details start near their parent
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
        var minDist = n1.radius + n2.radius + 30;
        force = -400 * alpha / (dist * dist);
        // Extra repulsion if overlapping
        if (dist < minDist) force -= (minDist - dist) * 0.5 * alpha;
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
      var idealDist = edge.isParentLink ? 120 : 180;
      force = (dist - idealDist) * 0.02 * alpha;
      ex = dx / dist * force; ey = dy / dist * force;
      if (!n1.fx) { n1.vx += ex; n1.vy += ey; }
      if (!n2.fx) { n2.vx -= ex; n2.vy -= ey; }
    }

    // Center gravity
    for (i = 0; i < nodes.length; i++) {
      n1 = nodes[i];
      if (n1.fx) continue;
      n1.vx += (cx - n1.x) * 0.005 * alpha;
      n1.vy += (cy - n1.y) * 0.005 * alpha;
    }

    // Apply velocities with damping
    for (i = 0; i < nodes.length; i++) {
      n1 = nodes[i];
      if (n1.fx) { n1.x = n1.fx; n1.y = n1.fy; continue; }
      n1.vx *= 0.6; n1.vy *= 0.6;
      n1.x += n1.vx; n1.y += n1.vy;
      // Soft boundary
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
    // Clear
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

      if (e.type && !e.isParentLink) {
        var label = document.createElementNS(NS, 'text');
        label.setAttribute('x', String(ctrlX));
        label.setAttribute('y', String(ctrlY));
        label.setAttribute('text-anchor', 'middle');
        label.setAttribute('class', 'edge-label');
        label.setAttribute('fill', e.color);
        label.setAttribute('opacity', '0.8');
        label.textContent = e.type.length > 20 ? e.type.slice(0, 19) + '\\u2026' : e.type;
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

      // Label
      var text = document.createElementNS(NS, 'text');
      text.setAttribute('x', String(n.x));
      text.setAttribute('y', String(n.y));
      text.setAttribute('text-anchor', 'middle');
      text.setAttribute('dominant-baseline', 'central');
      text.setAttribute('class', 'node-label');
      text.setAttribute('fill', n.textColor);
      var fontSize = n.level === 'core' ? 14 : n.level === 'major' ? 11 : 9;
      text.setAttribute('font-size', String(fontSize));
      if (n.level !== 'detail') text.setAttribute('font-weight', 'bold');
      var maxLen = n.level === 'core' ? 20 : n.level === 'major' ? 18 : 16;
      var labelText = n.label.length > maxLen ? n.label.slice(0, maxLen - 1) + '\\u2026' : n.label;
      text.textContent = labelText;
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

      nodeLayer.appendChild(g);
    });
  }

  function updatePositions() {
    // Update existing SVG elements without full rebuild
    nodeLayer.querySelectorAll('.node-group').forEach(function(g) {
      var id = g.getAttribute('data-id');
      var n = nodeById(id);
      if (!n) return;
      var circle = g.querySelector('circle:not(.expand-badge)');
      if (circle) { circle.setAttribute('cx', n.x); circle.setAttribute('cy', n.y); }
      var text = g.querySelector('text.node-label');
      if (text) { text.setAttribute('x', n.x); text.setAttribute('y', n.y); }
      var badge = g.querySelector('.expand-badge');
      if (badge) {
        badge.setAttribute('cx', n.x + n.radius * 0.7);
        badge.setAttribute('cy', n.y - n.radius * 0.7);
      }
      var badgeText = g.querySelectorAll('text.node-label');
      if (badgeText.length > 1) {
        badgeText[1].setAttribute('x', n.x + n.radius * 0.7);
        badgeText[1].setAttribute('y', n.y - n.radius * 0.7);
      }
    });

    // Update edges
    var edgeGs = edgeLayer.querySelectorAll('.edge-group');
    var edgeIdx = 0;
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
    var nodeEl = e.target.closest('.node-group');
    if (nodeEl) {
      var id = nodeEl.getAttribute('data-id');
      var n = nodeById(id);
      if (!n) return;
      isDragging = true;
      dragNode = n;
      // Convert mouse to SVG coordinates
      var pt = svgPoint(e);
      dragOffsetX = pt.x - n.x;
      dragOffsetY = pt.y - n.y;
      e.preventDefault();
      return;
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
      // Run short simulation to settle neighbors
      runSimulation(50);
      dragNode = null;
      return;
    }
    isPanning = false;
  });
  container.addEventListener('mouseleave', function() { isPanning = false; isDragging = false; dragNode = null; tooltip.style.display = 'none'; });

  // Click to expand/collapse
  container.addEventListener('click', function(e) {
    if (isDragging) return;
    var nodeEl = e.target.closest('.node-group');
    if (!nodeEl) return;
    var id = nodeEl.getAttribute('data-id');
    var level = nodeEl.getAttribute('data-level');
    if (level !== 'major') return;
    var hasChildren = graphNodes.some(function(d) { return d.level === 'detail' && d.parentMajorId === id; });
    if (!hasChildren) return;

    expanded[id] = !expanded[id];

    // Position newly revealed details near their parent
    if (expanded[id]) {
      var parent = nodeById(id);
      graphNodes.forEach(function(n) {
        if (n.level === 'detail' && n.parentMajorId === id) {
          n.x = parent.x + (Math.random() - 0.5) * 60;
          n.y = parent.y + (Math.random() - 0.5) * 60;
          n.vx = 0; n.vy = 0;
        }
      });
    }

    rebuildActive();
    render();
    runSimulation(150);
  });

  function svgPoint(e) {
    var rect = svg.getBoundingClientRect();
    return {
      x: (e.clientX - rect.left - panX) / scale,
      y: (e.clientY - rect.top - panY) / scale
    };
  }

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
