# Implementation Plan

> Technical approach for building and maintaining the Workflowy MCP Server.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                     Claude Desktop                          │
│                         (Client)                            │
└─────────────────────────┬───────────────────────────────────┘
                          │ stdio (JSON-RPC)
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                    MCP Server (Node.js)                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │   Tools     │  │   Cache     │  │   Config/Env        │ │
│  │  Registry   │  │  (30s TTL)  │  │   (.env)            │ │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘ │
│         │                │                     │            │
│         └────────────────┼─────────────────────┘            │
│                          ▼                                  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │              Workflowy API Client                     │  │
│  │  - Request builder (auth, headers)                    │  │
│  │  - Response parser                                    │  │
│  │  - Error handling + retry logic                       │  │
│  └──────────────────────────┬───────────────────────────┘  │
└─────────────────────────────┼───────────────────────────────┘
                              │ HTTPS
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                  Workflowy REST API                         │
│                 (workflowy.com/api/v1)                      │
└─────────────────────────────────────────────────────────────┘
```

## Technology Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Runtime | Node.js 18+ | Required for MCP SDK, native fetch |
| Language | TypeScript 5.x | Type safety, IDE support |
| MCP SDK | @modelcontextprotocol/sdk | Official, maintained |
| Validation | Zod 3.25+ | Runtime type checking, schema inference |
| Config | dotenv | Simple, standard, secure |
| Transport | stdio | Claude Desktop compatibility |

## Module Structure

```
src/
├── index.ts              # Entry point, server setup
├── api/
│   ├── client.ts         # Workflowy API wrapper
│   ├── types.ts          # API response types
│   └── retry.ts          # Retry logic with backoff
├── tools/
│   ├── search.ts         # search_nodes, find_insert_targets
│   ├── navigation.ts     # get_node, get_children, list_targets
│   ├── creation.ts       # create_node, insert_content, smart_insert
│   ├── modification.ts   # update_node, move_node, delete_node
│   └── completion.ts     # complete_node, uncomplete_node
├── utils/
│   ├── cache.ts          # Node caching with TTL
│   ├── hierarchy.ts      # Indentation parsing
│   └── paths.ts          # Breadcrumb path building
└── config/
    └── env.ts            # Environment variable handling
```

## Key Design Decisions

### ADR-001: Local Search Over API

**Context**: Workflowy has no search API endpoint.

**Decision**: Export all nodes, cache locally, search in-memory.

**Consequences**:
- (+) Fast repeated searches
- (+) Complex query support possible
- (-) Stale data between cache refreshes
- (-) Memory usage scales with outline size

### ADR-002: Hierarchical Insertion via Indentation

**Context**: Need to preserve structure when inserting multi-line content.

**Decision**: Parse indentation (2 spaces or 1 tab = 1 level), create nodes with parent references.

**Consequences**:
- (+) Simple, predictable behavior
- (+) Works with any text editor output
- (-) No markdown heading hierarchy
- (-) Mixed indent styles may confuse

### ADR-003: Smart Insert Workflow

**Context**: Users don't know node IDs, need to find insertion targets.

**Decision**: Combine search + insert into single tool with selection flow.

**Consequences**:
- (+) Single tool for common operation
- (+) Handles disambiguation gracefully
- (-) Multi-turn interaction for multiple matches
- (-) More complex tool implementation

### ADR-004: Cache Invalidation on Write

**Context**: Writes make cached node list stale.

**Decision**: Invalidate entire cache on any write operation.

**Consequences**:
- (+) Guaranteed consistency after writes
- (-) Performance penalty for write-heavy workflows
- (-) Could be more granular (future optimization)

### ADR-005: Bottom-Default Insertion Order

**Context**: Workflowy's "top" position inserts each node above the previous, reversing multi-node content order.

**Decision**: Default to "bottom" position for all insertions. When "top" is explicitly requested, only the first top-level node is placed at top; subsequent nodes use "bottom" to preserve order.

**Consequences**:
- (+) Content appears in correct order (as written)
- (+) Hierarchical content maintains parent-child relationships
- (+) "top" still places content block at top of parent
- (-) Multiple top-level nodes with "top" won't all be at very top
- (-) Slight semantic change from raw API behavior

## Error Handling Strategy

```typescript
// Retry configuration
const RETRY_CONFIG = {
  maxAttempts: 3,
  baseDelay: 1000,      // 1 second
  maxDelay: 10000,      // 10 seconds
  retryableStatuses: [429, 500, 502, 503, 504],
};

// Error categories
type ErrorCategory =
  | 'auth'          // 401, 403 - surface immediately
  | 'not_found'     // 404 - surface with helpful message
  | 'rate_limit'    // 429 - retry with backoff
  | 'server_error'  // 5xx - retry with backoff
  | 'network'       // Connection failed - retry
  | 'validation';   // Bad request - surface with details
```

## Security Implementation

### Credential Handling

```typescript
// Load from environment only
const API_KEY = process.env.WORKFLOWY_API_KEY;
if (!API_KEY) {
  console.error("WORKFLOWY_API_KEY required");
  process.exit(1);
}

// Never log credentials
function sanitizeForLogging(obj: unknown): unknown {
  // Recursively remove sensitive fields
}
```

### Content Protection

- No user content in error messages
- No node names/notes in logs
- Audit log for delete operations (future)

## Testing Strategy

### Unit Tests

- Indentation parser: edge cases, mixed styles
- Path builder: deep nesting, special characters
- Cache: TTL expiration, invalidation

### Integration Tests

- API client: mock server responses
- Tool handlers: end-to-end with mocked API
- Error scenarios: network failures, rate limits

### Test Infrastructure

```bash
# Run all tests
npm test

# Coverage report
npm run test:coverage

# Watch mode
npm run test:watch
```

## Deployment

### Build Process

```bash
npm run build      # TypeScript → JavaScript
npm run start      # Run built server
```

### Claude Desktop Configuration

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "node",
      "args": ["/path/to/dist/index.js"]
    }
  }
}
```

## Monitoring & Observability

### Logging Levels

- `error`: Unrecoverable failures
- `warn`: Retried operations, rate limits hit
- `info`: Tool invocations, cache events
- `debug`: API requests/responses (sanitized)

### Health Indicators

- API connectivity (test on startup)
- Cache hit rate
- Average response time

## Migration Path

### v1.x → v2.x (Future)

If breaking changes needed:
1. Deprecation warnings in v1.x final
2. Migration guide in changelog
3. Coexistence period (both versions)
4. v1.x end-of-life announcement
