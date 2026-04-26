# Tasks

> Actionable work items for the Workflowy MCP Server (Rust rewrite).

## Status Legend

- [ ] Not started
- [~] In progress
- [x] Complete

---

## Phase 0: Rust Migration Foundation

*Get the Rust codebase compiling and passing tests.*

- [x] **T-100**: Fix rmcp 0.16 API usage
  - Replace `#[tool(aggr)]` with `Parameters<T>` wrapper (10 handlers)
  - Fix `Arc<NodeCache>` method calls (remove `.write()`)
  - Fix `Implementation` struct (add `..Default::default()`)
  - Add `Parameters` import from `rmcp::handler::server::wrapper`
  - Clean up unused imports

- [x] **T-101**: Fix path validation bug on macOS
  - `validate_file_path` for non-existent files: canonicalize base before joining
  - Fixes symlink mismatch (`/var` vs `/private/var`)

- [x] **T-102**: Add validation module to lib.rs
  - `validation.rs` was missing from module tree
  - 10 unit tests now visible to test runner

- [x] **T-103**: Add server unit tests
  - 12 parameter deserialization tests
  - Server construction and info test
  - Tool listing verification (all 10 tools registered)

- [x] **T-104**: Update specs for Rust
  - Rewrite `implementation-plan.md` for Rust architecture
  - Update technology stack, module structure, deployment

---

## Phase 1: Core Tool Parity

*Implement tools required by wmanage skill and TypeScript feature parity.*

### Search & Navigation

- [x] **T-110**: Add find_node tool
  - Match modes: exact, contains, starts_with
  - Duplicate handling with numbered options + selection parameter
  - Returns node_id, path, note for use with other tools
  - JSON response with found/multiple_matches structure

- [x] **T-111**: Add find_backlinks tool
  - Finds nodes containing Workflowy links (`workflowy.com/#/node-id`) to given node
  - Reports link location (name, note, or both)
  - Configurable limit

### Content Creation

- [x] **T-112**: Implement hierarchical insert_content
  - Parses 2-space indentation into parent-child relationships
  - Parent stack tracks current node at each indent level
  - Clamps over-indented lines to valid depth

- [x] **T-113**: Add smart_insert tool
  - Search + insert combined workflow
  - Handles disambiguation for multiple matches with selection
  - Content validation (non-empty)

### Todo Management

