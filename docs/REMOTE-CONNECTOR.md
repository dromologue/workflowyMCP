# Remote MCP connector for claude.ai (web + desktop + mobile)

This server can run as a **remote custom connector** for claude.ai, in addition
to the stdio binary used by Claude Desktop. The connector serves the same
Workflowy tool surface over the MCP **Streamable HTTP** transport, behind an
**OAuth 2.1** resource-server gate.

- **Binary:** `workflowy-mcp-http` (`src/bin/mcp_http.rs`)
- **Transport:** rmcp Streamable HTTP, mounted in axum at `/mcp`
- **Auth:** the server validates bearer JWTs against a managed provider's JWKS;
  the provider runs the authorize/token/dynamic-client-registration endpoints
  that claude.ai drives. The server publishes RFC 9728 protected-resource
  metadata at `/.well-known/oauth-protected-resource`.
- **Hosting:** Fly.io (persistent container + volume for the name index).

The stdio binary (`workflowy-mcp-server`) is unchanged — local Claude Desktop
keeps working exactly as before. Both transports share `build_and_spawn` in
`src/server/mod.rs`, so server construction and the background name-index tasks
cannot drift between them.

> **Phase 1 is single-tenant.** The Workflowy API key is a deployment secret, so
> the connector always acts as one Workflowy account. A valid OAuth token proves
> *authentication against your IdP* — not that the caller is the account owner —
> so set **`MCP_ALLOWED_SUBJECTS`** to your own OAuth `sub` to turn the gate into
> authorisation (see §2.5). Multi-tenant (bring-your-own Workflowy key,
> per-tenant state isolation) is a planned Phase 2.
>
> **Authentication ≠ authorisation.** With `MCP_ALLOWED_SUBJECTS` empty the
> connector authorises *any* token valid for your issuer/audience — if your
> provider allows open sign-up, anyone who finds the URL gets full read/write
> (incl. `delete_node` / `bulk_update`). Always set the subject allow-list on a
> public deployment.

## 1. Endpoints

| Path | Auth | Purpose |
| --- | --- | --- |
| `/mcp` | Bearer JWT required | MCP Streamable HTTP endpoint (the connector URL) |
| `/.well-known/oauth-protected-resource` | public | RFC 9728 discovery: names the authorization server |
| `/healthz` | public | Liveness probe for Fly health checks |

A request to `/mcp` without a valid token returns **401** with a
`WWW-Authenticate: Bearer ... resource_metadata="https://<host>/.well-known/oauth-protected-resource"`
header, which is how claude.ai discovers where to begin the OAuth flow.

## 2. Set up the OAuth provider (authorization server)

Pick a DCR-capable provider — claude.ai requires **dynamic client registration**
and **port-agnostic localhost callbacks**. Recommended: **WorkOS AuthKit**
(`workos.com` — first-class MCP docs, implements the authorize/token endpoints,
free tier). Alternatives that also support DCR: **Stytch** (`stytch.com`),
**Auth0** (`auth0.com`), **Scalekit**.

In the provider, you need:
1. **Enable Dynamic Client Registration** (in WorkOS AuthKit: the
   Docs → AuthKit → Model Context Protocol integration).
2. **Get the exact issuer + JWKS from the discovery document** — do NOT
   hand-construct the URLs (paths differ per provider). Fetch:

   ```bash
   curl https://<your-provider-domain>/.well-known/oauth-authorization-server
   ```

   Copy `"issuer"` → `MCP_OAUTH_ISSUER` and `"jwks_uri"` → `MCP_OAUTH_JWKS_URL`.
3. Redirect URIs that accept **both** `http://localhost/callback` and
   `http://127.0.0.1/callback` with **port-agnostic** matching — claude.ai web,
   desktop, mobile, and Cowork each use a different ephemeral localhost port.
4. The token **audience** set to this connector's resource id
   (`<MCP_PUBLIC_BASE_URL>/mcp`) → `MCP_OAUTH_AUDIENCE` (defaults to that value).

## 2.5. Lock the connector to your identity (`MCP_ALLOWED_SUBJECTS`)

A valid token only proves the caller authenticated against *your* provider — not
that they are *you*. If the provider permits open sign-up, that is not enough.
Pin the connector to your OAuth subject:

1. Deploy (or run) with the allow-list **unset** the first time — the gate is
   permissive but logs the authenticated subject on every call.
