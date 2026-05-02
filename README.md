# Workflowy MCP Server

A Rust MCP server that gives Claude (Code, Desktop, or web) read/write access
to your Workflowy graph, plus a generic template for turning that raw access
into a working second brain.

There are two ways to use it:

1. **Bare MCP server.** Wire the binary into your MCP host and call the 26
   tools directly. No templates, no opinions.
2. **Second brain.** Hand [`BOOTSTRAP.md`](BOOTSTRAP.md) to Claude. The
   assistant follows the script: builds the binary, wires the host, sets up
   `~/code/secondBrain/` for your specific data, and installs the wflow
   skill that drives every subsequent session.

The methodology in option 2 is opinionated; the server itself is not. The
repo only ships generic templates — your node IDs, drafts, and session logs
live in `~/code/secondBrain/`, never in this repo.

---

## Quick install

```bash
git clone https://github.com/dromologue/workflowyMCP.git ~/code/workflowy-mcp-server
cd ~/code/workflowy-mcp-server
cargo build --release
echo "WORKFLOWY_API_KEY=<your-token>" > .env
```

Wire the resulting `target/release/workflowy-mcp-server` into your MCP host:

- **Claude Code:** `claude mcp add workflowy -- $(pwd)/target/release/workflowy-mcp-server`
- **Claude Desktop:** edit
  `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS)
  or `%APPDATA%\Claude\claude_desktop_config.json` (Windows). See
  [BOOTSTRAP.md](BOOTSTRAP.md) for the JSON shape.

Verify with `workflowy_status` — the host should return
`status: "ok"`, `api_reachable: true`.

That's it for plain MCP usage.

---

## Set up the second brain (recommended)

Hand [`BOOTSTRAP.md`](BOOTSTRAP.md) to Claude. The assistant runs through
six steps: build, wire the host, bootstrap `~/code/secondBrain/`, populate
your structural node IDs, install the wflow skill, and (optionally)
pre-warm the persistent name index. After bootstrap, the assistant follows
[`templates/skills/wflow/SKILL.md`](templates/skills/wflow/SKILL.md) as the
operating manual.

The detailed long-form walkthrough — multi-surface deployment, large-tree
convergence, troubleshooting — lives in [`docs/SETUP.md`](docs/SETUP.md).

---

## What ships in this repo

```
workflowyMCP/
├── BOOTSTRAP.md              ← LLM-facing install script (hand to Claude)
├── README.md                 ← this file
├── docs/SETUP.md             ← long-form bootstrap notes
├── specs/specification.md    ← authoritative behavioural spec
├── templates/
│   ├── secondbrain/          ← skeleton of ~/code/secondBrain/
│   │   ├── README.md
│   │   ├── memory/workflowy_node_links.md   (template; user fills in)
│   │   ├── drafts/  session-logs/  briefs/
│   └── skills/wflow/SKILL.md ← the operating manual the assistant follows
└── src/                       ← Rust MCP server source
```

User-specific data (node IDs, drafts, session logs, briefs) belongs in
`~/code/secondBrain/`. The repo content stays generic so the next person
who clones it gets a clean starting point.

---

## Tool reference

The server exposes 26 tools. `node_id` accepts any of: full UUID (with or
without hyphens), 12-char URL-suffix short hash, or 8-char prefix.

| Category | Tools |
|----------|-------|
| Search & navigate | `node_at_path`, `resolve_link`, `search_nodes`, `find_node`, `get_node`, `list_children`, `tag_search`, `get_subtree`, `find_backlinks`, `path_of`, `find_by_tag_and_path` |
| Create & edit | `create_node`, `batch_create_nodes`, `insert_content`, `smart_insert`, `convert_markdown`, `edit_node`, `move_node`, `delete_node`, `duplicate_node`, `create_from_template`, `bulk_update`, `bulk_tag`, `transaction`, `export_subtree` |
| Todos & scheduling | `list_todos`, `list_upcoming`, `list_overdue`, `daily_review`, `since` |
| Project management | `get_project_summary`, `get_recent_changes` |
| Diagnostics & ops | `workflowy_status`, `health_check`, `cancel_all`, `build_name_index`, `audit_mirrors`, `review`, `get_recent_tool_calls` |

For large workspaces, prefer `node_at_path` (path of names → UUID, ~1 second
on any tree size) and `resolve_link` (Workflowy URL + optional parent path
→ full node info) over `search_nodes`. They cost O(depth) API calls instead
of O(tree).

Conventions parsed from node text:

- Tags: `#inbox`, `#review`, `#urgent`
- Assignees: `@alice`, `@bob`
- Due dates: `due:2026-03-15`, `#due-2026-03-15`, or bare `2026-03-15`
  (priority order)

---

## Reliability properties

Every API-touching handler runs inside a uniform `run_handler` wrapper
that observes the server-wide cancel registry and applies a
kind-appropriate wall-clock deadline:

| Tool kind | Budget | Examples |
|-----------|--------|----------|
| Read | 30 s | `get_node`, `list_children` |
| Write | 15 s | `create_node`, `delete_node`, `edit_node` |
| Bulk | 210 s | `insert_content`, `transaction`, `bulk_update`, `path_of`, `node_at_path` |
| Walk | 20 s (internal) | `search_nodes`, `get_subtree`, `find_node` |

`cancel_all` interrupts any in-flight tool within ~50 ms. On budget
expiry, bulk operations return a structured partial-success payload
(`status: "partial"`, `created_count`, `last_inserted_id`, etc.) so the
caller can resume — no "no result received" without diagnostic.

For the full list (transport-timeout retry, `authenticated`/`api_reachable`
decoupling, `null` parameter handling, etc.) see
[`specs/specification.md`](specs/specification.md). 272 lib tests pin the
contracts, including 21 wiremock-driven failure-mode tests that run in
under 2 seconds.

---

## CLI: `wflow-do`

A second binary exposes the same operations as a plain shell command.
Useful as a fallback when the MCP transport drops.

```bash
target/release/wflow-do status                              # liveness
target/release/wflow-do search --query "concept maps"
target/release/wflow-do --dry-run delete <uuid>             # preview
target/release/wflow-do reindex --root <UUID> --root <UUID> # pre-warm index
```

Subcommands: `status`, `get`, `children`, `create`, `move`, `delete`,
`edit`, `search`, `audit-mirrors`, `review`, `index`, `reindex`. Use
`--json` for raw output, `--dry-run` (write verbs only) to preview without
calling the API.

---

## Development

```bash
cargo build              # debug
cargo build --release    # optimised
cargo test --lib         # 272 unit tests
cargo check              # type-check only
```

Architectural overview: [CLAUDE.md](CLAUDE.md). Behavioural spec:
[specs/specification.md](specs/specification.md).

---

## License

MIT
