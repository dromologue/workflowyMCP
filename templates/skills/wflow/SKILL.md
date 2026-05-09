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

Seven contracts the workflowy-mcp server ships — re-read this section if behaviour stops matching what the workflows describe; the routing decisions below assume them. Any additional service the user has configured (per `$SECONDBRAIN_DIR/memory/services.md`) ships its own contracts; document those alongside the service entry, not here.

1. **`complete_node` is the native completion path; `bulk_update` accepts `complete` / `uncomplete`.** The legacy `#done` tag-as-completion-marker is deprecated for tasks (`#done` on reading-list entries to mark "I've distilled this source" remains a separate convention). Workflowy's wire field is `note` for descriptions and `completed` for the boolean.
2. **`Parameters<T>` is the wrapper name on every tool's input.** If parameter-bearing calls suddenly silently misroute (every call acts as if you sent no arguments, only `workflowy_status` works), the server has regressed the wrapper name and the cowork client is validating against an empty schema. Recovery: route through `wflow-do` until the server is rebuilt.
3. **`use_index=true` is the recovery for walk-budget timeouts on name queries.** `find_node` and `search_nodes` answer from the persistent name index in O(1) with no walk budget. Index is name-only — descriptions need a live walk. Populate via `build_name_index(parent_id=<scope>)` once per fresh session or whenever the index is sparse.
4. **Every walk-shaped tool emits the same JSON-truncation envelope** (`truncated`, `truncation_limit`, `truncation_reason`, `truncation_recovery_hint`). Read these on every walk response. What you do with `truncated: true` depends on the **walk shape**, not on the truncation flag itself. *Audit-shaped* walks (every-reference sweeps, mirror audits, "review every node under X") cannot tolerate missing branches and must recover (`build_name_index(parent_id=<scope>)` then re-issue with `use_index=true`). *Research-shaped* walks ("what do I have on X", "trace my thinking on Y") tolerate partial coverage well — accept the partial result, surface the truncation banner verbatim, proceed. Reflexive recovery on every truncation adds latency without synthesis benefit; reflexive acceptance on audit walks misses the thing the audit was looking for. Decide which shape the current walk is before reacting.
5. **`parent_id: null` (or omitted) means workspace root, uniformly.** `create_node`, `batch_create_nodes`, `insert_content`, `list_children`, and every other parent-scoped tool accept null with the same semantics. Pre-2026-05-04 `insert_content` rejected null at the schema layer ("invalid type: null, expected a string"); the failure-report fix aligned its shape to the rest of the family.
6. **`insert_content` hard cap is 80 lines per call, lowered from 200 on 2026-05-04.** Above the cap, the call returns a typed error with a chunking instruction; chunk to ≤80 lines and pass the previous batch's `last_inserted_id` as the next call's `parent_id` to keep the hierarchy stitched together. The cap tracks the empirically safe transport ceiling.
7. **`move_node` is unified across the bare tool, `transaction.move`, and the CLI.** Until 2026-05-04 the bare handler used a propagation-retry wrapper while `transaction.move` used a plain method, producing the divergence the failure-report 2026-05-03 traced (11 % vs 100 % success rate). The retry now lives inside `client.move_node` itself, so every move caller gets identical resilience.
The CLI fallback (`wflow-do`) is in full surface parity with the MCP — every non-diagnostic tool has a matching subcommand. Drift fails the build, so the fallback path is always available when transport drops. Workflow orchestration that used to be duplicated between server and CLI (`create_mirror`, with more to follow) is being lifted into a shared `crate::workflows` module so a fix lands in both surfaces.

---

## UUID Parameter Discipline (read before EVERY write call)

Most preventable write-failure mode. Before any tool call that takes a UUID parameter (`move_node`, `edit_node`, `delete_node`, `complete_node`, `get_node`, `parent_id` on `create_node`, `new_parent_id` on `move_node`, every parameter typed `NodeId`), run this check — every time, no exceptions:

1. **Have the UUID string in front of you.** Full UUID (`550e8400-e29b-41d4-a716-446655440000`), 12-char URL-suffix short hash, or 8-char doc-prefix short hash. If you don't have it, resolve first via `node_at_path` / `resolve_link` / `find_node` / `list_children` or read it from `$SECONDBRAIN_DIR/memory/workflowy_node_links.md`. Don't make the write call yet.

