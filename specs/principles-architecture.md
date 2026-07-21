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
- **Every non-diagnostic MCP tool has a matching `wflow-do` CLI subcommand** routed through the same `WorkflowyClient`. The `wflow` skill's failure protocol falls back to the CLI whenever the MCP transport drops; if the CLI is missing a command the skill expects, the fallback path silently degrades and the user is forced to hand-edit in the Workflowy UI. New MCP tools must land with their CLI subcommand in the same commit. Pinned by `cli_covers_every_non_diagnostic_mcp_tool` in `src/bin/wflow_do.rs` — the test enumerates the (mcp-tool → cli-subcommand) pairs and fails the build if any tool ships without its CLI counterpart. `convert_markdown` (pure local transform, no API call) is the only intentional exclusion; `create_mirror` was a stub through 2026-05-04 but has shipped a real `create-mirror` CLI subcommand since the failure-report follow-up replaced it — it is a covered pair (`("create_mirror", "create-mirror")`), not an exclusion. `cancel_all` and `get_recent_tool_calls` ship as no-op CLI surfaces because the op log is in-process to the running MCP server. The non-diagnostic tool set is single-sourced as `defaults::NON_DIAGNOSTIC_MCP_TOOLS`; the CLI parity test asserts every entry (bar `convert_markdown`) has a pair, so the catalogue and the pair list cannot drift.
- **The public wflow skill template's `allowed-tools` frontmatter covers every non-diagnostic MCP tool.** The skill can only call tools its `allowed-tools` lists (the claude.ai upload enforces this), so a new MCP tool that ships without being added to the frontmatter is silently uncallable from the skill. Pinned by `skill_allowed_tools_covers_every_non_diagnostic_mcp_tool` in `tests/skill_allowed_tools.rs`, which parses `templates/skills/wflow/SKILL.md` frontmatter and asserts a `mcp__workflowy__<tool>` entry for each `defaults::NON_DIAGNOSTIC_MCP_TOOLS` member. Scope: only the in-repo public template is pinnable; the personal skill copy (outside the repo) is the user's to keep in sync.
- **Every parameter-bearing tool publishes a non-empty `properties` schema.** The `rmcp-macros 2.2` `#[tool]` proc macro auto-discovers the parameter type by matching the literal identifier `Parameters` on the last path segment of the function-arg type. The codebase's wrapper struct is therefore named `Parameters<T>` (not `TracedParams` or any synonym) — renaming it away from `Parameters` would re-introduce the 2026-05-03 silent-empty-schema failure where the cowork client validated against `{"properties": {}, "type": "object"}` and stripped every argument before they reached the server. Pinned by `parameter_bearing_tools_publish_non_empty_input_schema_properties` in `src/server.rs::tests`, which iterates every registered tool and asserts a non-empty `properties` block plus a non-empty `required` block on representative parameter-bearing tools. Same discipline as the wire-mapping rule: the schema is the contract with the client; if the contract is wrong, the call silently misroutes.
- **Every walk-shaped tool emits the same JSON-truncation envelope.** When a walk truncates (timeout, node-cap, or cancel), the JSON response includes the same four fields next to its `"truncation_limit"`: `truncated: bool`, `truncation_limit: usize`, `truncation_reason: "timeout"|"node_limit"|"cancelled"|null`, `truncation_recovery_hint: string` (the empty string when not truncated; otherwise [`TRUNCATION_RECOVERY_HINT`] naming `build_name_index` + `use_index` as the bypass). Pre-2026-05-03 the JSON tools emitted `truncation_limit` only — no reason, no hint — so a JSON caller hitting the 20 s walk budget on a big subtree had no actionable information. The fields are inlined at every site rather than spread from a helper so the audit is grep-able and the existing `json!({...})` literals stay readable. Pinned by `every_walk_tool_emits_full_truncation_envelope_in_json` in `src/server.rs::tests`, which scans the source and rejects any `"truncation_limit":` site whose surrounding json! block is missing the companion fields. Adding a new walk-shaped tool that emits a truncation field without the envelope companions fails the build.
- **`use_index` is the consistent fast path for name-and-description queries.** `find_node` and `search_nodes` both expose `use_index=true` to serve queries from the persistent name index in O(1) without burning the 20 s walk budget. As of 2026-07-12 the index also stores each node's `description`, and `search_nodes(use_index=true)` does a **token-AND match over name + description** via `NameIndex::search_tokens` (every whitespace-delimited token must appear, in any order) — closing the two field-report gaps that the former single-substring name-only `lookup` left (description content invisible; multi-token queries missing on broken contiguity). `find_node`'s `use_index` path is deliberately left on the name-only `lookup` so its `exact`/`starts_with`/`contains` semantics stay byte-compatible. Tools whose query criterion still can't be answered from the index (`tag_search` / `find_by_tag_and_path` need tags; `list_overdue` / `list_upcoming` / `daily_review` need due-date parsing; `find_backlinks` needs link-structure matching) do not expose `use_index` because the index doesn't track those fields — extending it to tags / dates is a larger project tracked separately. The truncation `recovery_hint` consistently names the `use_index` path even from tools that don't expose it, because the caller's recovery move is the same: re-issue the query against `find_node` or `search_nodes` with `use_index=true`, then narrow the live-walk part. **Freshness caveat:** a description is searchable only once its node has been walked into the index since the last rebuild, so the live walk stays authoritative for freshly-edited descriptions.
- **Every children listing is sorted into Workflowy display order at one funnel.** `list_children`, `get_subtree` (all levels), and the workspace-root listing route through `WorkflowyClient::get_children_cancellable` / `get_top_level_nodes_cancellable`, which call `sort_children_by_priority` (ascending `priority`, stable, `None` → head). The `/nodes?parent_id=` endpoint returns children in an internal/creation order — not the outline's display order — so without this sort the read tools disagreed with each other and with the UI (2026-07-12 field report, issue 3). Ascending-priority = top-of-outline is empirically confirmed and consistent with `reorder_nodes`' `priority=0`→head semantics. New read paths that materialise a sibling set route through the same funnel rather than sorting inline. Pinned by `sort_children_by_priority_ascending_none_first_stable`.

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

