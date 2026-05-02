# Specification

> What the Workflowy MCP Server does and why.

> **Updated 2026-04-26 (T-164)**: Rust v2 implements **38 tools** — 36 prior + audit_mirrors and review.
> Audit/review heuristics live in the shared `audit` lib module so the
> MCP handlers and the `wflow-do` CLI use one implementation.
> Concept mapping, graph analysis, and Dropbox integration have been removed from scope.
> See `tasks.md` for remaining roadmap.

## Implemented Tools (Rust v2 — 38 total)

| Category | Tool | Status |
|----------|------|--------|
| Search & Navigation | search_nodes | Implemented |
| Search & Navigation | find_node | Implemented |
| Search & Navigation | get_node | Implemented (returns parent + depth-1 children) |
| Search & Navigation | list_children | Implemented |
| Search & Navigation | tag_search | Implemented |
| Search & Navigation | get_subtree | Implemented |
| Search & Navigation | find_backlinks | Implemented |
| Content Creation | create_node | Implemented |
| Content Creation | insert_content | Implemented (hierarchical) |
| Content Creation | smart_insert | Implemented |
| Content Creation | convert_markdown | Implemented |
| Content Modification | edit_node | Implemented |
| Content Modification | move_node | Implemented |
| Content Modification | delete_node | Implemented |
| Content Modification | duplicate_node | Implemented |
| Content Modification | create_from_template | Implemented |
| Content Modification | bulk_update | Implemented |
| Content Modification | batch_create_nodes | Implemented (pipelined; per-op status) |
| Content Modification | transaction | Implemented (sequential with best-effort rollback) |
| Todo Management | list_todos | Implemented |
| Due Dates | list_upcoming | Implemented |
| Due Dates | list_overdue | Implemented |
| Due Dates | daily_review | Implemented |
| Project Management | get_project_summary | Implemented |
| Project Management | get_recent_changes | Implemented |
| Diagnostics & Ops | health_check | Implemented |
| Diagnostics & Ops | workflowy_status | Implemented (extended liveness + workload + rate-limit visibility) |
| Diagnostics & Ops | cancel_all | Implemented |
| Diagnostics & Ops | build_name_index | Implemented |
| Diagnostics & Ops | get_recent_tool_calls | Implemented (in-memory ring buffer, default 1024 entries) |
| API Expansion | path_of | Implemented (chain get_node from leaf to root) |
| API Expansion | bulk_tag | Implemented (tag many nodes by ID, parallel) |
| API Expansion | since | Implemented (single get_node + timestamp comparison) |
| API Expansion | find_by_tag_and_path | Implemented (tag ∩ hierarchical path filter) |
| API Expansion | export_subtree | Implemented (OPML / Markdown / JSON) |
| API Expansion | create_mirror | **Stub** — returns explanatory error; Workflowy's REST API does not expose mirror creation |
| Graph Hygiene | audit_mirrors | Implemented (T-164) — walks subtree, reports BROKEN / DRIFTED / ORPHAN / LONELY against the canonical_of:/mirror_of: convention |
| Graph Hygiene | review | Implemented (T-164) — four buckets: revisit-due, multi-pillar (≥3 signal), stale cross-pillar (>days_stale), source-MOC re-cited |

### audit_mirrors and review (T-164)

Two graph-hygiene tools landed 2026-04-26 to give MCP-callers the same
weekly-review and mirror-audit surface that the `wflow-do` CLI exposes.
Heuristics are extracted into `crate::audit` (a pure-data module — no
I/O, no client) so both transports share one implementation.

**audit_mirrors** walks `root_id` (default Distillations) and reports
findings in four classes against the wflow Mirror Discipline convention:

- **BROKEN**: `mirror_of:<uuid>` does not resolve in scope.
- **DRIFTED**: mirror name no longer substring-matches the canonical's name.
- **ORPHAN**: mirror's claimed canonical lacks a `canonical_of:` marker.
- **LONELY**: canonical with `canonical_of:` set but no mirrors point at it.

**review** walks the same default scope and surfaces:

- (a) `#revisit` notes whose `revisit_due:` date is past today.
- (b) Nodes where `max(mirror_of count, distinct pillar tag count) ≥ 3`.
  Max-not-sum guards against double-counting nodes that use both
  `mirror_of:` lines and pillar tags.
- (c) Cross-pillar concept maps with `last_modified` older than
  `days_stale` (default 90).
- (d) Source-MOC-shaped nodes whose description URLs/DOIs appear in any
  session-log file under `~/code/SecondBrain/session-logs/` modified in
  the last 7 days. Bucket (d) is skipped (returns empty) when the
  directory is unreachable.

Both tools return `{scope, scanned, truncated, truncation_reason, ...}`
plus the typed payload (`findings` array or `buckets` object). The
audit/review surfaces are read-only and idempotent — safe to schedule
weekly.

### Not Implemented

True native mirrors require upstream Workflowy API support that does
not exist as of 2026-04. The `create_mirror` tool is a stub that
returns an informative error so callers don't silently fall back to
the `mirror_of: <uuid>` note convention. Tracked in
`tasks/reliability-and-ergonomics.md` (T-157).

---

## Reliability Properties (Pass 1, 2026-04)

The server enforces three guarantees that callers can rely on without
re-implementing them:

1. **Cancellation preempts in-flight work.** A `cancel_all` call signals
   the shared cancel registry; outstanding tree walks observe the flag at
   their next checkpoint **and** inside the rate-limiter wait, the HTTP
   send (via `tokio::select!`), and the inter-attempt backoff sleep. A
   walk that would otherwise have spent minutes draining queued requests
   returns within ~50 ms of the cancel call. Cancelled walks return
   `truncation_reason = "cancelled"` with whatever was collected.
   Single-node read tools (`get_node`, `list_children`) wrap their API
   calls in `with_read_budget`, which races the inner future against the
   server-wide cancel registry **and** a wall-clock budget — so
   `cancel_all` interrupts single-node reads on the same ~50 ms cadence
   that walks already enforced.
2. **Truncation is locatable.** Every truncated response includes a
   `truncated_at_node_id` (and, in text-mode banners, a hierarchical
   `Walk stopped at: …` path) naming the parent whose subtree was not
   fully drained. Callers can re-scope precisely instead of guessing.
3. **`get_node` and `list_children` agree.** `get_node` now returns both
   the requested node and its depth-1 children, matching what
   `list_children` would return for the same ID. The two endpoints can no
   longer disagree on what a node's children are.
4. **Every tool invocation is recorded.** A fixed-capacity in-memory
   ring buffer captures `{ tool, params_hash, started_at, finished_at,
   duration_ms, status, error }` for every handler call. `params_hash`
   is a SHA-256 over the canonical-JSON form of the params (keys
   sorted, no whitespace) so identical calls hash the same regardless
   of producer formatting. `get_recent_tool_calls` exposes the buffer
   without recording itself.
