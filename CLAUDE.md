# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                  # compile (debug)
cargo build --release        # compile (optimized, LTO)
cargo test --lib             # run all unit tests (~242)
cargo test                   # run all tests (unit + integration)
cargo run --bin workflowy-mcp-server  # start MCP server
cargo check                  # type-check without building
```

## Architecture

Rust MCP server for Workflowy content management. Uses `rmcp` 0.16 over stdio transport for Claude Desktop integration.

### Module Structure

- **`src/server.rs`** — MCP tool_router with 23 tool handlers. `#[tool]` proc macros register tools; serde + schemars validate inputs via `Parameters<T>` wrapper. Uses `NodeId` newtype for all node ID parameters.
- **`src/api/client.rs`** — Workflowy API client with exponential backoff retry. `get_subtree_recursive()` fetches tree level-by-level via `/nodes?parent_id=` with configurable depth limit (crucial for 250k+ node trees). Returns `SubtreeFetch { nodes, truncated, limit }`; when the `MAX_SUBTREE_NODES` cap (10 000 by default, `defaults.rs`) is hit the flag is surfaced in every tool response so callers can narrow the scope.
- **`src/defaults.rs`** — Centralized constants for all magic numbers (cache TTL, retry config, validation limits, tree depth defaults).
- **`src/types.rs`** — Core types including `NodeId` newtype with `Deref<str>`, `AsRef<str>`, `PartialEq<String>`, and `JsonSchema` impls.
- **`src/utils/`** — Reusable modules: cache (injectable), date/tag parsing, node paths, subtree collection, rate limiter, job queue.
- **`src/cli/`** — Standalone CLI binaries (task map generation stub).

### Request Flow

```
MCP tool call → serde deserialization → Parameters<T> → handler → WorkflowyClient → retry loop → Workflowy API
```

Write operations invalidate the node cache via `self.cache.invalidate_node(id)` (cache is dependency-injected).

### Key Infrastructure

- **Node Cache** (`utils/cache.rs`): Injectable (or global `lazy_static` default), 30s TTL, parking_lot RwLock. O(n) subtree invalidation via parent-children index.
- **Rate Limiter** (`utils/rate_limiter.rs`): Token bucket, 5 req/sec, burst 10.
- **Job Queue** (`utils/job_queue.rs`): Background job lifecycle with TTL cleanup (tokio::spawn). Max 1000 job history.
- **Cancel Registry** (`utils/cancel.rs`): Generation-counter cancellation primitive. `cancel_all` bumps the counter so every outstanding `CancelGuard` returns `is_cancelled = true` at its next checkpoint; guards taken afterwards are fresh.
- **Name Index** (`utils/name_index.rs`): Case-insensitive `name -> [entry]` map plus short-hash → UUID maps (12-char URL-suffix and 8-char doc prefix), fed by every subtree walk. Backed by `parking_lot::RwLock`; invalidated per-node on every write. **Persisted to disk** at `$WORKFLOWY_INDEX_PATH` (default `$HOME/code/secondBrain/memory/name_index.json`): rehydrated on server startup, checkpointed every 30 s when dirty via write-then-rename, refreshed by a 6-hour background walk. **Auto-walks on short-hash miss**: `resolve_node_ref` fires a workspace walk with `RESOLVE_WALK_TIMEOUT_MS` budget when a short hash isn't cached; a watcher polls the index every 100 ms and cancels the walk as soon as the target appears. Callers no longer need to run `build_name_index` manually before passing a Workflowy URL fragment.
- **Date Parser** (`utils/date_parser.rs`): Extracts due dates from node text. Priority: `due:YYYY-MM-DD` > `#due-YYYY-MM-DD` > bare date.
- **Tag Parser** (`utils/tag_parser.rs`): Extracts `#tags` and `@mentions` from node text.
- **Node Paths** (`utils/node_paths.rs`): Builds hierarchical display paths by following parent_id chains.
- **Subtree** (`utils/subtree.rs`): Collects all descendants of a node. Todo/completion detection.

### Tree Walk Controls

`get_subtree_with_controls` on the client is the single entry point for every tree walk. All server handlers route through `WorkflowyMcpServer::walk_subtree`, which wraps:

1. A wall-clock deadline (`defaults::SUBTREE_FETCH_TIMEOUT_MS`, 20 s).
2. A `CancelGuard` from the shared `CancelRegistry`.
3. Opportunistic name-index ingestion of every returned node.

