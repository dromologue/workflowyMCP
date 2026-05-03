# Workflowy MCP Server

A Rust MCP server that gives Claude (Code, Desktop, or web) read/write access
to your Workflowy graph, plus a generic template for turning that raw access
into a working second brain.

There are two ways to use it:

1. **Bare MCP server.** Wire the binary into your MCP host and call the 26
   tools directly. No templates, no opinions.
2. **Second brain.** Hand [`BOOTSTRAP.md`](BOOTSTRAP.md) to Claude. The
   assistant follows the script: builds the binary, wires the host, sets up
   your secondBrain directory at whatever path you choose (exposed to the
   server via `$SECONDBRAIN_DIR`), and installs the wflow skill that
   drives every subsequent session.

The methodology in option 2 is opinionated; the server itself is not. The
repo only ships generic templates — your node IDs, drafts, and session logs
live wherever `$SECONDBRAIN_DIR` points, never in this repo.

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

## Environment variables

The server reads three env vars at runtime. The repository ships no
machine-specific defaults: a path you don't set is a feature you don't
use. Set them in the `env` block of your MCP host config (Claude Code:
`~/.claude.json`; Claude Desktop: `claude_desktop_config.json`) and,
when you also use the `wflow-do` CLI from a shell, in your shell
profile (`~/.zshrc` or `~/.bashrc`).

| Variable | Required? | What it controls |
|----------|-----------|------------------|
| `WORKFLOWY_API_KEY` | Yes | Bearer token for the Workflowy API. |
| `SECONDBRAIN_DIR` | Optional | Absolute path to your operational secondBrain directory (drafts, session logs, briefs, memory). When set, the `review` tool's bucket-d session-log scan and the `wflow-do index` default output path read from `$SECONDBRAIN_DIR/session-logs/`. Unset or empty disables those features (graceful skip). |
| `WORKFLOWY_INDEX_PATH` | Optional | Absolute path to the persistent name-index JSON. Conventionally `$SECONDBRAIN_DIR/memory/name_index.json`. Unset or empty disables persistence — the index then lives only in memory for the lifetime of each process. |

Example MCP host `env` block (Claude Code or Desktop):

```json
"env": {
  "WORKFLOWY_API_KEY": "<your token>",
  "SECONDBRAIN_DIR": "/absolute/path/to/secondBrain",
  "WORKFLOWY_INDEX_PATH": "/absolute/path/to/secondBrain/memory/name_index.json"
}
```

Example shell profile (so the CLI agrees with the MCP server):

```bash
export SECONDBRAIN_DIR="/absolute/path/to/secondBrain"
export WORKFLOWY_INDEX_PATH="$SECONDBRAIN_DIR/memory/name_index.json"
```

Neither path needs to be inside the user's home directory — a Dropbox /
iCloud / Google Drive folder works as long as the host process can
read and write it. Paths with spaces are fine in the MCP `env` block
(JSON quoting handles it) and in the shell profile (the export line
quotes the value).

---

## Set up the second brain (recommended)

Hand [`BOOTSTRAP.md`](BOOTSTRAP.md) to Claude. The assistant runs through
six steps: build, wire the host, bootstrap your secondBrain directory at
the path you set in `$SECONDBRAIN_DIR`, populate your structural node IDs,
install the wflow skill, and (optionally) pre-warm the persistent name
index. After bootstrap, the assistant follows
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
│   ├── secondbrain/          ← skeleton copied to $SECONDBRAIN_DIR
│   │   ├── README.md
│   │   ├── memory/workflowy_node_links.md   (template; user fills in)
│   │   ├── drafts/  session-logs/  briefs/
│   └── skills/wflow/SKILL.md ← the operating manual the assistant follows
└── src/                       ← Rust MCP server source
```

User-specific data (node IDs, drafts, session logs, briefs) belongs at
whatever path you set via `$SECONDBRAIN_DIR`. The repo content stays
generic so the next person who clones it gets a clean starting point.

---

## Tool reference

The server exposes 40 tools. `node_id` accepts any of: full UUID (with or
without hyphens), 12-char URL-suffix short hash, or 8-char prefix.

| Category | Tools |
|----------|-------|
| Search & navigate | `node_at_path`, `resolve_link`, `search_nodes`, `find_node`, `get_node`, `list_children`, `tag_search`, `get_subtree`, `find_backlinks`, `path_of`, `find_by_tag_and_path` |
| Create & edit | `create_node`, `batch_create_nodes`, `insert_content`, `smart_insert`, `convert_markdown`, `edit_node`, `move_node`, `delete_node`, `complete_node`, `duplicate_node`, `create_from_template`, `bulk_update`, `bulk_tag`, `transaction`, `export_subtree` |
| Todos & scheduling | `list_todos`, `list_upcoming`, `list_overdue`, `daily_review`, `since` |
| Project management | `get_project_summary`, `get_recent_changes` |
| Diagnostics & ops | `workflowy_status`, `health_check`, `cancel_all`, `build_name_index`, `audit_mirrors`, `review`, `get_recent_tool_calls` |

**Native task completion.** `complete_node(node_id)` toggles the
Workflowy `completed` boolean — the legacy `#done` tag-as-completion
workaround is deprecated for tasks. `bulk_update(operation: "complete"|"uncomplete", filter: …)`
toggles a filtered set in one call; `transaction` accepts the same ops
with rollback. Wire payload is `POST /nodes/{id}` with
`{"completed": true|false}`.

