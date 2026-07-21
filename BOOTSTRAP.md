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

**The 10-minute path.** Steps 1–4 are the minimal viable second brain:
build, wire the host, seed the directory, cache the structural node IDs.
Step 5 (skill install) takes over the first real session; Step 6 (index
pre-warm) matters for trees over ~12 k nodes and multi-surface setups.
If the user's tree is small and they run a single Claude surface, do
Steps 1–5 and skip Step 6 — the lazy walk covers the rest.

---

## Step 0 — Confirm prerequisites

Ask the user:

- Do they have a Workflowy account with API access enabled?
  (Workflowy → Settings → Integrations → API Token.)
- Do they have Rust installed? (`cargo --version` should print 1.70 or higher.)
- Which Claude surface(s) will they use? Claude Code, Claude Desktop, claude.ai,
  or several. The bootstrap differs slightly per surface (Step 4).

If the user wants only the MCP server, run Steps 1 and 2 then stop. Otherwise
continue through Step 7.

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

**Read this before wiring anything.** The env vars in this step must be set
in the **MCP host's config** (`env` block), not only in the user's shell
profile. The MCP server process inherits its environment from the host's
launch, not from the interactive shell — a var exported only in `.zshrc`
is invisible to the server, and the failure is silent: the persistent
index quietly runs in-memory-only and every short-hash resolve pays
cold-start latency. Set the vars in **both** places (host config for the
server, shell profile for the `wflow-do` CLI) in the same session, and
verify with `workflowy_status` — its `name_index.persistence` block
reports `configured: true` plus the resolved path when the server can
see the var.

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

Two further optional vars belong in the same block when the user wants
them: `WORKFLOWY_REVIEW_ROOT` (default root for the `review` and
`audit_mirrors` tools — picked in Step 4) and
`WORKFLOWY_INDEX_EXCLUDE_SUBTREES` (comma-separated UUIDs or 12-char
short hashes of subtrees that must never be written to the on-disk
index — set this if any subtree holds material the user does not want
in a durable local file).

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

While the user is choosing structural nodes, also ask: **which node is
their review anchor** — the subtree the `review` and `audit_mirrors`
tools should default to (typically Distillations or the root of their
knowledge area). Set it as `WORKFLOWY_REVIEW_ROOT` in the MCP host's
`env` block (Step 2). Without it, those two tools require an explicit
`root_id` on every call and will error on first casual use.

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

claude.ai's Settings → Skills upload expects a zip with a top-level
`<skill-name>/` directory wrapper, not a bare SKILL.md. Use the bundler
shipped with this repo:

```bash
scripts/bundle-skill.sh                                 # bundles ~/.claude/skills/wflow → dist/wflow.skill.zip
scripts/bundle-skill.sh --src templates/skills/wflow    # bundle the generic template directly (first-run, no live skill yet)
```

The bundler validates the SKILL.md frontmatter at bundle time —
descriptions over the 1024-char cap and `<` / `>` characters anywhere
in the YAML block are refused there rather than at upload (claude.ai's
upload validator rejects XML-like tags with no diagnostic). Upload the
resulting zip via Settings → Skills, then **start a fresh claude.ai
session**: skills are mounted at session-start and an in-flight thread
keeps its initial mount.

Note: the persistent name index is local-only — claude.ai is best used
as a read surface unless the MCP server is reachable as a remote service.

**Check before continuing:** the skill is loaded on the user's primary
surface. Future sessions will reference it on first interaction.

---

## Step 6 — Optional: pre-warm the persistent index for large trees

For workspaces with more than ~12 k nodes, the lazy short-hash resolver
can't cover the full tree on a single 20-second walk (its budget,
`RESOLVE_WALK_TIMEOUT_MS`, is deliberately short — exhaustive coverage
belongs to this step, not to interactive calls). Pre-warming the index
makes every URL the user pastes resolve O(1) thereafter.

Run the pre-warm **patient and unbudgeted** — this is the
coverage-complete mode; the flags matter:

```bash
~/code/workflowy-mcp-server/target/release/wflow-do reindex \
  --timeout-secs 0 --patient \
  --root <Tasks-UUID> \
  --root <Inbox-UUID> \
  --root <Reading-List-UUID> \
  --root <Distillations-UUID>
```

`--patient` retries rate-limited branches in waves instead of dropping
them, and `--timeout-secs 0` removes the per-root deadline so patience
can actually complete (a patient walk on a budget quietly stops being
patient; the CLI warns about the combination). Without these flags the
walk uses the interactive 20-second default and a large tree gets a
silently partial index. A full pass over a large workspace can take
minutes per root — that is the point. Re-run the same command any time
to extend coverage; the index merges on save, so the work is cumulative.

The CLI writes back to whatever `$WORKFLOWY_INDEX_PATH` points
at (typically `$SECONDBRAIN_DIR/memory/name_index.json`); the running
MCP server picks the file up automatically as long as both processes
read the same env var. Consider scheduling this same command nightly
(cron / launchd) — the server has no in-process background refresher,
so the scheduled reindex is what keeps coverage converged.

For trees under 5 k nodes, skip this step — the lazy walk handles
everything in the first session.

**Check before continuing:** the file at `$WORKFLOWY_INDEX_PATH`
exists, is non-empty, and reports a node count comparable to the user's
expected workspace size.

---

## Step 7 — Verify the whole chain

Before declaring the bootstrap done, prove one representative call of
each kind works end-to-end on the user's primary surface:

1. `workflowy_status` → `status: "ok"`, and `name_index.persistence`
   reports `configured: true` with the expected path.
2. `node_at_path` (or `find_node`) against one of the structural nodes
   cached in Step 4 → returns the UUID recorded in
   `workflowy_node_links.md`.
3. Paste one Workflowy URL from the user's tree → `resolve_link`
   returns the node (after Step 6, without a walk).
4. One trivial write-and-verify: `create_node` under Inbox, `get_node`
   it back, `delete_node` it (pass `expect_name`).

If any of the four fails, fix it now using the troubleshooting section
below — a bootstrap that ends on a broken chain costs the user their
first real session.

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
- **Index runs in-memory only after restart / cold-start latency on every
  short-hash resolve.** Likely the dual-config gap — the env var is set
  in `.zshrc` but not in the MCP host config (Claude Desktop's
  `claude_desktop_config.json` `mcpServers.workflowy.env`, Claude Code's
  `~/.claude.json`). The MCP server process inherits its env from the
  host's launch, not from the user's interactive shell. Diagnose with
  `workflowy_status` — its `name_index.persistence` block reports
  `configured: false` and a `warning` string when the env var is unseen,
  and `configured: true` with the resolved path when it is. Fix by
  copying `WORKFLOWY_INDEX_PATH` from `.zshrc` into the host config's
  `env` block, then restart the host.
- **`invalid type: null, expected a string` or `literal "null" is not a
  valid UUID`.** The host serialised an unresolved binding as JSON null
  or the literal four-char string `"null"` for a UUID-typed field. The
  error path names the offending field (e.g. `invalid parameters at
  \`.new_parent_id\`: ...`); resolve the actual UUID first and retry.
  Both forms are rejected at the deserialiser; the previous silent-
  routing-to-context behaviour is gone.
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