2. **NEVER write the literal string `null`, `"null"`, or any placeholder between parameter tags. Every UUID-typed parameter gets an explicit UUID. No exceptions.** This is the definitive rule, and the skill *deliberately* does not rely on the server's `parent_id: null = workspace root` affordance documented in server contract #5. Three observed pathologies make the affordance unsafe in practice: (a) on some host surfaces (claude.ai web / mobile) the host or model emits `null` when the UUID isn't immediately to hand, treating it as a placeholder — calls land on the wrong node or are rejected with a path-aware deserialization error; (b) when the literal string `"null"` rather than JSON `null` is passed, behaviour was observed routing to the most-recently-discussed contextual destination on three consecutive calls in a single session — neither workspace-root semantics nor a clean rejection; (c) even when the server *would* accept `null`, passing the cached workspace-root UUID explicitly is preferable because it makes the destination auditable in tool-result transcripts. **If the UUID isn't on screen in your reasoning, resolve it before the write call. Do not pass `null` even when the schema accepts it.** If you catch yourself about to type `null`, stop. Re-read the last few tool results; the UUID exists somewhere.

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

**The probe is unconditional.** Run it on every wflow session, regardless of host. Claude Desktop, Cowork, and claude.ai all lazy-load MCP tools on first `tool_search`; "the connector was working last session" is not a substitute for probing this session. Deferring the probe until a write is imminent is how the gap bites — the agent forgets, captures degrade silently to disk, and the user finds out a session later.

#### What to probe

- **`workflowy:*`** — required for every workflow. Probe always.
- **`Filesystem:*`** — required to read drafts and memory files. Probe always (skip only in Claude Code, where `Read` / `Write` / `Bash` are native).
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

**Fail loud when `$SECONDBRAIN_DIR` is unreachable from the executing subshell.** A non-interactive `bash` subshell (which is what `Read` / `Bash` tools and `ls $SECONDBRAIN_DIR/drafts/` calls run in) does NOT inherit `~/.zshrc` exports — the env var is only visible if it was also set in the MCP host's `env` block (`claude_desktop_config.json` for Desktop, the equivalent for other hosts). When the var is set in `.zshrc` but missing from the host config (or vice-versa), the subshell sees an empty string and silently reports "directory does not exist" — a phantom gap, not a real one. Before the draft check, resolve the var explicitly. If it expands to empty:

1. Stop. Do not write a draft to a fallback path.
2. Tell the user: "`$SECONDBRAIN_DIR` is unset in this subshell. Check both (a) your shell rc (`~/.zshrc` or `~/.bashrc`) and (b) the MCP host's `env` block in `claude_desktop_config.json`. Both need the same value; the subshell only sees what the host config sets, not what your shell rc sets."
3. Wait for the user to fix the gap and re-confirm the var is visible (`echo $SECONDBRAIN_DIR` from a tool-issued shell), then resume.

This rule prevents the failure mode where a half-configured environment makes the bootstrap fall through to "no canonical dir" silently — the most common cause of session drafts being staged in unexpected places.

### Step 2 — MCP health and node-ID resolution

**PERFORMANCE RULE:** the bootstrap must be fast. Never use `find_node` for structural nodes during bootstrap — read them from `memory/workflowy_node_links.md`. Use `search_nodes` with `max_depth:1` only as a last resort.

**WORKING-MEMORY RULE.** `workflowy_node_links.md` and `distillation_taxonomy.md` (whichever exist) must be **read into the conversation's working context during Bootstrap**, not treated as on-demand resources to be opened later when a workflow needs an ID. Cached UUIDs only bite when they are in front of the agent before the first tool call. Sessions that defer the read routinely run live searches that the cache would have answered in O(1), inflate latency, and fall back to walks that hit the truncation cap. Read both files at the top of Step 2; carry their tables forward through the workflow. The same applies to `services.md` whenever it is present — read it at the top of Step 0 before the probe so the queued service set is known before any `tool_search` call.

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

