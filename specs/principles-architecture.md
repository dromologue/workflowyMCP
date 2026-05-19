# Architecture Principles

> Foundational principles guiding architectural decisions for the Workflowy MCP Server.

## Guiding Philosophy

Architecture serves the user, not the architect. Every structural decision must trace back to a clear benefit for developers integrating this server or end users interacting through Claude.

---

## Core Principles

### 1. Separation of Concerns

Each module has one reason to exist and one reason to change.

- **Transport layer**: Handles MCP protocol communication only
- **Business logic**: Implements Workflowy operations independent of transport
- **API integration**: Manages Workflowy API interactions in isolation
- **Configuration**: Centralized, not scattered across modules

### 2. Dependency Inversion

High-level modules must not depend on low-level modules. Both should depend on abstractions.

- Define interfaces for external services (Workflowy API, caching)
- Inject dependencies rather than constructing them internally
- Enable testing through mock implementations
- Avoid coupling business logic to specific implementations

### 3. Single Source of Truth

Every piece of state should have exactly one authoritative location.

- Configuration lives in environment variables and config files only
- Cached data has clear ownership and invalidation rules
- No duplicate state that can diverge
- Workflowy is the source of truth; local state is ephemeral cache

### 4. Fail Fast, Recover Gracefully

Detect problems early but handle them without data loss.

- Validate inputs at system boundaries immediately
- Use typed errors that carry context for debugging
- Implement circuit breakers for external service calls
- Queue operations that can be retried safely

### 5. Minimal Surface Area

Expose only what users need; hide implementation details.

- Public API is the MCP tool interface—nothing more
- Internal modules use private exports by default
- Avoid leaky abstractions that expose Workflowy API quirks
- One way to accomplish each task from the user perspective

### 6. Stateless Where Possible

Minimize shared mutable state to reduce complexity.

- Tools should be idempotent when reasonable
- Cache is an optimization, not a requirement for correctness
- Session state belongs to the MCP client, not the server
- Design for horizontal scaling even if currently single-instance

### 7. Simplicity

The simplest design that satisfies the contract is the right one. Mechanisms compound: a clever wrapper here, a special case there, and the call graph becomes something only the original author can reason about. Reach for the boring solution first, and let extra structure earn its place by solving a real, current problem rather than an imagined future one.

- One mechanism per concern. If two safety nets cover the same failure mode, delete one.
- Prefer one shared abstraction over many bespoke ones — but never paper over a real difference with a leaky generic.
- Inline the obvious. A helper used in one place is not a helper.
- Trust internal invariants. Validate at system boundaries (user input, external APIs); don't re-validate at every call site.
- Delete defensive code that protects against scenarios that cannot happen in this codebase.
- A failure that costs five minutes to diagnose is not paid back by a thousand lines of preventive structure.

### 8. Consistency

Tools, modules, and call sites that are doing the same kind of thing must look the same. A new contributor (human or AI) should be able to read one handler and predict the shape of every other handler in its category. Inconsistency is the dominant source of latent bugs in this codebase: the 2026-05-02 4-minute write hang traced directly to one class of handler (single-node writes) skipping the safety-net wrapper that every other class used.

