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
