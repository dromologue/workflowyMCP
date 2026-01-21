# Constitution

> Non-negotiable principles governing the Workflowy MCP Server project.

## Detailed Principles

This constitution establishes core principles. For detailed guidance, see:

- **[Architecture Principles](./principles-architecture.md)** - Structural patterns, module boundaries, data flow
- **[Development Principles](./principles-development.md)** - Code style, testing approach, patterns to follow
- **[Security Principles](./principles-security.md)** - Secrets management, input validation, audit requirements

---

## Mission

**AI-native outlining**: A seamless bridge between Claude's intelligence and Workflowy's structure, enabling natural language interaction with hierarchical knowledge.

## Core Principles

### 1. Public Utility First

This is an open-source tool for the broader community. Design decisions must consider:
- Diverse use cases beyond the original author's workflow
- Clear documentation for self-service adoption
- Accessibility to developers of varying skill levels

### 2. Resilient by Default

The server must gracefully handle failures:
- Retry transient API failures with exponential backoff
- Never silently lose user data or content
- Graceful degradation when Workflowy API is unavailable
- Clear error messages that guide resolution

### 3. Strict Type Safety

TypeScript must be used to its full potential:
- No `any` types without explicit justification
- Strict null checks enabled
- Exhaustive type coverage for all API interfaces
- Zod schemas for runtime validation of external data

### 4. Paranoid Security

Sensitive data must be protected at all layers:
- API keys and credentials never logged, even at debug level
- Environment-based configuration only (no hardcoded secrets)
- All sensitive files gitignored by default
- No user content in error messages or logs
- Audit trail for destructive operations (delete, move)

### 5. Comprehensive Testing

Quality is non-negotiable:
- Unit tests for all business logic
- Integration tests for Workflowy API interactions
- >80% code coverage target
- Tests must pass before merge

### 6. Semver Strict Compatibility

Users must be able to trust version numbers:
- Breaking changes only in major versions
- Deprecation warnings before removal
- Changelog maintained for all releases
- Migration guides for major version upgrades

## Design Philosophy

### Smart Workflows Over Atomic Operations

Tools should handle multi-step operations intelligently:
- `smart_insert` over raw `create_node` for common cases
- Workflows that reduce cognitive load on users
- Sensible defaults that work for 80% of cases

### Smart Defaults with Override

When requests are ambiguous:
- Make reasonable assumptions based on context
- Clearly communicate what assumption was made
- Allow explicit override when needed
- Never block on clarification for recoverable decisions

### Indentation as Structure

Hierarchical content parsing:
- Spaces and tabs define nesting (2 spaces or 1 tab = 1 level)
- Simple, predictable, no magic parsing
- What you see is what you get

### User-Controlled Rate Limiting

Respect user agency:
- Expose configuration for rate limit behavior
- Cache aggressively but transparently
- Let users choose between speed and freshness

## Performance Targets

- Tool response time: <2 seconds for typical operations
- Cache TTL: Configurable, default 30 seconds
- Prioritize correctness over speed when in conflict

## Documentation Standards

Developer-focused documentation:
- JSDoc comments for all public functions
- Architecture decision records for significant choices
- Contribution guide for external contributors
- API reference for all MCP tools

## Architecture Constraints

### Closed Core

- Curated, stable tool set
- No plugin system or dynamic tool loading
- Quality over quantity in tool offerings
- Each tool must have clear, distinct purpose

### Technology Stack

- Runtime: Node.js 18+
- Language: TypeScript (strict mode)
- MCP SDK: Official `@modelcontextprotocol/sdk`
- Validation: Zod
- Transport: stdio (Claude Desktop compatible)

## What We Will NOT Do

- Support authentication methods other than API key
- Implement real-time sync or webhooks
- Build a web UI or standalone application
- Support Workflowy features not exposed via API
- Compromise security for convenience
