# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                  # compile (debug)
cargo build --release        # compile (optimized, LTO)
cargo test --lib             # run all unit tests (~283)
cargo test                   # run all tests (unit + integration)
cargo run --bin workflowy-mcp-server  # start MCP server
cargo check                  # type-check without building
```

## Architecture

Rust MCP server for Workflowy content management. Uses `rmcp` 0.16 over stdio transport for Claude Desktop integration.

### Module Structure

- **`src/server.rs`** — MCP tool_router with 38 tool handlers. `#[tool]` proc macros register tools; serde + schemars validate inputs via `Parameters<T>` wrapper (a drop-in replacement for `rmcp::Parameters<T>` that records framework-level deserialization failures to the op log before returning the typed `McpError`). Uses `NodeId` newtype for all node ID parameters. Every parameter struct carries `#[serde(deny_unknown_fields)]` so a typo'd field name fails fast with a recorded error instead of silently defaulting to `None`. **The wrapper struct must keep its name `Parameters`**: `rmcp-macros 0.16` discovers a tool's parameter type by matching the literal identifier `Parameters` on the last path segment of the function-arg type (`rmcp-macros/src/common.rs:64`). A wrapper named anything else makes the macro fall back to a hardcoded `{"type": "object", "properties": {}}` schema for every parameter-bearing tool — silently strips arguments at the wire (the cowork client then validates against the empty schema and drops them all). Pinned by `parameter_bearing_tools_publish_non_empty_input_schema_properties` in `src/server.rs::tests`.
- **`src/api/client.rs`** — Workflowy API client with exponential backoff retry. `get_subtree_recursive()` fetches tree level-by-level via `/nodes?parent_id=` with configurable depth limit (crucial for 250k+ node trees). Returns `SubtreeFetch { nodes, truncated, limit }`; when the `MAX_SUBTREE_NODES` cap (10 000 by default, `defaults.rs`) is hit the flag is surfaced in every tool response so callers can narrow the scope.
- **`src/defaults.rs`** — Centralized constants for all magic numbers (cache TTL, retry config, validation limits, tree depth defaults).
- **`src/types.rs`** — Core types including `NodeId` newtype with `Deref<str>`, `AsRef<str>`, `PartialEq<String>`, and `JsonSchema` impls.
- **`src/utils/`** — Reusable modules: cache (injectable), date/tag parsing, node paths, subtree collection, rate limiter, job queue.
- **`src/cli/`** — Standalone CLI binaries (task map generation stub).

### Request Flow

```
MCP tool call → Parameters<T> (serde + op_log on failure) → tool_handler! (op_log recorder + run_handler: kind-specific budget + cancel) → handler body → WorkflowyClient → retry loop → Workflowy API
```

Two recorder points sit on this path:
1. **`Parameters::from_context_part`** records an Err entry to the op
   log when serde rejects the payload — covering the path that the
   rmcp framework would otherwise drop before the handler body runs.
2. **`tool_handler!`** (the standard wrapper around every
   non-diagnostic handler body) records the handler's own outcome
   (Ok or Err) AND wraps the body in `run_handler(name, kind, ...)`
   so cancel-registry observation and the kind's wall-clock deadline
   apply uniformly. Diagnostics (`health_check`, `workflowy_status`,
   `cancel_all`, `get_recent_tool_calls`, `build_name_index`) keep
   the older `record_op!` macro because they own short, custom
   budgets that predate the taxonomy.

Together they guarantee every tool call attempt produces exactly one
op-log entry AND that no handler can sit past its kind's budget or
ignore `cancel_all`. Brief 2026-05-02 named the framework-rejection
silence and the unwrapped-write 4-minute hang as the two dominant
debugging black holes; `Parameters` plus `tool_handler!` close
both.

Write operations invalidate the node cache via `self.cache.invalidate_node(id)` (cache is dependency-injected).

### Key Infrastructure

