---
name: wflow
description: Integrated second-brain skill built on Workflowy plus any additional services the user has configured (declared in $SECONDBRAIN_DIR/memory/services.md). Capture, triage, distillation, retrieval, and synthesis. Triggered conversationally. Use when the user wants to plan their day, capture a task, triage their inbox, distil a source into atomic notes, journal, research a topic across their notes, or run a periodic review.
allowed-tools: Read, Write, Edit, Bash, Glob, Grep, AskUserQuestion, WebFetch, mcp__workflowy__workflowy_status, mcp__workflowy__health_check, mcp__workflowy__get_node, mcp__workflowy__find_node, mcp__workflowy__search_nodes, mcp__workflowy__list_children, mcp__workflowy__get_subtree, mcp__workflowy__create_node, mcp__workflowy__edit_node, mcp__workflowy__delete_node, mcp__workflowy__move_node, mcp__workflowy__insert_content, mcp__workflowy__smart_insert, mcp__workflowy__daily_review, mcp__workflowy__get_recent_changes, mcp__workflowy__list_overdue, mcp__workflowy__list_upcoming, mcp__workflowy__list_todos, mcp__workflowy__get_project_summary, mcp__workflowy__tag_search, mcp__workflowy__bulk_update, mcp__workflowy__find_backlinks
# Additional service tool namespaces (e.g. mcp__<service>__*) are listed in $SECONDBRAIN_DIR/memory/services.md and must be added to allowed-tools when configured.
---

# wflow — second-brain skill (template)

This is the generic skill template shipped by the workflowy-mcp-server repo. It deliberately contains **no user-specific node IDs** — those live in `$SECONDBRAIN_DIR/memory/workflowy_node_links.md`. On first use, walk the user through populating that file (see Bootstrap below).

The skill spans the full second-brain loop:

1. **Capture** — tasks, links, ink, reading material.
2. **Triage and prioritisation** — daily / weekly / monthly cascade.
3. **Synthesis** — distillation of reading and conversation into atomic notes.
4. **Retrieval** — graph queries across Workflowy and any additional services the user has configured.

It is invoked **conversationally** — the user does not need to type slash commands. Match by intent, not by exact wording.

## How the skill is invoked

| User intent | Workflow |
|---|---|
| "Plan today" / "what's on my plate" / morning review | Daily prioritisation |
| "Weekly review" / "where did the week go" | Weekly prioritisation |
| "Monthly review" / "set monthly themes" | Monthly prioritisation |
| "Capture X" / "remind me to" / "add X to my todos" | Task capture |
| "What's the status" / "where am I" | Project status |
| "Triage my inbox" / "process my inbox" | Inbox triage |
| "What's in my reading list" / "review reading" | Reading list management |
| "Journal" / "reflect on today" | Journal check-in |
| "Distil this" / "summarise this for the second brain" | Distil single source |
| "What do I have on X" / "trace my thinking on X" | Cross-system research |

When the intent is ambiguous, ask one clarifying question rather than guessing.

---

## Server contracts the workflows depend on

Four contracts the workflowy-mcp server ships — re-read this section if behaviour stops matching what the workflows describe; the routing decisions below assume them. Any additional service the user has configured (per `$SECONDBRAIN_DIR/memory/services.md`) ships its own contracts; document those alongside the service entry, not here.

1. **`complete_node` is the native completion path; `bulk_update` accepts `complete` / `uncomplete`.** The legacy `#done` tag-as-completion-marker is deprecated for tasks (`#done` on reading-list entries to mark "I've distilled this source" remains a separate convention). Workflowy's wire field is `note` for descriptions and `completed` for the boolean.
2. **`Parameters<T>` is the wrapper name on every tool's input.** If parameter-bearing calls suddenly silently misroute (every call acts as if you sent no arguments, only `workflowy_status` works), the server has regressed the wrapper name and the cowork client is validating against an empty schema. Recovery: route through `wflow-do` until the server is rebuilt.
3. **`use_index=true` is the recovery for walk-budget timeouts on name queries.** `find_node` and `search_nodes` answer from the persistent name index in O(1) with no walk budget. Index is name-only — descriptions need a live walk. Populate via `build_name_index(parent_id=<scope>)` once per fresh session or whenever the index is sparse.
4. **Every walk-shaped tool emits the same JSON-truncation envelope** (`truncated`, `truncation_limit`, `truncation_reason`, `truncation_recovery_hint`). Read these on every walk response — don't silently accept partial results.
The CLI fallback (`wflow-do`) is in full surface parity with the MCP — every non-diagnostic tool has a matching subcommand. Drift fails the build, so the fallback path is always available when transport drops.

---

## UUID Parameter Discipline (read before EVERY write call)

