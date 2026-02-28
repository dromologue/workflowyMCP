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
│   └── server.ts         # MCP server with all tool handlers (~4050 lines)
└── shared/               # Shared utilities (used by CLI and MCP)
    ├── api/
    │   ├── workflowy.ts  # Workflowy API client with retry
    │   └── retry.ts      # Retry logic with exponential backoff
    ├── config/
    │   └── environment.ts # Environment variables, constants
    ├── types/
    │   └── index.ts      # All TypeScript interfaces
    └── utils/
        ├── cache.ts               # Node caching with TTL
        ├── concept-map-html.ts    # Interactive HTML concept map generator
        ├── date-parser.ts         # Due date parsing from node text
        ├── jobQueue.ts            # Background job processing
        ├── keyword-extraction.ts  # Keywords, relevance scoring
        ├── large-markdown-converter.ts # Markdown → Workflowy format
        ├── node-paths.ts          # Breadcrumb path building
        ├── orchestrator.ts        # Parallel insertion orchestration
        ├── rateLimiter.ts         # Token bucket rate limiter
        ├── requestQueue.ts        # Concurrent request queue
        ├── scope-utils.ts         # Subtree traversal, scope filtering
        ├── subtree-parser.ts      # Content splitting for parallel insertion
        ├── tag-parser.ts          # Tag and assignee extraction
        └── text-processing.ts     # Parsing, formatting, link extraction
```

### MCP Tools (in server.ts)

| Category | Tools |
|----------|-------|
| Search & Navigation | **find_node**, search_nodes (with filters), get_node, get_children, **find_backlinks** |
| Content Creation | create_node, insert_content, smart_insert, convert_markdown_to_workflowy |
| Content Modification | update_node, move_node, delete_node, **duplicate_node**, **create_from_template**, **bulk_update** |
| Todo Management | create_todo, list_todos, complete_node, uncomplete_node, **list_upcoming**, **list_overdue** |
| Project Management | **get_project_summary**, **get_recent_changes**, **daily_review** |
| Knowledge Linking | find_related, create_links |
| Concept Mapping | get_node_content_for_analysis, **render_interactive_concept_map** (MCP Apps) |
| Graph Analysis | **analyze_relationships**, **create_adjacency_matrix**, **calculate_centrality**, **analyze_network_structure** |
| Batch & Async | batch_operations, submit_job, get_job_status, list_jobs, cancel_job |
| File Insertion | insert_file, submit_file_job |

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

### ADR-010: Staging Node Pattern for Content Insertion

**Context**: When inserting multiple nodes, concurrent API calls could cause nodes to briefly appear at unintended locations (like the Workflowy root) during the operation, before being moved to their correct parents. This creates visual clutter and confuses users.

**Decision**: Use a staging node pattern for hierarchical content insertion:
1. Create a temporary staging node (`__staging_temp__`) under the target parent
2. Create all hierarchical content inside the staging node
3. Move top-level children from staging to the actual parent
4. Delete the staging node

**Implementation**:
```
Target Parent
    └── __staging_temp__  ← 1. Create staging
            └── Content A ← 2. Create content here
            └── Content B
                └── Child B1

Target Parent             ← 3. Move top-level nodes
    └── Content A
    └── Content B
        └── Child B1
    (staging deleted)     ← 4. Delete staging
```

**Consequences**:
- (+) Nodes never appear at root or wrong location during insertion
- (+) Clean user experience - content appears atomically in correct location
- (+) Error recovery - staging node cleanup on failure
- (+) Works with both "top" and "bottom" position (reverse move order for "top")
- (-) Additional API calls (1 create + N moves + 1 delete)
- (-) Slightly increased latency for large insertions

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

### ADR-009: LLM-Powered Concept Mapping

**Context**: Users want to visualize conceptual relationships in their Workflowy content. Claude's semantic understanding can discover meaningful concepts and relationships that keyword matching cannot.

**Decision**: Implement a two-tool workflow where Claude acts as the semantic analyzer:
1. `get_node_content_for_analysis` - Extracts subtree content formatted for LLM analysis
2. `render_interactive_concept_map` - Renders Claude's discovered concepts and relationships as interactive HTML

**Architecture** (LLM-powered):
```
                          Claude Desktop
                               │
        ┌──────────────────────┼──────────────────────┐
        │                      │                      │
        ▼                      │                      ▼
get_node_content_for_analysis  │    render_interactive_concept_map
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
                               │    │ Interactive HTML + SVG  │
                               │    │ MCP Apps protocol       │
                               │    │ Inline in conversation  │
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
- (+) Interactive HTML output with zoom, pan, and collapse/expand
- (+) No external dependencies (no Dropbox, no image hosting)
- (-) Requires two tool calls instead of one
- (-) Depends on Claude's analysis quality
- (-) Cannot be used without an LLM orchestrator
- (-) Requires Claude Desktop MCP Apps support

### ADR-011: Tag/Assignee/Date Parsing from Node Text

**Context**: Workflowy has no native tag, assignee, or due date fields. Users encode this metadata inline in node text using conventions like `#tag`, `@person`, and `due:YYYY-MM-DD`.

**Decision**: Parse tags, assignees, and due dates from node text using regex. Provide reusable utility modules (`tag-parser.ts`, `date-parser.ts`) used by search filters, project summary, daily review, and bulk update tools.

**Parsing rules**:
- Tags: `/#([\w-]+)/g` — extracted from name and note, lowercased, deduplicated
- Assignees: `/@([\w-]+)/g` — same treatment
- Due dates (priority order): `due:YYYY-MM-DD` > `#due-YYYY-MM-DD` > bare `YYYY-MM-DD`

**Consequences**:
- (+) Works with existing Workflowy data without schema changes
- (+) Consistent parsing across all tools
- (-) Relies on user conventions — non-standard formats ignored
- (-) Bare dates may false-positive on unrelated date strings

### ADR-012: Interactive Concept Maps via MCP Apps

**Context**: Static PNG/JPEG concept maps cannot be explored interactively. Users want to collapse/expand concept clusters and zoom into areas of interest.

**Decision**: Implement interactive concept maps using the MCP Apps protocol extension. The `render_interactive_concept_map` tool declares a `_meta.ui.resourceUri` pointing to a `ui://` HTML resource. The server handles `ListResources` and `ReadResource` to serve self-contained HTML with SVG + vanilla JS.

**Architecture**:
```
Tool declaration → _meta.ui.resourceUri: "ui://concept-map/interactive"
Tool call        → generates HTML, stores in module-level variable
Host (Claude)    → fetches ui:// resource via ReadResource
                 → renders in sandboxed iframe inline in conversation
```

**Key decisions**:
1. Manual MCP Apps protocol on low-level `Server` class (avoids migration to `McpServer`)
2. Self-contained HTML (~20KB, no external dependencies)
3. Radial layout with collapsible major/detail clusters
4. Module-level state for last-generated map (simple, stateless per tool call)

**Consequences**:
- (+) Interactive exploration of concept relationships
- (+) No external dependencies or image hosting needed
- (+) Works alongside existing static maps (separate tool)
- (-) Requires Claude Desktop MCP Apps support
- (-) Module-level state means only one interactive map at a time

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