- Every tool handler runs through the same `tool_handler!` wrapper, classified by `ToolKind`. Diagnostics are the documented exception and own their own short budgets.
- Every wire-level field name maps to its Rust counterpart at exactly one boundary (the `client.rs` call site for writes, the serde `alias` for reads).
- Every cancellation-aware operation observes the same `CancelRegistry`. New operations that take time must thread a `CancelGuard`; they do not invent their own cancellation primitive.
- Every truncated subtree fetch surfaces the same `truncated` + `truncation_reason` + `truncated_at_node_id` triple. New tools that surface truncation reuse the helper, they don't roll their own banner.
- Every non-trivial error goes through `tool_error(operation, node_id, err)`. New error sites do not return bare `McpError` strings.
- **Every non-diagnostic MCP tool has a matching `wflow-do` CLI subcommand** routed through the same `WorkflowyClient`. The `wflow` skill's failure protocol falls back to the CLI whenever the MCP transport drops; if the CLI is missing a command the skill expects, the fallback path silently degrades and the user is forced to hand-edit in the Workflowy UI. New MCP tools must land with their CLI subcommand in the same commit. Pinned by `cli_covers_every_non_diagnostic_mcp_tool` in `src/bin/wflow_do.rs` — the test enumerates the (mcp-tool → cli-subcommand) pairs and fails the build if any tool ships without its CLI counterpart. `convert_markdown` (pure local transform) and `create_mirror` (stub) are intentionally excluded; `cancel_all` and `get_recent_tool_calls` ship as no-op CLI surfaces because the op log is in-process to the running MCP server.
- **Every parameter-bearing tool publishes a non-empty `properties` schema.** The `rmcp-macros 0.16` `#[tool]` proc macro auto-discovers the parameter type by matching the literal identifier `Parameters` on the last path segment of the function-arg type. The codebase's wrapper struct is therefore named `Parameters<T>` (not `TracedParams` or any synonym) — renaming it away from `Parameters` would re-introduce the 2026-05-03 silent-empty-schema failure where the cowork client validated against `{"properties": {}, "type": "object"}` and stripped every argument before they reached the server. Pinned by `parameter_bearing_tools_publish_non_empty_input_schema_properties` in `src/server.rs::tests`, which iterates every registered tool and asserts a non-empty `properties` block plus a non-empty `required` block on representative parameter-bearing tools. Same discipline as the wire-mapping rule: the schema is the contract with the client; if the contract is wrong, the call silently misroutes.
- **Every walk-shaped tool emits the same JSON-truncation envelope.** When a walk truncates (timeout, node-cap, or cancel), the JSON response includes the same four fields next to its `"truncation_limit"`: `truncated: bool`, `truncation_limit: usize`, `truncation_reason: "timeout"|"node_limit"|"cancelled"|null`, `truncation_recovery_hint: string` (the empty string when not truncated; otherwise [`TRUNCATION_RECOVERY_HINT`] naming `build_name_index` + `use_index` as the bypass). Pre-2026-05-03 the JSON tools emitted `truncation_limit` only — no reason, no hint — so a JSON caller hitting the 20 s walk budget on a big subtree had no actionable information. The fields are inlined at every site rather than spread from a helper so the audit is grep-able and the existing `json!({...})` literals stay readable. Pinned by `every_walk_tool_emits_full_truncation_envelope_in_json` in `src/server.rs::tests`, which scans the source and rejects any `"truncation_limit":` site whose surrounding json! block is missing the companion fields. Adding a new walk-shaped tool that emits a truncation field without the envelope companions fails the build.
- **`use_index` is the consistent fast path for name-based queries.** `find_node` and `search_nodes` both expose `use_index=true` to serve queries from the persistent name index in O(1) without burning the 20 s walk budget. Tools whose query criterion can't be answered from a name index (`tag_search` needs tags, `list_overdue` / `list_upcoming` / `daily_review` need due-date parsing on descriptions, `find_backlinks` needs description-content matching, `find_by_tag_and_path` needs tags) do not have `use_index` because the index doesn't track those fields — extending the index to tags / dates / descriptions is a larger project tracked separately. The truncation `recovery_hint` consistently names the `use_index` path even from tools that don't expose it, because the caller's recovery move is the same: re-issue the query against `find_node` or `search_nodes` with `use_index=true` for the name component, then narrow the live-walk part.

When two handlers diverge in pattern, the divergence is either a bug or a load-bearing design choice that earns a comment on the spot — naming the reason the standard pattern doesn't fit. The default is to converge.

---

## Structural Constraints

### Module Boundaries

```
┌─────────────────────────────────────────────────┐
│                   MCP Transport                  │
│              (stdio, protocol handling)          │
└─────────────────┬───────────────────────────────┘
                  │
┌─────────────────▼───────────────────────────────┐
│                  Tool Handlers                   │
│         (request validation, response format)    │
└─────────────────┬───────────────────────────────┘
                  │
┌─────────────────▼───────────────────────────────┐
│                 Business Logic                   │
│     (workflows, orchestration, transformations)  │
└─────────────────┬───────────────────────────────┘
                  │
┌─────────────────▼───────────────────────────────┐
│               Workflowy Client                   │
│          (API calls, caching, retry logic)       │
└─────────────────────────────────────────────────┘
```

### Data Flow Rules

1. Data flows downward through the stack
2. Errors propagate upward with context attached
3. No layer may skip levels (tools cannot call Workflowy directly)
4. Cross-cutting concerns (logging, metrics) use middleware patterns

---

## Decision Framework

When making architectural choices, evaluate in this order:

1. **Correctness**: Does it work reliably for all valid inputs?
2. **Simplicity**: Is this the simplest solution that could work?
3. **Maintainability**: Can another developer understand and modify this?
4. **Performance**: Does it meet the response time targets?
5. **Extensibility**: Can we add related features without restructuring?

