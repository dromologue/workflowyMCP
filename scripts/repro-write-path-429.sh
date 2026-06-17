#!/usr/bin/env bash
#
# repro-write-path-429.sh — replay the 2026-06-17 write-path failure report's
# call sequence against a real Workflowy workspace and dump the STRUCTURED
# responses at each step, so engineering can confirm the typed envelopes the
# report asked for (Recommendations A + D).
#
# Background: the report observed a session where the write path entered a
# persistent failure state — every `create_node` / `insert_content` returned a
# bare "Tool execution failed" with no `error_kind`, no `retry_after`, no
# `committed_count` — while reads stayed healthy. The fixes:
#   A. A 429 reaching the per-call envelope now classifies as `rate_limited`
#      with a typed `retry_after_secs` + `retryable`, instead of `unknown`.
#   D. A hard mid-batch `insert_content` failure now returns a `partial`
#      envelope carrying `created_count` + `last_inserted_id` (the resume
#      cursor) + the classification fields, instead of discarding progress.
#
# This script does NOT force a 429 (you cannot reliably induce upstream rate
# limiting on demand). Its job is to replay the exact sequence and print every
# response verbatim. If the upstream rate-limits mid-run, you will SEE the new
# typed envelope rather than a bare failure — that is the confirmation. Under
# normal conditions it simply demonstrates the happy path and cleans up.
#
# Usage:
#   WORKFLOWY_API_KEY=... [WORKFLOWY_TEST_PARENT=<uuid>] scripts/repro-write-path-429.sh
#
# WORKFLOWY_TEST_PARENT (optional): a node ID to create the scratch root under.
#   Omit to create at the workspace root. The scratch root and everything under
#   it is deleted on exit (success or failure) via the cleanup trap.
#
# Exit status: non-zero if any step fails hard (mirrors `wflow-do` exit codes;
# an insert_content error-partial exits non-zero with the resume cursor on
# stdout).

set -uo pipefail

if [[ -z "${WORKFLOWY_API_KEY:-}" ]]; then
  echo "error: WORKFLOWY_API_KEY must be set (this script makes REAL writes)." >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Prefer a release binary if present (faster); else build debug once.
WFLOW=""
for cand in target/release/wflow-do target/debug/wflow-do; do
  [[ -x "$cand" ]] && WFLOW="$cand" && break
done
if [[ -z "$WFLOW" ]]; then
  echo "# building wflow-do (debug)…" >&2
  cargo build --bin wflow-do >&2
  WFLOW="target/debug/wflow-do"
fi

run() {
  echo ""
  echo "### \$ wflow-do $*"
  "$WFLOW" "$@"
}

SCRATCH_ID=""
cleanup() {
  if [[ -n "$SCRATCH_ID" ]]; then
    echo ""
    echo "### cleanup: deleting scratch root $SCRATCH_ID"
    "$WFLOW" delete "$SCRATCH_ID" >/dev/null 2>&1 \
      && echo "cleaned up." \
      || echo "WARNING: could not delete scratch root $SCRATCH_ID — remove it manually." >&2
  fi
}
trap cleanup EXIT

echo "=== Step 1: status (read-path health + rate-limit posture) ==="
echo "    Look for: degraded_kind, retry_after_remaining_ms, probe_suppressed."
run status || true

echo ""
echo "=== Step 2: create the scratch root ==="
CREATE_ARGS=(create --name "repro-write-path-429 $(date -u +%Y%m%dT%H%M%SZ)")
[[ -n "${WORKFLOWY_TEST_PARENT:-}" ]] && CREATE_ARGS+=(--parent "$WORKFLOWY_TEST_PARENT")
CREATE_OUT="$(run "${CREATE_ARGS[@]}")"
echo "$CREATE_OUT"
# Pull the new node id out of the JSON/text response (12+ hex with hyphens).
SCRATCH_ID="$(printf '%s' "$CREATE_OUT" | grep -oiE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' | head -1)"
if [[ -z "$SCRATCH_ID" ]]; then
  echo "error: could not parse the created node id from the response above." >&2
  exit 1
fi
echo "scratch root id = $SCRATCH_ID"

echo ""
echo "=== Step 3: insert_content batch 1 (small, expected to succeed) ==="
run insert "$SCRATCH_ID" --content $'Phase 1\n  item 1a\n  item 1b\n  item 1c' || true

echo ""
echo "=== Step 4: insert_content batch 2 (larger second batch) ==="
echo "    If the upstream rate-limits here, the response is a STRUCTURED"
echo "    partial: {status:partial, reason:error, created_count, last_inserted_id,"
echo "    proximate_cause:rate_limited, retry_after_secs, retryable} — NOT a bare"
echo "    'Tool execution failed'. Re-run with the remaining lines under"
echo "    last_inserted_id to resume."
BATCH2=$'Phase 2\n  item 2a\n  item 2b\n  item 2c\n  item 2d\n  item 2e\n  item 2f\n  item 2g'
run insert "$SCRATCH_ID" --content "$BATCH2" || echo "(insert exited non-zero — inspect the partial envelope above)"

echo ""
echo "=== Step 5: read back what actually landed (no separate guesswork) ==="
echo "    With Recommendation D, the failed write itself reports committed_count,"
echo "    so this read is confirmation, not the only way to learn the truth."
run children "$SCRATCH_ID" || true

echo ""
echo "=== Done. Scratch root will be deleted on exit. ==="