Most preventable write-failure mode. Before any tool call that takes a UUID parameter (`move_node`, `edit_node`, `delete_node`, `complete_node`, `get_node`, `parent_id` on `create_node`, `new_parent_id` on `move_node`, every parameter typed `NodeId`), run this check — every time, no exceptions:

1. **Have the UUID string in front of you.** Full UUID (`550e8400-e29b-41d4-a716-446655440000`), 12-char URL-suffix short hash, or 8-char doc-prefix short hash. If you don't have it, resolve first via `node_at_path` / `resolve_link` / `find_node` / `list_children` or read it from `$SECONDBRAIN_DIR/memory/workflowy_node_links.md`. Don't make the write call yet.

2. **NEVER write the literal string `null` between parameter tags for a required UUID.** Some host surfaces (claude.ai web/mobile in particular) have a tendency to emit `null` when the UUID isn't immediately to hand. The MCP server has no fallback that resolves `null` to a previously-touched UUID — the call gets rejected, or worse, lands on the wrong node. If you catch yourself about to type `null`, stop. Re-read the last few tool results; the UUID exists somewhere.

3. **If the error reads `invalid parameters at \`.<field>\`: invalid type: null, expected a string`** — you typed `null` for `<field>`. Don't apologise and retry with `null`. Find the actual UUID for that field, then retry. The error names the field on purpose.

4. **Path-less version of the error** (`invalid type: null, expected a string` with no `at \`.<field>\``) means the running MCP binary is pre-2026-05-03. Restart Claude Desktop / re-launch the host process to pick up the path-aware deserializer.

5. **Workaround for surfaces that persistently strip bare-string UUIDs to `null`:** route writes through a tool whose parameters are an `operations` array — `transaction(operations=[{op: "move", node_id: "<uuid>", new_parent_id: "<uuid>"}, ...])` for multi-write batches, `batch_create_nodes(operations=[...])` for multi-creates. The UUIDs sit inside nested array items, dodging the bare-top-level-string encoding bug. Trade-off: one rollback unit per transaction. Last resort: `wflow-do` CLI, which bypasses host-side encoding entirely.

---

## System overview

The user has up to four complementary layers:

1. **Workflowy** — system of record and second-brain wiki. Holds tasks, projects, references, the journal, and (optionally) a Distillations subtree. Required.
2. **Additional services** *(optional — most users will skip this layer)* — declared in `$SECONDBRAIN_DIR/memory/services.md` if and only if the user wants extra surfaces. Each entry names a service (e.g. an ink-capture device, a document store, a reading-queue API), its MCP namespace, the workflows it participates in, and how to health-check it. The skill is service-agnostic; the file's absence is normal, not a fault.
3. **Claude** — bidirectional reader and writer.
4. **secondBrain directory** (`$SECONDBRAIN_DIR/`) — the operational outside. Holds drafts, session logs, the cached node-ID memory file, the services configuration, and external-facing briefs.

The discipline that turns this into a wiki rather than a notebook is **writing synthesis back**. Sessions that produce a useful summary, comparison, or framework should end with atomic notes saved into Workflowy (under a Distillations subtree if the user follows that pattern), mirrored into the right pillar/theme, and a session log entry written both to Workflowy and to `$SECONDBRAIN_DIR/session-logs/`.

---

## Bootstrap (run BEFORE every workflow)

A three-step probe at session start. All steps must complete before any workflow proceeds.

### Step 0 — Tool availability probe

Confirm the MCP tool surface this skill needs is actually loaded **before** doing anything else. claude.ai connectors can be disabled, removed, or fail to load silently; the skill must fail loud rather than silently degrade to filesystem-staging.

#### What to probe

- **`workflowy:*`** — required for every workflow.
- **`Filesystem:*`** — required to read drafts and memory files (skip in Claude Code, which uses native `Read`/`Write`/`Bash`).
- **Each additional service** *(only if the user has any)*. Check whether `$SECONDBRAIN_DIR/memory/services.md` exists; if it does, read it once at session start and probe each service's `bootstrap_probe` tool. Skip a service whose `bootstrap_probe` is `none`. Skip services whose `participates_in` doesn't intersect the workflow you're about to run. **If the file doesn't exist, skip this step entirely** — many users will run with Workflowy-only and that's a fully supported configuration.

#### How to probe

Use the **exact server name** as the `tool_search` query — descriptive phrases match the wrong connector (`"filesystem write file"` loads Netlify, `"list_allowed_directories"` loads Gmail, only `"Filesystem"` works):

- `tool_search(query="Workflowy")`
- `tool_search(query="Filesystem")`
- For each additional service: `tool_search(query="<exact server name from services.md>")`.

Verify each surface with a read-only call:

