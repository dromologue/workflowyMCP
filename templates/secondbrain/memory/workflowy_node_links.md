---
name: Workflowy Node Links
description: Cached Workflowy node IDs for structural and pillar nodes — avoids repeated find_node calls
type: reference
canonical_path: $SECONDBRAIN_DIR/memory/workflowy_node_links.md
---

The wflow skill reads this file on every bootstrap. Replace `<TBD>` placeholders with the actual UUIDs from your Workflowy account. Update `Last Verified` whenever you confirm an ID still resolves.

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

The set of nodes that Workflow 6 (Inbox Triage) sweeps in order. Append rows to add a new triage target without changing the skill. Each entry is processed in sequence; items move out into the canonical archive locations defined in `distillation_taxonomy.md`'s "Inbound routing table." A typical setup includes the master Inbox plus the reading-queue roots; extend with any other capture surface (mobile-shortcut Workflowy nodes, Slack-saved-items mirror, etc.).

| Order | Source Node          | Node ID | Notes |
| ----- | -------------------- | ------- | ----- |
| 1     | Inbox (master)       | <TBD>   | Untriaged tasks, links, ideas. The primary capture surface. |
| 2     | Reading List         | <TBD>   | The reading queue root — items here are URLs you want to read but haven't decided what to do with yet. |
| 3     | Reading WIP          | <TBD>   | Active reading queue: items being read, items recently read but not yet distilled. |

## Domain Nodes (under Tasks)

Domains are user-specific (Office / Personal / Project / etc.). Discover them once via `list_children` against the Tasks node and write the rows here.

| Node Name | Node ID | Last Verified |
| --------- | ------- | ------------- |
|           |         |               |

## Distillations layer (optional — only if you follow the second-brain discipline)

| Node Name                 | Node ID | Last Verified |
| ------------------------- | ------- | ------------- |
| Pillar 1 — distillations  |         |               |
| Pillar 2 — distillations  |         |               |
| Themes (parent)           |         |               |
| Cross-pillar concept maps |         |               |
| Session logs              |         |               |
