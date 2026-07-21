# Running this server behind a remote connector (claude.ai custom connectors)

This server is transport-agnostic: the same Rust binary that speaks stdio to
Claude Desktop / Claude Code can sit behind an HTTP shim and serve claude.ai
web/mobile as a custom connector. The shim is a thin transport layer — it
transforms no parameters and adds no logic — so everything in the tool
reference applies unchanged. (A reference connector implementation exists but
is not part of this public repo; any MCP-over-HTTP gateway works.)

One production-confirmed hazard is specific to the claude.ai
custom-connector surface and is **not fixable server-side**. This page
documents it and the mitigations built into the tool surface.

## The hazard: the host strips bare-string id parameters

Confirmed in production (2026-07-12 field report, issue 1): on the claude.ai
custom-connector surface, a **top-level bare-string `node_id` parameter is
dropped by the host** before the request reaches the connector. The same id
nested inside an operations array survives. No server code can recover a
parameter that never arrived. The observable symptom is a scoped read
silently collapsing to a workspace-root read (which the server then refuses
or misroutes), on roughly every second call.

## Mitigations (all shipped in this server)

1. **Route scoped reads through `read_batch`.** The operations-array shape
   (`read_batch(operations=[{op: "get_subtree", node_id: ...}])`) survives
   the host encoding. Use it for `get_node` / `list_children` /
   `get_subtree` whenever the host has shown stripping behaviour.
2. **Route writes through the operations-array tools.**
   `batch_create_nodes` and `transaction` carry per-op `node_id` fields that
   survive; bare single-node writes are the vulnerable shape.
3. **Required `parent_id` on the write tools.** `create_node`,
   `batch_create_nodes`, `insert_content`, and `create_mirror`'s
   `target_parent_id` reject omission/`null` at the wire with a field-named
   error — a stripped parameter fails loudly instead of landing content at
   the workspace root.
4. **`expect_name` on deletes.** A host that coerces a stripped id into a
   plausible contextual UUID cannot be detected server-side; the name-echo
   guard refuses the delete when the resolved node's name doesn't match.
5. **`scope_resolved` in every scoped response.** Read it after every call
   to verify what the server actually targeted.

## Related transport artefacts

- **Bare `{"error":"Error occurred during tool execution","request_id":…}`
  failures** originate above this server's handlers (rmcp framework or the
  transport wrapping a torn/timed-out connection). Every failure path inside
  this server emits a structured envelope. Recovery: read back to confirm
  what landed before retrying.
- **Large `get_subtree` results spilled to a file** are host rendering
  behaviour, not server truncation — the server caps by node count and
  wall-clock only, and reports both honestly in the truncation envelope.
- **The persistent name index is per-process-host.** A remote connector
  deployment should provision its own `WORKFLOWY_INDEX_PATH` on durable
  storage and schedule `wflow-do reindex --timeout-secs 0 --patient` for
  convergence, exactly as a local install does.