- `workflowy:health_check()` — `status: "ok"`, `authenticated: true`.
- `Filesystem:list_allowed_directories()` — includes the user's SecondBrain path.
- For each additional service whose probe is queued: invoke the tool name listed in its `bootstrap_probe` field. Treat anything other than a healthy response as a fail-loud condition.

#### Fail-loud protocol

If any required tool is unreachable:

1. Stop the bootstrap. Do not proceed to Step 1.
2. Name the missing tool and tell the user how to fix it: claude.ai connectors at `https://claude.ai/settings/connectors`, or Claude Desktop's `claude_desktop_config.json`. Confirm with the corresponding health-check tool.
3. Ask explicitly before staging a draft to disk. Never silently degrade. If the user consents, write to the universally allowlisted fallback path with the failure mode tagged in the filename (`...-mcp-down.md`) so the next session's Step 1 resumes execution.

### Step 1 — secondBrain draft check

Read `$SECONDBRAIN_DIR/drafts/`. Files there are pending writes from a previous session that didn't complete — most often because the Workflowy MCP was unstable. If a draft is present:

1. Read the draft to understand the routing plan and execution sequence.
2. After Step 2 completes, confirm with the user whether to resume execution against the existing plan or set it aside.
3. If resuming: execute the plan, then move the draft from `drafts/` to `session-logs/` with the original date prefix retained.
4. If setting aside: leave the draft in place and proceed with the new request.

### Step 2 — MCP health and node-ID resolution

**PERFORMANCE RULE:** the bootstrap must be fast. Never use `find_node` for structural nodes during bootstrap — read them from `memory/workflowy_node_links.md`. Use `search_nodes` with `max_depth:1` only as a last resort.

#### Persistent name index + path-based discovery

The MCP server keeps a disk-persisted name index at `$WORKFLOWY_INDEX_PATH` (conventionally `$SECONDBRAIN_DIR/memory/name_index.json`; unset disables persistence). It survives restarts; a 30-minute background task refreshes it; mutations checkpoint every 30 seconds.

**The fast retrieval surface to reach for first:**

- `node_at_path(path=["Top", "Sub", "Target"])` — walks a hierarchical path of node names. ONE `list_children` call per segment, so resolution is O(depth), not O(tree). Use this whenever you know where a node lives but not its UUID; visited nodes also feed the persistent index, accelerating future short-hash lookups under that branch.
- `resolve_link(link="...", search_parent_path=[...])` — built for the "I have a Workflowy URL, give me the node info" workflow. Pass the URL or short hash via `link`; pass an optional parent-name path via `search_parent_path` to scope the walk to a single subtree. Returns full node info on success.

**Short-hash auto-walk (fallback):** every `node_id` parameter accepts the 12-char URL-suffix or 8-char doc-prefix forms. On a cache miss the resolver runs a 5-minute walk. For trees over ~50 k nodes the fallback is unreliable — **prefer `node_at_path` or `resolve_link` with a parent path** rather than relying on the auto-walk.

**Building coverage explicitly:** `build_name_index(parent_id=...)` walks a single subtree deeply; the persistent index makes the work cumulative across sessions. For a one-shot deep index pass from the shell (independent of any running MCP), run `wflow-do reindex --root <UUID> [--root <UUID> ...]` — walks each root with the resolution budget, merges results into the same persistent file, and reports per-root coverage. Useful for fresh installs or recovery from sparse coverage.

#### Direct local index access (fastest possible lookup)

The persistent index file at `$WORKFLOWY_INDEX_PATH` (conventionally `$SECONDBRAIN_DIR/memory/name_index.json`) is plain JSON and can be queried **without going through the MCP at all**. Reach for this path before any MCP tool when:

- the MCP transport has been showing drops this session,
- you need to verify a UUID without making an API call, or
- you want to find every node matching a name pattern faster than tree-walking.

Schema:

```json
{
  "version": 1,
  "updated_at": <unix_seconds>,
  "nodes": [
    {"id": "<full-uuid>", "name": "<HTML-encoded name>", "parent_id": "<full-uuid or null>"}
  ]
}
```

Useful one-liners via Bash + jq:

```bash
INDEX="$WORKFLOWY_INDEX_PATH"   # conventionally $SECONDBRAIN_DIR/memory/name_index.json

# Resolve a Workflowy URL short hash to its full UUID
jq -r --arg h "<short-hash>" '.nodes[] | select(.id | endswith($h)) | .id' "$INDEX"

# Find every node whose name contains a substring (case-insensitive)
jq -r --arg q "<query>" '.nodes[] | select(.name | ascii_downcase | contains($q)) | "\(.id)\t\(.name)"' "$INDEX"

# Get a node's parent UUID
jq -r --arg id "<uuid>" '.nodes[] | select(.id == $id) | .parent_id' "$INDEX"
```

