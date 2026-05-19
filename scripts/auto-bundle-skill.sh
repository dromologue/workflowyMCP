#!/usr/bin/env bash
# auto-bundle-skill.sh
#
# PostToolUse hook helper. Reads the Claude Code hook payload from
# stdin (JSON), decides whether the just-edited file feeds either of
# the two wflow skill bundles (public or personal), and re-bundles
# whichever one(s) it touches.
#
# Two pipelines run in parallel:
#
#   1. PUBLIC bundle  →  <repo>/dist/wflow.skill.zip
#      Source:           <repo>/templates/skills/wflow/
#      Triggers:         any Edit/Write under that directory.
#      Distribution:     generic; safe to share, no personal data.
#
#   2. PERSONAL bundle →  $SECONDBRAIN_DIR/dist/wflow-personal.skill.zip
#      Source:            ~/.claude/skills/wflow/ (live personal skill)
#      Triggers:          any Edit/Write under ~/.claude/skills/wflow/,
#                         OR an edit whose realpath matches a file in
#                         that directory (e.g. the canonical at
#                         $SECONDBRAIN_DIR/memory/{taxonomy,node_links}.md
#                         that the personal skill symlinks to).
#      Distribution:      personal upload to the user's claude.ai
#                         account; lives under $SECONDBRAIN_DIR (Drive)
#                         so the personal data never leaves the user's
#                         own storage.
#
# Wired into .claude/settings.json on a `PostToolUse` Edit|Write|MultiEdit
# matcher. Surfaced 2026-05-10 (single bundle); split into two pipelines
# 2026-05-19 after the user asked that dist/ contain no personal data
# AND that personal-skill changes auto-publish to Drive.
#
# Exit codes:
#   0 — no-op (file isn't part of any skill source) OR all matching
#       rebuilds succeeded
#   non-zero — at least one bundler invocation failed (frontmatter
#              violation, source error, zip failure); the upstream tool
#              call still succeeded, the user is alerted that a rebuild
#              needs attention.

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
PERSONAL_SKILL_DIR="$HOME/.claude/skills/wflow"
BUNDLER="$SCRIPT_DIR_FOR_REPO/bundle-skill.sh"

if [[ ! -x "$BUNDLER" ]]; then
  echo "⚠ auto-bundle: $BUNDLER missing or not executable — skill bundle is now STALE." >&2
  exit 1
fi

# Classify the edit. An edit can match BOTH pipelines (e.g. if a
# personal-skill symlink happens to also live under templates — not
# the case today, but the classifier is robust regardless).
MATCH_PUBLIC=0
MATCH_PERSONAL=0

case "$FILE_PATH" in
  "$TEMPLATE_SKILL_DIR"/*) MATCH_PUBLIC=1 ;;
esac

case "$FILE_PATH" in
  "$PERSONAL_SKILL_DIR"/*) MATCH_PERSONAL=1 ;;
esac

# realpath check: an edit to the canonical at $SECONDBRAIN_DIR/memory/
# transparently propagates through the personal skill's symlinks but
# wouldn't fire either string-prefix check above. Resolve the edited
# path to its inode and compare against every file in the personal
# skill dir. If a match is found, the personal bundle is stale.
if [[ $MATCH_PERSONAL -eq 0 && -d "$PERSONAL_SKILL_DIR" ]]; then
  FILE_REAL="$(realpath "$FILE_PATH" 2>/dev/null || true)"
  if [[ -n "$FILE_REAL" ]]; then
    for f in "$PERSONAL_SKILL_DIR"/*; do
      [[ -e "$f" ]] || continue
      F_REAL="$(realpath "$f" 2>/dev/null || true)"
      if [[ -n "$F_REAL" && "$F_REAL" == "$FILE_REAL" ]]; then
        MATCH_PERSONAL=1
        break
      fi
    done
  fi
fi

if [[ $MATCH_PUBLIC -eq 0 && $MATCH_PERSONAL -eq 0 ]]; then
  exit 0
fi

# Run a bundler invocation and report. Captures stdout+stderr so on
# failure the user gets the diagnostic; on success we print a one-line
# alert that names the rebuilt path.
run_bundle() {
  local label="$1"
  local src="$2"
  local out="$3"

  local out_dir
  out_dir="$(dirname "$out")"
  mkdir -p "$out_dir" 2>/dev/null || true

  local result_out
  if result_out="$("$BUNDLER" --src "$src" --out "$out" 2>&1)"; then
    echo "🛎  wflow $label bundle rebuilt at $out — re-upload via claude.ai → Settings → Skills, then start a fresh session." >&2
    return 0
  else
    echo "⚠ auto-bundle: $label bundle FAILED — bundle is now STALE. Output:" >&2
    echo "$result_out" >&2
    return 1
  fi
}

EXIT=0

if [[ $MATCH_PUBLIC -eq 1 ]]; then
  run_bundle "public" "$TEMPLATE_SKILL_DIR" "$REPO_ROOT/dist/wflow.skill.zip" || EXIT=1
fi

if [[ $MATCH_PERSONAL -eq 1 ]]; then
  # Personal bundle lives under $SECONDBRAIN_DIR/dist/ so it tracks
  # the user's own Drive (the canonical home for personal-data
  # artefacts) rather than being staged under the repo's dist/.
  PERSONAL_OUT_DIR="${SECONDBRAIN_DIR:-}/dist"
  if [[ -z "${SECONDBRAIN_DIR:-}" ]]; then
    echo "⚠ auto-bundle: personal-skill edit detected but \$SECONDBRAIN_DIR is unset — cannot publish personal bundle." >&2
    echo "   Set SECONDBRAIN_DIR in the MCP host's env block AND your shell rc, then re-edit to trigger a rebuild." >&2
    EXIT=1
  else
    run_bundle "personal" "$PERSONAL_SKILL_DIR" "$PERSONAL_OUT_DIR/wflow-personal.skill.zip" || EXIT=1
  fi
fi

exit $EXIT
