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

**Status**: ⚠️ Partial. Read tools are idempotent. Writes are mutations against Workflowy — idempotency depends on upstream API. Hard caps now enforced (`MAX_SUBTREE_NODES=10_000`, `MAX_INSERT_CONTENT_LINES=80`, `MAX_REORDER_NODES=200`, `SUBTREE_FETCH_TIMEOUT_MS=20_000`) and surfaced in `truncated` flag + `truncation_reason` envelope on every walk-shaped tool. **Remaining gap**: no `offset`/`cursor` pagination for search_nodes / tag_search; callers narrow via `parent_id` + `max_depth` instead.

**Action items**:
- Add `offset`/`cursor` pagination to search_nodes, tag_search, get_children for callers that need cursor-style iteration rather than scope narrowing.

---

## 3. Transport & Cancellation

Support stdio for maximum compatibility. Add Streamable HTTP for networked deployments.

- **stdio**: Baseline, preferred for Claude Desktop integration
- **Streamable HTTP**: Future, for remote/multi-tenant deployments (SSE deprecated)
- Implement request cancellation and timeouts to prevent resource stranding

**Status**: ✅ stdio implemented; cancellation + per-tool timeouts fully wired. **Remaining gap**: no Streamable HTTP transport.

**Implemented**:
- `CancelRegistry` (generation counter) shared across the server. `cancel_all` bumps the generation; every outstanding tree walk returns partial results on its next checkpoint with `truncation_reason: "cancelled"` within ~50 ms.
- `tool_handler!(name, kind, params, body)` macro wraps every non-diagnostic handler. The `ToolKind` taxonomy (`Read` / `Write` / `Bulk` / `Walk`) selects the wall-clock budget from `defaults::*_TIMEOUT_MS`; the wrapper races the handler future against the cancel guard and the deadline.
- `WorkflowContext { cancel, deadline }` plumbs both signals through `workflows::*` functions so partial-success outcomes (e.g. `InsertContentOutcome::Partial`) are observable from both surfaces.

**Action items**:
- Plan Streamable HTTP transport as future milestone (not blocking v2.x).

---

## 4. Elicitation for Human-in-the-Loop

Use elicitation to fill missing parameters or confirm risky actions. Gate with capability checks.

- Confirm destructive operations (delete, bulk edit) before execution
- Never use elicitation to harvest sensitive data
- Fall back gracefully if host doesn't support elicitation

