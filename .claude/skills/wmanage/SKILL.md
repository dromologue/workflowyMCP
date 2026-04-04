---
name: wmanage
description: Day-to-day Workflowy management. Daily/weekly/monthly prioritisation, task capture, inbox triage, project status, reading list management, and journal check-ins. Use when the user wants to plan, review, organise their work, or journal.
argument-hint: [daily|weekly|monthly|capture|status|triage|reading|journal]
allowed-tools: Read, Write, Bash, Glob, Grep, AskUserQuestion, WebFetch, mcp__workflowy__get_node, mcp__workflowy__find_node, mcp__workflowy__search_nodes, mcp__workflowy__list_children, mcp__workflowy__get_subtree, mcp__workflowy__create_node, mcp__workflowy__edit_node, mcp__workflowy__delete_node, mcp__workflowy__move_node, mcp__workflowy__insert_content, mcp__workflowy__smart_insert, mcp__workflowy__daily_review, mcp__workflowy__get_recent_changes, mcp__workflowy__list_overdue, mcp__workflowy__list_upcoming, mcp__workflowy__list_todos, mcp__workflowy__get_project_summary, mcp__workflowy__tag_search, mcp__workflowy__bulk_update
---

# Workflowy Day-to-Day Manager

Manage daily work through Workflowy. Route to the appropriate command based on `$ARGUMENTS`.

## Entry Point

Parse `$ARGUMENTS` to determine the command:

| First argument | Command |
|----------------|---------|
| (empty) or `daily` | → Daily Prioritisation |
| `weekly` | → Weekly Prioritisation |
| `monthly` | → Monthly Prioritisation |
| `capture` | → Task Capture (remaining args are category + task) |
| `status` | → Project Status |
| `triage` | → Inbox Triage |
| `reading` | → Reading List Management |
| `journal` | → Journal Check-in |

If the first argument doesn't match any command, treat the entire `$ARGUMENTS` as a task to capture.

---

## Workflowy Structure

The user's Workflowy has this layout:

- **Tasks** (top-level node) — contains level-2 domain/category nodes:
  - Personal Tasks, Dromologue Tasks, Office Tasks (task domains)
  - **Reading List** (also under Tasks, used for `/wmanage reading`)
  - (others discovered dynamically via `list_children`)
- **Inbox** (top-level node) — untriaged links, ideas, items to process
- **Tags** (top-level node) — tag definitions discovered dynamically
- **Resources** (top-level node) — contains a **Links** node with sub-folders for archiving read/triaged links
- **Journal** (top-level node) — date-based journal entries, each child is a date node (e.g. "2026-04-03")

### Node Classification

Be aware of two classification systems:

1. **Workflowy todo states**: Nodes can be regular bullets, todos (unchecked `[ ]`), or completed (checked `[x]`). When creating tasks, always set them as Workflowy todo type. When filtering, use todo state: incomplete = active, completed = done.

2. **Custom tags**: Discovered at runtime from the Tags node. Tags are cross-cutting labels (e.g. `#urgent`, `#waiting`, `#someday`). Discover available tags by reading the Tags node when needed. Use tags to:
   - Filter and group items in reviews
   - Suggest tags when capturing tasks
   - Surface patterns like "5 items tagged #urgent across domains"

---

## Node Link Management

The skill maintains a cached table of Workflowy node IDs in the memory file `workflowy_node_links.md`. This avoids repeated `find_node` calls for structural nodes that rarely change.

**Memory file location** — try these paths in order, use the first one found by the `Read` tool:

1. `.auto-memory/workflowy_node_links.md` (Cowork sessions — relative to session mount)
2. `$HOME/.claude/memory/workflowy_node_links.md` (global fallback for Claude Code)

**If ALL reads fail** (file not found in any location): create an empty memory file using the template below at the first writable path. Then run first-use population (step 5 of bootstrap) to discover and populate the user's actual node IDs.

### Memory File Template

When creating or repairing the memory file, use this exact format. All ID fields start empty — the bootstrap populates them by discovering the user's actual Workflowy structure.

```markdown
---
name: Workflowy Node Links
description: Cached Workflowy node IDs for structural nodes — avoids repeated search_nodes calls
type: reference
---

## Structural Nodes (rarely change)

| Node Name | Node ID | Last Verified |
|-----------|---------|---------------|
| Tasks     |         |               |
| Inbox     |         |               |
| Tags      |         |               |
| Resources |         |               |
| Links     |         |               |
| Journal   |         |               |
| Reading List |      |               |

## Domain Nodes (under Tasks — discovered dynamically)

| Node Name | Node ID | Last Verified |
|-----------|---------|---------------|

## Priority Nodes (change periodically)

| Node Type | Node Name | Node ID | Last Verified |
|-----------|-----------|---------|---------------|
|           |           |         |               |
```

