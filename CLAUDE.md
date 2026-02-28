# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
npm install                  # install dependencies
npm run build                # compile TypeScript (tsc)
npm test                     # run all tests (vitest run)
npm run test:watch           # watch mode
npm run test:coverage        # coverage report
npx vitest run src/mcp/find-node.test.ts  # run a single test file
npm run mcp:start            # start MCP server (requires build first)
npm run mcp:dev              # build + start MCP server
```

## Architecture

TypeScript MCP server for Workflowy content management and concept mapping. Uses `@modelcontextprotocol/sdk` over stdio transport for Claude Desktop integration.

### Three Layers

- **`src/mcp/server.ts`** — The main file (~3800 lines). Contains all 24+ MCP tool definitions, handlers, and orchestration logic. Tools are registered inline via `ListToolsRequestSchema`; Zod schemas validate inputs in each handler.
- **`src/shared/`** — Reusable modules: API clients, utilities, config, types.
- **`src/cli/`** — Standalone CLI for concept map generation (uses Anthropic SDK + Graphviz WASM).

### Request Flow

```
MCP tool call → Zod validation → getCachedNodes() → Workflowy API → invalidateCache()
```

All Workflowy API calls go through `workflowyRequest()` in `src/shared/api/workflowy.ts`, which wraps every request with exponential backoff retry (3 attempts, retries on 429/5xx).

### Content Insertion Pipeline

Content must be 2-space indented text. The pipeline:

1. **Markdown input** → `convertLargeMarkdownToWorkflowy()` (`large-markdown-converter.ts`) converts to indented format
2. **Size routing** → `parallelInsertContent()` auto-selects: <20 nodes → single insert, ≥20 → parallel (up to 5 workers via `orchestrator.ts`)
3. **Subtree splitting** → `splitIntoSubtrees()` groups indent-0 nodes into ~50-node chunks
4. **Staging pattern** → `insertHierarchicalContent()` creates a `__staging_temp__` node, builds hierarchy under it, moves children to real parent, deletes staging node

### Key Infrastructure

- **Node Cache** (`cache.ts`): Module-level singleton, 30s TTL, fetches from `/nodes-export` on miss. `startBatch()`/`endBatch()` bracket bulk writes; >20% invalidation triggers full clear.
- **Rate Limiter** (`rateLimiter.ts`): Token bucket, 5 req/sec, burst 10.
- **Request Queue** (`requestQueue.ts`): Max 3 concurrent requests, 50ms batch delay, 20 ops/batch. Used by `batch_operations`.
- **Job Queue** (`jobQueue.ts`): Background jobs that survive across tool calls. Three registered executors: `insert_content`, `batch_operations`, `insert_file`. Claude submits via `submit_job`, polls with `get_job_status`.

### Workflowy API Constraints

- `/nodes-export` (bulk export) is rate-limited to 1 req/min by Workflowy
- All other endpoints use `/nodes` or `/nodes/{id}`
- API base: `https://workflowy.com/api/v1`

## Testing

Tests use Vitest with `globals: true`. All tests mock `workflowyRequest` — no live API calls. Test files live alongside source in `src/mcp/*.test.ts` and `src/shared/utils/*.test.ts`.

## Configuration

Environment variables loaded from `.env` via dotenv (`src/shared/config/environment.ts`):
- `WORKFLOWY_API_KEY` (required)
- `DROPBOX_APP_KEY`, `DROPBOX_APP_SECRET`, `DROPBOX_REFRESH_TOKEN` (for concept map image hosting)
- `ANTHROPIC_API_KEY` (for CLI `--auto` concept extraction)

## Module System

ES modules (`"type": "module"` in package.json). TypeScript uses `NodeNext` module resolution. Imports use `.js` extensions (e.g., `import "./mcp/server.js"`).
