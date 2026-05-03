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
