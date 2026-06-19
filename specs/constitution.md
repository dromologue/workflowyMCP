# Constitution

> Non-negotiable principles governing the Workflowy MCP Server project.
> **Consult this document at the start of any non-trivial task.** When two principles conflict, the conflict-resolution hierarchy at the bottom of this file decides.

---

## How to Use This Document

This constitution is the canonical reference for every contributor — human or AI agent. The expectation:

1. **Before starting work** — skim the eight Core Principles below. If the task touches a specific concern (architecture, security, MCP wire surface, code style), open the corresponding `principles-*.md` file for the detail.
2. **While working** — when a design choice is non-obvious, check whether one of the principles or the Helper-First Construction list already answers it. Reach for the existing helper, not a new one.
3. **Before committing** — run through the Definition of Done checklist. A commit that fails any item is not done.
4. **When in conflict** — apply the conflict-resolution hierarchy. Don't trade correctness for performance; don't trade security for convenience.

The eight principles are deliberately short. The detail lives in:

- **[Architecture Principles](./principles-architecture.md)** — structural patterns, module boundaries, the lift catalogue, every pin-tested invariant with its enforcement test name.
- **[Development Principles](./principles-development.md)** — Rust idioms, naming, testing patterns, anti-patterns.
- **[MCP Principles](./principles-mcp.md)** — protocol-level operational best practices with per-principle status against the current codebase.
- **[Security Principles](./principles-security.md)** — secrets handling, input validation, error message constraints, audit logging requirements.

---

## Mission

**AI-native outlining.** A seamless bridge between Claude's intelligence and Workflowy's structure, enabling natural language interaction with hierarchical knowledge — from both the MCP transport (for in-session use) and the `wflow-do` CLI (for batch/offline use). The two surfaces are equal citizens; one logic body serves both.

---

## Core Principles

### 1. Correctness is the First Concern

The server holds users' knowledge. Silent data loss, silent cache staleness, or silently-wrong results are unacceptable failure modes. Every other principle bends to this one when they conflict.

- Workflowy is the source of truth; local cache is ephemeral. Caches invalidate _before_ mutations land at the API, not after — a dropped future on timeout cannot strand stale data.
- Reads return what's in Workflowy now, or a typed error explaining why they can't. Never invented data, never sentinel values, never `unwrap_or("unknown")`.
- Writes are observable: every mutation appears in the op log; the cache invalidation is declared up-front via `MutationFootprint` so a missed invalidation is a workflow bug (testable), not a silent staleness bug (not).

### 2. Strict Typed Contracts

Rust's type system is the first line of defence — used to its full potential at every boundary.

