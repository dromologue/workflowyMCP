# Which surface, and why

WorkFlowy has been building its own tooling quickly, and several things this
server was once the only way to do are now native. That is a good outcome and
this page is the honest accounting of it: what each surface can do as of
**August 2026**, what is left that only this repository provides, and the one
routing rule that is not a preference but a constraint.

> **Open proposal (2026-09-05).** A re-survey and a measured proposal on what
> should remain in this repository now that the native tools carry most daily
> work lives in
> [`proposals/2026-09-05-what-stays-local.md`](proposals/2026-09-05-what-stays-local.md).
> It is not yet decided; this page stays canonical for current routing until it
> is.

If you read only one paragraph, read this one. **On a laptop, reach for the
native tools first for reading, searching, and ordinary writing; reach for this
server for the claim-based method, for anything scheduled, and for anything
that has to run where WorkFlowy's own tools cannot reach.** The last category
is not a matter of taste. A cloud sandbox and a phone have no desktop app and
no shell, and a routine that fires at 04:15 with nobody watching cannot fall
back to a surface that requires either.

## The five surfaces

| Surface | What it is | Reach for it when |
|---|---|---|
| **WorkFlowy Pro AI** | Chat with your notes, AI Nodes, AI Quick Actions, inside the WorkFlowy client itself. | You want a question answered or a passage rewritten while you are already in the outline, with nothing wired up. |
| **`wf` CLI** (`workflowy-cli` 3.3.x) | One binary with a local SQLite and full-text cache of the whole tree, plus an MCP server built in (`wf mcp`). Third-party, MIT, endorsed on workflowy.com. | Reading, searching, todos, tags, ordinary writes, nested writes via `doc:edit`, change streams via `watch:start`, webhooks. The best general-purpose index of the five. |
| **WorkFlowy desktop MCP** | The MCP embedded in the desktop app, driving the already-synced client on a stable localhost port. | Filtered traversal (`tree_seek`), batched nested writes under advisory leases (`tree_patch`), attachments, moving your actual client, screen capture. No REST rate limit at all. |
| **This server** (`mcp__workflowy__*`, `wflow-do`) | Rust, REST-backed, headless, with a persistent index you can withhold subtrees from. | The claim-based method: mirror discipline and drift auditing, the second-brain review, the `$SECONDBRAIN_DIR` layer, scheduled reindexing, and every failure contract. |
| **A remote connector** | This same binary behind an HTTP shim, serving claude.ai web and mobile and any cloud sandbox. | Anything that runs somewhere the four surfaces above cannot be installed. Nothing else here substitutes for it. |

## What went native, and what that costs this repo

Three things changed in 2026 that a reader of an older version of this
documentation would get wrong.

**The desktop MCP is no longer just attachments and zoom.** It now carries
`tree_seek`, which traverses from any anchor upward or downward with filters on
regular expressions, creation and modification windows, completion dates, URLs,
and child counts, and which can block until a match appears. It carries
`tree_patch`, which applies a batch of nested inserts, moves, deletes,
completions and renames in one call, with per-item block formats. Both run
against the client's already-synced copy, so they cost no REST quota and cannot
trip the rate limit. It also carries advisory **leases** on individual items,
which is a genuine concurrency primitive that this server does not have and
that matters the moment two writers share a tree. If the desktop app is running
and the work is interactive, that is now the better surface for most reads and
for many writes, and the older advice to come straight back here after
attachments is out of date.

**The `wf` CLI has grown past a search tool.** Alongside the cache it now has
`doc:edit` for nested outline writes in a single call, `tags` and `todos`
queries, node templates, bulk operations, a `watch` daemon streaming changes as
NDJSON, webhooks, and an `ai:propose` and `ai:apply` pair that previews an
LLM-authored diff before it lands. On a 235,000-node workspace the first sync
took under twenty seconds and searches answer in under 100 ms with the ancestor
path attached, which this server's index does not store.

**Native live mirrors exist, on the beta API.** `wf mirror:create`,
`mirror:info` and `mirror:remove` drive WorkFlowy's real mirror primitive:
one node, several places, genuinely synchronised rather than copied. Verified
on 2026-08-17, they remain **beta-only**: the same call against production
returns `beta_api_required`, and the production API reference documents no
mirror endpoint. See the mirror section below, because this is the piece of the
method most affected.

The consequence is stated plainly rather than defended. The native tools have
won the index-and-lookup half outright, and are now competitive on the
ordinary-write half. If your whole use case is "let Claude look things up in my
outline and add the odd node", install the CLI, wire in `wf mcp`, and stop
reading. You do not need this server.

## What is still only here

**The method, and the skill that executes it.** Regions, the claim-or-action
rule, one claim in one home, the distillation standard, the review sweep. That
lives in [`METHOD.md`](METHOD.md) and in
[`templates/skills/wflow/SKILL.md`](../templates/skills/wflow/SKILL.md), and no
tool ships it because it is not a feature.

**Mirror discipline and drift auditing.** Covered in its own section below.

**The second-brain layer.** `review`, the session-log and draft scan, the
memory canonicals, everything that reads and writes `$SECONDBRAIN_DIR`. The
native surfaces have no filesystem and no concept of a private data directory
sitting alongside the tree.