**Node name matching**: Structural node names in the user's Workflowy may include emoji prefixes (e.g. "📋 Tasks") or hashtags (e.g. "#tags"). When searching, match by substring — e.g. search for "Tasks" and accept any node whose name contains "Tasks" at the top level.

### Bootstrap (run BEFORE every command)

**PERFORMANCE RULE: The bootstrap must be fast. Never use `find_node` for structural nodes. Use `search_nodes` with `max_depth:1` as a last resort only.**

1. **Read** the node links memory file using the `Read` tool, trying each path in the order listed above.
   - If a file is found and has IDs populated → proceed to step 2.
   - If a file is found but has empty ID columns → proceed to step 5 (first-use population).
   - **If no file is found at any path** → create an empty file from the template at the first writable location, then proceed to step 5 (first-use population).

2. **Identify needed nodes** for the current command — **only load what this command requires**:
   - `daily`: Tasks + Weekly priorities node
   - `weekly`: Tasks + Monthly priorities node
   - `monthly`: Tasks + previous Monthly priorities node
   - `capture`: Tasks + Tags
   - `triage`: Tasks + Inbox + Links
   - `reading`: Tasks + Links + Reading List
   - `journal`: Tasks + Journal
   - `status`: Tasks only

3. **Skip validation if recently verified**: For each needed node that has a stored ID:
   - If `Last Verified` is **today's date** → **trust it, skip validation entirely** (no API call)
   - If `Last Verified` is within the last 7 days → **trust it, skip validation** (structural nodes don't move)
   - If `Last Verified` is older than 7 days or empty → validate via `get_node(stored_id)` (one fast API call per node)

4. **Resolve missing or invalid entries** (rare — only on first use or after structural changes):
   - Use `search_nodes(query="node name", max_depth=1, max_results=3)` — this is faster than `find_node` because it doesn't fetch full subtrees
   - **NEVER use `find_node`** for structural bootstrap — it fetches entire subtrees and is slow
   - Update the memory file with the correct Node ID and today's date as `Last Verified`

5. **First-use population**: If the memory file has few or no IDs, resolve only the nodes needed for the current command (not all 7). Use parallel `search_nodes` calls where possible.

### Updating Priority Nodes

Whenever the skill **creates** a new priority node (Monthly Priorities, Week of, Today) or **discovers** one via search:
- Update the Priority Nodes table in the memory file with the node's name, ID, and today's date
- Replace any previous entry of the same type (e.g. old "Week of" entry gets overwritten by the new one)

### Validation Rules

- Structural nodes: validate by name substring match (node names may contain emoji prefixes like "📋 Tasks")
- Priority nodes: validate by prefix match (e.g. node name starts with "Monthly Priorities" or "Week of" or "Today —")
- If a priority node's `Last Verified` date is older than 30 days, treat it as stale and re-resolve

---

## Prioritisation Cascade

The three time-horizon commands form a cascade. Each level reads the one above it for context:

```
Monthly priorities (themes, goals, big rocks)
  ↓ informs
Weekly priorities (this week's focus)
  ↓ informs
Daily priorities (today's tasks)
  ↓ context for
capture, triage, status, reading
```

Priority nodes are stored in Workflowy so they persist across sessions:
- "Monthly Priorities — April 2026" (under Tasks)
- "Week of 2026-03-31" (under Tasks)
- "Today — 2026-04-03" (under Tasks, optional)

---

## Command: Daily Prioritisation

**Default command** — runs when `/wmanage` is invoked with no arguments or with `daily`.

### Steps

1. **Load weekly context**: Use the cached Weekly priorities node ID from the node links memory file. If no cached ID or validation fails, use `search_nodes(query="Week of", max_depth=2, parent_id=Tasks_ID)` to find the most recent "Week of" node under Tasks, then update the memory file. Read its children to understand this week's priorities.

2. **Get daily review**: Call `daily_review` to get overdue items, upcoming items, and recent changes in one call.

3. **Present prioritised view**: Show the user:
   - Overdue items (grouped by domain, with urgency)
   - Today's upcoming items
   - How these relate to this week's priorities
   - Any items tagged #urgent or similar across domains

4. **Ask for today's focus**: Present the most important items and ask the user to confirm 1-3 focus items for today. Use `AskUserQuestion` with the top candidates as options.

5. **Optionally insert**: If the user wants, insert a "Today — [YYYY-MM-DD]" node under Tasks (using cached Tasks ID) with the chosen focus items as todo-type children. Update the Today entry in the Priority Nodes table of the memory file.

---

## Command: Weekly Prioritisation

Runs when `/wmanage weekly` is invoked.

### Steps

1. **Load monthly context**: Use the cached Monthly priorities node ID from the node links memory file. If no cached ID or validation fails, use `search_nodes(query="Monthly Priorities", max_depth=2, parent_id=Tasks_ID)` to find the most recent "Monthly Priorities" node under Tasks, then update the memory file. Read its children for this month's themes.

2. **Review the past week**:
   - Call `get_recent_changes` with a 7-day window to see what was modified
   - Call `list_overdue` to find items that slipped
   - Call `list_upcoming` with a 7-day window for what's coming

3. **Summarise**: Present to the user:
   - What got done this week (completed todos from recent changes)
   - What slipped (overdue items)
   - What's coming next week
   - Progress against monthly priorities

4. **Set weekly focus**: Ask the user to choose 3-5 focus items for the coming week, informed by monthly priorities. Use `AskUserQuestion`.

5. **Insert**: Create a "Week of [YYYY-MM-DD]" node under Tasks (using cached Tasks ID) with the chosen priorities as todo-type children. Include any carried-over items from the previous week. Update the Weekly entry in the Priority Nodes table of the memory file with the new node's ID.

---

## Command: Monthly Prioritisation

Runs when `/wmanage monthly` is invoked.

### Steps

1. **Review task domains**: Call `get_project_summary` on the Tasks node (using cached Tasks ID) to get a high-level view of all domains — node counts, todo states, tags, overdue items.

2. **Load previous month**: Use the cached Monthly priorities node ID from the memory file. If no cached ID or validation fails, use `search_nodes(query="Monthly Priorities", max_depth=2, parent_id=Tasks_ID)` to find the previous "Monthly Priorities" node. Review what was achieved vs. planned.

3. **Present themes**: Show the user:
   - Summary of each domain (items count, overdue, tags)
   - Previous month's priorities and their status
   - Tag patterns across all domains (e.g. clusters of #urgent items)

4. **Set monthly priorities**: Ask the user to define 3-5 monthly priorities (themes or big rocks). Use `AskUserQuestion`.

5. **Insert**: Create a "Monthly Priorities — [Month Year]" node under Tasks (using cached Tasks ID) with the priorities as children. Each priority can have sub-items for specific deliverables. Update the Monthly entry in the Priority Nodes table of the memory file with the new node's ID.

---

## Command: Task Capture

Runs when `/wmanage capture` is invoked. Remaining arguments after `capture` are parsed as `[category] <task description>`.

### Steps

1. **Parse arguments**: After removing `capture` from `$ARGUMENTS`:
   - If two+ words and the first word looks like a category name → treat first word as category, rest as task
   - If unclear → treat everything as the task description and ask for category

2. **Discover domains**: Use the cached Tasks node ID, then `list_children` to get all level-2 domain nodes.

3. **Match category**:
   - If category argument given, fuzzy-match it to a domain name (case-insensitive, partial match OK)
   - If no category or no match, present the domains to the user via `AskUserQuestion` and ask which one

4. **Discover tags** (optional): Use the cached Tags node ID → `list_children` to get available tags. Ask the user if they want to apply any tags to the new task.

5. **Create the task**: Use `insert_content` or `smart_insert` to add the task under the chosen domain. Set it as a **Workflowy todo type**. Include any tags in the node name or note.

6. **Confirm**: Tell the user: "Added '[task]' to [Domain] as a todo" (and list any tags applied).

---

## Command: Project Status

Runs when `/wmanage status` is invoked.

### Steps

1. **Get summary**: Call `get_project_summary` on the Tasks node (using cached Tasks ID) to get comprehensive stats.

2. **Load current priorities**: Use the cached Monthly and Weekly priority node IDs from the memory file for context. If missing or invalid, fall back to `search_nodes` with the appropriate query and update the memory file.

3. **Present status**: Show the user:
   - Per-domain breakdown (total items, active todos, completed, overdue)
   - Tag distribution across domains
   - Progress against current monthly/weekly priorities
   - Items needing attention (overdue, tagged #urgent)

4. **Suggest actions**: Based on the status, suggest next steps (e.g. "3 overdue items in Office — consider `/wmanage daily` to reprioritise").

---

## Command: Inbox Triage

Runs when `/wmanage triage` is invoked.

### Steps

1. **Load Inbox**: Use the cached Inbox node ID, then `list_children` to get all items.

2. **Load context**: Use cached Monthly/Weekly priority node IDs to inform triage decisions.

3. **Discover domains**: Use the cached Tasks node ID → `list_children` to get available domains.

4. **Discover link folders**: Use the cached Resources and Links node IDs → `list_children` on Links to get available link sub-folders (e.g. "Tech", "Design", "Research"). These are the archive destinations for URL items.

5. **Process each item interactively**: For each Inbox item:
   - Show the item content
   - If it's a URL, briefly describe what it looks like (don't fetch yet — save that for `/wmanage reading`)
   - Suggest a destination based on content and current priorities
   - Ask the user via `AskUserQuestion`:
     - **If it's a link/URL**: Present the Links sub-folders as options (e.g. "Archive to Links > Tech", "Archive to Links > Research"), plus Move to Reading List, Keep in Inbox, Delete
     - **If it's a task/idea**: Present task domains (Office, Home, etc.), plus Keep in Inbox, Delete

6. **Execute moves**: For items the user chose to move, use `move_node` to relocate them. Links go to the chosen sub-folder under Resources > Links. Tasks go to the chosen domain under Tasks (set as todo type).

6. **Summary**: Report how many items were triaged and where they went.

**Batch option**: If there are many items (>10), offer to show them all first and let the user make bulk decisions before executing.

---

## Command: Reading List Management

Runs when `/wmanage reading` is invoked.

### Steps

1. **Load Reading List**: Use the cached Reading List node ID, then `list_children` to get all items.

2. **Load priorities**: Use cached Monthly/Weekly priority node IDs for relevance scoring.

3. **Fetch and summarise**: For each item that looks like a URL:
   - Use `WebFetch` to retrieve the page content
   - Generate a 1-2 sentence summary
   - Score relevance (1-5) against current priorities
   - Note the estimated reading time if possible

4. **Present prioritised list**: Show all reading items sorted by relevance:
   - Title / URL
   - Summary
   - Relevance to current priorities (and why)
   - Suggested priority: Read Now / Read This Week / Someday / Archive

5. **Discover link folders**: Use cached Resources and Links node IDs → `list_children` on Links to get available link archive sub-folders.

6. **Ask for actions**: For each item, use `AskUserQuestion` to let user choose:
   - **Read Now** — keep on Reading List, mark high priority
   - **Read This Week** — keep on Reading List, mark medium priority
   - **Someday** — keep on Reading List, mark low priority
   - **Archive to Links > [folder]** — present the Links sub-folders as options, move the item there
   - **Delete** — remove the item

7. **Execute**: Use `edit_node` to add summaries to node notes. For items marked "Archive", use `move_node` to relocate them to the chosen sub-folder under Resources > Links. Reorder remaining Reading List items by priority.

---

## Command: Journal Check-in

Runs when `/wmanage journal` is invoked.

### Steps

1. **Find the Journal node**: Use the cached Journal node ID from the node links memory file.

2. **Check for today's entry**: Call `list_children` on the Journal node and look for a child matching today's date (format: `YYYY-MM-DD`, e.g. "2026-04-03").

3. **Gather context** (to prompt a richer check-in):
   - If a daily priorities node exists for today ("Today — YYYY-MM-DD"), read it for what the user planned
   - If a weekly priorities node exists, read it for current focus areas
   - Check `daily_review` for what was completed recently

4. **Prompt the check-in**: Ask the user via `AskUserQuestion` what they'd like to journal about. Present context-aware prompts:
   - "How did today go against your priorities?"
   - "What's on your mind?"
   - "What did you learn today?"
   - "Free-form entry"

   Then let the user type their journal entry.

5. **Create or append to today's entry**:
   - If today's date node **doesn't exist**: Use `insert_content` to create a new child under Journal named with today's date (`YYYY-MM-DD`), with the journal text as children nodes
   - If today's date node **already exists**: Use `insert_content` to append new children to the existing date node (supporting multiple check-ins per day)

6. **Structure the entry**: Format the journal content as:
   ```
   YYYY-MM-DD
     Check-in — HH:MM
       [journal content as child nodes]
       Priorities today: [list from daily priorities if available]
       Completed: [list from daily_review if available]
   ```

7. **Confirm**: Tell the user their journal entry has been saved under Journal > today's date.

---

## Output Formatting

All content inserted into Workflowy must use 2-space hierarchical indentation:

```
Top level item
  Child item
    Grandchild item
  Another child
```

Use `-` bullets only when outputting to the user in chat. Do not use `-` bullets in content sent to Workflowy via `insert_content`.

---

## Error Handling

- If a Workflowy node is not found (e.g. no "Tasks" node), tell the user and ask them to confirm the node name
- If no priority nodes exist yet (first use), skip the "load context" step and note that the user should run `/wmanage monthly` first to establish priorities
- If WebFetch fails on a URL in reading list, note the failure and continue with other items
- If the Inbox is empty during triage, tell the user and suggest other commands