If all reads fail, create a new memory file at the first writable path using the inline schema in [Memory file schemas](#memory-file-schemas) below. Then run **first-use population**: ask the user (via `AskUserQuestion`) for the names of their structural nodes (Tasks, Inbox, Journal, etc.), discover their IDs via `find_node`, and populate the table. The skill ships the schemas inline; no per-user data lives in the repo.

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

- **Distil single source** — turn a paper / article / chat into atomic notes. Place each note under the right pillar/theme. Mirror cross-cutting notes via `create_mirror(canonical_node_id, target_parent_id, pillar?)` — the mirror's name is copied from the canonical and its description gets `mirror_of: <canonical_uuid>` so `audit_mirrors` can surface drift later. Pass `pillar` when the canonical lacks a `canonical_of:` marker; existing markers are never overwritten.
- **Distil reading list (batch)** — process the reading queue in one pass, producing a session log entry.
- **Cross-system research** — query Workflowy for everything related to a topic. If `services.md` exists and any of its entries have `participates_in: retrieval`, query those services too and merge the results. Surface as a synthesis with citations back to source nodes.
- **Extract from additional services** *(only if `services.md` declares any with `participates_in: extraction`)* — route the service's outputs (marginalia, highlights, annotations) into Distillations. The exact extraction call lives in the service's MCP namespace; consult `services.md` for the namespace and the relevant tools.
- **Synthesis capture** — convert a chat-produced framework or comparison into an atomic note in Workflowy.
- **Review surface** — surface notes tagged `#revisit` (or similar), prompt for spaced-repetition action.

#### Three patterns every synthesis workflow shares

1. **The routing-plan gate.** Before writing anything to Distillations, build a draft routing table — *candidate atom name; destination pillar; mirror destinations; sources integrated* — and gate on user confirmation. Pair this with a **novelty check**: for each candidate atom, run a narrowed `find_node` / `search_nodes` (with `use_index=true` against the destination pillar UUID) for the key concept; if a canonical already covers the same ground, propose a mirror or backlink rather than a new atom. Both passes are cheap and both reduce drift between the user's mental model and what lands in the graph. Worth writing into "Distil single source", "Distil reading list", and "Synthesis capture" alike.
2. **The MOC-batch-mirror sequence.** Once the routing table is confirmed, execute in this order: (a) `create_node` the source MOC under its destination parent with the destination's explicit cached UUID; (b) `batch_create_nodes(operations=[...])` for the atomic-note children with `parent_id` populated on every operation (the nested-array shape protects against the bare-string-UUID encoding bug); (c) `create_mirror` selectively — mirror an atom into a destination only when it is a **substantive contribution** to that destination's canon, skip when it merely touches; (d) `create_node` the session log under the cached `Session logs` UUID and append the local mirror at `$SECONDBRAIN_DIR/session-logs/`; (e) only fall to `transaction.move` as a corrective when a placement misses. This sequence has roughly half the failure surface of create-then-move chains and should be the prescribed default.
3. **The Journal-scan + range-stamp convention.** When the user keeps a journal, every synthesis or review session begins with a scan of Journal entries over the period since the last journal-bearing session log. The session log's description (or first child) MUST stamp `Journal range covered: YYYY-MM-DD → YYYY-MM-DD` — the next session reads that stamp to know where to pick up. First-run convention: if no prior journal-bearing session log exists, scan back ~30 days. Lift only principle-level insights as atomic notes; personal-context entries stay in the Journal. Skip this pattern entirely if the user doesn't keep a journal.

#### Discipline lessons (each one paid for by an eval failure)

These shorten the gap between "the skill said X" and "the agent actually did X" on situations the skill used to leave implicit. Every phrase below is the lexical anchor for a specific failure mode the wflow eval suite caught — keep them present as the skill evolves.

- **Atomic notes use `batch_create_nodes` (not `insert_content`).** `insert_content` parses an indented text block into a hierarchy of names — it has no per-line description channel, so source attribution silently drops on every child node it creates. `batch_create_nodes(operations=[{name, description, parent_id}, ...])` accepts a description per operation and is the only insertion path that preserves the "source attribution in the description" rule of Atomic Note Discipline. (Eval Test 5: descriptions on 4 atomic notes were not set per-note in the `insert_content` call.)

- **Backlinks are `<a href="https://workflowy.com/#/<short-hash>">name</a>`, not raw UUIDs in prose.** The literal HTML anchor is the form the Workflowy UI renders as a clickable link. Writing "see canonical 4ef32619-..." as plain text means the link is invisible at the UI layer and audit tooling can't follow the reference. Always use the anchor form in descriptions. (Eval Test 11: description named the canonical UUID in plain text but did not write a Workflowy `<a href>` backlink.)

- **Mid-session task capture is offer-then-confirm.** When a task surfaces during an unrelated chat (i.e. capture is incidental, not the user's primary intent), split into two messages: (1) "I can capture this as `#TODO @<assignee>` under \<inferred-domain\> — confirm?" (2) on user yes, the actual `create_node` / `smart_insert` write. Collapsing both into a single move denies the user the chance to redirect or veto, and inferred-domain mistakes compound silently. When the user's primary intent IS task capture (they opened with "capture: X"), the offer collapses to inferred-domain confirmation in the first response and the create lands on yes — fine. The two-message rule applies only when capture is incidental. (Eval Test 24: captured directly without an explicit confirmation step.)

- **Daily prioritisation produces a priority-ordered briefing, not a per-source paste.** The output of the daily-review steps is *synthesised* into a single ranked list ordered by urgency × strategic importance against the week's priorities, headed by "the single most important thing today". A flat per-system summary ("Workflowy: X overdue. \<service\>: Y fresh. Session logs: Z open.") is data, not a briefing — surface that level only when the user asks for it. (Eval Test 1: final response was a flat per-system summary, not priority-organised.)

- **Explicit-check discipline.** Every workflow step that says "scan X" or "check Y" must explicitly state the result, including the negative. "Reading list: 3 items, none with action verbs." / "No fresh material from \<service\>." / "Session logs since yesterday: none open." Silent omission breaks the audit trail of what was checked. The discipline also requires re-issuing data calls at workflow start even when "the same data" was loaded earlier in the session — workflow timing changes the answer. (Eval Test 2: did not state "no actions detected"; Test 8: skipped re-issuing recent-changes / session-log calls at journal time.)

- **Cross-system retrieval requires both name AND content search per source.** Index-fast-path (`use_index=true`) catches name matches in O(1); description-touching content needs a live walk (`max_depth=5+`). For each external service whose `participates_in: retrieval` is declared in `services.md`: surface search AND content/grep search, even when the surface returned empty (a content search may catch text inside documents whose titles don't match). Skipping the content search because the surface returned empty is the failure mode the eval suite caught. (Eval Test 4, 2026-05-09: only the title search ran on the configured service; no follow-up grep keyword search.)

- **Uniform per-pillar mirroring.** When a synthesis produces 3+ atomic notes and two or more of them substantively bear on the same destination pillar, *all atoms* in the synthesis that touch that pillar get mirrored there — not just the most obviously-anchored ones. Per-atom mirror judgement is where drift creeps in: the agent picks the strongest contributors and silently drops a third atom even though it also bears on the destination. If an atom is genuinely orthogonal, skip the mirror but mark it explicitly in the routing table from pattern 1 above. (Eval Test 9: first "causality not difficulty" note arguably touches leadership but had no Lead mirror.)

- **Audit `scope_resolved` after every scoped call.** Every MCP tool that takes an `Option<NodeId>` for parent_id (`create_node`, `batch_create_nodes`, `insert_content`, `create_mirror`, `list_children`, `find_node`, `search_nodes`) returns a `scope_resolved` field in its response: `workspace_root` when the resolved scope was None (caller passed null or omitted), `scoped:<full-uuid>` otherwise. After every call where parent_id was null/omitted, read the field and confirm the resolved scope matches the intended destination — `workspace_root` must mean "I genuinely intended to write at the workspace root", not "I forgot the UUID". If the audit fails, halt the workflow and resolve the destination explicitly before any follow-up write. For `create_mirror` specifically — when batching multiple mirror passes across a synthesis — prefer `dry_run=true` first: the preview returns `scope_resolved`, the would-be `mirror_name`, and `would_annotate_canonical` without writing, so the destination check is verifiable before the eight production calls land. (Failure-report 2026-05-09: callers couldn't tell whether null parent_id had landed at workspace root, an inferred parent, or a cached focus; on `create_mirror` the same opacity made eight sequential null-null mirror calls unverifiable. Adding scope_resolved + dry_run to the response shape closes the gap.)

---

## End-of-session discipline

Every session that mutated the second-brain should:

1. Write a session log entry **both** to Workflowy (under the user's Session logs node, if they have one) and locally at `$SECONDBRAIN_DIR/session-logs/YYYY-MM-DD-<brief-name>.md`.
2. Move any pending drafts from `drafts/` to `session-logs/` once their writes have landed.
3. Update `memory/workflowy_node_links.md` if the user moved or renamed a structural node during the session.
4. **If this session resumes a previously-partial one, create a new sibling session-log node — don't edit the original.** Name the new node `[YYYY-MM-DD] — [original brief title] (resumption: complete)`. The original log captures the failure mode and routing decisions; the resumption log captures the resolution and final tally. Both stay readable, the audit trail is two-stage, and any later session reading the subtree sees the full arc instead of a description that's been overwritten. Same rule applies to the local `$SECONDBRAIN_DIR/session-logs/` mirror — write a new dated file with a `-resumption` or `-completion` suffix; don't overwrite the partial log.

If the MCP wedges mid-session (a write returns `Tool execution failed` and `workflowy_status` shows degraded health):

1. Stop writes immediately.
2. Save the in-flight plan as a markdown file in `$SECONDBRAIN_DIR/drafts/` with the date prefix and a clear "RESUME EXECUTION" header.
3. Tell the user the next session will resume from the draft.

---

## Memory file schemas

The three memory files this skill reads — `workflowy_node_links.md`, `distillation_taxonomy.md`, `services.md` — are **user-specific data** that lives at `$SECONDBRAIN_DIR/memory/<file>.md`. They are NOT shipped in the repo; the skill creates them on first use using the schemas below. Every line here is a fill-in-the-blank shape; replace `<TBD>`, `<UUID>`, `<Pillar 1>`, etc. with the user's actual values.

### `workflowy_node_links.md`

```markdown
---
name: Workflowy Node Links
description: Cached Workflowy node IDs for structural and pillar nodes — avoids repeated find_node calls
type: reference
canonical_path: $SECONDBRAIN_DIR/memory/workflowy_node_links.md
---

The wflow skill reads this file on every bootstrap. Replace `<TBD>` placeholders with the actual UUIDs from your Workflowy account. Update `Last Verified` whenever you confirm an ID still resolves.

## Synthesis Write Targets (Workflows 9–14) — only if you follow the second-brain discipline

Every node a synthesis session might write to. **Read this section into working context during Bootstrap** for any synthesis workflow; never resolve these by live search. Re-resolve any entry whose `Last Verified` is older than 30 days.

| Node Name                  | Node ID | Last Verified | Used By |
| -------------------------- | ------- | ------------- | ------- |
| Distillations (root)       | <TBD>   | <TBD>         | every synthesis search and write |
| Pillar 1 — distillations   | <TBD>   | <TBD>         | pillar canonical |
| Pillar 2 — distillations   | <TBD>   | <TBD>         | pillar canonical |
| Cross-pillar concept maps  | <TBD>   | <TBD>         | irreducibly cross-pillar claims |
| Session logs               | <TBD>   | <TBD>         | every synthesis session writes here |
| Themes (parent)            | <TBD>   | <TBD>         | parent for theme mirror destinations |

## Structural Nodes (rarely change)

| Node Name      | Node ID | Last Verified |
| -------------- | ------- | ------------- |
| Tasks          | <TBD>   | <TBD>         |
| Inbox          | <TBD>   | <TBD>         |
| Tags           | <TBD>   | <TBD>         |
| Resources      | <TBD>   | <TBD>         |
| Links          | <TBD>   | <TBD>         |
| Journal        | <TBD>   | <TBD>         |
| Reading List   | <TBD>   | <TBD>         |
| Distillations  | <TBD>   | <TBD>         |

## Reading List sub-nodes (under Reading List)

| Node Name | Node ID | Last Verified |
| --------- | ------- | ------------- |
|           |         |               |

## Triage Sources

The set of nodes that Workflow 6 (Inbox Triage) sweeps in order. Append rows to add a new triage target without changing the skill.

| Order | Source Node          | Node ID | Notes |
| ----- | -------------------- | ------- | ----- |
| 1     | Inbox (master)       | <TBD>   | Untriaged tasks, links, ideas. |
| 2     | Reading List         | <TBD>   | URLs you want to read but haven't decided what to do with. |
| 3     | Reading WIP          | <TBD>   | Items being read or recently read but not yet distilled. |

## Domain Nodes (under Tasks)

Domains are user-specific (Office / Personal / Project / etc.). Discover them once via `list_children` against the Tasks node and write the rows here.

| Node Name | Node ID | Last Verified |
| --------- | ------- | ------------- |
|           |         |               |
```

### `distillation_taxonomy.md`

Author once before the synthesise workflows (9–14) can run. The skill reads it during Bootstrap whenever a workflow needs pillar / theme / routing data.

```markdown
---
name: Distillation Taxonomy
description: Pillar / theme / routing data for the wflow skill — the semantic layer of the second brain.
type: reference
last_reviewed: <YYYY-MM-DD>
canonical_path: $SECONDBRAIN_DIR/memory/distillation_taxonomy.md
---

# Distillation Taxonomy

## Pillars (canonical)

A *pillar* is a top-level conceptual bucket. Each typically has a Link node (raw material under Resources / Links) and a Distillations node (atomic notes under Distillations). Three is a sensible minimum; five is the upper bound before pillars start overlapping.

| Pillar     | Focus                                                | Link node UUID | Distillations node UUID |
| ---------- | ---------------------------------------------------- | -------------- | ----------------------- |
| <Pillar 1> | <one-line description of what this pillar is about>  | `<UUID>`       | `<UUID>`                |
| <Pillar 2> | <…>                                                  | `<UUID>`       | `<UUID>`                |
| <Pillar 3> | <…>                                                  | `<UUID>`       | `<UUID>`                |

**Key thinkers per pillar (optional).**

- **<Pillar 1>** — <author>, <author>
- **<Pillar 2>** — <author>

**Cross-pillar concepts (optional).** Concepts that connect multiple pillars; synthesis touching them mirrors across both and gets a node under `Cross-pillar concept maps`.

- **<Concept name>** — connects <Pillar A> + <Pillar B>.

## Themes (cross-cutting)

A *theme* describes what a claim is about, not what it tells you to do. Themes typically mirror into one or more pillars.

| Theme     | Link UUID | Distillations UUID | Default pillar mirror | Notes |
| --------- | --------- | ------------------ | --------------------- | ----- |
| <Theme 1> | `<UUID>`  | `<UUID>`           | <Pillar X>            | <…>   |

## Inbound routing table

When triage (Workflow 6) or reading-list management (Workflow 7) needs to decide where an inbound link belongs.

| Topic marker                            | Destination Link folder | Default pillar for distillation |
| --------------------------------------- | ----------------------- | ------------------------------- |
| <e.g. "anthropic.com", "openai.com">    | <e.g. AI Link folder>   | <e.g. Build>                    |

## Tag conventions (optional)

- `#done` — applied to Reading List entries that have been distilled.
- `#session_<YYYY-MM-DD>` — applied to atoms created in a single distillation session.
- `#mirror_of:<short-hash>` — applied to a mirror node pointing at its canonical.
```

### `services.md`

Optional. Skip this file entirely if Workflowy is the only surface the user has.

```markdown
---
name: Additional Services
description: User-configured services the wflow skill probes and routes to alongside Workflowy.
type: reference
canonical_path: $SECONDBRAIN_DIR/memory/services.md
---

# Additional Services

The wflow skill ships service-agnostic. The `Workflowy` MCP is the one required surface; everything else — ink capture, document storage, reading queues, task systems, calendar — is optional and declared here.

## Schema

Each `## <Service Name>` block holds:

- **mcp_namespace** — the `mcp__<name>__*` prefix the tools use, or `none` if the service is reached via shell / HTTP rather than MCP.
- **purpose** — one-line description.
- **participates_in** — comma-separated workflow categories: `capture`, `triage`, `retrieval`, `synthesis`, `extraction`, `prioritisation`.
- **bootstrap_probe** — health-check tool name to call during Step 0, or `none`.
- **runbook_on_unreachable** — exact text to surface to the user when the probe fails.
- **notes** — optional. Anything else relevant: known fragility, OCR backend selection, rate limits, cache TTLs, etc.

## Configured services

(Populate with `## <Service>` blocks per service. Leave empty if Workflowy is the only surface.)
```

---

## Customisation

This template is generic. The user's customisations — preferred wording, project-specific routing rules, detailed workflow scripts — should be edited into their copy of the skill at `~/.claude/skills/wflow/SKILL.md`. Treat the template version (in this repo) as the upstream; pull updates manually when desired.