**Truncation envelope.** Every walk-shaped tool that emits JSON includes
the same four fields when its 20 s walk budget fires:

```json
{
  "truncated": true,
  "truncation_limit": 10000,
  "truncation_reason": "timeout",
  "truncation_recovery_hint": "Call build_name_index(parent_id=...) … then re-issue with use_index=true …"
}
```

Read `truncation_reason` and `truncation_recovery_hint` on every walk
response. For name-based queries (`search_nodes`, `find_node`),
`use_index=true` answers in O(1) from the persistent name index without
burning the walk budget — populate it first with
`build_name_index(parent_id=<scope>)`. Index path is name-only;
description-content matching still needs a live walk.

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
[`specs/specification.md`](specs/specification.md). 296 lib + 12 CLI
tests pin the contracts, including 21 wiremock-driven failure-mode tests
that run in under 2 seconds, plus four build-time invariant tests:

- `parameter_bearing_tools_publish_non_empty_input_schema_properties`
  fails the build if a tool's published schema has empty `properties`
  (the rmcp `Parameters<T>` wrapper rename trap).
- `every_walk_tool_emits_full_truncation_envelope_in_json` fails the
  build if a walk-shaped tool emits `truncation_limit` without the
  reason + recovery_hint companions.
- `cli_covers_every_non_diagnostic_mcp_tool` fails the build if a new
  MCP tool ships without its matching `wflow-do` subcommand.
- `cancel_all_preempts_inflight_create_node_via_run_handler` and the
  `path_of` companion pin the cancel-registry safety net.

---

## CLI: `wflow-do`

A second binary exposes the same operations as a plain shell command.
**Full surface parity** with the MCP server — every non-diagnostic tool
has a matching subcommand, enforced at build time. Useful as a fallback
when the MCP transport drops or when you want a Bash-driven workflow.

```bash
target/release/wflow-do status                              # liveness
target/release/wflow-do search --query "concept maps"       # substring filter
target/release/wflow-do find "Tasks" --use-index            # O(1) index lookup
target/release/wflow-do complete <uuid>                     # mark task done
target/release/wflow-do bulk-update complete --tag urgent   # bulk-toggle by filter
target/release/wflow-do --dry-run delete <uuid>             # preview
target/release/wflow-do reindex --root <UUID> --root <UUID> # pre-warm index
```

Forty subcommands grouped: read & navigate (`status`, `health-check`,
`get`, `children`, `subtree`, `find`, `search`, `tag-search`,
`backlinks`, `find-by-tag-and-path`, `node-at-path`, `path-of`,
`resolve-link`, `since`); todos & scheduling (`todos`, `overdue`,
`upcoming`, `daily-review`, `recent-changes`, `project-summary`);
single-node writes (`create`, `move`, `delete`, `edit`, `complete`);
bulk writes (`insert`, `smart-insert`, `duplicate`, `template`,
`bulk-update`, `bulk-tag`, `batch-create`, `transaction`, `export`);
graph hygiene (`audit-mirrors`, `review`, `index`, `reindex`,
`build-name-index`); diagnostics (`cancel-all`, `recent-tools`).

Use `--json` for raw output, `--dry-run` (write verbs only) to preview
without calling the API.

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