- `NodeId` newtype rejects malformed IDs (including the literal strings `"null"` / `"undefined"` / whitespace-only) at the parameter boundary, not deep in the API layer.
- `WorkflowyError` distinguishes `InvalidInput` (caller's fault) from operational variants so wrappers translate each to the correct envelope without inspecting strings.
- `Parameters<T>` wrapper enforces schema generation at compile time; the literal identifier name is load-bearing for `rmcp-macros 0.16` and pinned by `parameter_bearing_tools_publish_non_empty_input_schema_properties`.
- `#[serde(deny_unknown_fields)]` on every parameter struct so a typo'd field name fails fast with a recorded error rather than silently defaulting.
- No `.unwrap()` / `.expect()` outside test code or genuinely-unreachable post-conditions; no dropped `Result` values.

### 3. Resilient by Default

Failure paths are first-class, not afterthoughts. Every long-running operation observes cancellation; every walk has a wall-clock budget; every external call has a retry policy.

- Every non-diagnostic tool handler runs through the `tool_handler!` macro, classified by `ToolKind`. The wrapper observes the shared `CancelRegistry` and enforces a kind-keyed deadline so a runaway loop returns a structured timeout instead of "no result received".
- Every long-running operation in the request pipeline (HTTP send, rate-limiter wait, inter-attempt backoff) is racing the same `CancelGuard`. Skipping this regresses the reliability invariant that `cancel_all` frees the shared `RateLimiter` within ~50 ms.
- Every walk-shaped tool surfaces the four-field truncation envelope (`truncated`, `truncation_limit`, `truncation_reason`, `truncation_recovery_hint`) constructed via the canonical helper. A caller hitting the 20 s walk budget always receives the same actionable recovery hint, regardless of which tool they called.
- Retries use exponential backoff with jitter; never tight-loop a failing API call.

### 4. Helper-First Construction

Every cross-cutting concern with more than one possible call site has exactly one canonical entry point — and a pin test that enforces routing through it. The current catalogue:

| Concern | Helper | Pin test |
| --- | --- | --- |
| Validate-then-resolve a node ID | `validate_and_resolve()` | (collapsed boilerplate; no pin test needed) |
| Pre-mutation cache invalidation | `invalidate_for_mutation()` / `apply_footprint()` | (correctness, not enforcement) |
| Wrap a handler body | `tool_handler!()` macro | |
| Validation error envelope | `tool_invalid_params()` | `handler_body_validation_uses_structured_envelope_not_bare_invalid_params` |
| Operational error envelope | `tool_error()` | `operational_failures_route_through_tool_error_not_bare_internal_error` |
| Translate `WorkflowyError` from a workflow | `workflow_error_to_mcp()` | `workflow_error_translation_routes_through_workflow_error_to_mcp` |
| Build the JSON truncation envelope | `with_truncation_envelope()` / `truncation_envelope()` | `envelope_construction_routes_through_one_helper_no_inline_fields` |
| List-shaped aggregation | `compute_*` / `filter_*` in `aggregation.rs` | `list_shaped_handlers_route_through_aggregation_helpers` |
| Workflowy link → short-hash extraction | `utils::link_parser::extract_workflowy_short_hash()` | `link_parsing_routes_through_extract_workflowy_short_hash` |
| Render diagnostic `scope_resolved` | `scope_resolved_label()` | |
| MCP↔CLI surface parity | (the `wflow-do` subcommand for each tool) | `cli_covers_every_non_diagnostic_mcp_tool` |

**Rule for new helpers:** when a refactor establishes "X must always be done via Y", encode the rule as a grep-based pin test in the same commit that adopts the helper. Without the pin test, the rule survives one refactor; with the pin test, it survives indefinitely.

### 5. Single Source of Truth Across Surfaces

The MCP server and the `wflow-do` CLI are two windows onto the same tool surface. Logic shared between them lives once, in one of three modules:

- **`src/workflows.rs`** — orchestration that needs an API client. Each workflow takes `&WorkflowyClient + WorkflowContext` and returns `(TypedResult, MutationFootprint)`. The MCP wrapper applies the footprint; the CLI discards it.
- **`src/utils/aggregation.rs`** — pure aggregation over walked nodes. Both surfaces call the helper; tests pass deterministic `today`/`now_ms` so behaviour stays observable.
- **`src/audit.rs`** — pure analyses over node sets. Both surfaces call directly.

**Anti-pattern**: inlining the same orchestration in both binaries. Pre-2026-05-04 the codebase carried duplicate copies of `apply_txn_op`, `smart_insert`, `audit_mirrors_walk_chunked`; drift was silent until a user noticed the two surfaces disagreeing. The lift catalogue in `principles-architecture.md` is the canonical record of what has been collapsed; new tools with meaningful cross-surface duplication grow an entry there, not a second copy.

**Parity invariant:** every non-diagnostic MCP tool has a matching `wflow-do` CLI subcommand routed through the same `WorkflowyClient`. Pinned by `cli_covers_every_non_diagnostic_mcp_tool`. New MCP tools land with their CLI subcommand in the same commit.

### 6. Pin-Tested Invariants

Quality is non-negotiable, and consistency rules are pinned by tests that fail the build when violated. Coverage targets matter less than _enforcement_ targets.

- **Unit tests** for all business logic; per-module `#[cfg(test)]` blocks alongside source.
- **Integration tests** for live Workflowy API interactions (`tests/live_insert.rs`, gated by `WORKFLOWY_API_KEY`).
- **Pin tests** (a.k.a. invariant tests) for every consistency rule the team has agreed on. They grep the source at `cargo test` time so an invariant cannot be silently broken by a future contributor. The seven pin tests under the Helper-First table above are the current set; the full inventory lives in `principles-architecture.md`.

A rule worth stating in a doc is a rule worth pinning in code. Convention without enforcement decays in one refactor cycle.

### 7. Paranoid Security

Sensitive data is protected at every layer; security wins every conflict against convenience or performance.

- API keys and credentials are never logged at any level, never echoed (even partially masked), never embedded in error messages.
- Environment-based configuration only; no hardcoded secrets, no fallback to insecure defaults.
- All sensitive files (`.env`, credentials.json, machine-specific paths) gitignored by default.
- No user content in error messages or logs without explicit redaction.
- Destructive operations (`delete_node`, `move_node`, `bulk_update` with `delete`) leave an audit trail in the op log.
- All external input (user, API response, env var, file contents) is hostile until validated at the boundary.

### 8. Public Utility First

This is an open-source tool for the broader community. Design decisions consider use cases beyond the original author's workflow.

- Clear documentation for self-service adoption: README, `docs/SETUP.md`, `templates/secondbrain/`.
- No machine-specific paths in the repo — all user-data locations come from env vars (`$SECONDBRAIN_DIR`, `$WORKFLOWY_INDEX_PATH`) so a fresh user can stand up the workflow without inheriting the original author's IDs.
- Accessibility to developers of varying skill levels: typed errors carry hints; banner messages name the recovery path; CLI subcommands print actionable JSON.
- Backwards-compatible by default; breaking changes only in major versions with migration guidance.

---

## Definition of Done

Every commit must satisfy these checks. A commit that fails any item is not done; mark the task in-progress and finish before moving on.

- [ ] **Builds clean.** `cargo build --bins --lib` succeeds with no warnings introduced by this change.
- [ ] **All tests pass.** `cargo test --lib` is green. Live-integration tests (`tests/live_insert.rs`) pass if `WORKFLOWY_API_KEY` is set.
- [ ] **Pin tests updated.** If this change establishes a new "always do X via Y" rule, add a grep-based pin test in the same commit.
- [ ] **Specs in sync.** CLAUDE.md, the relevant `principles-*.md` file, and (for cross-surface logic) the lift catalogue in `principles-architecture.md` reflect the new state. No stale references to renamed functions or removed code.
- [ ] **CLI parity preserved.** If this change adds or modifies an MCP tool, the matching `wflow-do` subcommand lands in the same commit and `cli_covers_every_non_diagnostic_mcp_tool` passes.
- [ ] **No secrets in diff.** Quick scan: no API keys, tokens, `.env` contents, or machine-specific paths in the staged hunks.
- [ ] **Error envelopes consistent.** Any new error path routes through `tool_invalid_params` (validation) or `tool_error` (operational); workflow returns translate through `workflow_error_to_mcp`.
- [ ] **Cache invalidated up-front.** Any new write handler calls `invalidate_for_mutation` (or returns a `MutationFootprint`) _before_ the API call, not in the success branch.
- [ ] **Commented if non-obvious.** Workarounds, subtle invariants, and surprising design choices carry a one-line `WHY` comment naming the constraint or incident. Banal comments stay out.

---

## Conflict-Resolution Hierarchy

When two principles or design considerations conflict, apply this order:

1. **Correctness** — does the change reliably do what it claims for all valid inputs? Never trade away.
2. **Security** — does the change leak data, expand attack surface, or weaken validation? Never trade away.
3. **Simplicity** — is this the simplest design that satisfies (1) and (2)? Boring solutions first.
4. **Cross-surface consistency** — does the MCP and CLI behaviour converge? Lift, don't duplicate.
5. **Maintainability** — can another developer (or AI agent) read this in six months and predict the rest of the codebase from it?
6. **Performance** — does it meet the response-time targets (typical operation under 2 s)?
7. **Extensibility** — can related future features be added without restructuring?

Optimise in order. Never sacrifice (1) or (2) for anything below. Prefer (3) over (7) until extensibility is _proven_ necessary by a concrete user requirement, not an imagined one.

---

## Design Philosophy

### Smart Workflows Over Atomic Operations

Tools should handle multi-step operations intelligently. `smart_insert` over raw `create_node` for common cases; sensible defaults that work for 80% of cases; explicit overrides for the rest.

### Smart Defaults with Override

When requests are ambiguous, make the reasonable assumption, execute, and clearly communicate what assumption was made. Allow explicit override. Never block on clarification for recoverable decisions.

### Indentation as Structure

Hierarchical content uses 2-space indentation. Simple, predictable, no magic parsing. What you see is what you get.

### User-Controlled Rate Limiting

Cache aggressively but transparently. Expose configuration for rate-limit behaviour. Let users choose between speed and freshness.

---

## Performance Targets

- Tool response time: **under 2 seconds** for typical operations.
- Cache TTL: configurable, default 30 seconds.
- Subtree walk hard caps: `defaults::MAX_SUBTREE_NODES` (10 000) and `defaults::SUBTREE_FETCH_TIMEOUT_MS` (20 s) — whichever fires first surfaces truncation with a recovery hint.
- **Prioritise correctness over speed when in conflict.**

---

## Architecture Constraints

### Closed Core

- Curated, stable tool set; no plugin system or dynamic tool loading.
- Quality over quantity in tool offerings.
- Each tool has a clear, distinct purpose; overlapping tools are a smell.

### Technology Stack

- Language: **Rust** (2021 edition).
- Async runtime: Tokio.
- MCP SDK: rmcp 1.7 (proc-macro tool registration).
- Serialization: serde + schemars (JSON Schema generation).
- HTTP: reqwest with exponential backoff retry.
- Transport: stdio (Claude Desktop).

---

## What We Will NOT Do

- Support authentication methods other than API key for the Workflowy upstream.
- Implement real-time sync or webhooks.
- Build a web UI or standalone application.
- Support Workflowy features not exposed via the public API.
- Compromise security for convenience.
- Inline the same orchestration in both binaries when it can be lifted to `workflows.rs` / `aggregation.rs` / `audit.rs`.
- Ship a rule that isn't pinned by a test if a pin test is feasible.

---

## See Also

- [Architecture Principles](./principles-architecture.md) — structural detail, lift catalogue, full pin-test inventory
- [Development Principles](./principles-development.md) — Rust idioms, code style, testing patterns
- [MCP Principles](./principles-mcp.md) — protocol-level best practices with current status
- [Security Principles](./principles-security.md) — secrets, validation, audit logging
- [Implementation Plan](./implementation-plan.md) — technical approach
