# Tasks

> Actionable work items derived from the specification and implementation plan.

## Status Legend

- [ ] Not started
- [~] In progress
- [x] Complete

---

## Phase 1: Foundation Compliance

*Align current implementation with constitution requirements.*

### Code Quality

- [ ] **T-001**: Enable strict TypeScript mode
  - Add `"strict": true` to tsconfig.json (already present)
  - Audit for `any` types and eliminate
  - Add explicit return types to all functions

- [ ] **T-002**: Add JSDoc documentation
  - Document all exported functions
  - Document all tool schemas
  - Document configuration options

### Security Hardening

- [ ] **T-003**: Implement secure logging
  - Create sanitization utility for logs
  - Ensure no API keys in any log output
  - Ensure no user content in error messages

- [ ] **T-004**: Add audit logging for destructive operations
  - Log delete operations to stderr (no content, just node ID + timestamp)
  - Log move operations

### Error Handling

- [x] **T-005**: Implement retry logic with exponential backoff
  - Create retry utility with configurable attempts
  - Handle 429 (rate limit) with backoff
  - Handle 5xx errors with retry
  - Surface 4xx errors immediately

- [ ] **T-006**: Improve error messages
  - Map API errors to user-friendly messages
  - Include suggested actions in error responses
  - Never expose raw API errors to users

---

## Phase 2: Testing Infrastructure

*Establish comprehensive test coverage per constitution.*

### Setup

- [x] **T-007**: Configure test framework
  - Add Vitest with TypeScript support
  - Configure coverage reporting (v8)
  - Add test scripts: test, test:watch, test:coverage

### Unit Tests

- [ ] **T-008**: Test indentation parser
  - Spaces (2, 4 per level)
  - Tabs
  - Mixed indentation
  - Empty lines
  - Deep nesting (10+ levels)

- [ ] **T-009**: Test path builder
  - Root nodes
  - Deeply nested nodes
  - Nodes with long names (truncation)
  - Missing parent references

- [ ] **T-010**: Test cache utility
  - TTL expiration
  - Manual invalidation
  - Concurrent access

### Integration Tests

- [ ] **T-011**: Test API client with mocks
  - Successful responses
  - Error responses (401, 404, 429, 500)
  - Network failures
  - Timeout handling

- [ ] **T-012**: Test tool handlers end-to-end
  - search_nodes
  - smart_insert (single match, multiple matches, selection)
  - insert_content (flat, hierarchical)
  - CRUD operations

---

## Phase 3: Refactoring

*Restructure code per implementation plan.*

### Module Extraction

- [ ] **T-013**: Extract API client module
  - Move to `src/api/client.ts`
  - Add types to `src/api/types.ts`
  - Implement retry logic in `src/api/retry.ts`

- [ ] **T-014**: Extract tool modules
  - `src/tools/search.ts`
  - `src/tools/navigation.ts`
  - `src/tools/creation.ts`
  - `src/tools/modification.ts`

- [ ] **T-015**: Extract utility modules
  - `src/utils/cache.ts`
  - `src/utils/hierarchy.ts`
  - `src/utils/paths.ts`

### Configuration

- [ ] **T-016**: Implement configurable settings
  - Cache TTL (env: `WORKFLOWY_CACHE_TTL`)
  - Retry attempts (env: `WORKFLOWY_RETRY_ATTEMPTS`)
  - Rate limit behavior (env: `WORKFLOWY_RATE_LIMIT_MODE`)

---

## Phase 4: Documentation

*Developer-focused documentation per constitution.*

### Code Documentation

- [x] **T-017**: README with setup instructions
- [ ] **T-018**: Add CONTRIBUTING.md
  - Development setup
  - Code style guide
  - PR process
  - Testing requirements

- [ ] **T-019**: Add CHANGELOG.md
  - Document all releases
  - Breaking changes highlighted
  - Migration notes

### Architecture Documentation

- [ ] **T-020**: Document ADRs in specs/
  - ADR-001: Local search strategy
  - ADR-002: Hierarchical insertion
  - ADR-003: Smart insert workflow
  - ADR-004: Cache invalidation
  - ADR-005: Bottom-default insertion order

---

## Phase 5: Reliability

*Production-readiness improvements.*

### Resilience

- [ ] **T-021**: Implement graceful degradation
  - Handle API unavailable state
  - Return cached data with staleness warning
  - Queue writes for retry (optional)

- [ ] **T-022**: Add startup health check
  - Verify API connectivity
  - Validate credentials
  - Warn on configuration issues

### Observability

- [ ] **T-023**: Add structured logging
  - Log levels (error, warn, info, debug)
  - Consistent format
  - Timestamps

---

## Backlog

*Future considerations, not committed.*

- [ ] **B-003**: Offline queue for unreachable API
- [ ] **B-004**: Conflict detection for concurrent edits
- [ ] **B-005**: Support for Workflowy mirrors/live copies
- [ ] **B-006**: Recurring task support (repeat rules for todos)
- [ ] **B-007**: Cross-outline collaboration

---

## Completed

*Reference for done work.*

- [x] **T-000**: Initial implementation
  - Basic MCP server structure
  - All core tools implemented
  - Hierarchical insertion
  - Smart insert workflow
  - README documentation

- [x] **T-024**: Fix insertion order (content appearing reversed)
  - Default to "bottom" position for correct order
  - "top" only applies to first top-level node
  - Hierarchical content maintains parent-child relationships
  - ADR-005 documented

- [x] **T-025**: Add todo management tools
  - `create_todo`: Create checkbox items with completion state
  - `list_todos`: Filter by status, parent, search query
  - Works with existing `complete_node`/`uncomplete_node`
  - Detects todos by layoutMode or markdown checkbox syntax

