# MCP Server Production Principles

> Operational principles for building production-grade MCP servers.
> Derived from [15 Best Practices for Building MCP Servers in Production](https://thenewstack.io/15-best-practices-for-building-mcp-servers-in-production/) (Sep 2025), adapted to our Workflowy MCP server context.

---

## 1. Bounded Context

Model the server around a single domain. Expose only capabilities that belong to that domain.

- **Our domain**: Workflowy content management (CRUD, search, hierarchy)
- Tools are cohesive and uniquely named
- All inputs have JSON Schema (via `schemars` derives with descriptions)
- Tool descriptions document failure modes
- No cross-domain leakage (e.g. no filesystem, no email)

**Status**: ✅ Implemented. Review tool descriptions for completeness.

---

## 2. Stateless, Idempotent Tool Design

Agents may retry or parallelize requests. Design for this.

- Read tools (search, get, list) are naturally idempotent
- Write tools should accept client-generated IDs where the API supports it
- Return deterministic results for the same inputs
- Use pagination tokens and cursors for list operations
- Keep responses small and predictable

**Status**: ⚠️ Partial. Read tools are idempotent. Writes are mutations against Workflowy — idempotency depends on upstream API. **Gaps**: No pagination for search_nodes or tag_search. No result size caps documented.

**Action items**:
- Add `offset`/`cursor` pagination to search_nodes, tag_search, get_children
- Document max response sizes per tool
- Add `max_results` enforcement with hard cap

---

## 3. Transport & Cancellation

Support stdio for maximum compatibility. Add Streamable HTTP for networked deployments.

- **stdio**: Baseline, preferred for Claude Desktop integration
- **Streamable HTTP**: Future, for remote/multi-tenant deployments (SSE deprecated)
- Implement request cancellation and timeouts to prevent resource stranding

**Status**: ⚠️ stdio implemented. **Gaps**: No cancellation handling. No per-tool timeouts. No Streamable HTTP transport.

**Action items**:
- Add tokio timeout wrappers for each tool handler (configurable default)
- Plan Streamable HTTP transport as future milestone (not blocking v2.0)

---

## 4. Elicitation for Human-in-the-Loop

Use elicitation to fill missing parameters or confirm risky actions. Gate with capability checks.

- Confirm destructive operations (delete, bulk edit) before execution
- Never use elicitation to harvest sensitive data
- Fall back gracefully if host doesn't support elicitation

**Status**: ❌ Not implemented. rmcp may not support elicitation yet (June 2025 MCP spec feature).

**Action items**:
- For now: add `dry_run: Option<bool>` parameter to delete_node, move_node
- When dry_run=true, return a preview of what would happen without executing
- Implement elicitation when rmcp adds support + capability check

---

## 5. Security First

Follow MCP security best practices. OAuth 2.1 mandatory for HTTP transports.

- stdio uses Bearer token auth (appropriate — no OAuth needed)
- Non-predictable session identifiers (N/A for stdio)
- Never echo secrets in tool results or logs
- Minimize data exposure in responses

**Status**: ✅ Mostly implemented via existing security principles. Bearer token auth for Workflowy API. Tracing to stderr only. No secrets in responses.

**Action items**:
- Audit all error messages for accidental secret leakage
- When adding Streamable HTTP: implement OAuth 2.1
- Validate node IDs are UUID format before sending to API (prevent injection)

---

## 6. Dual UX: Agent-Parsable + Human-Readable

Responses must be LLM-parsable AND human-readable.

- Use structured content with JSON schemas for model consumption
- Keep error messages actionable with machine-readable codes
- Use `outputSchema` / `structuredContent` (June 2025 spec) when supported

**Status**: ⚠️ Responses are markdown-formatted (human-readable). **Gaps**: No structured/typed output. No machine-readable error codes.

**Action items**:
- Define error code enum (e.g. `NODE_NOT_FOUND`, `RATE_LIMITED`, `VALIDATION_FAILED`)
- Include error code in all error responses
- Plan structuredContent adoption when rmcp supports outputSchema

---

## 7. Production Instrumentation

Instrument like any production microservice.

- Structured logs with correlation IDs
- Include tool name and invocation ID per request
- Record latency, success/failure counts
- Surface rate limits explicitly so agents can budget calls

**Status**: ⚠️ Have structured tracing. **Gaps**: No correlation IDs per invocation. No latency recording. No rate limit info in responses.

**Action items**:
- Generate a request_id per tool invocation, include in all log spans
- Add timing spans around API calls
- Return rate limit info in tool results when approaching limits
- Add metrics counters (tool_calls_total, tool_errors_total, tool_duration_seconds)

---

## 8. Version & Advertise Capabilities

Semantic versioning for server and tools. Publish capabilities at handshake.

- Server version via `env!("CARGO_PKG_VERSION")` in ServerInfo
- Tool list published via `enable_tools()` capability
- Semantic versioning in Cargo.toml

**Status**: ✅ Basic versioning and capability advertisement in place.

**Action items**:
- Add changelog tracking for tool schema changes
- Consider tool-level versioning if/when breaking tool schemas

---

## 9. Decouple Prompts, Tools, Resources

Store reusable prompts server-side. Treat resources as read-only context surfaces.

- Tools are independent and composable
- No hardcoded templates in tool handlers
- Resources (if exposed) have explicit URIs and pagination

**Status**: ✅ Tools are independent. No prompts/resources interface yet.

**Action items**:
- Consider exposing MCP prompts for common workflows (e.g. "daily review", "project summary")
- Consider exposing MCP resources for frequently accessed nodes

---

## 10. Handle Large Outputs Responsibly

Don't inline megabytes into a single tool result.

- Truncate large payloads with a continuation indicator
- Return handles/URIs instead of full content for large trees
- Advertise total counts where feasible

**Status**: ❌ get_subtree and search could return unbounded payloads.

**Action items**:
- **Hard cap**: All text responses limited to ~50KB, truncated with "... (truncated, N more items)"
- get_subtree: enforce max_depth default (e.g. 3), paginate beyond that
- search_nodes: enforce max_results hard cap (e.g. 100)
- Return `total_count` alongside paginated results

---

## 11. Test with Real Hosts & Failure Injection

Validate against multiple MCP clients. Inject faults.

- Test with Claude Desktop (stdio)
- Test with MCP Inspector tool
- Inject: slow API responses, partial failures, malformed inputs, rate limiting

**Status**: ❌ No tests for Rust version yet.

**Action items**:
- Unit tests for each tool handler (mock WorkflowyClient)
- Integration test with MCP Inspector
- Fault injection tests (timeout, 429, 500, malformed JSON)
- Test with Claude Desktop as primary host

---

## 12. Package Like a Microservice

Containerize, declare transport, publish minimal images.

- Binary distribution (Rust compiles to single binary — good)
- README with tool catalog, schemas, examples, security notes

**Status**: ⚠️ Binary builds. **Gaps**: No Dockerfile. README needs MCP-specific tool catalog.

**Action items**:
- Create Dockerfile (multi-stage build, minimal runtime image)
- Update README with tool catalog table (name, description, params, examples)
- Add installation/configuration docs for Claude Desktop

---

## 13. Respect Platform Realities

Capabilities differ by host. Graceful degradation for unsupported features.

- stdio works everywhere — our baseline
- Don't depend on features not universally supported
- Feature flags for optional capabilities

**Status**: ✅ stdio-only, no dependency on advanced features.

---

## 14. API Design Fundamentals

Behind the MCP layer, keep the domain API clean.

- Least-privilege operations (each tool does one thing)
- Clear resource lifecycles (create → read → update → delete)
- Idempotent mutations where possible
- Validate all inputs at system boundary

**Status**: ⚠️ Tools are focused. **Gaps**: Node ID validation is missing. No input sanitization beyond schema.

**Action items**:
- Validate node_id format (UUID) before API calls
- Validate text inputs: max length, no null bytes, no control characters
- Sanitize content input for insert_content

---

## 15. Explicit Consent for Impactful Actions

Require confirmation for state changes. Provide dry-run mode.

- delete_node, move_node, bulk edit_node = high-impact operations
- Return a diff/preview of intended changes before execution
- Use structured content for machine-readable change summaries

**Status**: ❌ All mutations execute immediately with no preview.

**Action items**:
- Add `dry_run: Option<bool>` to delete_node, move_node, insert_content
- dry_run returns what would happen without executing
- Include "this action is irreversible" warning in delete tool description
- Consider requiring confirmation string for delete (e.g. "confirm_delete: true")

---

## Priority Matrix

| Priority | Principle | Effort | Impact |
|----------|-----------|--------|--------|
| P0 | #11 Testing | High | Critical |
| P0 | #2 Pagination | Medium | High |
| P0 | #10 Output caps | Low | High |
| P0 | #14 Input validation | Low | High |
| P1 | #15 dry_run for deletes | Low | Medium |
| P1 | #7 Correlation IDs | Low | Medium |
| P1 | #6 Error codes | Low | Medium |
| P1 | #3 Timeouts | Low | Medium |
| P2 | #12 Dockerfile | Low | Low |
| P2 | #4 Elicitation | Medium | Low (blocked) |
| P2 | #9 Prompts/Resources | Medium | Low |
| P3 | #3 Streamable HTTP | High | Future |

---

## See Also

- [Architecture Principles](./principles-architecture.md) — Structural guidance
- [Security Principles](./principles-security.md) — Security requirements
- [Development Principles](./principles-development.md) — Code-level guidance