### A walk reports coverage, not merely survival (`src/api/client.rs`)
- **"Complete" means "covered the subtree", never "did not hit a budget".**
  `fetch_descendants` drops a branch whose child fetch errors — the only
  alternative would be to fail the whole walk over one transient 429 — and
  before 2026-07-16 it dropped that branch with nothing but a `warn!` and
  still returned `TruncationOutcome::complete()`. `wflow-do reindex` then
  reported `1408 nodes ... complete` for a walk that had silently omitted
  whole subtrees, and no field in the response distinguished it from a full
  walk. Coverage was unpredictable rather than merely partial, which is
  worse: consumers (the wflow skill's fast path, `use_index=true`) treat a
  hit as authoritative and a miss as "not there".
- Every dropped branch is counted and named. `SubtreeFetch.skipped_branches`
  carries the parent IDs; a non-empty list forces `truncated: true` and, when
  the walk stopped for no other reason, `truncation_reason:
  "skipped_branches"`. A stopping reason (`NodeLimit` / `Timeout` /
  `Cancelled`) is the more specific fact and keeps precedence, but
  `skipped_branches` still reports alongside it — the two are independent
  facts and a walk can be both cut short *and* incomplete within what it
  reached. `TruncationOutcome::with_skipped` is the single fold point.
- **Recover before reporting.** Each level retries its dropped branches once
  before descending, since a dropped branch removes its entire subtree from
  the walk, not just one node. The retry waits out an open `retry_after`
  window (`RETRY_WINDOW_WAIT_SLACK_MS` of slack), because the dominant cause
  is a 429 and `request_cancellable` fails fast inside that window — an
  immediate retry would burn the branch's one attempt for nothing. The wait
  and the retry both respect the walk's deadline and cancel guard; when the
  budget cannot cover the wait, the branches report as skipped rather than
  blowing the budget.
- The recovery hint for this reason is reason-specific and **overrides any
  tool-specific hint** (`SKIPPED_BRANCHES_RECOVERY_HINT`, applied inside
  `truncation_envelope_with_hint`). Every other hint points at the persistent
  name index, and the index is populated by these same walks — so it shares
  the gap exactly. Pointing a caller there to recover from a partial walk
  restates the defect instead of resolving it. "Narrow the scope" is equally
  wrong: a narrower walk drops the same branches.
- Pinned by `walk_that_drops_a_branch_reports_skipped_branches_not_complete`,
  `fully_walked_subtree_reports_complete_with_no_skipped_branches`,
  `transient_branch_failure_is_recovered_by_the_retry`, and
  `skipped_branches_envelope_overrides_even_a_tool_specific_hint`.

### Subtree exclusion is a persistence rule, enforced at the one serialisation site (`src/utils/name_index.rs`)
- `WORKFLOWY_INDEX_EXCLUDE_SUBTREES` names subtrees that must never be
  **written to disk**. The rule is deliberately narrow: a walk may traverse
  an excluded subtree and the in-memory index may hold it (a live session
  resolving a hash under it still needs an answer, and that memory dies with
  the process); the durable file must not carry it.
- The filter therefore lives in `NameIndex::snapshot()` — the only place the
  persisted form is built, and called only by `save_to_disk`. That is what
  makes it total: the server's periodic checkpoint and an out-of-process
  `wflow-do reindex` both pass through it, so no writer and no walk scope can
  route around it. Pinned by `persisted_snapshot_is_the_only_serialisation_path`,
  which fails the build if `PersistedSnapshot` is ever constructed elsewhere.
- **Seed on `parent_id` as well as `id`.** The excluded root is normally
  *absent* from the index — that is the filter working. A closure that seeds
  only on `id` then finds nothing to walk down from and passes the entire
  subtree through: it fails open, silently, and only for the subtree it
  exists to protect. Observed against the live index on 2026-07-17, after a
  purge had removed the root. Pinned by
  `excludes_descendants_even_when_the_excluded_root_is_absent`, which was
  driven red against that defect before the fix landed (constitution
  principle 6: a control that cannot fail is not a control).
- An exclusion token that parses as neither a UUID nor a 12-char short hash
  is dropped **with a warning**, never kept as a never-matching filter — a
  silently-ignored exclusion is a privacy leak, not a cosmetic bug.
- **Residual, documented:** a node whose chain to an excluded root runs
  through an intermediate that is also absent is unreachable from any seed
  and will persist. Nothing in the entry links it to the token, so closing
  it needs ancestry the index does not carry. Walks return whole levels, so
  in practice a node's parent is present whenever the node is.

### Walk patience: coverage for unattended walks, latency for interactive ones (`src/api/client.rs`)
- `FetchControls::patient` (default `false`) is the single switch. Interactive
  walks fail fast on rate-limit pressure because a caller is waiting and a
  partial answer now beats a complete one in ten minutes; the scheduled
  reindex has the opposite preference, because an index that silently omits
  half the tree cannot tell you what it is missing.
- **Why one retry wave cannot converge.** `request_cancellable` short-circuits
  every request inside an open `retry_after` window, so a single 429 makes the
  rest of the level fail in microseconds and `buffer_unordered` drains it
  straight into the skipped pile. `retry_skipped_branches` waits the window
  out, but re-fanning thousands of branches re-trips the limit and everything
  queued behind the fresh 429 fails fast again. `recover_skipped_branches`
  therefore loops waves under `patient`, stopping on a wave that recovers
  nothing (a 404 will not heal by waiting), on cancel, or on the deadline.
- Both fan-out sites read their width from `FetchControls::concurrency()` so a
  patient walk cannot be patient in one and greedy in the other. Pinned by
  `walk_fan_out_reads_concurrency_from_controls`.
- `--patient` pairs with `--timeout-secs 0`. The retry declines to wait when
  the wait would outlast the deadline, so a patient walk on a short budget
  quietly stops being patient; the CLI warns rather than silently obliging.

### The name index has two writers; saves merge, they do not clobber (`src/utils/name_index.rs`)
- The persistent index is written by **two independent processes**: the
  long-running MCP server (loads at startup, checkpoints every
  `INDEX_SAVE_INTERVAL_SECS` when dirty) and `wflow-do reindex` (loads,
  walks, saves once). Both used to serialise their whole in-memory map
  straight over the file, so each silently discarded every entry the other
  had added since its own load — a window of *hours* for the server. The
  observed symptom was non-monotonic churn in the file: a subtree present at
  15:20 was gone at 16:08, replaced by a different one, even though `ingest`
  is purely additive and nothing prunes. Additive merge semantics cannot
  explain entries disappearing; two last-writer-wins savers can.
- `save_to_disk` is therefore a **read-merge-write under an exclusive
  cross-process lock** (`SaveLock`, an `fs4` advisory lock on a `.lock`
  sibling). The lock covers the whole critical section: without it two savers
  can both read, both merge, and the later rename still drops the earlier's
  additions — the same clobber on a shorter fuse. The lock sits on a sibling
  file, not the index, because the save renames a temp file over the index
  and would swap the very inode a lock was held against. Advisory locks
  release when the handle closes (including on panic or kill), so a crashed
  writer cannot wedge the index the way a `create_new` lock file would.
- **Merge rule: memory wins for any ID this process knows** (it observed that
  node itself, so it is at least as fresh); disk-only IDs are adopted (we
  have no opinion, the other writer did observe them); **tombstoned IDs are
  skipped**. The tombstone set exists because a naive union would resurrect
  entries this process just deleted — the on-disk copy still carries them,
  written by a process that never saw the delete. A merge that protects
  additions must not silently undo deletions.
- An unreadable or wrong-schema file is overwritten rather than propagated as
  an error: refusing to save because another writer left a bad file would
  strand this process's own work, and the save that follows replaces the bad
  file with a good one.
- **Residual, documented:** a writer that never observes a delete can still
  re-add that node from its own memory on its next save. The index is a cache
  whose miss semantics are "not there" and whose stale hit 404s on read, so
  this is a lesser failure than the clobber and needs a shared write-ahead
  log — not a bigger lock — to close properly.
- Pinned by `concurrent_writers_additions_survive_each_others_saves`,
  `merge_adopts_disk_only_entries_into_memory`,
  `merge_does_not_overwrite_a_locally_renamed_node_with_the_stale_disk_name`,
  `merge_does_not_resurrect_a_locally_deleted_node`, and
  `save_overwrites_a_corrupt_file_rather_than_failing`.

### Read-path economy under the rate limit (2026-07-21 second-brain pass)

Seven invariants landed together, all with the same aim: many large,
frequent queries and updates must fit inside Workflowy's 429/timeout
regime without re-paying API cost for state local knowledge can serve.
Spec contracts `C-server-011` … `C-server-017` carry the full statements;
the structural rules for contributors are:

- **Children listings are cached and invalidated at single funnels.**
  Consult/populate ONLY in `get_children_cancellable` /
  `get_top_level_nodes_cancellable`; invalidate ONLY in
  `request_cancellable`'s non-GET pre-send hook (plus the server-side
  `invalidate_for_mutation`, which converges on the same `NodeCache`
  instance). `get_node` is never cache-served — verify-after-write stays
  live. A new read path that lists children MUST route through the
  funnel, not around it.