Optimize for the order listed. Never sacrifice correctness for performance. Prefer simplicity over extensibility until extensibility is proven necessary.

---

## Rust Idioms (Applied)

The following Rust patterns are actively enforced in this codebase:

### Newtype Pattern
- `NodeId` wraps `String` for type-safe node ID handling across the API boundary
- Prevents mixing node IDs with arbitrary strings at compile time
- Implements `Deref<Target=str>`, `AsRef<str>`, `Display`, `From`, `PartialEq<String>`

### Dependency Injection over Global State
- `NodeCache` is injected into `WorkflowyMcpServer` via `with_cache()` constructor
- Global `lazy_static` cache remains as convenience default but is not required
- Enables testing with isolated cache instances

### Centralized Constants
- All magic numbers live in `src/defaults.rs` (single source of truth)
- Config structs reference `defaults::*` in their `Default` impls
- Validation constants re-export from defaults for backward compatibility

### Proper Error Propagation
- No sentinel values (`unwrap_or("unknown")`) — use `Result` and `?`
- `WorkflowyClient::new()` returns `Result`, not panicking `.expect()`
- Helper constructors: `WorkflowyError::internal()`, `WorkflowyError::parse()`

### Type Alias for Complex Types
- `BoxFuture<'a, T>` alias simplifies recursive async function signatures

### Cancellation Propagation Contract
- Long-running tree walks are cooperatively cancellable via the shared
  `CancelRegistry` (a generation counter; see `utils/cancel.rs`).
- Cancellation must be observable inside *every* awaitable inside the walk:
  the rate-limiter wait (`acquire_cancellable`), the in-flight HTTP send
  (raced via `tokio::select!` in `try_request_cancellable`), and the
  inter-attempt backoff sleep (`sleep_cancellable`).
- Adding a new long-running operation to the request pipeline requires
  threading a `CancelGuard` through it. Skipping this regresses the
  reliability invariant that `cancel_all` frees the shared `RateLimiter`
  within ~50 ms.

### Truncation Locatability
- Every partial subtree fetch carries `truncated_at_node_id` naming the
  parent whose subtree was cut short. Banner helpers
  (`truncation_banner_from_fetch`) resolve that against the fetched
  nodes to display a hierarchical path. New tools that surface
  truncation must reuse this helper rather than rolling their own
  message — divergent banners erode the caller's ability to re-scope.
- Every walk-shaped tool that emits JSON spreads the four-field
  envelope (`truncated`, `truncation_limit`, `truncation_reason`,
  `truncation_recovery_hint`) into its payload. The helper
  `truncation_envelope(truncated, limit, reason)` produces the map;
  `with_truncation_envelope(payload, ...)` merges it into a `json!({...})`
  literal. Pinned by `every_walk_tool_emits_full_truncation_envelope_in_json`.

### Pre-call cache invalidation for mutations
- Every write handler invalidates cache + name-index entries for the
  affected nodes BEFORE calling the API, not in the success branch.
  The `invalidate_for_mutation(&[id, ...])` helper centralises this.
  Reason: `tool_handler!` enforces a wall-clock budget; if its timeout
  arm fires the inner future is dropped, and any post-API invalidation
  code never runs. The mutation may have already landed at the API,
  leaving the cache stale. Pre-call invalidation makes the contract
  robust to timeout, cancel, and panic at the cost of one redundant
  API read on a failed mutation. Surfaced by the 2026-05-03
  architecture review.

### Validation errors carry the operation context
- Validation failures route through `tool_invalid_params(operation, node_id, msg)`
  rather than bare `McpError::invalid_params(msg, None)` — same `data`
  envelope shape as `tool_error` (`operation`, `node_id`, `hint`,
  `proximate_cause: "invalid_params"`, `error`). Bare
  `McpError::invalid_params` is reserved for failures that genuinely
  don't know which operation produced them (parsing-stage, framework-
  level). The 2026-05-03 architecture review surfaced 40+ direct
  `McpError::invalid_params` sites that lost the operation context;
  migration is incremental — the helper exists, sample sites have
  been migrated, and the rule applies to all new validation errors.

