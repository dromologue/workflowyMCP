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
| Text search | Search node names and notes by keyword |
| Path display | Show full breadcrumb path for disambiguation |
| Target listing | Access Workflowy shortcuts (inbox, starred) |
| Full export | Retrieve entire outline for comprehensive analysis |

**Success criteria**: User can locate any node in <2 tool calls.

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
| Concept map | Generate visual PNG/JPEG graph of node relationships |

**Keyword extraction**:
- Filters common stop words
- Prioritizes significant terms (3+ characters)
- Scores matches by title vs note occurrence

**Link placement options**:
- `child`: Creates a "🔗 Related" child node with links (default)
- `note`: Appends links to the node's existing note

**Link format**: `[Node Title](https://workflowy.com/#/nodeId)`

**Concept map generation**:
- Center node displayed with related nodes arranged around it
- Edge labels show matching keywords between nodes
- Edge width indicates relevance strength
- Output formats: PNG (default), JPEG
- Configurable search scope (children, siblings, ancestors, all)
- Direct insertion into Workflowy via Dropbox image hosting
- Fallback: save locally to `~/Downloads/concept-map-{timestamp}.png`
- Optimized for readability with clear node labels and colors

**Search scopes**:
- `all`: Search entire Workflowy knowledge base
- `children`: Search only descendants of the center node
- `siblings`: Search only peer nodes (same parent)
- `ancestors`: Search only the parent chain

**Image hosting** (Dropbox):
- Requires Dropbox OAuth configuration (app key, secret, refresh token)
- Images stored in `/concept-maps/` folder
- Shareable links generated automatically
- When configured, concept maps can be inserted directly into Workflowy nodes

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

### Flow 4: Visualize Knowledge Connections

```
User: "Create a concept map showing how this project connects to other nodes"

1. get_node retrieves the target node content
2. generate_concept_map analyzes keywords and finds related nodes
3. Graphviz renders the relationship graph
4. PNG/JPEG saved to Downloads folder
5. User can insert image into Workflowy or share
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

### Reliability

- Retry transient failures: 3 attempts with backoff
- Cache invalidation: On any write operation
- Error recovery: Clear messages, suggested actions

### Security

- Credentials: Environment variables only
- Logging: No user content or secrets
- Transport: Local stdio (no network exposure)

## Future Considerations

*Not committed, but designed to accommodate:*

- Batch operations for bulk updates
- Template system for common content patterns
- Conflict detection for concurrent edits
- Offline queue for unreachable API
