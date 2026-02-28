/**
 * Interactive concept map HTML generator.
 * Produces a self-contained HTML string with SVG + vanilla JS
 * that renders a collapsible, interactive concept map.
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

// Color palettes matching the static Graphviz maps
const CORE_COLOR = "#1a5276";
const MAJOR_COLORS = ["#2874a6", "#1e8449", "#b9770e", "#6c3483", "#1abc9c"];
const DETAIL_COLORS = ["#5dade2", "#58d68d", "#f4d03f", "#bb8fce", "#76d7c4"];

function escapeHtml(str: string): string {
  return str.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function truncateLabel(str: string, maxLen = 30): string {
  const chars = Array.from(str);
  if (chars.length > maxLen) return chars.slice(0, maxLen - 1).join("") + "\u2026";
  return str;
}

function getEdgeColor(type: string): string {
  const t = type.toLowerCase();
  if (t.includes("contrast") || t.includes("oppose")) return "#c0392b";
  if (t.includes("support") || t.includes("extend")) return "#27ae60";
  if (t.includes("require") || t.includes("depend")) return "#8e44ad";
  return "#566573";
}

/**
 * Generate a self-contained interactive HTML concept map.
 */
export function generateInteractiveConceptMapHTML(
  title: string,
  coreNode: { id: string; label: string },
  concepts: InteractiveConcept[],
  relationships: InteractiveRelationship[],
): string {
  const WIDTH = 900;
  const HEIGHT = 900;
  const CX = WIDTH / 2;
  const CY = HEIGHT / 2;
  const R_MAJOR = 280;
  const R_DETAIL = 100;

  const majors = concepts.filter((c) => c.level === "major");
  const details = concepts.filter((c) => c.level === "detail");

  // Assign positions: core at center, majors in circle, details around their parent
  interface NodePos { x: number; y: number; color: string; textColor: string; radius: number; level: string; parentMajorId?: string }
  const positions = new Map<string, NodePos>();

  // Core
  positions.set(coreNode.id, { x: CX, y: CY, color: CORE_COLOR, textColor: "#fff", radius: 40, level: "core" });

  // Majors
  majors.forEach((m, i) => {
    const angle = (2 * Math.PI * i) / Math.max(majors.length, 1) - Math.PI / 2;
    const x = CX + R_MAJOR * Math.cos(angle);
    const y = CY + R_MAJOR * Math.sin(angle);
    const color = MAJOR_COLORS[i % MAJOR_COLORS.length];
    const r = Math.max(28, Math.min(28 + (m.importance || 5) * 1.5, 42));
    positions.set(m.id, { x, y, color, textColor: "#fff", radius: r, level: "major" });
  });

  // Details — placed around their parent major concept
  const detailsByParent = new Map<string, InteractiveConcept[]>();
  for (const d of details) {
    const pid = d.parentMajorId || majors[0]?.id || coreNode.id;
    if (!detailsByParent.has(pid)) detailsByParent.set(pid, []);
    detailsByParent.get(pid)!.push(d);
  }

  for (const [parentId, children] of detailsByParent) {
    const parentPos = positions.get(parentId);
    if (!parentPos) continue;
    // Spread details in an arc around the parent
    const parentAngle = Math.atan2(parentPos.y - CY, parentPos.x - CX);
    const arcSpread = Math.min(Math.PI * 0.6, children.length * 0.35);
    const startAngle = parentAngle - arcSpread / 2;

    children.forEach((d, i) => {
      const angle = children.length === 1
        ? parentAngle
        : startAngle + (arcSpread * i) / Math.max(children.length - 1, 1);
      const x = parentPos.x + R_DETAIL * Math.cos(angle);
      const y = parentPos.y + R_DETAIL * Math.sin(angle);
      const colorIdx = details.indexOf(d);
      const color = DETAIL_COLORS[colorIdx % DETAIL_COLORS.length];
      const r = Math.max(20, Math.min(20 + (d.importance || 3) * 1.2, 32));
      positions.set(d.id, { x, y, color, textColor: "#1a1a1a", radius: r, level: "detail", parentMajorId: parentId });
    });
  }

  // Build SVG edges
  const edgeSvg: string[] = [];
  const addedEdges = new Set<string>();
  for (const rel of relationships) {
    const key = [rel.from, rel.to].sort().join("|||");
    if (addedEdges.has(key)) continue;
    addedEdges.add(key);

    const fromPos = positions.get(rel.from);
    const toPos = positions.get(rel.to);
    if (!fromPos || !toPos) continue;

    const color = getEdgeColor(rel.type);
    const width = Math.max(1, Math.min(1 + (rel.strength || 5) * 0.3, 4));
    const dashed = rel.type.toLowerCase().includes("contrast") || rel.type.toLowerCase().includes("oppose");

    // Determine if this edge connects to a detail node (for collapse)
    const fromDetail = positions.get(rel.from)?.level === "detail";
    const toDetail = positions.get(rel.to)?.level === "detail";
    const detailParent = fromDetail ? positions.get(rel.from)?.parentMajorId : toDetail ? positions.get(rel.to)?.parentMajorId : undefined;

    // Quadratic bezier with slight curve
    const mx = (fromPos.x + toPos.x) / 2;
    const my = (fromPos.y + toPos.y) / 2;
    const dx = toPos.x - fromPos.x;
    const dy = toPos.y - fromPos.y;
    const cx = mx - dy * 0.15;
    const cy = my + dx * 0.15;

    const label = rel.type !== "relates to" ? escapeHtml(truncateLabel(rel.type, 20)) : "";
    const parentAttr = detailParent ? ` data-parent-major="${escapeHtml(detailParent)}"` : "";
    const detailClass = (fromDetail || toDetail) ? " detail-edge" : "";

    edgeSvg.push(`<g class="edge${detailClass}"${parentAttr}>`);
    edgeSvg.push(`  <path d="M${fromPos.x},${fromPos.y} Q${cx},${cy} ${toPos.x},${toPos.y}" fill="none" stroke="${color}" stroke-width="${width}"${dashed ? ' stroke-dasharray="6,3"' : ""} opacity="0.7"/>`);
    if (label) {
      edgeSvg.push(`  <text x="${cx}" y="${cy}" text-anchor="middle" font-size="9" fill="${color}" opacity="0.85">${label}</text>`);
    }
    edgeSvg.push("</g>");
  }

  // Build SVG nodes
  const nodeSvg: string[] = [];

  // Draw core node
  const corePos = positions.get(coreNode.id)!;
  nodeSvg.push(`<g class="node core-node" data-id="${escapeHtml(coreNode.id)}">`);
  nodeSvg.push(`  <circle cx="${corePos.x}" cy="${corePos.y}" r="${corePos.radius}" fill="${corePos.color}" stroke="#0d2f3e" stroke-width="3"/>`);
  nodeSvg.push(`  <text x="${corePos.x}" y="${corePos.y}" text-anchor="middle" dominant-baseline="central" fill="${corePos.textColor}" font-size="14" font-weight="bold">${escapeHtml(truncateLabel(coreNode.label, 20))}</text>`);
  nodeSvg.push("</g>");

  // Draw major nodes
  for (const m of majors) {
    const pos = positions.get(m.id)!;
    const hasChildren = detailsByParent.has(m.id);
    const cls = hasChildren ? "node major-node collapsible" : "node major-node";
    nodeSvg.push(`<g class="${cls}" data-id="${escapeHtml(m.id)}">`);
    nodeSvg.push(`  <circle cx="${pos.x}" cy="${pos.y}" r="${pos.radius}" fill="${pos.color}" stroke="#fff" stroke-width="2" class="node-circle"/>`);
    nodeSvg.push(`  <text x="${pos.x}" y="${pos.y}" text-anchor="middle" dominant-baseline="central" fill="${pos.textColor}" font-size="11" font-weight="bold">${escapeHtml(truncateLabel(m.label, 18))}</text>`);
    if (hasChildren) {
      // Collapse indicator
      nodeSvg.push(`  <text x="${pos.x + pos.radius - 6}" y="${pos.y - pos.radius + 10}" text-anchor="middle" font-size="12" fill="#fff" class="collapse-indicator">\u2212</text>`);
    }
    nodeSvg.push("</g>");
  }

  // Draw detail nodes
  for (const d of details) {
    const pos = positions.get(d.id);
    if (!pos) continue;
    const parentId = pos.parentMajorId || "";
    nodeSvg.push(`<g class="node detail-node" data-id="${escapeHtml(d.id)}" data-parent-major="${escapeHtml(parentId)}">`);
    nodeSvg.push(`  <circle cx="${pos.x}" cy="${pos.y}" r="${pos.radius}" fill="${pos.color}" stroke="#fff" stroke-width="1.5" class="node-circle"/>`);
    nodeSvg.push(`  <text x="${pos.x}" y="${pos.y}" text-anchor="middle" dominant-baseline="central" fill="${pos.textColor}" font-size="9">${escapeHtml(truncateLabel(d.label, 16))}</text>`);
    nodeSvg.push("</g>");
  }

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
.node { cursor: pointer; transition: opacity 0.3s ease; }
.node:hover circle.node-circle { stroke-width: 3; filter: brightness(1.1); }
.collapsible { cursor: pointer; }
.collapsible .collapse-indicator { pointer-events: none; }
.detail-node, .detail-edge { transition: opacity 0.35s ease, transform 0.35s ease; }
.detail-node.collapsed, .detail-edge.collapsed { opacity: 0; pointer-events: none; }
.edge { transition: opacity 0.3s ease; }
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
    <svg id="mapSvg" viewBox="0 0 ${WIDTH} ${HEIGHT}" xmlns="http://www.w3.org/2000/svg">
      <g id="zoomGroup">
        <g id="edgeLayer">${edgeSvg.join("\n")}</g>
        <g id="nodeLayer">${nodeSvg.join("\n")}</g>
      </g>
    </svg>
    <div class="legend">
      <div class="legend-item"><div class="legend-dot" style="background:${CORE_COLOR}"></div> Core concept</div>
      <div class="legend-item"><div class="legend-dot" style="background:${MAJOR_COLORS[0]}"></div> Major concept (click to collapse)</div>
      <div class="legend-item"><div class="legend-dot" style="background:${DETAIL_COLORS[0]}"></div> Detail concept</div>
      <div class="legend-item"><div class="legend-line" style="background:#566573"></div> Relationship</div>
    </div>
  </div>
</div>
<script>
(function() {
  const collapsedState = {};
  const svg = document.getElementById('mapSvg');
  const zoomGroup = document.getElementById('zoomGroup');
  const container = document.getElementById('svgContainer');

  // Zoom and pan state
  let scale = 1, panX = 0, panY = 0, isPanning = false, startX = 0, startY = 0;

  function applyTransform() {
    zoomGroup.setAttribute('transform', 'translate(' + panX + ',' + panY + ') scale(' + scale + ')');
  }

  container.addEventListener('wheel', function(e) {
    e.preventDefault();
    const delta = e.deltaY > 0 ? 0.9 : 1.1;
    scale = Math.max(0.3, Math.min(scale * delta, 5));
    applyTransform();
  });

  container.addEventListener('mousedown', function(e) {
    if (e.target.closest('.node')) return;
    isPanning = true; startX = e.clientX - panX; startY = e.clientY - panY;
  });
  container.addEventListener('mousemove', function(e) {
    if (!isPanning) return;
    panX = e.clientX - startX; panY = e.clientY - startY;
    applyTransform();
  });
  container.addEventListener('mouseup', function() { isPanning = false; });
  container.addEventListener('mouseleave', function() { isPanning = false; });

  // Collapse/expand
  document.querySelectorAll('.collapsible').forEach(function(node) {
    node.addEventListener('click', function() {
      const majorId = node.getAttribute('data-id');
      collapsedState[majorId] = !collapsedState[majorId];
      updateVisibility();
    });
  });

  function updateVisibility() {
    document.querySelectorAll('.detail-node').forEach(function(el) {
      const parentId = el.getAttribute('data-parent-major');
      el.classList.toggle('collapsed', !!collapsedState[parentId]);
    });
    document.querySelectorAll('.detail-edge').forEach(function(el) {
      const parentId = el.getAttribute('data-parent-major');
      el.classList.toggle('collapsed', !!collapsedState[parentId]);
    });
    document.querySelectorAll('.collapsible .collapse-indicator').forEach(function(el) {
      const majorId = el.closest('.collapsible').getAttribute('data-id');
      el.textContent = collapsedState[majorId] ? '+' : '\\u2212';
    });
  }

  // Hover highlight
  document.querySelectorAll('.node').forEach(function(node) {
    node.addEventListener('mouseenter', function() {
      const id = node.getAttribute('data-id');
      document.querySelectorAll('.edge').forEach(function(edge) {
        const path = edge.querySelector('path');
        if (!path) return;
        const d = path.getAttribute('d') || '';
        // Highlight edges connected to this node by checking path data
        edge.style.opacity = '1';
      });
    });
    node.addEventListener('mouseleave', function() {
      document.querySelectorAll('.edge').forEach(function(edge) {
        edge.style.opacity = '';
      });
    });
  });

  window.resetView = function() { scale = 1; panX = 0; panY = 0; applyTransform(); };
  window.expandAll = function() { Object.keys(collapsedState).forEach(function(k) { collapsedState[k] = false; }); updateVisibility(); };
  window.collapseAll = function() {
    document.querySelectorAll('.collapsible').forEach(function(n) { collapsedState[n.getAttribute('data-id')] = true; });
    updateVisibility();
  };
})();
</script>
</body>
</html>`;
}