- **A 429 drains the token bucket** (`stamp_rate_limited` →
  `RateLimiter::drain()`). Never reintroduce burst carry-over across a
  retry window.
- **Blocking maintenance runs on blocking threads.** The index
  checkpoint (and any future fs-heavy background task) wraps in
  `spawn_blocking`; snapshots serialise compact.
- **Local state answers before the network walks.** The short-hash miss
  path merges the disk index (mtime-guarded
  `refresh_from_disk_if_changed`) before walking; workflows record
  `created_nodes` in the `MutationFootprint` so `apply_footprint` seeds
  the index at create time; `wflow-do resolve-link` hydrates from and
  contributes back to the persisted index. A new workflow that creates
  nodes MUST call `footprint.record_created(...)`.
- **Index-backed read paths share the walk paths' predicates**
  (`C-server-018`). `find_backlinks`/`tag_search` `use_index` scans and
  the `prefer_index` fallback mode on `find_node`/`search_nodes` route
  through `node_links_to` / `node_has_tag` / the index matchers — never
  a private reimplementation, so fast and authoritative paths cannot
  fork. Subtree scoping over the index goes through
  `NameIndex::is_descendant_of` (never a direct-parent-equality
  shortcut), and nothing lock-reacquiring runs inside a
  `for_each_entry` closure.
