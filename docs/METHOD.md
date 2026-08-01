# The second-brain method

WorkFlowy is adding AI features, and so is every other tool that holds your notes. Asking your outline a question in plain language is becoming a commodity; the part of this repository that wires a model to an API will matter less every month. What will still matter is the method: the regions you divide your thinking into, and the discipline that everything you file in them is either a claim or an action. It is worth reading even if you never install the server.

## Accumulating is not compounding

Note systems fail the same way. They accumulate. Capture is easy, every tool optimises for it, and so the tree grows: clippings, half-read articles, meeting notes, an inbox nobody triages. Retrieval then gets hard, and the standard answer is better search.

Better search finds the note. It does not make the note worth finding.

The two problems have different solutions, which is why conflating them is expensive. Retrieval is a tooling problem, and tooling is improving quickly enough that betting against it would be foolish. What you wrote down is a discipline problem, and no model can retrospectively turn a page of highlights into a claim you can argue with. A system compounds when each addition makes the existing material more useful. Most systems only get bigger.

Two decisions determine which of those you end up with: how you divide the space, and what you allow into it.

## Start with the regions

Before capturing anything, decide the regions: the areas your thinking divides into. This is the first act of the method and the one most people skip, because capture feels productive and classification feels like admin.

A region is not a folder for a project, a client, or a source. Projects end, clients churn, and sources describe where material came from rather than what it is about. A region is a durable division of your attention, the kind that survives a change of job.

Regions come in three kinds, and they behave differently enough that treating them alike is the commonest way a clean structure goes soft.

**Conceptual regions** are the divisions of your practice, and they are best named as activities rather than subjects. "How we build", "how we decide", "how we lead", "how we learn". The phrasing does real work: a region called Architecture invites you to file anything architectural, while a region called *how we build* only accepts claims about building, which is a narrower and more useful question. Name a region as a noun and it becomes a magnet for material; name it as an activity and it stays a place for conclusions. Six to nine of these is comfortable, and they should be close to permanent.

**Theme regions** are what you are currently following that cuts across the conceptual ones. A technology, a debate, a shift in the market. Themes are more volatile by nature: they appear, absorb attention for a year or two, and then either dissolve into the conceptual regions as ordinary practice or stop mattering. Keeping them separate from conceptual regions is what stops a hot topic colonising your whole structure and then leaving a hole when it cools.

**Life regions** are the parts of your life that are not the practice at all. Health, family, money, the house, whatever you are learning for its own sake. They belong in the same system for an unglamorous reason: if they are not here they are in a second system, and a second system is one you will not keep. They generate fewer claims and more actions than the other two, and that is fine.

Keep the whole set small enough to hold in your head. If you cannot name your regions without looking them up, you have topics rather than regions, and you will file inconsistently because you are choosing from a list you cannot see. Beyond a dozen in total, the structure is recording your interests rather than organising them.

Regions change slowly, and that is the property that makes them useful: a claim filed today should still be in a sensible place in three years. When one does need to split, splitting it is a deliberate act with a migration behind it, never a decision made while filing a note at speed.

Write the set down as a taxonomy and route from the written list rather than from the mood of the moment. That is the taxonomy's whole function: it makes "where does this go" a question with an answer, decided once and applied consistently. In the skill file that ships with this repository, conceptual regions are called pillars and themes have their own subtree; the words matter less than the fact that the list lives outside your head.

## Everything in a region is a claim or an action

Once the regions exist, one rule governs what may enter them. A node is either a claim or an action.

A claim states something you could agree or disagree with. "Team structure constrains architecture more reliably than architecture constrains team structure" is a claim. "Conway's Law and team topologies" is not; it is a container, and you must open it to learn anything. The difference is not stylistic. A claim can be read at a glance, contradicted by another claim, cited in an argument, or found by a search that had no idea the source existed. A container is a filing decision. A claim is a thought you can build on.

An action states something someone will do. It carries an owner, usually a date, and enough specificity that a future reader can tell whether it has happened. "Chase the vendor" is not an action; "Decide whether to renew the vendor contract before the November board" is.

Anything that is neither is raw material: a quote, a transcript, a clipping, a page of highlights. Raw material is not a problem, and capturing it fast matters. It simply does not belong in a region. It sits in an inbox until someone turns it into a claim, turns it into an action, or drops it, and dropping it is a legitimate outcome that most systems never quite permit.

This gives a test sharp enough to use while reading: if you cannot turn a source into a claim or an action, you have not finished with it. The instinct to save the whole thing for later is the instinct that builds a warehouse.

Three conventions keep the rule honest, each earned by the mess its absence caused.

Structure on claims and topics, never on people. A thinker's name is a trailing attribution, never the head of a note and never the organising axis. Filing by author produces a bibliography; you can find what Argyris said, but not what you concluded, and the moment two thinkers bear on the same problem the structure fights you.

