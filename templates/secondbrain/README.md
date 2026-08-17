# Your claim-based second brain (the operational outside)

This directory holds the part of your second brain that lives *outside* Workflowy — the cached node IDs, in-flight drafts, session logs, and briefs intended for collaborators or future Claude sessions. The second brain proper lives in Workflowy: a small set of durable **regions**, and under them nothing that is not a **claim** you could argue with or an **action** someone will take.

Read [`docs/METHOD.md`](../../docs/METHOD.md) before you populate any of this, and write your regions down before you capture anything. The files here are the method's working memory; they do not supply the method. In particular `memory/distillation_taxonomy.md` is where your region list actually lives, and routing from a written list rather than from the mood of the moment is the whole point of having one.

Two decisions govern everything filed below. **Divide the space before you fill it**, into conceptual regions named as activities ("how we build", "how we decide"), theme regions for what cuts across them, and life regions for what is not the practice at all. **Admit only claims and actions**: a claim states something a reader could contradict, an action names an owner and a date, and everything else is raw material that belongs in an inbox until someone turns it into one of the two or drops it. A claim that bears on several regions lives canonically in one and appears in the others as a mirror pointing back, which is what `create_mirror` writes and `audit_mirrors` checks.

**On tooling.** Reading and searching your tree is now done better by Workflowy's own tools than by this server; see [`docs/SURFACES.md`](../../docs/SURFACES.md) for which surface to use for what. This server, and this directory, carry the method rather than the retrieval.

The MCP server reads this directory through the `$SECONDBRAIN_DIR` env var (set in your MCP host config, e.g. Claude Code's `~/.claude.json` or Claude Desktop's `claude_desktop_config.json`). The persistent name index lives at `$WORKFLOWY_INDEX_PATH`, conventionally `$SECONDBRAIN_DIR/memory/name_index.json`. Both env vars are optional — leave them unset and the dependent features (the persistent index, the `review` tool's bucket-d session-log scan) simply skip. The wflow skill at `~/.claude/skills/wflow/SKILL.md` reads `$SECONDBRAIN_DIR/memory/workflowy_node_links.md` on every bootstrap.

**Memory files are user-specific data and are NOT shipped in the repo.** The `memory/` directory ships empty; the wflow skill creates `workflowy_node_links.md` (and, if you opt into the second-brain discipline, `distillation_taxonomy.md` and `services.md`) on first use, walking you through population. The schemas live inline in the skill at `templates/skills/wflow/SKILL.md` under "Memory file schemas" — no separate per-user templates in this repo. The MCP server writes `name_index.json` here automatically the first time it checkpoints.

## Layout

```
secondBrain/
├── README.md                       ← this file
├── memory/                         ← (empty in the repo; populated on first use)
│   ├── workflowy_node_links.md    ← created by the skill — cached structural node IDs
│   ├── distillation_taxonomy.md   ← (optional) authored by you — pillars, themes, routing
│   ├── services.md                ← (optional) authored by you — additional MCP services
│   └── name_index.json            ← persistent name index (auto-managed by the MCP)
├── drafts/                         ← distillations drafted but not yet written to Workflowy
├── session-logs/                   ← one markdown file per session that mutated the graph
└── briefs/                         ← documents for collaborators / future Claude sessions
```

## What goes in each subdir

### `memory/workflowy_node_links.md`

A markdown file with tables of structural node IDs. The wflow skill creates this file on first use (schema in the skill template) and reads it on every bootstrap so it doesn't have to re-walk the tree to find Tasks, Inbox, Journal, etc. Update it whenever a structural node moves or is renamed. Old entries (>7 days) are flagged for re-verification by the skill.

### `memory/distillation_taxonomy.md` (optional in name only)

Your regions, written down: the conceptual ones (called pillars in the skill), the themes that cut across them, the key thinkers, and the inbound routing rules. You author this once, before the synthesise workflows can run. It is nominally optional because the server runs without it, but the method does not: without a written region list you route by mood and file inconsistently, which is the failure this file exists to prevent. Keep it short enough to recite. Schema in the skill template.

### `memory/services.md` (optional)

Declares any MCP services beyond Workflowy (ink capture, reading queues, task systems, etc.) that the skill should probe alongside Workflowy. Skip the file entirely if Workflowy is your only surface.

### `memory/name_index.json`

The MCP server's persistent short-hash → full-UUID cache. Auto-managed: rehydrated from disk on server startup, checkpointed every 30 seconds when dirty, and grown by every walk any tool performs plus explicit `wflow-do reindex` passes (there is no in-process background refresher). Do not edit by hand.

### `drafts/`

In-flight distillation work. When a session produces atomic notes but doesn't finish writing them to Workflowy (most often because the MCP wedged), the partial work goes here as a markdown file with a date prefix. The next session reads `drafts/` first; if there's a pending plan, it asks the user whether to resume.

### `session-logs/`

A local mirror of any `Session logs` subtree you keep in Workflowy. Format: `YYYY-MM-DD-brief-name.md`, capturing routing decisions, source URLs, and any failure-mode observations. The Workflowy node is the navigation surface; the local file is the audit trail.

### `briefs/`

Documents intended for somebody other than you — Claude Code, an external collaborator, a vendor — that don't make sense to put in Workflowy itself.

## Discipline

- **Every node you file is a claim or an action.** If you cannot turn a source into one of the two, you have not finished reading it. A node that only names a topic is a container, and containers are where material goes to be forgotten.
- **Mirror sparingly.** Mirror a claim into a second region when it is a substantive contribution there, not when it merely brushes it. Roughly one node in ten is about right; mirroring everything reproduces the duplication the canonical rule exists to prevent, with markers on it.
- **Update `memory/workflowy_node_links.md` when structural nodes change.** The wflow skill validates entries older than 7 days but only when invoked.
- **Draft before write.** Any distillation involving more than three or four Workflowy mutations should produce a file in `drafts/` first. This protects against MCP wedges and gives you a chance to veto the routing before the graph mutates.
- **One session log per session that touched the graph.** Both the Workflowy node and the local file should agree.
- **Verify that the write landed.** An API acknowledgement is not evidence. A batch that half-applied looks identical to one that succeeded until you read it back.
- **Keep it shallow.** Region, source cluster, atoms. Three levels carries almost everything; the fifth almost never earns itself.
