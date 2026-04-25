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