- **Node Cache** (`utils/cache.rs`): Injectable (or global `lazy_static` default), 30s TTL, parking_lot RwLock. O(n) subtree invalidation via parent-children index.
- **Rate Limiter** (`utils/rate_limiter.rs`): Token bucket, 10 req/sec, burst 20.
- **Job Queue** (`utils/job_queue.rs`): Background job lifecycle with TTL cleanup (tokio::spawn). Max 1000 job history.
- **Cancel Registry** (`utils/cancel.rs`): Generation-counter cancellation primitive. `cancel_all` bumps the counter so every outstanding `CancelGuard` returns `is_cancelled = true` at its next checkpoint; guards taken afterwards are fresh.
- **Name Index** (`utils/name_index.rs`): Case-insensitive `name -> [entry]` map plus short-hash → UUID maps (12-char URL-suffix and 8-char doc prefix), fed by every subtree walk. Backed by `parking_lot::RwLock`; invalidated per-node on every write. **Persisted to disk** at `$WORKFLOWY_INDEX_PATH` (default `$HOME/code/secondBrain/memory/name_index.json`): rehydrated on server startup, checkpointed every 30 s when dirty via write-then-rename, refreshed by a 30-minute background walk (calibrated against 250 k-node trees so quasi-full coverage builds up over a working day rather than a working week). **Auto-walks on short-hash miss**: `resolve_node_ref` fires a workspace walk with `RESOLVE_WALK_TIMEOUT_MS` budget when a short hash isn't cached; a watcher polls the index every 100 ms and cancels the walk as soon as the target appears. Callers no longer need to run `build_name_index` manually before passing a Workflowy URL fragment.
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

### MCP Tools

| Category | Tools |
|----------|-------|
| Search & Navigation | search_nodes, find_node, get_node, list_children, tag_search, get_subtree, find_backlinks, find_by_tag_and_path, node_at_path, path_of, resolve_link, since |
| Content Creation | create_node, batch_create_nodes, insert_content (hierarchical), smart_insert, convert_markdown |
| Content Modification | edit_node, move_node, delete_node, duplicate_node, create_from_template, bulk_update, bulk_tag, transaction |
| Todo Management | list_todos, complete_node |
| Due Dates & Scheduling | list_upcoming, list_overdue, daily_review |
| Project Management | get_project_summary, get_recent_changes |
| Diagnostics & Ops | health_check, workflowy_status, cancel_all, build_name_index, get_recent_tool_calls, audit_mirrors, review, export_subtree |

### CLI Parity (`wflow-do`)

The `wflow-do` binary at `src/bin/wflow_do.rs` is in **full surface parity** with the MCP tool list above. Every non-diagnostic, non-stub MCP tool has a matching CLI subcommand routed through the same `WorkflowyClient`. New MCP tools must land with their `wflow-do` subcommand in the same commit. The build-time test `cli_covers_every_non_diagnostic_mcp_tool` enumerates the (mcp-tool → cli-subcommand) pairs and fails CI if a tool is added without its CLI counterpart. `convert_markdown` (pure local transform) and `create_mirror` (stub) are intentionally excluded; `cancel_all` and `get_recent_tool_calls` ship as no-op CLI surfaces because the op log only exists in the running MCP server.

### Workflowy API Constraints

- All endpoints use `/nodes` or `/nodes/{id}`
- API base: `https://workflowy.com/api/v1`
- Auth: Bearer token via `WORKFLOWY_API_KEY`

### Known Limitations

