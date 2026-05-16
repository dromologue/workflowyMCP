# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Read This First — Project Constitution

**Before starting any non-trivial task in this repository, consult [`specs/constitution.md`](specs/constitution.md).** It is the canonical reference for every contributor — human or AI agent — and establishes:

- The eight Core Principles that govern every design decision (correctness first, typed contracts, resilience, helper-first construction, single source of truth across MCP + CLI, pin-tested invariants, paranoid security, public utility).
- The **Definition of Done** checklist — every commit must satisfy each item before it ships.
- The **Conflict-Resolution Hierarchy** for when principles compete (correctness > security > simplicity > cross-surface consistency > maintainability > performance > extensibility).
- Pointers into the four detail files: `specs/principles-architecture.md`, `specs/principles-development.md`, `specs/principles-mcp.md`, `specs/principles-security.md`.

When this file (CLAUDE.md) and the constitution disagree, the constitution wins — this file is the operational guide (commands, project structure, known limitations); the constitution is the law.

## Commands

```bash
cargo build                  # compile (debug)
cargo build --release        # compile (optimized, LTO)
cargo test --lib             # run all unit tests (364)
cargo test                   # run all tests (unit + integration)
cargo run --bin workflowy-mcp-server  # start MCP server
cargo check                  # type-check without building

# Skill bundling (claude.ai upload)
scripts/bundle-skill.sh                      # bundle ~/.claude/skills/wflow → dist/wflow.skill.zip
scripts/bundle-skill.sh --src <dir>          # alternate skill source
scripts/bundle-skill.sh --out <zip>          # alternate output path
```

The bundler validates frontmatter (no `<` or `>` characters, description ≤ 1024 chars) and writes the skill source as a `<skill-name>/` directory inside the zip — the structure claude.ai's Settings → Skills upload expects.

### Auto-bundle on skill edit (mandatory)

Whenever any file under `~/.claude/skills/wflow/` is edited (`SKILL.md`, `distillation_taxonomy.md`, `workflowy_node_links.md`, or any companion file), the bundle at `dist/wflow.skill.zip` MUST be rebuilt and the user alerted. The harness enforces this via a `PostToolUse` hook in `.claude/settings.json` matching `Edit|Write|MultiEdit`, which delegates to `scripts/auto-bundle-skill.sh`. The wrapper:

1. Reads the tool-call payload from stdin and pulls `tool_input.file_path`.
2. No-ops if the file isn't under `~/.claude/skills/wflow/`.
3. Otherwise runs `scripts/bundle-skill.sh` and prints a 🛎 alert line on stderr naming the rebuilt zip path. The alert is what the user sees — do NOT also re-state the rebuild in your own response, the hook output is the single source of truth.
4. On bundler failure (frontmatter violation, source error) prints a ⚠ STALE warning with the bundler's stderr so the user can fix the underlying issue.

This means: **never run `scripts/bundle-skill.sh` by hand after editing the skill in this project** — the hook has already done it, and a manual rerun is wasted work. Only invoke the bundler explicitly when bundling from a non-default `--src` (e.g. the generic template) or to a non-default `--out`.

Never ask the user to "remember to re-bundle" — the hook removes that obligation. The only outstanding step on the user's side is re-uploading the freshly-bundled zip to claude.ai → Settings → Skills and starting a fresh session, which the alert message names explicitly.

## Architecture

Rust MCP server for Workflowy content management. Uses `rmcp` 0.16 over stdio transport for Claude Desktop integration.

### Module Structure

