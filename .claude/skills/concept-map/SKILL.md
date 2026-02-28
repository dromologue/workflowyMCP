---
name: concept-map
description: Generate an interactive concept map from a Workflowy subtree. Analyzes content semantically and renders a zoomable, collapsible HTML visualization. Use when the user asks for a concept map, mind map, or visual overview of a Workflowy topic.
argument-hint: [node-name-or-topic]
allowed-tools: Read, Bash, Glob, Grep, AskUserQuestion
---

# Interactive Concept Map Generator

Generate an interactive, zoomable concept map from a Workflowy subtree. The map renders as HTML inside Claude Desktop (via MCP Apps) and is also saved to `~/Downloads/` for browser viewing.

## Workflow

Follow these steps in order. Do not skip steps.

### Step 1: Find the node

Use the `$ARGUMENTS` as a search term to locate the Workflowy node.

- Try `find_node` with `match_mode: "contains"` first
- If no match, fall back to `search_nodes` with the argument as the query
- If multiple matches, show the user the options and ask which one to use
- If no matches at all, tell the user and stop

### Step 2: Extract content

Call `get_node_content_for_analysis` with:
- `node_id`: the ID from Step 1
- `follow_links`: true (to capture cross-references)
- `include_notes`: true
- `format`: "structured"

Read through the returned content carefully. Understand the hierarchy, themes, and how ideas relate to each other.

### Step 3: Analyze for concepts and relationships

From the content, identify:

**Major concepts** (5-8 ideally):
- The main themes, categories, or pillars in the content
- Each gets an `id` (lowercase-kebab-case), `label`, `level: "major"`, and `importance` (1-10)
- Higher importance = larger node in the visualization

**Detail concepts** (2-5 per major):
- Specific ideas, examples, or sub-themes that belong under a major concept
- Each gets `level: "detail"` and a `parent_major_id` pointing to its parent major
- If a detail relates to multiple majors, assign it to the strongest connection

**Relationships** (aim for 10-25):
- Connections between concepts (major-to-major, major-to-detail, or detail-to-detail)
- Each relationship needs a `type` — a verb phrase describing the connection
- Each relationship gets a `strength` (1-10) reflecting how strong the connection is

**Relationship type vocabulary** (use these or similar semantic labels):
- Causal/dependency: `produces`, `enables`, `requires`, `leads to`, `depends on`
- Evaluative: `critiques`, `extends`, `develops`, `refines`, `challenges`
- Comparative: `contrasts with`, `differs from`, `parallels`, `complements`
- Hierarchical: `includes`, `is a type of`, `exemplifies`, `generalizes`
- Influence: `influences`, `shapes`, `informs`, `draws from`

**Quality guidelines:**
- Prioritize non-obvious connections the user might not have noticed
- Avoid trivial parent-child relationships that are already visible in the outline
- Use specific relationship labels, not generic "relates to"
- Every concept should have at least one relationship

### Step 4: Render the map

Call `render_interactive_concept_map` with:

```json
{
  "title": "Descriptive title based on the content",
  "core_concept": {
    "label": "Central theme label",
    "description": "Brief description of the central organizing idea"
  },
  "concepts": [
    {
      "id": "concept-id",
      "label": "Display Label",
      "level": "major",
      "importance": 8
    },
    {
      "id": "detail-id",
      "label": "Detail Label",
      "level": "detail",
      "importance": 5,
      "parent_major_id": "concept-id"
    }
  ],
  "relationships": [
    {
      "from": "concept-id",
      "to": "other-id",
      "type": "enables",
      "strength": 7
    }
  ]
}
```

### Step 5: Report results

After the tool returns, tell the user:
- How many concepts and relationships were mapped
- Where the HTML file was saved (from the tool result's `file_path`)
- That the map is a **force-directed layout** — nodes spread out to avoid overlap
- **Click any major concept** to expand its detail children
- **Drag nodes** to rearrange
- **Mouse wheel** to zoom, **drag background** to pan
- The HTML file in `~/Downloads/` is fully self-contained — no server needed, just open it in any browser

### Fallback behavior

If Claude Desktop fails to render the concept map inline (e.g., "Unsupported UI resource content format"), do NOT treat this as an error. Instead:

1. Tell the user: "The concept map couldn't render inline, but it has been saved as an interactive HTML file."
2. Provide the file path from the tool result's `file_path` field
3. Tell them to open it in their browser — it's a self-contained force-directed graph with full click-to-expand, drag, zoom, and pan. No server or internet connection required.