5. **Workload visibility.** `workflowy_status` reports
   `in_flight_walks` (live count, RAII-tracked through `walk_subtree`),
   `last_request_ms` (wall-clock duration of the most recent HTTP
   call), `tree_size_estimate` (last non-truncated unscoped walk),
   plus the most recent upstream rate-limit headers (`RateLimit-*` and
   `X-RateLimit-*` are both captured). Callers checking whether to
   launch a heavy query can see both liveness and load before
   committing.
6. **Short-hash node references.** Every handler that takes a
   `node_id` accepts three forms: full UUID (with or without hyphens),
   the **12-char URL-suffix** form Workflowy puts in URLs
   (`workflowy.com/#/abc123def456`), and the **8-char prefix** form
   used widely in docs and skill files (e.g. `c1ef1ad5` for
   `c1ef1ad5-…`, the first segment of the canonical 8-4-4-4-12
   hyphenated layout). Resolution is `O(1)` against the server's
   name index when the entry is cached. **On a cache miss,
   `resolve_node_ref` walks the workspace synchronously** with the
   extended budget (`defaults::RESOLVE_WALK_TIMEOUT_MS`, 5 minutes)
   and the resolution node cap (`defaults::RESOLVE_WALK_NODE_CAP`,
   100 000); a watcher polls the index every 100 ms and cancels the
   walk as soon as the target appears, so found-early lookups don't
   pay the full timeout. The cache-miss error message distinguishes a
   **truncated** walk (budget exhausted before the workspace was fully
   covered — the hash may exist in an unwalked region; recovery hint:
   re-run with the full UUID, scope a `find_node` call, or rebuild the
   index) from an **exhaustive** walk (the workspace was fully covered
   without finding the target — the hash is genuinely absent: stale,
   shared from another account, or typo'd). Both cases include
   `nodes_walked` and `elapsed_ms` so the caller can choose the right
   recovery without guessing. The 8-char form is collision-aware: if
   two distinct UUIDs share a prefix, resolution returns `None` and
   the caller must disambiguate via the full UUID. The name index has
   no TTL — once populated it serves lookups indefinitely until a
   write invalidates the affected entry.
6a. **Persistent name index.** The name index survives server
   restarts. On startup, `WorkflowyMcpServer::with_cache_and_persistence`
   reads `$WORKFLOWY_INDEX_PATH` (default
   `$HOME/code/secondBrain/memory/name_index.json`) and rehydrates
   every entry it finds; a missing or unreadable file is logged and
   the server starts empty. Mutations set a dirty flag; a background
   task flushes to disk every `defaults::INDEX_SAVE_INTERVAL_SECS`
   (30 s) using a write-then-rename protocol so a crash mid-save
   never produces a half-written file. A second background task
   walks the workspace root every
   `defaults::INDEX_REFRESH_INTERVAL_SECS` (30 minutes) so newly
   added or renamed nodes get indexed without user action. The 30-min
   cadence is calibrated against a 250 k-node workspace and a per-walk
   budget of ~12 k nodes — at this rate quasi-full coverage builds up
   over a working day. Both background tasks are no-ops when no save
   path is configured (test paths, custom embeddings).
7. **`edit_node` field-loss workaround.** When both `name` and
   `description` are supplied to `edit_node`, the client splits the
   update into two sequential POSTs (one per field) instead of a
   combined payload. This works around an observed upstream issue
   where the combined form intermittently lost one field. Costs an
   extra round-trip; produces deterministic results.
8. **`move_node` retry-with-refresh.** On a parent-related 4xx error
   ("parent not found"/"stale parent"), `move_node` re-fetches the
   target parent's children listing and retries the move once. 5xx
   errors continue to use the standard exponential-backoff path.
9. **Propagation-lag tolerance for read paths.** `get_node` and
   `list_children` go through `*_with_propagation_retry` helpers on
   `WorkflowyClient` that retry up to 3 times (200/400/800 ms backoff)
   on a 404. Workflowy has been observed to return a node ID via a
   parent's children listing before that ID is queryable directly —
   the brief calls this "Pattern A". The retry closes the consistency
   window that callers used to have to handle themselves.
10. **Structured tool errors.** Every handler that fails goes through
    `tool_error(operation, node_id, err)` which picks a JSON-RPC error
    code (`RESOURCE_NOT_FOUND` for 404s, `INTERNAL_ERROR` otherwise),
    sets `message` to `"<operation>: <err>"`, and attaches a `data`
    payload with `{operation, node_id, hint, error}`. Clients that
    only display the bare `message` still see the operation; clients
    that surface `data` get a one-sentence remediation hint
    (propagation lag, timeout, backend error, auth failure, cancelled).
11. **Per-tool health visibility.** `workflowy_status.per_tool_health`
    is a histogram over the most recent 200 op-log entries, reporting
    `total / ok / err / ok_rate / status` per tool with status thresholds
    `healthy ≥ 75%`, `degraded ≥ 50%`, `failing < 50%`. Pattern B
    (search succeeds while direct reads fail) is now diagnosable from
    a single status response.

11a. **Per-tool wall-clock budgets.** Every API-touching tool runs
    against an upstream-independent deadline so a hung Workflowy
    backend cannot wedge the MCP tool surface for longer than the
    budget. Walks use `SUBTREE_FETCH_TIMEOUT_MS` (20 s).
    Single-node reads use `READ_NODE_TIMEOUT_MS` (30 s) via the
    server-side `with_read_budget` helper. `edit_node` uses
    `EDIT_NODE_TIMEOUT_MS` (60 s) and shares the same deadline across
    its split name+description POSTs so a flaky upstream cannot
    double the budget. **Single-node writes** (`create_node`,
    `delete_node`) use `WRITE_NODE_TIMEOUT_MS` (15 s) — bounding the
    retry loop end-to-end means a transient upstream slowness cannot
    make one node-creation burn the full
    `RETRY_MAX_ATTEMPTS × HTTP_TIMEOUT_SECS` (~150 s), which was the
    root cause of the 4-minute `insert_content` hangs in the
    2026-05-02 report. **`insert_content`** carries an additional
    end-to-end budget of `INSERT_CONTENT_TIMEOUT_MS` (210 s — well
    inside the MCP client's 4-min hard timeout) and returns a
    structured partial-success payload (`status: "partial"`,
    `reason: "timeout"|"cancelled"`, `created_count`, `total_count`,
    `last_inserted_id`, `stopped_at_line`) when the budget fires, so
    callers can resume from where the call stopped instead of seeing
    "no result received" with no diagnostic. Hitting any budget
    returns `Timeout`, which `tool_error` translates into a
    structured response with the `Timeout` proximate cause; the
    underlying reqwest send is dropped cleanly so the rate-limiter
    slot and connection-pool slot are immediately available for the
    next call.

11b. **Transport-level failures are retryable.** `WorkflowyError::is_retryable`
    classifies both server-side errors (429 + 5xx status codes) **and**
    transport-side errors (connect/read/body timeouts, dropped requests
    surfaced by `reqwest` as `HttpError`). A transient read-timeout
    against a slow upstream now flows through the backoff loop instead
    of returning `RetryExhausted` after a single attempt — but only
    within the per-tool wall-clock budget above, so retries cannot
    extend a hung call past its deadline.

11c. **`authenticated` is decoupled from probe success.** The client
    stamps `last_success_unix_ms` on every 2xx response and
    `last_auth_failure_unix_ms` only on 401/403. Probes derive
    `authenticated` from `recent_auth_failure(AUTH_FAILURE_WINDOW_SECS)`
    (5 minutes) instead of equating it with probe success — so a
    transient timeout or 5xx after a successful write burst no longer
    flips the auth signal. `last_successful_api_call_ms_ago` is
    surfaced alongside so callers can anchor a single degraded probe
    against proof of recent liveness. Both `health_check` and
    `workflowy_status` make two attempts inside the same wall-clock
    budget; auth failures skip the retry since the answer won't change.

11d. **`api_reachable` is decoupled from probe success.** Same pattern
    as `authenticated`: the diagnostic tools used to set
    `api_reachable = probe_succeeded`, which meant two consecutive
    probe blips during a heavy write burst flipped the status to
    degraded even though the burst itself was the proof of liveness.
    `derive_api_reachable` now treats a 2xx within
    `API_REACHABILITY_FRESHNESS_MS` (30 s) as positive evidence: the
    probe is one signal, a recent successful tool call is another,
    and either suffices. The response carries
    `api_reachable_via_recent_success: true` when the probe failed
    but the freshness window saved the verdict, so callers can tell
    the cases apart.

11e. **`null` parameters are reliable, not intermittent.** Tools that
    accept "no scope = workspace root" (`list_children`,
    `find_node.parent_id`, `create_node.parent_id`,
    `tag_search.parent_id`, etc.) declare the field as
    `Option<NodeId>` with `#[serde(default)]`. Both `null` and an
    omitted field deserialise to `None`, which the handler routes
    to the top-level fetch — the schema and the runtime agree.
    Previously `node_id: null` intermittently surfaced "Tool
    execution failed" because some MCP clients send `null` and some
    omit the field, and only one form was accepted.

12. **Best-effort transactions.** `transaction` applies a sequence of
   create/edit/delete/move operations sequentially; on first failure
   it replays inverse operations in reverse order to roll back
   what already succeeded. `delete` is intentionally not invertible
   (a deleted subtree's exact ids/timestamps cannot be recreated), so
   transactions should sequence deletes last. `batch_create_nodes` is
   a separate, non-transactional pipelined creator for cases where
   per-op partial success is acceptable.

---

## Load Testing & Failure-Mode Coverage

Reliability properties 11a–11c make claims about how the server
behaves when the upstream is slow, hung, or returning specific HTTP
codes. Those claims are pinned by a `load_tests` module in `server.rs`
that runs against a real in-process HTTP mock (`wiremock`), rather
than against `http://invalid.local` — which only exercises the
no-network failure mode and cannot simulate the "upstream accepts the
connection and then sits on it" pattern that the 2026-04-30 MCP
failure report described.

### Mock infrastructure

- `WorkflowyClient::new_with_configs(base_url, api_key, retry, rate_limit)`
  takes explicit retry and rate-limit configs so tests can dial down
  retry attempts and dial up the rate-limit burst. Production callers
  use the no-arg `new`, which routes to `new_with_configs` with the
  project-wide defaults.
- `WorkflowyMcpServer::with_read_budget_ms(ms)` is a `#[cfg(test)]`
  builder method that overrides the `READ_NODE_TIMEOUT_MS` budget the
  server applies to single-node reads. Tests target it so failure
  paths complete in milliseconds instead of the production 30 s.
- `wiremock::MockServer` binds to a random localhost port; each test
  scripts request matchers and per-request delays. Custom
  `wiremock::Respond` impls let one mock return different responses
  on different attempts (used for "first call hangs", "first call
  404", and similar scripted recoveries).

### Covered failure modes

Every test below is a sub-second exercise of a property the server
claims to enforce. A failure of any one of them is a regression in
the corresponding reliability property.

| Failure mode | Test | Asserts |
|--------------|------|---------|
| Upstream accepts then never responds (`list_children`) | `list_children_against_hung_upstream_returns_within_budget` | Tool returns a `tool_error` well under the 5 s mock delay; the `with_read_budget` deadline (300 ms in test) is what fired. |
| Same on `get_node` (parallel parent + children fetches) | `get_node_against_hung_upstream_returns_within_budget` | Both branches drop on the budget; the parallel `tokio::join!` cannot stretch the call past it. |
| `cancel_all` mid-flight on a slow read | `cancel_all_preempts_inflight_list_children_within_50ms_slice` | After the call enters its delay, `cancel_all` produces a Cancelled return within ~500 ms — the 50 ms cancel-poll cadence the implementation guarantees. |
| Burst of 20 concurrent reads (the smoke test from the report) | `burst_of_20_list_children_completes_under_load` | All 20 calls succeed; total wall-clock under 3 s. |
| One hung call does not wedge follow-up reads | `one_hung_call_does_not_wedge_other_reads` | The first call hangs for 30 s; 5 follow-up calls all return promptly under 2 s. The hung call itself ultimately surfaces a budget error. |
| Propagation-lag 404 recovery | `list_children_recovers_from_propagation_lag_404` | First request 404s, retry returns 200; tool surfaces the 200 body. |
| Transient 503 retry within budget | `list_children_retries_503_within_read_budget` | Retry config of 2 attempts is honoured; tool surfaces the eventual 200. |
| Auth failure (401) is not retried | `list_children_does_not_retry_on_401` | Single upstream request observed; tool surfaces a fast error rather than burning the read budget. |
| Children listing carries `parent_id` query param | `children_query_param_is_passed_to_upstream` | Routing pin: a future endpoint refactor that drops the `parent_id` query string trips the test. |
| Every handler routes short-hash inputs through `resolve_node_ref` | `handlers_route_unindexed_short_hashes_through_resolver` | Three handlers (Optional `root_id`, Optional `parent_id`, required `node_id`) against an empty-workspace mock — the resolver concludes exhaustive walk and returns the expected "Short-hash … was not found" error. A future bypass that passes a raw short hash to the API layer breaks this. |
| Mutation errors carry structured `tool_error` payload | `mutation_errors_carry_structured_data_payload` | `delete_node` / `edit_node` / `move_node` against a 404 mock each surface a structured error message naming the operation — the brief acceptance criterion that bare "Tool execution failed" is a regression. |
| `list_children` with `null` or missing `node_id` returns workspace top-level | `list_children_null_node_id_returns_workspace_root` | Both `{"node_id": null}` and `{}` deserialise to `None` and route to the top-level fetch; the body labels the scope as `workspace root` so the caller knows what they got. |
| `derive_api_reachable` honours recent success when probe fails | `derive_api_reachable_honours_recent_success_when_probe_fails` | Pure unit test of the freshness-window logic: a 2xx within `API_REACHABILITY_FRESHNESS_MS` keeps `api_reachable: true` even when the latest probe blipped. |
| `insert_content` returns structured partial-success on cancel | `insert_content_returns_partial_on_cancel` | `cancel_all` mid-insert produces `status: "partial"`, `reason: "cancelled"`, `created_count >= 1`, `last_inserted_id` set — the same code path the timeout branch uses, deterministically scriptable from a test. |

### Why this matters

The previous failure-mode coverage was four `invalid.local` tests
that each ran for ~30 s waiting for the read budget to expire — the
same 30 s a user would experience in production. They proved the
budget existed but said nothing about which retry-loop branch fired
or whether `cancel_all` actually preempted anything. The mock-based
suite covers fourteen distinct paths in under 2 s total and is the
primary regression net for any future changes to `with_read_budget`,
the propagation-retry layer, the 503/transport retry policy,
`cancel_all`'s preemption behaviour, the short-hash resolver, the
structured `tool_error` payload, the `null`-as-workspace-root
contract on read tools, the `derive_api_reachable` freshness window,
or the `insert_content` partial-success payload. Full-suite runtime
dropped from 160 s to ~40 s as the slow tests were retired.

---

## Overview

The Workflowy MCP Server is a Model Context Protocol server that enables Claude (and other MCP-compatible AI assistants) to read, search, and write to a user's Workflowy outline. It transforms Workflowy into an AI-accessible knowledge base and capture system.

## User Personas

### Primary: Knowledge Workers

- Use Workflowy as their primary thinking/planning tool
- Want to capture AI-generated insights directly into their outline
- Need to reference their existing notes during AI conversations

### Secondary: Developers

- Building AI workflows that require persistent structured storage
- Integrating Workflowy into automation pipelines
- Extending Claude's capabilities with external memory

## Core Capabilities

### 1. Search & Discovery

**Goal**: Find relevant nodes quickly without knowing exact structure.

| Feature | Description |
|---------|-------------|
| **Fast node lookup** | Find nodes by exact name with duplicate handling |
| Text search | Search node names and notes by keyword |
| **Filtered search** | Filter by tag, assignee, completion status, and date range |
| Path display | Show full breadcrumb path for disambiguation |
| **Backlinks** | Find all nodes linking to a given node |

**Success criteria**: User can locate any node in <2 tool calls.

#### search_nodes Tool (Enhanced)

Full-text search with optional structured filters. When filters are applied, returns enriched results with tags, assignees, and due dates.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | yes | Text to search for in names and notes |
| `tag` | string | no | Filter by tag (e.g. "inbox", "urgent") |
| `assignee` | string | no | Filter by assignee (e.g. "alice") |
| `status` | "all" \| "pending" \| "completed" | no | Filter by completion status |
| `root_id` | string | no | Limit search to a subtree |
| `scope` | string | no | Scope type: this_node, children, siblings, ancestors, all |
| `modified_after` | string | no | ISO date — only nodes modified after this date |
| `modified_before` | string | no | ISO date — only nodes modified before this date |

**Filter pipeline** (applied in order):
1. Scope/subtree narrowing
2. Text search (name + note)
3. Tag filtering (parsed from `#tag` in text)
4. Assignee filtering (parsed from `@person` in text)
5. Status filtering (completed vs pending)
6. Date range filtering

**Conventions** for tags, assignees, and due dates:
- **Tags**: `#inbox`, `#review`, `#urgent` — parsed from node name and note text
- **Assignees**: `@alice`, `@bob` — parsed from node name and note text
- **Due dates**: `due:2026-03-15`, `#due-2026-03-15`, or bare `2026-03-15` — parsed in priority order

---

#### find_backlinks Tool

Find all nodes that contain a Workflowy internal link to a given node.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `node_id` | string | yes | The node to find backlinks for |
| `include_context` | boolean | no | Include surrounding text context (default: true) |

**Response**:
```json
{
  "target": { "id": "abc", "name": "Target Node" },
  "backlink_count": 3,
  "backlinks": [
    {
      "id": "xyz",
      "name": "Linking Node",
      "path": "Work > Notes > Linking Node",
      "context": "...as discussed in [Target Node]..."
    }
  ]
}
```

---

---

#### find_node Tool

Fast node lookup by name that returns the node ID ready for use with other tools. Designed for when Claude needs to quickly identify a specific node.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | The name of the node to find |
| `match_mode` | "exact" \| "contains" \| "starts_with" | no | How to match (default: "exact") |
| `selection` | number | no | If multiple matches, the 1-based index to select |

**Match modes**:
- `exact`: Node name must exactly match (case-insensitive)
- `contains`: Node name contains the search term
- `starts_with`: Node name starts with the search term

**Behavior**:
1. **Single match**: Returns node ID, name, path, and note directly
2. **Multiple matches**: Returns numbered options with paths for disambiguation
3. **With selection**: Returns the specific node from the match list

**Response (single match)**:
```json
{
  "found": true,
  "node_id": "abc123",
  "name": "Project Ideas",
  "path": "Work > Projects > Project Ideas",
  "note": "My project notes...",
  "message": "Single match found. Use node_id with other tools."
}
```

**Response (multiple matches)**:
```json
{
  "found": true,
  "multiple_matches": true,
  "count": 3,
  "message": "Found 3 nodes named 'Ideas'. Which one do you mean?",
  "options": [
    {"option": 1, "name": "Ideas", "path": "Work > Ideas", "id": "abc"},
    {"option": 2, "name": "Ideas", "path": "Personal > Ideas", "id": "def"},
    {"option": 3, "name": "Ideas", "path": "Archive > Ideas", "id": "ghi"}
  ],
  "usage": "Call find_node again with selection: <number> to get the node_id"
}
```

**Use case**: When Claude needs to find a node by name to use its ID with other tools (insert_content, get_children, create_links, etc.)

---

### 2. Navigation & Retrieval

**Goal**: Traverse and read the outline structure.

| Feature | Description |
|---------|-------------|
| Get node | Retrieve single node by ID with metadata |
| List children | Get immediate children of any node |
| Root listing | Access top-level nodes |

**Success criteria**: Any node accessible with known ID or parent reference.

### 3. Content Creation

**Goal**: Add new information to the outline.

| Feature | Description |
|---------|-------------|
| **insert_content** | THE PRIMARY TOOL for all node insertion - single, bulk, todos, any size |
| **convert_markdown_to_workflowy** | REQUIRED for markdown - converts to Workflowy format |
| Smart insert | Search-and-insert workflow with selection |
| Parallel processing | Auto-optimizes for any workload size (1 to 1000+ nodes) |
| Order preservation | Content appears in same order as provided |
| Staging node pattern | Prevents nodes from appearing at unintended locations during insertion |
| **Async job queue** | Background processing for large workloads with progress tracking (planned) |

**Single entry point for all insertions**:

`insert_content` is the ONLY tool needed for creating nodes. It handles:
- **Single nodes**: One line of content
- **Bulk hierarchical content**: Multiple indented lines
- **Todos**: Use `[ ]` for pending, `[x]` for completed
- **Any workload size**: Auto-parallelizes for large content (≥20 nodes)

**Workflow for markdown content**:
1. Convert markdown → `convert_markdown_to_workflowy`
2. Insert result → `insert_content`

**Position behavior**:
- `top` (default): First node placed at top, subsequent nodes follow in order
- `bottom`: Content appended after existing children, order preserved

**Staging node pattern**:

To prevent nodes from briefly appearing at the root or wrong location during multi-node insertions, the insertion tools use a staging node pattern:

1. Create a temporary staging node (`__staging_temp__`) under the target parent
2. Create all hierarchical content inside the staging node
3. Move top-level children from staging to the actual parent (respecting position)
4. Delete the staging node

This ensures nodes are never visible at unintended locations during the operation.

**Success criteria**: Claude-generated content appears in Workflowy with correct structure and order, with 70%+ time savings for workloads over 50 nodes.

---

#### insert_content Tool

**THE PRIMARY TOOL** for all node insertion into Workflowy. Use this for everything: single nodes, bulk content, todos, any hierarchical structure.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `parent_id` | string | yes | Target parent node ID |
| `content` | string | yes | Content in 2-space indented format (see examples below) |
| `position` | "top" \| "bottom" | no | Position relative to siblings (default: top) |

**Content format examples**:

```
# Single node
My new node

# Multiple nodes (siblings)
First node
Second node
Third node

# Hierarchical content
Parent node
  Child 1
  Child 2
    Grandchild
  Child 3

# Todo items
[ ] Pending task
[x] Completed task
[ ] Another pending task
  [ ] Nested subtask

# Mixed content
Project Plan
  [ ] Research phase
    Gather requirements
    Interview stakeholders
  [ ] Design phase
    Create wireframes
    [x] Review with team
```

**For markdown content**: Use `convert_markdown_to_workflowy` first to convert markdown to indented format, then pass the result to `insert_content`.

**Behavior**: Automatically uses parallel insertion for workloads ≥20 nodes, single-agent for smaller content.

---

#### smart_insert Tool

Search for a target node by name and insert content. Combines find + insert in one workflow.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `search_query` | string | yes | Search term to find the target parent |
| `content` | string | yes | Content in 2-space indented format (same as insert_content) |
| `position` | "top" \| "bottom" | no | Position relative to siblings (default: top) |
| `selection` | number | no | If multiple matches, the 1-based index to select |

**Content must be in 2-space indented format**. For markdown, use `convert_markdown_to_workflowy` first.

**Behavior**:
1. Searches for nodes matching `search_query`
2. If single match: inserts content immediately
3. If multiple matches: returns options for user selection
4. User calls again with `selection` to complete insertion

---

#### convert_markdown_to_workflowy Tool

**REQUIRED** for any markdown content. Converts markdown documents to Workflowy's 2-space indented format. This is the ONLY way to format markdown for Workflowy.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `markdown` | string | yes | The markdown content to convert |
| `options` | object | no | Conversion settings (see below) |
| `analyze_only` | boolean | no | If true, return stats only without converting |

**Options**:

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `preserveInlineFormatting` | boolean | true | Keep **bold**, *italic*, `code`, links |
| `convertTables` | boolean | true | Convert tables to hierarchical lists |
| `includeHorizontalRules` | boolean | true | Include --- as separator nodes |
| `maxDepth` | number | 10 | Maximum nesting depth |
| `preserveTaskLists` | boolean | true | Keep [x] and [ ] checkbox markers |

**Supported markdown elements**:
- Headers (H1-H6, ATX `#` and setext `===`/`---` styles)
- Nested lists (ordered and unordered)
- Task lists with checkboxes (`[ ]` and `[x]`)
- Fenced code blocks with language labels
- Tables (converted to hierarchical structure)
- Blockquotes (single and nested)
- Inline formatting (bold, italic, links)

**Response**:
```json
{
  "success": true,
  "content": "Converted content...",
  "node_count": 42,
  "stats": {
    "headers": 5,
    "list_items": 20,
    "code_blocks": 2,
    "tables": 1,
    "blockquotes": 3,
    "task_items": 8,
    "paragraphs": 15
  },
  "warnings": [],
  "usage_hint": "Ready to use with insert_content"
}
```

**Workflow**:
```
1. User provides markdown document
2. Call convert_markdown_to_workflowy with markdown
3. Take the "content" from response
4. Call insert_content with that content
```

**Use case**: Converting README files, documentation, meeting notes, or any markdown content for insertion into Workflowy.

---

### 4. Todo Management

**Goal**: Create and manage task lists within Workflowy.

| Feature | Description |
|---------|-------------|
| Create todos | Use `insert_content` with checkbox syntax `[ ]` or `[x]` |
| List todos | Retrieve all todos with filtering by status, parent, search |
| Complete/Uncomplete | Toggle completion status of any node |
| **List upcoming** | Todos due in the next N days, sorted by urgency |
| **List overdue** | Past-due items sorted by most overdue first |
| **Daily review** | One-call standup summary: overdue, upcoming, recent, pending |

**Creating todos**:

Use `insert_content` with checkbox syntax:
```
[ ] Pending task
[x] Completed task
[ ] Another task
  [ ] Nested subtask
```

**Todo identification**:
- Nodes with `layoutMode: "todo"`
- Nodes using checkbox syntax (`[ ]` or `[x]`)

**Filtering options** (for `list_todos`):
- `status`: "all", "pending", or "completed"
- `parent_id`: Scope to todos under a specific node
- `query`: Text search within todo names/notes

**Due date parsing** (priority order):
1. `due:2026-03-15` — explicit due date tag
2. `#due-2026-03-15` — hashtag-style due date
3. `2026-03-15` — bare date in text

---

#### list_upcoming Tool

List todos due within a time window, sorted by urgency.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `days` | number | no | Days ahead to look (default: 7) |
| `root_id` | string | no | Limit to a subtree |
| `include_no_date` | boolean | no | Include undated pending todos (default: false) |
| `limit` | number | no | Max results (default: 50) |

---

#### list_overdue Tool

List past-due incomplete items, sorted by most overdue first.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `root_id` | string | no | Limit to a subtree |
| `limit` | number | no | Max results (default: 50) |

---

#### daily_review Tool

One-call daily standup summary combining overdue items, upcoming deadlines, recent changes, and top pending todos.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `root_id` | string | no | Limit review to a subtree |
| `overdue_limit` | number | no | Max overdue items to show (default: 10) |
| `upcoming_days` | number | no | Days ahead for upcoming items (default: 7) |
| `recent_days` | number | no | Days back for recent changes (default: 1) |
| `pending_limit` | number | no | Max pending todos to show (default: 20) |

**Response**:
```json
{
  "as_of": "2026-02-28",
  "summary": {
    "total_nodes": 1250,
    "pending_todos": 47,
    "overdue_count": 3,
    "due_today": 2,
    "modified_today": 12
  },
  "overdue": [...],
  "due_soon": [...],
  "recent_changes": [...],
  "top_pending": [...]
}
```

---

**Success criteria**: Full task management workflow without leaving Claude.

### 5. Knowledge Linking

**Goal**: Discover and create connections between related content.

| Feature | Description |
|---------|-------------|
| Find related | Analyze node content, extract keywords, find matching nodes |
| Create links | Generate Workflowy internal links to related nodes |
| Auto-discovery | Automatically find relevant connections based on content |

**Keyword extraction**:
- Filters common stop words
- Prioritizes significant terms (3+ characters)
- Scores matches by title vs note occurrence

**Link placement options**:
- `child`: Creates a "🔗 Related" child node with links (default)
- `note`: Appends links to the node's existing note

**Link format**: `[Node Title](https://workflowy.com/#/nodeId)`

_Concept mapping and graph analysis tools have been removed from scope._

### 6. Content Modification

**Goal**: Update existing nodes.

| Feature | Description |
|---------|-------------|
| Update node | Change name and/or note |
| Move node | Relocate to different parent |
| Complete/Uncomplete | Toggle task completion status |
| Delete node | Permanent removal |
| **Duplicate node** | Deep-copy a node and its subtree to a new location |
| **Create from template** | Copy a template subtree with `{{variable}}` substitution |
| **Bulk update** | Apply an operation to all nodes matching a filter |

---

#### duplicate_node Tool

Deep-copy a node and its entire subtree to a new parent. Preserves hierarchy, names, and notes.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `source_id` | string | yes | Node to duplicate |
| `target_parent_id` | string | yes | Where to place the copy |
| `position` | "top" \| "bottom" | no | Position under target (default: top) |

---

#### create_from_template Tool

Copy a template subtree with variable substitution. Template nodes use `{{variable_name}}` placeholders in names and notes.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `template_id` | string | yes | Root node of the template subtree |
| `target_parent_id` | string | yes | Where to place the instantiated copy |
| `variables` | object | yes | Key-value map of variable substitutions |
| `position` | "top" \| "bottom" | no | Position under target (default: top) |

**Example**:
```json
{
  "template_id": "tmpl-abc",
  "target_parent_id": "projects",
  "variables": {
    "project_name": "Alpha",
    "owner": "Alice",
    "deadline": "2026-04-01"
  }
}
```

Template node `{{project_name}} Plan` becomes `Alpha Plan`. All `{{owner}}` in names and notes become `Alice`.

---

#### bulk_update Tool

Apply an operation to all nodes matching a filter. Supports dry-run mode for previewing matches.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `filter` | object | yes | Filter criteria (query, tag, assignee, status, root_id) |
| `operation` | string | yes | One of: complete, uncomplete, delete, add_tag, remove_tag |
| `tag` | string | conditional | Tag to add/remove (required for add_tag/remove_tag) |
| `dry_run` | boolean | no | Preview matches without modifying (default: false) |
| `limit` | number | no | Max nodes to modify (default: 50, safety limit) |

**Operations**:
- `complete` / `uncomplete`: Toggle completion status
- `delete`: Permanently remove matching nodes
- `add_tag`: Append `#tag` to node names
- `remove_tag`: Remove `#tag` from names and notes

---

### 6b. Project Management

**Goal**: High-level project visibility and tracking.

| Feature | Description |
|---------|-------------|
| **Project summary** | Stats, tag counts, assignees, overdue items for a subtree |
| **Recent changes** | Nodes modified within a time window |

---

#### get_project_summary Tool

Get a comprehensive summary of a subtree: total nodes, tag distribution, assignee distribution, overdue count, and completion stats.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `node_id` | string | yes | Root node of the project |
| `depth` | number | no | Max depth to analyze (default: unlimited) |

**Response**:
```json
{
  "root": { "id": "abc", "name": "Project Alpha" },
  "stats": {
    "total_nodes": 85,
    "total_todos": 32,
    "completed_todos": 18,
    "pending_todos": 14,
    "completion_rate": "56%",
    "overdue_count": 3,
    "has_notes": 25
  },
  "top_tags": [
    { "tag": "inbox", "count": 8 },
    { "tag": "review", "count": 5 }
  ],
  "top_assignees": [
    { "assignee": "alice", "count": 12 },
    { "assignee": "bob", "count": 7 }
  ]
}
```

---

#### get_recent_changes Tool

Find nodes modified within a time window.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `hours` | number | no | How many hours back to look (default: 24) |
| `root_id` | string | no | Limit to a subtree |
| `limit` | number | no | Max results (default: 50) |

---

**Success criteria**: All CRUD operations available and reversible (except delete). Project visibility available in a single tool call.

## User Flows

### Flow 1: Capture AI Output

```
User: "Summarize this article and add it to my Research node"

1. Claude generates summary (hierarchical content)
2. smart_insert searches for "Research"
3. If multiple matches → return numbered options
4. User selects → system automatically:
   - Analyzes workload size
   - Uses parallel insertion if beneficial (≥20 nodes)
   - Falls back to single-agent for small content
5. Content inserted with hierarchy preserved
6. Confirmation with target path and performance stats shown
```

### Flow 2: Reference Existing Notes

```
User: "What did I write about project planning?"

1. search_nodes for "project planning"
2. Results show paths: "Work > Projects > Planning Guide"
3. get_node retrieves full content
4. Claude uses content to inform response
```

### Flow 3: Task Management

```
User: "Add my weekly tasks to the Tasks node"

1. find_node for "Tasks"
2. insert_content with checkbox syntax:
   [ ] Review inbox
   [ ] Process email
   [ ] Update project status
   [x] Already done item
3. Confirmation with created todos

User: "Mark my weekly review tasks as complete"

1. search_nodes for "weekly review"
2. get_children to list tasks
3. complete_node for each task
4. Confirmation of completed items
```

### Flow 4: Large Content Insertion (Automatic Parallelization)

```
User: "Import this research outline into my Project node" (provides 200+ node outline)

1. Claude calls insert_content (the only insertion tool needed)
   → System automatically detects 180 nodes
   → Parallel insertion enabled automatically

2. Behind the scenes, the system:
   - Analyzes workload: 180 nodes, 5 subtrees
   - Assigns 4 workers (automatically determined)
   - Each worker gets independent rate limiter (5 req/sec)
   - Workers process their subtrees concurrently

3. Progress tracked during execution:
   - Worker 1: 45 nodes (completed)
   - Worker 2: 38 nodes (in progress, 80%)
   - Worker 3: 52 nodes (completed)
   - Worker 4: 45 nodes (completed)

4. Results returned to Claude:
   {
     "created_nodes": 180,
     "duration_seconds": 8.7,
     "actual_savings_percent": 76,
     "mode": "parallel_workers"
   }

5. If any subtree fails:
   - Automatic retry (up to 2 attempts)
   - Partial success reported with failed subtree details

Note: Claude uses insert_content for ALL insertions. Parallel optimization
happens automatically for workloads ≥20 nodes.
```

### Flow 6: Markdown Document Import

```
User: "Import this markdown README into my Documentation node"

1. Claude calls convert_markdown_to_workflowy with the markdown content
   → Converts headers, lists, code blocks, tables to indented format
   → Returns converted content and stats

2. Claude calls insert_content with the converted content
   → System auto-optimizes based on node count
   → Content inserted with hierarchy preserved

3. Confirmation with stats:
   - 47 nodes created
   - 5 headers, 20 list items, 2 code blocks converted
   - Duration: 2.3 seconds
```

## Constraints

### API Limitations

- Export endpoint: 1 request per minute (rate limited by Workflowy)
- No real-time sync: Changes require manual refresh
- No search API: Must export and filter locally

### Scope Boundaries

- Single user: No multi-user or sharing features
- API key auth only: No OAuth or session management
- Read/write only: No Workflowy UI features (colors, expand/collapse state)

## Non-Functional Requirements

### Performance

- Typical operation: <2 seconds
- Search with cache: <500ms
- Full export: <5 seconds (depends on outline size)

**Large dataset optimizations**:
- Scope filtering uses indexed lookups (O(n) instead of O(n²))
- Tree traversal capped at 500 nodes per request
- Hierarchical content insertion batches concurrent API calls (up to 10 per batch)
- Parent-child relationships indexed for O(1) traversal

### Reliability

- Retry transient failures: 3 attempts with backoff
- Cache invalidation: On any write operation
- Error recovery: Clear messages, suggested actions

### Security

- Credentials: Environment variables only
- Logging: No user content or secrets
- Transport: Local stdio (no network exposure)

### 7. Batch Operations & High-Load Handling

**Goal**: Handle multiple operations efficiently without overwhelming the Workflowy API.

| Feature | Description |
|---------|-------------|
| Batch operations | Execute multiple create/update/delete/move operations in a single call |
| Request queuing | Controlled concurrency with configurable limits |
| Rate limiting | Proactive token bucket rate limiter to prevent API throttling |
| Selective cache invalidation | Invalidate only affected nodes instead of full cache |

---

#### batch_operations Tool

Execute multiple operations with controlled concurrency and rate limiting.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `operations` | array | yes | Array of operations to execute |
| `parallel` | boolean | no | Execute in parallel (default: true) |

**Operation structure**:
```json
{
  "type": "create" | "update" | "delete" | "move" | "complete" | "uncomplete",
  "params": { /* operation-specific parameters */ }
}
```

**Operation params by type**:
- `create`: `{name, note?, parent_id?, position?}`
- `update`: `{node_id, name?, note?}`
- `delete`: `{node_id}`
- `move`: `{node_id, parent_id, position?}`
- `complete`/`uncomplete`: `{node_id}`

**Response**:
```json
{
  "success": true,
  "message": "All 10 operations completed successfully",
  "total": 10,
  "succeeded": 10,
  "failed": 0,
  "results": [
    {
      "index": 0,
      "operation": { "type": "create", "params": {...} },
      "status": "fulfilled",
      "result": { "id": "abc123", "name": "..." }
    }
  ],
  "queue_stats": {
    "queueLength": 0,
    "activeRequests": 0,
    "totalProcessed": 10,
    "totalFailed": 0
  }
}
```

**Use cases**:
- Bulk node creation (e.g., importing a list of items)
- Mass updates (e.g., completing multiple todos)
- Mixed operations in a single batch

---

#### Configuration

High-load behavior is configured via environment constants:

**Queue Configuration** (`QUEUE_CONFIG`):
- `maxConcurrency`: Max parallel API requests (default: 3)
- `batchDelay`: Wait time before processing batch (default: 50ms)
- `maxBatchSize`: Max operations per batch (default: 20)

**Rate Limiting** (`RATE_LIMIT_CONFIG`):
- `requestsPerSecond`: Max sustained request rate (default: 5)
- `burstSize`: Allowed burst capacity (default: 10)

---

#### Performance Characteristics

| Scenario | Without Batching | With Batching |
|----------|-----------------|---------------|
| Create 10 nodes | ~2000ms (10 × 200ms) | ~400ms (parallel) |
| Create 100 nodes | ~20s | ~4s |
| Mixed 50 operations | Sequential | Parallel with rate limiting |

**Success criteria**: Handle 100+ operations without API rate limit errors.

---

### 8. Multi-Agent Parallel Insertion (Automatic)

**Goal**: Provide fast, efficient content insertion automatically for all hierarchical content.

Parallel insertion is **fully automatic** - Claude simply uses `insert_content` and the system optimizes based on workload size.

| Feature | Description |
|---------|-------------|
| **Fully automatic** | `insert_content` auto-parallelizes based on workload |
| Workload analysis | System determines optimal worker count |
| Subtree splitting | Divides content into independent subtrees |
| Parallel workers | Multiple workers with independent rate limiters |
| Progress tracking | Real-time updates during execution |
| Automatic retry | Failed subtrees retry up to 2 times |
| Smart fallback | Falls back to single-agent for <20 nodes |

**No manual tool selection required**: Claude should simply use `insert_content` for all hierarchical content. The system automatically uses parallel workers when beneficial (≥20 nodes).

---

#### analyze_workload Tool

Analyze hierarchical content to estimate insertion performance. Useful for understanding large workloads before insertion.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | string | yes | Hierarchical content to analyze (2-space indented) |
| `max_workers` | number | no | Maximum workers to consider (1-10, default: 5) |

**Response**:
```json
{
  "success": true,
  "analysis": {
    "total_nodes": 150,
    "subtree_count": 4,
    "recommended_workers": 4,
    "subtrees": [
      {
        "id": "subtree-0",
        "node_count": 42,
        "root_text": "First Section...",
        "estimated_ms": 8400
      }
    ]
  },
  "time_estimates": {
    "single_agent_ms": 30000,
    "single_agent_seconds": 30,
    "parallel_ms": 9400,
    "parallel_seconds": 9.4,
    "savings_percent": 69,
    "savings_seconds": 20.6
  },
  "recommendation": "Use insert_content - it auto-optimizes for any workload size"
}
```

**Use case**: Before inserting large content, analyze to understand time estimates. Note: You don't need to analyze before inserting - `insert_content` handles optimization automatically.

---

#### How insert_content Handles Large Workloads

When `insert_content` receives hierarchical content, it automatically:

1. **Content splitting**: Parses content into independent subtrees based on top-level nodes
2. **Worker assignment**: Each subtree assigned to a worker with its own rate limiter
3. **Parallel execution**: Workers process subtrees concurrently
4. **Retry handling**: Failed subtrees automatically retry (up to 2 attempts)
5. **Result merging**: All results combined with detailed stats

**Response includes performance stats**:
```json
{
  "success": true,
  "message": "Successfully inserted 150 nodes",
  "total_nodes": 150,
  "created_nodes": 150,
  "mode": "parallel_workers",
  "duration_seconds": 8.2,
  "performance": {
    "estimated_single_agent_ms": 30000,
    "actual_parallel_ms": 8234,
    "actual_savings_percent": 73
  }
}
```

---

#### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Orchestrator                             │
│  ┌───────────┐  ┌───────────┐  ┌───────────┐  ┌───────────┐ │
│  │ Worker 1  │  │ Worker 2  │  │ Worker 3  │  │ Worker N  │ │
│  │ RateLimiter│  │ RateLimiter│  │ RateLimiter│  │ RateLimiter│ │
│  │ Subtree A │  │ Subtree B │  │ Subtree C │  │ Subtree D │ │
│  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘ │
└────────┼──────────────┼──────────────┼──────────────┼───────┘
         └──────────────┴──────────────┴──────────────┘
                              ↓
                   Workflowy API (5 req/sec each)
```

Each worker has its own rate limiter, allowing true parallelism without competing for the same token bucket.

---

#### Subtree Splitting Algorithm

Content is split at top-level node boundaries:

```
Input:
  Section A          ← Subtree 1 root
    Child A1
    Child A2
  Section B          ← Subtree 2 root
    Child B1
      Grandchild B1a
  Section C          ← Subtree 3 root
    Child C1

Output: 3 independent subtrees
```

**Balancing rules**:
- Target nodes per subtree: 50 (configurable)
- Minimum nodes for separate subtree: 5
- Small adjacent groups merged to reduce overhead
- Maximum subtrees capped at `max_workers`

---

#### Performance Benchmarks

| Nodes | Single Agent | 5 Workers | Savings |
|-------|--------------|-----------|---------|
| 50 | ~10 sec | ~3 sec | 70% |
| 100 | ~20 sec | ~5 sec | 75% |
| 200 | ~40 sec | ~9 sec | 78% |
| 500 | ~100 sec | ~22 sec | 78% |

**Automatic optimization**:

The system automatically selects the optimal insertion strategy based on workload size:

| Node Count | Automatic Behavior | Performance |
|------------|-------------------|-------------|
| < 20 | Single-agent | Fast for small content |
| 20-50 | Parallel (2-3 workers) | ~50-60% time savings |
| 50-100 | Parallel (3-4 workers) | ~70% time savings |
| 100-200 | Parallel (4-5 workers) | ~75% time savings |
| 200+ | Parallel (5 workers) | ~78%+ time savings |

**No manual tool selection required**: Claude should simply use `insert_content` for all hierarchical content. Parallel optimization happens automatically.

**Success criteria**: Insert 200+ nodes with >70% time savings compared to single-agent approach.

---

### 9. Async Job Queue (Background Processing)

**Goal**: Handle large workloads without hitting API rate limits or timeouts. Claude can hand off large operations to the server for background processing.

| Feature | Description |
|---------|-------------|
| **Job submission** | Submit large workloads for background processing |
| **Progress tracking** | Check job status and progress percentage |
| **Result retrieval** | Get results when job completes |
| **Job cancellation** | Cancel pending or in-progress jobs |
| **Rate limit handling** | Server manages API pacing automatically |

**Why use the job queue**:
- Avoid API rate limit errors on large operations
- Prevent Claude timeouts on long-running tasks
- Enable true background processing
- Track progress of long operations

---

#### submit_job Tool

Submit a large workload for background processing. Returns a job ID to track progress.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `type` | "insert_content" \| "batch_operations" | yes | Type of job |
| `params` | object | yes | Job parameters (varies by type) |
| `description` | string | no | Human-readable description |

**Job params by type**:
- `insert_content`: `{parentId, content, position?}`
- `batch_operations`: `{operations: [{type, params}...]}`

**Response**:
```json
{
  "success": true,
  "job_id": "job-1234567890-1",
  "type": "insert_content",
  "status": "pending",
  "description": "Insert 150 nodes under 'Research'",
  "estimated_nodes": 150,
  "message": "Job submitted for background processing. Use get_job_status to check progress."
}
```

---

#### get_job_status Tool

Check the progress of a submitted job.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `job_id` | string | yes | The job ID from submit_job |

**Response**:
```json
{
  "success": true,
  "job_id": "job-1234567890-1",
  "status": "processing",
  "progress": {
    "total": 150,
    "completed": 89,
    "failed": 0,
    "percentComplete": 59,
    "currentOperation": "Inserting content"
  },
  "created_at": "2024-01-15T10:30:00.000Z",
  "started_at": "2024-01-15T10:30:01.000Z"
}
```

**Job statuses**:
- `pending`: Waiting to start
- `processing`: Currently executing
- `completed`: Finished successfully
- `failed`: Finished with errors
- `cancelled`: Cancelled by user

---

#### get_job_result Tool

Get the result of a completed job.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `job_id` | string | yes | The job ID from submit_job |

**Response** (completed job):
```json
{
  "success": true,
  "job_id": "job-1234567890-1",
  "status": "completed",
  "result": {
    "success": true,
    "nodesCreated": 150,
    "nodeIds": ["abc123", "def456", ...]
  }
}
```

---

#### list_jobs Tool

List all jobs with optional status filtering.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `status` | array | no | Filter by status (default: all) |

**Response**:
```json
{
  "success": true,
  "jobs": [
    {
      "job_id": "job-1234567890-1",
      "type": "insert_content",
      "status": "completed",
      "progress": { "total": 150, "completed": 150, "percentComplete": 100 },
      "description": "Insert 150 nodes",
      "created_at": "2024-01-15T10:30:00.000Z"
    }
  ],
  "queue_stats": {
    "pending": 0,
    "processing": 1,
    "completed": 5,
    "failed": 0,
    "cancelled": 0,
    "total": 6
  }
}
```

---

#### cancel_job Tool

Cancel a pending or in-progress job.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `job_id` | string | yes | The job ID to cancel |

**Response**:
```json
{
  "success": true,
  "message": "Job job-1234567890-1 cancelled"
}
```

---

#### Job Queue Workflow

```
User: "Insert this large research document (500+ nodes)"

1. Claude calls submit_job with type: "insert_content"
   → Returns: {job_id: "job-123", status: "pending"}

2. Claude can check progress:
   get_job_status(job_id: "job-123")
   → Returns: {status: "processing", progress: {completed: 245, total: 512, percentComplete: 48}}

3. When done, get results:
   get_job_result(job_id: "job-123")
   → Returns: {status: "completed", result: {nodesCreated: 512, nodeIds: [...]}}

The server handles all rate limiting internally (5 req/sec with burst of 10).
Jobs are retained for 30 minutes after completion.
```

**Success criteria**: Insert 500+ nodes without API rate limit errors or timeouts.

---

## Future Considerations

*Not committed, but designed to accommodate:*

- Conflict detection for concurrent edits
- Offline queue for unreachable API
- Recurring task support (repeat rules for todos)
- Cross-outline collaboration (multi-user shared nodes)