- **`src/server/`** — MCP tool_router split across `mod.rs` (server struct, helpers, tool_handler!, all 40+ #[tool] handlers, tests) and `params.rs` (parameter struct definitions, ~40 of them). The 2026-05-03 architecture-review file split moved the param struct slab out of mod.rs to make the handler-side navigable. `#[tool]` proc macros register tools; serde + schemars validate inputs via `Parameters<T>` wrapper. Uses `NodeId` newtype for all node ID parameters. Every parameter struct carries `#[serde(deny_unknown_fields)]` so a typo'd field name fails fast with a recorded error instead of silently defaulting to `None`. **The wrapper struct must keep its name `Parameters`**: `rmcp-macros 0.16` discovers a tool's parameter type by matching the literal identifier `Parameters` on the last path segment of the function-arg type (`rmcp-macros/src/common.rs:64`). A wrapper named anything else makes the macro fall back to a hardcoded `{"type": "object", "properties": {}}` schema for every parameter-bearing tool — silently strips arguments at the wire. Pinned by `parameter_bearing_tools_publish_non_empty_input_schema_properties`.

  **Helper inventory** (all in `server/mod.rs`):
  - `tool_handler!(self, name, kind, params, body)` — the standard wrapper for every non-diagnostic handler. Combines op-log recording + cancel-registry observation + kind-keyed wall-clock budget.
  - `validate_and_resolve(raw) -> Result<String, McpError>` — collapses the validate-then-resolve pair every handler taking a node_id needs.
  - `invalidate_for_mutation(&[id, ...])` — pre-call cache + name-index invalidation. Every write handler uses it BEFORE the API call so a wrapper-level timeout/cancel can't strand stale data.
  - `truncation_envelope(truncated, limit, reason)` and `with_truncation_envelope(payload, ...)` — the four-field JSON envelope for walk-shaped responses (`truncated`, `truncation_limit`, `truncation_reason`, `truncation_recovery_hint`). **Canonical adoption invariant (2026-05-16 sweep):** every walk-shaped tool's JSON payload routes through one of these two helpers — `with_truncation_envelope` for fresh-payload merge, `truncation_envelope` for fold-into-existing-Map use. Pinned by `envelope_construction_routes_through_one_helper_no_inline_fields` (forbids inline `"truncation_limit":` JSON keys outside the helpers' own definitions and the test module) AND by `every_walk_tool_emits_full_truncation_envelope_in_json` (residual check that any surviving inline site also carries reason + hint).
  - `tool_error(op, node_id, err)` and `tool_invalid_params(op, node_id, msg)` — structured error helpers. The first for operational failures, the second for validation failures. Both produce the same `data` envelope shape so clients see one error model. **Universal contract: every error path emits the structured envelope.** Validation failures route through `tool_invalid_params` (allow-listed only in two helpers — `check_node_id` at line ~441 and the short-hash-resolve helper at line ~1207 — where the operation name isn't statically known); operational failures route through `tool_error` with no exemptions. Pinned by `handler_body_validation_uses_structured_envelope_not_bare_invalid_params` and `operational_failures_route_through_tool_error_not_bare_internal_error` — adding a new bare `McpError::invalid_params(...)` inside a handler body, or any bare `McpError::internal_error(...)`, fails the build before it ships.
  - `workflow_error_to_mcp(operation, node_id, err)` — the single canonical translator for `WorkflowyError` returns from `crate::workflows::*` functions. Maps `InvalidInput` → `tool_invalid_params`, every other variant → `tool_error`. Every handler that delegates to a workflow MUST route the error through this helper rather than hand-writing a `match err { InvalidInput => …, _ => … }` arm — pre-2026-05-04 each handler hand-wrote the match and drifted on the operation-name string. Pinned by `workflow_error_translation_routes_through_workflow_error_to_mcp` which forbids `WorkflowyError::InvalidInput` matching anywhere in `server/mod.rs` except inside the helper's own body and the test module.

- **`src/utils/aggregation.rs`** — pure aggregation functions shared between MCP handlers and the `wflow-do` CLI: `compute_overdue`, `compute_upcoming`, `compute_recent_changes`, `filter_todos`, `compute_project_summary`, `compute_daily_review`. Take `today` / `now_ms` as parameters rather than reading the system clock. Single source of truth — both surfaces call the same function so they can't drift in semantics. **Adoption invariant (2026-05-16):** every list-shaped MCP handler MUST route through the matching aggregation helper. Pinned by `list_shaped_handlers_route_through_aggregation_helpers` which grep-audits the source so a handler that reimplements the date-window / status-filter loop inline fails the build. `compute_project_summary` and `compute_daily_review` returned typed structs (`ProjectSummary` / `DailyReview` with `#[derive(Serialize)]`) rather than `Vec<Value>` because the JSON shape is a single object, not a list — the typed shape is the contract.
- **`src/api/client.rs`** — Workflowy API client with exponential backoff retry. `get_subtree_recursive()` fetches tree level-by-level via `/nodes?parent_id=` with configurable depth limit (crucial for 250k+ node trees). Returns `SubtreeFetch { nodes, truncated, limit }`; when the `MAX_SUBTREE_NODES` cap (10 000 by default, `defaults.rs`) is hit the flag is surfaced in every tool response so callers can narrow the scope.
- **`src/defaults.rs`** — Centralized constants for all magic numbers (cache TTL, retry config, validation limits, tree depth defaults).
- **`src/types.rs`** — Core types including `NodeId` newtype with `Deref<str>`, `AsRef<str>`, `PartialEq<String>`, and `JsonSchema` impls. **`Deserialize` is hand-written** (not derived) so the host-encoded literal strings `"null"` / `"undefined"` reject up-front at the parameter boundary instead of routing as opaque IDs that the API layer fails on later. Whitespace-only strings reject too; empty string is preserved as the workspace-root sentinel some handlers (`list_children`, `insert_content`, etc.) use. Surfaced 2026-05-09 by a Claude Desktop session that observed `parent_id="null"` (string, not JSON null) silently routing to contextual destinations across three consecutive `create_node` / `create_mirror` calls.
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
   Deserialization runs through `serde_path_to_error::deserialize` so
   the error message carries the offending field path (e.g. `invalid
   parameters at \`.new_parent_id\`: invalid type: null, expected a
   string`). Without the path, a host (LLM or MCP client) sending
   `null` for a required `NodeId` saw only "invalid type: null,
   expected a string" with no field name and no way to self-correct;
   the 2026-05-03 `move_node` incident motivated the switch. Pinned
   by `null_required_uuid_field_error_names_the_field`. Failures
   route through `tool_invalid_params` so the wire error carries the
   same `{operation, node_id, hint, proximate_cause, error}` envelope
   as every other validation failure.
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
- **Name Index** (`utils/name_index.rs`): Case-insensitive `name -> [entry]` map plus short-hash → UUID maps (12-char URL-suffix and 8-char doc prefix), fed by every subtree walk. Backed by `parking_lot::RwLock`; invalidated per-node on every write. **Persisted to disk** at `$WORKFLOWY_INDEX_PATH` (unset or empty disables persistence — the index then lives only in memory; the repo ships no machine-specific default path so each user wires it through their MCP host config): rehydrated on server startup, checkpointed every 30 s when dirty via write-then-rename, refreshed by a 30-minute background walk (calibrated against 250 k-node trees so quasi-full coverage builds up over a working day rather than a working week). **Auto-walks on short-hash miss**: `resolve_node_ref` fires a workspace walk with `RESOLVE_WALK_TIMEOUT_MS` budget when a short hash isn't cached; a watcher polls the index every 100 ms and cancels the walk as soon as the target appears. Callers no longer need to run `build_name_index` manually before passing a Workflowy URL fragment.
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
| Content Modification | edit_node, move_node, reorder_nodes, delete_node, duplicate_node, create_from_template, bulk_update, bulk_tag, transaction |
| Todo Management | list_todos, complete_node |
| Due Dates & Scheduling | list_upcoming, list_overdue, daily_review |
| Project Management | get_project_summary, get_recent_changes |
| Mirror Discipline | create_mirror, audit_mirrors |
| Diagnostics & Ops | health_check, workflowy_status, cancel_all, build_name_index, get_recent_tool_calls, review, export_subtree |

### CLI Parity (`wflow-do`)

The `wflow-do` binary at `src/bin/wflow_do.rs` is in **full surface parity** with the MCP tool list above. Every non-diagnostic MCP tool has a matching CLI subcommand routed through the same `WorkflowyClient`. New MCP tools must land with their `wflow-do` subcommand in the same commit. The build-time test `cli_covers_every_non_diagnostic_mcp_tool` enumerates the (mcp-tool → cli-subcommand) pairs and fails CI if a tool is added without its CLI counterpart. `convert_markdown` (pure local transform) is intentionally excluded; `cancel_all` and `get_recent_tool_calls` ship as no-op CLI surfaces because the op log only exists in the running MCP server. (`create_mirror` was a stub through 2026-05-04; the failure-report follow-up replaced the stub with a real convention-based implementation, and the `create-mirror` CLI subcommand landed in the same commit.)

### Shared workflow orchestration (`src/workflows.rs`)

Workflow orchestration that used to be duplicated between the MCP server's `#[tool]` handlers and the `wflow-do` CLI subcommands lives in `src/workflows.rs`. Functions there take `&WorkflowyClient` plus typed inputs and a `&WorkflowContext<'_>`, return `(TypedResult, MutationFootprint)`, and are called by both surfaces — the MCP handler wraps with `tool_handler!` (cancel + op-log + deadline) and applies the footprint via `apply_footprint`; the CLI propagates errors via `?` and discards the footprint. This is the same pattern `audit::audit_mirrors`, `audit::build_review`, and `utils::aggregation::*` already follow for pure functions; `workflows.rs` extends it to functions that need an API client.

**Foundation types** (all in `src/workflows.rs`):
- `WorkflowContext { cancel: Option<&'a CancelGuard>, deadline: Option<Instant> }` — first-class cancel + deadline. MCP fills both; CLI passes `WorkflowContext::default()`.
- `MutationFootprint { invalidated_nodes, invalidated_name_index }` — workflows declare which IDs they touched; the MCP wrapper applies invalidation, the CLI ignores. Replaces the pre-2026-05-04 hand-written `invalidate_for_mutation(&[id, ...])` arguments at every handler.
- `workflow_error_to_mcp(operation, node_id, err)` (in server/mod.rs) — translates `WorkflowyError::InvalidInput` → `tool_invalid_params` and every other variant → `tool_error`. Replaces the inline `match err { InvalidInput => …, _ => … }` arms.

**Workflows currently lifted**:

_2026-05-04 batch:_
- `create_mirror_via_convention` — duplicate canonical name + write `mirror_of:` note + optional `canonical_of:` annotation on canonical.
- `insert_content_via_indented` (+ `parse_indented_content`, `InsertContentOutcome`, `PartialReason`) — 2-space-indented payload up to `MAX_INSERT_CONTENT_LINES` with cancel + deadline-aware partial success.
- `run_transaction` (+ `TxnOp`, `TxnOpKind`, `TxnInverse`, `TransactionOutcome`, internal `apply_txn_step` + `run_txn_inverse`) — sequential apply with best-effort LIFO rollback.
- `apply_bulk_op` (+ `BulkOp`, `BulkOpResult`) — apply step for `bulk_update` (delete / complete / uncomplete / add_tag / remove_tag).
- `smart_insert_under_target` — thin wrapper over `insert_content_via_indented` for the post-disambiguation insertion phase.
- `reorder_nodes_via_priority` (+ `ReorderOutcome`, `ReorderEntry`, `ReorderPartialReason`) — places a list of node IDs in a specified order under a parent. Walks the desired list in REVERSE issuing `priority=0` per move so renormalising upstream priorities can't make the batched reorder fight itself. Validates non-empty / no-duplicates / no parent-as-child / cap of `MAX_REORDER_NODES` before any API touch; returns per-id ok / error / skipped entries with `Complete` or `Partial { reason: cancelled | timeout }` envelope. Side effect: ids not currently under `parent_id` are reparented (the primitive is built on `move_node`).

_2026-05-16 refactor-review batch (Phases 1a/1b/1c):_

- `audit_mirrors_walk` (+ `AuditMirrorsWalkOutcome`) — chunked-or-single subtree walk for `audit_mirrors`, including the best-effort root-node include and dedupe-by-id pass. Pre-lift the MCP decremented `child_depth = max_depth.saturating_sub(1)` while the CLI hardcoded `7`; both surfaces now flow through the workflow's single decrement so the depth budget cannot drift. The MCP side ingests every returned node into the persistent name index AFTER the workflow returns (the workflow itself is index-agnostic, matching the existing pattern of per-surface side effects).
- `extract_unresolved_mirror_targets` — pure helper returning the set of `mirror_of:` UUIDs encountered in `nodes` that don't resolve in scope (end-match in both directions to cover short-hash forms). Both surfaces call this; each surface then resolves the unresolved set through its own data source (MCP: O(1) name index; CLI: live `get_node` calls). The resolution data source is the per-surface differentiator the lift preserves intentionally — the MCP's index doesn't store descriptions, so `has_canonical_marker` is `None` for index-resolved canonicals while the CLI sees `Some(bool)`. This is documented behaviour, not drift.

Pure aggregations that also belong in this lift catalogue (live in `src/utils/aggregation.rs`, not `workflows.rs`, because they need no client):

- `compute_project_summary` (+ `ProjectSummary` / `ProjectSummaryStats` / `ProjectSummaryRoot`) — per-subtree counts, completion percent, tag/assignee histograms, recently-modified list. Pre-lift the MCP emitted a nested `stats` object with conditional `tags`/`assignees` while the CLI flat-printed counts and stripped tag prefixes — most divergent of the three orchestrations not yet lifted at the time of the architecture review.
- `compute_daily_review` (+ `DailyReview` / `DailyReviewSummary`) — four-bucket review (overdue / due_soon / recent_changes / top_pending) on top of the per-bucket helpers. Pre-lift the CLI hardcoded `horizon=7` while the MCP parameterised `upcoming_days`; the CLI emitted `days_until` while the MCP emitted `days_until_due`. Field names converged on the MCP forms (`days_until_due`) so adoption was wire-preserving on the more-visible surface.

Before 2026-05-04 these orchestrations lived in two places (server handler body + CLI subcommand body) and silently drifted — the failure-report 2026-05-03 follow-up surfaced specific drifts (server's `smart_insert` was flat-only while CLI was indent-aware; CLI's `transaction` rollback used JSON-blob inverses while server's used a typed enum). The 2026-05-16 architecture-review batch closed the remaining three drifts (`daily_review`, `audit_mirrors`, `get_project_summary`). **Adding a new tool that has meaningful CLI/server surface duplication should grow an entry here rather than inlining the orchestration in both binaries.**

### Workflowy API Constraints

- All endpoints use `/nodes` or `/nodes/{id}`
- API base: `https://workflowy.com/api/v1`
- Auth: Bearer token via `WORKFLOWY_API_KEY`

### Known Limitations

- `bulk_update` supports `complete`, `uncomplete`, `delete`, `add_tag`, `remove_tag`. `complete`/`uncomplete` route through `client.set_completion`, the same code path the single-node `complete_node` tool uses. The wire payload is `POST /nodes/{id}` with `{"completed": true}` or `{"completed": false}` (the read-side `WorkflowyNode::completed` boolean has no serde alias, so the wire field is literally `completed`); pinned by `tests::write_field_names::set_completion_*` so the description→note failure shape cannot recur on the completion path. The `transaction` tool also accepts `complete` / `uncomplete` ops with rollback to the prior boolean state via `RestoreCompletion`.
- Subtree fetches cap at `defaults::MAX_SUBTREE_NODES` (10 000) **and** at `defaults::SUBTREE_FETCH_TIMEOUT_MS` (20 000 ms). Tools surface a `truncated` flag plus a `truncation_reason` (`node_limit` / `timeout` / `cancelled`) when either budget fires; `duplicate_node`, `create_from_template`, and `bulk_update` (delete) refuse to run against a truncated view.
- `find_node` and `search_nodes` refuse to scan from the workspace root when `parent_id` is omitted. Pass `parent_id`, set `allow_root_scan=true` to accept the full walk, or set `use_index=true` after running `build_name_index` to serve from the opportunistic name index. The 2026-05-03 eval run revealed every search scoped under Distillations was timing out at the 20 s walk budget once the subtree grew past the 10 000-node walk cap. `use_index=true` answers in O(1) from the persistent name index without burning any walk budget; the trade-off is name-only match (description content still needs the live walk). The truncation banner emitted on timeout / node-cap responses now names this recovery path explicitly so callers don't have to read the docs.
- **JSON-truncation envelope consistency.** Every walk-shaped tool that emits JSON includes the same four fields next to its `"truncation_limit"`: `truncated`, `truncation_limit`, `truncation_reason` (`timeout` / `node_limit` / `cancelled` / null), and `truncation_recovery_hint` (the empty string when not truncated; otherwise the same `TRUNCATION_RECOVERY_HINT` string the markdown banner emits). **Construction must route through one of the two canonical helpers** — `with_truncation_envelope(payload, truncated, limit, reason)` for fresh-payload merge, or `obj.extend(truncation_envelope(truncated, limit, reason))` for fold-into-existing-Map use after `serde_json::to_value(&typed)`. Pinned by two tests: (a) `envelope_construction_routes_through_one_helper_no_inline_fields` (2026-05-16) forbids any inline `"truncation_limit":` JSON key outside the helpers' own definitions and the test module — the contract becomes enforceable by `cargo build`; (b) `every_walk_tool_emits_full_truncation_envelope_in_json` (residual) verifies any surviving inline site carries reason + hint. The pre-2026-05-16 codebase had ~13 inline emit sites; the sweep collapsed them all to helper calls.
- `insert_content` caps at `defaults::MAX_INSERT_CONTENT_LINES` (80) per call. The cap was lowered from 200 on 2026-05-04 in response to the failure-report 2026-05-03 session, which observed ≥80-line payloads failing at the MCP transport layer with no diagnostic. Above the cap the call returns a typed error with a chunking instruction; chunk to ≤80 lines and pass the previous batch's `last_inserted_id` as the next call's `parent_id` to keep the hierarchy stitched together. The previous "soft warn at 80, hard cap at 200" two-tier model collapsed into a single boundary since no observed caller could reliably get above 80 anyway. `parent_id` accepts `null` (or omission) as workspace root, matching `create_node` / `batch_create_nodes` / `list_children` — pre-2026-05-04 it rejected null at the schema layer ("invalid type: null, expected a string"), the asymmetry the failure-report flagged as the single biggest cause of session friction.
- `edit_node` requires at least one of `name` or `description`; an empty patch is rejected at the handler boundary rather than POSTed as `{}`. The wire field for the description body is `note` (Workflowy's API name); `client.rs` maps `description` → `note` at the boundary on writes, and serde's `alias = "note"` covers reads. Sending `description` literally returns 200 OK with the field silently dropped — that was the 2026-05-02 P2.4 field-loss symptom, and the regression is now pinned by `tests::write_field_names` in `src/api/client.rs`.
- `move_node` invalidates the cache for the node, the new parent, **and** the old parent (captured via a pre-read). The name index is invalidated for the moved node. The 2026-05-04 unification removed the previous `move_node_with_propagation_retry` wrapper and inlined the 404 propagation-retry loop into `client.move_node` itself, so every move caller (the bare tool handler, `transaction.move`, the `wflow-do move` CLI) hits the same code path. The failure-report 2026-05-03 session observed an 11 % vs 100 % success-rate divergence between the bare tool and `transaction.move`, traced to that wrapper-vs-bare split. Pinned by `move_node_embeds_propagation_retry_loop`.
- `NodeId` rejects the literal four-char string `"null"` (and `"undefined"`) at the deserialiser, with a path-aware error naming the offending field. Surfaced 2026-05-09 by a Claude Desktop session that observed `parent_id="null"` (string, not JSON-null) silently routing to contextually-derived destinations across three consecutive `create_node` / `create_mirror` calls — the host serialiser was emitting `"null"` for what should have been an explicit UUID, and the server was accepting it. The 2026-05-10 fix added a hand-written `Deserialize` on `NodeId` (in `src/types.rs`) that refuses the literal up-front so the failure is observable at the wire and the host can self-correct on retry. Whitespace-only strings are also rejected; empty string is preserved (some handlers special-case `""` as the workspace-root sentinel — `list_children`'s `None | Some("")` pattern). Pinned by `tests::node_id_rejects_literal_null_string`, `tests::node_id_rejects_literal_undefined_string`, `tests::node_id_rejects_whitespace_only`, and the path-aware `server::tests::literal_null_string_in_required_uuid_field_error_names_the_field` companion to the existing JSON-null test.
- `workflowy_status.name_index` carries a `persistence` envelope with `configured`, `path`, `env_var`, and `warning` fields. When `WORKFLOWY_INDEX_PATH` is unset (or set only in `.zshrc`, which the MCP server process does not see), `configured: false`, `path: null`, and `warning` carries a one-line message naming the env var and pointing at the recommended host-config fix (`claude_desktop_config.json mcpServers.workflowy.env.WORKFLOWY_INDEX_PATH`). Surfaced 2026-05-09: the dual-config gap meant the persistent index could be silently disabled with the agent paying cold-start latency on every short-hash resolve. Pinned by `server::tests::name_index_persistence_envelope_warns_when_env_unset` and `server::tests::name_index_persistence_envelope_reports_path_when_env_set`.
- `reorder_nodes` orders a list of node IDs under a given parent by walking the desired list in REVERSE and issuing `move_node` with `priority=0` per id. Workflowy's priority semantics on `move_node` are _position-relative-to-siblings_ and renormalise after every call, so a naive forward `priority=0,1,2,…` loop fights itself; the reverse-priority-0 trick lands the desired sequence at the head of the parent's children regardless of how many other siblings the parent already has. Side effect: ids not currently under `parent_id` are reparented as part of the reorder (the primitive is built on `move_node`). Capped at `defaults::MAX_REORDER_NODES` (200) per call. Validation rejects empty lists, duplicates, and `parent_id` appearing in `node_ids` before any API touch. Returns a typed `Complete | Partial { reason: cancelled | timeout }` envelope with per-id `ok / error / skipped` entries; partial outcomes are safe to retry by re-issuing the full list (each move is idempotent in the reverse-priority-0 model).
- `cancel_all` bumps the cancel-registry generation: every outstanding tree walk returns partial results on its next checkpoint with `truncation_reason = "cancelled"`. New calls start fresh.
- `health_check` calls `GET /nodes` (top-level only) with a 5-second budget; it is safe to use as a liveness probe regardless of tree size.
- `list_children` accepts both `node_id` and `parent_id` for the parent-to-list-children-of, since every other tool in this server that scopes to a parent uses `parent_id`. Both names reach the handler with the same semantics. Any third name fails the `deny_unknown_fields` check with a typed `unknown field` error.
- `degraded` self-clears on recovery: once the failing tool returns OK, `workflowy_status.last_failure` and the `DEGRADED` warning that gates `create_node` both clear together via `OpLog::last_unrecovered_failure`. A success on a _different_ tool does NOT clear another tool's failure warning.
- `create_mirror` implements a documented `mirror_of:` / `canonical_of:` note convention, NOT native Workflowy mirrors (the public REST API does not expose mirror creation). Edits to the canonical do **not** propagate to the mirror — the link is structural and human-curated, audited via `audit_mirrors`. Optional `pillar` parameter writes a `canonical_of: <pillar>` marker to the canonical when it lacks one; existing markers are never overwritten. Mirror-of-self is rejected up-front. The orchestration lives in `crate::workflows::create_mirror_via_convention` and is shared with the `wflow-do create-mirror` CLI.
- `audit_mirrors` separates the **walk scope** (which subtree is traversed to find mirrors) from the **canonical-resolution scope** (where `mirror_of:` UUIDs are looked up to decide whether they resolve). Cross-pillar mirroring is the standard Mirror Discipline pattern, so a mirror's canonical living in a different pillar is the normal case rather than a fault. Two behaviours fall out of that split:
  - **Chunked walk** (default when `root_id` is omitted): the handler lists the root's direct children and walks each as its own subtree with `defaults::SUBTREE_FETCH_TIMEOUT_MS` budget, then unions the node sets. The Distillations subtree exceeds the 10 000-node walk cap, so a single unchunked walk against the default root times out at the root and reports zero findings. Chunking sidesteps the cap because each pillar fits comfortably. Pass `chunked=false` to opt out (e.g. when scoping to a small subtree where the cap is irrelevant); pass `chunked=true` with an explicit `root_id` to force chunking under any scope. Per-chunk envelope (`id`, `name`, `scanned`, `truncated`, `truncation_reason`) is surfaced under `chunks` so callers can see which pillar timed out.
  - **Widened canonical resolution** (default `cross_scope_resolve=true`): for every `mirror_of:` UUID encountered in scope that the walk itself didn't resolve, the MCP handler consults the persistent name index via `NameIndex::lookup_entry_by_id` (O(1), ~55k entries in production). When the canonical is found there, it is added to the `external_canonicals` map passed to `audit::audit_mirrors_with_external` and the mirror classifies OK (or DRIFTED if names diverge). Pre-2026-05-16 the audit conflated the two scopes and emitted false-positive BROKEN findings for every legitimate cross-pillar mirror — the symptom the 2026-05-16 weekly-synthesis report named. The CLI (`wflow-do audit-mirrors`) implements the same widening by issuing one `get_node` per unresolved UUID (rate-limited via the client). Pass `cross_scope_resolve=false` to restore the legacy in-scope-only classifier. The ORPHAN check is skipped for external canonicals when the resolver path doesn't carry the canonical's description (the name index doesn't store descriptions, so the handler treats marker-presence as unknown rather than absent — only `has_canonical_marker=Some(false)` triggers ORPHAN, never `None`).

## Testing

Unit tests use `#[cfg(test)]` modules alongside source. No live API calls in unit tests. Integration test in `tests/live_insert.rs` requires `WORKFLOWY_API_KEY`.

```bash
cargo test --lib                           # unit tests only (314 tests)
cargo test --lib -- test_name              # run specific test
cargo test --lib -- --nocapture            # with stdout
```

## Configuration

Environment variables loaded from `.env` via dotenvy (`src/config.rs`):
- `WORKFLOWY_API_KEY` (required)
- `WORKFLOWY_INDEX_PATH` (optional) — disk path for the persistent name index. Unset or empty disables persistence. The repository ships no machine-specific default; each user (or MCP host config) wires the path explicitly.
- `SECONDBRAIN_DIR` (optional) — root of the user's operational `secondBrain` directory (drafts, session logs, briefs, memory). The `review` tool's bucket-d session-log scan and the `wflow-do index` default output path read from `$SECONDBRAIN_DIR/session-logs/`. Unset or empty disables those features (graceful skip).

## Templates and setup

The repo ships generic templates so a fresh user (or LLM agent bootstrapping one) can stand up the same workflow without inheriting the original author's specific node IDs:

- `docs/SETUP.md` — step-by-step guide for an LLM to install the MCP and provision the user's secondBrain directory.
- `templates/skills/wflow/SKILL.md` — generic copy of the wflow skill with no user-specific node IDs.
- `templates/secondbrain/` — skeleton of the operational secondBrain directory (README, memory schema, drafts/session-logs/briefs subdirs).

User-specific data (cached node IDs, drafts, session logs, briefs) lives in whatever path the user sets via `$SECONDBRAIN_DIR` — never in this repo. The repo carries no opinion about where on disk that lives.

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
