# Workflowy MCP Server

A Rust MCP server that turns Workflowy into a working second brain — capture, triage, distillation, retrieval, and synthesis — driven by Claude (Code, Desktop, or web). The server gives any LLM read/write access to your Workflowy graph; the templates in this repo turn that raw access into a disciplined workflow that compounds across sessions.

You can use it three ways:

1. **As a plain MCP server** — Claude Desktop or Claude Code calls 26 tools against your Workflowy account. Skip the templates entirely.
2. **As a second brain** — install the `templates/secondbrain/` skeleton and the `templates/skills/wflow/SKILL.md` skill, and Claude follows a structured capture → triage → distillation → retrieval loop.
3. **As a starting point** — fork the templates, adjust the workflow categories to your taste, layer in your own conventions.

The methodology is opinionated; the server is not. Use whichever level suits you.

---

## Install

Prerequisites: [Rust toolchain](https://rustup.rs) (1.70+), a Workflowy account with [API access](https://workflowy.com/api-key), and Claude Desktop and/or Claude Code.

```bash
git clone https://github.com/dromologue/workflowyMCP.git ~/code/workflowy-mcp-server
cd ~/code/workflowy-mcp-server
cargo build --release
```

The binary lands at `target/release/workflowy-mcp-server`.

---

## Configure

Create `.env` in the repo root:

```bash
WORKFLOWY_API_KEY=<your-api-key>
```

Then wire the server into your MCP host.

### Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "/absolute/path/to/workflowy-mcp-server/target/release/workflowy-mcp-server",
      "cwd": "/absolute/path/to/workflowy-mcp-server"
    }
  }
}
```

Restart Claude Desktop. You can also pass credentials via `env` if you'd rather not rely on `cwd`.

### Claude Code

```bash
claude mcp add workflowy -- /absolute/path/to/workflowy-mcp-server/target/release/workflowy-mcp-server
```

Verify with `claude mcp list` — the entry should report `✓ Connected`.

That's it for plain MCP usage. To stop here, jump to [Tool reference](#tool-reference). To set up the second-brain workflow, continue.

---

## Set up the second brain

The repo ships generic templates so you (or an LLM bootstrapping you) can stand up a working second brain without inheriting the original author's specific node IDs. The detailed walk-through is in [docs/SETUP.md](docs/SETUP.md). The summary:

```bash
# 1. Create your operational secondBrain directory
mkdir -p ~/code/secondBrain
cp -R templates/secondbrain/* ~/code/secondBrain/

# 2. Install the wflow skill (Claude Code only — Claude Desktop uses Project instructions)
mkdir -p ~/.claude/skills/wflow
cp templates/skills/wflow/SKILL.md ~/.claude/skills/wflow/SKILL.md

# 3. Identify your structural Workflowy nodes (Tasks, Inbox, Journal, etc.)
#    and write the UUIDs into ~/code/secondBrain/memory/workflowy_node_links.md
#    The wflow skill will walk you through this on first use.
```

After Step 1 the MCP server's persistent name index begins checkpointing to `~/code/secondBrain/memory/name_index.json` automatically. After Step 3 your second brain has the node IDs it needs to operate; future sessions read this file at boot.

For Claude Desktop or claude.ai, paste `templates/skills/wflow/SKILL.md` into a Project's custom instructions or upload it as a Skill. Same content, different surface — see the multi-surface deployment section in [docs/SETUP.md](docs/SETUP.md).

---

## Methodology

The skill organises Claude's work into two halves.

**Operate** is the tempo of your working week. Daily prioritisation, weekly review, monthly themes, task capture, inbox triage, project status, reading-list management, journaling. These are short, frequent, conversational interactions: "what's on my plate today?", "capture this task", "triage my inbox."

**Synthesise** is the slower work of turning captured material into a graph that compounds. Distil a single source into atomic notes, batch-process the reading list, run cross-system research that pulls together everything you have on a topic. Synthesis writes back into a `Distillations` subtree (or wherever you keep your atomic notes), mirrored into pillars and themes.

Three habits hold it together:

- **Cached node IDs** at `~/code/secondBrain/memory/workflowy_node_links.md`. The wflow skill reads this on every bootstrap so it never has to re-walk the tree to find Tasks or Inbox.
- **Drafts before writes.** Any distillation involving more than a few mutations produces a markdown file in `~/code/secondBrain/drafts/` first. Protects against MCP wedges; gives you a chance to veto routing before the graph mutates.
- **One session log per session that touched the graph.** Both a Workflowy node (for navigation) and a local file at `~/code/secondBrain/session-logs/` (for audit). They should agree.

The repo's [templates/secondbrain/README.md](templates/secondbrain/README.md) covers the operational discipline in more depth.

---

## How short-hash resolution works

Workflowy URLs use a 12-character short hash (the trailing 12 hex of the UUID, like `c4ae1944b67e`). The Workflowy API requires the full UUID. The server resolves the gap automatically:

- A **persistent name index** at `~/code/secondBrain/memory/name_index.json` (override via `WORKFLOWY_INDEX_PATH`) caches every short hash → UUID it has ever seen. Survives restarts. Checkpointed every 30 seconds when dirty. Refreshed by a 30-minute background walk.
- On a cache miss, the resolver **walks the workspace synchronously** with a 5-minute budget. A watcher polls the index every 100 ms and cancels the walk as soon as the target appears, so found-early lookups don't pay the full timeout. The walk uses a per-call cancellation registry so it never tears down concurrent indexing work.

For most personal Workflowy graphs (under ~40 k nodes), the very first short-hash you paste resolves within a minute and everything after is O(1) against the persistent index.

### Large-tree convergence (~50 k nodes and up)

A 5-minute walk indexes roughly 12 k nodes at the Workflowy API's 5 RPS sustained rate. If your workspace is bigger than that, no single walk covers the full tree. The system is designed to **converge over time**:

1. The 30-minute background refresher keeps walking, accumulating coverage in the persistent index.
2. Foreground misses still trigger an immediate walk to maximise the chance of an in-session resolution.
3. When a miss returns truncated, the error message reports `nodes_walked / tree_size_estimate` so you know the gap.

Practical recovery for huge trees: scope explicitly. `build_name_index` accepts a `parent_id`, so you can index a known subtree (Projects, Areas, Reading List) deeply in one short walk rather than relying on root-walk breadth. Once a subtree is indexed, every short hash within it resolves O(1) thereafter and survives restarts via the persistent index.

---

## Tool reference

The server exposes 26 tools. Most operations work in terms of `node_id`, which accepts any of: full UUID (with or without hyphens), 12-char URL-suffix short hash, or 8-char prefix used in docs.

**Search & navigate:** `search_nodes`, `find_node`, `get_node`, `list_children`, `tag_search`, `get_subtree`, `find_backlinks`, `path_of`, `find_by_tag_and_path`.

**Create & edit:** `create_node`, `batch_create_nodes`, `insert_content`, `smart_insert`, `convert_markdown`, `edit_node`, `move_node`, `delete_node`, `duplicate_node`, `create_from_template`, `bulk_update`, `bulk_tag`, `transaction`, `export_subtree`.

**Todos & scheduling:** `list_todos`, `list_upcoming`, `list_overdue`, `daily_review`, `since`.

**Project management:** `get_project_summary`, `get_recent_changes`.

**Diagnostics & ops:** `workflowy_status`, `health_check`, `cancel_all`, `build_name_index`, `audit_mirrors`, `review`, `get_recent_tool_calls`.

Conventions parsed from node text:

- Tags: `#inbox`, `#review`, `#urgent`
- Assignees: `@alice`, `@bob`
- Due dates: `due:2026-03-15`, `#due-2026-03-15`, or bare `2026-03-15` (priority order)

Large-tree behaviour: subtree fetches cap at 10 000 nodes and a 20-second wall-clock budget; every response includes `truncated` plus a `truncation_reason`. Use `parent_id` / `max_depth` to scope down. `health_check` is a sub-second liveness probe safe to use as a circuit breaker.

---

## CLI: wflow-do

A second binary, `wflow-do`, exposes the same operations as a plain shell command. Useful as a fallback for transport-layer drops in the MCP layer — Bash dispatch is independent of MCP dispatch.

```bash
cargo build --release --bin wflow-do

# Examples
target/release/wflow-do status                              # liveness + rate-limit snapshot
target/release/wflow-do search --query "concept maps"
target/release/wflow-do --dry-run delete <uuid>             # plan-mode preview
target/release/wflow-do review --days-stale 90              # what's worth re-reading
target/release/wflow-do audit-mirrors                       # canonical_of:/mirror_of: drift
```

Subcommands: `status`, `get`, `children`, `create`, `move`, `delete`, `edit`, `search`, `audit-mirrors`, `review`, `index`. Use `--json` for raw output, `--dry-run` (write verbs only) to preview without calling the API.

---

## Development

```bash
cargo build              # debug
cargo build --release    # optimised
cargo test --lib         # 242 unit tests
cargo check              # type-check only
```

[CLAUDE.md](CLAUDE.md) has the architectural overview; [specs/specification.md](specs/specification.md) is the authoritative behavioural spec.

---

## License

MIT
