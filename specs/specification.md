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
| **insert_content** | THE PRIMARY TOOL for all node insertion - single, bulk, todos, any size |
| **convert_markdown_to_workflowy** | REQUIRED for markdown - converts to Workflowy format |
| Smart insert | Search-and-insert workflow with selection |
| Find insert targets | Search for potential parent nodes before insertion |
| Parallel processing | Auto-optimizes for any workload size (1 to 1000+ nodes) |
| Order preservation | Content appears in same order as provided |
| Staging node pattern | Prevents nodes from appearing at unintended locations during insertion |
| **File insertion** | Insert files directly without Claude reading them first |
| **Async job queue** | Background processing for large workloads with progress tracking |

**Single entry point for all insertions**:

`insert_content` is the ONLY tool needed for creating nodes. It handles:
- **Single nodes**: One line of content
- **Bulk hierarchical content**: Multiple indented lines
- **Todos**: Use `[ ]` for pending, `[x]` for completed
- **Any workload size**: Auto-parallelizes for large content (≥20 nodes)

**Workflow for markdown content**:
1. Convert markdown → `convert_markdown_to_workflowy`
2. Insert result → `insert_content`

**Position behavior**:
- `top` (default): First node placed at top, subsequent nodes follow in order
- `bottom`: Content appended after existing children, order preserved

**Staging node pattern**:

To prevent nodes from briefly appearing at the root or wrong location during multi-node insertions, the insertion tools use a staging node pattern:

1. Create a temporary staging node (`__staging_temp__`) under the target parent
2. Create all hierarchical content inside the staging node
3. Move top-level children from staging to the actual parent (respecting position)
4. Delete the staging node

This ensures nodes are never visible at unintended locations during the operation.

**Success criteria**: Claude-generated content appears in Workflowy with correct structure and order, with 70%+ time savings for workloads over 50 nodes.

---

#### insert_content Tool

**THE PRIMARY TOOL** for all node insertion into Workflowy. Use this for everything: single nodes, bulk content, todos, any hierarchical structure.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `parent_id` | string | yes | Target parent node ID |
| `content` | string | yes | Content in 2-space indented format (see examples below) |
| `position` | "top" \| "bottom" | no | Position relative to siblings (default: top) |

**Content format examples**:

```
# Single node
My new node

# Multiple nodes (siblings)
First node
Second node
Third node

# Hierarchical content
Parent node
  Child 1
  Child 2
    Grandchild
  Child 3

# Todo items
[ ] Pending task
[x] Completed task
[ ] Another pending task
  [ ] Nested subtask

# Mixed content
Project Plan
  [ ] Research phase
    Gather requirements
    Interview stakeholders
  [ ] Design phase
    Create wireframes
    [x] Review with team
```

**For markdown content**: Use `convert_markdown_to_workflowy` first to convert markdown to indented format, then pass the result to `insert_content`.

**Behavior**: Automatically uses parallel insertion for workloads ≥20 nodes, single-agent for smaller content.

---

#### smart_insert Tool

Search for a target node by name and insert content. Combines find + insert in one workflow.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `search_query` | string | yes | Search term to find the target parent |
| `content` | string | yes | Content in 2-space indented format (same as insert_content) |
| `position` | "top" \| "bottom" | no | Position relative to siblings (default: top) |
| `selection` | number | no | If multiple matches, the 1-based index to select |

**Content must be in 2-space indented format**. For markdown, use `convert_markdown_to_workflowy` first.

**Behavior**:
1. Searches for nodes matching `search_query`
2. If single match: inserts content immediately
3. If multiple matches: returns options for user selection
4. User calls again with `selection` to complete insertion

---

#### find_insert_targets Tool

Search for potential target nodes to insert content into. Used when Claude needs to preview available targets before deciding where to insert.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | yes | Search term to find potential targets |

**Response**:
```json
{
  "found": true,
  "count": 3,
  "targets": [
    { "id": "abc", "name": "Projects", "path": "Work > Projects", "children_count": 5 },
    { "id": "def", "name": "Project Ideas", "path": "Personal > Project Ideas", "children_count": 12 }
  ],
  "message": "Found 3 potential targets. Use insert_content with the desired parent_id."
}
```

**Use case**: When Claude wants to show the user available insertion targets before committing to an insertion location.

---

#### convert_markdown_to_workflowy Tool

