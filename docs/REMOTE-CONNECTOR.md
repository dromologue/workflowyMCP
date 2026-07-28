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

## Read parity: a shim is not the only way to drift

The hazards above are transport-level. There is a second, quieter class that
bites any connector deployment which is not literally the same build as the
local one: **the two can disagree on what a read returns.**

The concrete case, and the reason this section exists. Workflowy's
`/nodes?parent_id=` endpoint returns children in an internal/creation order,
**not** the outline's display order. This server sorts every children listing
ascending by `priority` (the empirically-confirmed display order — lowest
renders at the top; see Known Limitations in `CLAUDE.md`). A connector
deployment running a build from before that fix returns the raw order instead.
The result is that `list_children` on the **same parent** comes back in
**opposite orders** depending on which surface asked, and nothing in either
response admits it. An operator who lists through one surface and verifies a
write through the other sees what looks like the write having gone in
backwards, and can burn several round-trips chasing an inversion that was never
there.

Two rules follow:

1. **Pin the connector to the same build as the local server, and guard it.**
   If the connector is a separate checkout rather than the same binary, treat
   read behaviour as a parity surface: a check that fails the push when the two
   disagree is cheap, and the disagreement is otherwise invisible. Count call
   sites rather than grepping for the symbol — one sorted funnel out of two is
   as broken as none when a caller happens to use the other one.
2. **Ascending `priority` is the ground truth for outline order.** Never infer
   order from the order a listing arrives in, on any surface. Priority-less
   nodes land at the head, which is why an ordered insert can read back
   reversed. When order genuinely matters, assert it with `reorder_nodes` —
   the only primitive that guarantees a sequence — rather than trusting
   placement.

The same class of bug had a purely local variant, fixed 2026-07-28: the
per-listing sort was invisible in `get_subtree`'s flat `nodes` array, because a
walk level is assembled from a `buffer_unordered` stream and so arrived in
fetch-completion order, non-deterministically between identical runs of the
identical walk. Each level is now re-keyed to its parents' order before it is
emitted, so `list_children` and `get_subtree` agree by construction and both
reflect the outline.

## Two surfaces onto one tree: routing guidance

Running both a local stdio server and a remote connector against the **same
Workflowy account** is a supported topology — the connector is a nightly
follower of the local canonical index, so either can serve reads. But they
share one upstream account and therefore **one API rate limit**: two live
clients issuing requests double the load against that single budget, and it is
easy to lose track of which surface actually answered a call.

The recommended policy is to route by which surface can reach the local server,
not to spread traffic across both:

- **Where the local stdio server is reachable (a desktop/laptop running Claude
  Desktop or Claude Code), use it.** It is lower-latency, needs no network
  round-trip to a hosted shim, and keeps the connector's quota free.
- **Use the remote connector only for surfaces that cannot reach the local
  server** — claude.ai web and mobile. That is the connector's reason to exist.
- Treat a fall-through from local to connector as an incident signal, not a
  default: if the local server is unresponsive on a host that should use it,
  restart that host rather than silently sending its traffic through the
  connector. `pgrep`-ing the server binary plus a `workflowy_status` /
  `health_check` probe tells you which surface is live.

This is an operational deployment policy, not server behaviour — the binary
cannot know which device called it, so it is enforced by convention and by the
operator's health monitoring, not by a code path in this repo.

### A third surface, if the WorkFlowy desktop app is running

WorkFlowy's own desktop app exposes an MCP server on a stable localhost port
(`mcp__workflowy-desktop__*`). It drives the running client against the
already-synced local tree, so it has no REST rate limit and can traverse the
whole tree instantly — but it has no filesystem, no persistent name index, no
mirror discipline, no privacy exclusions (it exposes the entire tree), and its
ids are 12-char handles rather than full UUIDs. It is a **complement, not a
replacement**: reach for it only for what this server cannot do — attachments,
navigating the user's client, live-watch, and rate-limit-free whole-tree reads
— and return to this server for everything else.

Its absence is routine, not exceptional: the app simply may not be running.
Any skill or scheduled task that reaches for it must fall back to this server
automatically rather than aborting, and report only the specific step that has
no substitute here. Falling the other way — from this server outward to a
remote connector on a machine that can reach local — is not a fallback but an
incident; restart the local host instead.
