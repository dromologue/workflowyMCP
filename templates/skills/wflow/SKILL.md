---
name: wflow
description: Integrated second-brain skill built on Workflowy plus any additional services the user has configured (declared in $SECONDBRAIN_DIR/memory/services.md). Capture, triage, distillation, retrieval, and synthesis. Triggered conversationally. Use when the user wants to plan their day, capture a task, triage their inbox, distil a source into atomic notes, journal, research a topic across their notes, or run a periodic review.
allowed-tools: Read, Write, Edit, Bash, Glob, Grep, AskUserQuestion, WebFetch, mcp__workflowy__workflowy_status, mcp__workflowy__health_check, mcp__workflowy__get_node, mcp__workflowy__find_node, mcp__workflowy__search_nodes, mcp__workflowy__list_children, mcp__workflowy__get_subtree, mcp__workflowy__read_batch, mcp__workflowy__create_node, mcp__workflowy__edit_node, mcp__workflowy__delete_node, mcp__workflowy__move_node, mcp__workflowy__reorder_nodes, mcp__workflowy__insert_content, mcp__workflowy__smart_insert, mcp__workflowy__complete_node, mcp__workflowy__duplicate_node, mcp__workflowy__create_from_template, mcp__workflowy__batch_create_nodes, mcp__workflowy__transaction, mcp__workflowy__bulk_update, mcp__workflowy__bulk_tag, mcp__workflowy__create_mirror, mcp__workflowy__audit_mirrors, mcp__workflowy__daily_review, mcp__workflowy__get_recent_changes, mcp__workflowy__list_overdue, mcp__workflowy__list_upcoming, mcp__workflowy__list_todos, mcp__workflowy__get_project_summary, mcp__workflowy__tag_search, mcp__workflowy__find_backlinks, mcp__workflowy__find_by_tag_and_path, mcp__workflowy__node_at_path, mcp__workflowy__path_of, mcp__workflowy__resolve_link, mcp__workflowy__since, mcp__workflowy__convert_markdown, mcp__workflowy__export_subtree, mcp__workflowy__review, mcp__workflowy__build_name_index
# Additional service tool namespaces (e.g. mcp__SERVICENAME__*) are listed in $SECONDBRAIN_DIR/memory/services.md and must be added to allowed-tools when configured.
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

Eleven contracts the workflowy-mcp server ships — re-read this section if behaviour stops matching what the workflows describe; the routing decisions below assume them. Any additional service the user has configured (per `$SECONDBRAIN_DIR/memory/services.md`) ships its own contracts; document those alongside the service entry, not here.

1. **`complete_node` is the native completion path; `bulk_update` accepts `complete` / `uncomplete`.** The legacy `#done` tag-as-completion-marker is deprecated for tasks (`#done` on reading-list entries to mark "I've distilled this source" remains a separate convention). Workflowy's wire field is `note` for descriptions and `completed` for the boolean.
2. **`Parameters<T>` is the wrapper name on every tool's input.** If parameter-bearing calls suddenly silently misroute (every call acts as if you sent no arguments, only `workflowy_status` works), the server has regressed the wrapper name and the cowork client is validating against an empty schema. Recovery: route through `wflow-do` until the server is rebuilt.
3. **`use_index=true` is the recovery for walk-budget timeouts on name queries.** `find_node` and `search_nodes` answer from the persistent name index in O(1) with no walk budget. Index is name-only — descriptions need a live walk. Populate via `build_name_index(parent_id=<scope>)` once per fresh session or whenever the index is sparse.
4. **Every walk-shaped tool emits the same JSON-truncation envelope** (`truncated`, `truncation_limit`, `truncation_reason`, `truncation_recovery_hint`). Read these on every walk response. What you do with `truncated: true` depends on the **walk shape**, not on the truncation flag itself. *Audit-shaped* walks (every-reference sweeps, mirror audits, "review every node under X") cannot tolerate missing branches and must recover (`build_name_index(parent_id=<scope>)` then re-issue with `use_index=true`). *Research-shaped* walks ("what do I have on X", "trace my thinking on Y") tolerate partial coverage well — accept the partial result, surface the truncation banner verbatim, proceed. Reflexive recovery on every truncation adds latency without synthesis benefit; reflexive acceptance on audit walks misses the thing the audit was looking for. Decide which shape the current walk is before reacting.
5. **Write tools REQUIRE an explicit `parent_id`; reads still treat null/omit as workspace root.** As of 2026-06-16 the four write tools (`create_node`, `batch_create_nodes`, `insert_content`, `create_mirror`) reject a null or omitted `parent_id` at the wire with a field-named error — pass the destination UUID / short hash, or the empty string `""` for the deliberate "workspace root" choice. Read tools (`list_children`, `find_node`, `search_nodes`) keep null/omit = root (a missing parent there carries no destructive intent). This closes the host-stripping/coercion silent-misroute (a stripped parameter now fails loudly instead of landing a write at root). See UUID Parameter Discipline below.
6. **`insert_content` caps at 80 lines AND reports the committed-count cursor on EVERY failure.** Above the 80-line cap (lowered from 200 on 2026-05-04, tracking the safe transport ceiling) the call returns a typed error with a chunking instruction. On a partial stop — cancel, timeout, OR a hard mid-batch API error (e.g. a 429 on the 10th line) — it returns `{status:"partial", reason:"cancelled"|"timeout"|"error", created_count, total_count, last_inserted_id, ...}`. **`reason:"error"` is flagged `is_error:true` and additionally carries `proximate_cause` / `retry_after_secs` / `retryable`** (see contract 9). In every case `created_count` + `last_inserted_id` tell you exactly what landed; **resume by re-running the remaining lines under `last_inserted_id`** (or the original parent if it's null). You never need a separate read to learn what committed — though a read-back is still the only *proof* a write persisted (contract 10). Pre-2026-06-17 a hard error discarded this cursor; resuming is now uniform across all three stop reasons.
7. **`move_node` is unified across the bare tool, `transaction.move`, and the CLI.** Until 2026-05-04 the bare handler used a propagation-retry wrapper while `transaction.move` used a plain method, producing the divergence the failure-report 2026-05-03 traced (11 % vs 100 % success rate). The retry now lives inside `client.move_node` itself, so every move caller gets identical resilience.
8. **`reorder_nodes(parent_id, node_ids[])` is the primitive for ordering a set of siblings.** Workflowy's `move_node` priority is *position-relative-to-siblings* and renormalises after every call, so a naive forward `priority=0,1,2,…` loop fights itself when batched. The tool walks the desired list in REVERSE and issues `move_node` with `priority=0` per id — every move plants its node at position 0 and the previously-planted nodes shift one step right, so after N moves the head of the parent's children is the requested sequence. Side effect: ids not currently under `parent_id` are reparented (the primitive is built on `move_node`, not a sibling-only assertion). Capped at 200 ids per call. Returns `Complete` or `Partial { reason: cancelled | timeout }` with per-id `ok / error / skipped` entries; partial outcomes are safe to retry by re-issuing the full list. Use this any time a workflow says "reorder this set" — never roll a forward-priority loop.
9. **Every error response is a typed envelope — branch on `proximate_cause`, not on the message string.** Failures carry `{operation, node_id, proximate_cause, retryable, retry_after_secs, hint, error}`. `proximate_cause` is one of `rate_limited` / `timeout` / `upstream_error` / `auth_failure` / `not_found` / `cancelled` / `lock_contention` / `cache_miss` / `invalid_params` / `unknown`. **`rate_limited` (429) is the one to act on:** `retryable:true` and `retry_after_secs:N` tell you to wait N seconds and re-issue the same call — it is a recoverable transient, NOT a failed write. Do not treat a bare top-line "tool failed" string (what some clients render) as undiagnosable; the structured `data` envelope carries the real cause. Pre-2026-06-17 a 429 was mislabelled `unknown` with the `retry_after` buried; it is now first-class. `retryable:false` (auth, invalid_params, unknown) means fix-and-retry or stop, never blind-retry.
10. **The write ok-counter is receipt, not commit — a live re-read is the only proof.** `per_tool_health.<tool>.ok` counts handler receipts; a rolled-back `transaction` and an `is_error:true` `insert_content` partial both record as `Err` (commit-accurate since 2026-06-01 / 2026-06-17), but a write whose response was lost to a transport timeout is genuinely ambiguous. **`create_node` takes an optional best-effort `idempotency_key`** (2026-06-17): pass a stable token to make a retry of that create safe — a repeated key replays the original node with an `idempotent_replay:` message instead of double-writing. It covers retry-after-success and retry-before-write, but NOT an ambiguous timeout after the write was sent (the server never recorded the success) — so for a single-node create lost to a timeout you STILL read back before retrying. It's server-process-scoped (the `wflow-do` CLI has no key — a one-shot process can't dedupe). `insert_content` / `batch_create_nodes` take no key — their committed-count cursor / per-op `succeeded` (contract 6) make resume safe by construction. Make verify-after-write the discipline for any write near a rate-limit window.
11. **`find_node` and `search_nodes` refuse a workspace-root scan when `parent_id` is omitted.** On a large tree an unscoped name search would burn the full walk budget, so the tools reject it rather than time out. Supply `parent_id` to scope the search, set `allow_root_scan=true` to accept the full walk deliberately, or set `use_index=true` to answer from the persistent name index in O(1) (name-only — description content still needs a live walk).