`SubtreeFetch` carries `truncation_reason: Option<TruncationReason>` (`NodeLimit`/`Timeout`/`Cancelled`) and `elapsed_ms`. Handlers surface both through the `truncation_banner_with_reason` helper and the JSON responses.

Per-level child fetches run via `futures::stream::buffer_unordered(SUBTREE_FETCH_CONCURRENCY)` (5). The rate limiter serialises requests internally, so this parallelism collapses HTTP RTT stalls without exceeding the sustained rate.

### MCP Tools (26 total)

| Category | Tools |
|----------|-------|
| Search & Navigation | search_nodes, find_node, get_node, list_children, tag_search, get_subtree, find_backlinks |
| Content Creation | create_node, insert_content (hierarchical), smart_insert, convert_markdown |
| Content Modification | edit_node, move_node, delete_node, duplicate_node, create_from_template, bulk_update |
| Todo Management | list_todos |
| Due Dates & Scheduling | list_upcoming, list_overdue, daily_review |
| Project Management | get_project_summary, get_recent_changes |
| Diagnostics & Ops | health_check, cancel_all, build_name_index |

### Workflowy API Constraints

- All endpoints use `/nodes` or `/nodes/{id}`
- API base: `https://workflowy.com/api/v1`
- Auth: Bearer token via `WORKFLOWY_API_KEY`

### Known Limitations

- `bulk_update` `complete` / `uncomplete` are rejected at the handler boundary — the Workflowy completion endpoints are not yet modelled in the client. Tag-based workflows are the interim substitute.
- Subtree fetches cap at `defaults::MAX_SUBTREE_NODES` (10 000) **and** at `defaults::SUBTREE_FETCH_TIMEOUT_MS` (20 000 ms). Tools surface a `truncated` flag plus a `truncation_reason` (`node_limit` / `timeout` / `cancelled`) when either budget fires; `duplicate_node`, `create_from_template`, and `bulk_update` (delete) refuse to run against a truncated view.
- `find_node` refuses to scan from the workspace root when `parent_id` is omitted. Pass `parent_id`, set `allow_root_scan=true` to accept the full walk, or set `use_index=true` after running `build_name_index` to serve from the opportunistic name index.
- `edit_node` requires at least one of `name` or `description`; an empty patch is rejected at the handler boundary rather than POSTed as `{}`.
- `move_node` invalidates the cache for the node, the new parent, **and** the old parent (captured via a pre-read). The name index is invalidated for the moved node.
- `cancel_all` bumps the cancel-registry generation: every outstanding tree walk returns partial results on its next checkpoint with `truncation_reason = "cancelled"`. New calls start fresh.
- `health_check` calls `GET /nodes` (top-level only) with a 5-second budget; it is safe to use as a liveness probe regardless of tree size.

## Testing

Unit tests use `#[cfg(test)]` modules alongside source. No live API calls in unit tests. Integration test in `tests/live_insert.rs` requires `WORKFLOWY_API_KEY`.

```bash
cargo test --lib                           # unit tests only (159 tests)
cargo test --lib -- test_name              # run specific test
cargo test --lib -- --nocapture            # with stdout
```

## Configuration

Environment variables loaded from `.env` via dotenv (`src/config.rs`):
- `WORKFLOWY_API_KEY` (required)
- `WORKFLOWY_INDEX_PATH` (optional) — disk path for the persistent name index. Default `$HOME/code/secondBrain/memory/name_index.json`. Empty string disables persistence.

## Templates and setup

The repo ships generic templates so a fresh user (or LLM agent bootstrapping one) can stand up the same workflow without inheriting the original author's specific node IDs:

- `docs/SETUP.md` — step-by-step guide for an LLM to install the MCP and provision the user's `~/code/secondBrain/` directory.
- `templates/skills/wflow/SKILL.md` — generic copy of the wflow skill with no user-specific node IDs.
- `templates/secondbrain/` — skeleton of the operational secondBrain directory (README, memory schema, drafts/session-logs/briefs subdirs).

User-specific data (cached node IDs, drafts, session logs, briefs) lives only in `~/code/secondBrain/` — never in this repo.

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| rmcp 0.16 | MCP server framework (proc macros, stdio transport) |
| tokio | Async runtime |
| reqwest | HTTP client |
| serde + serde_json | Serialization |
| schemars | JSON Schema for tool params |
| thiserror | Custom error types |
| tracing | Structured logging |
| parking_lot | Fast RwLock |
| chrono | Date handling |
| regex | Pattern matching (dates, tags) |
| futures | `buffer_unordered` for parallel child fetches |