- **The limiter is AIMD** (`C-server-019`): 429 → drain + halve rate
  (floor 1 req/s); success → +0.1 req/s toward the configured ceiling.
- **Incremental sync is a local diff** (`C-server-020`): the index
  records `last_modified` (schema v3) and `wflow-do changed-since`
  serves change windows with zero API calls.
- **Deliberately rejected:** serving `bulk_tag`'s pre-read from the name
  index. The fetched name is the *base for the mutation* — a stale
  index name would clobber a rename, and a stale already-tagged name
  would silently skip a live untagged node. Correctness outranks the
  saved round-trip (constitution hierarchy rule 1).
- **Deliberately rejected:** parallel sibling creates in
  `insert_content` (`C-server-021`). The resume cursor assumes prefix
  completion; concurrency would make the documented resume double-write
  after a mid-run failure. `batch_create_nodes` is the sanctioned
  concurrent create path.

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

### Resolve-walk single-flight per scope (`src/server/mod.rs`)

Every on-demand short-hash resolution walk routes through one helper:
`walk_for_short_hash_inner(short_hash, parent_id)` in `src/server/mod.rs`.
Both wrappers — `walk_for_short_hash` (workspace root) and
`walk_for_short_hash_scoped` (caller-supplied parent) — are thin
delegates to the unified helper, so the single-flight invariant
applies to every call site. The helper maintains an
`inflight_resolve_walk_scopes: Arc<parking_lot::Mutex<HashSet<Option<String>>>>`
keyed by scope (None = workspace root, Some(uuid) = parent UUID). A
second caller for an in-flight scope attaches as a *secondary*: it
polls the name index every 100 ms for its target until either the
index resolves the hash (primary's ingestion succeeded) or the
primary releases the scope marker (primary completed or its task
was dropped).

**Why single-flight.** Pre-2026-05-19 each `resolve_link` call on a
cold cache fired its own workspace walk. On the user's 56k-node
tree at 10 req/s through the shared rate limiter, two concurrent
calls each consumed the same rate-limit tokens, doubling the time
the limiter stayed saturated and starving every other tool. The
100 ms watcher already short-circuited duplicate walks on a hit,
but it didn't recognise that another walk against the same scope
was already running. Single-flight per scope collapses N concurrent
resolves to one walk-worth of load on the rate limiter; secondaries
pay only the index-poll cost (negligible).

**Drop safety.** The primary installs an `InflightResolveGuard`
(an RAII wrapper) before running the walk; if the primary's future
is dropped (cancel, panic, `tool_handler!` timeout), the guard's
`Drop` impl removes the scope marker. Secondaries notice the marker
disappearing on their next 100 ms poll and exit with a final index
check — they never wait forever on a dead primary.

**Routing invariant**: pinned by
`concurrent_resolve_walks_share_one_walk_per_scope` in
`server::load_tests`, which fires two concurrent walks for the
same scope and asserts (via wiremock's `.expect(1)`) that only one
underlying HTTP call hit the upstream. A refactor that drops the
single-flight registry — or routes a new resolve site around
`walk_for_short_hash_inner` — fails the test before it ships.

### Resolve-walk envelope symmetry (`resolve_link`)

`resolve_link` returns `Ok` with a structured payload on every
non-validation outcome: on hit `{id, name, description, parent_id,
resolved_via}`; on miss `{resolved: null, short_hash, scope,
nodes_walked, elapsed_ms, hint}` merged with the four-field
truncation envelope via `with_truncation_envelope`. Pre-2026-05-19
the miss branch returned `Err(tool_invalid_params)` which:

- recorded the call as an `Err` in the op log,
- flipped the `degraded` gate via
  `OpLog::last_unrecovered_failure`, which then gated every
  subsequent `create_node` behind a "server in degraded state"
  warning,
- gave the caller no structured detail about the partial walk
  (`nodes_walked`, `truncation_reason`) to decide a recovery
  path.

Returning `Ok` with `resolved: null` is symmetric with every other
walk-shaped tool (`search_nodes`, `find_node`, `tag_search` all
return Ok with empty matches arrays on no-hit). The caller branches
on `resolved == null` rather than parsing error strings. Pinned by
`resolve_link_returns_ok_with_null_resolved_on_walk_miss` which
verifies the envelope shape AND asserts the op log records the
call as Ok (so the degraded gate cannot regress).

### Diagnostic-probe rate-limiter bypass (`probe_top_level`)

`health_check` and `workflowy_status` issue their probe via
`WorkflowyClient::probe_top_level(deadline)`, which calls
`try_request_cancellable` directly — bypassing both the rate-limit
acquire and the retry loop. The 2026-05-19 user-report observed
both probe attempts timing out inside the rate-limit queue while
an in-flight resolve walk drained the bucket; the 5-second probe
budget elapsed in the queue without ever issuing a network
round-trip, producing the misleading `"two attempts failed:
Timeout | Timeout"` outcome.

The bypass is single-shot per probe (~one request per
diagnostic call) so the upstream impact is negligible. Retries are
also skipped because the caller (`probe_upstream_with_retry`) owns
its own two-attempt budget; nesting the client-level retry inside
that halves each attempt's effective budget and produces the
"Timeout | Timeout" failure mode without ever issuing a network
call.

**Routing invariant**: pinned by
`probe_upstream_with_retry_uses_probe_top_level_not_throttled_path`,
which grep-audits `server/mod.rs` to confirm the probe path uses
`probe_top_level` and never the throttled
`get_top_level_nodes_cancellable`. A regression — re-routing
the probe through the queued path — fails the test before
production.

### `degraded_kind` classification (`workflowy_status`, `health_check`)

When `api_reachable: false` the diagnostic responses carry a
`degraded_kind` string distinguishing the remediation path:

- `"auth"` — recent 401/403 observed; the API key is wrong.
- `"cancelled"` — probe cancelled mid-flight, likely
  `cancel_all` in progress.
- `"local_queue_wedged"` — probe timed out AND `in_flight_walks > 0`;
  the local rate limiter is the bottleneck, not the upstream. Wait
  or `cancel_all`.
- `"upstream_unreachable"` — probe timed out with no in-flight
  work; the upstream is genuinely down. Wait and retry.
- `"upstream_error"` — any other non-success probe outcome.

When `api_reachable: true` the field is `null`. The classification
sits in one helper, `classify_degraded_kind(api_reachable,
probe_error, in_flight_walks)`, so both `health_check` and
`workflowy_status` produce the same value for the same inputs.
Pinned by `workflowy_status_emits_degraded_kind_field` (verifies
the field is present in the response shape) and
`classify_degraded_kind_distinguishes_local_queue_from_upstream`
(unit-tests every classification branch).

### Workflow orchestration shared between MCP and CLI (`src/workflows.rs`)

Workflows that need an API client AND are surfaced by both binaries
live in `src/workflows.rs`. The 2026-05-04 lift extracted
`create_mirror_via_convention`, `insert_content_via_indented`,
`run_transaction`, `apply_bulk_op`, and `smart_insert_under_target`
into this module after the failure-report 2026-05-03 follow-up flagged
the duplication ("why do we have two code bases for the CLI and the
Server"). The 2026-05-22 lift added `resolve_link_via_walk_and_scan`,
`find_node_by_short_hash`, `build_resolve_link_hit_payload`, and
`build_resolve_link_miss_payload` after a user-report showed the CLI
and MCP `resolve_link` envelopes had drifted to incompatible shapes
(CLI: `{link, node}` on hit, Err on miss; MCP: full four-field
truncation envelope with `resolved_via` discriminator). `audit::*`
and `utils::aggregation::*` cover the pure-function half of the same
idea; `workflows::*` covers the half that takes a client.

**Resolve-link lift specifics (2026-05-22).** The full orchestration
doesn't lift cleanly because the MCP handler has server-only concerns
the CLI cannot share: a persistent name index (preflight `O(1)`
lookup, post-walk ingestion), a single-flight scope marker
(`inflight_resolve_walk_scopes`) that collapses N concurrent
resolves to one walk-worth of load on the rate limiter, and an
in-process op-log + degraded-state tracking. The lift extracts the
two pieces that genuinely can be shared: (a) the walk-and-scan step
(`resolve_link_via_walk_and_scan` + the pure `find_node_by_short_hash`
helper); (b) the JSON envelope construction
(`build_resolve_link_hit_payload`, `build_resolve_link_miss_payload`,
and the `RESOLVE_LINK_RECOVERY_HINT` constant). The MCP handler
layers its preflight, single-flight, and ingest on top; the CLI calls
the lifted helpers directly. Both surfaces emit the same wire shape
on hit and on miss. Pinned by
`cli_resolve_link_routes_through_lifted_payload_builders` (grep
audit on `src/bin/wflow_do.rs`) and by the workflow-level shape
tests in `src/workflows.rs::tests`.

Truncation envelope helpers (`truncation_envelope`,
`truncation_envelope_with_hint`, `with_truncation_envelope`,
`with_truncation_envelope_and_hint`, `TRUNCATION_RECOVERY_HINT`) live
in `src/utils/truncation_envelope.rs` so both the server handlers and
the lifted workflows can call them without a module cycle. Pre-2026-05-22
they lived in `src/server/mod.rs`; the extraction was driven by the
resolve_link lift and preserves the existing pin tests
(`every_walk_tool_emits_full_truncation_envelope_in_json` and
`envelope_construction_routes_through_one_helper_no_inline_fields`)
since they audit only `mod.rs` source and the helper bodies were
moved out, not duplicated.

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

**2026-06-16 cross-surface reuse pass.** A full MCP↔CLI audit found the
project had lifted every footprint-bearing orchestration but left the
*read-and-filter predicates* and two write orchestrations un-lifted, and
those had silently drifted. The pass closed them:

- **Mutating lifts** (`workflows.rs`): `duplicate_subtree` + `instantiate_template`,
  sharing one private `deep_copy_subtree(client, source, target, transform, ctx)`
  helper (a per-node `Fn(&WorkflowyNode) -> (String, Option<String>)` closure is
  the only thing that differs between deep-copy and template-instantiate — the
  BFS walk, truncation refusal, and footprint are shared). `walk_parent_chain`
  (cycle-guarded parent walk for `path_of`). Pinned by
  `duplicate_and_template_route_through_workflows`.
- **Pure predicate helpers** (`utils/`): `tag_parser::node_has_tag`,
  `tag_parser::add_tag_to_name` / `remove_tag_from_name`,
  `link_parser::node_links_to`, `html::strip_html`,
  `aggregation::filter_bulk_candidates`. Each replaced a pair of
  divergent inline copies; two were correctness bugs (CLI `bulk-tag`
  had no idempotency; MCP tag-search used a substring scan that shadowed
  longer tags). Routing pinned by `read_predicates_route_through_shared_helpers`.
- **Shared constants** (`defaults.rs`): `READ_BATCH_VALID_OPS`,
  `BULK_UPDATE_VALID_OPS`, `SECONDS_PER_DAY`, `DEFAULT_REVIEW_ROOT` —
  previously hand-written inline in both binaries.

The pass deliberately did NOT touch three flagged items, on Principle-7
grounds (the simplest design that satisfies the contract): the CLI's
`classify()` error-cause stringifier (cosmetic stderr label; sharing it
needs a net-new string-classifier, not reuse of an existing one), the
byte-identical `PartialReason` / `ReorderPartialReason` enums (6 lines,
serialise-identical, split documented by outcome), and a per-node regex
recompile inside `apply_bulk_op` (the single-source `remove_tag_from_name`
helper is preferable to threading a precompiled regex). Recorded as
deferred follow-ups in `tasks/todo.local.md`.

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