**Headless and scheduled work.** `wflow-do` has full parity with the MCP tool
surface, enforced at build time, and talks to the REST API directly rather than
to a cache something else has to keep fresh. A cron job cannot depend on a
desktop app being open or on a sync having run.

**A tree you can withhold part of.** This server's on-disk index takes an
exclusion list. The CLI cache and the desktop MCP hold everything, with no way
to hold back a private subtree. That matters exactly when the index is
replicated somewhere, and matters much less when it never leaves the machine.

**Failure contracts.** Typed causes with retry-ability and `retry_after`,
truncation envelopes that admit what a walk missed, a name-echo guard on
deletes, an operation log. That is what the tests are for, and it is what makes
an unattended run diagnosable the next morning.

## Mirrors: the one place the method depends on a beta feature

The claim-based method needs the same claim to appear in more than one region
without becoming two claims that drift apart. This server implements that as a
convention: the mirror carries a `mirror_of:` marker in its description, the
canonical carries `canonical_of:`, and `audit_mirrors` walks the tree and
reports every mirror whose canonical has vanished, whose claim text has
diverged, or whose canonical never got its marker.

WorkFlowy's native mirrors do the first part properly and the convention does
not: a native mirror is one node rendered twice, so it cannot drift at all.
When they reach production, the honest thing is to move onto them, and this
repository should follow the method rather than defend its own plumbing.

Three things are worth knowing before that happens.

They are beta-only today. Production returns `beta_api_required` and mirrors
created against beta are not coherently visible on production, so a second
brain built on them is not portable back.

The audit does not become redundant when they ship. A native mirror cannot
drift in wording, but the questions `audit_mirrors` actually answers are wider
than that: is this claim mirrored into a region where it is a substantive
contribution or one it merely brushes, is the canonical marked, has the
canonical been deleted from underneath. Those are method questions and they
survive the mechanism changing.

The convention deliberately allows per-region divergence in one respect. A
mirror carries the tags appropriate to the region it sits in, which is why the
drift check strips trailing tags before comparing claim text. A native mirror,
being literally the same node, cannot do that. That is a real trade and the
migration will have to answer it rather than assume it away.

Until then, `create_mirror` and `audit_mirrors` remain the production-safe
path, and `wf --beta mirror:create` is available if you want to experiment.

## The routing rule that is not a preference

All five surfaces reach one WorkFlowy account and therefore share **one API
rate limit**, with one exception: the desktop MCP reads and writes the synced
client rather than the public API, so it does not draw on that budget at all.
Everything else does, and every additional live client divides a single
allowance.

On a machine that can reach the native tools:

Prefer the `wf` CLI or the desktop MCP for reads, searches, and ordinary
writes. Prefer the desktop MCP specifically when the app is running and the
work is interactive, because it costs no quota. Come to this server for the
method work, for anything touching `$SECONDBRAIN_DIR`, and for anything
scheduled.

Treat the desktop MCP's absence as routine rather than exceptional. It exists
only while the app is running, so a missing namespace or a connection refused
on its port is expected. Fall back to this server automatically, note the
fallback in one line, and carry on. Only attachments, client navigation and
screen capture have no substitute here, and only those steps should ever come
back as genuinely blocked.

**Never route laptop traffic through a remote connector.** It shares the same
account and the same rate limit, so traffic that could have gone locally burns
quota that the surfaces with no alternative depend on. A fall-through from
local to connector is an incident signal: restart the local host instead.

## The constraint: what runs where nothing native can

This is the part that is not negotiable, and it is the reason the connector's
tool surface must stay complete even as native tools absorb more of the daily
work.

A scheduled cloud routine runs in a sandbox with no WorkFlowy desktop app, no
`wf` binary, no local MCP server and no access to the machine any of those live
on. A phone has none of them either. For both, an MCP-over-HTTP connector is
the only road in, and it must carry every tool the work needs, including the
deterministic ones a model must not be trusted to improvise.

The concrete case in production is a daily journal write. Date-node resolution,
missing-level creation, and the indented insert all live in the connector as a
single `journal_insert_today` tool precisely so that no cloud model ever makes
a structural decision about a date node. That was not a design preference; it
followed a run of failures in which one did.

Two rules follow, and they bind regardless of how good the native tools get.

**Do not narrow the connector's tool surface on the grounds that a native tool
now does the same job.** The native tool is not reachable from where the
connector runs. Retiring a connector tool because a laptop has a better option
removes the only implementation available to the surface that has no laptop.

**Do not reroute a scheduled cloud routine to a native surface.** It will fail
silently, at 04:15, with nobody watching, and the failure will look exactly
like a quiet day.

## Where the engineering detail lives

This page is about choosing a surface. What this server's own tools do, the
environment variables, the `wflow-do` CLI and the reliability contracts are in
[`SERVER.md`](SERVER.md), kept separate so that
[`../README.md`](../README.md) and [`METHOD.md`](METHOD.md) can stay about the
method rather than the machinery.

## Measuring it rather than guessing

Because the surfaces share one budget and it is easy to lose track of which one
answered a call, this server ships an optional per-call usage log. Set
`WORKFLOWY_USAGE_LOG_DIR` and every MCP call and every `wflow-do` invocation
appends one JSON line recording the timestamp, the surface, the tool, whether
it succeeded, how long it took, and the proximate cause on failure. The
question of which surface actually carries your work is then answered by
counting rather than by preference, which is the only way this page stays
honest as the native tools keep improving.
