# Concept Mapping with Claude, Workflowy & Dropbox

A visual concept mapping tool that connects Claude's AI with your Workflowy knowledge base. Generate hierarchical concept maps that reveal relationships between ideas in your notes, automatically hosted via Dropbox and embedded in your outline.

## What It Does

**Turn your notes into visual knowledge maps.**

You provide a list of concepts (philosophers, theories, themes, project components) and a Workflowy node to analyze. The tool:

1. Scans your Workflowy content for where those concepts appear
2. Extracts relationship labels from context ("influences", "contrasts with", "extends")
3. Organizes concepts hierarchically based on document structure
4. Generates a visual graph with labeled connections
5. Uploads to Dropbox and embeds the image in your Workflowy

## Example

```
You: "Create a concept map of my philosophy notes showing how
      Heidegger, Dewey, phenomenology, and pragmatism relate"

Claude: Analyzes your notes, finds that:
        - Heidegger appears in 12 nodes, develops phenomenology
        - Dewey appears in 8 nodes, central to pragmatism
        - "phenomenology contrasts with pragmatism" found in context

        → Generates hierarchical map with labeled relationships
        → Uploads to Dropbox, embeds in your Philosophy node
```

## The Stack

| Component | Role |
|-----------|------|
| **Claude** | AI that understands your request, orchestrates the analysis, interprets results |
| **Workflowy** | Your knowledge base - the source content being mapped |
| **Dropbox** | Image hosting - makes concept maps viewable in Workflowy |
| **Graphviz** | Graph rendering engine (runs locally via WASM) |

## Concept Map Features

### Academic-Style Mapping

Follows Cornell University concept mapping guidelines:

- **Core concept** at center (your main theme)
- **Hierarchical levels** - major concepts vs. detail concepts based on document depth
- **Labeled relationships** - shows *how* concepts connect, not just that they do
- **Visual encoding** - node size = frequency, edge color = relationship type

### Relationship Detection

Automatically extracts relationship types from your content:

| Type | Examples |
|------|----------|
| Causal | leads to, causes, results in |
| Hierarchical | includes, is part of, contains |
| Comparative | contrasts with, similar to, differs from |
| Dependency | requires, enables, prevents |
| Evaluative | supports, opposes, extends, critiques |

### Output

- Square aspect ratio (2000x2000 max) for balanced visual layout
- 300 DPI for high-resolution display
- Unicode support (French accents, German umlauts, etc.)
- Maximum 35 concepts per map (split larger sets into themed groups)

## Quick Start

### Prerequisites