The CLI fallback (`wflow-do`) is in full surface parity with the MCP — every non-diagnostic tool has a matching subcommand. Drift fails the build, so the fallback path is always available when transport drops. Workflow orchestration that used to be duplicated between server and CLI (`create_mirror`, with more to follow) is being lifted into a shared `crate::workflows` module so a fix lands in both surfaces.

---

## UUID Parameter Discipline (the canonical hazard; everything below cross-references this)

The most preventable failure mode on this skill, and the single load-bearing discipline. **Before any tool call that takes a UUID parameter — write or read — run this check.**

**The hazard.** Some MCP host surfaces strip bare top-level string parameters. When the model emits a UUID and the host serialiser turns it into `null` — or emits the literal string `"null"`, or the model itself emits `null` when the UUID isn't to hand — the call either rejects with a path-aware deserialization error, lands at the workspace root, or silently misroutes to the most-recently-discussed contextual destination. The fault is symmetric: writes (`move_node`, `edit_node`, `create_node`, `parent_id`, `new_parent_id`) and reads (`get_node`, `list_children`, `get_subtree`, `node_id`) both suffer it.

**The wire-level guard is not a backstop you can rely on.** The server's `NodeId` deserializer rejects literal `"null"` strings and JSON `null` for required fields, but the 2026-05-27 observation confirms that some hosts can coerce `null` to a contextual UUID _before_ the server sees it — every `null` passed for parent / canonical IDs during that session resolved silently to the intended node, masking the misroute by luck. Until the host-side coercion is closed (server-side hardening tracked in `tasks/todo.local.md`), **this discipline is the only line of defence**. Treat the rules below as enforced by your own attention, not by the schema.

**The rules, every UUID parameter, every time:**

1. **Have the UUID on screen.** Full UUID, 12-char URL-suffix hash, or 8-char doc-prefix hash. If you don't have it, resolve first via `node_at_path` / `resolve_link` / `find_node` / `list_children` or read it from `$SECONDBRAIN_DIR/memory/workflowy_node_links.md`. Don't make the call yet.

2. **NEVER write the literal `null`, `"null"`, or any placeholder between parameter tags. Every UUID-typed parameter gets an explicit UUID. No exceptions.** As of 2026-06-16 the four write tools (`create_node`, `batch_create_nodes`, `insert_content`, `create_mirror`) REQUIRE an explicit `parent_id` — omitting it or passing `null` is rejected at the wire with a field-named error. For the deliberate "workspace root" destination pass the empty string `""` (the root sentinel); otherwise pass the destination UUID / short hash so the placement is auditable. If you catch yourself about to type `null`, stop. Re-read the last few tool results; the UUID exists somewhere.

3. **If the error reads `invalid parameters at \`.<field>\`: invalid type: null, expected a string`** — you typed `null` for `<field>`. Find the actual UUID. The error names the field on purpose. *Path-less variant* (`invalid type: null, expected a string` with no `at \`.<field>\``) means the running MCP binary is pre-2026-05-03 — restart the host to pick up the path-aware deserializer.

4. **Recovery for hosts that persistently strip:** route through tools whose UUIDs sit inside a nested `operations` array. UUIDs in operation objects survive hosts that strip top-level strings.
   - **Multi-writes:** `transaction(operations=[{op:"move",node_id,new_parent_id}, …])`. Trade-off: one rollback unit per transaction.
   - **Multi-creates:** `batch_create_nodes(operations=[…])`.
   - **Multi-reads:** `read_batch(operations=[{op:"get_subtree"|"list_children"|"get_node", node_id, max_depth?}, …])`.
   - **Last resort:** the `wflow-do` CLI bypasses host-side encoding entirely.

