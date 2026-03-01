---
name: concept-map
description: Generate an interactive concept map from a Workflowy subtree. Analyzes content semantically and renders a zoomable, collapsible HTML visualization. Use when the user asks for a concept map, mind map, or visual overview of a Workflowy topic.
argument-hint: [node-name-or-topic] [level N]
allowed-tools: Read, Bash, Glob, Grep, AskUserQuestion
---

# Interactive Concept Map Generator

Generate an interactive, zoomable concept map from a Workflowy subtree. Saves a self-contained HTML file to `~/Downloads/` that works in any browser.

## Workflow

Follow these steps in order. Do not skip steps.

### Step 1: Parse arguments

The `$ARGUMENTS` may include:
- A **search term** (the node name or topic)
- An optional **depth** specified as "level N", "depth N", or "to level N" (e.g., "Organisational Prompts level 3")

Parse the depth number if present. If not specified, use unlimited depth (omit the depth flag).

Also check if the user wants to **insert the outline into Workflowy** (e.g., "insert", "add to workflowy", "save outline"). If so, add the `--insert` flag.

### Step 2: Run the CLI

Use the Bash tool to run the concept-map CLI from the project directory:

```bash
cd ~/code/workflowy-mcp-server && npx tsx src/cli/concept-map.ts --search "<search-term>" --auto [--depth <N>] [--insert]
```

- `--search "<search-term>"`: The node name to find in Workflowy
- `--auto`: Use Claude API to semantically analyze content and discover concepts + relationships
- `--depth <N>`: Optional depth limit (omit for unlimited)
- `--core "<label>"`: Optional override for the central concept label
- `--insert`: Insert a structured outline into Workflowy as a sibling of the analyzed node (with links and backlinks)
- `--force`: Overwrite existing outline (use with `--insert` if regenerating)

If the user provided specific concepts instead of asking for auto-discovery:
```bash
cd ~/code/workflowy-mcp-server && npx tsx src/cli/concept-map.ts --search "<search-term>" --concepts "A,B,C" [--depth <N>] [--insert]
```

### Step 3: Report results

After the CLI completes, tell the user:
- How many concepts and relationships were mapped (from CLI output)
- Where the HTML file was saved
- That the map is a **force-directed layout** — nodes spread out to avoid overlap
- **Click any major concept** to expand its detail children
- **Drag nodes** to rearrange
- **Mouse wheel** to zoom, **drag background** to pan
- **Physics button** opens sliders to tune charge, repulsion, gravity, and spacing
- The HTML file is fully self-contained — no server needed, just open in any browser

### Fallback: MCP tools (Claude Desktop only)

If running inside Claude Desktop with MCP tools available, you can alternatively use the MCP tool workflow:
1. `find_node` with `match_mode: "contains"` to locate the node
2. `get_node_content_for_analysis` with `node_id`, `depth` (if specified), `follow_links: true`, `include_notes: true`, `format: "structured"`
3. Analyze the content for concepts and relationships (see quality guidelines below)
4. `render_interactive_concept_map` with the analysis results

### Quality guidelines for concept analysis

**Major concepts** (5-8): Main themes, categories, or pillars. Each gets `level: "major"` and `importance` (1-10).

**Detail concepts** (2-5 per major): Specific ideas under each major. Each gets `level: "detail"` and `parent_major_id`.

**Relationships** (10-25): Use specific verb phrases:
- Causal: `produces`, `enables`, `requires`, `leads to`, `depends on`
- Evaluative: `critiques`, `extends`, `develops`, `refines`, `challenges`
- Comparative: `contrasts with`, `differs from`, `parallels`, `complements`
- Hierarchical: `includes`, `is a type of`, `exemplifies`, `generalizes`
- Influence: `influences`, `shapes`, `informs`, `draws from`

Prioritize non-obvious connections. Use specific labels, not generic "relates to". Every concept needs at least one relationship.
