# Development Principles

> Code-level guidance for writing maintainable, reliable Rust software for this codebase.

## Philosophy

Code is written once but read many times. Optimise for the reader. Every line should communicate intent clearly to a developer encountering it for the first time — including the AI agent that will read it a year from now without any context from the original commit.

---

## Core Principles

### 1. Explicit Over Implicit

Make behaviour obvious from the code itself.

- Name functions and variables to describe their purpose.
- Avoid magic numbers — use named constants in `src/defaults.rs`.
- Don't rely on side effects that aren't obvious from the call site.
- Prefer verbose clarity over clever brevity.

```rust
// Avoid
let t = 30_000;
tokio::time::sleep(Duration::from_millis(t)).await;

// Prefer
tokio::time::sleep(Duration::from_millis(defaults::CACHE_TTL_MS)).await;
```

### 2. Fail Loudly

When something goes wrong, make it unmistakable. The 2026-05-02 4-minute write hang traced to a handler skipping the safety-net wrapper — silent failures are the most expensive failures.

- Return typed errors (`WorkflowyError` variants) with context, not generic strings.
- Never swallow `Result` values silently; the project has `#![warn(unused_must_use)]` for this.
- Use the structured error envelope helpers (`tool_error`, `tool_invalid_params`, `workflow_error_to_mcp`) so every error response carries `{operation, node_id, hint, proximate_cause, error}`.
- Log failures with enough detail to diagnose, but never log secrets or user content.

```rust
// Avoid — swallows context, doesn't classify
return Err(McpError::internal_error("failed".into(), None));

// Prefer — routes through the canonical helper, classifies proximate cause
return Err(tool_error("get_node", Some(&node_id), e));
```

### 3. Small Functions, Clear Names

Each function does one thing; its name says what.

- If a comment is needed to explain what code does, extract a named function instead.
- Functions longer than ~50 lines often do too much. Handler bodies in `server/mod.rs` are the exception when their tool-specific JSON shape is large, but the orchestration inside them is usually two helper calls and a `with_truncation_envelope`.
- Avoid generic names like `process`, `handle`, `manage`.
- Use consistent naming: `compute_*` for pure aggregations, `apply_*` for footprint-applying mutations, `validate_*` for boundary checks, `*_via_*` for workflow orchestrations.

### 4. Type Everything

Rust's type system is a tool for correctness, not a burden.

- Define explicit types for all parameter structs (`schemars::JsonSchema` derives the wire contract).
- Use newtypes for domain values that should not be confused with raw strings: `NodeId`, `WorkflowyError::InvalidInput { reason: String }` is better than a bare `String` error.
- Use discriminated unions (Rust enums with data) for state machines: `TransactionOutcome { Applied | RolledBack }`, `InsertContentOutcome { Complete | Partial { reason, … } }`.
- No `Box<dyn Any>` without a documented reason; no `serde_json::Value` where a typed struct would do.

```rust
// Typed state machine — the caller cannot forget a branch
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum InsertContentOutcome {
    Complete { parent_id: Option<String>, created_count: usize, last_inserted_id: Option<String> },
    Partial  { parent_id: Option<String>, reason: PartialReason, … },
}
```

### 5. Test Behaviour, Not Implementation

Tests verify what the code does, not how it does it. They are documentation; name them to describe expected behaviour.

- Test the public interface, not private internals.
- Pin tests (grep-based) enforce consistency invariants — every rule worth stating in CLAUDE.md is worth pinning here. See Principle 6 in the constitution.
- Avoid mocking internal implementation details; the project uses dependency-injected `WorkflowyClient` so unit tests construct a real client against a stub HTTP endpoint when needed.
- One behaviour per test, where practical. The test name is the behaviour spec.

```rust
// Avoid — tests implementation
#[test] fn calls_invalidate_for_mutation_internally() { … }

// Prefer — tests observable behaviour
#[test] fn move_node_invalidates_old_parent_cache_before_api_call() { … }
```

### 6. Defensive at Boundaries, Trusting Inside

Validate external input rigorously; trust internal contracts.

- Validate at every system boundary: tool parameters (`Parameters<T>` + `#[serde(deny_unknown_fields)]`), API responses (typed deserialisation), env vars (typed parse with error on missing), file contents (length caps, encoding checks).
- Inside the codebase, trust that types guarantee correctness. Don't re-validate at every call site.
- The `NodeId` newtype's hand-written `Deserialize` rejects `"null"` / `"undefined"` / whitespace-only strings up-front — once a `NodeId` reaches a handler body, it has already been validated; the handler should not re-check.

### 7. Helpers Earn Their Keep

A helper used in one place is not a helper — it's a misplaced inline. Don't extract a function whose body would be shorter than its call site.

- Extract when there are three or more call sites with substantially identical logic.
- Extract when the inline version forces the reader to context-switch between concerns (e.g. business logic intermingled with envelope construction).
- Don't extract for hypothetical future call sites. The simplest design that satisfies today's contract is the right one.

When an extraction _does_ land, name the canonical helper in CLAUDE.md's helper inventory and add a pin test if the rule is "always use this helper for this concern".

---

## Code Style

### Formatting

- `cargo fmt` handles all formatting — don't fight it. Run before committing.
- No manual alignment or stylistic overrides outside the standard `rustfmt.toml` (which the project uses unmodified at present).
- Consistent casing: `snake_case` for functions/variables, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for constants.

