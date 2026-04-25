# wflow Test Suite

A test suite for the `wflow` skill — the integrated Workflowy + reMarkable second-brain workflow. Designed to be picked up and executed by Claude Code without further explanation.

## What's covered

26 tests across 8 groups:

| Group | Count | What it exercises |
|---|---|---|
| A. Workflow Triggering | 8 | One test per workflow defined in the skill: morning review, reading triage, reMarkable extraction, cross-system research, synthesis capture, task capture, link management, journal check-in. |
| B. Atomic Note Discipline | 3 | Multi-claim decomposition, single-concept compression, backlink discipline. |
| C. Mirror and MOC Discipline | 3 | Cross-pillar mirroring with `mirror_of:` references, single-pillar restraint, source MOC creation. |
| D. Routing | 3 | Engineering-to-Build, decision-to-Decide-with-Lead-mirror, ambiguous routing requiring user input. |
| E. Cross-System | 2 | Pre-filing duplication detection, contradiction surfacing with `#revisit`. |
| F. Tool Reliability | 3 | `remarkable_image` vs `remarkable_read` for handwritten content, `move_node` retry pattern, full-stack reMarkable timeout diagnosis. |
| G. End-of-Session Discipline | 2 | Proactive distillation offer, mid-session task capture. |
| H. Negative / Sanity | 2 | Out-of-scope query (no skill invocation), draft thought (no mirror propagation). |

## Safety model

Each test carries a `safety` field that controls execution mode:

- **`read_only` (6 tests)** — exercises only read paths (search, list, retrieve). Run live against the real Workflowy and reMarkable without risk.
- **`requires_sandbox` (16 tests)** — writes to Workflowy. Must scope all writes to a sandbox node so they cannot pollute the canonical Distillations layer.
- **`plan_only` (4 tests)** — Claude Code runs the prompt with the wflow skill loaded but with tool execution disabled or interrupted. The grader scores Claude's *stated approach* against the expectations rather than the executed result. Useful for tests that simulate failure conditions (e.g. reMarkable server down) or that test recovery patterns without breaking real state.

## Sandbox setup (one-off)

Before the first run that includes `requires_sandbox` tests:

1. In Workflowy, under Distillations (`7e351f77`), create a node named `Test Sandbox`.
2. Note its UUID.
3. Set the environment variable `WFLOW_TEST_SANDBOX_NODE_ID=<uuid>` so the runner can pass it into the executor's context.
4. Confirm tests prefix newly created nodes with `[test:<id>]` to make cleanup trivial.

After a run, archive or delete the `Test Sandbox` subtree to reset state. The skill's mirror discipline ensures real canonical nodes are not touched as long as the sandbox boundary holds.

## Running with Claude Code

The simplest invocation — paste this prompt into Claude Code from the directory containing this README:

```
Read /path/to/wflow-tests/evals/evals.json. For each eval object:
1. Print the id, group, safety, and prompt.
2. If safety is "plan_only", run the prompt with the wflow skill loaded but answer 
   the prompt without executing any MCP tool calls — describe what you would do 
   and let the expectations be checked against that description.
3. If safety is "read_only", run the prompt live with the wflow skill loaded.
4. If safety is "requires_sandbox", scope all writes to descendants of the node 
   named "Test Sandbox" under Distillations. Do not touch any node outside that 
   subtree. Prefix every node you create with "[test:<id>]".
5. After each eval completes, score the transcript against each expectation: 
   pass / fail / unclear, with a one-sentence evidence note per expectation.
6. Save results to results/results-<timestamp>.json following the grading.json 
   schema in /mnt/skills/examples/skill-creator/references/schemas.md.
7. At the end, print a summary table grouped by category.
```

## Running with the skill-creator harness

For full benchmarking with statistical aggregation across multiple runs, use the
skill-creator's `run_eval.py`:

```
python -m skill-creator.scripts.run_eval \
  --eval-set /path/to/wflow-tests/evals/evals.json \
  --skill-path /path/to/installed/wflow/ \
  --model claude-opus-4-7 \
  --runs-per-eval 3
```

This gives you variance estimates per test, which is useful when iterating on the
skill description or workflow definitions and wanting to know if a change is real
or noise. Note the harness ignores the non-standard `safety` field — you must
either manually flag which tests are sandbox-eligible for a given run, or run all
tests in plan-only mode by adding the relevant instruction to the runner prompt.

## Interpreting failures

Three failure modes worth distinguishing in the results:

1. **Discipline failures** — Claude triggered the right workflow but skipped a
   discipline rule (no source attribution, no mirror, no session log). These point
   at gaps in the skill's instruction text. Fix in `SKILL.md`.

2. **Routing failures** — Claude misclassified a claim into the wrong pillar.
   These point at either ambiguous routing rules in the skill or a genuinely
   borderline case that should have been flagged for confirmation. Look at test
   17 (Edmondson) for the pattern of how ambiguity should be surfaced.

3. **Tool reliability failures** — Claude didn't apply the documented retry / OCR
   / diagnostic patterns. These point at either the patterns being buried too
   deep in the skill body or the model under-triggering them. Promote them
   higher up in `SKILL.md` if they keep failing.

## Where this stops short

The suite tests behaviour, not graph integrity. After a meaningful run, run a
hand check on a few sample distillations to confirm:

- Mirrors actually propagate edits (change canonical, observe mirror updates)
- Backlinks resolve in the Workflowy UI
- The Cross-pillar concept maps node remains coherent rather than becoming a
  dumping ground

These structural checks aren't easily encoded as expectations — they need a human
eye on the graph itself.
