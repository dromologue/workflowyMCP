# Workflowy MCP Server

An MCP server that gives Claude full read/write access to your Workflowy outline. Search, insert, organize, and manage tasks — all through natural language.

## Install

```bash
git clone https://github.com/dromologue/workflowyMCP.git
cd workflowyMCP
npm install
npm run build
```

## Configure

1. Get your API key from [workflowy.com/api-key](https://workflowy.com/api-key)

2. Create `.env` in the project root:

```
WORKFLOWY_API_KEY=your-api-key
```

3. Add to Claude Desktop config:

**macOS:** `~/Library/Application Support/Claude/claude_desktop_config.json`
**Windows:** `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "node",
      "args": ["/absolute/path/to/workflowyMCP/dist/index.js"]
    }
  }
}
```

4. Restart Claude Desktop.

## Tools

### Search & Navigate

| Tool | What it does |
|------|-------------|
| `search_nodes` | Text search with optional tag, assignee, status, and date filters |
| `find_node` | Look up a node by exact name, returns ID for use with other tools |
| `get_node` | Get a node by ID |
| `get_children` | List children of a node |
| `find_backlinks` | Find all nodes linking to a given node |

### Create & Edit

| Tool | What it does |
|------|-------------|
| `insert_content` | Insert hierarchical content (auto-parallelizes for large payloads) |
| `smart_insert` | Search for a target node, then insert content into it |
| `update_node` | Edit a node's name or note |
| `move_node` | Move a node to a new parent |
| `delete_node` | Delete a node |
| `duplicate_node` | Deep-copy a node and its subtree to a new location |
| `create_from_template` | Copy a template subtree with `{{variable}}` substitution |

### Tasks & Todos

| Tool | What it does |
|------|-------------|
| `list_todos` | List todos under a node |
| `complete_node` / `uncomplete_node` | Toggle completion |
| `list_upcoming` | Todos due in the next N days, sorted by urgency |
| `list_overdue` | Past-due items sorted by most overdue first |
| `bulk_update` | Apply an operation to all nodes matching a filter |

### Project Management

| Tool | What it does |
|------|-------------|
| `get_project_summary` | Stats, tag counts, assignees, overdue items for a subtree |
| `get_recent_changes` | Nodes modified within a time window |
| `daily_review` | One-call standup summary: overdue, upcoming, recent, pending |

### Concept Maps & Task Maps

| Tool | What it does |
|------|-------------|
| `get_node_content_for_analysis` | Export subtree content for Claude to analyze |
| `render_interactive_concept_map` | Generate an interactive HTML concept map inline in Claude Desktop |
| `generate_task_map` | Build a concept map from your Tags node — tags as concepts, matched nodes as details |

### Graph Analysis

| Tool | What it does |
|------|-------------|
| `analyze_relationships` | Extract relationships from data objects, compute graph density |
| `create_adjacency_matrix` | Build adjacency matrix from relationship pairs |
| `calculate_centrality` | Degree, betweenness, closeness, eigenvector centrality |
| `analyze_network_structure` | Combined relationship extraction + centrality in one step |

### Files & Bulk

| Tool | What it does |
|------|-------------|
| `insert_file` | Insert a file's contents (server reads the file directly) |
| `convert_markdown_to_workflowy` | Convert markdown to Workflowy's indented format |
| `batch_operations` | Multiple create/update/delete in one call |
| `submit_job` / `get_job_status` | Background processing for large workloads |

## Conventions

Tags, assignees, and due dates are parsed from node text:

- **Tags:** `#inbox`, `#review`, `#urgent`
- **Assignees:** `@alice`, `@bob`
- **Due dates:** `due:2026-03-15`, `#due-2026-03-15`, or bare `2026-03-15`

## Optional: Anthropic API (for CLI auto-extraction)

```
ANTHROPIC_API_KEY=sk-ant-your-key
```

Enables `--auto` mode in the CLI concept map tool.

## Concept Maps

Generate interactive, zoomable concept maps from any Workflowy subtree. Claude analyzes the content, discovers concepts and relationships, and renders a force-directed HTML visualization.

### Quick start

```bash
# Via Claude Code skill (recommended)
/concept-map Philosophy

# Via CLI with Claude auto-analysis
npm run concept-map -- --search "Topic" --auto

# With depth limit and Workflowy outline insertion
npm run concept-map -- --search "Topic" --auto --depth 3 --insert
```

### Interaction

- **Click a major concept** to expand its detail children
- **Drag any node** to rearrange the layout
- **Scroll** to zoom, **drag background** to pan
- **Expand All / Collapse All** buttons in the toolbar

The HTML file is saved to `~/Downloads/` — fully self-contained, no server needed.

## Task Maps

Generate a concept map from your Workflowy Tags node. Finds all `#tag` definitions under the root-level "Tags" node, searches for matching nodes using prefix matching (e.g. `#action_` matches `#action_review`), and visualises tags as major concepts with matched nodes as expandable details.

### Quick start

```bash
# Generate task map
npm run task-map

# Exclude completed nodes
npm run task-map -- --exclude-completed

# Also insert outline into Workflowy and save to Dropbox
npm run task-map -- --insert
```

If Dropbox is configured, the HTML is also uploaded to `/Workflowy/TaskMaps/` and a link node is added under your Tags node in Workflowy.

## Development

```bash
npm run build        # compile TypeScript
npm test             # run tests
npm run mcp:dev      # build + start server
```

## License

MIT
