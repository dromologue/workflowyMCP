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
├── index.ts              # Entry point (re-exports MCP server)
├── cli/                  # Command-line interface
│   ├── concept-map.ts    # Standalone concept map generation
│   └── setup.ts          # Interactive credential wizard
├── mcp/
│   └── server.ts         # MCP server with all tool handlers (~2000 lines)
└── shared/               # Shared utilities (used by CLI and MCP)
    ├── api/
    │   ├── workflowy.ts  # Workflowy API client with retry
    │   ├── dropbox.ts    # Dropbox OAuth + image upload
    │   └── retry.ts      # Retry logic with exponential backoff
    ├── config/
    │   └── environment.ts # Environment variables, constants
    ├── types/
    │   └── index.ts      # All TypeScript interfaces
    └── utils/
        ├── cache.ts          # Node caching with TTL
        ├── node-paths.ts     # Breadcrumb path building
        ├── text-processing.ts # Parsing, formatting, link extraction
        └── keyword-extraction.ts # Keywords, relevance scoring
```

### MCP Tools (in server.ts)

| Category | Tools |
|----------|-------|
| Search & Navigation | **find_node**, search_nodes, get_node, get_children, export_all, list_targets, find_insert_targets |
| Content Creation | create_node, insert_content, smart_insert |
| Content Modification | update_node, move_node, delete_node |
| Todo Management | create_todo, list_todos, complete_node, uncomplete_node |
| Knowledge Linking | find_related, create_links |
| Concept Mapping (Legacy) | generate_concept_map |
| **LLM-Powered Concept Mapping** | **get_node_content_for_analysis, render_concept_map** |

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

### ADR-006: Knowledge Linking via Keyword Extraction

**Context**: Users want to discover connections between related content in their Workflowy outline.

**Decision**: Extract keywords from node content, filter stop words, score matches by title vs note occurrence, rank by relevance.

**Algorithm**:
1. Extract keywords: lowercase, remove punctuation, filter words < 3 chars
2. Filter 100+ English stop words (the, and, is, etc.)
3. Score matches: +1 per occurrence, +2 bonus for title matches
4. Rank by total score, return top N results

**Consequences**:
- (+) Simple, predictable matching behavior
- (+) Title matches weighted higher (more intentional)
- (+) No external NLP dependencies
- (-) English-centric stop word list
- (-) No semantic understanding (only keyword matching)

### ADR-007: Concept Map Generation (Legacy)

**Context**: Visual representation of node relationships aids understanding of knowledge structure.

**Decision**: Generate Graphviz DOT format, render via @hpcc-js/wasm-graphviz, convert to PNG/JPEG via Sharp, host on Dropbox for Workflowy embedding.

**Architecture** (keyword-based):
```
Node Content → Keyword Extraction → Related Node Scoring
                                          ↓
DOT Graph ← Edge/Node Building ← Top N Results
    ↓
SVG (Graphviz WASM) → PNG/JPEG (Sharp @ 2400px, 300 DPI)
    ↓
Dropbox Upload → Shareable URL → Workflowy Node Insert
```

**Consequences**:
- (+) No native dependencies (WASM-based)
- (+) High-quality output suitable for embedding
- (+) Optional Dropbox hosting with local fallback
- (-) Requires Dropbox OAuth setup for auto-insert
- (-) Large dependencies (Sharp, Graphviz WASM)
- (-) Keyword matching lacks semantic understanding

### ADR-009: LLM-Powered Concept Mapping

**Context**: The keyword-based concept map approach (ADR-007) requires users to provide concepts upfront and only finds relationships through co-occurrence patterns. This misses the semantic understanding that Claude can provide.

**Decision**: Implement a two-tool workflow where Claude acts as the semantic analyzer:
1. `get_node_content_for_analysis` - Extracts subtree content formatted for LLM analysis
2. `render_concept_map` - Renders Claude's discovered concepts and relationships

**Architecture** (LLM-powered):
```
                          Claude Desktop
                               │
        ┌──────────────────────┼──────────────────────┐
        │                      │                      │
        ▼                      │                      ▼
get_node_content_for_analysis  │           render_concept_map
        │                      │                      │
        ▼                      │                      │
┌───────────────────┐          │         ┌───────────────────┐
│ Workflowy Subtree │          │         │ Claude's Analysis │
│ + Linked Content  │──────────┼────────▶│ - Concepts        │
│ (JSON/Outline)    │   Claude │         │ - Relationships   │
└───────────────────┘  Analyzes│         │ - Hierarchy       │
                               │         └─────────┬─────────┘
                               │                   │
                               │                   ▼
                               │    ┌────────────────────────┐
                               │    │ Graphviz DOT → SVG     │
                               │    │ Sharp → PNG/JPEG       │
                               │    │ Dropbox → Workflowy    │
                               │    └────────────────────────┘
```

**Key Design Decisions**:

1. **Claude as orchestrator**: The MCP server doesn't call Claude API internally. Instead, Claude Desktop coordinates the workflow, calling tools in sequence.

2. **Link following**: `get_node_content_for_analysis` parses Workflowy internal links (`[text](https://workflowy.com/#/node-id)`) and includes linked content to capture cross-references.

3. **Structured output**: Content returned as JSON with depth, path, and link metadata so Claude can understand hierarchy and connections.

4. **Semantic relationships**: Claude discovers relationship types ("produces", "critiques", "enables") through understanding, not regex pattern matching.

**Consequences**:
- (+) Leverages Claude's semantic understanding for concept discovery
- (+) No need to provide concepts upfront - Claude finds them
- (+) Meaningful relationship labels based on actual content meaning
- (+) Follows links to include cross-referenced content
- (+) Same high-quality visual output as legacy approach
- (-) Requires two tool calls instead of one
- (-) Depends on Claude's analysis quality
- (-) Cannot be used without an LLM orchestrator

### ADR-008: Retry Logic with Exponential Backoff

**Context**: Transient API failures should be retried automatically.

**Decision**: Implement retry wrapper with configurable attempts, exponential backoff, and jitter.

**Configuration**:
- Max attempts: 3
- Base delay: 1000ms
- Max delay: 10000ms
- Retryable: 429, 500, 502, 503, 504
- Non-retryable: 4xx (except 429)

**Consequences**:
- (+) Transparent retry for transient failures
- (+) Rate limit (429) handled gracefully
- (+) Jitter prevents thundering herd
- (-) Increased latency on failures
- (-) May mask persistent issues if not logged

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
