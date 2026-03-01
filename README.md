# Workflowy MCP Server

An MCP server that gives Claude full read/write access to your Workflowy outline. Search, insert, organize, manage tasks, and generate interactive visualizations — all through natural language.

Works with **Claude Desktop** (MCP tools) and **Claude Code** (CLI + skills).

## Install

```bash
git clone https://github.com/dromologue/workflowyMCP.git
cd workflowyMCP
npm install
npm run build
```

## Configure

### Required: Workflowy API key

1. Get your API key from [workflowy.com/api-key](https://workflowy.com/api-key)

2. Create `.env` in the project root:

```
WORKFLOWY_API_KEY=your-api-key
```

### Optional: Anthropic API (for concept map auto-analysis)

```
ANTHROPIC_API_KEY=sk-ant-your-key
```

Enables `--auto` mode in the CLI concept map tool, where Claude analyzes content and extracts concepts automatically.

### Optional: Dropbox (for cloud-hosted maps)

When configured, concept maps and task maps are uploaded to Dropbox and a clickable link is added to your Tasks node in Workflowy.

1. Create a Dropbox app at [dropbox.com/developers/apps](https://www.dropbox.com/developers/apps):
   - Choose **Scoped access**
   - Choose **Full Dropbox** access
   - Under **Permissions**, enable `files.content.write` and `sharing.write`

2. Generate a refresh token (the app console provides a short-lived access token; you need a long-lived refresh token):
   - Use the OAuth 2 flow or the Dropbox API Explorer to obtain a refresh token for your app

3. Add to `.env`:

```
DROPBOX_APP_KEY=your-app-key
DROPBOX_APP_SECRET=your-app-secret
DROPBOX_REFRESH_TOKEN=your-refresh-token
```

Maps are saved to:
- **Concept maps:** `/Workflowy/ConceptMaps/`
- **Task maps:** `/Workflowy/TaskMaps/`

A clickable link to each map is added under your root-level **Tasks** node in Workflowy.

### Claude Desktop setup

Add to your Claude Desktop config:

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

Restart Claude Desktop. All tools below become available as MCP tools that Claude can call directly.

### Claude Code setup

No extra config needed — the CLI tools and skills work directly from the project directory.

To add the skills to Claude Code, copy the skill folders from the repo or create them under `~/.claude/skills/`:
- `/concept-map` — `/concept-map [topic]` to generate concept maps
- `/task-map` — `/task-map` to generate task maps

## Usage

### Via Claude Desktop

Ask Claude naturally — it will use the MCP tools:

- "Search my Workflowy for anything tagged #review"
- "Create a concept map of my Philosophy notes"
- "Generate a task map from my tags"
- "What's overdue in my Projects?"

### Via Claude Code

Use the skills:

```bash
/concept-map Philosophy        # generate concept map from a Workflowy subtree
/task-map                      # generate task map from Tags node
/task-map --exclude-completed  # exclude completed nodes
```

Or use the CLI directly:

```bash
# Concept maps (requires ANTHROPIC_API_KEY for --auto)
npm run concept-map -- --search "Topic" --auto
npm run concept-map -- --search "Topic" --auto --depth 3 --insert

# Task maps
npm run task-map
npm run task-map -- --exclude-completed
npm run task-map -- --insert          # also insert outline into Workflowy
```

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
| `render_interactive_concept_map` | Generate an interactive HTML concept map |
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

## Concept Maps

Generate interactive, zoomable concept maps from any Workflowy subtree. Claude analyzes the content, discovers concepts and relationships, and renders a force-directed HTML visualization.

### Interaction

- **Click a major concept** to expand its detail children
- **Drag any node** to rearrange the layout
- **Scroll** to zoom, **drag background** to pan
- **Expand All / Collapse All** buttons in the toolbar

The HTML file is saved to `~/Downloads/` — fully self-contained, no server needed. If Dropbox is configured, it is also uploaded and a clickable link is added to your Tasks node in Workflowy.

## Task Maps

Generate a concept map from your Workflowy Tags node. Finds all `#tag` definitions under the root-level "Tags" node, searches for matching nodes using prefix matching (e.g. `#action_` matches `#action_review`), and visualises tags as major concepts with matched nodes as expandable details.

If Dropbox is configured, the HTML is uploaded to `/Workflowy/TaskMaps/` and a clickable link is added under your Tasks node in Workflowy.

## Development

```bash
npm run build        # compile TypeScript
npm test             # run tests
npm run mcp:dev      # build + start server
```

## License

MIT
