# Setting up your Second Brain

This guide is written for an LLM (Claude, GPT, etc.) bootstrapping a fresh user onto the workflowy-mcp-server stack. Read it linearly. Each step has a check that determines whether you should proceed; do not skip checks.

The goal is a working installation in which:

1. The MCP server resolves Workflowy URLs and short-hash node IDs without manual setup.
2. The user has an `~/code/secondBrain/` directory holding their cached node IDs, drafts, session logs, and any briefs that don't belong inside Workflowy itself.
3. The wflow skill (or equivalent) is installed at `~/.claude/skills/wflow/` and reads from `~/code/secondBrain/`.

The repository ships generic templates only. The user's specific node IDs, session content, and briefs live in `~/code/secondBrain/` — never in this repo.

---

## Step 0 — Confirm prerequisites

Ask the user:

- Do they have a Workflowy account with API access enabled? (Settings → Integrations → API Token.)
- Do they want the second-brain workflow templated alongside the bare MCP, or just the MCP itself?

If only the MCP is wanted, complete Step 1 and stop. If they want the full second-brain workflow, continue through Step 5.

---

## Step 1 — Install and configure the MCP server

Build the binary:

```bash
git clone <this-repo> ~/code/workflowy-mcp-server
cd ~/code/workflowy-mcp-server
cargo build --release
```

Create a `.env` in the repo root:

```bash
WORKFLOWY_API_KEY=<the user's API token>
```

Optional environment variables:

| Variable | Default | Purpose |
|----------|---------|---------|
| `WORKFLOWY_INDEX_PATH` | `$HOME/code/secondBrain/memory/name_index.json` | Disk path for the persistent name index. Set to empty string to disable persistence. |
| `RUST_LOG` | `info` | Tracing filter (e.g. `debug`, `workflowy_mcp_server=trace`). |

