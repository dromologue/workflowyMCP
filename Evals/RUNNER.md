# Claude Code Runner Prompt

Copy and paste this entire prompt into Claude Code from the directory containing this file.

It assumes the `wflow` skill is installed and the sandbox setup in `README.md` has been done.

---

You are running the wflow test suite. Read `evals/evals.json` from this directory.

For each eval in `evals.evals[]`, run this loop:

## Per-test loop

**1. Announce the test.** Print:

```
─────────────────────────────────────────────
Test {id} — {group}
Safety: {safety}
Prompt: {prompt}
─────────────────────────────────────────────
```

**2. Execute according to safety mode.**

- **`read_only`** — Run the prompt live with the wflow skill loaded. Use whichever read-side MCP tools the skill calls for (`workflowy:search_nodes`, `workflowy:list_children`, `remarkable_search`, `remarkable_browse`, etc.).

- **`requires_sandbox`** — Run the prompt live with the wflow skill loaded, but enforce these constraints on writes:
  - Every newly created Workflowy node MUST be a descendant of the node named "Test Sandbox" under Distillations (`7e351f77`). Look up its UUID before the first write of the run and store it.
  - Every newly created node's name MUST be prefixed with `[test:{id}]` so cleanup is trivial.
  - If the skill's natural workflow would create a node outside the sandbox, redirect the write into the sandbox and note the redirection in your transcript.
  - reMarkable is read-only by tooling so no special sandboxing needed there.

- **`plan_only`** — Run the prompt with the wflow skill loaded but DO NOT execute any MCP tool calls. Instead, in your transcript, narrate the exact tool calls you would make, in order, with the parameters you would pass — and provide a final answer based on that narration. The grader checks the narration against the expectations.

**3. Capture transcript.** After execution, summarise into a JSON object matching this shape:

```json
{
  "id": <int>,
  "group": "<string>",
  "safety": "<string>",
  "prompt": "<string>",
  "transcript_summary": "<2-4 sentence summary of what happened>",
  "tool_calls_made": [
    {"tool": "workflowy:search_nodes", "args_summary": "..."},
    {"tool": "remarkable_search", "args_summary": "..."}
  ],
  "nodes_created": [
    {"name": "[test:5] ...", "parent": "Test Sandbox / ..."}
  ],
  "final_response": "<the user-facing reply>"
}
```

**4. Grade.** For each expectation in `evals.evals[i].expectations`, judge pass / fail / unclear. Be strict: an expectation that says "the transcript shows X" requires concrete evidence in the captured transcript, not a plausible inference. Output:

```json
{
  "id": <int>,
  "expectations": [
    {
      "text": "<expectation text>",
      "passed": true,
      "evidence": "<one sentence pointing at the specific transcript element>"
    }
  ],
  "summary": {"passed": <int>, "failed": <int>, "unclear": <int>, "total": <int>, "pass_rate": <float>}
}
```

**5. Persist.** Append both objects to `results/run-{timestamp}.json` (create the directory if missing). Use one timestamp per full run, not per test.

## Run order

Run tests in the order they appear in `evals.json`. The order is intentional:

- Group A first establishes that each workflow triggers correctly
- Groups B-D test discipline and routing, building on A
- Groups E-G test the integration habits that turn the system into a wiki
- Group H confirms the skill doesn't over-trigger

Do NOT parallelise. The sandbox is a shared resource and the session log discipline depends on entries appearing in chronological order.

## Stop conditions

Stop the run early and surface to the user if:

- More than two consecutive `requires_sandbox` tests create nodes outside the sandbox boundary — this indicates the sandbox enforcement is broken and continuing will pollute real data.
- Any reMarkable test triggers a full-stack timeout (matching test 22's failure condition) — bail out, run the test 22 diagnostic, and ask the user to restart the reMarkable MCP server before resuming.
- Any test produces no transcript at all (model error, tool unavailable, etc.) — log it and skip; do not retry blindly.

## Final summary

When the run ends, print a table:

```
GROUP                          TESTS   PASS  FAIL  UNCLEAR  PASS_RATE
A. Workflow Triggering         8       _     _     _        _.__
B. Atomic Note Discipline      3       _     _     _        _.__
...
TOTAL                          26      _     _     _        _.__
```

Then surface the three failures most worth attention — pick the ones that point at clear gaps in the skill's instruction set rather than borderline judgement calls. For each, name the gap in one sentence and propose one concrete edit to `SKILL.md`.

## Cleanup

After printing the summary, ask the user whether to delete or archive the `Test Sandbox` subtree. Default to archive — they may want to inspect the artifacts before discarding.
