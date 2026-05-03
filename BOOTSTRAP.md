# Bootstrap — for Claude

> Hand this file to Claude (Code, Desktop, or claude.ai) when you want to install
> the Workflowy MCP server **and** wire up a working second-brain workflow.

This file is a script for the assistant. It assumes the user has cloned this
repo and wants the assistant to do the rest. The assistant should follow each
step in order, run the listed checks, and stop on first failure.

The contract:

- **Repo content stays generic.** The template files in this repo never
  contain user-specific node IDs, drafts, or session data.
- **User-specific data lives outside the repo.** Everything user-shaped — node
  IDs, drafts, briefs, session logs — lands in the user's secondBrain
  directory at whatever path they choose. The path is exposed to the
  server through `$SECONDBRAIN_DIR`; the repo carries no opinion about
  where on disk it lives.
- **The wflow skill is the operating manual.** Once setup is complete, the
  assistant follows [`templates/skills/wflow/SKILL.md`](templates/skills/wflow/SKILL.md)
  for every Workflowy interaction.

If the user wants only the bare MCP server (no second-brain workflow), stop
after Step 2. Otherwise run the full sequence.

---

## Step 0 — Confirm prerequisites

Ask the user:

- Do they have a Workflowy account with API access enabled?
  (Workflowy → Settings → Integrations → API Token.)
- Do they have Rust installed? (`cargo --version` should print 1.70 or higher.)
- Which Claude surface(s) will they use? Claude Code, Claude Desktop, claude.ai,
  or several. The bootstrap differs slightly per surface (Step 4).

If the user wants only the MCP server, run Steps 1 and 2 then stop. Otherwise
continue through Step 6.

---

## Step 1 — Build the binary

```bash
cd ~/code/workflowy-mcp-server      # adjust if cloned elsewhere
cargo build --release
```

Confirm the binary exists:

```bash
ls -lh target/release/workflowy-mcp-server
```

**Check before continuing:** binary present, `cargo test --lib` reports all
tests pass.

---

## Step 2 — Wire up the MCP host

Ask the user for their Workflowy API key. Then write `.env` in the repo root:

```bash
echo "WORKFLOWY_API_KEY=<the key>" > .env
```

### Claude Code

```bash
claude mcp add workflowy -- $(pwd)/target/release/workflowy-mcp-server
```

After the entry is added, edit `~/.claude.json` and set the `env` block
on the `workflowy` server so the index path and secondBrain root agree
with Step 3:

```json
"env": {
  "WORKFLOWY_API_KEY": "<the key>",
  "SECONDBRAIN_DIR": "/absolute/path/to/secondBrain",
  "WORKFLOWY_INDEX_PATH": "/absolute/path/to/secondBrain/memory/name_index.json"
}
```

Verify with `claude mcp list` — the `workflowy` entry should report
`✓ Connected`.

### Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json`
(macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "/absolute/path/to/workflowy-mcp-server/target/release/workflowy-mcp-server",
      "env": {
        "WORKFLOWY_API_KEY": "<the key>",
        "SECONDBRAIN_DIR": "/absolute/path/to/secondBrain",
        "WORKFLOWY_INDEX_PATH": "/absolute/path/to/secondBrain/memory/name_index.json"
      }
    }
  }
}
```

Restart Claude Desktop.

Both env vars are optional — without them the server runs read-only-ish
(no persistent index, the `review` tool's bucket-d skipped). They become
mandatory the moment you do Step 3.

### Verify

Ask the host to call `workflowy_status`. The response should include
`status: "ok"`, `api_reachable: true`, and a non-zero `last_request_ms`.

If the user only wanted the bare MCP, stop here.

**Check before continuing:** `workflowy_status` returns healthy.

---

## Step 3 — Bootstrap the secondBrain directory

This directory holds **everything user-specific**: cached node IDs, drafts,
session logs, briefs. The template ships skeleton subdirectories only.

Ask the user where they want the directory to live (e.g.
`~/code/secondBrain`, a Dropbox folder, an iCloud or Google Drive folder
they want synced across machines). Export the path as `SECONDBRAIN_DIR`
in their shell profile so every tool that reads it agrees, then create
and seed the directory:

```bash
export SECONDBRAIN_DIR="<the path the user chose>"        # add to ~/.zshrc or ~/.bashrc too
mkdir -p "$SECONDBRAIN_DIR"
cp -R ~/code/workflowy-mcp-server/templates/secondbrain/* "$SECONDBRAIN_DIR/"
```

The resulting layout:

```
$SECONDBRAIN_DIR/
├── README.md                       ← copy of the template; describes each subdir
├── memory/
│   └── workflowy_node_links.md    ← cached node IDs (you fill in Step 4)
├── drafts/                         ← in-flight distillations awaiting execution
├── session-logs/                   ← per-session audit trail
└── briefs/                         ← documents for collaborators / future sessions
```

When wiring the MCP host (Step 2), make sure the `env` block also sets
`SECONDBRAIN_DIR` and `WORKFLOWY_INDEX_PATH=$SECONDBRAIN_DIR/memory/name_index.json`
so the server, `wflow-do` CLI, and review tool all agree on the path.

The persistent name index (`memory/name_index.json`) appears the first time
the MCP server checkpoints — usually within 30 seconds of the first read.

**Check before continuing:** the secondBrain directory exists with all
four subdirectories and a `README.md`. `memory/workflowy_node_links.md`
is the template (with `<TBD>` placeholders), not user data yet.

---

## Step 4 — Populate the user's structural node IDs

Walk the user through identifying their structural Workflowy nodes. None of
these are mandatory — only fill rows for nodes the user actually has.

| Node | What it is | Typical name |
|------|-----------|--------------|
| Tasks root | Where they keep todos | `📋 Tasks` |
| Inbox | Untriaged capture target | `Inbox` |
| Journal | Date-based daily entries | `Journal` |
| Reading List | Reading queue | `Reading List` |
| Resources | Long-term reference material | `Resources` |
| Distillations | Atomic-notes layer (optional) | `Distillations` |

For each, use `find_node` (with `parent_id` scoped to the workspace root) or
`search_nodes` to discover the full UUID, then write the row into
`$SECONDBRAIN_DIR/memory/workflowy_node_links.md`. Verify each ID with
`get_node`.

**Important:** every value in this file is the user's specific data. Never
copy any of it back into the repo or share it publicly.

**Check before continuing:** the file has at least Tasks, Inbox, and Journal
filled in, with verified UUIDs.

---

## Step 5 — Install the wflow skill (Claude Code) or paste it (Claude Desktop)

The wflow skill is the operating manual the assistant follows for every
Workflowy interaction in subsequent sessions. The template lives at
[`templates/skills/wflow/SKILL.md`](templates/skills/wflow/SKILL.md) and is
already generic — it reads user-specific node IDs from
`$SECONDBRAIN_DIR/memory/workflowy_node_links.md` at runtime.

### Claude Code

```bash
mkdir -p ~/.claude/skills/wflow
cp ~/code/workflowy-mcp-server/templates/skills/wflow/SKILL.md ~/.claude/skills/wflow/SKILL.md
```

Restart the Claude Code host or run `/skill list` to confirm `wflow` is
loaded.

### Claude Desktop

Open the user's Project, paste the contents of
`templates/skills/wflow/SKILL.md` into the Project's custom instructions.
The skill template reads user-specific node IDs at runtime through the
filesystem MCP, so a single paste covers every Project that touches the
same Workflowy graph.

### claude.ai

Upload `templates/skills/wflow/SKILL.md` as a Skill via Settings →
Capabilities. Note: the persistent name index is local-only — claude.ai is
best used as a read surface unless the MCP server is reachable as a remote
service.

**Check before continuing:** the skill is loaded on the user's primary
surface. Future sessions will reference it on first interaction.

---

## Step 6 — Optional: pre-warm the persistent index for large trees

For workspaces with more than ~12 k nodes, the lazy short-hash resolver
can't cover the full tree on a single 5-minute walk. Pre-warming the index
makes every URL the user pastes resolve O(1) thereafter.

```bash
~/code/workflowy-mcp-server/target/release/wflow-do reindex \
  --root <Tasks-UUID> \
  --root <Inbox-UUID> \
  --root <Reading-List-UUID> \
  --root <Distillations-UUID>
```

Each root walk is bounded by `RESOLVE_WALK_TIMEOUT_MS` (5 minutes by
default). The CLI writes back to whatever `$WORKFLOWY_INDEX_PATH` points
at (typically `$SECONDBRAIN_DIR/memory/name_index.json`); the running
MCP server picks the file up automatically as long as both processes
read the same env var.

For trees under 5 k nodes, skip this step — the lazy walk handles
everything in the first session.

**Check before continuing:** the file at `$WORKFLOWY_INDEX_PATH`
exists, is non-empty, and reports a node count comparable to the user's
expected workspace size.

---

## What to do if the assistant or user gets stuck

- **MCP shows offline / `workflowy_status` returns `degraded`.** Check
  `WORKFLOWY_API_KEY`, restart the MCP host, then call `health_check` to
  see whether it's an auth issue (`authenticated: false`) or a transient
  outage (`api_reachable: false` but `authenticated: true`).
- **`Short-hash X is not in the name index`.** The lazy walk should fire
  automatically. If it does not, confirm `$WORKFLOWY_INDEX_PATH` points
  at a writable location and that the MCP host has the env var set.
  For huge trees, re-run Step 6 with the appropriate `--root` UUIDs.
- **`insert_content` returns `status: "partial"`.** The bulk budget fired
  before the call completed. The response carries `created_count`,
  `last_inserted_id`, and `stopped_at_line`; resume by re-running with the
  remaining lines under `last_inserted_id` (or the original parent if the
  field is null).
- **More detailed troubleshooting:** see [`docs/SETUP.md`](docs/SETUP.md)
  for the long-form walkthrough, including multi-surface deployment
  guidance and convergence strategies for large trees.

---

## Hand-off

Once the bootstrap completes, the operating manual takes over. Subsequent
sessions should treat
[`templates/skills/wflow/SKILL.md`](templates/skills/wflow/SKILL.md) (or its
installed copy) as the source of truth for the user's second-brain
workflow. The wflow skill assumes the bootstrap has run; if a session finds
`$SECONDBRAIN_DIR/memory/workflowy_node_links.md` is still full of `<TBD>`
placeholders, restart this bootstrap from Step 4.
