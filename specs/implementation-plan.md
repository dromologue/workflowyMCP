# Implementation Plan

> Technical approach for building and maintaining the Workflowy MCP Server.

## Architecture Overview

```
+-------------------------------------------------------------+
|                     Claude Desktop                          |
|                         (Client)                            |
+----------------------------+--------------------------------+
                             | stdio (JSON-RPC)
                             v
+-------------------------------------------------------------+
|                    MCP Server (Rust)                         |
|  +-------------+  +-------------+  +---------------------+  |
|  |   rmcp      |  |   Cache     |  |   Config/Env        |  |
|  | tool_router |  |  (30s TTL)  |  |   (.env + dotenv)   |  |
|  +------+------+  +------+------+  +----------+----------+  |
|         |                |                     |             |
|         +----------------+---------------------+             |
|                          v                                   |
|  +------------------------------------------------------+   |
|  |              Workflowy API Client                      |   |
|  |  - reqwest HTTP (auth, headers)                        |   |
|  |  - serde JSON deserialization                          |   |
|  |  - Exponential backoff + jitter retry                  |   |
|  +----------------------------+--------------------------+   |
+-------------------------------+------------------------------+
                                | HTTPS
                                v
+-------------------------------------------------------------+
|                  Workflowy REST API                          |
|                 (workflowy.com/api/v1)                       |
+-------------------------------------------------------------+
```

## Technology Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Language | Rust (2021 edition) | Memory safety, performance, no runtime |
| Async Runtime | Tokio | Industry standard for async Rust |
| MCP SDK | rmcp 0.16 | Rust MCP framework with proc macros |
| HTTP Client | reqwest 0.12 | Async HTTP with JSON support |
| Serialization | serde + serde_json | Type-safe (de)serialization |
| Schema | schemars 1.0 | JSON Schema generation for tool params |
| Error Handling | thiserror 2.0 | Derive macro for custom errors |
| Logging | tracing + tracing-subscriber | Structured async logging |
| Config | dotenv 0.15 | Environment variable loading |
| Concurrency | parking_lot 0.12 | Fast RwLock (no poisoning) |
| CLI | clap 4.4 | Derive-based argument parsing |
| Transport | rmcp stdio | Claude Desktop compatibility |

## Module Structure

```
src/
+-- lib.rs                    # Library exports
+-- main.rs                   # MCP server entry point
+-- server.rs                 # MCP tool_router: 23 tool handlers
+-- error.rs                  # Custom error types (WorkflowyError)
+-- config.rs                 # Config validation, constants
+-- types.rs                  # Core types (WorkflowyNode, etc.)
+-- validation.rs             # Input validation (UUID, text, truncation)
+-- api/
|   +-- mod.rs
|   +-- client.rs             # Workflowy API client with retry + path validation
+-- utils/
|   +-- mod.rs
|   +-- cache.rs              # Node cache with O(n) subtree invalidation
|   +-- rate_limiter.rs       # Token bucket rate limiter
|   +-- job_queue.rs          # Background job queue with TTL cleanup
|   +-- date_parser.rs        # Due date extraction (due:, #due-, bare YYYY-MM-DD)
|   +-- tag_parser.rs         # Tag/assignee extraction (#tag, @mention)
|   +-- node_paths.rs         # Hierarchical path building
|   +-- subtree.rs            # Subtree collection, todo detection
|   +-- request_queue.rs      # [STUB] Batch request queue
|   +-- orchestrator.rs       # [STUB] Multi-worker orchestrator
|   +-- text_processing.rs    # [STUB] UUID validation
|   +-- task_map.rs           # [STUB] Task map generator
+-- tools/
|   +-- mod.rs
|   +-- insert.rs             # [STUB] Insert tool helpers
|   +-- search.rs             # [STUB] Search tool helpers
+-- cli/
    +-- mod.rs
    +-- task_map.rs           # CLI tool for task maps [STUB]

tests/
+-- live_insert.rs            # Integration test (real API)
```

### MCP Tools (23 total — in server.rs)

| Category | Tools |
|----------|-------|
| Search & Navigation | search_nodes, **find_node**, get_node, **list_children**, tag_search, get_subtree, **find_backlinks** |
| Content Creation | create_node, insert_content (hierarchical), **smart_insert**, **convert_markdown** |
| Content Modification | edit_node, move_node, delete_node, **duplicate_node**, **create_from_template**, **bulk_update** |
| Todo Management | **list_todos** |
| Due Dates & Scheduling | **list_upcoming**, **list_overdue**, **daily_review** |
| Project Management | **get_project_summary**, **get_recent_changes** |

### MCP Tools (planned — remaining TypeScript feature parity)