Treat the file as **read-only** from the assistant's side — only the MCP server (or `wflow-do reindex`) should write to it, because their write paths use an atomic write-then-rename protocol. A direct edit risks racing with the 30-second checkpoint.

When a file lookup misses, fall back to `node_at_path` / `resolve_link` / the MCP auto-walk in that order. When it hits, you've saved an API round-trip and any transport-layer fragility on top of that.

#### Memory file location

Try these paths in order; use the first one found by the `Read` tool:

1. `.auto-memory/workflowy_node_links.md` (Cowork sessions — relative to session mount)
2. `$SECONDBRAIN_DIR/memory/workflowy_node_links.md` (canonical for non-Cowork sessions)
3. `$HOME/.claude/projects/*/memory/workflowy_node_links.md` (Claude Code project memory)
4. `$HOME/.claude/memory/workflowy_node_links.md` (legacy global fallback)

If all reads fail, create a new memory file at the first writable path using the schema documented in `templates/secondbrain/memory/workflowy_node_links.md`. Then run **first-use population**: ask the user (via `AskUserQuestion`) for the names of their structural nodes (Tasks, Inbox, Journal, etc.), discover their IDs via `find_node`, and populate the table.

#### First-use population (only when memory file is empty)

Use `AskUserQuestion` to confirm the user's structural node names. Common patterns:

- "Where do you keep your todos?" → Tasks
- "Where do you capture untriaged items?" → Inbox
- "Where do you write daily entries?" → Journal
- "Where do you keep your reading queue?" → Reading List

For each name the user provides, call `find_node(name="<name>", parent_id=<workspace root>)` to get the UUID, then write the row into the memory file. Do not assume any default structure.

---

## Workflow categories (skeletons — flesh out per user)

The detailed implementation of each workflow lives in the user's customised copy of this file at `~/.claude/skills/wflow/SKILL.md`. The category list below is the framework; the prompts and tool sequences are user-specific.

### Operate (day-to-day)

- **Daily prioritisation** — surface today's todos, overdue items, and recently-modified projects via `daily_review`. Suggest a focus block for the morning.
- **Weekly prioritisation** — review last week's completions and unmoved items. Identify what to drop, what to escalate.
- **Monthly prioritisation** — set themes; promote/demote pillar work.
- **Task capture** — infer domain from content; place under the appropriate Tasks subtree as a Workflowy todo.
- **Project status** — for a named project, return current state (todos open, recent activity, blockers tagged).
- **Inbox triage** — walk Inbox children, route each to the right subtree (or delete).
- **Reading list management** — surface WIP reading, recent additions, items tagged for distillation.
- **Journal check-in** — append a dated entry under the Journal node.

### Synthesise (slower, compounding)

- **Distil single source** — turn a paper / article / chat into atomic notes. Place each note under the right pillar/theme. Mirror cross-cutting notes.
- **Distil reading list (batch)** — process the reading queue in one pass, producing a session log entry.
- **Cross-system research** — query Workflowy for everything related to a topic. If `services.md` exists and any of its entries have `participates_in: retrieval`, query those services too and merge the results. Surface as a synthesis with citations back to source nodes.
- **Extract from additional services** *(only if `services.md` declares any with `participates_in: extraction`)* — route the service's outputs (marginalia, highlights, annotations) into Distillations. The exact extraction call lives in the service's MCP namespace; consult `services.md` for the namespace and the relevant tools.
- **Synthesis capture** — convert a chat-produced framework or comparison into an atomic note in Workflowy.
- **Review surface** — surface notes tagged `#revisit` (or similar), prompt for spaced-repetition action.

---

## End-of-session discipline

Every session that mutated the second-brain should:

1. Write a session log entry **both** to Workflowy (under the user's Session logs node, if they have one) and locally at `$SECONDBRAIN_DIR/session-logs/YYYY-MM-DD-<brief-name>.md`.
2. Move any pending drafts from `drafts/` to `session-logs/` once their writes have landed.
3. Update `memory/workflowy_node_links.md` if the user moved or renamed a structural node during the session.

If the MCP wedges mid-session (a write returns `Tool execution failed` and `workflowy_status` shows degraded health):

1. Stop writes immediately.
2. Save the in-flight plan as a markdown file in `$SECONDBRAIN_DIR/drafts/` with the date prefix and a clear "RESUME EXECUTION" header.
3. Tell the user the next session will resume from the draft.

---

## Customisation

This template is generic. The user's customisations — preferred wording, project-specific routing rules, detailed workflow scripts — should be edited into their copy of the skill at `~/.claude/skills/wflow/SKILL.md`. Treat the template version (in this repo) as the upstream; pull updates manually when desired.
