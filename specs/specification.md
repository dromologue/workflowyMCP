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

### 5. Content Modification

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