2. Connect from claude.ai once, then read your subject from the logs:

   ```bash
   fly logs --app <app> | grep 'authenticated; allow-list empty'
   # → ... subject="user_01XXXX..." authenticated; allow-list empty ...
   ```

3. Set the allow-list to that subject (a restart, not a rebuild):

   ```bash
   fly secrets set --app <app> MCP_ALLOWED_SUBJECTS="user_01XXXX..."
   ```

From then on, a token whose `sub` is absent or unlisted is refused with **403**
(authenticated, but not authorised). Multiple owners → comma-separate the list.
The server logs a stark startup warning whenever the gate is active but the
allow-list is empty.

### JWKS hardening (no configuration required)

The resource server fetches the provider JWKS with a 5 s timeout (a hanging
provider can't stall the auth middleware) and refetches at most once per 60 s on
an unknown `kid`, so a flood of tokens bearing random `kid`s can't amplify into
unbounded outbound JWKS fetches. Legitimate key rotation still converges within
one cooldown window.

## 3. Deploy to Fly.io

```bash
# One-time: copy the template and set a unique app name, then:
cp fly.toml.example fly.toml      # edit `app` to your globally-unique name
fly apps create <app>

# Secrets (never commit these):
fly secrets set --app <app> \
  WORKFLOWY_API_KEY=wf_xxx \
  MCP_OAUTH_ISSUER=https://your-tenant.provider.com \
  MCP_OAUTH_JWKS_URL=https://your-tenant.provider.com/oauth2/jwks \
  MCP_PUBLIC_BASE_URL=https://<app>.fly.dev

fly deploy
```

The default `fly.toml` runs **volumeless**: the name index lives in memory and
rebuilds opportunistically from tool-call walks (one less moving part, and the
machine can be placed in any region without a volume host-pin). To enable
persistence instead, add a `[mounts]` block to `fly.toml`, set
`WORKFLOWY_INDEX_PATH=/data/name_index.json`, and
`fly volumes create connector_data --size 1 --region <region>` — note a
volume pins the machine to one region's host, so pick a region with capacity.

Fly terminates TLS, so the connector is reachable at `https://<app>.fly.dev`.
Confirm discovery is live:

```bash
curl https://<app>.fly.dev/.well-known/oauth-protected-resource
# → {"resource":"https://<app>.fly.dev/mcp","authorization_servers":["https://..."],...}
```

## 4. Add the connector in claude.ai

claude.ai → **Settings → Connectors → Add custom connector** → paste
`https://<app>.fly.dev/mcp` → complete the OAuth consent. The Workflowy tools
then appear in chats. Repeat in the desktop app (same account, same connector).

## Using the connector

Once connected, the full Workflowy tool surface (search, navigate, create, edit,
todos, scheduling, mirrors, …) is available in any claude.ai chat. A first smoke
test:

> *"Use the Workflowy connector: run a health check, then show me 5 nodes under
> &lt;a parent node id you know&gt;."*

Two habits make sessions reliable — the tree is large and the Workflowy API is
rate-limited:

- **Scope reads; never walk the whole workspace.** Pass an explicit `parent_id`
  (a UUID), or run `build_name_index` once and then `search_nodes` /
  `find_node` with `use_index=true`. An unscoped search from the root times out
  against the 20 s / 10 000-node budget and the server will refuse it. Prefer the
  scoped aggregates (`list_todos`, `daily_review`, `get_node`, `list_children`).
  Set `WORKFLOWY_REVIEW_ROOT` (a Fly secret) so `review` / `audit_mirrors` have a
  default scope, or pass `root_id` each call.
- **Respect rate limits.** Call `workflowy_status` *once*; if it reports
  `rate_limited` with a `retry_after`, wait that long and don't poll again —
  each probe consumes a token from the window you're waiting on.

- **Route scoped reads through `read_batch` on this connector.** **Confirmed in
  production (claude.ai web):** the host strips bare-string id parameters before
  they reach the server, so a scoped `search_nodes` / `find_node` /
  `build_name_index` call arrives with no `parent_id` and collapses to a refused
  root walk — it looks like the server rejecting your scope, but the id never
  arrived. `read_batch` nests the id *inside* the operations object, where it
  survives intact, so it is the reliable read path until the host-side stripping
  is fixed upstream. (This is why the `wflow` / `wflow-connector` skills route
  connector reads through `read_batch` by default.)

> **Skill (optional):** the repo's `templates/skills/wflow/` skill turns the raw
> tools into a second-brain workflow (capture, triage, distil, review). Bundle it
> with `scripts/bundle-skill.sh` and upload to claude.ai → Settings → Skills; it
> applies the same scoping/rate-limit discipline automatically.

## 5. Local testing

With a provider test tenant:

```bash
export WORKFLOWY_API_KEY=wf_xxx
export MCP_OAUTH_ISSUER=... MCP_OAUTH_JWKS_URL=... MCP_PUBLIC_BASE_URL=http://localhost:8080
cargo run --bin workflowy-mcp-http
curl -i -X POST http://localhost:8080/mcp           # → 401 + WWW-Authenticate
curl    http://localhost:8080/.well-known/oauth-protected-resource   # → 200 JSON
```

To smoke-test the tool surface **without** a provider, bypass the gate (local
only):

```bash
MCP_AUTH_DISABLED=1 WORKFLOWY_API_KEY=wf_xxx cargo run --bin workflowy-mcp-http
```

> **Never set `MCP_AUTH_DISABLED=1` in a public deployment.** The tool surface
> includes `delete_node` and `bulk_update`; an unauthenticated public endpoint
> is full read/write access to the Workflowy account. The server logs a stark
> warning at startup when the gate is disabled and **fails closed** otherwise:
> if the OAuth env vars are missing and `MCP_AUTH_DISABLED` is not `1`, the
> binary refuses to start.

## 6. Configuration reference

See `.env.example`. Connector-only vars: `BIND_ADDR`, `PORT`,
`MCP_OAUTH_ISSUER`, `MCP_OAUTH_JWKS_URL`, `MCP_PUBLIC_BASE_URL`,
`MCP_OAUTH_AUDIENCE`, `MCP_ALLOWED_SUBJECTS` (identity lock — see §2.5),
`MCP_ALLOWED_HOSTS`, `MCP_ALLOWED_ORIGINS`,
`MCP_AUTH_DISABLED`. Shared with the stdio binary: `WORKFLOWY_API_KEY`
(required), `WORKFLOWY_INDEX_PATH`, `SECONDBRAIN_DIR`, `WORKFLOWY_REVIEW_ROOT`
(default scope for `review` / `audit_mirrors`; no hardcoded fallback).

## 7. Troubleshooting (gotchas hit in a real WorkOS AuthKit + Fly bring-up)

The OAuth flow has several gates in series; a failure surfaces in claude.ai as a
generic *"Authorization with the MCP server failed"*. The **Fly logs are the
real diagnostic** (`fly logs --app <app>`). Two non-obvious blockers, in the
order you hit them:

1. **`error=InvalidAudience` — the token `aud` is not your resource URL.**
   WorkOS AuthKit stamps the access-token `aud` with a **default value unique to
   the environment — in practice the WorkOS `client_id`**, *not* the
   resource-indicator URL, unless the client requests the resource and AuthKit
   matches it. The server therefore accepts `MCP_OAUTH_AUDIENCE` as a
   **comma-separated list**; set it to both the resource id and the client id:

   ```bash
   fly secrets set --app <app> \
     MCP_OAUTH_AUDIENCE="https://<app>.fly.dev/mcp,client_01XXXXXXXXXXXXXXXXXXXXXXXX"
   ```

   The build logs the actual `aud` on mismatch (`token aud [...] matches none of
   accepted [...]`) so you can read the exact value and pin it. Accepting the
   `client_id` as audience is sound for a single-tenant connector (the signature
   and issuer checks already prove the token came from *your* AuthKit
   environment for *your* Claude client).

2. **`rejected request with disallowed Host header (possible DNS rebinding
   attempt)` — rmcp only allows localhost by default.** The rmcp
   `StreamableHttpService` has a DNS-rebinding guard that rejects any `Host`
   header that isn't localhost unless you configure an allow-list. Add your
   public hostname (this is a restart, not a rebuild):

   ```bash
   fly secrets set --app <app> MCP_ALLOWED_HOSTS="<app>.fly.dev"
   ```

   (If a later failure shows a rejected *Origin*, set `MCP_ALLOWED_ORIGINS`
   likewise — though claude.ai calls server-side and usually sends no `Origin`.)

A successful connect logs `create new session` + `Service initialized as server`
with `client_info: … name: "Anthropic/ClaudeAI"` and no rejections.
