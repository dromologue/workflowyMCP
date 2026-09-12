# The claim-based second brain

**A method for building a note system out of claims rather than clippings, and
the tooling that runs it on WorkFlowy.**

Most note systems accumulate. This one is built to compound: you divide your
thinking into a small set of durable **regions**, and you allow nothing into
them that is not a **claim** you could argue with or an **action** someone will
take. A claim is a sentence you could contradict. "Team structure constrains
architecture more reliably than the reverse" is a claim; "Conway's Law and team
topologies" is a container you have to open before it tells you anything.
Everything else here exists to make that one distinction cheap enough to hold
to.

**[Read the method first](docs/METHOD.md).** It is the part worth your time,
and it applies to any outliner, with or without this software. WorkFlowy now
ships its own AI, and so does everyone else; asking your notes a question in
plain language has become a commodity, and the plumbing in this repository
matters less every month. What you wrote down, and where it lives, is the part
no model can fix for you afterwards. That is the bet this repository is
increasingly made of: the retrieval half is being won by the native tools, and
should be, while the claim discipline is not a feature anyone will ship you.

If you want it running rather than only understood, there is software here to
run it: a Rust MCP server connecting Claude (Desktop, Code, or claude.ai) to
your workspace, a `wflow-do` CLI with the same surface for scripts and cron,
and a skill file that carries the method as instructions a model follows. It is
one section of this page and one document behind it, which is the proportion it
deserves.

In practice that means you say *"capture this as a task under Projects"*
mid-conversation and it lands in the right region, tagged and dated. You ask
*"what do I have on organisational design?"* and get an answer assembled from
your own claims. You paste a WorkFlowy link and Claude knows which node you
mean. You run a morning review as a single question.

Two ways in:

1. **The method only.** Read [`docs/METHOD.md`](docs/METHOD.md) and
   [`templates/skills/wflow/SKILL.md`](templates/skills/wflow/SKILL.md), write
   your regions down, and apply it by hand. Install nothing.
2. **The method, automated.** Hand [`BOOTSTRAP.md`](BOOTSTRAP.md) to Claude and
   let it run the install: build, wire the host, seed your private data
   directory, cache your structural node IDs, install the wflow skill that
   drives every later session. About ten minutes.

Before you install anything, it is worth reading
[`docs/SURFACES.md`](docs/SURFACES.md), which says candidly what WorkFlowy's own
tools now do better than this one and what is left that only this repository
provides. Several people should read it and then install the `wf` CLI instead.

Your data stays yours. The repo ships only generic templates; everything
personal (your regions, node IDs, drafts, session logs, the search index) lives
at a path you choose, outside the repo, on your machine.

---

## What the method asks of you

Three commitments, described properly in [`docs/METHOD.md`](docs/METHOD.md) and
summarised here so you can judge whether it suits you.

**Divide the space before you fill it.** Regions come in three kinds:
*conceptual* regions naming the activities of your practice ("how we build",
"how we decide"), *theme* regions for what cuts across them, and *life* regions
for everything that is not the practice at all. Name them as activities rather
than subjects, keep them few enough to recite, and write the list down so
routing is a decision made once rather than every time.

**Admit only claims and actions.** A claim states something you could disagree
with. An action names an owner and a date. Everything else is raw material: it
belongs in an inbox until someone turns it into one of the two, or drops it. If
you cannot turn a source into a claim or an action, you have not finished
reading it.

**One claim, one home.** A claim that bears on several regions lives canonically
in one and appears in the others as a mirror pointing back. Copies drift;
`audit_mirrors` exists to tell you when they have.

---

## What building one actually looks like

The rules above are short enough to memorise and that makes them sound easy.
They are not, and the honest account of the work is the most useful thing this
page can give you.

**The first act is writing your regions down, and it happens before you capture
anything.** Most people skip it, because capture feels productive and
classification feels like admin. The cost of skipping it is not disorder; it is
that you file by mood, so the same idea lands in three places over a year and
none of them is where you look for it. Six to nine conceptual regions, a
handful of themes, the life regions you would otherwise keep in a second system
you abandon. If you cannot recite the list, it is a list of topics rather than
regions.

