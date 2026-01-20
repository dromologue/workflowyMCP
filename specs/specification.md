# Specification

> What the Workflowy MCP Server does and why.

## Overview

The Workflowy MCP Server is a Model Context Protocol server that enables Claude (and other MCP-compatible AI assistants) to read, search, and write to a user's Workflowy outline. It transforms Workflowy into an AI-accessible knowledge base and capture system.

## User Personas

### Primary: Knowledge Workers

- Use Workflowy as their primary thinking/planning tool
- Want to capture AI-generated insights directly into their outline
- Need to reference their existing notes during AI conversations

### Secondary: Developers

- Building AI workflows that require persistent structured storage
- Integrating Workflowy into automation pipelines
- Extending Claude's capabilities with external memory

## Core Capabilities

### 1. Search & Discovery

**Goal**: Find relevant nodes quickly without knowing exact structure.

| Feature | Description |
|---------|-------------|
| **Fast node lookup** | Find nodes by exact name with duplicate handling |
| Text search | Search node names and notes by keyword |
| Path display | Show full breadcrumb path for disambiguation |
| Target listing | Access Workflowy shortcuts (inbox, starred) |
| Full export | Retrieve entire outline for comprehensive analysis |

**Success criteria**: User can locate any node in <2 tool calls.

---

#### find_node Tool

Fast node lookup by name that returns the node ID ready for use with other tools. Designed for when Claude needs to quickly identify a specific node.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | The name of the node to find |
| `match_mode` | "exact" \| "contains" \| "starts_with" | no | How to match (default: "exact") |
| `selection` | number | no | If multiple matches, the 1-based index to select |

**Match modes**:
- `exact`: Node name must exactly match (case-insensitive)
- `contains`: Node name contains the search term
- `starts_with`: Node name starts with the search term

**Behavior**:
1. **Single match**: Returns node ID, name, path, and note directly
2. **Multiple matches**: Returns numbered options with paths for disambiguation
3. **With selection**: Returns the specific node from the match list

**Response (single match)**:
```json
{
  "found": true,
  "node_id": "abc123",
  "name": "Project Ideas",
  "path": "Work > Projects > Project Ideas",
  "note": "My project notes...",
  "message": "Single match found. Use node_id with other tools."
}
```

**Response (multiple matches)**:
```json
{
  "found": true,
  "multiple_matches": true,
  "count": 3,
  "message": "Found 3 nodes named 'Ideas'. Which one do you mean?",
  "options": [
    {"option": 1, "name": "Ideas", "path": "Work > Ideas", "id": "abc"},
    {"option": 2, "name": "Ideas", "path": "Personal > Ideas", "id": "def"},
    {"option": 3, "name": "Ideas", "path": "Archive > Ideas", "id": "ghi"}
  ],
  "usage": "Call find_node again with selection: <number> to get the node_id"
}
```

**Use case**: When Claude needs to find a node by name to use its ID with other tools (insert_content, get_children, create_links, etc.)

---

### 2. Navigation & Retrieval

**Goal**: Traverse and read the outline structure.

| Feature | Description |
|---------|-------------|
| Get node | Retrieve single node by ID with metadata |
| List children | Get immediate children of any node |
| Root listing | Access top-level nodes |

**Success criteria**: Any node accessible with known ID or parent reference.

### 3. Content Creation

**Goal**: Add new information to the outline.

| Feature | Description |
|---------|-------------|
| Create node | Add node with name, note, parent, position |
| Hierarchical insert | Parse indented content into nested structure |
| Smart insert | Search-and-insert workflow with selection |
| Markdown support | Headers, todos, code blocks, quotes |
| Order preservation | Content appears in same order as provided |

**Position behavior**:
- `bottom` (default): Content appended after existing children, order preserved
- `top`: First node placed at top, subsequent nodes follow in order

**Success criteria**: Claude-generated content appears in Workflowy with correct structure and order.

### 4. Todo Management

**Goal**: Create and manage task lists within Workflowy.

| Feature | Description |
|---------|-------------|
| Create todo | Create a checkbox item with optional initial completion state |
| List todos | Retrieve all todos with filtering by status, parent, search |
| Complete/Uncomplete | Toggle completion status of any node |

**Todo identification**:
- Nodes with `layoutMode: "todo"`
- Nodes using markdown checkbox syntax (`- [ ]` or `- [x]`)

**Filtering options**:
- `status`: "all", "pending", or "completed"
- `parent_id`: Scope to todos under a specific node
- `query`: Text search within todo names/notes

**Success criteria**: Full task management workflow without leaving Claude.

