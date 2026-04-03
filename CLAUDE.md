# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                  # compile (debug)
cargo build --release        # compile (optimized, LTO)
cargo test --lib             # run all 81 unit tests
cargo test                   # run all tests (unit + integration)
cargo run --bin workflowy-mcp-server  # start MCP server
cargo check                  # type-check without building
```

## Architecture

Rust MCP server for Workflowy content management. Uses `rmcp` 0.16 over stdio transport for Claude Desktop integration.

### Module Structure

- **`src/server.rs`** — MCP tool_router with 17 tool handlers. `#[tool]` proc macros register tools; serde + schemars validate inputs via `Parameters<T>` wrapper.
- **`src/api/client.rs`** — Workflowy API client with exponential backoff retry (3 attempts, jitter, retries on 429/5xx).
- **`src/utils/`** — Reusable modules: cache, date/tag parsing, node paths, subtree collection, rate limiter, job queue.
- **`src/cli/`** — Standalone CLI binaries for concept map and task map generation (stubs).

### Request Flow

```
MCP tool call → serde deserialization → Parameters<T> → handler → WorkflowyClient → retry loop → Workflowy API
```

Write operations invalidate the node cache via `get_cache().invalidate_node(id)`.

### Key Infrastructure

- **Node Cache** (`utils/cache.rs`): Global `lazy_static` singleton, 30s TTL, parking_lot RwLock. O(n) subtree invalidation via parent-children index.
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

- Concept mapping tools not yet ported (get_node_content_for_analysis, render_interactive_concept_map)
- Batch async tools not yet ported (batch_operations, submit_job, get_job_status)
- Request queue and orchestrator are stubs
- bulk_update complete/uncomplete operations are no-ops (API endpoints not modeled yet)

## Testing

Unit tests use `#[cfg(test)]` modules alongside source. No live API calls in unit tests. Integration test in `tests/live_insert.rs` requires `WORKFLOWY_API_KEY`.

```bash
cargo test --lib                           # unit tests only (90 tests)
cargo test --lib -- test_name              # run specific test
cargo test --lib -- --nocapture            # with stdout
```

## Configuration

Environment variables loaded from `.env` via dotenv (`src/config.rs`):
- `WORKFLOWY_API_KEY` (required)
- `DROPBOX_APP_KEY`, `DROPBOX_APP_SECRET`, `DROPBOX_REFRESH_TOKEN` (optional, all-or-none)

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