**REQUIRED** for any markdown content. Converts markdown documents to Workflowy's 2-space indented format. This is the ONLY way to format markdown for Workflowy.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `markdown` | string | yes | The markdown content to convert |
| `options` | object | no | Conversion settings (see below) |
| `analyze_only` | boolean | no | If true, return stats only without converting |

**Options**:

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `preserveInlineFormatting` | boolean | true | Keep **bold**, *italic*, `code`, links |
| `convertTables` | boolean | true | Convert tables to hierarchical lists |
| `includeHorizontalRules` | boolean | true | Include --- as separator nodes |
| `maxDepth` | number | 10 | Maximum nesting depth |
| `preserveTaskLists` | boolean | true | Keep [x] and [ ] checkbox markers |

**Supported markdown elements**:
- Headers (H1-H6, ATX `#` and setext `===`/`---` styles)
- Nested lists (ordered and unordered)
- Task lists with checkboxes (`[ ]` and `[x]`)
- Fenced code blocks with language labels
- Tables (converted to hierarchical structure)
- Blockquotes (single and nested)
- Inline formatting (bold, italic, links)

**Response**:
```json
{
  "success": true,
  "content": "Converted content...",
  "node_count": 42,
  "stats": {
    "headers": 5,
    "list_items": 20,
    "code_blocks": 2,
    "tables": 1,
    "blockquotes": 3,
    "task_items": 8,
    "paragraphs": 15
  },
  "warnings": [],
  "usage_hint": "Ready to use with insert_content"
}
```

**Workflow**:
```
1. User provides markdown document
2. Call convert_markdown_to_workflowy with markdown
3. Take the "content" from response
4. Call insert_content with that content
```

**Use case**: Converting README files, documentation, meeting notes, or any markdown content for insertion into Workflowy.

---

### 4. Todo Management

**Goal**: Create and manage task lists within Workflowy.

| Feature | Description |
|---------|-------------|
| Create todos | Use `insert_content` with checkbox syntax `[ ]` or `[x]` |
| List todos | Retrieve all todos with filtering by status, parent, search |
| Complete/Uncomplete | Toggle completion status of any node |

**Creating todos**:

Use `insert_content` with checkbox syntax:
```
[ ] Pending task
[x] Completed task
[ ] Another task
  [ ] Nested subtask
```

**Todo identification**:
- Nodes with `layoutMode: "todo"`
- Nodes using checkbox syntax (`[ ]` or `[x]`)

**Filtering options** (for `list_todos`):
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

1. Claude generates summary (hierarchical content)
2. smart_insert searches for "Research"
3. If multiple matches → return numbered options
4. User selects → system automatically:
   - Analyzes workload size
   - Uses parallel insertion if beneficial (≥20 nodes)
   - Falls back to single-agent for small content
5. Content inserted with hierarchy preserved
6. Confirmation with target path and performance stats shown
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
User: "Add my weekly tasks to the Tasks node"

1. find_node for "Tasks"
2. insert_content with checkbox syntax:
   [ ] Review inbox
   [ ] Process email
   [ ] Update project status
   [x] Already done item
3. Confirmation with created todos

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

### Flow 5: Large Content Insertion (Automatic Parallelization)

```
User: "Import this research outline into my Project node" (provides 200+ node outline)

1. Claude calls insert_content (the only insertion tool needed)
   → System automatically detects 180 nodes
   → Parallel insertion enabled automatically

2. Behind the scenes, the system:
   - Analyzes workload: 180 nodes, 5 subtrees
   - Assigns 4 workers (automatically determined)
   - Each worker gets independent rate limiter (5 req/sec)
   - Workers process their subtrees concurrently

3. Progress tracked during execution:
   - Worker 1: 45 nodes (completed)
   - Worker 2: 38 nodes (in progress, 80%)
   - Worker 3: 52 nodes (completed)
   - Worker 4: 45 nodes (completed)

4. Results returned to Claude:
   {
     "created_nodes": 180,
     "duration_seconds": 8.7,
     "actual_savings_percent": 76,
     "mode": "parallel_workers"
   }

5. If any subtree fails:
   - Automatic retry (up to 2 attempts)
   - Partial success reported with failed subtree details

Note: Claude uses insert_content for ALL insertions. Parallel optimization
happens automatically for workloads ≥20 nodes.
```

### Flow 6: Markdown Document Import

