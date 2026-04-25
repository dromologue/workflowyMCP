# Workflowy MCP — Reliability & Ergonomics Plan

Source brief: triage notes in PR/conversation 2026-04-25. Original brief
references P1–P5 priority tracks. This plan reorganises that brief into
discrete passes that can be picked up across sessions.

**Resume protocol:** before starting a new pass, re-read this file end to
end, scan `git log --oneline` for any commits tagged `[reliability]`, and
confirm the "Current state" line below matches reality. Each pass ends
with: code + tests + commit + tick the box here.

---

## Current state

- Pass 1: complete
- Pass 2: complete (177 tests passing)
- Pass 3: complete (177 tests passing)
- Pass 4: not started
- Pass 5: not started
- Pass 6: not started
- Pass 7: not started

Last touched: 2026-04-25 (Pass 2 + Pass 3 implemented).

---

## Pass 1 — Cancellation that actually cancels (+ two cheap wins)

**Why first.** The "server hangs after a burst of failures" symptom (brief
1.2) and "`cancel_all` not actually cancelling" (1.3) are the same bug:
the cancel guard is only checked at coarse checkpoints, while the rate
limiter's `acquire().await` and the HTTP request itself ignore it. Net
effect: a long walk pins the shared `RateLimiter`, every new tool call
queues behind it, and `cancel_all` does not preempt — it only flags.

**Scope.**

1. Make `RateLimiter::acquire` cancellation-aware. Add an
   `acquire_cancellable(&CancelGuard)` variant (or thread a
   `CancellationToken` through). Wakeups must check the guard between
   `tokio::time::sleep` slices, not only at the top.
2. Wrap each `try_request` HTTP call in `tokio::select!` against a
   cancellation signal so an in-flight `reqwest::send()` is dropped on
   cancel rather than awaited to completion.
3. Thread a `CancelGuard` through `WorkflowyClient::request` /
   `try_request` so the existing `FetchControls.cancel` propagates all
   the way down — not just to the level boundaries in
   `fetch_descendants`.
4. **Brief 3.3 — truncation banner with path.** When
   `fetch_descendants` truncates, capture the parent node whose
   children were not fully drained and surface its hierarchical path.
   Wire through `SubtreeFetch` (new field `truncated_at_path:
   Option<String>`) and `truncation_banner_with_reason`.
5. **Brief 3.4 — `get_node` children semantics.** Decide: populate
   `children` from a depth-1 `get_children` call inside `get_node`, OR
   remove the field. Document the choice in the tool description.
   Default: populate, since it's a single extra HTTP call and matches
   caller intuition.

**Acceptance.**

- New unit test: spawn a fake walk that loops on `acquire`, fire
  `cancel_all`, assert the walk returns within 100 ms.
- New unit test: cancellation observed during in-flight HTTP returns
  `TruncationReason::Cancelled` and frees the rate-limiter immediately.
- Existing 159 unit tests still pass.
- `get_subtree` truncation banner now includes a `truncated_at` path.
- `get_node` either populates `children` or omits it; tool description
  matches.

**Out of scope.** New tools, name-index changes, anything in P2+.

**Commit message.** `Make cancellation preempt rate-limiter and HTTP
calls; surface truncation path in subtree banner; populate get_node
children`

---

## Pass 2 — Observability the assistant can self-diagnose with

**Why next.** Until the assistant can see *why* a call failed or hung,
every reliability fix is invisible. Brief items 1.1 (deserialisation
failure visibility), 1.2 acceptance #3 (`workflowy_status`), and 4.2
(rate-limit header exposure) all live here.

**Scope.**

1. **Brief 1.1 — better deserialisation diagnostics.** Hook
   `rmcp`'s deserialisation error path so failures log: tool name, the
   raw JSON-RPC params blob (truncated to 1 KB), and a hint listing
   required fields. The server cannot fix a client sending `null`, but
   it can make the bug visible in one log line.