5. **Destructive ops carry a heightened guard.** For `delete_node` and any `transaction` delete op, resolve the target and **visually confirm both its UUID and its current name on screen immediately before the call** — not from memory, not from a UUID carried several turns back. Never issue a delete with `null` or any placeholder in `node_id`. The host-coercion path means a `null` delete can land on an unintended-but-plausible node — the most-recently-discussed one — and a delete cannot be rolled back. The wire-level deserializer guard does not protect you here, because some hosts coerce `null` to a real contextual UUID before the server sees it. The server offers a name-echo guard: pass `expect_name` (the node's current name) on `delete_node` and on any `transaction` delete op, and the server refuses the delete unless the resolved node's trimmed name matches. Always pass it on any delete whose `node_id` was resolved indirectly — and otherwise treat this rule as enforced by your own attention, not by the schema.

**Side-effect-verification corollary.** When a write *appears* to succeed despite an obviously-malformed parameter (e.g. a `create_node` whose success message reports placement at the workspace root when you intended a specific parent), verify the side effect at the destination before chaining further writes. The first chain link being silently wrong is what produces the orphan-accumulation pattern.

Every discipline section that follows (Read-path, Multi-write batch, synthesis pattern 2, the scope_resolved audit) builds on this. They cross-reference rather than re-explain.

---

## Read-path discipline for freshly-created nodes (the sweep trap)

The read-side compound of UUID Parameter Discipline. For established material the persistent name index at `$WORKFLOWY_INDEX_PATH` is the clean second channel; for recently created material the index can still be empty for that subtree (it only learns nodes through walks and explicit reindex passes), and the live-read channel is exposed to the encoding hazard described above.

**The discipline, in order:**

1. **Check index freshness against node age.** Pull `updated_at` from `$WORKFLOWY_INDEX_PATH` (`jq '.updated_at' "$WORKFLOWY_INDEX_PATH"`) and compare against the target's `created_at`. If `index.updated_at > node.created_at`, the index covers the subtree.

2. **Index fresh → reconstruct from local JSON.** Walk `parent_id` links from the target downward. No walk budget, no host encoding, no rate limiter. Always prefer for established material.

3. **Node newer than index → scoped reindex first.** `build_name_index(parent_id=<UUID>)` from the same session that created the node, or `wflow-do reindex --root <UUID>` from the shell. There is no in-process background refresher — the scoped reindex (or a scheduled `wflow-do reindex --timeout-secs 0 --patient` job) is the only refresh path.

4. **End-of-capture reflex.** Fire the scoped reindex immediately after writes settle on any workflow that may want to read the new content same-session. Sub-second cost; keeps the read path open.

5. **Live read needed →** `read_batch(operations=[…])` per UUID Parameter Discipline rule 4. Never a bare `get_subtree(node_id=<UUID>)` when the host has shown stripping behaviour.

6. **Last resort.** Workflowy OPML/text export into `$SECONDBRAIN_DIR/drafts/`. Evidence the discipline broke down, not a routine path.

**Why this matters.** Without freshness-checking, an unscoped `get_subtree` on a freshly-created reference document can return an empty shell (headings present, bodies absent) and the agent proceeds to "distil" the shell. Cheap check; expensive miss.

---

## Pillar-node descriptions are the carved-out exception to "content in sub-nodes"

The general convention when writing material into Workflowy: **content goes in sub-nodes, never in the description** of an existing node. A node's description (its note) is reserved for source attribution, backlinks (`mirror_of:`, `canonical_of:`), or a one-line gloss — never the body of the content. This preserves the tree structure that `audit_mirrors`, `bulk_tag`, search, and the visualiser all assume.

**The exception: pillar / bucket nodes.** Pillar-level nodes (Distillations pillars, Cross-pillar concept maps root, Themes parent root, any OP source cluster that serves as a pillar home) carry a generic, evergreen content description as their note that *describes the pillar itself* — its scope, the type of material it holds, what belongs there. That description IS node-level metadata, not content. It's appropriate on the node because it doesn't decay session-over-session and because it's *about* the node, not stored *in* the node. A first-pass at pillar review on 2026-05-24 created `📋 pillar review` summary children with composition / outcome sub-nodes under each bucket (following the general rule); the user corrected this, the children were deleted, and a generic content description was set as the note on each of the eight pillar / bucket nodes.

**How to apply:**

1. For pillar / bucket-level nodes — a generic content description on the node as its note. Not a session log entry, not "what changed this week"; a description of what the pillar contains at a level of generality that holds across sessions.
2. For every other node (atomic notes, source clusters, theme sub-buckets when not bucket-level, session logs, journal entries, captured tasks) — content goes in sub-nodes. The description holds source attribution, backlinks, or a one-line gloss.
3. The exception is narrow. If unsure whether a node qualifies as pillar/bucket-level, default to the general rule (sub-nodes). The exception applies only when the description genuinely describes the pillar's purpose generically.

The user-specific pillar descriptions and the list of which nodes qualify as pillar/bucket-level live in `distillation_taxonomy.md`. This convention layer carries the rule; the taxonomy carries the data.

---

## Multi-write batch discipline (transaction-over-move)

When a workflow performs 2+ writes that share a logical batch — moves with a common destination, edits across related nodes, deletes within one subtree — route through `transaction(operations=[…])` from the start. UUID Parameter Discipline rule 4 already names `transaction` as the operations-array recovery; this section promotes it to a default beyond that recovery role.

**Two reasons beyond rule 4:**

1. **Single rollback unit.** A failure mid-batch rolls back what already landed; sequential per-op calls leave partial state stranded.
2. **Auditable plan.** A single `transaction` call presents the full operation list before execution; sequential calls force the agent to reason about partial state across many tool turns.

**When NOT to use transaction:**

- **Single-shot writes.** No rollback value for a single create / move / edit.
- **Interactive per-item triage** (inbox triage, reading-list management) — bundling defeats the per-item user gate.
- **Non-transaction-supported ops** — `create_mirror`, `bulk_update`, `reorder_nodes`, `bulk_tag` are their own tools. (The `transaction` schema does accept `complete` / `uncomplete` if completion needs to be inside the rollback unit.)

**Sizing.** Chunk batches > ~80 ops into multiple transactions.
**Op ordering.** Cascading deletes come *last* — earlier ops on the same subtree get 404'd if the parent is deleted first.

**Dry-run standing rule.** Any write batch larger than **5 operations** follows a two-step pattern, no exceptions: (1) print the planned sequence — one line per operation, in execution order, naming the verb, the target / parent UUID, and the node name; (2) ask y/n before any real write — explicit confirmation, not "I'll proceed unless you object". This eliminates the "I created N things and now can't tell where they went" failure class — it costs ~30 seconds per batch, and without it an assistant creating nodes faster than it can verify destinations produces orphans the user only discovers after the fact. **Print the plan even when the destinations are obvious** — the point is not only the user's veto but forcing yourself to spell out the routing before committing, which is the moment ambiguities become visible. The `wflow-do` CLI supports `--dry-run` on any subcommand (it prints the planned op and exits without touching the API) if you want to stage the plan mechanically.

---

## The Distillation Standard (read before any synthesis write)

Every node in your Distillations layer — heading or atom, canonical or mirror — should meet one standard. It is both the bar a fresh distillation clears at creation and the bar a cleanup pass enforces.

1. **Every node is a self-standing claim.** Read on its own, stripped of any thinker parenthetical, it still states something. The thinker is a trailing `(Name)` attribution only — never the head of the name, never the organising axis. Structure on claims and topics, not on people. No bare-label or headline-led heads ("DDD in the AI Era — Author"); the head of a source's cluster is that source's **lead claim**, with supporting atoms beneath it.

2. **Use real words, not index jargon.** Don't label nodes "MOC" / "source MOC" / "synthesis MOC" or tag them `#moc` — the term meant "Map of Content" and carries no meaning for a reader. A source's distilled cluster is simply its lead-claim head plus its atoms; a pillar / theme node is an index node.

3. **Descriptions carry only graph plumbing.** A single `Source: <url>` line plus `canonical_of:` / `mirror_of:` markers — no `Author:` / `Published:` / `Captured:` lines, no capture-lines, no `— source MOC` / `— article` suffixes, no publication dates, no enumeration prefixes. Substantive content lives in the claim (the name) and in child atoms. **Judgement exception:** a description that *argues* something stays (evidence backing the claim, a backlink concept-map); one that only *records where the note came from* goes. When in doubt, strip.

4. **Canonical first, then mirrors verbatim.** Edit the canonical's name, then rename each mirror to match byte-for-byte (a mirror keeps only its own `mirror_of:` description).

---

## Bulk-edit discipline (large convention sweeps)

When sweeping many nodes at once — renames, tag strips, description cleanups, a Distillation-Standard pass:

- **The on-disk name index can be stale.** It refreshes only every ~30 min, so a sweep run soon after another edit pass reads *pre-edit* names. Use the index only to enumerate the work-list by **UUID** (UUIDs never change); read each node's **current name live** before composing its edit, or you will overwrite a freshly-edited name with a stale one.
- **Pass full UUIDs, never short hashes, in a sweep** — a short hash triggers a resolution walk that times out and burns rate budget.
- **`export_subtree` / `bulk_update` may coerce their scope parameter to null** on some host surfaces — if scoped calls misbehave, enumerate per-container with `read_batch` instead.
- **Stage deterministically, apply verbatim.** Build the new names/descriptions in a script → JSON plan → feed the strings verbatim into `transaction` ≤4-op batches. Never retype a UUID or a long claim by hand.
- **Pace and verify.** A few seconds between write batches; verify-read periodically (re-read the node, confirm the new name AND an advanced `last_modified` — never trust `ok:true` alone). Never reindex under throttle.

---

## System overview

The user has up to four complementary layers:

1. **Workflowy** — system of record and second-brain wiki. Holds tasks, projects, references, the journal, and (optionally) a Distillations subtree. Required.
2. **Additional services** *(optional — most users will skip this layer)* — declared in `$SECONDBRAIN_DIR/memory/services.md` if and only if the user wants extra surfaces. Each entry names a service (e.g. an ink-capture device, a document store, a reading-queue API), its MCP namespace, the workflows it participates in, and how to health-check it. The skill is service-agnostic; the file's absence is normal, not a fault.
3. **Claude** — bidirectional reader and writer.
4. **secondBrain directory** (`$SECONDBRAIN_DIR/`) — the operational outside. Holds drafts, session logs, the cached node-ID memory file, the services configuration, and external-facing briefs.

The discipline that turns this into a wiki rather than a notebook is **writing synthesis back**. Sessions that produce a useful summary, comparison, or framework should end with atomic notes saved into Workflowy (under a Distillations subtree if the user follows that pattern), mirrored into the right pillar/theme, and a session log entry written to `$SECONDBRAIN_DIR/session-logs/`. The session log is a local-filesystem record — the local file is the audit trail. (A user who wants an in-Workflowy navigation node for logs may keep one, but it is optional, not a dual-write obligation; the filesystem is canonical.)

---

## Bootstrap (run BEFORE every workflow)

A three-step probe at session start. All steps must complete before any workflow proceeds.

### Step 0 — Tool availability probe

Confirm the MCP tool surface this skill needs is actually loaded **before** doing anything else. MCP servers can be disabled, removed, or fail to load silently; the skill must fail loud rather than silently degrade to filesystem-staging.

**The probe is unconditional.** Run it on every wflow session, regardless of host. Hosts lazy-load MCP tools on first `tool_search`; "the MCP server was working last session" is not a substitute for probing this session. Deferring the probe until a write is imminent is how the gap bites — the agent forgets, captures degrade silently to disk, and the user finds out a session later.

#### What to probe

- **`workflowy:*`** — required for every workflow. Probe always.
- **`Filesystem:*`** — required to read drafts and memory files. Probe always (skip only in Claude Code, where `Read` / `Write` / `Bash` are native).
- **Each additional service** *(only if the user has any)*. Check whether `$SECONDBRAIN_DIR/memory/services.md` exists; if it does, read it once at session start and probe each service's `bootstrap_probe` tool. Skip a service whose `bootstrap_probe` is `none`. Skip services whose `participates_in` doesn't intersect the workflow you're about to run. **If the file doesn't exist, skip this step entirely** — many users will run with Workflowy-only and that's a fully supported configuration.

#### How to probe

Use the **exact server name** as the `tool_search` query — descriptive phrases match the wrong MCP server (`"filesystem write file"` loads Netlify, `"list_allowed_directories"` loads Gmail, only `"Filesystem"` works):

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
2. Name the missing tool and tell the user how to fix it: check the MCP server configuration for their host (e.g. `claude_desktop_config.json` for Claude Desktop). Confirm with the corresponding health-check tool.
3. Ask explicitly before staging a draft to disk. Never silently degrade. If the user consents, write to the universally allowlisted fallback path with the failure mode tagged in the filename (`...-mcp-down.md`) so the next session's Step 1 resumes execution.

#### Degraded protocol (the third state)

The Fail-loud protocol above fires on **unreachable** tools. A third state sits between healthy and unreachable: **degraded** — the MCP server process is responsive (auth fine, name index populated, no in-flight walks stuck) but upstream Workflowy API calls are timing out. `health_check` / `workflowy_status` returns `status: "degraded"` with `api_reachable: false` and `authenticated: true`. This is an upstream blip, not an MCP fault, and it does NOT warrant the Fail-loud stop-everything path.

When `degraded` is observed:

1. **Filesystem-only work continues normally** — taxonomy / memory edits, draft writing and review, audit, design discussion, session-log appends to `$SECONDBRAIN_DIR/session-logs/`. Each is a productive use of the wait. Do not stop and do not ask permission; the API is expected to recover.
2. **Defer write-bound workflows** — anything requiring `create_node`, `edit_node`, `move_node`, `batch_create_nodes`, `transaction`, `reorder_nodes`, or any other Workflowy mutation. Tell the user explicitly: "Workflowy is degraded; write plan queued for when the API recovers." Hold the routing table in working memory, or commit it to a draft under `$SECONDBRAIN_DIR/drafts/` only if the session is about to end.
3. **Re-probe periodically.** `workflowy_status` is sub-second even when degraded. Check every few minutes; the moment `status: "ok"` returns, fire the queued writes.
4. **Do not stage drafts during degraded state** unless the user explicitly asks — staging-as-fallback is for *unreachable*, not *degraded*. Mis-routing degraded as unreachable wastes a productive filesystem window with permission-asking overhead; mis-routing it as healthy produces hung tool calls that race the transport timeout.

The reflex on a single failed write is to escalate to "MCP is broken, let's stage". On a degraded probe that reflex is wrong — the right action is to keep working on what doesn't need the API while the API recovers.

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

The MCP server keeps a disk-persisted name index at `$WORKFLOWY_INDEX_PATH` (conventionally `$SECONDBRAIN_DIR/memory/name_index.json`; unset disables persistence). It survives restarts; mutations checkpoint every 30 seconds; coverage grows from every walk any tool performs and converges via explicit reindex passes — there is no in-process background refresher.

**The fast retrieval surface to reach for first:**

- `node_at_path(path=["Top", "Sub", "Target"])` — walks a hierarchical path of node names. ONE `list_children` call per segment, so resolution is O(depth), not O(tree). Use this whenever you know where a node lives but not its UUID; visited nodes also feed the persistent index, accelerating future short-hash lookups under that branch.
- `resolve_link(link="...", search_parent_path=[...])` — built for the "I have a Workflowy URL, give me the node info" workflow. Pass the URL or short hash via `link`; pass an optional parent-name path via `search_parent_path` to scope the walk to a single subtree. Returns full node info on success.

**Short-hash auto-walk (fallback):** every `node_id` parameter accepts the 12-char URL-suffix or 8-char doc-prefix forms. On a cache miss the resolver runs a 20-second walk (the interactive budget; exhaustive coverage belongs to the scheduled reindex). For trees over ~50 k nodes the fallback is unreliable — **prefer `node_at_path` or `resolve_link` with a parent path** rather than relying on the auto-walk.

**Building coverage explicitly:** `build_name_index(parent_id=...)` walks a single subtree deeply; the persistent index makes the work cumulative across sessions. For a one-shot deep index pass from the shell (independent of any running MCP), run `wflow-do reindex --timeout-secs 0 --patient --root <UUID> [--root <UUID> ...]` — the two flags make the pass coverage-complete (patient retry of rate-limited branches, no per-root deadline); without them each root gets only the interactive 20-second budget. Merges results into the same persistent file and reports per-root coverage. Useful for fresh installs, recovery from sparse coverage, and as a scheduled nightly job.

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

The detailed implementation of each workflow lives in the user's customised copy of this file at `~/.claude/skills/wflow/SKILL.md`. The category list below is the framework; the prompts and tool sequences are user-specific. **The numbers are load-bearing:** cross-references elsewhere in this file (and in the memory-file schemas) cite workflows as "Workflow 6", "Workflows 9–14", etc. — they refer to the numbering below.

### Operate (day-to-day)

1. **Daily prioritisation** — surface today's todos, overdue items, and recently-modified projects via `daily_review`. Suggest a focus block for the morning.
2. **Weekly prioritisation** — review last week's completions and unmoved items. Identify what to drop, what to escalate.
3. **Monthly prioritisation** — set themes; promote/demote pillar work.
4. **Task capture** — infer domain from content; place under the appropriate Tasks subtree as a Workflowy todo. **When capture is incidental** (an action item surfaces mid-chat rather than being the user's primary intent), split into two messages: (1) "I can capture this as `#TODO @<assignee>` under `<inferred-domain>` — confirm?", (2) on user yes, the actual write. When the user's *primary* intent is capture ("capture: X"), the offer collapses to a domain confirmation in the first response and the create lands on yes.
5. **Project status** — for a named project, return current state (todos open, recent activity, blockers tagged).
6. **Inbox triage** — walk Inbox children, route each to the right subtree (or delete).
7. **Reading list management** — surface WIP reading, recent additions, items tagged for distillation.
8. **Journal check-in** — append a dated entry under the Journal node.

### Synthesise (slower, compounding)

9. **Distil single source** — turn a paper / article / chat into atomic notes. Place each note under the right pillar/theme. Mirror cross-cutting notes via `create_mirror(canonical_node_id, target_parent_id, pillar?)` — the mirror's name is copied from the canonical and its description gets `mirror_of: <canonical_uuid>` so `audit_mirrors` can surface drift later. Pass `pillar` when the canonical lacks a `canonical_of:` marker; existing markers are never overwritten.
10. **Sweep an existing node into the second brain** — take an already-in-Workflowy node (reference document, transcript, discussion thread) and distil its subtree into the canonical pillars/themes:
  1. **Resolve precisely.** `resolve_link` with a scope hint (`search_parent_id` / `search_parent_path`), never an unscoped walk — unscoped on a large workspace times out at the 20 s subtree budget before finding anything.
  2. **Establish a clean read path** per Read-path discipline above — freshness check, reconstruct or reindex, `read_batch` for live reads, export as last resort.
  3. **Apply the routing-plan gate (pattern 1) and the novelty check** before writing. A sweep is the highest-risk place to silently duplicate a canonical that already covers the source.
  4. **Route per `distillation_taxonomy.md`** — pillar / theme / cross-pillar classification reads from the taxonomy, not from the sweep content. The taxonomy is canonical on borderline cases.
  5. **Write via the head-batch-mirror sequence (pattern 2).** Single cluster-head create, batched atomic notes, selective mirrors, session-log entry.
11. **Distil reading list (batch)** — process the reading queue in one pass, producing a session log entry.
12. **Cross-system research** — query Workflowy for everything related to a topic. If `services.md` exists and any of its entries have `participates_in: retrieval`, query those services too and merge the results. Surface as a synthesis with citations back to source nodes.
13. **Extract from additional services** *(only if `services.md` declares any with `participates_in: extraction`)* — route the service's outputs (marginalia, highlights, annotations) into Distillations. The exact extraction call lives in the service's MCP namespace; consult `services.md` for the namespace and the relevant tools.
14. **Synthesis capture** — convert a chat-produced framework or comparison into an atomic note in Workflowy.
15. **Review surface** — surface notes tagged `#revisit` (or similar), prompt for spaced-repetition action.

#### Three patterns every synthesis workflow shares

1. **The routing-plan gate.** Before writing anything to Distillations, build a draft routing table — *candidate atom name; destination pillar; mirror destinations; sources integrated* — and gate on user confirmation. Pair this with a **novelty check**: for each candidate atom, run a narrowed `find_node` / `search_nodes` (with `use_index=true` against the destination pillar UUID) for the key concept; if a canonical already covers the same ground, propose a mirror or backlink rather than a new atom. Both passes are cheap and both reduce drift between the user's mental model and what lands in the graph. Worth writing into "Distil single source", "Distil reading list", and "Synthesis capture" alike.
2. **The head-batch-mirror sequence.** Once the routing table is confirmed, execute in this order: (a) `create_node` the source cluster under its destination parent with the destination's explicit cached UUID; (b) `batch_create_nodes(operations=[...])` for the atomic-note children with `parent_id` populated on every operation (operations-array shape per UUID Parameter Discipline rule 4); (c) `create_mirror` selectively — mirror an atom into a destination only when it is a **substantive contribution** to that destination's canon, skip when it merely touches; (d) write the session log to `$SECONDBRAIN_DIR/session-logs/` (filesystem-canonical; no Workflowy write required for the log); (e) only fall to `transaction.move` as a corrective when a placement misses. This sequence has roughly half the failure surface of create-then-move chains and should be the prescribed default.
3. **The Journal-scan + range-stamp convention.** When the user keeps a journal, every synthesis or review session begins with a scan of Journal entries over the period since the last journal-bearing session log. The session log's description (or first child) MUST stamp `Journal range covered: YYYY-MM-DD → YYYY-MM-DD` — the next session reads that stamp to know where to pick up. First-run convention: if no prior journal-bearing session log exists, scan back ~30 days. Lift only principle-level insights as atomic notes; personal-context entries stay in the Journal. Skip this pattern entirely if the user doesn't keep a journal.

#### Discipline lessons (each one paid for by an eval failure)

These shorten the gap between "the skill said X" and "the agent actually did X" on situations the skill used to leave implicit. Every phrase below is the lexical anchor for a specific failure mode the wflow eval suite caught — keep them present as the skill evolves.

- **Atomic notes use `batch_create_nodes` (not `insert_content`).** `insert_content` parses an indented text block into a hierarchy of names — it has no per-line description channel, so source attribution silently drops on every child node it creates. `batch_create_nodes(operations=[{name, description, parent_id}, ...])` accepts a description per operation and is the only insertion path that preserves the "source attribution in the description" rule of Atomic Note Discipline. (Eval Test 5: descriptions on 4 atomic notes were not set per-note in the `insert_content` call.)

- **Backlinks are `<a href="https://workflowy.com/#/<short-hash>">name</a>`, not raw UUIDs in prose.** The literal HTML anchor is the form the Workflowy UI renders as a clickable link. Writing "see canonical 550e8400-..." as plain text means the link is invisible at the UI layer and audit tooling can't follow the reference. Always use the anchor form in descriptions. (Eval Test 11: description named the canonical UUID in plain text but did not write a Workflowy `<a href>` backlink.)

- **Mid-session task capture is offer-then-confirm.** When a task surfaces during an unrelated chat (i.e. capture is incidental, not the user's primary intent), split into two messages: (1) "I can capture this as `#TODO @<assignee>` under \<inferred-domain\> — confirm?" (2) on user yes, the actual `create_node` / `smart_insert` write. Collapsing both into a single move denies the user the chance to redirect or veto, and inferred-domain mistakes compound silently. When the user's primary intent IS task capture (they opened with "capture: X"), the offer collapses to inferred-domain confirmation in the first response and the create lands on yes — fine. The two-message rule applies only when capture is incidental. (Eval Test 24: captured directly without an explicit confirmation step.)

- **Daily prioritisation produces a priority-ordered briefing, not a per-source paste.** The output of the daily-review steps is *synthesised* into a single ranked list ordered by urgency × strategic importance against the week's priorities, headed by "the single most important thing today". A flat per-system summary ("Workflowy: X overdue. \<service\>: Y fresh. Session logs: Z open.") is data, not a briefing — surface that level only when the user asks for it. (Eval Test 1: final response was a flat per-system summary, not priority-organised.)

- **Explicit-check discipline.** Every workflow step that says "scan X" or "check Y" must explicitly state the result, including the negative. "Reading list: 3 items, none with action verbs." / "No fresh material from \<service\>." / "Session logs since yesterday: none open." Silent omission breaks the audit trail of what was checked. The discipline also requires re-issuing data calls at workflow start even when "the same data" was loaded earlier in the session — workflow timing changes the answer. (Eval Test 2: did not state "no actions detected"; Test 8: skipped re-issuing recent-changes / session-log calls at journal time.)

- **Cross-system retrieval requires both name AND content search per source.** Index-fast-path (`use_index=true`) catches name matches in O(1); description-touching content needs a live walk (`max_depth=5+`). For each external service whose `participates_in: retrieval` is declared in `services.md`: surface search AND content/grep search, even when the surface returned empty (a content search may catch text inside documents whose titles don't match). Skipping the content search because the surface returned empty is the failure mode the eval suite caught. (Eval Test 4, 2026-05-09: only the title search ran on the configured service; no follow-up grep keyword search.)

- **Uniform per-pillar mirroring.** When a synthesis produces 3+ atomic notes and two or more of them substantively bear on the same destination pillar, *all atoms* in the synthesis that touch that pillar get mirrored there — not just the most obviously-anchored ones. Per-atom mirror judgement is where drift creeps in: the agent picks the strongest contributors and silently drops a third atom even though it also bears on the destination. If an atom is genuinely orthogonal, skip the mirror but mark it explicitly in the routing table from pattern 1 above. (Eval Test 9: first "causality not difficulty" note arguably touches leadership but had no Lead mirror.)

- **Audit `scope_resolved` on every scoped call.** Every parent-scoped tool (`create_node`, `batch_create_nodes`, `insert_content`, `create_mirror`, `list_children`, `find_node`, `search_nodes`) returns `scope_resolved: "workspace_root"` or `scope_resolved: "scoped:<uuid>"`. Read it after every call where parent_id was null / omitted and confirm the scope matches the intent — `workspace_root` must mean "intended workspace root", not "forgot the UUID". If it doesn't, halt and resolve explicitly before any follow-up write. For batched `create_mirror` passes, use `dry_run=true` first: it returns `scope_resolved`, the would-be `mirror_name`, and `would_annotate_canonical` without writing — verifiable preview before production. (Underlying hazard: UUID Parameter Discipline above.)

---

## Explicit-check discipline (applies to every workflow, not just synthesis)

Any workflow step that says "scan X" or "check Y" must explicitly state the result, *including the negative*. The user cannot tell, from a silent skip, whether you ran the check and found nothing, ran it and forgot to surface the result, or skipped it entirely — silent omission breaks the audit trail of what was checked. Three rules make it auditable:

1. **Re-issue data calls at the start of each workflow**, even if "the same data" loaded earlier in the session. Resident knowledge from Bootstrap or a prior workflow is not a substitute for a fresh call — workflow timing changes the answer (a task completed since the morning probe, a fresh session log written, new reading material arriving). The journal check-in is the canonical place this fails: the morning probes loaded today's tasks / completions / fresh activity, and a journal-time check-in skips the calls because "it was just loaded". Re-issue them.
2. **Report the negative result as a sentence, not as silence.** Phrasings: "Reading list: 3 items, none with action verbs." / "No fresh material from `<service>`." / "Session logs since yesterday: none open." A briefing that omits a section the user expects (because that section was empty) reads as if the check was skipped.
3. **The discipline applies to triage and journal workflows even when the source is empty.** Say "no actions detected" / "nothing to triage" / "no items match `<filter>`" rather than producing an empty response. The discipline is *saying-it-explicitly*.

---

## Output formatting for content sent to Workflowy

Content inserted into Workflowy via `insert_content` must use **2-space hierarchical indentation** (two spaces per level, no bullet characters):

```
Top level item
  Child item
    Grandchild item
  Another child
```

Use `-` bullets only when outputting to the user in chat. Do not use `-` bullets or markdown headers in content sent to Workflowy.

### Ordered lists must land in their intended order

**Sequential content — numbered steps, ranked priorities, chronological entries, anything where position carries meaning — must read top-to-bottom in the intended order after the write.** This is not automatic. `insert_content`, `batch_create_nodes`, and any sequential `create_node` loop plant each new sibling at the *head* of the parent's children unless an explicit `priority` is set (Workflowy's `POST /nodes` defaults a node with no priority to position 0). The last item created therefore appears first, and an N-line ordered list arrives **fully reversed** — step 5 above step 1, the lowest priority above the highest, the narrative running backwards.

The discipline whenever order matters:

1. **Treat any list with numbering or narrative flow as order-bearing** — distillation sequences, priority briefings, procedure steps, timelines, ranked options. When in doubt, assume it matters.
2. **Verify after the write, not just before.** Read the parent back (`list_children` / `get_node`) and confirm the sequence reads in the intended direction. Reversal is the default failure, not an edge case — check for it every time.
3. **Repair with `reorder_nodes(parent_id, node_ids[])`** — pass the node IDs in the intended top-to-bottom order. The tool's reverse-priority-0 primitive (Bootstrap rule 8) lands them in exactly that sequence regardless of how they currently sit. This is the canonical fix; never hand-roll a forward-priority loop to correct order.
4. **Prefer building the structure correctly in one pass** where the tool allows it — a single `insert_content` payload preserves the parent→child *hierarchy*, but sibling order within a level can still invert, so the read-back in step 2 is mandatory regardless of which tool wrote the list.

The rule in one line: **an ordered list is not "inserted" until a read-back confirms it reads forwards.**

---

## End-of-session discipline

Every session that mutated the second-brain should:

1. Write a session log entry to `$SECONDBRAIN_DIR/session-logs/YYYY-MM-DD-<brief-name>.md`. The session log is a local-filesystem record (filesystem-canonical); a Workflowy navigation node for logs is optional, not a dual-write obligation.
2. Move any pending drafts from `drafts/` to `session-logs/` once their writes have landed.
3. **Update the canonicals when the session made a structural change to Workflowy** — see the next subsection. This is the discipline that prevents `memory/workflowy_node_links.md` and `memory/distillation_taxonomy.md` from drifting silently out of sync with the live tree.
4. **If this session resumes a previously-partial one, write a new dated file in `$SECONDBRAIN_DIR/session-logs/` — don't overwrite the original.** Use a `-resumption` or `-completion` suffix. The original log captures the failure mode and routing decisions; the resumption log captures the resolution and final tally. Both stay readable, the audit trail is two-stage, and any later session reading the directory sees the full arc instead of a file that's been overwritten.

### Structural-change discipline (update the canonicals)

Two filesystem artefacts hold the cached Workflowy structure the skill depends on:

- **`$SECONDBRAIN_DIR/memory/workflowy_node_links.md`** — structural-node and priority-node UUID cache.
- **`$SECONDBRAIN_DIR/memory/distillation_taxonomy.md`** — pillars, themes, register classifications, routing rules (only required if the user follows the synthesise workflows; if there are no Distillations, skip this file entirely).

Both files are **canonical at the `$SECONDBRAIN_DIR/memory/` path**. They are not shipped in the public skill bundle (it would be impossible to ship a generic one — every user's pillars, structural nodes, and routing rules differ). The skill's bootstrap creates them on first use from the schemas in the Memory file schemas section below.

**Update obligation.** After any session that makes a **structural change** to Workflowy, before closing the session, update the relevant canonical(s). Structural changes are:

1. **New pillar / pillar rename / pillar UUID change** → `workflowy_node_links.md` synthesis-write-targets table + `distillation_taxonomy.md` Pillars table.
2. **New theme / theme rename** → `distillation_taxonomy.md` Themes table.
3. **New structural node under a pillar or theme** (e.g. a new sub-grouping under a pillar's Distillations node, a new theme distillation root) → corresponding section in `workflowy_node_links.md`.
4. **New Reading-List sub-node** (e.g. a new current-reading thread, a new bulk-import source) → "Reading List sub-nodes" table in `workflowy_node_links.md`.
5. **New atomic note that earns a long-lived cross-reference** (typically a synthesis or meta-synthesis node that other notes will backlink to) → the relevant atomic-notes table in `workflowy_node_links.md`. Ephemeral draft notes do NOT belong here.
6. **A previously-cached node was deleted, moved out of scope, or its parent changed materially** → update or remove the entry; add a one-line change-log entry naming the move so a future reader can correlate the cache delta with the Workflowy delta.
7. **Routing-rule change** (a new convergence concept reclassified between cross-pillar and pillar-native; a register reclassification of a thinker; a new pillar with its own register) → `distillation_taxonomy.md` routing-nuance section.

**What is NOT a structural change.** Adding a regular atomic note that nothing else will backlink to; renaming an atomic note; capturing a journal entry; completing a task; routine inbox-triage moves; reading-list imports of individual articles. The canonicals cache *structure*, not *content*.

**How to apply.** When the session arc closes and any of points 1-7 above applies:

1. State explicitly which point applies — surfaces the change so the user can sanity-check it.
2. Edit the relevant canonical at its `$SECONDBRAIN_DIR/memory/` path.
3. Append a change-log entry to `workflowy_node_links.md` under a "Change log" section dated today, naming the change in one or two sentences. The change log is what lets a future session diff "what changed in Workflowy structure since I last looked" without walking the tree.
4. If the user distributes the skill bundle to another host, remind them that the bundle ships generic — their personal data files live only at `$SECONDBRAIN_DIR/memory/` and are read at session start; the skill bundle alone does not carry the canonicals.

**Why this is the discipline, not a soft suggestion.** When the canonicals drift, the agent reads structurally-outdated UUIDs at Bootstrap and routes content to nodes that no longer exist (or that have moved). The update discipline is what prevents that.

If the MCP wedges mid-session (a write returns `Tool execution failed` and `workflowy_status` shows degraded health):

1. Stop writes immediately.
2. Save the in-flight plan as a markdown file in `$SECONDBRAIN_DIR/drafts/` with the date prefix and a clear "RESUME EXECUTION" header.
3. Tell the user the next session will resume from the draft.

#### Upstream rate-limiting (HTTP 429) runbook

Distinct from a crashed server and from a transport-layer drop: here Workflowy is alive and authenticating fine but **throttling the account's quota**. In a bulk write session this is the single biggest time-sink, so treat it as a normal operating condition.

- **Symptom.** Mid-session a write returns a structured rate-limit error rather than succeeding. Since 2026-06-01 calls issued inside an open window **fail fast** (sub-millisecond) instead of hanging the full ~4-min transport timeout — so the modern symptom is a fast `proximate_cause: "rate_limited"` envelope, not a hang. (A hang now means a genuinely stuck call, not a 429 — diagnose that as upstream-unreachable / local-queue, not rate-limiting.)
- **Diagnosis.** The failing call's own error envelope now carries `proximate_cause: "rate_limited"`, `retryable: true`, and `retry_after_secs: N` (2026-06-17) — read N straight off the error, no separate status poll needed. For the fuller posture, `workflowy_status` shows `degraded_kind: "rate_limited"` + a non-null `retry_after_remaining_ms`; `probe_suppressed: true` confirms the probe is short-circuiting on cached posture so it does not burn the quota you are waiting to recover.
- **Trip point.** Roughly the **third write-batch** in quick succession within one working window. Reads add pressure but writes dominate; a `transaction` of several ops counts as one batch.
- **Recovery.** Wait the error's own `retry_after_secs` (the authoritative number); absent that, **~90 seconds** reliably clears the window, and `workflowy_status.retry_after_remaining_ms` counts it down. Do **not** poll `workflowy_status` during the window (the probe holds too). When it clears, the next call goes straight through. If the window tripped mid-`insert_content`, the partial envelope's `created_count` + `last_inserted_id` tell you where to resume — re-issue the remaining lines under `last_inserted_id` once the window clears.
- **Shared-bridge corollary.** Multiple MCP servers (Workflowy, Filesystem, and any others) share one local MCP client. A held Workflowy 429 call blocks that shared client, so calls to *other* servers queued behind it also time out — they are **not** independently broken. Local-file work proceeds fine *between* Workflowy calls; schedule notes, logs, and memory edits into the Workflowy cool-down gaps.
- **Sustainable cadence.** A few small write batches, then a verify-read, then pause. One small batch per working turn is the safe rhythm once the limit is warm.

#### The write ok-counter is not a commit signal

`per_tool_health.<tool>.ok` (including `transaction.ok`) increments on **receipt** of the call at the server, **not** on durable commit. When a write's response is lost to a 429 hold, the operation **rolls back** but the counter has already advanced — and a `status: "applied"` returned just before a window closes is not sufficient on its own either.

- A rising ok-count does **not** prove a write landed. **The only reliable confirmation that a mutation persisted is a live re-read of the node.** Make verify-after-write the discipline for any batch that ran near a rate-limit window.
- This is why verify-before-reapply is safe: re-reading first means a rolled-back batch is reapplied exactly once and a landed batch is skipped — no double-apply risk.

---

## MCP failure-handling: server-side vs transport-layer

The Workflowy MCP exposes `workflowy_status` as the definitive arbiter of *where* a failure happened. Use it whenever you see two or more consecutive failures of the same call shape.

`workflowy_status` returns, among other fields: `paths.<tool>` (`healthy` / `degraded` / `failing` / `untested`), `last_failure` (`{tool, at_unix_ms, reason, proximate_cause}` for the most recent server-recorded error), `per_tool_health.<tool>` (ok / err counts and rate), `degraded_kind` (the cause of `degraded`), `retry_after_remaining_ms` (ms left in an open 429 window, else `null`), and `probe_suppressed` (`true` when the probe short-circuited on cached posture rather than issuing an HTTP call — it does this inside an open `retry_after` window so the diagnostic doesn't consume the quota it is measuring).

### The parity-test heuristic

If you just observed N failures of `<tool>` but `workflowy_status` reports `paths.<tool>: healthy` with `0` errors in `per_tool_health.<tool>` and `last_failure: null` (or naming a *different* tool), **the failures never reached the server** — this is a client-side transport bug, not a server problem. Retrying makes it worse: it accumulates client state without ever touching upstream, and partial state already created cannot be cleaned up via the MCP because the cleanup calls are dropped too.

When the parity test detects a transport-layer failure:

1. **Stop the call sequence immediately.** Do not retry the failed call shape.
2. **Capture state.** Note any orphan UUIDs created during the broken window (the `create_node` success message names where each node landed).
3. **Tell the user.** Recommend manual cleanup in the Workflowy web UI — faster than the retry loop.
4. **Fall back to** `wflow-do` for any remaining writes (Bash dispatch is independent of MCP tool dispatch, so it reaches the API even when MCP calls are silently dropped).
5. **Treat further MCP-routed writes as suspect** until `workflowy_status` and your local view agree again.

### Schema-empty failure mode (the dishonest-success cousin)

A shape that looks like the parity-test pattern but is structurally different: every parameter-bearing tool behaves as if called with no arguments. `get_node` rejects with `missing field 'node_id'` on a known-good UUID; `list_children` silently returns the workspace root regardless of what you passed; `workflowy_status` (the one parameterless tool) works fine.

Root cause: the server published a JSON schema with `"properties": {}` for every parameter-bearing tool (the `Parameters<T>` wrapper was renamed server-side — rmcp-macros identifier-matches that exact name on the function arg type to find the schema), so the client validated arguments against the empty schema and stripped them before they reached the server. Server-side counters look healthy because the call did reach the server — just with an empty argument object.

**Diagnosis:** call `get_node` with a known-good UUID; if the reply is `missing field 'node_id'`, the schema is empty. Confirm by reading the tool list — every parameter-bearing tool's `inputSchema.properties` should be non-empty. **Mitigation:** the fix is server-side; until it ships, route writes through `wflow-do` (it bypasses the MCP tool surface, so it's unaffected) and tell the user about the schema-empty mode explicitly so they don't think they're imagining it.

### Truncation routing (once you've decided to recover)

The walk-shape rule (contract 4) decides *whether* to recover from `truncated: true`; this is *how*. Do NOT immediately retry — read `truncation_reason` and route:

1. **`timeout` or `node_limit`** — the subtree exceeds what one walk can cover. For **name-based queries** (`search_nodes`, `find_node`): call `build_name_index(parent_id=<scope>)` once to populate the persistent name index, then re-issue the original query with `use_index=true` (O(1), ignores the walk budget; name-only — description-content matching still needs a live walk, so narrow `parent_id` to a subtree for content queries). For **non-name queries** (`tag_search`, `find_backlinks`, `daily_review`, etc.): the index doesn't help — narrow `parent_id` / `max_depth` and re-issue, or stage the walk over multiple smaller subtrees.
2. **`cancelled`** — `cancel_all` was bumped during the walk. Safe to retry as-is.

### Server-side failures

When `last_failure` IS populated and matches what you observed, the server saw the failure. Read `data.proximate_cause` and route:

- `not_found` — the server already retries 404s with backoff, so an exhausted-retry `not_found` means the node really doesn't exist or upstream propagation is unusually slow. Verify with `list_children` of the parent.
- `timeout` — narrow scope (lower `max_depth`, pass a `parent_id` / `root_id`) before retrying.
- `cancelled` — `cancel_all` preempted the call; safe to retry.
- `rate_limited` — upstream 429; the account quota is spent, NOT a server fault. Read `retry_after_remaining_ms` from `workflowy_status` and wait it out (see the rate-limit runbook above). Do not retry inside the window.
- `upstream_error` — Workflowy backend; back off and retry.
- `auth_failure` — API key wrong or revoked; tell the user.
- `lock_contention` / `cache_miss` — server-internal; retry after a short wait.

### Fail-closed warning on `create_node`

When reads or mutations have failed in the last 30 s, a successful `create_node` response is suffixed with a `⚠ DEGRADED: server in degraded state — <tool> failed N ms ago …` banner. When you see it, **do not chain follow-up writes on the new UUID** until `workflowy_status` shows the previously-failing tool back to `healthy`. Chaining `create → move → move → …` while the move path is wedged is what scatters orphaned nodes across the tree.

---

## Workflowy navigation notes

- **Prefer `create_node(name, parent_id=<UUID>)` over create-then-move.** When you know the destination, pass `parent_id` directly. Create-at-root-then-move doubles the failure surface — a created-at-root node can be stranded for hours if follow-up `move_node` calls drop at the transport layer. The success message names the resolved parent, so you can audit placement at the moment of creation.
- Prefer `list_children` with a known node ID over search for navigating the main structure.
- `move_node` reliability (separate from the parameter-encoding issue in UUID Parameter Discipline): the server retries 404s automatically (propagation lag). For non-404 failures, retry 2–3 times and re-fetch the parent's children with `list_children` between attempts to avoid acting on stale IDs.
- `edit_node`: the server splits `name` + `description` into two POSTs internally to dodge an upstream field-loss bug, so passing both together is safe.
- When filing links, title raw URLs by fetching the page rather than storing a bare URL.

---

## Error handling

- If a structural node is not found (e.g. no "Tasks" node), tell the user, confirm the node name, and fall to Bootstrap first-use population.
- If no priority nodes exist yet (first use), skip the "load context" step and note that the user should set monthly priorities first to establish themes.
- If `WebFetch` fails on a reading-list URL, note the failure and continue with the other items.
- If a triage source (e.g. the Inbox) is empty, say so and suggest another workflow.
- If `workflowy_status` itself fails, the MCP is wholly unresponsive — recommend a host restart and stop further work in this session.

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
| Themes (parent)            | <TBD>   | <TBD>         | parent for theme mirror destinations |

(Session logs are not a Workflowy write target — they are written to `$SECONDBRAIN_DIR/session-logs/` on the local filesystem. See End-of-session discipline.)

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

This template is generic. The user's customisations — preferred wording, project-specific routing rules, detailed workflow scripts — should be edited into their own installed copy of the skill (typically at `$HOME/.claude/skills/wflow/SKILL.md` for local Claude Code, or the uploaded skill on claude.ai). Treat the template version (in this repo, at `templates/skills/wflow/SKILL.md`) as the upstream; pull updates into your copy manually when desired.

**Propagation chain — how an edit to the template reaches a running session.** The canonical template lives at `templates/skills/wflow/SKILL.md` in the workflowy-mcp-server repo. To ship it to the claude.ai web/mobile surfaces it is bundled by `scripts/bundle-skill.sh` into `dist/wflow.skill.zip` (the repo's auto-bundle hook does this automatically on any edit under `templates/skills/wflow/`), and the user then re-uploads that zip at claude.ai → Settings → Skills and starts a fresh session. The full chain is `canonical templates/skills/wflow/SKILL.md → scripts/bundle-skill.sh → dist/wflow.skill.zip → manual re-upload`. Local Claude Code reads the installed copy from disk directly, so it sees changes without the bundle step; the upload is only needed for the web/mobile surfaces. The user's personal data files (`workflowy_node_links.md`, `distillation_taxonomy.md`, `services.md`) are never bundled — they live only at `$SECONDBRAIN_DIR/memory/` and are read at session start by the Bootstrap.