```
User: "Import this markdown README into my Documentation node"

1. Claude calls convert_markdown_to_workflowy with the markdown content
   → Converts headers, lists, code blocks, tables to indented format
   → Returns converted content and stats

2. Claude calls insert_content with the converted content
   → System auto-optimizes based on node count
   → Content inserted with hierarchy preserved

3. Confirmation with stats:
   - 47 nodes created
   - 5 headers, 20 list items, 2 code blocks converted
   - Duration: 2.3 seconds
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

### 8. Multi-Agent Parallel Insertion (Automatic)

**Goal**: Provide fast, efficient content insertion automatically for all hierarchical content.

Parallel insertion is **fully automatic** - Claude simply uses `insert_content` and the system optimizes based on workload size.

| Feature | Description |
|---------|-------------|
| **Fully automatic** | `insert_content` auto-parallelizes based on workload |
| Workload analysis | System determines optimal worker count |
| Subtree splitting | Divides content into independent subtrees |
| Parallel workers | Multiple workers with independent rate limiters |
| Progress tracking | Real-time updates during execution |
| Automatic retry | Failed subtrees retry up to 2 times |
| Smart fallback | Falls back to single-agent for <20 nodes |

**No manual tool selection required**: Claude should simply use `insert_content` for all hierarchical content. The system automatically uses parallel workers when beneficial (≥20 nodes).

---

#### analyze_workload Tool

Analyze hierarchical content to estimate insertion performance. Useful for understanding large workloads before insertion.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | string | yes | Hierarchical content to analyze (2-space indented) |
| `max_workers` | number | no | Maximum workers to consider (1-10, default: 5) |

**Response**:
```json
{
  "success": true,
  "analysis": {
    "total_nodes": 150,
    "subtree_count": 4,
    "recommended_workers": 4,
    "subtrees": [
      {
        "id": "subtree-0",
        "node_count": 42,
        "root_text": "First Section...",
        "estimated_ms": 8400
      }
    ]
  },
  "time_estimates": {
    "single_agent_ms": 30000,
    "single_agent_seconds": 30,
    "parallel_ms": 9400,
    "parallel_seconds": 9.4,
    "savings_percent": 69,
    "savings_seconds": 20.6
  },
  "recommendation": "Use insert_content - it auto-optimizes for any workload size"
}
```

**Use case**: Before inserting large content, analyze to understand time estimates. Note: You don't need to analyze before inserting - `insert_content` handles optimization automatically.

---

#### How insert_content Handles Large Workloads

When `insert_content` receives hierarchical content, it automatically:

1. **Content splitting**: Parses content into independent subtrees based on top-level nodes
2. **Worker assignment**: Each subtree assigned to a worker with its own rate limiter
3. **Parallel execution**: Workers process subtrees concurrently
4. **Retry handling**: Failed subtrees automatically retry (up to 2 attempts)
5. **Result merging**: All results combined with detailed stats

**Response includes performance stats**:
```json
{
  "success": true,
  "message": "Successfully inserted 150 nodes",
  "total_nodes": 150,
  "created_nodes": 150,
  "mode": "parallel_workers",
  "duration_seconds": 8.2,
  "performance": {
    "estimated_single_agent_ms": 30000,
    "actual_parallel_ms": 8234,
    "actual_savings_percent": 73
  }
}
```

---

#### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Orchestrator                             │
│  ┌───────────┐  ┌───────────┐  ┌───────────┐  ┌───────────┐ │
│  │ Worker 1  │  │ Worker 2  │  │ Worker 3  │  │ Worker N  │ │
│  │ RateLimiter│  │ RateLimiter│  │ RateLimiter│  │ RateLimiter│ │
│  │ Subtree A │  │ Subtree B │  │ Subtree C │  │ Subtree D │ │
│  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘ │
└────────┼──────────────┼──────────────┼──────────────┼───────┘
         └──────────────┴──────────────┴──────────────┘
                              ↓
                   Workflowy API (5 req/sec each)
```

Each worker has its own rate limiter, allowing true parallelism without competing for the same token bucket.

---

#### Subtree Splitting Algorithm

Content is split at top-level node boundaries:

```
Input:
  Section A          ← Subtree 1 root
    Child A1
    Child A2
  Section B          ← Subtree 2 root
    Child B1
      Grandchild B1a
  Section C          ← Subtree 3 root
    Child C1

Output: 3 independent subtrees
```