### One source of truth for cross-surface aggregation
- Aggregation logic shared between MCP handlers and the `wflow-do` CLI
  lives in `src/utils/aggregation.rs` as pure functions taking
  `&[WorkflowyNode]` and producing either `Vec<serde_json::Value>`
  (list shapes) or typed `Serialize` structs (single-object shapes).
  Today: `compute_overdue`, `compute_upcoming`,
  `compute_recent_changes`, `filter_todos`, `compute_project_summary`
  (→ `ProjectSummary` / `ProjectSummaryStats` / `ProjectSummaryRoot`),
  `compute_daily_review` (→ `DailyReview` / `DailyReviewSummary`).
  Pre-2026-05-03 each surface implemented these independently; the
  CLI parity build-time test catches surface drift but not semantic
  divergence between two parallel implementations. Routing both
  surfaces through one helper makes them converge by construction;
  the helpers take their `today` / `now_ms` as parameters rather
  than reading the system clock so they stay pure and tests can pin
  behaviour against arbitrary timestamps.

  **Adoption invariant** (2026-05-16). Every list-shaped MCP handler
  MUST route through the matching aggregation helper. Pinned by
  `list_shaped_handlers_route_through_aggregation_helpers` which
  grep-audits the source so a future handler that reimplements the
  date-window / status-filter loop inline fails the build. The
  six handlers covered are `list_overdue` → `compute_overdue`,
  `list_upcoming` → `compute_upcoming`, `get_recent_changes` →
  `compute_recent_changes`, `list_todos` → `filter_todos`,
  `daily_review` → `compute_daily_review`,
  `get_project_summary` → `compute_project_summary`.

### One source of truth for cross-surface JSON envelopes

- The four-field truncation envelope (`truncated`, `truncation_limit`,
  `truncation_reason`, `truncation_recovery_hint`) is constructed
  through one of two canonical helpers in `src/server/mod.rs`:
  `with_truncation_envelope(payload, truncated, limit, reason)` for
  fresh-payload merge, or `obj.extend(truncation_envelope(...))`
  for fold-into-existing-Map use after `serde_json::to_value(&typed)`.
  Pre-2026-05-16 the codebase carried ~13 inline emit sites that
  re-wrote the four-field block; a 2026-05-16 sweep collapsed them.
  **Construction invariant**: pinned by
  `envelope_construction_routes_through_one_helper_no_inline_fields`
  which forbids any inline `"truncation_limit":` JSON key outside
  the helpers' own definitions and the test module. The contract
  is enforceable by `cargo build` rather than by a source-grep audit.

### One canonical translator for workflow errors

- Every `WorkflowyError` returned from a `crate::workflows::*` call
  is translated to `McpError` via `workflow_error_to_mcp(operation,
  node_id, err)` — the helper maps `InvalidInput` →
  `tool_invalid_params`, every other variant → `tool_error`. Pre-
  2026-05-04 each handler that delegated to a workflow hand-wrote
  the `match err { InvalidInput => …, _ => … }` arms and got the
  operation name slightly wrong each time. **Translation invariant**:
  pinned by `workflow_error_translation_routes_through_workflow_error_to_mcp`
  which forbids `WorkflowyError::InvalidInput` matching anywhere
  in `server/mod.rs` except inside the helper's own body and the
  test module.

### Workflowy link → short-hash extraction (`src/utils/link_parser.rs`)

Every URL/short-hash extraction routes through one helper:
`extract_workflowy_short_hash(input: &str) -> Option<String>` in
`src/utils/link_parser.rs`. Both consumers — the MCP `resolve_link`
handler and the `wflow-do resolve-link` CLI subcommand — call the
helper directly; neither hand-rolls the parse. Pre-2026-05-19 the
two surfaces each had their own inline parser, both of which used the
same anti-pattern: `last_segment.chars().filter(|c|
c.is_ascii_hexdigit()).collect()` over the entire URL string. The
filter drops every non-hex character, including the URL's structural
separators (`?`, `&`, `=`, `/`), and so concatenates every hex
character anywhere in the URL into one "candidate hash" — which
silently corrupts on URLs carrying `?focusedItem=<hash>` query
parameters (Workflowy's "copy link to this bullet, focused under
that parent" form, where the inner-most target lives in the query
string and not the fragment). The user-report on 2026-05-19 named
this as the assistant "having trouble resolving internal links";
the symptom is a confidently-wrong hash rather than a typed error.

The helper handles every observed URL form in priority order:

1. `?focusedItem=<hash>` query parameter (wins over the path
   fragment — it identifies the inner-most target).
2. `/#/<hash>` URL fragment (address-bar form).
3. `/s/<slug>/<hash>` shared-URL trailing segment.
4. Bare 32-char UUID (hyphenated or not).
5. Bare 12-char URL-suffix short hash.
6. Bare 8-char doc-form prefix short hash.

