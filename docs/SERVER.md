# The server, the CLI, and how they behave

This is the engineering half of the repository, kept apart from
[`../README.md`](../README.md) so the method there is not buried under it. Read
this when you are installing, operating, or extending the software. Nothing
here is needed to practise the method, and if you are choosing between
surfaces read [`SURFACES.md`](SURFACES.md) first: on a laptop most daily
reading and writing is better done by WorkFlowy's own tools than by this.

---

## Why this implementation

Most Workflowy integrations are demos. This one has been hardened in daily
production use against a 250,000-node workspace, and every hard-won lesson is
encoded in the code and pinned by a test: over 500 of them, including
build-time invariant tests that make the design rules unbreakable by future
contributors.

Concretely, the problems you would otherwise hit in week two have already been
hit, diagnosed, and engineered away:

- **Rate limits don't ruin your session.** The client fails fast inside a
  429 window instead of hanging for four minutes, adapts its request rate
  when the API pushes back (halve on 429, creep back on success), and never
  fires a burst into a freshly-reset quota. Bulk writes that stop early
  always tell you exactly what landed and how to resume.
- **Big trees don't time out your questions.** A persistent name index turns
  names, tags, backlinks, and Workflowy URLs into answers in O(1) from a
  local file, with no tree walk and no API calls. Searches fall back to live,
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
- **The whole tree in one call.** The search index rebuilds from Workflowy's
  bulk `GET /nodes-export` endpoint: the entire workspace in a single request,
  seconds not minutes, with no level-by-level walk, no truncation, and no 429
  storm. `wflow-do reindex --full-export` is the nightly path; the
  coverage-complete `--patient` walk remains for scoped rebuilds.

The same logic serves both surfaces, the MCP server for conversation and the
`wflow-do` CLI for scripts and cron, with parity enforced at build time so the
two can never drift apart.

---

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
| `WORKFLOWY_INDEX_PATH` | Optional | Absolute path to the persistent name-index JSON. Conventionally `$SECONDBRAIN_DIR/memory/name_index.json`. Unset or empty disables persistence, so the index lives only in memory for the lifetime of each process. |
| `WORKFLOWY_USAGE_LOG_DIR` | Optional | Directory for a durable per-call usage log (`{ts, surface, tool, ok, ms, cause}` JSONL, one file per day). Lets you measure this server's load, for instance against WorkFlowy's official desktop MCP. Unset disables it. |
| `WORKFLOWY_REVIEW_ROOT` | Optional | Default root node for the `review` and `audit_mirrors` tools when `root_id` is omitted (your review-anchor / "Distillations" node). No hardcoded fallback: if unset, those two tools require an explicit `root_id`. |
| `WORKFLOWY_INDEX_EXCLUDE_SUBTREES` | Optional | Comma-separated full UUIDs and/or 12-char short hashes whose subtrees must never be **written to the persistent index file**. Walks may still traverse them in memory (a live session still needs answers), but the on-disk index, a durable artefact other tools read, never carries them. Exclusion is transitive (root + all descendants); malformed tokens are dropped with a warning. Set this for any subtree holding material you don't want in a local file. |

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

Neither path needs to be inside your home directory; a Dropbox / iCloud /
Google Drive folder works as long as the host process can read and write it.
**Set the vars in the host config, not only your shell profile**, because the server
process inherits its environment from the host's launch, and a var visible
only to your interactive shell silently disables the features it drives.

---

---

## The tool surface

45 tools. `node_id` accepts a full UUID (with or without hyphens), the
12-char short hash from any Workflowy URL, or the 8-char doc prefix; paste
whatever you have.

Read this table alongside [`SURFACES.md`](SURFACES.md) rather than as
a shopping list. On a machine that can reach WorkFlowy's own tools, most of the
search-and-navigate row and much of the create-and-edit row is better served
natively, at no API cost. The rows that have no native equivalent are mirror
discipline, `review`, and the diagnostics; those, plus headless and remote
operation, are the reason to run this at all.

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
  `prefer_index=true`: answer from the local index when it can, fall back
  to a live scoped walk when it can't, one call either way. `tag_search`
  and `find_backlinks` take `use_index=true` for zero-API-call sweeps.
  The index matches names *and* descriptions, token-AND, any order.
- **Reads that survive awkward hosts.** `read_batch` runs many reads in one
  call with bounded concurrency and per-operation status, the reliable
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
- **Clean text in, rich nodes out.** Since the 2026.01 API parses markdown
  in a node's name on write (so stored names carry `<b>`/`<a>`/`<time>`
  markup), reads render back to clean display text (links keep their URL,
  dates unwrap to their label) and search matches the visible text, not the
  tags. `create_node` also takes an explicit `layout` (`todo`/`h1`/`h2`/`h3`/
  `code-block`/`quote-block`) so you can build headers and checklists
  directly.

Conventions parsed from node text: tags (`#project`), assignees (`@alice`),
due dates (`due:2026-03-15`, `#due-2026-03-15`, or a bare date).

---

---

## Reliability, in numbers

Every API-touching handler runs inside a uniform wrapper with a
kind-appropriate wall-clock budget, cancellation support, and an op-log
entry: a call can time out, but it cannot vanish:

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
typed envelope: `proximate_cause`, `retryable`, `retry_after_secs`, a
hint, so the right recovery is explicit rather than guessed. The full
behavioural contract, including 21 wiremock-driven failure-mode tests and
the build-time invariant suite, lives in
[`specs/specification.md`](../specs/specification.md) with a machine-checked
[traceability matrix](../specs/traceability.md) mapping every contract to the
test that pins it.

---

---

## The CLI: `wflow-do`

Everything the MCP server does, as a shell command, with full surface parity
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
wflow-do reindex --full-export                       # whole tree in one bulk call
wflow-do reindex --timeout-secs 0 --patient --root <uuid>   # coverage-complete scoped build
```

Forty-two subcommands, `--json` for raw output, `--dry-run` on write verbs.
The nightly reindex rebuilds the whole index from one bulk `/nodes-export`
call (`--full-export`); the `--patient` walk is the convergence mechanism for
*scoped* rebuilds, waiting out rate-limit windows instead of dropping
branches. Either way the work is cumulative: every walk any tool performs
extends the same persistent file.

---

---

## Development

```bash
cargo build --release    # optimised build (server + CLI)
cargo test --lib         # 500+ unit tests, no live API calls
cargo test               # full suite: lib + portability + traceability + eval coverage
```

The architecture guide is [CLAUDE.md](../CLAUDE.md); the law of the project,
eight core principles, a definition of done, and a conflict-resolution
hierarchy, is [`specs/constitution.md`](../specs/constitution.md). Every
consistency rule worth stating is pinned by a test that fails the build when
violated. Contributions are held to the same standard, which is precisely
why you can build on this without reading the whole source first.
