# Workflowy MCP Server

A Model Context Protocol (MCP) server that integrates [Workflowy](https://workflowy.com) with Claude Desktop, enabling AI-powered management of your Workflowy outlines.

> **Note**: This project also serves as a reference implementation of [GitHub's SpecKit](https://github.com/github/spec-kit) for spec-driven development. See [Spec-Driven Development](#spec-driven-development) below.

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
| `generate_concept_map` | Generate a visual concept map with configurable search scope and insert directly into Workflowy via Dropbox |

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

To insert concept map images directly into Workflowy, configure Dropbox for image hosting. This is a one-time setup.

### Step 1: Create a Dropbox App

1. Go to [Dropbox App Console](https://www.dropbox.com/developers/apps)
2. Click **Create app** → Choose **Scoped access** → Choose **Full Dropbox**
3. Name it anything (e.g., "Workflowy Concept Maps") → Click **Create app**
4. On the app page, copy your **App key** and **App secret**

### Step 2: Set Permissions

1. Go to the **Permissions** tab
2. Check: `files.content.write` and `sharing.write`
3. Click **Submit**

### Step 3: Get Your Authorization Code

Open this URL in your browser (replace `YOUR_APP_KEY` with your actual app key):

```
https://www.dropbox.com/oauth2/authorize?client_id=YOUR_APP_KEY&response_type=code&token_access_type=offline
```

Click **Allow**. Dropbox will show you a **code** - copy it.

### Step 4: Exchange Code for Refresh Token

Run this command in Terminal (replace the THREE placeholders):

```bash
curl -X POST https://api.dropbox.com/oauth2/token \
  -d code=PASTE_YOUR_CODE_HERE \
  -d grant_type=authorization_code \
  -d client_id=YOUR_APP_KEY \
  -d client_secret=YOUR_APP_SECRET
```

You'll get a JSON response. Find the `"refresh_token"` value - it looks like:
```
"refresh_token": "xxxxxxxxxxxxAAAAAAAAAxxxxxxxxxxxxxxxxxxxxxxx"
```

### Step 5: Add to .env

Add these three lines to your `.env` file:

```bash
DROPBOX_APP_KEY=your-app-key
DROPBOX_APP_SECRET=your-app-secret
DROPBOX_REFRESH_TOKEN=paste-the-refresh-token-from-step-4
```

### Step 6: Test

Restart Claude Desktop and try:
> "Create a concept map for my Research node and insert it there"

Concept map images will be stored in `/workflowy/conceptMaps/` in your Dropbox.

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

## Spec-Driven Development

This project uses [GitHub's SpecKit](https://github.com/github/spec-kit) as a reference implementation of spec-driven development (SDD). SpecKit provides a structured methodology for building software with AI assistants by defining specifications upfront.

### What is SpecKit?

SpecKit transforms how we build software by putting specifications first. Instead of jumping straight into code, you:

1. **Define** what you want (constitution, specification)
2. **Plan** how to build it (implementation plan)
3. **Track** what needs doing (tasks)
4. **Implement** with AI assistance

The specs become a "single source of truth" that AI coding assistants reference throughout development.

### Spec Structure

```
specs/
├── constitution.md        # Non-negotiable principles
├── specification.md       # What the tool does
├── implementation-plan.md # Technical approach & ADRs
└── tasks.md              # Actionable work items
```

#### `constitution.md` - The Rules

Defines non-negotiable principles that guide all decisions:

- **Mission**: Core value proposition ("AI-native outlining")
- **Quality standards**: TypeScript strictness, test coverage
- **Security posture**: How credentials and data are handled
- **Compatibility**: Versioning and breaking change policy
- **Design philosophy**: Smart workflows vs atomic operations

Example principle from this project:
> "Paranoid Security: API keys and credentials never logged, even at debug level"

#### `specification.md` - The What

Documents what the tool does without prescribing how:

- **User personas**: Who uses this and why
- **Core capabilities**: Features organized by goal
- **User flows**: Step-by-step interaction patterns
- **Constraints**: API limits, scope boundaries
- **Success criteria**: How to know if it's working

#### `implementation-plan.md` - The How

Technical decisions and architecture:

- **Architecture diagrams**: System component layout
- **Technology stack**: Runtime, language, dependencies
- **Module structure**: Code organization
- **ADRs**: Architecture Decision Records documenting key choices
- **Error handling**: Strategy for failures
- **Testing approach**: What and how to test

Example ADR from this project:
> **ADR-005: Bottom-Default Insertion Order**
> Context: Workflowy's "top" position reverses multi-node content.
> Decision: Default to "bottom", apply "top" only to first node.

#### `tasks.md` - The Work

Actionable items derived from the spec:

- **Phased approach**: Foundation → Testing → Refactoring → Docs → Reliability
- **Task states**: `[ ]` pending, `[~]` in progress, `[x]` complete
- **Backlog**: Future considerations not yet committed
- **Completed**: Reference for done work

### Benefits of This Approach

1. **Living documentation**: Specs evolve with the code
2. **AI context**: Assistants reference specs for consistent decisions
3. **Onboarding**: New contributors understand design rationale
4. **Traceability**: Link tasks → specs → implementation

### Using SpecKit with Claude

When working with Claude on this project:

```
"Before implementing, check specs/constitution.md for project principles"
"Reference specs/implementation-plan.md for the ADR on caching"
"Update specs/tasks.md to mark T-005 as complete"
```

The specs act as persistent memory across conversations.

### Learn More

- [SpecKit Repository](https://github.com/github/spec-kit)
- [Spec-Driven Development Guide](https://github.com/github/spec-kit/blob/main/spec-driven.md)
- [This project's specs](./specs/)

## License

MIT

## Contributing

Contributions are welcome! Please open an issue or submit a pull request.

Before contributing, review:
- `specs/constitution.md` for project principles
- `specs/tasks.md` for open work items

## Acknowledgments

- [Anthropic](https://anthropic.com) for Claude and the MCP protocol
- [Workflowy](https://workflowy.com) for the outliner and API
- [GitHub](https://github.com) for SpecKit and the spec-driven methodology