| Category | Tools |
|----------|-------|
| Batch & Async | batch_operations, submit_job, get_job_status |

## Key Design Decisions

### ADR-001: Local Search Over API

**Context**: Workflowy has no search API endpoint.

**Decision**: Export all nodes, cache locally, search in-memory.

**Consequences**:
- (+) Fast repeated searches
- (+) Complex query support possible
- (-) Stale data between cache refreshes
- (-) Memory usage scales with outline size

### ADR-002: Hierarchical Insertion via Indentation

**Context**: Need to preserve structure when inserting multi-line content.

**Decision**: Parse indentation (2 spaces = 1 level), create nodes with parent references.

**Status**: Currently creates flat nodes. Hierarchy parsing is a TODO.

### ADR-004: Cache Invalidation on Write

**Context**: Writes make cached node list stale.

**Decision**: Invalidate affected nodes on write operations. O(n) subtree invalidation via parent-children index.

### ADR-005: Bottom-Default Insertion Order

**Context**: Workflowy's "top" position inserts each node above the previous, reversing order.

**Decision**: Default to "bottom" position for all insertions.

### ADR-008: Retry Logic with Exponential Backoff

**Configuration**:
- Max attempts: 3
- Base delay: 1000ms, Max delay: 10000ms
- Retryable: 429, 500, 502, 503, 504
- Jitter: +/-10% to prevent thundering herd

### ADR-013: Rust Rewrite

**Context**: TypeScript codebase had critical issues (promise handling bugs, path traversal, memory leaks, O(n^2) cache, unsafe type casts).

**Decision**: Rewrite in Rust using rmcp for compile-time safety guarantees.

**Key improvements**:
- Result<T> forces error handling (no silent failures)
- Ownership system prevents memory leaks
- parking_lot::RwLock for thread-safe shared state
- serde for type-safe deserialization (no unsafe casts)
- Path traversal prevention with canonicalize()

## Error Handling Strategy

```rust
#[derive(Error, Debug)]
pub enum WorkflowyError {
    ApiError { status: u16, message: String, source: Option<Box<dyn Error>> },
    RetryExhausted { attempts: u32, reason: String },
    InvalidPath { reason: String },
    InvalidInput { reason: String },
    ConfigError { reason: &'static str },
    CacheError { reason: String },
    ParseError { reason: String },
    // ... more variants
}

// All functions return Result<T> = std::result::Result<T, WorkflowyError>
// Error propagation is forced by the compiler
```

## Testing Strategy

### Unit Tests (in-module, `#[cfg(test)]`)

| Module | Tests | Coverage |
|--------|-------|----------|
| server.rs (params + info + tools) | 33 tests | All 23 param structs, server info, tool listing |
| validation.rs | 10 tests | UUID, text, truncation |
| api/client.rs | 3 tests | Path traversal, valid paths |
| utils/cache.rs | 2 tests | Insert/get, O(n) perf |
| utils/rate_limiter.rs | 2 tests | Token bucket, async wait |
| utils/job_queue.rs | 2 tests | Lifecycle, cleanup bounds |
| utils/date_parser.rs | 14 tests | Due date formats, priority, overdue detection |
| utils/tag_parser.rs | 11 tests | Tags, assignees, dedup, node parsing |
| utils/node_paths.rs | 6 tests | Root, nested, missing parent, truncation |
| utils/subtree.rs | 10 tests | Flat/nested trees, todo detection, completion |
| **Total** | **90 tests** | |

### Integration Tests

- `tests/live_insert.rs` — Real API test (requires WORKFLOWY_API_KEY)

### Running Tests

```bash
cargo test --lib           # Unit tests only
cargo test                 # All tests
cargo test --lib -- --nocapture  # With stdout
```

## Deployment

### Build Process

```bash
cargo build --release      # Optimized binary (LTO enabled)
```

### Claude Desktop Configuration

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "/path/to/workflowy-mcp-server"
    }
  }
}
```

## Remaining Work

### High Priority
- [x] Add find_node tool (match_mode: exact/contains/starts_with)
- [x] Add daily_review, list_overdue, list_upcoming tools
- [x] Add smart_insert tool
- [x] Add get_recent_changes, get_project_summary tools
- [x] Rename get_children → list_children
- [x] Implement hierarchical insert_content (parse indentation)
- [x] Add find_backlinks, list_todos tools
- [x] Add duplicate_node, create_from_template, bulk_update tools
- [x] Add convert_markdown tool

### Medium Priority
- [ ] Implement request queue with batching
- [ ] Implement orchestrator (multi-worker insertion)
- [ ] Add batch_operations, submit_job, get_job_status

### Low Priority
- [ ] CLI tools (task_map)
- [ ] Performance optimization
- [ ] Metrics/monitoring exports