Descriptions carry plumbing, not content. In the pattern this repository uses, a node's name holds the claim and its description holds only a source link and the markers that connect it to the rest of the graph. Anything that argues belongs in the claim or in a child note. This keeps the tree structurally uniform, which is what makes bulk operations, auditing and visualisation possible at all.

Say the thing in real words. Naming a node "MOC" or tagging it with the vocabulary of your filing system tells a future reader nothing. If a label would not survive being read aloud to someone else, it is index jargon and it should go.

## One claim, one home

A claim usually bears on more than one region, and a claim that bears on a conceptual region and a theme at once is the normal case rather than the exception. The obvious move is to write it in both. It is the wrong one, because two copies drift and neither is authoritative once they do.

The method holds a claim in exactly one canonical region and represents it elsewhere as a mirror: a node carrying the same name and a marker naming its canonical. Edits go to the canonical first, then to the mirrors verbatim. An audit can then walk the tree and report every mirror whose name has diverged from its canonical, and every mirror whose canonical no longer exists.

WorkFlowy has native mirrors, and where they are available they are the better mechanism. The convention here exists because the public API does not expose them, and it carries a benefit the native feature does not: a marker in a description is machine-readable by anything that can read the tree, so drift becomes checkable rather than merely visible. Either way the rule is the same, and it is a rule about thinking rather than about software. One claim has one home; every other appearance points at it.

Two habits protect this in practice. Before writing anything, draft the routing first: candidate claims, destination regions, mirrors, sources integrated. Confirm that, then write. And check for novelty before adding: search the destination for the concept, and if something already covers the ground, mirror or link to it rather than adding a near-duplicate. Both passes are cheap. Skipping them is how a second brain fills up with three slightly different statements of the same idea, none of them canonical.

## Two loops, running at different speeds

The daily loop is operational: capture a task, triage the inbox, review what is due, work the reading queue, keep a journal. It is mundane and needs no cleverness, but it keeps material arriving and stops the inbox becoming a graveyard.

The slower loop is where the value is. Take a source, or a conversation, or a piece of work you have just finished, and turn it into claims and actions. Route them to regions. Mirror what belongs in several. Write a log of what you did.

The two are connected by design. The daily loop feeds the slow one; the slow one is what makes the daily one worth running. A second brain that only captures is an inbox with ambitions. One that only synthesises runs out of material.

Session logs live on the filesystem rather than in the outline, and this is deliberate. The log records what was checked, what was written and what was decided; it is an audit trail rather than knowledge, and mixing the two pollutes the regions with process. It also gives the next session somewhere to pick up from.

## What using it looks like

Five things you do. The examples are illustrative, but their shape is taken from a second brain that has run this way daily for months: about 1,500 claims across nine regions, three levels deep, roughly a tenth of them mirrored.

**Distilling a source.** You have read something worth keeping, and you say so. What comes back first is not a note; it is a routing table. The claims the source supports, the region each would land in, which should mirror elsewhere, and which existing notes already cover the ground. You correct that, because routing is where your judgement is needed and drafting is not. On your confirmation it writes a cluster: a head carrying the source's lead claim, the atoms beneath it, the mirrors, and a log to disk.

The result looks like this, and the head is the source's argument in one sentence rather than its title:

```text
Delegating a judgement to a system never delegates responsibility for it
(Author, Work) #decide #ethics
  ⌙ Source: https://example.org/the-paper
    canonical_of: decide
  ├── An accountable decision needs a person who can be asked why, so any
  │   automation that removes that person removes the accountability #decide
  ├── Speed of decision and reviewability trade against each other, and most
  │   deployments choose speed without recording the choice #decide
  └── The audit question is not "was the model correct" but "could a human
      have overturned it in time" #decide #ethics
```

Nothing in that tree is a heading. Read any line alone and it asserts something.

**Asking across regions.** *"What do I have on how teams absorb new tools?"* Retrieval crosses regions by design, because a question rarely respects your filing. The answer comes back as your own claims with links to them, which is the point: you are reading your conclusions, not re-reading your sources. Where two claims conflict, you see both, and that is usually the most valuable output of the whole exercise.

**Capturing an action mid-conversation.** Something surfaces that someone has to do. The system offers before it writes: *"I can capture this as a todo under Client work, owner you, due before the November board. Confirm?"* Offer-then-confirm exists because an inferred region is right most of the time and silently wrong the rest, and silent misfiling compounds. When capture is your explicit intent the offer collapses to confirming the region.

**The weekly pass.** One session a week does the work that makes the daily loop worth running: read what came in, turn it into claims and actions, route them, mirror what belongs in several regions, tidy anything orphaned, and write a log. An hour on a Saturday is enough. This cadence matters more than any individual session, because a second brain is a compounding asset and compounding needs regularity more than it needs intensity.

