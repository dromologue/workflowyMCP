#!/usr/bin/env bash
# auto-bundle-skill.sh
#
# PostToolUse hook helper. Reads the Claude Code hook payload from
# stdin (JSON), decides whether the just-edited file is part of the
# wflow skill source, and if so re-bundles into dist/wflow.skill.zip
# and prints a clear "alert" line so the user knows to re-upload.
#
# Wired into .claude/settings.json on a `PostToolUse` Edit|Write
# matcher. Surfaced 2026-05-10: keeping the bundle in sync with the
# skill source by hand was unreliable — sessions that edited the
# skill but forgot to re-bundle produced a stale wflow.skill.zip
# that uploaded the previous version, which then looked like a
# claude.ai cache issue when it was actually a forgotten rebuild.
# The hook makes the rebuild deterministic and visible.
#
# Exit codes:
#   0 — no-op (file isn't part of the skill source) OR rebuild succeeded
#   non-zero — bundle script itself failed (missing source, frontmatter
#              violation, zip failure); the upstream tool call still
#              succeeded, the user is alerted that the rebuild needs
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

# The wflow skill source dir. When the file_path falls under this
# prefix, the bundle is stale and needs rebuilding. Anything else is
# a no-op for this hook.
SKILL_DIR="$HOME/.claude/skills/wflow"

# Resolve symlinks defensively — file_path may come through as the
# canonical path or as a path traversed through a symlink. The
# string-prefix check below is correct in both cases as long as
# SKILL_DIR is the canonical form, which on macOS / Linux it
# normally is.
case "$FILE_PATH" in
  "$SKILL_DIR"/*) ;;       # under skill dir → continue
  *) exit 0 ;;             # not relevant → no-op
esac

# Re-bundle. The bundler script lives next to this one in the repo.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUNDLER="$SCRIPT_DIR/bundle-skill.sh"
if [[ ! -x "$BUNDLER" ]]; then
  echo "⚠ auto-bundle: $BUNDLER missing or not executable — skill bundle is now STALE." >&2
  exit 1
fi

# Run the bundler quietly and capture its output for the alert. If
# it succeeds we print a single concise alert; on failure we surface
# the bundler's stderr so the user can fix the frontmatter / source
# error.
if BUNDLE_OUT="$("$BUNDLER" 2>&1)"; then
  # bundle-skill.sh prints to stdout: "✓ bundled <name> → <path>" plus
  # context lines. Pull the path so the alert names where to find the
  # fresh artefact.
  BUNDLE_PATH="$(grep -oE '/[^ ]+\.skill\.zip' <<<"$BUNDLE_OUT" | head -1)"
  echo "🛎  wflow skill changed — bundle rebuilt at ${BUNDLE_PATH:-dist/wflow.skill.zip}. Re-upload via claude.ai → Settings → Skills, then start a fresh session." >&2
  exit 0
else
  echo "⚠ auto-bundle: bundle-skill.sh failed — skill bundle is now STALE. Output:" >&2
  echo "$BUNDLE_OUT" >&2
  exit 1
fi