- `bulk_update` supports `complete`, `uncomplete`, `delete`, `add_tag`, `remove_tag`. `complete`/`uncomplete` route through `client.set_completion`, the same code path the single-node `complete_node` tool uses. The wire payload is `POST /nodes/{id}` with `{"completed": true}` or `{"completed": false}` (the read-side `WorkflowyNode::completed` boolean has no serde alias, so the wire field is literally `completed`); pinned by `tests::write_field_names::set_completion_*` so the description→note failure shape cannot recur on the completion path. The `transaction` tool also accepts `complete` / `uncomplete` ops with rollback to the prior boolean state via `RestoreCompletion`.
- Subtree fetches cap at `defaults::MAX_SUBTREE_NODES` (10 000) **and** at `defaults::SUBTREE_FETCH_TIMEOUT_MS` (20 000 ms). Tools surface a `truncated` flag plus a `truncation_reason` (`node_limit` / `timeout` / `cancelled`) when either budget fires; `duplicate_node`, `create_from_template`, and `bulk_update` (delete) refuse to run against a truncated view.
- `find_node` and `search_nodes` refuse to scan from the workspace root when `parent_id` is omitted. Pass `parent_id`, set `allow_root_scan=true` to accept the full walk, or set `use_index=true` after running `build_name_index` to serve from the opportunistic name index. The 2026-05-03 eval run revealed every search scoped under Distillations was timing out at the 20 s walk budget once the subtree grew past the 10 000-node walk cap. `use_index=true` answers in O(1) from the persistent name index without burning any walk budget; the trade-off is name-only match (description content still needs the live walk). The truncation banner emitted on timeout / node-cap responses now names this recovery path explicitly so callers don't have to read the docs.
- **JSON-truncation envelope consistency.** Every walk-shaped tool that emits JSON includes the same four fields next to its `"truncation_limit"`: `truncated`, `truncation_limit`, `truncation_reason` (`timeout` / `node_limit` / `cancelled` / null), and `truncation_recovery_hint` (the empty string when not truncated; otherwise the same `TRUNCATION_RECOVERY_HINT` string the markdown banner emits). Pinned by `every_walk_tool_emits_full_truncation_envelope_in_json` which grep-audits the source — adding a new walk-shaped tool that emits `"truncation_limit"` without the reason + hint companions fails the build.
- `insert_content` caps at `defaults::MAX_INSERT_CONTENT_LINES` (200) per call. Above the soft warn threshold (`SOFT_WARN_INSERT_CONTENT_LINES`, 80) the success response includes a chunking hint. The hard cap exists because oversized payloads have been observed to fail at the MCP transport layer before reaching the handler — split into batches of ≤80 and pass each batch's `last_inserted_id` as `parent_id` of the next call to preserve hierarchy.
- `edit_node` requires at least one of `name` or `description`; an empty patch is rejected at the handler boundary rather than POSTed as `{}`. The wire field for the description body is `note` (Workflowy's API name); `client.rs` maps `description` → `note` at the boundary on writes, and serde's `alias = "note"` covers reads. Sending `description` literally returns 200 OK with the field silently dropped — that was the 2026-05-02 P2.4 field-loss symptom, and the regression is now pinned by `tests::write_field_names` in `src/api/client.rs`.
- `move_node` invalidates the cache for the node, the new parent, **and** the old parent (captured via a pre-read). The name index is invalidated for the moved node.
- `cancel_all` bumps the cancel-registry generation: every outstanding tree walk returns partial results on its next checkpoint with `truncation_reason = "cancelled"`. New calls start fresh.
- `health_check` calls `GET /nodes` (top-level only) with a 5-second budget; it is safe to use as a liveness probe regardless of tree size.
- `list_children` accepts both `node_id` and `parent_id` for the parent-to-list-children-of, since every other tool in this server that scopes to a parent uses `parent_id`. Both names reach the handler with the same semantics. Any third name fails the `deny_unknown_fields` check with a typed `unknown field` error.
- `degraded` self-clears on recovery: once the failing tool returns OK, `workflowy_status.last_failure` and the `DEGRADED` warning that gates `create_node` both clear together via `OpLog::last_unrecovered_failure`. A success on a *different* tool does NOT clear another tool's failure warning.

## Testing

Unit tests use `#[cfg(test)]` modules alongside source. No live API calls in unit tests. Integration test in `tests/live_insert.rs` requires `WORKFLOWY_API_KEY`.

```bash
cargo test --lib                           # unit tests only (286 tests)
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