**Balancing rules**:
- Target nodes per subtree: 50 (configurable)
- Minimum nodes for separate subtree: 5
- Small adjacent groups merged to reduce overhead
- Maximum subtrees capped at `max_workers`

---

#### Performance Benchmarks

| Nodes | Single Agent | 5 Workers | Savings |
|-------|--------------|-----------|---------|
| 50 | ~10 sec | ~3 sec | 70% |
| 100 | ~20 sec | ~5 sec | 75% |
| 200 | ~40 sec | ~9 sec | 78% |
| 500 | ~100 sec | ~22 sec | 78% |

**Automatic optimization**:

The system automatically selects the optimal insertion strategy based on workload size:

| Node Count | Automatic Behavior | Performance |
|------------|-------------------|-------------|
| < 20 | Single-agent | Fast for small content |
| 20-50 | Parallel (2-3 workers) | ~50-60% time savings |
| 50-100 | Parallel (3-4 workers) | ~70% time savings |
| 100-200 | Parallel (4-5 workers) | ~75% time savings |
| 200+ | Parallel (5 workers) | ~78%+ time savings |

**No manual tool selection required**: Claude should simply use `insert_content` for all hierarchical content. Parallel optimization happens automatically.

**Success criteria**: Insert 200+ nodes with >70% time savings compared to single-agent approach.

---

### 9. Async Job Queue (Background Processing)

**Goal**: Handle large workloads without hitting API rate limits or timeouts. Claude can hand off large operations to the server for background processing.

| Feature | Description |
|---------|-------------|
| **Job submission** | Submit large workloads for background processing |
| **Progress tracking** | Check job status and progress percentage |
| **Result retrieval** | Get results when job completes |
| **Job cancellation** | Cancel pending or in-progress jobs |
| **Rate limit handling** | Server manages API pacing automatically |

**Why use the job queue**:
- Avoid API rate limit errors on large operations
- Prevent Claude timeouts on long-running tasks
- Enable true background processing
- Track progress of long operations

---

#### submit_job Tool

Submit a large workload for background processing. Returns a job ID to track progress.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `type` | "insert_content" \| "batch_operations" | yes | Type of job |
| `params` | object | yes | Job parameters (varies by type) |
| `description` | string | no | Human-readable description |

**Job params by type**:
- `insert_content`: `{parentId, content, position?}`
- `batch_operations`: `{operations: [{type, params}...]}`

**Response**:
```json
{
  "success": true,
  "job_id": "job-1234567890-1",
  "type": "insert_content",
  "status": "pending",
  "description": "Insert 150 nodes under 'Research'",
  "estimated_nodes": 150,
  "message": "Job submitted for background processing. Use get_job_status to check progress."
}
```

---

#### get_job_status Tool

Check the progress of a submitted job.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `job_id` | string | yes | The job ID from submit_job |

**Response**:
```json
{
  "success": true,
  "job_id": "job-1234567890-1",
  "status": "processing",
  "progress": {
    "total": 150,
    "completed": 89,
    "failed": 0,
    "percentComplete": 59,
    "currentOperation": "Inserting content"
  },
  "created_at": "2024-01-15T10:30:00.000Z",
  "started_at": "2024-01-15T10:30:01.000Z"
}
```

**Job statuses**:
- `pending`: Waiting to start
- `processing`: Currently executing
- `completed`: Finished successfully
- `failed`: Finished with errors
- `cancelled`: Cancelled by user

---

#### get_job_result Tool

Get the result of a completed job.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `job_id` | string | yes | The job ID from submit_job |

**Response** (completed job):
```json
{
  "success": true,
  "job_id": "job-1234567890-1",
  "status": "completed",
  "result": {
    "success": true,
    "nodesCreated": 150,
    "nodeIds": ["abc123", "def456", ...]
  }
}
```

---

#### list_jobs Tool

List all jobs with optional status filtering.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `status` | array | no | Filter by status (default: all) |

**Response**:
```json
{
  "success": true,
  "jobs": [
    {
      "job_id": "job-1234567890-1",
      "type": "insert_content",
      "status": "completed",
      "progress": { "total": 150, "completed": 150, "percentComplete": 100 },
      "description": "Insert 150 nodes",
      "created_at": "2024-01-15T10:30:00.000Z"
    }
  ],
  "queue_stats": {
    "pending": 0,
    "processing": 1,
    "completed": 5,
    "failed": 0,
    "cancelled": 0,
    "total": 6
  }
}
```

