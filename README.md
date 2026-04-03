# Workflowy MCP Server

A Rust MCP server that gives Claude full read/write access to your Workflowy outline. Search, insert, organize, manage tasks, and track deadlines — all through natural language.

Works with **Claude Desktop** (MCP tools) and **Claude Code** (CLI + skills).

## Install

```bash
# Requires Rust toolchain (https://rustup.rs)
git clone https://github.com/dromologue/workflowyMCP.git
cd workflowyMCP
cargo build --release
```

## Configure

### Required: Workflowy API key

1. Get your API key from [workflowy.com/api-key](https://workflowy.com/api-key)

2. Create `.env` in the project root:

```
WORKFLOWY_API_KEY=your-api-key
```

### Optional: Dropbox (for cloud-hosted maps)

When configured, concept maps are uploaded to Dropbox with clickable links in Workflowy.

```
DROPBOX_APP_KEY=your-app-key
DROPBOX_APP_SECRET=your-app-secret
DROPBOX_REFRESH_TOKEN=your-refresh-token
```

All three must be set, or none.

### Claude Desktop setup

Add to your Claude Desktop config:

**macOS:** `~/Library/Application Support/Claude/claude_desktop_config.json`
**Windows:** `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "/absolute/path/to/workflowyMCP/target/release/workflowy-mcp-server"
    }
  }
}
```

Restart Claude Desktop. All tools below become available as MCP tools that Claude can call directly.

### Claude Code setup

No extra config needed — the skills work directly from the project directory.

## Usage

### Via Claude Desktop

Ask Claude naturally — it will use the MCP tools:

- "Search my Workflowy for anything tagged #review"
- "What's overdue in my Projects?"
- "Add a task to Office: Review Q2 budget"
- "Give me a daily review of my tasks"

## Tools (23 implemented)

### Search & Navigate

| Tool | What it does |
|------|-------------|
| `search_nodes` | Text search in node names and descriptions |
| `find_node` | Look up a node by name (exact, contains, or starts_with match modes) |
| `get_node` | Get a node by ID |
| `list_children` | List children of a node |
| `tag_search` | Search by tag (`#tag` or `@person`) in names, descriptions, and tags |
| `get_subtree` | Get the full tree under a node |
| `find_backlinks` | Find all nodes that link to a given node |

### Create & Edit

| Tool | What it does |
|------|-------------|
| `create_node` | Create a new node with optional parent and position |
| `insert_content` | Insert hierarchical content (2-space indentation = nesting) |
| `smart_insert` | Search for a target node, then insert content into it |
| `convert_markdown` | Convert markdown to Workflowy-compatible indented format |
| `edit_node` | Edit a node's name or description |
| `move_node` | Move a node to a new parent |
| `delete_node` | Delete a node |
| `duplicate_node` | Deep-copy a node and its subtree |
| `create_from_template` | Copy template with `{{variable}}` substitution |
| `bulk_update` | Apply operations to filtered nodes (with dry_run mode) |

### Todos & Scheduling

| Tool | What it does |
|------|-------------|
| `list_todos` | List todo items with optional parent, status, and text filters |
| `list_upcoming` | Items due in the next N days, sorted by nearest deadline |
| `list_overdue` | Past-due items sorted by most overdue first |
| `daily_review` | One-call standup: overdue, upcoming, recent changes, pending todos |

### Project Management

| Tool | What it does |
|------|-------------|
| `get_project_summary` | Stats, tag counts, assignees, overdue items for a subtree |
| `get_recent_changes` | Nodes modified within a time window |

### Not Yet Ported

Concept mapping (get_node_content_for_analysis, render_interactive_concept_map),
batch async (batch_operations, submit_job, get_job_status).
See `specs/tasks.md` for full roadmap.

## Conventions

Tags, assignees, and due dates are parsed from node text:

- **Tags:** `#inbox`, `#review`, `#urgent`
- **Assignees:** `@alice`, `@bob`
- **Due dates:** `due:2026-03-15`, `#due-2026-03-15`, or bare `2026-03-15` (priority order)

## Development

```bash
cargo build              # compile (debug)
cargo build --release    # compile (optimized)
cargo test --lib         # run 90 unit tests
cargo check              # type-check only
cargo run --bin workflowy-mcp-server  # start server
```

## License

MIT