- [x] **T-026**: Add knowledge linking tools
  - `find_related`: Extract keywords, find related nodes by relevance score
  - `create_links`: Generate internal Workflowy links to related content
  - Keyword extraction with stop word filtering
  - Link placement options: child node or note appendage
  - Auto-discovery of connections based on content analysis

- [x] **T-027**: Add visual concept map generation
  - `generate_concept_map`: Create PNG/JPEG graph of node relationships
  - Graphviz WASM for graph rendering (no native dependencies)
  - Sharp for SVG to PNG/JPEG conversion
  - Center node with related nodes arranged by relevance
  - Edge labels show matching keywords
  - Edge width indicates connection strength
  - Optimized for readability with clear colors and labels

- [x] **T-028**: Add direct image insertion into Workflowy
  - Integrate imgbb API for image hosting
  - `insert_into` parameter to specify target node
  - Automatic upload and node creation with image URL
  - Markdown image syntax for inline display
  - Fallback to local save if no API key configured
  - Optional `save_locally` parameter for local backup

- [x] **T-029**: Switch to Dropbox for image hosting
  - Replace imgbb with Dropbox API integration
  - OAuth 2.0 with refresh token for long-lived access
  - Automatic access token refresh with caching
  - Images stored in `/workflowy/conceptMaps/` folder
  - Shareable links generated automatically

- [x] **T-030**: Add search scope to concept map
  - `scope` parameter: this_node, children, siblings, ancestors, all
  - Filter nodes before keyword matching
  - Default scope: all (entire Workflowy)
  - Scope included in response and inserted node note

- [x] **T-031**: Improve concept map reliability
  - Increase resolution to 2400px width, 300 DPI
  - Add defensive checks for scope filtering (Array.isArray, depth limits)
  - Fix allNodes.find error (API returns {nodes:[]} not array)

- [x] **T-032**: Auto-insert concept maps into Workflowy
  - Re-enable Dropbox integration for image hosting
  - Auto-create child node in source node with concept map image
  - Node includes markdown image, scope info, and keywords
  - Fallback to local ~/Downloads/ if Dropbox not configured

- [x] **T-033**: Add find_node tool for fast node lookup
  - Fast node identification by exact name
  - Support for exact, contains, and starts_with match modes
  - Duplicate handling with numbered options and selection parameter
  - Returns node_id ready for use with other tools
  - Full test coverage (28 unit tests)

- [x] **T-034**: Add task & knowledge management utility modules
  - `tag-parser.ts`: Extract #tags and @mentions from node text (21 tests)
  - `date-parser.ts`: Parse due dates in 3 formats with priority order (22 tests)
  - `scope-utils.ts`: Subtree traversal, scope filtering, children index (13 tests)
  - Extracted `filterNodesByScope` from server.ts to shared utility

- [x] **T-035**: Enhance search_nodes with structured filters
  - Added optional parameters: tag, assignee, status, root_id, scope, modified_after, modified_before
  - Filter pipeline: scope → text → tag → assignee → status → date range
  - Enriched JSON output with tags, assignees, due dates when filters present
  - 12 unit tests for filter pipeline

- [x] **T-036**: Add project management tools
  - `get_project_summary`: Stats, tag counts, assignees, overdue items for a subtree (12 tests)
  - `get_recent_changes`: Nodes modified within a time window (9 tests)

- [x] **T-037**: Add due date and scheduling tools
  - `list_upcoming`: Todos due in next N days, sorted by urgency (19 tests, shared with list_overdue)
  - `list_overdue`: Past-due items sorted by most overdue first
  - `daily_review`: One-call standup summary (14 tests)

- [x] **T-038**: Add knowledge linking tools
  - `find_backlinks`: Find all nodes linking to a given node (9 tests)

- [x] **T-039**: Add content reuse tools
  - `duplicate_node`: Deep-copy a node and its subtree (4 tests)
  - `create_from_template`: Copy template with `{{variable}}` substitution (8 tests)

- [x] **T-040**: Add bulk update tool
  - Filter by query, tag, assignee, status, root_id
  - Operations: complete, uncomplete, delete, add_tag, remove_tag
  - Dry-run mode and configurable safety limit (11 tests)

- [x] **T-041**: Add interactive concept maps via MCP Apps
  - `render_interactive_concept_map` tool with `_meta.ui.resourceUri`
  - Self-contained HTML generator (`concept-map-html.ts`) with SVG + vanilla JS
  - Collapsible major/detail clusters, zoom/pan, expand/collapse all
  - MCP resource handlers (ListResources, ReadResource) for serving HTML
  - Auto-assignment of unparented detail concepts to most-connected major
  - 13 unit tests for HTML generator

- [x] **T-042**: Legacy cleanup and code removal
  - Removed 5 redundant tools: generate_concept_map, render_concept_map, find_insert_targets, export_all, list_targets
  - Removed 7 dead functions/constants from server.ts
  - Removed legacy types from shared/types
  - Removed legacy tests (15 tests)
  - Server.ts reduced from ~4800 to ~3780 lines
- [x] **T-043**: Graph-Tools consolidation
  - Ported 7 graph algorithms from ~/code/Graph-Tools/mcp-graph-server/ to TypeScript
  - Created src/shared/utils/graph-analysis.ts (~280 lines, zero external dependencies)
  - Created src/shared/utils/graph-analysis.test.ts (31 tests)
  - Added 4 MCP tools: analyze_relationships, create_adjacency_matrix, calculate_centrality, analyze_network_structure
  - Eliminated Ruby CLI, D3.js visualization, Express server, and file I/O dependencies
  - Users now run one MCP server instead of two