---

#### cancel_job Tool

Cancel a pending or in-progress job.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `job_id` | string | yes | The job ID to cancel |

**Response**:
```json
{
  "success": true,
  "message": "Job job-1234567890-1 cancelled"
}
```

---

#### Job Queue Workflow

```
User: "Insert this large research document (500+ nodes)"

1. Claude calls submit_job with type: "insert_content"
   → Returns: {job_id: "job-123", status: "pending"}

2. Claude can check progress:
   get_job_status(job_id: "job-123")
   → Returns: {status: "processing", progress: {completed: 245, total: 512, percentComplete: 48}}

3. When done, get results:
   get_job_result(job_id: "job-123")
   → Returns: {status: "completed", result: {nodesCreated: 512, nodeIds: [...]}}

The server handles all rate limiting internally (5 req/sec with burst of 10).
Jobs are retained for 30 minutes after completion.
```

**Success criteria**: Insert 500+ nodes without API rate limit errors or timeouts.

---

### 10. File Insertion (Direct File Handoff)

**Goal**: Allow Claude to pass file paths directly to the server without reading file contents first.

| Feature | Description |
|---------|-------------|
| **Direct file insertion** | Server reads and inserts file contents |
| **Auto format detection** | Detects markdown from file extension |
| **Markdown conversion** | Automatically converts .md files |
| **Background file jobs** | Submit large files for background processing |

**Why use file insertion**:
- Claude doesn't need to read or parse file contents
- Server handles format detection and conversion
- Reduces token usage in conversation
- Better handling of large files

---

#### insert_file Tool

Insert a file's contents into Workflowy. The server reads, converts (if needed), and inserts.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `file_path` | string | yes | Absolute path to the file |
| `parent_id` | string | yes | Target parent node ID |
| `position` | "top" \| "bottom" | no | Position relative to siblings (default: top) |
| `format` | "auto" \| "markdown" \| "plain" | no | How to process the file (default: auto) |

**Format options**:
- `auto`: Detect from extension (`.md`/`.markdown` → markdown conversion, else plain)
- `markdown`: Force markdown-to-Workflowy conversion
- `plain`: Treat as pre-formatted 2-space indented content

**Response**:
```json
{
  "success": true,
  "message": "Inserted 47 nodes using parallel workers",
  "nodes": [...],
  "file": {
    "name": "research-notes.md",
    "size": 15234,
    "format": "markdown",
    "node_count": 47
  }
}
```

---

#### submit_file_job Tool

Submit a large file for background insertion. Use for large files to avoid timeouts.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `file_path` | string | yes | Absolute path to the file |
| `parent_id` | string | yes | Target parent node ID |
| `position` | "top" \| "bottom" | no | Position relative to siblings (default: top) |
| `format` | "auto" \| "markdown" \| "plain" | no | How to process the file (default: auto) |
| `description` | string | no | Optional description for tracking |

**Response**:
```json
{
  "success": true,
  "job_id": "job-1234567890-2",
  "type": "insert_file",
  "status": "pending",
  "description": "Insert file 'thesis.md' under 'Research'",
  "file": {
    "name": "thesis.md",
    "size": 245678,
    "path": "/Users/me/Documents/thesis.md"
  },
  "message": "File job submitted for background processing. Use get_job_status to check progress."
}
```

---

#### File Insertion Workflow

```
User: "Add my research notes from ~/Documents/research.md to my Research node"

1. Claude calls insert_file:
   insert_file(
     file_path: "/Users/me/Documents/research.md",
     parent_id: "xyz123"
   )

2. Server automatically:
   - Reads the file
   - Detects .md extension → markdown format
   - Converts markdown to Workflowy format
   - Inserts using parallel workers

3. Returns result:
   {
     "success": true,
     "file": {"name": "research.md", "format": "markdown", "node_count": 89}
   }

Claude never needs to read or parse the file - server handles everything.
```

**For large files** (200+ nodes expected):
```
1. Claude calls submit_file_job instead
2. Server processes in background with rate limiting
3. Claude checks progress with get_job_status
4. Gets results with get_job_result when complete
```

**Success criteria**: Insert file contents without Claude reading the file first.

---

## Future Considerations

*Not committed, but designed to accommodate:*

- Template system for common content patterns
- Conflict detection for concurrent edits
- Offline queue for unreachable API
