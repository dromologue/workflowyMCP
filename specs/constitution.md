# Constitution

> Non-negotiable principles governing the Workflowy MCP Server project.

## Detailed Principles

This constitution establishes core principles. For detailed guidance, see:

- **[Architecture Principles](./principles-architecture.md)** - Structural patterns, module boundaries, data flow
- **[Development Principles](./principles-development.md)** - Code style, testing approach, patterns to follow
- **[Security Principles](./principles-security.md)** - Secrets management, input validation, audit requirements

---

## Mission

**AI-native outlining**: A seamless bridge between Claude's intelligence and Workflowy's structure, enabling natural language interaction with hierarchical knowledge.

## Core Principles

### 1. Public Utility First

This is an open-source tool for the broader community. Design decisions must consider:
- Diverse use cases beyond the original author's workflow
- Clear documentation for self-service adoption
- Accessibility to developers of varying skill levels

### 2. Resilient by Default

The server must gracefully handle failures:
- Retry transient API failures with exponential backoff
- Never silently lose user data or content
- Graceful degradation when Workflowy API is unavailable
- Clear error messages that guide resolution

### 3. Strict Typed Contracts

Rust's type system is the first line of defence — used to its full potential:

- `NodeId` newtype rejects malformed IDs (incl. the literal strings `"null"`/`"undefined"`) at the parameter boundary, not deep in the API layer.
- `WorkflowyError` distinguishes `InvalidInput` (caller's fault) from operational variants so the wrapper can translate each to the right MCP envelope without inspecting strings.
- `Parameters<T>` wrapper enforces schema generation at compile time; the literal name is load-bearing for `rmcp-macros` and pinned by `parameter_bearing_tools_publish_non_empty_input_schema_properties`.
- `#[serde(deny_unknown_fields)]` on every parameter struct so a typo'd field name fails fast with a recorded error instead of silently defaulting to `None`.
- `schemars`-generated JSON schemas for every tool's input — the wire contract is derived, not hand-written.

No `Result` is dropped silently (`#![warn(unused_must_use)]`); no `.unwrap()` / `.expect()` outside test code or unreachable post-conditions.

### 4. Paranoid Security

Sensitive data must be protected at all layers:
- API keys and credentials never logged, even at debug level
- Environment-based configuration only (no hardcoded secrets)
- All sensitive files gitignored by default
- No user content in error messages or logs
- Audit trail for destructive operations (delete, move)

### 5. Comprehensive Testing — with Pin-Tested Invariants

Quality is non-negotiable, and consistency invariants are pinned by tests that fail the build when violated:

- **Unit tests** for all business logic; per-module `#[cfg(test)]` blocks alongside source.
- **Integration tests** for Workflowy API interactions (`tests/live_insert.rs`, gated by `WORKFLOWY_API_KEY`).
- **Pin tests** (a.k.a. invariant tests) for every consistency rule the team has agreed on — they grep the source at `cargo test` time so an invariant cannot be silently broken by a future contributor. Coverage targets matter less than enforcement targets: if a rule is worth stating, it's worth pinning.
- Tests must pass before merge.

Examples of currently-pinned invariants:

- `parameter_bearing_tools_publish_non_empty_input_schema_properties` — the `Parameters` wrapper rule that the wire schema depends on.
- `handler_body_validation_uses_structured_envelope_not_bare_invalid_params` — handler-body validation routes through the structured error helper.
- `operational_failures_route_through_tool_error_not_bare_internal_error` — runtime failures route through the structured error helper.
- `workflow_error_translation_routes_through_workflow_error_to_mcp` — workflow returns translate through one canonical helper, not inline `match` arms.
- `list_shaped_handlers_route_through_aggregation_helpers` — every list-shaped MCP handler routes through `src/utils/aggregation.rs`.
- `envelope_construction_routes_through_one_helper_no_inline_fields` — JSON truncation envelopes use the canonical helper, not inline four-field emission.
- `cli_covers_every_non_diagnostic_mcp_tool` — the `wflow-do` CLI surface mirrors the MCP surface.

The bar for adding a new pin test: any time a refactor establishes that "X must always be done via Y", encode the rule in a grep-based test in the appropriate `tests` module. Pre-2026 the project carried these as conventions in CLAUDE.md; the post-2026 standard is convention plus a test that fails the build if the convention slips.

### 6. Single Source of Truth Across Surfaces

The MCP server and the `wflow-do` CLI are two windows onto the same tool surface; logic shared between them lives once. Three modules carry the contract:

- **`src/workflows.rs`** — orchestration that needs an API client. Each workflow takes `&WorkflowyClient + WorkflowContext` and returns `(TypedResult, MutationFootprint)`. Both surfaces call the workflow; the MCP wrapper applies the footprint via `apply_footprint`, the CLI discards it.
- **`src/utils/aggregation.rs`** — pure aggregation over walked nodes. `compute_overdue`, `compute_upcoming`, `compute_recent_changes`, `filter_todos`, `compute_project_summary`, `compute_daily_review`. Both surfaces call the helper; tests pass deterministic `today`/`now_ms` to keep behaviour observable.
- **`src/audit.rs`** — pure analyses over node sets (`audit_mirrors_with_external`, `build_review`). Both surfaces call directly.

Anti-pattern: **inlining the same orchestration in both binaries**. Pre-2026-05-04 the codebase carried duplicate copies of `apply_txn_op`, `smart_insert`, and `audit_mirrors_walk_chunked` in each binary; drift was silent and only surfaced when a user noticed the two surfaces disagreeing. The lift catalogue in `principles-architecture.md` is the canonical record of what has been collapsed; new tools with meaningful cross-surface duplication should grow an entry there, not a second copy.

### 7. Semver Strict Compatibility

Users must be able to trust version numbers:

- Breaking changes only in major versions
- Deprecation warnings before removal
- Changelog maintained for all releases
- Migration guides for major version upgrades

## Design Philosophy

### Smart Workflows Over Atomic Operations

Tools should handle multi-step operations intelligently:
- `smart_insert` over raw `create_node` for common cases
- Workflows that reduce cognitive load on users
- Sensible defaults that work for 80% of cases

### Smart Defaults with Override

When requests are ambiguous:
- Make reasonable assumptions based on context
- Clearly communicate what assumption was made
- Allow explicit override when needed
- Never block on clarification for recoverable decisions

### Indentation as Structure

Hierarchical content parsing:
- Spaces and tabs define nesting (2 spaces or 1 tab = 1 level)
- Simple, predictable, no magic parsing
- What you see is what you get

### User-Controlled Rate Limiting

Respect user agency:
- Expose configuration for rate limit behavior
- Cache aggressively but transparently
- Let users choose between speed and freshness

## Performance Targets

- Tool response time: <2 seconds for typical operations
- Cache TTL: Configurable, default 30 seconds
- Prioritize correctness over speed when in conflict

## Documentation Standards

Developer-focused documentation:
- JSDoc comments for all public functions
- Architecture decision records for significant choices
- Contribution guide for external contributors
- API reference for all MCP tools

## Architecture Constraints

### Closed Core

- Curated, stable tool set
- No plugin system or dynamic tool loading
- Quality over quantity in tool offerings
- Each tool must have clear, distinct purpose

### Helper-First Construction

Every cross-cutting concern with more than one possible call site has exactly one canonical entry point — and a pin test that enforces routing through it. The canonical helpers are listed in `principles-architecture.md` and in the helper inventory in CLAUDE.md. Examples in current code:

- Validate-then-resolve a node ID → `validate_and_resolve()`
- Pre-mutation cache invalidation → `invalidate_for_mutation()` (or `MutationFootprint` via `apply_footprint()`)
- Wrap a handler body → `tool_handler!()` macro
- Validation error envelope → `tool_invalid_params()`
- Operational error envelope → `tool_error()`
- Translate a `WorkflowyError` from a workflow call → `workflow_error_to_mcp()`
- Build the JSON truncation envelope → `with_truncation_envelope()` (fresh-payload merge) or `truncation_envelope()` (fold-into-existing-Map)
- Render the diagnostic `scope_resolved` field → `scope_resolved_label()`

When the rule becomes "always go through helper X for case Y", encode it via a grep-based pin test in the same commit that adopts the helper. Without the pin test, the rule survives one refactor; with the pin test, it survives indefinitely.

### Technology Stack

- Language: Rust (2021 edition)
- Async Runtime: Tokio
- MCP SDK: rmcp 0.16 (proc macro tool registration)
- Serialization: serde + schemars (JSON Schema generation)
- HTTP: reqwest with exponential backoff retry
- Transport: stdio (Claude Desktop compatible)

## What We Will NOT Do

- Support authentication methods other than API key
- Implement real-time sync or webhooks
- Build a web UI or standalone application
- Support Workflowy features not exposed via API
- Compromise security for convenience
