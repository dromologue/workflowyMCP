# Workflowy MCP Server

A Model Context Protocol (MCP) server that integrates [Workflowy](https://workflowy.com) with Claude Desktop, enabling AI-powered management of your Workflowy outlines.

## Features

### Core Operations
- **Search** nodes by text across your entire outline
- **Navigate** and retrieve nodes and their children
- **Create** new nodes with markdown formatting
- **Edit** existing node names and notes
- **Delete** nodes permanently
- **Move** nodes to new locations

### Knowledge Management
- **Todo Management** - Create, list, and complete checkbox items
- **Find Related** - Discover connections between nodes using keyword analysis
- **Create Links** - Auto-generate internal links to related content

### Visual Concept Maps
Generate visual relationship graphs that show how your ideas connect:
- **Automatic keyword extraction** from node content
- **Relevance scoring** to find the most related nodes
- **Configurable scope** - search children, siblings, ancestors, or entire outline
- **High-resolution output** (2400px, 300 DPI)
- **Auto-insert into Workflowy** via Dropbox integration (or save locally)

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

### Todo Management

| Tool | Description |
|------|-------------|
| `create_todo` | Create a checkbox todo item with optional initial completion state |
| `list_todos` | List all todos with filtering by status, parent, and search text |
| `complete_node` | Mark a todo (or any node) as completed |
| `uncomplete_node` | Mark a todo (or any node) as incomplete |

### Knowledge Linking

| Tool | Description |
|------|-------------|
| `find_related` | Find nodes related to a given node based on keyword analysis |
| `create_links` | Create internal links from a node to related content in the knowledge base |
| `generate_concept_map` | Generate a visual concept map and insert into the source node (requires Dropbox config) |

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

**Create todos:**
> "Add a todo to my inbox: Review Q4 budget proposal"

**List pending tasks:**
> "Show me all my incomplete todos"

**Complete a task:**
> "Mark the 'Send report' task as complete"

**Filter todos by location:**
> "List all todos under my Work Projects node"

**Find related content:**
> "Find nodes related to this article about machine learning"

**Auto-link knowledge:**
> "Create links from this node to related content in my knowledge base"

**Generate visual concept map:**
> "Create a concept map showing how this project connects to other nodes and add it to my Research node"

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

## Dropbox Configuration (Optional)

To auto-insert concept map images into Workflowy, configure Dropbox for image hosting.

### Step 1: Create a Dropbox App

1. Go to [Dropbox App Console](https://www.dropbox.com/developers/apps)
2. Click **Create app** → **Scoped access** → **Full Dropbox** → Name it → **Create app**
3. Copy your **App key** and **App secret**

### Step 2: Set Permissions

1. Go to the **Permissions** tab
2. Check: `files.content.write` and `sharing.write`
3. Click **Submit**

### Step 3: Get Refresh Token

Open this URL (replace `YOUR_APP_KEY`):
```
https://www.dropbox.com/oauth2/authorize?client_id=YOUR_APP_KEY&response_type=code&token_access_type=offline
```

Click **Allow** and copy the code. Then run:
```bash
curl -X POST https://api.dropbox.com/oauth2/token \
  -d code=YOUR_CODE \
  -d grant_type=authorization_code \
  -d client_id=YOUR_APP_KEY \
  -d client_secret=YOUR_APP_SECRET
```

Copy the `refresh_token` from the response.

### Step 4: Add to .env

```bash
DROPBOX_APP_KEY=your-app-key
DROPBOX_APP_SECRET=your-app-secret
DROPBOX_REFRESH_TOKEN=your-refresh-token
```

Images will be stored in `/workflowy/conceptMaps/` in your Dropbox and auto-inserted into the source node.

## Concept Map Scope Options

The `generate_concept_map` tool supports different search scopes:

| Scope | Description |
|-------|-------------|
| `all` | Search entire Workflowy (default) |
| `children` | Search only descendants of the node |
| `siblings` | Search only peer nodes (same parent) |
| `ancestors` | Search only parent chain |
| `this_node` | No related nodes (visualization only) |

Example:
> "Create a concept map for my Project node, only searching its children"

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
