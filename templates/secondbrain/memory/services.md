---
name: Additional Services
description: User-configured services that the wflow skill probes and routes to alongside Workflowy. Each entry tells the skill which MCP namespace to call, what the service provides, which workflows use it, and how to health-check it. The skill itself is service-agnostic; everything user-specific lives here.
type: reference
canonical_path: $SECONDBRAIN_DIR/memory/services.md
---

# Additional Services (template)

The wflow skill ships service-agnostic. The `Workflowy` MCP is the one required surface; everything else — ink capture (reMarkable, Supernote, etc.), document storage (Google Drive, Dropbox, Notion), reading services (Readwise, Pocket), task systems (Linear, Jira), calendar — is optional and declared here.

The skill reads this file during Bootstrap (Step 0) to decide which MCPs to probe, and during the relevant workflows to know which tool namespace to call. Adding a new service is a matter of adding an entry below, not editing the skill.

## Schema

Each `## <Service Name>` block holds:

- **mcp_namespace** — the `mcp__<name>__*` prefix the tools use, or `none` if the service is reached via shell/HTTP rather than MCP.
- **purpose** — short description of what the service provides (e.g. "ink capture, PDF/EPUB reading with marginalia").
- **participates_in** — comma-separated list of workflow categories: `capture`, `triage`, `retrieval`, `synthesis`, `extraction`, `prioritisation`. The skill uses this to decide which workflows should consider this service.
- **bootstrap_probe** — the health-check tool to call during Step 0 of Bootstrap. Use the exact tool name. Set to `none` to skip the probe (the service is then assumed available without verification).
- **notes** — optional. Anything else relevant: known fragility, OCR backend selection, rate limits, cache TTLs, etc.

Example shape (commented out — replace with your actual services):

```
## <Service Name>
- mcp_namespace: <namespace>
- purpose: <one-line>
- participates_in: <comma-separated workflow categories>
- bootstrap_probe: <tool name or `none`>
- notes: <optional>
```

## Configured services

(Populate this section with your own service entries — one `## <Service>` block per entry. Leave the section empty if Workflowy is the only surface you use; the skill works fine standalone.)
