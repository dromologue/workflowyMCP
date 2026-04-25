# Workflowy MCP Server

A Rust MCP server that gives Claude full read/write access to your Workflowy outline. Search, insert, organize, manage tasks, and track deadlines — all through natural language.

Works with **Claude Desktop** and **Claude Code** as an MCP server.

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

Restart Claude Desktop after saving. The 26 tools listed below become available immediately.

**Large trees (100k+ nodes):** Search and review tools use a `max_depth` parameter (default: 3–5) to avoid fetching the entire tree. Subtree fetches also cap at 10 000 nodes **and** at a 20-second wall-clock budget; every tool response includes a `truncated` flag with a `truncation_reason` (`node_limit`, `timeout`, or `cancelled`) when either fires, so you can narrow with `parent_id`/`root_id` or reduce `max_depth`. `duplicate_node`, `create_from_template`, and `bulk_update` (delete) refuse to run against a truncated view to avoid partial copies or partial deletes. Use `health_check` as a sub-second liveness probe before kicking off larger calls, `cancel_all` to abort in-flight walks, and `build_name_index` to populate the cached name index so `find_node` with `use_index=true` answers without hitting the API.

### 3. Claude Code

Register the MCP server with Claude Code so its 26 tools appear in every session:

```bash
claude mcp add workflowy -- /absolute/path/to/workflowyMCP/target/release/workflowy-mcp-server
```

Verify with `claude mcp list` — the entry should report `✓ Connected`.

## Usage

Ask Claude naturally — it will use the MCP tools:

- "Search my Workflowy for anything tagged #review"
- "What's overdue in my Projects?"
- "Add a task to Office: Review Q2 budget"
- "Give me a daily review of my tasks"

## Tools (26 implemented)

### Search & Navigate

| Tool | What it does |
|------|-------------|
| `search_nodes` | Text search in node names and descriptions |
| `find_node` | Look up a node by name (exact, contains, or starts_with match modes) |
| `get_node` | Get a node by ID, plus a depth-1 listing of its direct children |
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
| `bulk_update` | Apply `delete`, `add_tag`, or `remove_tag` to filtered nodes (with `dry_run` mode). `complete` / `uncomplete` are not yet supported. |

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

### Diagnostics & Ops

| Tool | What it does |
|------|-------------|
| `health_check` | Sub-second liveness probe: confirms API reachability and reports cache + name-index sizes |
| `cancel_all` | Cancels every in-flight tree walk; outstanding calls return partial results with `truncation_reason = "cancelled"` |
| `build_name_index` | Walks a subtree to populate the opportunistic name index, enabling `find_node` with `use_index=true` to answer without hitting the API |

## CLI: wflow-do

A second binary, `wflow-do`, exposes the same `WorkflowyClient` operations as a plain shell command. Use it as a workaround for transport-layer drops in Claude Desktop's MCP layer — Bash dispatch is independent of MCP tool dispatch, so shelling out from any Claude session reaches the API even when the MCP transport is silently dropping calls.

Build it alongside the server:

```bash
cargo build --release --bin wflow-do
```

The binary lives at `target/release/wflow-do`. It reads `WORKFLOWY_API_KEY` from the environment or `.env` (same loader as the MCP server). Example session:

```bash
# liveness + rate-limit snapshot
wflow-do status

# create a node, capture the UUID for use downstream
NODE=$(wflow-do create --name "Triage" | tail -n1)

# move it under a known parent, then delete when done
wflow-do move "$NODE" --to <parent-uuid>
wflow-do delete "$NODE"
```

Available subcommands: `status`, `get`, `children`, `create`, `move`, `delete`, `edit`, `search`. Add `--json` for raw JSON on every command, `--quiet` to suppress info-level logging. On error the binary writes `<command>: <message> [<proximate_cause>]` to stderr and exits 1; the proximate-cause taxonomy matches the MCP server's `tool_error` classification.

## Conventions

Tags, assignees, and due dates are parsed from node text:

- **Tags:** `#inbox`, `#review`, `#urgent`
- **Assignees:** `@alice`, `@bob`
- **Due dates:** `due:2026-03-15`, `#due-2026-03-15`, or bare `2026-03-15` (priority order)

## Development

```bash
cargo build              # compile (debug)
cargo build --release    # compile (optimized)
cargo test --lib         # run 159 unit tests
cargo check              # type-check only
cargo run --bin workflowy-mcp-server  # start server
```

## License

MIT