### 5. Knowledge Linking

**Goal**: Discover and create connections between related content.

| Feature | Description |
|---------|-------------|
| Find related | Analyze node content, extract keywords, find matching nodes |
| Create links | Generate Workflowy internal links to related nodes |
| Auto-discovery | Automatically find relevant connections based on content |
| Concept map (legacy) | Generate visual graph using keyword matching |
| **LLM-powered concept map** | Multi-tool workflow for semantic concept discovery |

**Keyword extraction**:
- Filters common stop words
- Prioritizes significant terms (3+ characters)
- Scores matches by title vs note occurrence

**Link placement options**:
- `child`: Creates a "🔗 Related" child node with links (default)
- `note`: Appends links to the node's existing note

**Link format**: `[Node Title](https://workflowy.com/#/nodeId)`

---

#### LLM-Powered Concept Mapping (Recommended)

The LLM-powered approach uses Claude's semantic understanding to discover meaningful conceptual relationships, rather than mechanical keyword matching.

**Two-tool workflow**:

1. **`get_node_content_for_analysis`**: Extracts subtree content formatted for LLM analysis
2. **`render_concept_map`**: Renders Claude's discovered concepts and relationships

**Why this approach**:
- Claude reads and **understands** the content semantically
- Claude **discovers** concepts through reasoning, not keyword matching
- Claude identifies **meaningful relationships** from context
- Relationship labels reflect actual semantic connections ("critiques", "extends", "enables")

**Tool 1: `get_node_content_for_analysis`**

Extracts content from a Workflowy subtree, including linked content.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `node_id` | string | required | Root node to analyze |
| `depth` | number | unlimited | Maximum depth to traverse |
| `include_notes` | boolean | true | Include node notes |
| `max_nodes` | number | 500 | Maximum nodes to return |
| `follow_links` | boolean | true | Follow internal Workflowy links |
| `format` | "structured" \| "outline" | "structured" | Output format |

**Link following**: Automatically parses Workflowy internal links (`[text](https://workflowy.com/#/node-id)`) and includes linked content from outside the immediate hierarchy. This enables discovery of cross-references and connections.

**Output (structured format)**:
```json
{
  "root": { "id": "...", "name": "Topic", "note": "..." },
  "total_nodes": 47,
  "total_chars": 23456,
  "truncated": false,
  "linked_nodes_included": 5,
  "content": [
    {
      "depth": 0,
      "id": "node1",
      "name": "Child Topic",
      "note": "Detailed notes...",
      "path": "Topic > Child Topic",
      "links_to": ["node5", "node8"]
    }
  ],
  "linked_content": [
    {
      "depth": -1,
      "id": "node5",
      "name": "Referenced Topic",
      "note": "Content from linked node...",
      "path": "Other Section > Referenced Topic"
    }
  ]
}
```

**Tool 2: `render_concept_map`**

Renders a visual concept map from Claude's semantic analysis.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `title` | string | yes | Map title |
| `core_concept` | object | yes | Central concept (`{label, description?}`) |
| `concepts` | array | yes | Discovered concepts (2-35) |
| `relationships` | array | yes | Relationships between concepts |
| `output` | object | no | Format, insertion target, output path |

**Concept structure**:
```json
{
  "id": "truth-procedure",
  "label": "Truth Procedure",
  "level": "major",  // or "detail"
  "importance": 8,   // 1-10, affects node size
  "description": "Optional description"
}
```

**Relationship structure**:
```json
{
  "from": "core",  // or concept id
  "to": "truth-procedure",
  "type": "produces",  // semantic relationship
  "strength": 9,   // 1-10, affects edge weight
  "evidence": "Brief quote showing relationship"
}
```

**Common relationship types**:
- `produces`, `enables`, `requires` (causal/dependency)
- `critiques`, `extends`, `develops` (evaluative)
- `contrasts with`, `differs from` (comparative)
- `includes`, `examples of`, `type of` (hierarchical)
- `influences`, `relates to` (general)

---

#### Legacy Concept Map (Keyword-Based)

The original `generate_concept_map` tool uses keyword matching. It requires the user to provide concepts upfront and finds relationships through co-occurrence and pattern matching.

**Parameters**:
- `node_id`: Parent node whose children will be analyzed
- `core_concept`: The central concept (defaults to parent node name)
- `concepts`: List of concepts/terms to map (required, minimum 2, maximum 35)
- `scope`: Search scope for content analysis (default: children)
- `format`: PNG (default) or JPEG
- `title`: Custom title for the map

