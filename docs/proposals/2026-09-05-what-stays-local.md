# What stays local, and what native tooling has taken

**Status: proposal, not yet decided.** Survey run 2026-09-05, re-running the
2026-08-17 pass in [`../SURFACES.md`](../SURFACES.md). If accepted, the
conclusions fold into that document and this file becomes a record of the
reasoning. The test applied throughout is the one that was asked for: keep a
capability here only where a native tool cannot do something we actually need.

## What changed natively in the last three weeks: nothing material

The 2026-08-17 survey found the native tools had taken the index-and-lookup
half outright. Re-running it today finds the position unmoved.

The `wf` CLI is still 3.3.1, with the same command set. The desktop MCP still
exposes the same fourteen tools, although the app itself was rebuilt on
2 September (4.3.2609020650), so the surface has been held stable across at
least one release rather than merely sitting untouched. The production REST API
still documents no mirror endpoint. `wf mirror:info` against production still
returns `beta_api_required` and still succeeds under `--beta`, and a production
`GET /nodes` still carries no `data.mirror` field.

One correction to the earlier survey, because it was wrong in a way that
matters. I previously implied that description-aware search was ours alone.
It is not. `wf` indexes note text: searching for a UUID fragment that exists
only inside a note returns the node, from cache and live alike. The earlier
`mirror_of` query failed on underscore tokenisation in the FTS index, not on
coverage, and reading a tokeniser artefact as a capability gap is exactly the
error this document exists to avoid.

## The two facts that decide the answer

Everything below follows from these, so they are worth stating before the
recommendations rather than inside them.

**The desktop MCP cannot read or write descriptions.** `tree_patch` accepts
`name`, `block_format` and `children` on an insert and `name` on a replace.
There is no note field anywhere in its schema. `tree_seek` will *match* on note
text, which is genuinely useful and was confirmed today against live nodes, but
it returns only the name, so the note that caused the match is invisible to the
caller. The claim-based method keeps its entire graph layer in descriptions:
`mirror_of:`, `canonical_of:`, and the single `Source:` line. A surface that
can neither read nor write a description cannot carry the method, however good
its traversal is. This is a structural limit rather than a missing convenience,
and it is the single strongest reason something has to remain.

**The connector is this repo's tool surface, so removing a tool here removes it
from the phone and the cloud.** The connector twin serves claude.ai on mobile
and the 04:15 UTC routine in an Anthropic sandbox. Neither has a WorkFlowy
desktop app, a shell, or this machine's filesystem, so "a native tool does this
better" is never a reason to drop a tool: better, but unreachable from where
the connector runs. The instruction to keep only what native tooling cannot do
therefore has to be read per surface. Read as a single global cull it would
break the two surfaces with no alternative.

## What six weeks of measurement actually shows

The repo ships a per-call usage log for this question, and it has been running
since 23 July: 6,259 parsed calls across 38 days, split 3,201 through the MCP
server and 3,058 through `wflow-do`. Failures were 580, dominated by 348
unclassified and 101 not-found, with rate limiting a negligible 5.

Two findings from it are worth more than the totals.

**No call was ever logged against the desktop MCP.** The wflow skill is
supposed to append a caller-side line whenever it routes work there, and there
are none in six weeks. Either the routing advice written on 17 August is not
being followed, or the instrumentation for it does not work. Both are worth
knowing and neither is currently visible anywhere else, so this should be
checked before any further decision rests on assumed desktop-MCP usage.

**The log cannot authorise a deletion on its own, and `since` proves it.**
`since` has zero calls in six weeks of laptop logs, and eight references in the
cloud routine's prompt. The log instruments only the local server and the local
CLI; every connector call lands on the Fly volume instead. Zero local calls
means "not used on this laptop", never "unused".

## Recommendations

### Retire four tools

These four have zero calls in six weeks **and** zero references in any caller:
the cloud routine's prompt, the live wflow skill, or the public skill template.
They are the only ones where both tests agree.

`convert_markdown` is the clearest. The API has parsed markdown in `name` on
every write since the 2026.01 patch, so the tool converts what the wire already
converts. `duplicate_node` and `create_from_template` are both covered by
`wf node:template`, which is where that work would go anyway. `get_recent_tool_calls`
is a diagnostic that already ships as a no-op on the CLI and reports on an op
log that only exists inside a running server.

Retiring these costs four tools' worth of surface, four CLI subcommands, and
the parity test entries that pin them. It is a real simplification and a small
one, and pretending otherwise would be the dishonest part of this proposal.

### Demote four more to review rather than deletion

`daily_review`, `get_project_summary`, `smart_insert` and `list_upcoming` have
zero calls in six weeks but are documented in the skill, which means the skill
is advertising capability nobody exercises. That is worth resolving in one
direction or the other, but the resolution is a decision about the method's
workflows rather than about native tooling, and it should not be smuggled into
a tooling cull. `wf todos --since` and `wf todos --target` cover a good part of
what the first and last of these do.

### Keep the rest, and stop running the local server as a daily driver

The tools that carry the method have no native equivalent on any surface:
`create_mirror` and `audit_mirrors`, `review`, `find_backlinks`,
`find_by_tag_and_path`, `reorder_nodes`, `since`, the tag operations in
`bulk_tag` and `bulk_update`, and `transaction` with its rollback. `wf` has no
backlink query, no path-scoped tag intersection, no reorder, no rollback across
a batch, no creation-keyed window, and no notion of a canonical or a mirror
beyond the beta primitive. The measured usage agrees: `audit_mirrors` and
`review` were called nineteen times each, `find_backlinks` and
`find_by_tag_and_path` ten each, `reorder_nodes` 184.

The change worth making is not to the tool list but to the routing. Reads,
searches and ordinary writes should go to `wf`, which is faster, costs no
quota, and returns ancestor paths. The local stdio server should stop being the
default surface for a Claude Code or Desktop session and become what it is
already best at: the method tools, the `$SECONDBRAIN_DIR` layer, scheduled work
through `wflow-do`, and the binary behind the connector. That is a
configuration change in the MCP hosts rather than a code change here, and it
does not touch the connector at all.

### Do not act on this without checking the desktop-MCP instrumentation

The zero desktop-MCP lines mean the current routing picture is partly
unmeasured. Fixing that costs little and would make the next pass of this
document evidence-led on all four surfaces rather than three.

## What would change the answer

Native mirrors reaching production is the one development that would move a
load-bearing piece. Even then the audit survives the mechanism, because it asks
whether the canonical is marked, whether it still exists, and whether the claim
is a substantive contribution to the region it is mirrored into. One genuine
trade would need answering rather than assuming: the convention deliberately
permits per-region tag divergence, and a native mirror, being one node rendered
twice, cannot carry different tags in different pillars.

The other development would be a note field on the desktop MCP's `tree_patch`
and note content in `tree_seek` output. That would remove the structural reason
a description-aware surface has to exist locally, and would make a much larger
cull arguable than the one proposed here.
