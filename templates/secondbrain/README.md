# Your Second Brain (operational outside)

This directory holds the part of your second brain that lives *outside* Workflowy — the cached node IDs, in-flight drafts, session logs, and briefs intended for collaborators or future Claude sessions. The second brain proper (your knowledge graph of atomic notes, distillations, and structural nodes) lives in Workflowy.

The MCP server at `~/code/workflowy-mcp-server` reads from and writes to this directory. The wflow skill at `~/.claude/skills/wflow/SKILL.md` reads `memory/workflowy_node_links.md` on every bootstrap.

## Layout

```
secondBrain/
├── README.md                       ← this file
├── memory/
│   ├── workflowy_node_links.md    ← cached node IDs you've identified manually
│   └── name_index.json            ← persistent name index (auto-managed by the MCP)
├── drafts/                         ← distillations drafted but not yet written to Workflowy
├── session-logs/                   ← one markdown file per session that mutated the graph
└── briefs/                         ← documents for collaborators / future Claude sessions
```

## What goes in each subdir

### `memory/workflowy_node_links.md`

A markdown file with tables of structural node IDs. The wflow skill reads this at every bootstrap so it doesn't have to re-walk the tree to find Tasks, Inbox, Journal, etc. Update it whenever a structural node moves or is renamed. Old entries (>7 days) are flagged for re-verification by the skill.

### `memory/name_index.json`

The MCP server's persistent short-hash → full-UUID cache. Auto-managed: rehydrated from disk on server startup, checkpointed every 30 seconds when dirty, refreshed by a background walk every 6 hours. Do not edit by hand.

### `drafts/`

In-flight distillation work. When a session produces atomic notes but doesn't finish writing them to Workflowy (most often because the MCP wedged), the partial work goes here as a markdown file with a date prefix. The next session reads `drafts/` first; if there's a pending plan, it asks the user whether to resume.

### `session-logs/`

A local mirror of any `Session logs` subtree you keep in Workflowy. Format: `YYYY-MM-DD-brief-name.md`, capturing routing decisions, source URLs, and any failure-mode observations. The Workflowy node is the navigation surface; the local file is the audit trail.

### `briefs/`

Documents intended for somebody other than you — Claude Code, an external collaborator, a vendor — that don't make sense to put in Workflowy itself.

## Discipline

- **Update `memory/workflowy_node_links.md` when structural nodes change.** The wflow skill validates entries older than 7 days but only when invoked.
- **Draft before write.** Any distillation involving more than three or four Workflowy mutations should produce a file in `drafts/` first. This protects against MCP wedges and gives you a chance to veto the routing before the graph mutates.
- **One session log per session that touched the graph.** Both the Workflowy node and the local file should agree.
