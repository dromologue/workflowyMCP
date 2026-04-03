# Workflowy MCP Server

A Rust MCP server that gives Claude full read/write access to your Workflowy outline. Search, insert, organize, manage tasks, and track deadlines — all through natural language.

Works with **Claude Desktop** (MCP tools) and **Claude Code** (CLI + skills).

## Prerequisites

- [Rust toolchain](https://rustup.rs) (1.70+)
- A Workflowy account with API access
- Claude Desktop and/or Claude Code

## Install

```bash
git clone https://github.com/dromologue/workflowyMCP.git
cd workflowyMCP
cargo build --release
```

The compiled binary is at `target/release/workflowy-mcp-server`.

## Configure

### 1. Workflowy API key

Get your API key from [workflowy.com/api-key](https://workflowy.com/api-key), then create `.env` in the project root:

```
WORKFLOWY_API_KEY=your-api-key
```

### 2. Claude Desktop (MCP server)

Add to your Claude Desktop config:

- **macOS:** `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Windows:** `%APPDATA%\Claude\claude_desktop_config.json`

**Option A — `.env` file (recommended):** Set `cwd` so the server finds `.env` automatically:

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "/absolute/path/to/workflowyMCP/target/release/workflowy-mcp-server",
      "cwd": "/absolute/path/to/workflowyMCP"
    }
  }
}
```

**Option B — inline credentials:**

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "/absolute/path/to/workflowyMCP/target/release/workflowy-mcp-server",
      "env": {
        "WORKFLOWY_API_KEY": "your-api-key"
      }
    }
  }
}
```

Restart Claude Desktop after saving. The 23 tools listed below become available immediately.

**Large trees (100k+ nodes):** Search and review tools use a `max_depth` parameter (default: 3–5) to avoid fetching the entire tree. Increase `max_depth` for deeper searches, or use `parent_id`/`root_id` to scope operations to a subtree.

### 3. Claude Code (skill)

The repo includes a `/wmanage` skill for day-to-day Workflowy management directly from Claude Code.

**Install the skill** by registering the MCP server with Claude Code:

```bash
# From the project directory
claude mcp add workflowy -- ./target/release/workflowy-mcp-server
```

Or, if you prefer to point at the absolute path:

```bash
claude mcp add workflowy -- /absolute/path/to/workflowyMCP/target/release/workflowy-mcp-server
```

The `/wmanage` skill is automatically available when Claude Code is run from the project directory (it lives at `.claude/skills/wmanage/SKILL.md`). To use it from other directories, copy the `.claude/skills/wmanage/` folder into that project's `.claude/skills/` directory.

## Usage

### Via Claude Desktop (MCP tools)

Ask Claude naturally — it will use the MCP tools:

- "Search my Workflowy for anything tagged #review"
- "What's overdue in my Projects?"
- "Add a task to Office: Review Q2 budget"
- "Give me a daily review of my tasks"

### Via Claude Code (`/wmanage` skill)

The `/wmanage` skill provides structured commands for daily work management:

```
/wmanage              # Daily prioritisation (default)
/wmanage daily        # Same as above
/wmanage weekly       # Weekly review and planning
/wmanage monthly      # Monthly themes and goals
/wmanage capture      # Quick task capture (e.g. /wmanage capture office Review Q2 budget)
/wmanage status       # Project status overview
/wmanage triage       # Process Inbox items interactively
/wmanage reading      # Prioritise and summarise reading list items
/wmanage journal      # Daily journal check-in
```

**Prioritisation cascade:** Monthly priorities inform weekly planning, which informs daily focus. Run `/wmanage monthly` first to establish your priorities, then use `/wmanage weekly` and `/wmanage daily` for ongoing planning.

**Expected Workflowy structure:** The skill expects top-level nodes named Tasks, Inbox, Tags, Resources (with a Links sub-node), and Journal. It discovers domains (Office, Home, etc.) dynamically as children of the Tasks node.

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

## Conventions

Tags, assignees, and due dates are parsed from node text:

- **Tags:** `#inbox`, `#review`, `#urgent`
- **Assignees:** `@alice`, `@bob`
- **Due dates:** `due:2026-03-15`, `#due-2026-03-15`, or bare `2026-03-15` (priority order)

## Development

```bash
cargo build              # compile (debug)
cargo build --release    # compile (optimized)
cargo test --lib         # run 106 unit tests
cargo check              # type-check only
cargo run --bin workflowy-mcp-server  # start server
```

## License

MIT
