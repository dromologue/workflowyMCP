---
name: Distillation Taxonomy
description: Pillar / theme / routing data for the wflow skill — the semantic layer of the second brain. Externalised so the skill itself stays generic; additions of pillars, themes, or routing rules live here.
type: reference
last_reviewed: <YYYY-MM-DD when you last verified this content>
canonical_path: $SECONDBRAIN_DIR/memory/distillation_taxonomy.md
---

# Distillation Taxonomy (template)

This is the data file that pairs with `workflowy_node_links.md`. The wflow skill (`~/.claude/skills/wflow/SKILL.md`) defines *behaviour* — workflows, conventions, error handling. This file defines *data* — which pillars and themes exist, which UUIDs they live at, where to route inbound material. When you add a pillar / theme / routing rule, edit this file alone; the skill picks up the change at next bootstrap.

**Companion file:** `workflowy_node_links.md` covers structural nodes (Tasks, Inbox, Reading List, Distillations roots). This file covers the semantic layer — pillars, themes, routing.

---

## Pillars (canonical)

A *pillar* is a top-level conceptual bucket that distilled notes mirror into. Each pillar typically has both a **Link node** (where raw, undistilled material is filed) under Resources / Links, and a **Distillations node** (where atomic notes synthesised from the raw material live) under Distillations. The Distillations node functions as the pillar's Map of Content.

Replace the placeholder rows with your own pillars. Three is a sensible minimum; five tends to be the upper bound before pillars start overlapping.

| Pillar | Focus | Link node UUID | Distillations node UUID |
|--------|-------|----------------|-------------------------|
| <Pillar 1> | <one-line description of what this pillar is about> | `<UUID>` | `<UUID>` |
| <Pillar 2> | <…> | `<UUID>` | `<UUID>` |
| <Pillar 3> | <…> | `<UUID>` | `<UUID>` |

**Key thinkers per pillar (optional):** if your distillations cluster around specific authors, list them here. Helps the skill recognise sources quickly during triage.

- **<Pillar 1>** — <author>, <author>, <author>
- **<Pillar 2>** — <author>, <author>
- **<Pillar 3>** — <author>

**Cross-pillar concepts (optional):** when a single concept connects multiple pillars (e.g. one author's framework cuts across two of your pillars), list it here so distillations on that concept get mirrored into both pillars instead of being filed under one.

- **<Concept name>** — connects <Pillar A> + <Pillar B>. Synthesis touching this concept mirrors across both and gets a node under `Cross-pillar concept maps`.

---

## Themes (cross-cutting)

A *theme* is a structural property of a claim — what the claim is *about*, not what it tells you to do. Themes typically mirror into one or more pillars. Examples might be: AI, Ethics, Productivity, Climate. Use whatever cuts your domain.

Theme structure has split state by design — not every theme has both a Link folder and a Distillations folder. Where a folder is missing, mirroring routes to the available side only until you create the gap-filler.

| Theme | Link UUID | Distillations UUID | Default pillar mirror | Notes |
|-------|-----------|--------------------|-----------------------|-------|
| <Theme 1> | `<UUID>` | `<UUID>` | <Pillar X> | <e.g. "cross-pillar; routes to Build + Lead"> |
| <Theme 2> | `<UUID>` | `<UUID>` | <Pillar Y> | |

---

## Inbound routing table

When triage (Workflow 6) or reading-list management (Workflow 7) needs to decide *where* an inbound link belongs, this table is the lookup. Keys are topic markers (URLs, keywords, source domains); values are the destination Link folder + the pillar a subsequent distillation should target.

| Topic marker | Destination Link folder | Default pillar for distillation |
|--------------|-------------------------|---------------------------------|
| <e.g. "anthropic.com", "openai.com"> | <e.g. AI Link folder> | <e.g. Build> |
| <…> | <…> | <…> |

---

## Tag conventions (optional)

If you use specific tags as workflow markers, document them here. Common patterns:

- `#done` — applied to Reading List entries that have been distilled. Distinct from native task completion (which uses Workflowy's `completed` boolean).
- `#session_<YYYY-MM-DD>` — applied to atoms created in a single distillation session, for traceability.
- `#mirror_of:<short-hash>` — applied to a mirror node pointing back at the canonical it was mirrored from.

Replace, extend, or remove as your conventions evolve.