2. **Brief 1.2 #3 — `workflowy_status` tool.** Either rename
   `health_check` to `workflowy_status` (breaking) or add it as an
   alias. Extend the response to include `in_flight_walks`,
   `last_request_ms`, `tree_size_estimate` (cached, refreshed lazily).
   Hard 1-second budget remains.
3. **Brief 4.2 — upstream rate-limit headers.** Capture
   `RateLimit-Remaining` / `RateLimit-Reset` (or whatever Workflowy
   sends) on every response, store on `WorkflowyClient`, expose via
   `workflowy_status`.

**Acceptance.**

- Sending a malformed `node_id` produces a log line containing the
  tool name and the offending payload.
- `workflowy_status` returns within 1 s on a 250k-node account and
  reports `in_flight_walks` correctly when a walk is mid-flight.
- Upstream rate-limit data appears in `workflowy_status` after at
  least one real request has run.

**Commit message.** `Add structured deserialisation logging,
workflowy_status tool, and upstream rate-limit visibility`

---

## Pass 3 — Operation log (Brief 4.1)

**Scope.**

1. Append `{ tool, params_hash, started_at, finished_at, status,
   backend_latency_ms, error }` to an in-memory ring buffer (size
   ~1 000) on every tool call. Wrap once in
   `WorkflowyMcpServer::with_logging` so individual handlers don't
   each have to remember.
2. Add `get_recent_tool_calls(limit, since)` MCP tool returning the
   last N entries.
3. Optional: gate persistence to disk behind an env var
   (`WORKFLOWY_OP_LOG_PATH`); JSONL append-only.

**Acceptance.**

- 100 sequential tool calls produce 100 log entries, retrievable via
  `get_recent_tool_calls(limit=100)`.
- `params_hash` is stable for identical inputs (`sha256` of canonical
  JSON).
- Ring buffer evicts oldest on overflow without panicking.

**Commit message.** `Add per-call operation log and get_recent_tool_calls
tool`

---

## Pass 4 — Ergonomic ID and lookup surface (Brief 3.1, 3.2)

**Why later.** This is the largest single piece of work and benefits
from observability already being in place so we can measure index
warm-up cost.

**Scope.**

1. **Brief 3.1 — short-hash IDs.** Workflowy URLs use a 12-char hex
   suffix of the UUID. Add a `node_ref` parser that accepts either
   form and resolves short → full via the name index (extended to
   carry full UUIDs keyed by their last-12 hex). Wire it into every
   handler that takes `node_id`. Behaviour on collision: return 409
   with all matching full UUIDs.
2. **Brief 3.2 — automatic name index.** Today the index is
   opportunistic and TTL'd at 5 min. Promote it to an authoritative
   index that is:
   - Populated lazily on first unscoped query (single full walk,
     bounded by the existing 20 s deadline; partial result allowed).
   - Updated on every write (`create_node`, `edit_node`,
     `move_node`, `delete_node`).
   - Backed by `parking_lot::RwLock<HashMap<NameLower,
     Vec<IndexEntry>>>` plus a parallel `HashMap<ShortHash, FullUuid>`.
   - No TTL; eviction only on explicit invalidate.
3. Make `find_node` use the index by default; remove the
   `use_index` param (deprecated in tool description, accepted but
   ignored).

**Acceptance.**

- `find_node` with no `parent_id` against a populated index returns
  in <100 ms for a known name.
- A `create_node` followed by `find_node` for the new name returns
  the new ID within 50 ms.
- A short-hash `node_id` resolves transparently in any handler.

**Commit message.** `Authoritative name index with short-hash ID
resolution`

---

## Pass 5 — Heavy-workflow primitives (Brief 2.1, 2.3, 2.4)

**Scope.**

1. **Brief 2.4 — `edit_node` partial updates.** Reproduce the
   "name + description together" failure first. Add a regression
   test that hammers 100 sequential `edit_node` calls with both
   fields and asserts both round-trip. If the upstream API
   genuinely loses a field, document it and split into two requests
   server-side (transparent to the caller).
