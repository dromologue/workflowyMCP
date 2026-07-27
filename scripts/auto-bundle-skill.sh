#!/usr/bin/env bash
# auto-bundle-skill.sh
#
# PostToolUse hook helper. Reads the Claude Code hook payload from
# stdin (JSON), decides whether the just-edited file feeds the PUBLIC
# wflow skill bundle, and re-bundles it if so.
#
#   PUBLIC bundle  →  <repo>/dist/wflow.skill.zip
#   Source:           <repo>/templates/skills/wflow/
#   Triggers:         any Edit/Write under that directory.
#   Distribution:     generic; safe to share, no personal data.
#
# Wired into .claude/settings.json on a `PostToolUse` Edit|Write|MultiEdit
# matcher. Surfaced 2026-05-10 (single bundle); split into two pipelines
# 2026-05-19 (public here, personal via a live-edit-triggered path into
# $SECONDBRAIN_DIR/dist/wflow-personal.skill.zip).
#
# PERSONAL PIPELINE RETIRED 2026-07-27. It duplicated a job secondBrain's
# own repo now does more robustly: a git pre-commit hook there
# (scripts/build-skill-zips.py) rebuilds dist/wflow.zip deterministically
# on every commit, and a matching hook in dromologue-sync verifies it at
# push time — proven correct by a control-evals suite this live-edit
# trigger never had. Running both meant two mechanisms building
# equivalent content under two different filenames
# (wflow-personal.skill.zip here vs wflow.zip there), which is exactly
# the kind of duplicate-cache confusion this estate has spent this
# session eliminating everywhere else. See secondBrain's own
# memory/workflowy_node_links.md change log (2026-07-27) for the fuller
# account, and dromologue-estate-sync.md for the cross-repo record.
#
# Exit codes:
#   0 — no-op (file isn't part of the public skill source) OR the
#       rebuild succeeded
#   non-zero — the bundler invocation failed (frontmatter violation,
#              source error, zip failure); the upstream tool call still
#              succeeded, the user is alerted that a rebuild needs
#              attention.

set -uo pipefail

# Read the hook JSON payload from stdin. Tolerate empty / non-JSON
# payloads (e.g. when run by hand for testing) by silently no-op'ing.
PAYLOAD="$(cat)"
if [[ -z "$PAYLOAD" ]]; then
  exit 0
fi

# Extract the file_path the tool acted on. Both Edit and Write put it
# at tool_input.file_path. jq is the standard claude-code dependency
# but we fall back to a python one-liner if it's not on PATH so the
# hook is robust on minimal hosts.
extract_file_path() {
  if command -v jq >/dev/null 2>&1; then
    jq -r '.tool_input.file_path // empty' <<<"$PAYLOAD"
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    print(d.get("tool_input", {}).get("file_path", ""))
except Exception:
    pass
' <<<"$PAYLOAD"
  else
    # No JSON parser available — let the hook silently no-op rather
    # than failing every Edit / Write in the session.
    echo ""
  fi
}

FILE_PATH="$(extract_file_path)"
if [[ -z "$FILE_PATH" ]]; then
  exit 0
fi

SCRIPT_DIR_FOR_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR_FOR_REPO/.." && pwd)"
TEMPLATE_SKILL_DIR="$REPO_ROOT/templates/skills/wflow"
BUNDLER="$SCRIPT_DIR_FOR_REPO/bundle-skill.sh"

if [[ ! -x "$BUNDLER" ]]; then
  echo "⚠ auto-bundle: $BUNDLER missing or not executable — skill bundle is now STALE." >&2
  exit 1
fi

case "$FILE_PATH" in
  "$TEMPLATE_SKILL_DIR"/*) ;;
  *) exit 0 ;;
esac

# Run the bundler and report. Captures stdout+stderr so on failure the
# user gets the diagnostic; on success we print a one-line alert that
# names the rebuilt path.
OUT="$REPO_ROOT/dist/wflow.skill.zip"
mkdir -p "$(dirname "$OUT")" 2>/dev/null || true

if RESULT_OUT="$("$BUNDLER" --src "$TEMPLATE_SKILL_DIR" --out "$OUT" 2>&1)"; then
  echo "🛎  wflow public bundle rebuilt at $OUT" >&2
  exit 0
else
  echo "⚠ auto-bundle: public bundle FAILED — bundle is now STALE. Output:" >&2
  echo "$RESULT_OUT" >&2
  exit 1
fi
