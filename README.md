# Workflowy MCP Server

**Give Claude a second brain, built on the outliner you already trust.**

This is a Rust MCP server that connects Claude — Desktop, Code, or claude.ai —
to your Workflowy workspace, plus a complete, opinionated method for turning
that connection into a working second brain: capture, triage, retrieval,
distillation, and review, all driven by conversation.

Say *"capture this as a task under Projects"* mid-conversation and it lands in
the right place, tagged and dated. Ask *"what do I have on organisational
design?"* and get an answer assembled from your own notes in about a second,
without walking your tree. Paste any Workflowy link and Claude knows exactly
which node you mean. Run a morning review — overdue items, due-soon, what
changed yesterday — as a single question.

Two ways in:

1. **Bare MCP server.** Wire the binary into your MCP host and use the 45
   tools directly. No templates, no opinions.
2. **Second brain.** Hand [`BOOTSTRAP.md`](BOOTSTRAP.md) to Claude and let it
   run the install: build, wire the host, seed your private data directory,
   cache your structural node IDs, install the wflow skill that drives every
   later session. Steps 1–4 are a ten-minute job.

Your data stays yours: the repo ships only generic templates, and everything
personal — node IDs, drafts, session logs, the search index — lives at a path
you choose, outside the repo, on your machine.

---

## Why this one

Most Workflowy integrations are demos. This one has been hardened in daily
production use against a 250,000-node workspace, and every hard-won lesson is
encoded in the code and pinned by a test — over 500 of them, including a
suite of build-time invariant tests that make the design rules unbreakable by
future contributors.

Concretely, that means the problems you would otherwise hit in week two have
already been hit, diagnosed, and engineered away:

- **Rate limits don't ruin your session.** The client fails fast inside a
  429 window instead of hanging for four minutes, adapts its request rate
  when the API pushes back (halve on 429, creep back on success), and never
  fires a burst into a freshly-reset quota. Bulk writes that stop early
  always tell you exactly what landed and how to resume.
- **Big trees don't time out your questions.** A persistent name index turns
  names, tags, backlinks, and Workflowy URLs into answers in O(1) from a
  local file — no tree walk, no API calls. Searches fall back to live,
  scoped walks only when the index can't answer, and every truncated result
  says so honestly, with a recovery hint.
- **Nothing fails silently.** Every walk reports its coverage, every error
  carries a typed cause (`rate_limited`, `timeout`, `auth`, …) with a
  retry-ability flag, every write is auditable in an operation log, and
  deletes support a name-echo guard so a coerced ID can't take out the wrong
  node.
- **Repeat reads are nearly free.** Complete children listings are cached
  with write-through invalidation, node payloads serialise sparse, and
  overlapping queries collapse to single API calls.

The same logic serves two surfaces: the MCP server for conversation, and a
`wflow-do` CLI with full parity for scripts, cron jobs, and the terminal —
enforced at build time, so the two can never drift apart.

---

## Quick install (five minutes)

You need Rust 1.75+ (`rustup install stable`), a Workflowy API key
(Workflowy → Settings → API), and an MCP host (Claude Code or Claude
Desktop).

```bash
git clone https://github.com/dromologue/workflowyMCP.git ~/code/workflowy-mcp-server
cd ~/code/workflowy-mcp-server
cargo build --release
echo "WORKFLOWY_API_KEY=<your-token>" > .env
```

Wire `target/release/workflowy-mcp-server` into your host:

