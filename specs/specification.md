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

Follows academic concept mapping principles (Cornell University guidelines):

1. **Core concept at center**: The main topic/theme is placed prominently at the top
2. **Hierarchical arrangement**: Concepts arranged in levels (major → detail)
3. **Labeled relationships**: Connections show *how* concepts relate (not just that they do)
4. **Visual encoding**: Node size = frequency, edge color = relationship type

**Creating a concept map** (based on Cornell/academic best practices):
1. Identify the core concept - the central theme being mapped
2. List concepts/terms to map - these become nodes in the visualization
3. Analyze content to find relationships - tool scans Workflowy children for co-occurrence
4. Extract relationship labels - phrases like "influences", "contrasts with", "includes" from context
5. Organize hierarchically - concepts found in shallower Workflowy nodes become "major concepts", deeper ones become "details"
6. Generate visual map - Graphviz renders the hierarchical network

**Parameters**:
- `node_id`: Parent node whose children will be analyzed
- `core_concept`: The central concept (defaults to parent node name)
- `concepts`: List of concepts/terms to map (required, minimum 2, maximum 35)
- `scope`: Search scope for content analysis (default: children)
- `format`: PNG (default) or JPEG
- `title`: Custom title for the map

**Limits**:
- Maximum 35 concepts per map (prevents oversized graphs that fail to render)
- For larger concept sets, split into multiple focused maps by theme/category

**Visual encoding**:
- **Node levels**: Core (dark blue, large) → Major (medium colors) → Details (lighter colors, smaller)
- **Node size**: Larger = concept appears in more nodes
- **Edge labels**: Relationship type extracted from content context
- **Edge colors**: Green = supporting, Red dashed = contrasting, Purple = dependency, Gray = general

**Relationship extraction**: The tool scans content for relationship words:
- Causal: leads to, causes, results in
- Hierarchical: includes, is part of, contains
- Comparative: contrasts with, similar to, differs from
- Dependency: requires, enables, prevents
- Evaluative: supports, opposes, extends, critiques

**Output**:
- Square aspect ratio (2000x2000 max, 300 DPI) for balanced visual layout
- Unicode support for accented characters (French, German, etc.)
- Auto-insert into source node via Dropbox image hosting
- Fallback: save locally to `~/Downloads/` if Dropbox not configured

**Search scopes**:
- `children`: Search only descendants of the parent node (default)
- `all`: Search entire Workflowy knowledge base
- `siblings`: Search only peer nodes (same parent)
- `ancestors`: Search only the parent chain

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

### Flow 4: Visualize Knowledge Connections

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