### Imports

- Group imports by source: `std` first, then external crates, then `crate::` paths. `rustfmt` reorders within groups.
- Avoid `use crate::foo::*;` glob imports outside a `mod.rs` re-export block — they hide what's in scope.
- Prefer named exports; the project uses `mod params; pub use params::*;` only at re-export boundaries.

### Comments

Comments explain _why_, not _what_. Good code is self-documenting for the _what_. The constitution's commit checklist captures this as "commented if non-obvious".

```rust
// Avoid — describes what code does, banal
// Loop through nodes and filter by type
let filtered: Vec<_> = nodes.iter().filter(|n| n.layout_mode.as_deref() == Some("todo")).collect();

// Prefer — explains the non-obvious constraint
// Workflowy uses layout_mode=="todo" to mark a node as a todo-bullet AND
// uses completed_at to mark it done; both checks needed because the wire
// surface conflates the two states under one boolean read at one layer
// and two writes at another. See client.rs::set_completion.
let pending_todos = nodes.iter().filter(|n| is_todo(n) && !is_completed(n));
```

The codebase already has many "Pre-2026-05-03 …" comments naming the incident that motivated a piece of code. That style is encouraged — the date + symptom is more useful than a generic "for correctness" hand-wave.

### Error Messages

Write error messages for the person debugging at 2 a.m.

- Include relevant context: IDs, values, operation attempted, budget that fired.
- Suggest possible causes or remediation. The `tool_error` classifier already attaches a hint string per `ProximateCause`; new error sites benefit from extending the classifier rather than hand-writing a hint.
- Don't expose internal state to end users that they can't act on; the structured envelope is the right shape — the user gets `proximate_cause` + `hint`, the log gets the full chain.

```rust
// Avoid — opaque
return Err(WorkflowyError::internal("failed"));

// Prefer — names the budget, the elapsed time, and the recovery path
return Err(WorkflowyError::Timeout);
// → routes through tool_error which classifies as ProximateCause::Timeout
// → caller sees hint: "upstream timeout — narrow scope or wait for load to drop"
```

---

## Patterns to Follow

### Result Types for Expected Failures

For operations that can fail in expected ways, use `Result<T, WorkflowyError>` — and let the wrapper translate the variant to its envelope. The variants are themselves the type-safe error categorisation; don't stringify them at the boundary.

### Early Returns

Reduce nesting by handling edge cases first. The handler bodies in `server/mod.rs` use this consistently for `validate_and_resolve(...)?` and footprint application.

```rust
async fn handler(&self, params: …) -> Result<CallToolResult, McpError> {
    tool_handler!(self, "name", ToolKind::Read, params, {
        let resolved = self.validate_and_resolve(&params.node_id).await?;
        // main logic at top level — every error path already routes
        // through the structured envelope via `?`
    })
}
```

### Async/Await Over Manual Future Composition

Use `async`/`await` for readability. Avoid `.then()` chains and manual `Future` construction unless you need a specific shape (e.g. `tokio::select!` for racing cancel + deadline + work).

### Immutability by Default

Prefer `let` over `let mut`. Mutate only when the shape genuinely demands it (e.g. building up a JSON `serde_json::Map` for fold-into-envelope). Pure functions in `aggregation.rs` take `&[WorkflowyNode]` and return owned `Vec<Value>` — no shared mutable state.

### Pin Tests for Consistency Rules

Any rule of the form "always do X via Y" gains a grep-based pin test in the same commit that establishes the rule. See the helper inventory in the constitution; the seven currently-pinned rules are the existence proof that the pattern scales.

---

## Anti-Patterns to Avoid

- **Boolean blindness** — `fn foo(force: bool, dry_run: bool, recursive: bool)`. Use an `enum FooMode { … }` or named fields on a config struct.
- **Stringly typed** — `match status { "pending" => …, "completed" => … }`. Use an enum with `parse`/`as_str` methods.
- **God objects** — `WorkflowyMcpServer` is intentionally narrow (router + cache + index + cancel + budgets + op log); new long-lived state belongs in its own module with a constructor.
- **Copy-paste inheritance** — duplicate orchestration in MCP and CLI is the canonical anti-pattern this codebase has fought multiple rounds against. See Principle 5 in the constitution and the lift catalogue.
- **Comments as deodorant** — if code needs a paragraph of explanation, rewrite it. The comment should name the _surprise_, not the _operation_.
- **Sentinel values** — never `unwrap_or("unknown")` or `unwrap_or_default()` to silence an error. Use `Result` and let the typed variant surface.
- **Panic in library code** — no `.unwrap()` / `.expect()` outside test code or genuinely-unreachable post-conditions. Use `?` and the typed `WorkflowyError` variants.
- **Defensive code for impossible cases** — if an invariant guarantees `x` is non-empty, don't `if x.is_empty() { return Err(…) }`. Trust the type; if the invariant is wrong, fix the invariant.

---

## See Also

- [Constitution](./constitution.md) — core principles, definition of done, conflict-resolution hierarchy
- [Architecture Principles](./principles-architecture.md) — structural guidance, lift catalogue, full pin-test inventory
- [MCP Principles](./principles-mcp.md) — protocol-level operational best practices
- [Security Principles](./principles-security.md) — security requirements