Wire the binary into the user's MCP-host config (Claude Desktop, the IDE extension, etc.). The exact path varies by host — for Claude Desktop on macOS it is `~/Library/Application Support/Claude/claude_desktop_config.json`. The relevant block:

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "<absolute path to>/target/release/workflowy-mcp-server",
      "env": {
        "WORKFLOWY_API_KEY": "<token>"
      }
    }
  }
}
```

Restart the host after editing the config. Verify the connection by asking it to call `workflowy_status` — the result should report `healthy`.

**Check before continuing:** the MCP server connects, `workflowy_status` returns healthy, and a no-arg `health_check` call returns within 5 seconds.

---

## Step 2 — Bootstrap the secondBrain directory

If the user wants the full workflow, copy the templated layout into their home:

```bash
mkdir -p ~/code/secondBrain
cp -R ~/code/workflowy-mcp-server/templates/secondbrain/* ~/code/secondBrain/
```

The result has this layout:

```
~/code/secondBrain/
├── README.md                       ← describes what each subdir is for
├── memory/
│   └── workflowy_node_links.md    ← cached structural node IDs (you fill this)
├── drafts/                         ← in-flight distillations awaiting execution
├── session-logs/                   ← one markdown file per session that mutated Distillations
└── briefs/                         ← documents intended for collaborators / future Claude sessions
```

The persistent name index (`memory/name_index.json`) appears automatically the first time the MCP server checkpoints.

**Check before continuing:** all four subdirectories exist; the README.md template was copied in.

---

## Step 3 — Identify the user's structural Workflowy nodes

The cached node-ID file is what every workflow consults to avoid re-walking the tree to find Tasks, Inbox, Journal, etc. You populate it once; the wflow skill reads it on every bootstrap.

Walk the user through identifying their structural nodes. The expected categories (none of these are mandatory — only fill rows for nodes the user actually has):

- **Tasks root** — where the user puts their todos. Often called `📋 Tasks` or similar.
- **Inbox** — untriaged capture target.
- **Journal** — date-based daily entries.
- **Reading List** — reading queue.
- **Resources** — long-term reference material.
- **Distillations** (optional) — the layer for atomic notes synthesised from reading and conversation. Only relevant if the user follows the second-brain discipline described in `templates/skills/wflow/SKILL.md`.

For each one, use `find_node` (with `parent_id` scoped to root) or `search_nodes` to discover the full UUID, then write the row into `~/code/secondBrain/memory/workflowy_node_links.md`. The template at `templates/secondbrain/memory/workflowy_node_links.md` is the schema; replace the `<TBD>` placeholders.

**Important:** every value in this file is the user's specific data. Never check it into a public repo.

**Check before continuing:** the user's `workflowy_node_links.md` has at least Tasks, Inbox, and Journal filled in. Verify each ID by calling `get_node` on it.

---

## Step 4 — Install the wflow skill (optional, full second-brain only)

Copy the skill template into the user's Claude Code skills directory:

```bash
mkdir -p ~/.claude/skills/wflow
cp ~/code/workflowy-mcp-server/templates/skills/wflow/SKILL.md ~/.claude/skills/wflow/SKILL.md
```

The template is generic — it references `~/code/secondBrain/` paths and reads node IDs dynamically from `workflowy_node_links.md`. No user-specific node IDs live in the skill itself.

If the user wants their own customisations (extra workflows, project-specific routing rules), edit the copy at `~/.claude/skills/wflow/SKILL.md`. Treat that file as the user's, not the template's — pull updates from the repo template manually when desired.

**Check before continuing:** the skill is loaded — restart the Claude Code host or run `/skill list` to verify.

---

## Step 5 — Trigger an initial deep index pass (recommended)

The MCP server walks lazily when a short-hash misses its cache. On large trees (50 k+ nodes) the lazy walk can't cover everything in one shot, so paying the cost up front via the `wflow-do reindex` CLI is the cleanest setup.

First, identify the user's top-level subtree UUIDs (already cached in `~/code/secondBrain/memory/workflowy_node_links.md` from Step 3). Then:

```bash
~/code/workflowy-mcp-server/target/release/wflow-do reindex \
  --root <Tasks-UUID> \
  --root <Inbox-UUID> \
  --root <Reading-List-UUID> \
  --root <Distillations-UUID> \
  --root <any-other-major-subtree-UUIDs>
```

Each root walk is bounded by the resolution timeout (`RESOLVE_WALK_TIMEOUT_MS`, 5 minutes by default), so the full pass for a half-dozen roots runs in 5-25 minutes depending on tree shape. The CLI hydrates from the existing `~/code/secondBrain/memory/name_index.json`, walks each root, and saves the merged index back. Re-run the same command later to extend coverage; the persistent index makes the work cumulative.

For smaller trees (≤ 5 k nodes) you can also ask the MCP-driven assistant to run `build_name_index allow_root_scan=true` once — equivalent in effect, just routed through MCP rather than the CLI.

**Check before continuing:** `~/code/secondBrain/memory/name_index.json` exists and is non-empty. With the persistent index in place, every Workflowy URL the user pastes resolves cleanly. For nodes the index hasn't reached yet, the assistant can fall back to `node_at_path` (path of names → UUID in ~1 second) or `resolve_link` (URL + parent-path hint → full node info).

---

## Step 6 — Multi-surface deployment

The MCP server's persistent name index is the cross-session memory layer. It works automatically across every surface that calls the same binary — Claude Code, Claude Desktop, the IDE extension, anything else that speaks MCP. Each host spawns its own process, but they all read and write the same `~/code/secondBrain/memory/name_index.json`, so updates from one session land on disk via the 30-second checkpoint and are inherited by the next session on any surface.

The **skill markdown** (the wflow workflow itself) does not auto-port the same way. `~/.claude/skills/wflow/SKILL.md` is a Claude Code convention; other hosts don't auto-discover that path. Pick the approach that matches the user's surfaces:

### Claude Code (terminal, IDE extension)

Already covered by Step 4 — the skill at `~/.claude/skills/wflow/SKILL.md` is auto-discovered. No extra work.

### Claude Desktop (macOS / Windows app)

Two viable approaches, in order of preference:

**Option A — Project custom instructions.** Open a Project in the desktop app, paste the contents of `templates/skills/wflow/SKILL.md` into its custom instructions. The skill template reads user-specific node IDs at runtime from `~/code/secondBrain/memory/workflowy_node_links.md` via the filesystem MCP, so a single paste covers every project that uses the same Workflowy graph.

**Option B — Filesystem MCP read at session start.** If the host has the filesystem MCP allowlisting `~/code/secondBrain/` and `~/.claude/skills/wflow/`, the user can simply ask "read `~/.claude/skills/wflow/SKILL.md` and follow that workflow." No porting needed; the markdown is the canonical source. This works well if the user opens many short Claude Desktop sessions and doesn't want to maintain Project instructions.

Verify the workflowy MCP entry exists in `~/Library/Application Support/Claude/claude_desktop_config.json` and points at the release binary you built in Step 1. Restart Claude Desktop after edits — the host reads the config once at launch.

### claude.ai (web)

Upload `templates/skills/wflow/SKILL.md` as a Skill via Settings → Capabilities. Same content, different surface. The Workflowy MCP needs to be configured as a remote MCP (claude.ai's MCP support is web-friendly but distinct from desktop MCP); see Anthropic's claude.ai MCP documentation. The persistent index will not be shared with web sessions unless the MCP server is reachable as a remote service — for most users, claude.ai is best treated as a read-only retrieval surface (using the in-built Workflowy export or an HTTP MCP gateway) rather than a write surface.

### What stays in sync, what needs manual mirror

| Layer | Sync mechanism | User action when something changes |
|-------|----------------|-----------------------------------|
| Persistent name index (`name_index.json`) | Auto via disk; 30 s checkpoint | None |
| Cached structural IDs (`workflowy_node_links.md`) | Single file in `~/code/secondBrain/`; every surface reads the same copy | None — but update the file when a structural node moves |
| Drafts and session logs | Same — single canonical directory | Discipline: write back to `~/code/secondBrain/session-logs/` |
| Skill markdown (workflow logic) | **Manual** — repo template is upstream, each surface holds a cached copy | Re-paste / re-upload when the template changes substantively |

The cleanest mental model: `~/code/secondBrain/` is the canonical source of truth for cross-session state. The repo's `templates/skills/wflow/SKILL.md` is the canonical source for workflow logic. Each surface holds whatever cached copies are necessary; the data layer self-syncs, the markdown layer needs a manual mirror only when the upstream template changes.

---

## What goes where (summary)

| Where | Purpose | User-specific? |
|-------|---------|----------------|
| This repo | MCP server source, templates, setup docs | No |
| `~/code/secondBrain/memory/workflowy_node_links.md` | Cached structural node IDs | Yes |
| `~/code/secondBrain/memory/name_index.json` | Persistent name index (auto-managed) | Yes |
| `~/code/secondBrain/drafts/` | In-flight distillation drafts | Yes |
| `~/code/secondBrain/session-logs/` | Per-session audit trails | Yes |
| `~/code/secondBrain/briefs/` | Documents for collaborators | Yes |
| `~/.claude/skills/wflow/SKILL.md` | The wflow skill (copy of the template, then user-customised) | Becomes user's |

Anything user-specific must never be committed back into this repo.

---

## Troubleshooting

- **"Short-hash X is not in the name index" on a fresh install.** The auto-walk should fire automatically; if it does not, confirm the `WORKFLOWY_INDEX_PATH` env variable is unset or points to a writable path, and that the MCP host has permission to read/write `~/code/secondBrain/memory/`.
- **Server hangs on `tag_search`.** The deadline-honouring fix landed in commit `d378f56`. Confirm the binary is built from that commit or later.
- **`build_name_index` reports `truncation_reason: "timeout"`.** The tree is larger than a single 5-minute walk can cover; subsequent passes converge. The background refresher (every 30 minutes) accumulates coverage. For trees ≥ 50 k nodes, scope `build_name_index` to a specific subtree (`parent_id` = Projects, Areas, etc.) — indexing one subtree deeply is faster than indexing the whole root shallowly. Once a subtree is indexed, every short hash inside it resolves O(1) and survives restarts.

- **A short-hash lookup returns "did NOT cover" with low coverage percentage.** Your tree is much larger than the resolver's 5-minute budget. Workarounds, in order of effort: (a) call `build_name_index` with `parent_id` set to a region likely to contain the target — often Projects or whatever subtree the URL came from; (b) get the full UUID from the Workflowy app's URL bar and pass that instead of the short hash; (c) leave the server running — the 30-minute background refresher accumulates coverage over hours.
