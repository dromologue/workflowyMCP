# Workflowy MCP Server

A Model Context Protocol (MCP) server that integrates [Workflowy](https://workflowy.com) with Claude Desktop, enabling AI-powered management of your Workflowy outlines.

## Features

- **Search** nodes by text across your entire Workflowy outline
- **Navigate** and retrieve nodes and their children
- **Create** new nodes with markdown formatting support
- **Edit** existing node names and notes
- **Delete** nodes permanently
- **Move** nodes to new locations
- **Complete/Uncomplete** nodes for task management
- **Insert content** directly from Claude into any node

## Prerequisites

- [Node.js](https://nodejs.org/) v18 or later
- [Claude Desktop](https://claude.ai/download)
- A Workflowy account with an API key

## Installation

### 1. Clone the Repository

```bash
git clone https://github.com/dromologue/workflowyMCP.git
cd workflowyMCP
```

### 2. Install Dependencies

```bash
npm install
```

### 3. Get Your Workflowy API Key

1. Log in to [Workflowy](https://workflowy.com)
2. Visit [https://workflowy.com/api-key](https://workflowy.com/api-key)
3. Generate and copy your API key

### 4. Configure Environment

Create a `.env` file in the project root:

```bash
WORKFLOWY_USERNAME=your-email@example.com
WORKFLOWY_API_KEY=your-api-key-here
```

### 5. Build the Server

```bash
npm run build
```

### 6. Configure Claude Desktop

Open your Claude Desktop configuration file:

- **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Windows**: `%APPDATA%\Claude\claude_desktop_config.json`

Add the Workflowy server to your configuration:

```json
{
  "mcpServers": {
    "workflowy": {
      "command": "node",
      "args": ["/absolute/path/to/workflowyMCP/dist/index.js"]
    }
  }
}
```

Replace `/absolute/path/to/workflowyMCP` with the actual path where you cloned the repository.

### 7. Restart Claude Desktop

Quit and reopen Claude Desktop to load the MCP server.

## Available Tools

### Search & Navigation

| Tool | Description |
|------|-------------|
| `search_nodes` | Search for nodes by text in names and notes. Returns full paths for easy identification. |
| `get_node` | Retrieve a specific node by its ID |
| `get_children` | List child nodes of a parent (or root-level nodes) |
| `find_insert_targets` | Search for potential target nodes with numbered list for selection |
| `list_targets` | List available shortcuts (inbox, user-defined) |
| `export_all` | Export all nodes (rate limited: 1 request/minute) |

### Content Management

| Tool | Description |
|------|-------------|
| `create_node` | Create a new node with optional markdown formatting |
| `update_node` | Edit a node's name and/or note |
| `delete_node` | Permanently delete a node |
| `move_node` | Move a node to a new parent location |
| `complete_node` | Mark a node as completed |
| `uncomplete_node` | Mark a node as incomplete |

### Smart Insertion

| Tool | Description |
|------|-------------|
| `smart_insert` | Search for a node and insert content with interactive selection |
| `insert_content` | Insert content directly into a node by ID |

## Smart Insert Workflow

The `smart_insert` tool provides an interactive workflow for inserting content:

1. **Single match**: If your search finds exactly one node, content is inserted immediately
2. **Multiple matches**: Returns a numbered list with full paths for disambiguation
3. **Selection**: Call again with the `selection` parameter to complete insertion

Example interaction:
```
User: "Insert my meeting notes into the Projects node"

Claude uses smart_insert with search_query="Projects"

Response (multiple matches):
[1] Work > Projects
    ID: abc123
[2] Personal > Side Projects
    ID: def456
[3] Archive > Old Projects
    ID: ghi789

User: "Use option 1"

Claude uses smart_insert with selection=1

Response: Inserted 3 node(s) into "Projects"
```

## Usage Examples

Once configured, you can interact with Workflowy through Claude Desktop:

**Search your outline:**
> "Search my Workflowy for meeting notes"

**Smart content insertion (recommended):**
> "Summarize this article and add it to my Research node"

Claude will search for "Research", show you matching nodes if there are multiple, and let you pick the right one.

**Create new nodes:**
> "Create a new node in my inbox called 'Weekly Review' with a note about tasks to review"

**Manage tasks:**
> "Mark the 'Send report' task as complete"

**Navigate your outline:**
> "Show me all the children of my Projects node"

**Find where to insert:**
> "Find nodes where I could add my book notes"

## Markdown Formatting

When creating nodes, you can use markdown formatting:

- `# Header` → H1 heading
- `## Header` → H2 heading
- `### Header` → H3 heading
- `- [ ] Task` → Uncompleted todo
- `- [x] Task` → Completed todo
- `` ```code``` `` → Code block
- `> Quote` → Quote block
- `**bold**` → Bold text
- `*italic*` → Italic text
- `[text](url)` → Hyperlink

## Troubleshooting

### Server not appearing in Claude Desktop

1. Verify the path in `claude_desktop_config.json` is correct and absolute
2. Ensure the project is built (`npm run build`)
3. Check that Node.js is in your PATH
4. Restart Claude Desktop completely

### Authentication errors

1. Verify your API key at [https://workflowy.com/api-key](https://workflowy.com/api-key)
2. Check that `.env` file exists and contains correct credentials
3. Ensure there are no extra spaces in the `.env` file

### Rate limiting

The `export_all` tool is rate limited to 1 request per minute by Workflowy's API. Use `search_nodes` for frequent queries instead.

## Development

```bash
# Build the project
npm run build

# Run directly (after building)
npm start

# Build and run
npm run dev
```

## API Reference

This server uses the [Workflowy REST API](https://workflowy.com/api-reference/). Key endpoints:

- `POST /api/v1/nodes` - Create node
- `POST /api/v1/nodes/:id` - Update node
- `GET /api/v1/nodes/:id` - Get node
- `GET /api/v1/nodes` - List children
- `DELETE /api/v1/nodes/:id` - Delete node
- `GET /api/v1/nodes-export` - Export all nodes

## License

MIT

## Contributing

Contributions are welcome! Please open an issue or submit a pull request.

## Acknowledgments

- [Anthropic](https://anthropic.com) for Claude and the MCP protocol
- [Workflowy](https://workflowy.com) for the outliner and API