**Limits**:
- Maximum 35 concepts per map (prevents oversized graphs that fail to render)
- Maximum 5,000 nodes analyzed for edge building (prevents timeout on large datasets)
- Maximum 1,000 unique edges per map (prevents memory exhaustion)
- For larger concept sets, split into multiple focused maps by theme/category

---

#### Visual Encoding (Both Approaches)

- **Node levels**: Core (dark blue, large) → Major (medium colors) → Details (lighter colors, smaller)
- **Node size**: Larger = more important/frequent
- **Edge labels**: Relationship type
- **Edge colors**: Green = supporting, Red dashed = contrasting, Purple = dependency, Gray = general

**Output**:
- Square aspect ratio (2000x2000 max, 300 DPI) for balanced visual layout
- Unicode support for accented characters (French, German, etc.)
- Auto-insert into source node via Dropbox image hosting
- Fallback: save locally to `~/Downloads/` if Dropbox not configured

**Image hosting** (Dropbox):
- Requires Dropbox OAuth configuration (app key, secret, refresh token)
- Images stored in `/workflowy/conceptMaps/` folder
- Shareable links generated automatically
- Concept maps inserted as child nodes with markdown image syntax

**Success criteria**: Surface relevant connections user might not have noticed.

### 6. Content Modification

**Goal**: Update existing nodes.

| Feature | Description |
|---------|-------------|
| Update node | Change name and/or note |
| Move node | Relocate to different parent |
| Complete/Uncomplete | Toggle task completion status |
| Delete node | Permanent removal |

**Success criteria**: All CRUD operations available and reversible (except delete).

## User Flows

### Flow 1: Capture AI Output

```
User: "Summarize this article and add it to my Research node"

1. Claude generates summary
2. smart_insert searches for "Research"
3. If multiple matches → return numbered options
4. User selects → content inserted with hierarchy preserved
5. Confirmation with target path shown
```

### Flow 2: Reference Existing Notes

```
User: "What did I write about project planning?"

1. search_nodes for "project planning"
2. Results show paths: "Work > Projects > Planning Guide"
3. get_node retrieves full content
4. Claude uses content to inform response
```

### Flow 3: Task Management

```
User: "Mark my weekly review tasks as complete"

1. search_nodes for "weekly review"
2. get_children to list tasks
3. complete_node for each task
4. Confirmation of completed items
```

### Flow 4: Visualize Knowledge Connections (LLM-Powered)

```
User: "Create a concept map of my Badiou philosophy notes"

1. Claude calls get_node_content_for_analysis to retrieve subtree content
   - All child nodes with names, notes, paths returned
   - Internal Workflowy links are followed to include connected content

2. Claude reads and semantically analyzes the content:
   - Discovers key concepts: Event, Truth, Subject, Fidelity, Situation
   - Identifies relationships from context:
     * "Event produces Truth" (found in: "The Event ruptures the situation...")
     * "Subject constituted by Fidelity" (found in: "...the Subject emerges through...")
     * "Badiou critiques Deleuze" (found in: "...Badiou's critique of immanence...")

3. Claude calls render_concept_map with discovered analysis:
   {
     "title": "Badiou's Event Philosophy",
     "core_concept": { "label": "Event" },
     "concepts": [
       { "id": "truth", "label": "Truth", "level": "major", "importance": 9 },
       { "id": "subject", "label": "Subject", "level": "major", "importance": 8 },
       { "id": "fidelity", "label": "Fidelity", "level": "detail", "importance": 6 }
     ],
     "relationships": [
       { "from": "core", "to": "truth", "type": "produces", "strength": 9 },
       { "from": "subject", "to": "fidelity", "type": "constituted by", "strength": 8 }
     ],
     "output": { "insert_into_workflowy": "abc123" }
   }

4. Tool renders Graphviz visualization and uploads to Dropbox
5. Concept map inserted as child node with image and summary

Key difference from legacy approach:
- Claude DISCOVERS concepts through understanding, not keyword matching
- Relationships are semantically meaningful, not pattern-matched
- No need to provide concepts upfront - Claude finds them
```

### Flow 4b: Visualize Knowledge Connections (Legacy Keyword-Based)

