# Development Principles

> Code-level guidance for writing maintainable, reliable software.

## Philosophy

Code is written once but read many times. Optimize for the reader. Every line should communicate intent clearly to a developer encountering it for the first time.

---

## Core Principles

### 1. Explicit Over Implicit

Make behavior obvious from the code itself.

- Name functions and variables to describe their purpose
- Avoid magic numbers—use named constants
- Don't rely on side effects that aren't obvious from the call site
- Prefer verbose clarity over clever brevity

```typescript
// Avoid
const t = 30000;
await sleep(t);

// Prefer
const CACHE_TTL_MS = 30_000;
await sleep(CACHE_TTL_MS);
```

### 2. Fail Loudly

When something goes wrong, make it unmistakable.

- Throw typed errors with context, not generic strings
- Never swallow exceptions silently
- Use assertions for impossible states (and let them crash)
- Log failures with enough detail to diagnose

```typescript
// Avoid
try { ... } catch { return null; }

// Prefer
try { ... } catch (error) {
  throw new WorkflowyApiError('Failed to fetch node', { nodeId, cause: error });
}
```

### 3. Small Functions, Clear Names

Each function should do one thing and its name should say what.

- If you need a comment to explain what code does, extract it into a named function
- Functions longer than 20-30 lines often do too much
- Avoid generic names like `process`, `handle`, `manage`
- Use consistent naming conventions throughout

### 4. Type Everything

TypeScript's type system is a tool for correctness, not a burden.

- Define explicit return types for public functions
- Use discriminated unions for state machines
- Leverage `as const` for literal types
- No `any` without documented justification

```typescript
// Discriminated union for operation results
type OperationResult =
  | { status: 'success'; data: NodeData }
  | { status: 'not_found'; searchedId: string }
  | { status: 'error'; error: Error };
```

### 5. Test Behavior, Not Implementation

Tests verify what the code does, not how it does it.

- Test public interfaces, not private methods
- One assertion per test when practical
- Tests are documentation—name them to describe expected behavior
- Avoid mocking internal implementation details

```typescript
// Avoid: testing implementation
test('calls workflowyApi.search internally', ...);

// Prefer: testing behavior
test('returns matching nodes when search term exists in content', ...);
```

### 6. Defensive at Boundaries, Trusting Inside

Validate external input rigorously; trust internal contracts.

- Validate all user input, API responses, file contents
- Use Zod schemas for runtime validation of external data
- Inside modules, trust that types guarantee correctness
- Don't duplicate validation at every layer

---

## Code Style

### Formatting

- Prettier handles all formatting—don't fight it
- No manual alignment or stylistic overrides
- Consistent casing: `camelCase` for variables/functions, `PascalCase` for types

### Imports

- Group imports: external packages, then internal modules, then types
- Use path aliases for deep imports (`@/services/...`)
- Prefer named exports over default exports

### Comments

Comments explain *why*, not *what*. Good code is self-documenting for the *what*.

```typescript
// Avoid: describes what code does
// Loop through nodes and filter by type
const filtered = nodes.filter(n => n.type === 'task');

// Prefer: explains non-obvious reasoning
// Workflowy marks completed items with 'task' type even after completion
const filtered = nodes.filter(n => n.type === 'task');
```

### Error Messages

Write error messages for the person debugging at 2am.

- Include relevant context (IDs, values, operation attempted)
- Suggest possible causes or remediation
- Don't expose implementation details to end users

---

## Patterns to Follow

### Result Types for Expected Failures

For operations that can fail in expected ways, use result types instead of exceptions.

```typescript
type SearchResult =
  | { found: true; nodes: Node[] }
  | { found: false; reason: 'empty_query' | 'no_matches' };
```

### Early Returns

Reduce nesting by handling edge cases first.

```typescript
// Prefer
function processNode(node: Node | null) {
  if (!node) return null;
  if (node.archived) return null;

  // Main logic at top level
  return transformNode(node);
}
```

### Async/Await Over Promises

Use async/await for readability. Avoid `.then()` chains.

### Immutability by Default

Prefer `const`, spread operators, and pure functions. Mutate only when performance requires it (and document why).

---

## Anti-Patterns to Avoid

- **Boolean blindness**: Use descriptive types instead of `boolean` parameters
- **Stringly typed**: Use enums or literal unions instead of arbitrary strings
- **Callback hell**: Use async/await or proper promise handling
- **Copy-paste inheritance**: Extract shared logic into functions
- **Comments as deodorant**: If code needs extensive comments, rewrite it

---

## See Also

- [Constitution](./constitution.md) - Core principles and mission
- [Architecture Principles](./principles-architecture.md) - Structural guidance
- [Security Principles](./principles-security.md) - Security requirements
- [Implementation Plan](./implementation-plan.md) - Technical approach