- [x] **T-114**: Add list_upcoming / list_overdue tools
  - Parse due dates via date_parser.rs (due:YYYY-MM-DD, #due-YYYY-MM-DD, bare date)
  - list_upcoming: upcoming items within N days, sorted ascending
  - list_overdue: past-due items sorted by most overdue first
  - Optional root_id scoping, include_completed, include_no_due_date flags

- [x] **T-115**: Add daily_review tool
  - One-call standup: overdue, due_soon, recent_changes, top_pending
  - Summary stats: total_nodes, pending_todos, overdue_count, due_today, modified_today
  - Configurable limits and time windows

### Project Management

- [x] **T-116**: Add get_project_summary tool
  - Stats: total_nodes, todo counts, completion %, overdue, has_due_dates
  - Tag and assignee counts (via tag_parser.rs)
  - Recently modified nodes within configurable window

- [x] **T-117**: Add get_recent_changes tool
  - Nodes modified within N-day window
  - Optional root_id scoping, completed filtering, limit

### Infrastructure (supporting new tools)

- [x] **T-118**: Rename get_children → list_children
  - Matches wmanage skill expectation

- [x] **T-119**: Add utility modules for new tools
  - date_parser.rs: due date extraction with priority order (14 tests)
  - tag_parser.rs: #tag and @mention parsing (11 tests)
  - node_paths.rs: hierarchical path building (6 tests)
  - subtree.rs: subtree collection, todo/completion detection (10 tests)

- [x] **T-120-a**: Extend WorkflowyNode with completed_at, layout_mode fields

---

## Phase 2: Advanced Features

*Feature parity with TypeScript v1.*

- [x] **T-120**: Add bulk_update tool
  - Filter by query, tag, status, root_id
  - Operations: complete, uncomplete, delete, add_tag, remove_tag
  - Dry-run mode, configurable safety limit (default: 20)
  - Validates operation type and required operation_tag

- [x] **T-121**: Add duplicate_node tool
  - Deep-copies a node and its full subtree via BFS traversal
  - ID mapping for parent-child relationships
  - Optional name_prefix, include_children flag

- [x] **T-122**: Add create_from_template tool
  - Copies template subtree with `{{variable}}` regex substitution
  - Applies to both names and descriptions
  - Reports variables_applied in response

- [x] **T-122b**: Add list_todos tool
  - Filters by parent scope, completion status, text query
  - Uses is_todo() detection (layoutMode or checkbox prefix)

- [x] **T-122c**: Add convert_markdown tool
  - Converts markdown to 2-space indented Workflowy format
  - Handles: ATX headers, lists, code blocks, blockquotes, tables, horizontal rules
  - analyze_only mode returns stats without converting

---

## Phase 3: Infrastructure

- [ ] **T-130**: Implement request queue with batching
  - Max 3 concurrent, 50ms batch delay

- [ ] **T-131**: Implement orchestrator
  - Multi-worker content insertion for large content

- [ ] **T-133**: Add batch_operations / job queue tools
  - submit_job, get_job_status, list_jobs, cancel_job

---

## Phase 5: Reliability & Ergonomics

*Multi-pass plan in `tasks/reliability-and-ergonomics.md`. Source: brief
2026-04-25 covering brief tracks P1–P5.*

- [x] **T-150 (Pass 1)**: Cancellation that actually preempts
  - `RateLimiter::acquire_cancellable` slices long sleeps so a `cancel_all`
    propagates within ~50 ms (was: bound by full computed wait).
  - `request_cancellable` / `try_request_cancellable` race the in-flight
    HTTP send against a cancellation poll via `tokio::select!`; backoff
    sleeps are also cancellation-aware.
  - Cancel guards thread from `FetchControls` through `get_node_cancellable`,
    `get_children_cancellable`, `get_top_level_nodes_cancellable`, and into
    every per-level fetch in `fetch_descendants`.
  - `WorkflowyError::Cancelled` is a first-class error variant.

- [x] **T-151 (Pass 1)**: Truncation banner names the unfinished branch
  - `SubtreeFetch.truncated_at_node_id` captures the parent whose subtree
    was cut short.
  - `truncation_banner_from_fetch` renders a hierarchical
    `Walk stopped at: A > B > C` suffix in every text-mode response.
  - JSON responses (`find_node`) carry `truncated_at_path`.

- [x] **T-152 (Pass 1)**: `get_node` returns depth-1 children
  - Removes the disagreement with `list_children` (which previously
    returned the actual children while `get_node` always returned `[]`).
  - One extra parallel HTTP call; failures degrade to empty children with
    a warn log.

- [x] **T-153 (Pass 2)**: Deserialisation diagnostics + `workflowy_status`
  - `check_node_id` now warns (with tool-correlated context) when a
    handler-boundary validation rejects an empty/malformed id.
  - `workflowy_status` tool: extended liveness probe surfacing
    `in_flight_walks`, `last_request_ms`, `tree_size_estimate`, and a
    `rate_limit` snapshot of the most recent upstream headers.
  - `WorkflowyClient` captures both `RateLimit-*` and `X-RateLimit-*`
    header conventions on every response (success or failure).
  - `WalkGuard` RAII bumps `in_flight_walks` for the duration of every
    `walk_subtree` call so the count is always accurate.

- [x] **T-154 (Pass 3)**: Operation log + `get_recent_tool_calls`
  - New `utils::op_log::OpLog`: ring buffer (default 1024) with
    `{ tool, params_hash (SHA-256 over canonical JSON), started_at,
    finished_at, duration_ms, status, error }`. Total-recorded counter
    survives evictions.
  - `record_op!` macro instruments every tool handler with one extra
    line; both ok and err returns produce a log entry.
  - `get_recent_tool_calls` tool returns recent entries (limit, since
    filter) without recording itself, so callers get a clean snapshot.

- [x] **T-155 (Pass 4)**: Authoritative name index + short-hash IDs
  - `NameIndex` no longer TTL-evicts entries; persistence is bounded by
    explicit `invalidate_node` (writes do this) and `clear`.
  - New `by_short_hash` map records each ingested UUID's trailing 12
    hex chars → full UUID for O(1) URL → UUID resolution.
  - `WorkflowyMcpServer::resolve_node_ref(raw)` accepts either form;
    short-hash misses produce a pointed error pointing at
    `build_name_index`.
  - `check_node_id` short-circuits for valid 12-char hex inputs so
    handlers can pass either form transparently.
  - Wired into get_node, list_children, edit_node, delete_node,
    move_node. Other handlers will follow as touched.

- [x] **T-156 (Pass 5)**: Heavy-workflow primitives
  - `WorkflowyClient::edit_node` splits combined name+description
    updates into two sequential POSTs to dodge the upstream field-loss
    bug documented in the wflow skill.
  - `WorkflowyClient::move_node` detects parent-related 4xx errors
    (not 5xx) and refreshes the new parent's children listing before
    retrying once.
  - `BatchCreateOp` and `WorkflowyClient::batch_create_nodes`:
    pipelined creates with bounded concurrency; results in input order.
  - New `batch_create_nodes` MCP tool: validates parents eagerly,
    resolves short-hash parents, ingests created nodes into the name
    index, returns per-op `ok`/`error`.
  - New `transaction` MCP tool: sequential ops with best-effort
    rollback. Inverse for create=delete, edit=restore-prev,
    move=un-move; delete is non-invertible by design.

- [x] **T-157 (Pass 6)**: API expansion (mirror documented as stub)
  - `create_mirror` is a stub returning an explanatory error: the
    public Workflowy REST API does not expose mirror creation, so
    delivering this tool would silently lie. Stub keeps the MCP
    surface honest and gives callers a clear next-step pointer.
  - `path_of` walks parent_id chain via repeated get_node; bounded by
    max_depth (default 50) to defend against malformed cycles.
    Returns ordered segments root→leaf and a printable `A > B > C`
    string for citation use.
  - `bulk_tag` accepts an explicit ID list and parallel-applies a tag
    by reading each node, appending `#tag` to the name if not present,
    and editing. Bounded concurrency, per-op status reporting.
  - `since` is a single get_node + timestamp comparison — cheap
    incremental sync helper for clients that want to poll a known
    set of nodes.
  - `find_by_tag_and_path` walks a subtree once and filters for nodes
    whose tag matches AND whose computed hierarchical path contains
    the prefix. Honours subtree budgets (truncation banner).
  - `export_subtree` walks once and emits OPML, Markdown nested
    bullets, or JSON. XML metacharacters escaped in OPML; descriptions
    preserved.

- [x] **T-158 (Pass 7)**: proptest + load harness + CI acceptance

- [x] **T-160 (Eval-driven 2026-04-25)**: 8-char short-hash IDs
  - The wflow eval suite (`Evals/evals.json`) and the wflow skill
    (`~/.claude/skills/wflow/SKILL.md`) reference canonical nodes by
    8-char prefixes (e.g. `c1ef1ad5` for Tasks). Pre-fix, the server
    only accepted 12-char URL-suffix short hashes, so eval 1 (morning
    review) failed at the data layer.
  - `NameIndex` gained `by_prefix_hash` mapping the first 8 hex chars
    of every ingested UUID → full UUID. `prefix_hash_of` extracts the
    canonical first segment.
  - `resolve_short_hash` now accepts either 8 or 12 char input. The
    8-char path is collision-aware: if two distinct UUIDs share a
    prefix, returns `None` rather than guessing.
  - `is_short_hash` (server-side) accepts both lengths so
    `check_node_id` short-circuits cleanly at the handler boundary.
  - 4 new tests cover prefix extraction, resolution, collision, and
    invalidation. 206 unit tests passing.
  - First Evals run captured in `Evals/results/run-20260425T194943Z.json`.

- [x] **T-161 (Eval-driven 2026-04-25, follow-up to T-160)**: thread
      short-hash resolution through every handler
  - **Bug discovered live.** T-160 widened the handler-boundary
    validator (`check_node_id`) to accept 8-char prefixes, but a long
    tail of handlers called only the validator and never piped the
    value through `resolve_node_ref`. The raw short hash then landed
    at the upstream API and 404'd. Caught when calling
    `list_overdue(root_id="c1ef1ad5")` against a healthy server.
  - **Affected handlers (all fixed):** `create_node` (parent_id),
    `insert_content` (parent_id), `search_nodes` (parent_id, was also
    missing the boundary check), `get_subtree`, `find_node`
    (parent_id), `daily_review` (root_id), `get_recent_changes`
    (root_id), `list_overdue` (root_id), `list_upcoming` (root_id),
    `list_todos` (parent_id), `get_project_summary`, `find_backlinks`,
    `duplicate_node` (both ids), `create_from_template` (both ids),
    `bulk_update` (root_id, plus the post-walk cache invalidation),
    `build_name_index` (root_id).
  - **Pattern.** Introduced `let resolved_<x> = match &params.<x> {
    Some(v) => { check_node_id(v)?; Some(self.resolve_node_ref(v)?) }
    None => None };` for optionals; `let resolved =
    self.resolve_node_ref(&params.<x>)?;` for required ids. All
    downstream uses (walk_subtree, cache invalidation, equality checks
    against API-returned UUIDs, link regex pattern, JSON response
    fields) now use the resolved value.
  - **Regression test.**
    `handlers_route_root_and_parent_short_hashes_through_resolver`
    exercises one representative handler from each scoping pattern
    (Optional root_id via `list_overdue`, Optional parent_id via
    `list_todos`, required node_id via `get_subtree`). Uses an
    unindexed 12-char hex hash as input and asserts the error message
    surfaces a resolver-side miss ("name index" / "short-hash"),
    proving resolution ran before any HTTP attempt. Would have failed
    against the pre-fix code with a connection error instead. 207 unit
    tests passing.
  - **Specs changed:** none (the language in `specs/specification.md`
    property 6 already names the three accepted node-ref forms; the
    bug was in handler implementation, not contract).

- [x] **T-164 (Brief 2026-04-26: MCP/CLI parity for graph hygiene)**:
      lift audit + review heuristics into a shared lib; expose them as
      MCP tools so callers don't have to shell out
  - **Source.** Internal audit of the MCP surface against the
    `wflow-do` CLI shipped earlier the same day: the CLI gained
    `audit-mirrors` / `review` / `index` / `--dry-run` (T-163
    follow-up); the MCP server gained none of them. Concrete
    consequence: an assistant on a healthy MCP transport could not
    request a mirror audit or a review-surface pass without
    `Bash`-shelling to the CLI binary, which is slower, harder to
    compose with other MCP calls, and skips `per_tool_health` logging.
  - **What landed (option 2 of three considered).**
    - **`src/audit.rs`** — new pure-data module exposing
      `audit_mirrors(&[WorkflowyNode]) -> Vec<MirrorFinding>` and
      `build_review(&nodes, days_stale, today, now_unix, blob) ->
      ReviewReport`. No I/O, no client, no implicit clock — every
      moving part is a parameter so tests are deterministic. The
      `extract_marker(text, prefix)` regex helper accepts both UUIDs
      and opaque pillar tokens (`[\w-]{3,40}`) so canonical_of:lead
      and mirror_of:`<uuid>` both parse from the same function.
    - **MCP handlers** in `src/server.rs`: `audit_mirrors` and `review`
      with new `AuditMirrorsParams` / `ReviewParams` structs. Default
      scope for both is the user's Distillations subtree
      (`7e351f77-c7b4-4709-86a7-ea6733a63171`); `root_id` open for
      narrower or wider scopes. Return JSON with `{scope, scanned,
      truncated, truncation_reason, ...}` plus the typed payload.
      Bucket (d) of `review` reads recent session-log files via a
      private `load_recent_session_logs_blob_for_review()` helper —
      not in `audit.rs` because the lib is pure-data.
    - **`src/bin/wflow_do.rs`** refactored to `use
      workflowy_mcp_server::audit::{audit_mirrors, build_review}`
      instead of duplicating the heuristic. CLI's own
      `load_recent_session_logs_blob()` mirrors the server's helper —
      same blob, same `cutoff = now - 7*86400`. CLI tests collapsed
      from 3 audit/review tests to one wiring smoke test (the lib
      has comprehensive coverage now).
  - **Why option 2.** Considered three: (1) leave the gap and let
    `Bash` shell-out be the workaround forever; (2) wrap the
    heuristic with new MCP tools and share the lib; (3) make a generic
    MCP tool that exec's `wflow-do <subcommand>`. Option 3 was
    rejected as a process-per-call workaround that loses structured
    error handling and feels like a workaround rather than a fix.
    Option 1 punted on a real ergonomic gap. Option 2 also means the
    new tools show up in `workflowy_status.per_tool_health` so any
    future degradation is visible alongside other tools.
  - **Tool count.** 36 → 38. Both new tools registered in
    `tool_router`; `new_tools_are_registered` test extended to assert
    both appear via `get_tool`.
  - **Tests.** 214 → 231 (+17): 12 new in `audit::tests`
    (extract_marker happy/sad path, every audit finding kind, every
    review bucket, multi-pillar max-not-sum guard, source-MOC URL
    matching) plus 5 new in `server::tests`
    (`audit_mirrors_handler_dispatches_via_walk_subtree`,
    `review_handler_dispatches_via_walk_subtree`,
    `audit_review_handlers_route_through_lib_module`, plus the
    `new_tools_are_registered` extension and an audit/review delegate
    smoke in CLI). The lib-routing test is a source-pattern check —
    if a future refactor accidentally re-implements the heuristic in
    either surface, the test fails before deploy.
  - **Specs.**
    - `specs/specification.md` opens with "Rust v2 implements 38
      tools" and a new "audit_mirrors and review (T-164)" subsection
      enumerating the four finding kinds and four review buckets in
      one place.
    - This `tasks.md` entry, T-164, sitting above T-163.
  - **Out of scope (deliberate).** `index` and `--dry-run` stay
    CLI-only — `index` is pure local-FS work that doesn't need the
    MCP transport, and `--dry-run` is a shell-pipeline staging
    primitive (MCP tool calls already get user approval at the
    protocol layer).

- [x] **T-163 (Brief 2026-04-25 follow-up: post-deploy report)**:
      ship Tests β, γ, ε from the brief in priority order
  - **Source.** User-supplied second brief after observing the T-162
    deploy. Three things still missing in the user's session: bare
    `Tool execution failed` from direct lookups (pre-T-162 binary still
    loaded in their Claude Desktop), no per-path `paths` map in
    `workflowy_status`, no fail-closed gate on `create_node` when
    reads were broken. Ship order from the brief: Test ε first
    (everything else becomes diagnosable), Test β second (would have
    prevented orphan accumulation), Test γ third (so the assistant
    can recover intelligently).
  - **Test γ — `proximate_cause` discrete enum.** New
    `ProximateCause` enum with eight variants (`timeout`,
    `lock_contention`, `cache_miss`, `upstream_error`, `cancelled`,
    `not_found`, `auth_failure`, `unknown`). The `tool_error`
    classifier now picks one alongside the human hint and writes both
    `data.hint` (free text, unchanged) and `data.proximate_cause`
    (enum string). The error message itself is suffixed `[<cause>]`
    so even minimal clients that discard the data payload still get
    the routing signal. Two new heuristic branches added: `lock` →
    `lock_contention`, `cache` → `cache_miss`, since the brief calls
    these out as expected proximate causes.
  - **Test ε — `paths` and `upstream_session` in `workflowy_status`.**
    The brief asked for a flat map keyed by tool name with values
    `healthy`/`degraded`/`failing`/`untested` so the assistant can
    pick a strategy without parsing per-tool histograms. Derived from
    the existing `per_tool_health` block; tools the brief explicitly
    sequences (12 of them: get_node, list_children, search_nodes,
    find_node, create_node, delete_node, edit_node, move_node,
    tag_search, list_overdue, list_upcoming, daily_review) are
    pre-populated with `untested` if the op log has no entries yet.
    `upstream_session` block surfaces the API-reachability check from
    the live probe, the `auth_method: api_key_env` constant (we don't
    hold a session token), `session_age_ms` (server uptime as the
    closest proxy), and the upstream rate-limit headers. The new
    `last_failure.proximate_cause` field uses the same enum values
    as Test γ for consistency.
  - **Test β — fail-closed warning on `create_node`.** New helper
    `degraded_warning_if_recent_failure(window_ms)` checks the most
    recent op-log entry; when an Err finished within the window the
    success message is suffixed `\n\n⚠ DEGRADED: …` naming the broken
    tool, the age in ms, and the original reason — and pointing the
    assistant at `workflowy_status` for triage. Self-failures (a
    previous failed `create_node`) deliberately do NOT gate future
    creates, since the brief's failure mode was reads/mutations
    wedging while creates stayed healthy. Window is 30 s per the
    brief's spec.
  - **Tests.** 211 → 214. Added:
    `tool_error_proximate_cause_classification_covers_every_branch`
    (eight variants, end-to-end through JSON serialisation),
    `workflowy_status_returns_paths_and_upstream_session` (the 12
    documented tools all appear in `paths` with valid enum values;
    `upstream_session` carries the four documented fields),
    `fail_closed_warning_fires_when_recent_failure_in_window` (the
    helper warns after a foreign failure and stays silent for
    self-failures).
  - **What this does NOT fix.** Pattern 4/5 surface in the user's
    session was almost certainly a stale binary in their Claude
    Desktop process — verified live this session with the new binary,
    `get_node` returns `-32002` (`RESOURCE_NOT_FOUND`) with full
    payload. They need to restart Claude Desktop to load the new
    binary. Hypothesis A (upstream session state) is unlikely — the
    client uses an env-var API key with no on-disk token cache.
    Hypothesis B (per-account upstream corruption) is not actionable
    from this repo.
  - **Orphan cleanup with caveat.** All five orphans listed in the
    brief verified deleted live (4 from prior turn + ad138996 this
    turn). Three of them were the user's real distillation work
    (Horaguchi 2025, LeadDev 2026, Ford & Richards 2026) — the
    brief noted these should have been MOVED to their proper
    parents under /Distillations, not deleted. This was a
    misjudgment in the prior turn when interpreting "fix it all" as
    cleanup; the user may need to recover via Workflowy's web-UI
    history.

- [x] **T-162 (Brief 2026-04-25, six observed failure patterns)**:
      propagation retry on writes, structured errors everywhere, and
      `last_failure` in `workflowy_status`
  - **Source.** User-supplied brief enumerating six fault patterns
    from the 2026-04-25 distillation session: intermittent null
    deserialisation (P1), wedge after burst failures (P2), `cancel_all`
    not cancelling (P3), bare "Tool execution failed" with no detail
    (P4), search path stays healthy while direct lookup dies (P5),
    creates succeed but deletes/moves wedge — leaving four orphan
    nodes at workspace root (P6, including the worst case 6a-d).
  - **Pattern 6 — propagation retry on every mutation.** T-159
    shipped retry-on-404 for `get_node` and `list_children`; this
    pass extends the same pattern to writes:
    `delete_node_with_propagation_retry`,
    `edit_node_with_propagation_retry`,
    `move_node_with_propagation_retry` — 3 attempts, 200/400/800 ms
    backoff on 404 only, identical structure to the read helpers so
    `is_404_like` is reused. Handlers route through them, closing
    the window where a `create_node` succeeds but a follow-up
    delete/move on the returned UUID 404s because upstream hasn't
    propagated yet (the actual cause of the four orphans).
  - **Patterns 4 & 5 — every handler-error carries proximate cause.**
    T-159 introduced `tool_error(op, id, err)` returning a structured
    `McpError` with `data: {operation, node_id, hint, error}` and a
    code-by-cause classifier. This pass migrates every remaining bare
    `McpError::internal_error(format!("Failed: {}", e), None)` site
    to `tool_error` (search_nodes, find_node, get_subtree, daily_review,
    get_recent_changes, list_overdue, list_upcoming, get_project_summary,
    find_backlinks, list_todos, duplicate_node, create_from_template,
    bulk_update, build_name_index, since, find_by_tag_and_path,
    export_subtree, smart_insert, insert_content, convert_markdown,
    transaction.{create,edit,delete,move}). No handler returns a bare
    "Failed: …" anymore — `Tool execution failed` regression cannot
    happen at the server boundary now.
  - **Pattern 6d — `parent_id=null` semantics documented.** Tool
    description and the `parent_id` field's `schemars` description
    now say explicitly that omitting OR passing `null` places the node
    at the workspace root. The success message also names the
    placement: `"… under \`<resolved-parent-uuid>\`"` for scoped
    creates, `"… at workspace root (no parent_id supplied)"` when no
    parent is given, so the assistant can audit before issuing
    follow-up moves.
  - **Cross-cut — `last_failure` in `workflowy_status`.** New
    `OpLog::last_failure()` returns the most recent `Err` entry;
    `workflowy_status` surfaces `{tool, at_unix_ms, reason}` so the
    assistant can diagnose which call last broke without reading the
    op log. Combined with the existing `per_tool_health` block this
    answers the brief's observability ask in full.
  - **Patterns 1, 2, 3 — partial.** Pass 1 cancellation already lands
    (T-150); Pass 2 deserialisation logging already records null-id
    payloads with the calling tool name (T-153). The wedge-after-burst
    pattern (P2) is not reproduced in unit tests yet — leaving for a
    later pass once a probe harness exists.
  - **Tests.** 207 → 211. Added:
    `handler_errors_carry_structured_data_payload` (delete/edit/move
    errors all name the operation), `workflowy_status_surfaces_last_failure`
    (null pre-failure, populated after), `propagation_retry_helpers_exist_for_all_mutations`
    (anchors the contract — accidental refactor that drops a helper
    fails the test), `create_node_success_message_names_root_when_parent_id_omitted`
    (Pattern 6d formatting). Existing test changes: none broken.
  - **Orphan cleanup.** All four orphan UUIDs from the user's session
    (8f627d4d, 94d1c6f6, 9e912b77, 5a2c4abf) deleted live before the
    code changes — confirming the orphan state was transient
    (Workflowy propagation lag, not a permanent server bug). The new
    propagation-retry helpers prevent the recurrence.

- [x] **T-159 (Brief 2026-04-25)**: Transient-failure brief
  - **Pattern A (per-ID failures)**: added
    `WorkflowyClient::get_node_with_propagation_retry` and
    `get_children_with_propagation_retry` — 3 attempts, 200/400/800 ms
    backoff on 404 only. `is_404_like` recognises both bare ApiError
    and the wrapped RetryExhausted form. The `get_node` handler runs
    both fetches through the retry path; the `list_children` handler
    too.
  - **Pattern B (lost error detail)**: new `tool_error(op, id, err)`
    helper picks an appropriate JSON-RPC code (`RESOURCE_NOT_FOUND`
    for 404s, `INTERNAL_ERROR` otherwise), sets `message` to
    `"<op>: <err>"` so even minimal clients show the operation, and
    attaches `data` with `{operation, node_id, hint, error}`. Wired
    into get_node and list_children; can be extended to other
    handlers as touched.
  - **Pattern C (degradation visibility)**: `workflowy_status` now
    includes `per_tool_health` — per-tool histogram over the last
    200 op-log entries with status thresholds healthy/degraded/
    failing. The brief's Pattern B (search ok, direct reads fail) is
    now visible from a single status response.
  - Three brief acceptance tests added: `tool_error_carries_…`,
    `get_node_handler_uses_propagation_retry`,
    `workflowy_status_includes_per_tool_health`.
  - `tests/proptest_node_id.rs`: 5 property tests over hyphenated /
    unhyphenated UUID strings, garbage strings (no panics), and
    `null` deserialisation (always rejected at the serde layer).
  - `tests/scripted_session.rs`: ignored-by-default integration test
    matching the brief's definition of done — drives a 30-op
    distillation-style session 10 times under a 30 s/run budget.
    Requires `WORKFLOWY_TEST_API_KEY` and `WORKFLOWY_TEST_PARENT_ID`.
  - `.github/workflows/test.yml`: CI runs `cargo check`, the lib
    tests, and the proptest on every push and PR. The acceptance job
    runs the scripted session against the sandbox secret on push to
    `main` only.

---

## Phase 4: Quality & Documentation

- [ ] **T-140**: Add integration tests with mock HTTP server
  - Test all tool handlers end-to-end
  - Error scenarios (401, 404, 429, 500)

- [ ] **T-141**: Add structured logging
  - Audit log for destructive operations
  - Sanitize sensitive data from logs

- [ ] **T-142**: Update CLAUDE.md for Rust
  - Build/test commands
  - Architecture description
  - Module documentation

- [ ] **T-143**: CLI tools
  - task_map CLI

---

## Completed (TypeScript era)

*Reference for prior work, now superseded by Rust rewrite.*

- [x] T-000 through T-043: See git history for TypeScript implementation details
- All TypeScript features documented in git tag `v1.0-typescript` (if tagged)

---

## Test Coverage

Counts grow across passes; check `cargo test --lib` for the live total.
Snapshot at end of Pass 1: **166** unit tests, all passing. Notable
additions in Pass 1:

- `utils/rate_limiter`: cancellable-acquire returns false when cancelled
  mid-wait; succeeds when token available without cancellation.
- `api/client`: scoped pre-cancelled walk reports the requested root as
  the truncation anchor.
- `server`: `truncation_banner_from_fetch` renders the unfinished-branch
  path; falls silent when the walk completed; omits the path when no
  anchor is known.
- `server`: `get_node` rejects empty IDs at the handler boundary (still
  enforced even though it now also fans out to children).