```
User: "Create a concept map of my philosophy notes showing how Heidegger, Dewey,
       phenomenology, and pragmatism relate"

1. User provides the parent node and list of concepts
2. generate_concept_map scans all children for concept occurrences
3. Tool extracts relationship labels from context (e.g., "Heidegger critiques pragmatism")
4. Concepts organized hierarchically based on Workflowy depth:
   - Major concepts (found in shallower notes)
   - Detail concepts (found in deeper nested notes)
5. Graphviz renders hierarchical network with labeled edges
6. PNG auto-inserted into Workflowy (or saved to Downloads)

Example output structure:
- Core: "Philosophy Notes" (center)
- Major: "Heidegger", "Dewey" (connected to core with "includes")
- Details: "phenomenology", "pragmatism" (connected with "influences", "contrasts with")
- Cross-links: "Heidegger" → "phenomenology" ("develops"),
              "phenomenology" ↔ "pragmatism" ("contrasts with")
```

## Constraints

### API Limitations

- Export endpoint: 1 request per minute (rate limited by Workflowy)
- No real-time sync: Changes require manual refresh
- No search API: Must export and filter locally

### Scope Boundaries

- Single user: No multi-user or sharing features
- API key auth only: No OAuth or session management
- Read/write only: No Workflowy UI features (colors, expand/collapse state)

## Non-Functional Requirements

### Performance

- Typical operation: <2 seconds
- Search with cache: <500ms
- Full export: <5 seconds (depends on outline size)

**Large dataset optimizations**:
- Scope filtering uses indexed lookups (O(n) instead of O(n²))
- Concept map edge building limited to 5,000 nodes and 1,000 edges
- Hierarchical content insertion batches concurrent API calls (up to 10 per batch)
- Parent-child relationships indexed for O(1) traversal

### Reliability

- Retry transient failures: 3 attempts with backoff
- Cache invalidation: On any write operation
- Error recovery: Clear messages, suggested actions

### Security

- Credentials: Environment variables only
- Logging: No user content or secrets
- Transport: Local stdio (no network exposure)

### 7. Batch Operations & High-Load Handling

**Goal**: Handle multiple operations efficiently without overwhelming the Workflowy API.

| Feature | Description |
|---------|-------------|
| Batch operations | Execute multiple create/update/delete/move operations in a single call |
| Request queuing | Controlled concurrency with configurable limits |
| Rate limiting | Proactive token bucket rate limiter to prevent API throttling |
| Selective cache invalidation | Invalidate only affected nodes instead of full cache |

---

#### batch_operations Tool

Execute multiple operations with controlled concurrency and rate limiting.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `operations` | array | yes | Array of operations to execute |
| `parallel` | boolean | no | Execute in parallel (default: true) |

**Operation structure**:
```json
{
  "type": "create" | "update" | "delete" | "move" | "complete" | "uncomplete",
  "params": { /* operation-specific parameters */ }
}
```

**Operation params by type**:
- `create`: `{name, note?, parent_id?, position?}`
- `update`: `{node_id, name?, note?}`
- `delete`: `{node_id}`
- `move`: `{node_id, parent_id, position?}`
- `complete`/`uncomplete`: `{node_id}`

**Response**:
```json
{
  "success": true,
  "message": "All 10 operations completed successfully",
  "total": 10,
  "succeeded": 10,
  "failed": 0,
  "results": [
    {
      "index": 0,
      "operation": { "type": "create", "params": {...} },
      "status": "fulfilled",
      "result": { "id": "abc123", "name": "..." }
    }
  ],
  "queue_stats": {
    "queueLength": 0,
    "activeRequests": 0,
    "totalProcessed": 10,
    "totalFailed": 0
  }
}
```

**Use cases**:
- Bulk node creation (e.g., importing a list of items)
- Mass updates (e.g., completing multiple todos)
- Mixed operations in a single batch

---

#### Configuration

High-load behavior is configured via environment constants:

**Queue Configuration** (`QUEUE_CONFIG`):
- `maxConcurrency`: Max parallel API requests (default: 3)
- `batchDelay`: Wait time before processing batch (default: 50ms)
- `maxBatchSize`: Max operations per batch (default: 20)

**Rate Limiting** (`RATE_LIMIT_CONFIG`):
- `requestsPerSecond`: Max sustained request rate (default: 5)
- `burstSize`: Allowed burst capacity (default: 10)

---

#### Performance Characteristics

| Scenario | Without Batching | With Batching |
|----------|-----------------|---------------|
| Create 10 nodes | ~2000ms (10 × 200ms) | ~400ms (parallel) |
| Create 100 nodes | ~20s | ~4s |
| Mixed 50 operations | Sequential | Parallel with rate limiting |

**Success criteria**: Handle 100+ operations without API rate limit errors.

---

## Future Considerations

*Not committed, but designed to accommodate:*

- Template system for common content patterns
- Conflict detection for concurrent edits
- Offline queue for unreachable API