Anything else returns `None` so the caller raises a typed
invalid-params error rather than invent a hash. **Routing
invariant**: pinned by
`link_parsing_routes_through_extract_workflowy_short_hash` in
`server/mod.rs::tests`, which grep-audits both `server/mod.rs` and
`bin/wflow_do.rs` for the forbidden char-level hex-filter pattern
applied to URL input. The single helper definition (and a
legitimate `.all(|c| c.is_ascii_hexdigit())` validator inside
`is_short_hash`) are exempt; everywhere else the pattern fails
the build.

### Workflow orchestration shared between MCP and CLI (`src/workflows.rs`)

Workflows that need an API client AND are surfaced by both binaries
live in `src/workflows.rs`. The 2026-05-04 lift extracted
`create_mirror_via_convention`, `insert_content_via_indented`,
`run_transaction`, `apply_bulk_op`, and `smart_insert_under_target`
into this module after the failure-report 2026-05-03 follow-up flagged
the duplication ("why do we have two code bases for the CLI and the
Server"). `audit::*` and `utils::aggregation::*` cover the pure-
function half of the same idea; `workflows::*` covers the half that
takes a client.

Every workflow function obeys the same contract:

1. **Inputs**: `&WorkflowyClient` + typed inputs (resolved IDs preferred)
   + `&WorkflowContext<'_>`.
2. **Output**: `Ok((TypedResult, MutationFootprint))` on success,
   `Err(WorkflowyError)` on failure. `WorkflowyError::InvalidInput`
   is reserved for caller-supplied parameter problems; the MCP wrapper
   translates it to `tool_invalid_params`, the CLI prints the message.
3. **Side effects declared, not applied**. The workflow returns a
   `MutationFootprint` listing which node IDs need cache + name-index
   invalidation; the MCP wrapper applies them via `apply_footprint`,
   the CLI discards them. Pre-lift each handler hand-wrote
   `invalidate_for_mutation(&[id, ...])` arguments and one slip
   produced silent cache-staleness; the declarative footprint makes
   missed invalidations a workflow bug instead.
4. **Cancel + deadline come from the context**. `WorkflowContext`
   carries `Option<&CancelGuard>` and `Option<Instant>` deadline. The
   MCP passes its `cancel_registry.guard()` and the `ToolKind` budget;
   the CLI passes `WorkflowContext::default()` (None, None). Workflows
   that observe both signals between iterations
   (`insert_content_via_indented`) gain partial-success behaviour for
   free in both surfaces.
5. **Wrappers translate errors uniformly**. The MCP handler calls
   `workflow_error_to_mcp(operation, node_id, err)` once instead of
   matching the WorkflowyError variants inline; the CLI propagates
   via `?` and stringifies. Adding a new workflow means writing a
   one-line translator call, not a repeat of the match arms.

The pattern delivers two properties:

- **One bug fix lands in both surfaces.** The 2026-05-04 transaction
  lift collapsed `apply_txn_op` (server) and `apply_txn_step` (CLI)
  into one `run_transaction`; pre-lift drift between the two
  rollback shapes is gone by construction.
- **Test depth lives once.** Workflow tests cover the orchestration
  semantics (validation, cap enforcement, cancel/deadline behaviour,
  rollback, partial-success); the MCP and CLI keep thin smoke tests
  pinning their respective wrapping (envelope shapes, exit codes).

When the orchestration genuinely diverges between MCP and CLI (e.g.
walks that need cancel-aware budgets only the MCP carries), keep the
divergent step on each surface and lift only the step that doesn't
diverge. The lift goal is "no duplicate logic", not "one function for
everything".

---

## Anti-Patterns to Avoid

- **God objects**: No single module should know about everything
- **Circular dependencies**: Indicates unclear boundaries
- **Shotgun surgery**: Changes requiring edits across many files
- **Premature abstraction**: Don't add extension points until needed
- **Configuration sprawl**: All config in one place, not scattered
- **Sentinel values**: Never return fake data on error; propagate errors
- **Panic in library code**: Use `Result` instead of `.expect()` / `.unwrap()`

---

## See Also

- [Constitution](./constitution.md) - Core principles and mission
- [Development Principles](./principles-development.md) - Code-level guidance
- [Security Principles](./principles-security.md) - Security requirements
- [Implementation Plan](./implementation-plan.md) - Technical approach