**Then the discipline is refusal.** Almost everything that arrives is neither a
claim nor an action, and the work is turning it into one or dropping it. A
source you cannot reduce to a claim is a source you have not finished reading,
which is uncomfortable and usually true. This is the part no tool does for you,
and it is the reason a second brain compounds rather than merely accumulating:
what makes the existing material more useful is a new claim that argues with it,
not a new clipping that sits beside it.

**The structure stays shallow and the mirrors stay rare.** Region, source
cluster, atoms. Three levels carry almost everything; depth is where material
goes to be forgotten. Roughly one node in ten earns a mirror, and mirroring
everything that merely touches a second region reproduces the duplication the
canonical rule exists to prevent, with markers on it.

**And it is maintained, not merely written.** Conventions sharpen, which means
retrofitting the material they now govern: in the tree these rules came from,
six sweeps ran inside a single month, one of them reframing 225 nodes into
claim-led form. Some of what you concluded will later be wrong, and marking a
cluster stale with a dated note is cheap where silently trusting it is not.
Because claims are sentences and labels are not, you can measure whether you are
meeting your own standard: count the share of nodes at claim depth that are
sentence-shaped, watch it, and a falling number tells you the discipline is
slipping before the structure visibly rots.

[`docs/METHOD.md`](docs/METHOD.md) is the full account, with worked examples and
the rules a real change record supports. It is the document this repository
exists to serve.

---

## Getting the method running

Have your regions written down before you start; the install asks for them,
and [`docs/METHOD.md`](docs/METHOD.md) explains how to arrive at a set worth
keeping. Then hand [`BOOTSTRAP.md`](BOOTSTRAP.md) to Claude. It walks the
seven steps: build, wire the host, seed your private `$SECONDBRAIN_DIR`, cache
your structural node IDs (Inbox, Tasks, Journal…), install the wflow skill,
pre-warm the search index for large trees, and verify the whole chain
end-to-end. After that, every session opens with your workflows available
conversationally: daily and weekly reviews, task capture, inbox triage,
reading-list management, distillation of sources into atomic notes, mirror
discipline with drift auditing, and cross-note research.

The long-form walkthrough (multi-surface setups, large-tree convergence,
troubleshooting) is in [`docs/SETUP.md`](docs/SETUP.md). Running behind a
remote connector for claude.ai web/mobile is covered in
[`docs/REMOTE-CONNECTOR.md`](docs/REMOTE-CONNECTOR.md).

---

### If you only want the bare server

Skip the method entirely and wire the binary in by hand:

You need Rust 1.75+ (`rustup install stable`), a Workflowy API key
(Workflowy → Settings → API), and an MCP host (Claude Code or Claude
Desktop).

```bash
git clone https://github.com/dromologue/workflowyMCP.git ~/code/workflowy-mcp-server
cd ~/code/workflowy-mcp-server
cargo build --release
echo "WORKFLOWY_API_KEY=<your-token>" > .env
```

Wire `target/release/workflowy-mcp-server` into your host:

