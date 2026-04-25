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

- [ ] **T-153 (Pass 2)**: Deserialisation diagnostics + `workflowy_status`
- [ ] **T-154 (Pass 3)**: Operation log + `get_recent_tool_calls`
- [ ] **T-155 (Pass 4)**: Authoritative name index + short-hash IDs
- [ ] **T-156 (Pass 5)**: Heavy-workflow primitives (batch / transaction /
  edit_node hardening / move_node retry)
- [ ] **T-157 (Pass 6)**: Mirror primitives + API expansion
- [ ] **T-158 (Pass 7)**: proptest + load harness + CI acceptance

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