- **Claude Code:** `claude mcp add workflowy -- $(pwd)/target/release/workflowy-mcp-server`
- **Claude Desktop:** edit
  `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS)
  or `%APPDATA%\Claude\claude_desktop_config.json` (Windows). See
  [BOOTSTRAP.md](BOOTSTRAP.md) for the JSON shape.

Verify by asking Claude to call `workflowy_status` — you want
`status: "ok"`, `api_reachable: true`, `authenticated: true`. Then try it:

> "List the children of my workspace root."
> "Create a node called *Read later* under my Inbox."
> "What did I change in the last two days?"

That's the bare server working. The `.env` file covers a binary launched from
the repo directory; putting the same key in the host's `env` block (next
section) works from anywhere and is the recommended form.

## Make it a second brain (recommended)

Hand [`BOOTSTRAP.md`](BOOTSTRAP.md) to Claude. It walks the seven steps:
build, wire the host, seed your private `$SECONDBRAIN_DIR`, cache your
structural node IDs (Inbox, Tasks, Journal…), install the wflow skill,
pre-warm the search index for large trees, and verify the whole chain
end-to-end. After that, every session opens with your workflows available
conversationally: daily and weekly reviews, task capture, inbox triage,
reading-list management, distillation of sources into atomic notes, mirror
discipline with drift auditing, and cross-note research.

The long-form walkthrough (multi-surface setups, large-tree convergence,
troubleshooting) is in [`docs/SETUP.md`](docs/SETUP.md). Running behind a
remote connector for claude.ai web/mobile is covered in
[`docs/REMOTE-CONNECTOR.md`](docs/REMOTE-CONNECTOR.md).

---

## Environment variables

The server reads five env vars at runtime. The repository ships no
machine-specific defaults: a path or node ID you don't set is a feature you
don't use. Set them in the `env` block of your MCP host config (Claude Code:
`~/.claude.json`; Claude Desktop: `claude_desktop_config.json`) and,
when you also use the `wflow-do` CLI from a shell, in your shell
profile (`~/.zshrc` or `~/.bashrc`).

| Variable | Required? | What it controls |
|----------|-----------|------------------|
| `WORKFLOWY_API_KEY` | Yes | Bearer token for the Workflowy API. |
| `SECONDBRAIN_DIR` | Optional | Absolute path to your operational secondBrain directory (drafts, session logs, briefs, memory). When set, the `review` tool's bucket-d session-log scan and the `wflow-do index` default output path read from `$SECONDBRAIN_DIR/session-logs/`. Unset or empty disables those features (graceful skip). |
| `WORKFLOWY_INDEX_PATH` | Optional | Absolute path to the persistent name-index JSON. Conventionally `$SECONDBRAIN_DIR/memory/name_index.json`. Unset or empty disables persistence — the index then lives only in memory for the lifetime of each process. |
| `WORKFLOWY_REVIEW_ROOT` | Optional | Default root node for the `review` and `audit_mirrors` tools when `root_id` is omitted (your review-anchor / "Distillations" node). No hardcoded fallback — if unset, those two tools require an explicit `root_id`. |
| `WORKFLOWY_INDEX_EXCLUDE_SUBTREES` | Optional | Comma-separated full UUIDs and/or 12-char short hashes whose subtrees must never be **written to the persistent index file**. Walks may still traverse them in memory (a live session still needs answers), but the on-disk index — a durable artefact other tools read — never carries them. Exclusion is transitive (root + all descendants); malformed tokens are dropped with a warning. Set this for any subtree holding material you don't want in a local file. |

Example MCP host `env` block (Claude Code or Desktop):

```json
"env": {
  "WORKFLOWY_API_KEY": "<your token>",
  "SECONDBRAIN_DIR": "/absolute/path/to/secondBrain",
  "WORKFLOWY_INDEX_PATH": "/absolute/path/to/secondBrain/memory/name_index.json",
  "WORKFLOWY_REVIEW_ROOT": "<your review-anchor node id, optional>"
}
```

Example shell profile (so the CLI agrees with the MCP server):

```bash
export SECONDBRAIN_DIR="/absolute/path/to/secondBrain"
export WORKFLOWY_INDEX_PATH="$SECONDBRAIN_DIR/memory/name_index.json"
```

Neither path needs to be inside your home directory — a Dropbox / iCloud /
Google Drive folder works as long as the host process can read and write it.
**Set the vars in the host config, not only your shell profile** — the server
process inherits its environment from the host's launch, and a var visible
only to your interactive shell silently disables the features it drives.

---

## The tool surface

45 tools. `node_id` accepts a full UUID (with or without hyphens), the
12-char short hash from any Workflowy URL, or the 8-char doc prefix — paste
whatever you have.

| Category | Tools |
|----------|-------|
| Search & navigate | `node_at_path`, `resolve_link`, `search_nodes`, `find_node`, `get_node`, `list_children`, `tag_search`, `get_subtree`, `find_backlinks`, `path_of`, `find_by_tag_and_path`, `read_batch` |
| Create & edit | `create_node`, `batch_create_nodes`, `insert_content`, `smart_insert`, `convert_markdown`, `edit_node`, `move_node`, `reorder_nodes`, `delete_node`, `complete_node`, `duplicate_node`, `create_from_template`, `bulk_update`, `bulk_tag`, `transaction`, `export_subtree` |
| Mirror discipline | `create_mirror` (convention-based `mirror_of:` linking), `audit_mirrors` (finds broken and drifted mirrors) |
| Todos & scheduling | `list_todos`, `list_upcoming`, `list_overdue`, `daily_review`, `since` |
| Project management | `get_project_summary`, `get_recent_changes` |
| Diagnostics & ops | `workflowy_status`, `health_check`, `cancel_all`, `build_name_index`, `review`, `get_recent_tool_calls` |

Highlights worth knowing before you need them:

- **Index-first retrieval.** `search_nodes` and `find_node` take
  `prefer_index=true` — answer from the local index when it can, fall back
  to a live scoped walk when it can't, one call either way. `tag_search`
  and `find_backlinks` take `use_index=true` for zero-API-call sweeps.
  The index matches names *and* descriptions, token-AND, any order.
- **Reads that survive awkward hosts.** `read_batch` runs many reads in one
  call with bounded concurrency and per-operation status — the reliable
  shape on hosts that mangle single-ID parameters.
- **Writes that can't land in the wrong place.** The write tools require an
  explicit `parent_id` (empty string means workspace root), every scoped
  response echoes `scope_resolved` so you can verify the target, and
  `delete_node` accepts an `expect_name` guard that refuses a delete when
  the resolved node's name doesn't match.
- **Batches that resume.** `insert_content` reports a committed-count
  cursor on *every* failure, so a rate-limited batch resumes exactly where
  it stopped, with no double-writes. `transaction` rolls back on failure.
- **Ordering that matches the app.** Listings sort into Workflowy display
  order; `insert_content` writes explicit ascending priorities;
  `reorder_nodes` is the deterministic reorder primitive.

Conventions parsed from node text: tags (`#project`), assignees (`@alice`),
due dates (`due:2026-03-15`, `#due-2026-03-15`, or a bare date).