**Auditing drift.** Ask for a mirror audit and you get every mirror whose name no longer matches its canonical, and every mirror whose canonical has been deleted. Both accumulate quietly, and neither is visible by reading the tree. Run it monthly.

## Rules earned in operation

These rules are not deductions from the method. They are what the change record of a working second brain shows, and most were adopted only after their absence caused a mess.

**Sharpening a rule means paying to retrofit it.** Six convention sweeps ran inside a single month: a tag backfill across 326 nodes, a deprecated-tag strip across 78, a jargon removal across 87, a claim-led reframing across 225. Each was a rule arriving after the material it governs. The alternative to retrofitting is two layers in one tree, each obeying a different vintage of the standard, and no way to tell which you are reading. Do the sweep as one deterministic pass: build the plan as data, apply it verbatim, verify a sample by reading back. Never retype a claim by hand.

**Delete navigation that a query can compute.** Four hand-curated "cross-pillar reference" nodes were retired once it was clear that a backlink search answered the same question dynamically and could not go stale. The session-log subtree, 185 nodes, was deleted from the outline entirely and moved to the filesystem. Anything you maintain that a tool can derive is debt with an upkeep cost.

**Never leave placeholder scaffolding.** Five stubs reading "(Mirror syntheses about leadership go here)" sat untouched from the day the structure was created until the day they were swept as clutter. Empty structure does not invite filling; it makes an empty tree look populated, which is worse than an obviously empty one.

**A claim that promises a list must deliver it.** An audit of 1,316 nodes looked for one pattern: notes titled "three forces converge" or "the four conditions" that never enumerate them. The promise-without-payload note is the most seductive thing you can write, because it feels like a conclusion while deferring the work.

**Themes get promoted or they die.** One theme accumulated for months, earned a full conceptual region, and had its material moved across in a single deliberate migration. That is the lifecycle working. The failure mode is a theme that keeps growing without ever being promoted, until it holds more than the regions it was supposed to cut across.

**Mark staleness rather than living with it.** When a cluster was found to contradict current thinking on four points, it was tagged for revisit with a dated note saying exactly where it had gone stale. A second brain is a record of what you concluded, and some of it will be wrong later. Flagging is cheap; silently trusting a stale cluster is not.

**Mirror sparingly.** Roughly one node in ten carries a mirror. Mirroring everything that touches a region reproduces the duplication the canonical rule exists to prevent, only with markers on it. Mirror when a claim is a substantive contribution to the second region, not when it merely brushes it.

**Verify that the write landed.** Every write session in the record ends with a live re-read confirming the nodes persisted. An API acknowledgement is not evidence, and a batch that half-applied looks identical to one that succeeded until you look.

**Keep it shallow.** Region, source cluster, atoms. Three levels carries almost everything; the fourth is occasionally justified and the fifth almost never is. Depth is where material goes to be forgotten.

**Measure whether you are meeting your own standard.** Because claims are sentences and labels are not, you can count. In the tree these rules came from, about 95% of nodes at claim depth are sentence-shaped, and the residue that is not is exactly where the last convention sweep did not reach. That is a number you can check on your own tree in a single query, and a falling one tells you the discipline is slipping before the structure visibly rots.

## What the software is for

Read in order, the method needs very little from a tool. It needs to read and write an outline reliably, to resolve a reference to a node without ambiguity, to make a batch of related writes without leaving half of them applied, and to tell you plainly when it could not do what you asked.

That is roughly what this repository provides, along with the mirror-drift audit, an index that answers questions about a large tree without walking it, and a skill file that carries the method itself as instructions a model follows. The [wflow skill](../templates/skills/wflow/SKILL.md) is the method in executable form; the server is what it stands on.

If WorkFlowy's own AI eventually does all of that natively, the right response is to move the method onto it and retire the plumbing here. The method was never the plumbing.

## Adopting it

Write your regions down first, before installing anything. Name the conceptual ones as activities, list the themes you are actually following, and include the life regions rather than leaving them to a second system you will abandon. That list is the method's foundation, and getting it roughly right matters more than any tool decision that follows.

Then read [`templates/skills/wflow/SKILL.md`](../templates/skills/wflow/SKILL.md). It describes the workflows, the standard a distilled note has to meet, and the disciplines each workflow observes, in a form generic enough to apply to any outline. Much of it will suggest changes to how you already work.

If you want it automated, [`BOOTSTRAP.md`](../BOOTSTRAP.md) walks a model through the install: build the server, wire it into your host, create the private directory that holds your regions and taxonomy, and cache the node identifiers your workflows will need.

The regions in the template are placeholders, and they should be. What transfers is not the categories but the discipline: divide the space before you fill it, and let nothing in that is not a claim you could argue with or an action someone will take.