**Status**: ⚠️ Partial. Elicitation primitive not yet implemented (rmcp 0.16 doesn't expose it). `dry_run` adopted on the highest-impact mutation tools:
- `create_mirror` honours `dry_run=true` via the shared `create_mirror_dry_run` workflow — returns the would-be canonical / target / pillar resolution without writing.
- `bulk_update` honours `dry_run=true` — returns the matched node set without applying the operation.

**Action items**:
- Add `dry_run: Option<bool>` to `delete_node`, `move_node`, `insert_content` for the same preview shape.
- Implement elicitation when rmcp adds support + capability check.

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

**Status**: ✅ Structured error envelope + typed JSON responses; machine-readable error codes via `ProximateCause` enum. **Remaining gap**: no `outputSchema` / `structuredContent` adoption (waiting on rmcp support).

**Implemented**:
- `ProximateCause` enum (`Timeout` / `LockContention` / `CacheMiss` / `UpstreamError` / `Cancelled` / `NotFound` / `AuthFailure` / `InvalidParams` / `Unknown`) ships in the `data.proximate_cause` field of every error response. Callers route on the discrete value, not a parsed hint string.
- Every error response carries the same `{operation, node_id, hint, proximate_cause, error}` envelope — `tool_error` for operational failures, `tool_invalid_params` for validation failures, `workflow_error_to_mcp` for workflow returns. Pinned by `handler_body_validation_uses_structured_envelope_not_bare_invalid_params` + `operational_failures_route_through_tool_error_not_bare_internal_error` + `workflow_error_translation_routes_through_workflow_error_to_mcp`.
- Walk-shaped tool responses are typed JSON with the four-field truncation envelope (`truncated`, `truncation_limit`, `truncation_reason`, `truncation_recovery_hint`) routed through one canonical helper. Pinned by `envelope_construction_routes_through_one_helper_no_inline_fields`.
- Aggregation responses (`compute_project_summary`, `compute_daily_review`) return typed `#[derive(Serialize)]` structs (`ProjectSummary`, `DailyReview`) — the JSON shape is the contract, derived not hand-written.

**Action items**:
- Plan `structuredContent` adoption when rmcp supports `outputSchema`.

---

## 7. Production Instrumentation

Instrument like any production microservice.

- Structured logs with correlation IDs
- Include tool name and invocation ID per request
- Record latency, success/failure counts
- Surface rate limits explicitly so agents can budget calls

**Status**: ✅ Op log + per-tool health histogram + uniform error model close the brief 2026-05-02 visibility gap. **Remaining gaps**: no correlation IDs per invocation, no rate limit info in tool result text (only in `workflowy_status`).

**Implemented**:
- `TracedParams<T>` wrapper records every parameter-deserialization failure to the op log before returning the typed `McpError`. Brief 2026-05-02 named the framework-rejected requests (which never reached the handler body and thus never moved per-tool counters) as the dominant debugging black hole. `TracedParams` closes the gap end-to-end: every rejected call appears in the log, every rejection carries a typed `proximate_cause`.
- `OpLog.last_unrecovered_failure()` self-clears once a success on the same tool lands after the failure, so `degraded` surfaces match what the system is actually doing.
- `per_tool_health` histogram over the most recent 200 op-log entries reports `{total, ok, err, ok_rate, status}` per tool. Status thresholds: `healthy ≥ 75%`, `degraded ≥ 50%`, `failing < 50%`.
- `paths` map in `workflowy_status` gives a flat tool→health-status view for callers that want to gate routing decisions without parsing the histogram.
- Every parameter struct carries `#[serde(deny_unknown_fields)]` so a field-name typo fails fast with a typed error rather than a silent default.

**Action items**:
- Generate a request_id per tool invocation, include in all log spans.
- Return rate limit info in tool result text when approaching limits (currently only surfaced via `workflowy_status`).
- Add metrics counters (tool_calls_total, tool_errors_total, tool_duration_seconds).

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

**Status**: ✅ 364 unit tests + integration suite + pin-tested invariants. **Remaining gap**: no fault-injection harness for upstream API errors; relies on the `tests/live_insert.rs` integration test (gated by `WORKFLOWY_API_KEY`) for end-to-end coverage.

**Implemented**:
- `cargo test --lib` runs 364 unit tests in ~40 s covering every tool handler, workflow, aggregation helper, parameter validation, and pin-tested invariant. Tests are per-module `#[cfg(test)]` blocks alongside source.
- Pin tests grep the source at build time to enforce consistency rules (envelope adoption, error helper routing, CLI/MCP parity, aggregation helper adoption). See the constitution's Helper-First Construction table for the full list.
- Live-integration test in `tests/live_insert.rs` exercises real Workflowy API paths when `WORKFLOWY_API_KEY` is set.
- Daily Claude Desktop usage is the de-facto primary-host smoke test; failure modes get filed as `principles-architecture.md` incident comments and pinned by tests.

**Action items**:
- Add fault-injection harness for upstream 429 / 500 / malformed JSON — currently only the live test exercises real failure shapes.

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

**Status**: ✅ Tools are focused; input validation is enforced at the boundary via the `NodeId` newtype and `#[serde(deny_unknown_fields)]` on every parameter struct.

**Implemented**:
- `NodeId` newtype hand-written `Deserialize` rejects the literal strings `"null"` / `"undefined"` and whitespace-only at the parameter boundary, before the handler body runs. Empty string is preserved as the workspace-root sentinel for handlers that special-case it (`list_children`, `insert_content` etc.).
- Every parameter struct in `src/server/params.rs` carries `#[serde(deny_unknown_fields)]` so a typo'd field name fails fast with a recorded `invalid_parameters at \`.field_name\`: unknown field` error rather than silently defaulting.
- `Parameters<T>` wrapper routes deserialisation failures through `serde_path_to_error` so the error path names the offending field — pinned by `null_required_uuid_field_error_names_the_field` and the `literal_null_string_*` companions.
- `insert_content` enforces the `MAX_INSERT_CONTENT_LINES` cap at the workflow level; oversized payloads return a typed `InvalidInput` with a chunking instruction.

**Action items**:
- Add max-length / control-character validation for free-text inputs (`name`, `description`, `content`) — currently bounded by Workflowy API limits, not validated client-side.

---

## 15. Explicit Consent for Impactful Actions

Require confirmation for state changes. Provide dry-run mode.

- delete_node, move_node, bulk edit_node = high-impact operations
- Return a diff/preview of intended changes before execution
- Use structured content for machine-readable change summaries

**Status**: ⚠️ Partial. `dry_run` adopted on `create_mirror` (returns the would-be canonical / target / pillar resolution) and `bulk_update` (returns the matched node set without applying). Walk-shaped tools naturally surface a preview shape via the `truncated` + `truncation_reason` envelope. **Remaining gap**: `delete_node`, `move_node`, `insert_content` execute immediately.

**Action items**:
- Add `dry_run: Option<bool>` to `delete_node`, `move_node`, `insert_content` for symmetric preview surface.
- Include "this action is irreversible" warning in the `delete_node` tool description string.
- Consider requiring a confirmation-string parameter on `delete_node` (e.g. `confirm_delete: true`) for the highest-impact mutation.

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