---

## Reliability, in numbers

Every API-touching handler runs inside a uniform wrapper with a
kind-appropriate wall-clock budget, cancellation support, and an op-log
entry — a call can time out, but it cannot vanish:

| Tool kind | Budget | Examples |
|-----------|--------|----------|
| Read | 30 s | `get_node`, `list_children` |
| Write | 15 s | `create_node`, `delete_node`, `edit_node` |
| Bulk | 180 s | `insert_content`, `transaction`, `bulk_update` |
| Walk | 20 s (internal) | `search_nodes`, `get_subtree`, `find_node` |

`cancel_all` interrupts anything in flight within ~50 ms. Every walk-shaped
response carries a four-field truncation envelope (`truncated`,
`truncation_limit`, `truncation_reason`, `truncation_recovery_hint`) so a
partial answer is never mistaken for a complete one. Every error carries a
typed envelope — `proximate_cause`, `retryable`, `retry_after_secs`, a
hint — so the right recovery is explicit rather than guessed. The full
behavioural contract, including 21 wiremock-driven failure-mode tests and
the build-time invariant suite, lives in
[`specs/specification.md`](specs/specification.md) with a machine-checked
[traceability matrix](specs/traceability.md) mapping every contract to the
test that pins it.

---

## The CLI: `wflow-do`

Everything the MCP server does, as a shell command — full surface parity,
enforced at build time. Use it for scheduled jobs, shell pipelines, and as a
fallback when you'd rather not open a chat window.

```bash
wflow-do status                                      # liveness
wflow-do search --query "concept maps" --use-index   # zero-API-call search
wflow-do find "Tasks" --use-index                    # O(1) name lookup
wflow-do backlinks <uuid> --use-index                # who links here?
wflow-do changed-since 2026-07-14 --root <uuid>      # local incremental diff
wflow-do complete <uuid>                             # mark done
wflow-do bulk-update complete --tag urgent           # bulk-toggle by filter
wflow-do --dry-run delete <uuid>                     # preview first
wflow-do reindex --timeout-secs 0 --patient --root <uuid>   # coverage-complete index build
```

Forty-two subcommands, `--json` for raw output, `--dry-run` on write verbs.
The nightly `reindex --patient` job is the convergence mechanism for the
search index: it waits out rate-limit windows instead of dropping branches,
and its work is cumulative — every walk any tool performs extends the same
persistent file.

---

## What ships in this repo

```text
workflowyMCP/
├── BOOTSTRAP.md              ← LLM-facing install script (hand to Claude)
├── README.md                 ← this file
├── docs/SETUP.md             ← long-form setup walkthrough
├── docs/REMOTE-CONNECTOR.md  ← claude.ai custom-connector notes
├── specs/                    ← behavioural spec, principles, traceability
├── templates/
│   ├── secondbrain/          ← skeleton copied to $SECONDBRAIN_DIR
│   └── skills/wflow/SKILL.md ← the operating manual the assistant follows
├── dist/wflow.skill.zip      ← ready-to-upload skill bundle for claude.ai
└── src/                      ← Rust MCP server + wflow-do CLI
```

Everything specific to you — cached node IDs, your pillars and routing
rules, drafts, session logs, the name index — lives at `$SECONDBRAIN_DIR`
and `$WORKFLOWY_INDEX_PATH`, never in the repo. Clone it and you get a clean
starting point; so does the next person.

| File you'll create | Lives at | What it holds |
|------|----------|---------------|
| `workflowy_node_links.md` | `$SECONDBRAIN_DIR/memory/` | Cached UUIDs for your structural nodes (Inbox, Tasks, Reading List…) plus the triage-sources table. |
| `distillation_taxonomy.md` | `$SECONDBRAIN_DIR/memory/` | Your pillars, themes, and routing rules for the synthesis workflows. |
| `name_index.json` | `$WORKFLOWY_INDEX_PATH` | Auto-managed persistent search index. Survives restarts; checkpoints every 30 s; grows with every walk and converges via the scheduled `reindex --patient` job. |
| `drafts/`, `session-logs/`, `briefs/` | `$SECONDBRAIN_DIR/` | In-flight work, per-session audit trails, handoff documents. |

---

## Development

```bash
cargo build --release    # optimised build (server + CLI)
cargo test --lib         # 500+ unit tests, no live API calls
cargo test               # full suite: lib + portability + traceability + eval coverage
```

The architecture guide is [CLAUDE.md](CLAUDE.md); the law of the project —
eight core principles, a definition of done, and a conflict-resolution
hierarchy — is [`specs/constitution.md`](specs/constitution.md). Every
consistency rule worth stating is pinned by a test that fails the build when
violated. Contributions are held to the same standard, which is precisely
why you can build on this without reading the whole source first.

## Licence

MIT