2. **Brief 2.3 — `move_node` stale parent.** Add an internal
   "move with refresh" path: on a 4xx that mentions parent, refresh
   the parent's children listing, retry once, then surface. Add a
   `move_node_strict` variant that does *not* retry, for callers
   that want the failure visible.
3. **Brief 2.1 — `batch_create_nodes(operations)`.** Implement
   client-side pipelining at `SUBTREE_FETCH_CONCURRENCY` parallelism,
   returning IDs in input order. Each create is independent; on any
   failure return per-operation status. Not transactional.
4. **Brief 2.1 — `transaction(operations)`.** Implement
   create/edit/move/delete atomically by ordering operations,
   capturing pre-state for rollback (delete = re-create, edit =
   re-edit), and replaying inverses on first failure. Document
   "best-effort atomicity" — true atomicity needs upstream
   transactions, which Workflowy doesn't expose.

**Acceptance.**

- 100 sequential `edit_node` with both fields all succeed, no field
  loss.
- 20 sibling `move_node` calls with no manual refresh all succeed.
- `batch_create_nodes` of 30 nodes completes in <5 s on a healthy
  account; partial failures reported per-op.
- `transaction` rolls back cleanly on a forced mid-batch failure.

**Commit message.** `Add batch_create_nodes and transaction; harden
edit_node and move_node`

---

## Pass 6 — Mirrors and API expansion (Brief 2.2, 5.x)

**Pre-work.** First investigate Workflowy's mirror API. If the public
REST surface does not expose mirror creation, this entire pass becomes
a documentation note and we close 2.2 as "not feasible without
upstream changes."

**Scope (assuming upstream supports it).**

1. `create_mirror(canonical_node_id, target_parent_id, priority?)`.
2. Surface mirror relationships in `get_node` and `find_backlinks`
   (`mirrors: [{ id, parent_id, parent_path }]`).
3. Brief 5 nice-to-haves, in order of likely use:
   - `path_of(node_id)` — already half-built in `utils/node_paths.rs`.
   - `bulk_tag(node_ids, tag)` — thin wrapper around `bulk_update`.
   - `since(node_id, timestamp)` — single API hit + comparison.
   - `find_by_tag_and_path(tag, path_prefix)` — combine `tag_search`
     with a path-prefix filter.
   - `export_subtree(node_id, format)` — OPML / Markdown / JSON.

**Acceptance.** Each tool has a unit test covering the happy path
plus one error case. `path_of` works on a known deep node and matches
the manually-walked path.

**Commit message.** `Add mirror primitives and export/path/since
helpers`

---

## Pass 7 — Test scaffolding (Brief repository scaffolding section)

**Scope.**

1. `proptest`-driven boundary tests for every `Parameters<T>`:
   generate random valid + invalid shapes; assert errors are clean
   and never panic.
2. Load-test harness simulating a 30-write distillation session
   (mixed reads/writes); record p50/p95/p99 latencies and assert
   them under threshold.
3. CI workflow (`.github/workflows/acceptance.yml`) that runs the
   full unit suite plus the load test against a sandbox account
   gated by `WORKFLOWY_TEST_API_KEY`.

**Acceptance.** CI green on `main`. Load test passes thresholds. Plan
notes the thresholds chosen.

**Commit message.** `Add proptest, load harness, and CI acceptance
workflow`

---

## Out of scope across all passes

- Changing the Workflowy data model (mirrors / tags / PARA structure are
  the user's domain).
- Adding LLM features inside the MCP server (intelligence belongs in
  the assistant; the MCP stays a thin transport).
- Brief 1.1 server-side "fix" for client-sent nulls. The server's
  rejection is correct; Pass 2 makes the bug visible, but the actual
  fix is in whichever MCP client is dropping the field.

---

## Definition of done for the whole plan

The brief's P1 acceptance script — open connection, list root, drill
four levels deep into a known-large subtree, ten sequential creates
with parent re-resolution, ten sequential moves, edit a node with
both name + description, finish with `workflowy_status` — completes
with zero errors in under 30 s, ten consecutive runs.

That script lives in `tests/scripted_session.rs` and is added in Pass
7 alongside the other scaffolding.