- **Claude Code:** `claude mcp add workflowy -- $(pwd)/target/release/workflowy-mcp-server`
- **Claude Desktop:** edit
  `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS)
  or `%APPDATA%\Claude\claude_desktop_config.json` (Windows). See
  [BOOTSTRAP.md](BOOTSTRAP.md) for the JSON shape.

Verify by asking Claude to call `workflowy_status`; you want
`status: "ok"`, `api_reachable: true`, `authenticated: true`. Then try it:

> "List the children of my workspace root."
> "Create a node called *Read later* under my Inbox."
> "What did I change in the last two days?"

That's the bare server working. The `.env` file covers a binary launched from
the repo directory; putting the same key in the host's `env` block (next
section) works from anywhere and is the recommended form.

---

## The software, in one section

Everything below this line is a tool for building the thing above it, and it is
deliberately the smaller half of this page.

A second brain built this way needs very little from software. It needs to read
and write an outline reliably, resolve a reference to a node without ambiguity,
make a batch of related writes without leaving half of them applied, and say
plainly when it could not do what was asked. WorkFlowy's own tooling now does
most of that better than this server does, and you should use it: see
[`docs/SURFACES.md`](docs/SURFACES.md) for which surface to reach for and why.

What is left here is what carries the method rather than the outline. Mirror
creation and the drift audit that tells you when copies have diverged. The
second-brain review. The private data directory holding your regions, your
routing rules, your drafts and session logs. Scheduled and headless operation.
And a remote connector for a phone or an unattended cloud run, where none of
WorkFlowy's own tools can be installed.

The engineering detail, the tool reference, the environment variables, the
`wflow-do` CLI and the reliability contracts all live in
[`docs/SERVER.md`](docs/SERVER.md). If WorkFlowy's own AI eventually does all of
it natively, the right response is to move the method onto that and retire the
plumbing here. The method was never the plumbing.

---

## Where this sits among WorkFlowy's own tools

WorkFlowy has been building fast, and several things this server was once the
only way to do are now native. As of September 2026 there are five ways to put a
model in front of a WorkFlowy tree: WorkFlowy Pro's own AI inside the client,
the [`wf` CLI](https://workflowy.com/help/workflowy-cli), the
[MCP embedded in the desktop app](https://workflowy.com/help/claude-desktop/),
this server, and a remote connector for anywhere the first four cannot be
installed.

**[`docs/SURFACES.md`](docs/SURFACES.md) is the full accounting**, kept current
because it decides real routing. The short version:

**Use the native tools for reading, searching, and ordinary writing.** The `wf`
CLI answers from a local full-text cache in under 100 ms with each hit's
ancestor path attached, costs no API quota, and now also does nested writes,
todos, tags, change streams and webhooks. The desktop MCP traverses the
already-synced client with filters on regular expressions and date windows,
applies batched nested writes under advisory leases, and draws on no REST quota
at all. Both are better than this server at those jobs. If that is your whole
use case, install the CLI, wire in `wf mcp`, and stop there.

**Use this server for the method, and for where the others cannot go.** The
claim-based discipline and the skill that executes it, mirror creation and
drift auditing, the second-brain review, the `$SECONDBRAIN_DIR` layer, a
scheduled reindex, an index you can withhold private subtrees from, and typed
failures that make an unattended run diagnosable the next morning. None of that
is a tool WorkFlowy is likely to ship, because none of it is a feature.

**And use it for anything that runs where WorkFlowy's tools cannot be
installed.** A cloud sandbox and a phone have no desktop app and no shell. The
same binary behind an HTTP shim serves both as a custom connector, which is why
that surface has to keep a complete tool set even as the native tools absorb
more of the daily work. See
[`docs/REMOTE-CONNECTOR.md`](docs/REMOTE-CONNECTOR.md).

Two things have genuinely arrived since, both verified against the production
API on 12 September 2026, and both worth knowing if you are weighing the method.

**Calendar targets.** A create can now name `today`, `tomorrow`, `next_week`, or
a literal `2027-03-09` as its `parent_id`, and WorkFlowy materialises the whole
missing year/month/day chain in its own calendar and puts your node under it.
That replaces every line of date-node arithmetic a journal writer used to carry,
and it removes the duplicate-date-node problem at the root, because the node you
get back is WorkFlowy's own rather than a lookalike sitting beside it. This
server cannot reach it yet: its id validator accepts a UUID or a short hash and
rejects everything else, target keys included.

**Native mirrors, half-arrived.** The write endpoint is live on production:
`POST /nodes/{id}/mirror` creates a real mirror, one node genuinely rendered in
two places, which is what this repository's `mirror_of:` convention only
approximates. The linkage that tells you it *is* a mirror is still beta-only,
and on a production read a native mirror comes back with an empty name and no
mirror metadata at all. So the convention stays the production-safe path, not
because mirrors are unavailable but because a second brain built on them would
be invisible to every tool that reads the production API — this server, the
connector, and the unattended cloud run included. `docs/SURFACES.md` carries the
evidence and the migration conditions.

All of these share one WorkFlowy account and therefore **one API rate limit**
(the desktop MCP excepted, since it reads the synced client rather than the
API). Running several live clients divides a single budget, which is why this
repo ships an optional per-call usage log (`WORKFLOWY_USAGE_LOG_DIR`) so the
question of which surface actually carries your work is answered by counting
rather than by preference.

### Installing the CLI, and using it as an MCP surface

```bash
curl -fsSL https://github.com/rodolfo-terriquez/workflowy-cli/releases/latest/download/install.sh | bash
wf login            # or: WORKFLOWY_API_KEY=... wf login
wf cache:sync       # whole tree into ~/.workflowy/db
wf doctor
```

The install script verifies a SHA-256 checksum from the release before it moves
anything into place, which is the reason it is safe to pipe. On a
235,000-node workspace the first sync took **16.5 seconds** and full-text search
answers in **under 100 ms** with the ancestor path attached, materially better
than this server's own name index, which stores names and descriptions but not
paths and has no ranking. Subsequent syncs re-export the whole tree, so they hit
WorkFlowy's ~65 s floor on `/nodes-export`; sync on a schedule, not in a loop.

Wire the same binary in as an MCP server:

```bash
claude mcp add workflowy-cli -s user -- "$HOME/.local/bin/wf" mcp
```

or, for Claude Desktop, in `claude_desktop_config.json`:

```json
"workflowy-cli": { "command": "/Users/you/.local/bin/wf", "args": ["mcp"] }
```

`wf mcp --tools read,search,add` narrows the exposed set if a full 30-tool
surface is more than a given host needs.

**On the Claude Desktop `.mcpb` extension:** it packages
[`workflowy-local-mcp`](https://github.com/rodolfo-terriquez/workflowy-local-mcp),
not the CLI, and at the time of writing it lags: v1.2.4 against the CLI's
v3.3.1, 8 tools against 30, a second SQLite cache under
`com.workflowy.local-mcp` rather than the CLI's `~/.workflowy`, and a second
place your API key is stored. If you already have the CLI, `wf mcp` gives you a
strictly larger tool set off one cache and one credential. Install the
extension only if you want the double-click install and no terminal; do not run
both.

One privacy consequence is worth stating plainly. The CLI's cache and the
desktop MCP both hold your *entire* tree locally, with no way to withhold a
subtree. This server's on-disk index is the only one that can be told to
exclude subtrees (`WORKFLOWY_INDEX_EXCLUDE_SUBTREES`), which matters when that
index is replicated somewhere, to a hosted connector say. A local-only cache
and a replicated index deserve different postures; keep the exclusion on
whatever leaves the machine.

## What ships in this repo

```text
workflowyMCP/
│
│  THE METHOD  (read in this order)
├── docs/METHOD.md            ← the method: regions, claims, actions, mirrors
├── templates/skills/wflow/   ← the method as instructions a model follows
├── templates/secondbrain/    ← skeleton copied to $SECONDBRAIN_DIR
├── README.md                 ← this file
│
│  RUNNING IT
├── BOOTSTRAP.md              ← LLM-facing install script (hand to Claude)
├── docs/SETUP.md             ← long-form setup walkthrough
├── docs/SURFACES.md          ← which surface to use, and why
├── docs/proposals/           ← dated proposals, not yet decided
│
│  THE SOFTWARE
├── docs/SERVER.md            ← tool reference, env vars, CLI, reliability
├── docs/REMOTE-CONNECTOR.md  ← claude.ai custom-connector notes
├── specs/                    ← behavioural spec, principles, traceability
├── dist/wflow.skill.zip      ← ready-to-upload skill bundle for claude.ai
└── src/                      ← Rust MCP server + wflow-do CLI
```

Everything specific to you (cached node IDs, your regions and routing
rules, drafts, session logs, the name index) lives at `$SECONDBRAIN_DIR`
and `$WORKFLOWY_INDEX_PATH`, never in the repo. Clone it and you get a clean
starting point; so does the next person.

| File you'll create | Lives at | What it holds |
|------|----------|---------------|
| `workflowy_node_links.md` | `$SECONDBRAIN_DIR/memory/` | Cached UUIDs for your structural nodes (Inbox, Tasks, Reading List…) plus the triage-sources table. |
| `distillation_taxonomy.md` | `$SECONDBRAIN_DIR/memory/` | Your pillars, themes, and routing rules for the synthesis workflows. |
| `name_index.json` | `$WORKFLOWY_INDEX_PATH` | Auto-managed persistent search index. Survives restarts; checkpoints every 30 s; grows with every walk and converges via the scheduled `reindex --patient` job. |
| `drafts/`, `session-logs/`, `briefs/` | `$SECONDBRAIN_DIR/` | In-flight work, per-session audit trails, handoff documents. |

---

## Licence

MIT
