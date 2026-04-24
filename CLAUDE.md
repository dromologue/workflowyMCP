# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                  # compile (debug)
cargo build --release        # compile (optimized, LTO)
cargo test --lib             # run all 122 unit tests
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
- **Date Parser** (`utils/date_parser.rs`): Extracts due dates from node text. Priority: `due:YYYY-MM-DD` > `#due-YYYY-MM-DD` > bare date.
- **Tag Parser** (`utils/tag_parser.rs`): Extracts `#tags` and `@mentions` from node text.
- **Node Paths** (`utils/node_paths.rs`): Builds hierarchical display paths by following parent_id chains.
- **Subtree** (`utils/subtree.rs`): Collects all descendants of a node. Todo/completion detection.

### MCP Tools (23 total)

| Category | Tools |
|----------|-------|
| Search & Navigation | search_nodes, find_node, get_node, list_children, tag_search, get_subtree, find_backlinks |
| Content Creation | create_node, insert_content (hierarchical), smart_insert, convert_markdown |
| Content Modification | edit_node, move_node, delete_node, duplicate_node, create_from_template, bulk_update |
| Todo Management | list_todos |
| Due Dates & Scheduling | list_upcoming, list_overdue, daily_review |
| Project Management | get_project_summary, get_recent_changes |

### Workflowy API Constraints

- All endpoints use `/nodes` or `/nodes/{id}`
- API base: `https://workflowy.com/api/v1`
- Auth: Bearer token via `WORKFLOWY_API_KEY`

### Known Limitations

- `bulk_update` `complete` / `uncomplete` are rejected at the handler boundary — the Workflowy completion endpoints are not yet modelled in the client. Tag-based workflows are the interim substitute.
- Subtree fetches cap at `defaults::MAX_SUBTREE_NODES` (10 000). Tools surface a `truncated` flag when the cap is hit; `duplicate_node`, `create_from_template`, and `bulk_update` (delete) refuse to run against a truncated view.

## Testing

Unit tests use `#[cfg(test)]` modules alongside source. No live API calls in unit tests. Integration test in `tests/live_insert.rs` requires `WORKFLOWY_API_KEY`.

```bash
cargo test --lib                           # unit tests only (122 tests)
cargo test --lib -- test_name              # run specific test
cargo test --lib -- --nocapture            # with stdout
```

## Configuration

Environment variables loaded from `.env` via dotenv (`src/config.rs`):
- `WORKFLOWY_API_KEY` (required)

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