- Node.js v18+
- [Claude Desktop](https://claude.ai/download)
- Workflowy account with [API key](https://workflowy.com/api-key)
- Dropbox account (for image hosting)

### Installation

```bash
git clone https://github.com/dromologue/workflowyMCP.git
cd workflowyMCP
npm install
npm run build
```

### Configuration

Create `.env` in the project root:

```bash
# Required - Workflowy
WORKFLOWY_USERNAME=your-email@example.com
WORKFLOWY_API_KEY=your-api-key

# Required for concept maps via MCP - Dropbox
DROPBOX_APP_KEY=your-app-key
DROPBOX_APP_SECRET=your-app-secret
DROPBOX_REFRESH_TOKEN=your-refresh-token

# Optional - for CLI auto concept extraction
ANTHROPIC_API_KEY=your-anthropic-api-key
```

Add to Claude Desktop config (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):

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

Restart Claude Desktop.

## Dropbox Setup

Concept maps require Dropbox for image hosting. Without it, images save to `~/Downloads/` instead.

### 1. Create Dropbox App

1. Go to [Dropbox App Console](https://www.dropbox.com/developers/apps)
2. Create app → Scoped access → Full Dropbox
3. Name it (e.g., "workflowy-concept-maps")
4. Copy **App key** and **App secret**

### 2. Set Permissions

In the Permissions tab, enable:
- `files.content.write` (upload images)
- `sharing.write` (create shareable links)

Click **Submit**.

### 3. Authorize

Open in browser (replace YOUR_APP_KEY):
```
https://www.dropbox.com/oauth2/authorize?client_id=YOUR_APP_KEY&response_type=code&token_access_type=offline
```

Copy the authorization code.

### 4. Get Refresh Token

```bash
curl -X POST https://api.dropbox.com/oauth2/token \
  -d code=YOUR_CODE \
  -d grant_type=authorization_code \
  -d client_id=YOUR_APP_KEY \
  -d client_secret=YOUR_APP_SECRET
```

Add the `refresh_token` from the response to your `.env`.

## Using Concept Maps

### Basic Usage

```
"Create a concept map of my Research node using these concepts:
 machine learning, neural networks, backpropagation, gradient descent"
```

### With Scope Control

| Scope | What it searches |
|-------|------------------|
| `children` | Only descendants of the node (default) |
| `all` | Entire Workflowy |
| `siblings` | Peer nodes only |
| `ancestors` | Parent chain only |

```
"Create a concept map for my Project node, only searching its children"
```

### Parameters

| Parameter | Description |
|-----------|-------------|
| `node_id` | Parent node to analyze |
| `concepts` | List of terms to map (2-35 required) |
| `core_concept` | Central theme (defaults to node name) |
| `scope` | Search scope for analysis |
| `format` | `png` (default) or `jpeg` |
| `title` | Custom title for the map |

## CLI Tool: render-concept-map

Generate concept maps from JSON definitions. This standalone tool renders concept maps without requiring Workflowy - perfect for programmatic generation or manual concept definition.

### Basic Usage

```bash
# Generate example JSON
npx tsx src/cli/render-concept-map.ts --example > concepts.json

# Render from JSON file
npx tsx src/cli/render-concept-map.ts --input concepts.json --output map.png

# Pipe JSON from stdin
cat concepts.json | npx tsx src/cli/render-concept-map.ts --output map.png

# Output SVG instead of PNG (for PDF conversion)
npx tsx src/cli/render-concept-map.ts --input concepts.json --svg --output map.svg

# Output raw DOT graph for debugging
npx tsx src/cli/render-concept-map.ts --input concepts.json --dot
```

### CLI Options

| Option | Description |
|--------|-------------|
| `-i, --input <file>` | Input JSON file (use `-` for stdin) |
| `-o, --output <file>` | Output file path |
| `-f, --format <type>` | `png` (default), `jpeg`, or `pdf` |
| `-w, --width <pixels>` | Output width (default: 4000) |
| `-h, --height <pixels>` | Output height (default: 3000) |
| `-d, --dpi <number>` | DPI for rendering (default: 300) |
| `--font-size <number>` | Base font size (default: 14) |
| `--example` | Output example JSON to stdout |
| `--svg` | Output SVG instead of raster image |
| `--dot` | Output DOT graph source |

### JSON Format

```json
{
  "title": "Map Title",
  "core_concept": {
    "label": "Central Concept",
    "description": "Optional description"
  },
  "concepts": [
    {
      "id": "concept-id",
      "label": "Display Label",
      "level": "major",
      "importance": 8
    }
  ],
  "relationships": [
    {
      "from": "core",
      "to": "concept-id",
      "type": "enables",
      "description": "Explanation of why this relationship exists",
      "strength": 0.8,
      "bidirectional": false
    }
  ]
}
```

### Relationship Types

Relationships use a defined vocabulary organized by category:

| Category | Types | Edge Color |
|----------|-------|------------|
| Causal | `causes`, `enables`, `prevents`, `triggers`, `influences` | Blue |
| Structural | `contains`, `part_of`, `instance_of`, `derives_from`, `extends` | Green |
| Temporal | `precedes`, `follows`, `co_occurs` | Orange |
| Logical | `implies`, `contradicts`, `supports`, `refines`, `exemplifies` | Purple |
| Comparative | `similar_to`, `contrasts_with`, `generalizes`, `specializes` | Teal |
| Other | `related_to` | Gray |

### Example: Heidegger Concept Map

```bash
# Generate the example
npx tsx src/cli/render-concept-map.ts --example > heidegger.json

# Render at high resolution
npx tsx src/cli/render-concept-map.ts \
  --input heidegger.json \
  --output heidegger.png \
  --width 4000 \
  --height 3000
```

Output:
```
Rendering: Heidegger's Fundamental Ontology
  Core: Being (Sein)
  Concepts: 9
  Relationships: 11

Concept map saved to: heidegger.png
  Size: 301.0 KB
  Dimensions: 4000x3000 @ 300 DPI
```

## Additional Tools

The server includes full Workflowy management to support your knowledge work:

### Search & Navigation
- `search_nodes` - Find nodes by text
- `get_node` / `get_children` - Navigate structure
- `export_all` - Full outline export

### Content Management
- `create_node` / `update_node` / `delete_node` / `move_node`
- `smart_insert` - Search-and-insert workflow
- Markdown formatting support

### Todo Management
- `create_todo` / `list_todos` / `complete_node` / `uncomplete_node`

### Knowledge Linking
- `find_related` - Discover connections via keyword analysis
- `create_links` - Auto-generate internal links

## Troubleshooting

### Concept map fails silently
- Check concept count (max 35)
- Verify Dropbox permissions include `sharing.write`
- Re-authorize if you changed permissions

### "Not permitted to access this endpoint"
- Missing `sharing.write` permission
- Must re-authorize after adding permissions

### Images upload but don't appear in Workflowy
- Check Claude's response for the Dropbox URL
- Manually add to any node if needed

## Architecture

The project is organized into three distinct layers:

```
src/
├── cli/                    # Command-line interface
│   ├── render-concept-map.ts  # Standalone concept map renderer
│   └── setup.ts            # Interactive credential wizard
│
├── mcp/                    # MCP server for Claude Desktop
│   └── server.ts           # Tool handlers & server setup
│
├── shared/                 # Shared modules (used by both)
│   ├── api/
│   │   ├── workflowy.ts    # Workflowy REST client
│   │   ├── dropbox.ts      # Dropbox image hosting
│   │   └── retry.ts        # Exponential backoff logic
│   ├── utils/
│   │   ├── text-processing.ts   # DOT escaping, parsing, link extraction
│   │   ├── keyword-extraction.ts # Relevance scoring
│   │   ├── cache.ts        # Node caching (30s TTL)
│   │   └── node-paths.ts   # Breadcrumb path building
│   ├── config/
│   │   └── environment.ts  # Env vars & validation
│   └── types/
│       └── index.ts        # TypeScript interfaces (including RelationshipType)
│
└── index.ts                # Entry point (re-exports MCP server)
```

### Module Responsibilities

| Layer | Purpose | Entry Point |
|-------|---------|-------------|
| **CLI** | Standalone concept map rendering | `npx tsx src/cli/render-concept-map.ts` |
| **MCP** | Claude Desktop integration | `npm run mcp:start` |
| **Shared** | Common functionality | Imported by both |

## Development

```bash
# Build
npm run build           # Compile TypeScript

# Test
npm test                # Run 96 tests
npm run test:coverage   # With coverage report

# MCP Server
npm run mcp:start       # Start MCP server
npm run mcp:dev         # Build + start

# CLI Tool
npx tsx src/cli/render-concept-map.ts --example  # Generate example JSON
npx tsx src/cli/render-concept-map.ts --input concepts.json --output map.png

# Aliases
npm start               # Same as mcp:start
```

## License

MIT

## Acknowledgments

- [Anthropic](https://anthropic.com) - Claude and MCP protocol
- [Workflowy](https://workflowy.com) - Outliner and API
- [Dropbox](https://dropbox.com) - Image hosting
- [Graphviz](https://graphviz.org) - Graph visualization
