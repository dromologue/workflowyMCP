#!/usr/bin/env bash
# bundle-skill.sh
#
# Bundle a wflow skill source directory into a claude.ai-uploadable zip.
#
# Anthropic's skill format expects a top-level directory inside the
# zip whose name matches the skill name in SKILL.md frontmatter, with
# SKILL.md and any companion files inside. Flat-at-root zips do not
# parse on the upload side. This script builds that structure exactly,
# validates the frontmatter so common upload-side rejections (XML-like
# tags in description, description over the 1024-char cap) fail at
# bundle time instead of at upload time, and writes to a predictable
# path under the repo's `dist/` directory.
#
# Surfaced as a need 2026-05-01 (todo: "Automate skill bundling").
# Created 2026-05-10 after a session-end audit found that the previous
# instructions assumed a `bundle-skill.sh` rig that did not exist on
# disk anywhere; bundling was being done ad-hoc by hand each time.
#
# Usage:
#   scripts/bundle-skill.sh                        # defaults: <repo>/templates/skills/wflow → dist/wflow.skill.zip
#   scripts/bundle-skill.sh --src <dir>            # override skill source
#   scripts/bundle-skill.sh --out <zip>            # override output path
#   scripts/bundle-skill.sh --src <dir> --out <z>  # both
#
# Default source is the GENERIC template skill in this repo. The bundle
# at dist/wflow.skill.zip is meant to be public-distributable — it carries
# no user-specific node IDs, no personalised pillars, no cached
# Workflowy data. Personal customisations (Justin's live skill at
# `~/.claude/skills/wflow/`) are intentionally NOT bundled here; for a
# personal upload to claude.ai, bundle that source explicitly with
# `--src ~/.claude/skills/wflow --out <somewhere personal>`.
#
# Exit codes:
#   0  — bundle written, frontmatter clean
#   1  — source dir missing, SKILL.md missing
#   2  — frontmatter validation failed (XML tags, oversized description)
#   3  — zip command failed

set -euo pipefail

# ── Defaults ────────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_SRC="$REPO_ROOT/templates/skills/wflow"
DEFAULT_OUT="$REPO_ROOT/dist/wflow.skill.zip"

SRC_DIR="$DEFAULT_SRC"
OUT_ZIP="$DEFAULT_OUT"

# ── Parse args ──────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --src)
      SRC_DIR="$2"
      shift 2
      ;;
    --out)
      OUT_ZIP="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '1,/^set -e/p' "$0" | sed 's/^# \?//;/^set -e/d'
      exit 0
      ;;
    *)
      echo "unknown arg: $1" >&2
      exit 1
      ;;
  esac
done

# ── Pre-flight ──────────────────────────────────────────────────────────────
if [[ ! -d "$SRC_DIR" ]]; then
  echo "ERROR: skill source directory not found: $SRC_DIR" >&2
  exit 1
fi

SKILL_MD="$SRC_DIR/SKILL.md"
if [[ ! -f "$SKILL_MD" ]]; then
  echo "ERROR: SKILL.md not found at $SKILL_MD" >&2
  exit 1
fi

SKILL_NAME="$(basename "$SRC_DIR")"

# ── Frontmatter validation ──────────────────────────────────────────────────
# Extract lines between the first two `---` markers.
FRONTMATTER="$(awk '/^---$/{c++; next} c==1{print}' "$SKILL_MD")"

if [[ -z "$FRONTMATTER" ]]; then
  echo "ERROR: $SKILL_MD has no YAML frontmatter (expected --- ... --- block at top)" >&2
  exit 2
fi

# Extract the description value. Handle both quoted and unquoted forms.
# The description value runs from `description:` to the end of that line
# (descriptions are single-line YAML strings in this skill format).
DESCRIPTION_LINE="$(printf '%s\n' "$FRONTMATTER" | grep -m1 '^description:' || true)"
if [[ -z "$DESCRIPTION_LINE" ]]; then
  echo "ERROR: frontmatter has no 'description:' field" >&2
  exit 2
fi

# Strip the `description: ` prefix and the optional surrounding quotes.
DESCRIPTION_VALUE="${DESCRIPTION_LINE#description: }"
DESCRIPTION_VALUE="${DESCRIPTION_VALUE#\"}"
DESCRIPTION_VALUE="${DESCRIPTION_VALUE%\"}"

DESC_LEN=${#DESCRIPTION_VALUE}
if [[ $DESC_LEN -gt 1024 ]]; then
  echo "ERROR: description is $DESC_LEN chars; the upload cap is 1024." >&2
  echo "       Tighten the description field in $SKILL_MD." >&2
  exit 2
fi

# Sweep the entire frontmatter for `<` and `>` — the upload validator
# rejects any XML-like tags in the frontmatter (description and YAML
# comments alike). The 2026-05-10 incident: a `<service>` placeholder
# in the description and a `mcp__<service>__*` example in a comment
# both tripped the validator with no diagnostic.
if printf '%s\n' "$FRONTMATTER" | grep -q '[<>]'; then
  echo "ERROR: frontmatter contains '<' or '>' — the upload validator rejects XML-like tags." >&2
  echo "       Offending lines:" >&2
  printf '%s\n' "$FRONTMATTER" | grep -n '[<>]' | sed 's/^/         /' >&2
  exit 2
fi

# ── Build ───────────────────────────────────────────────────────────────────
mkdir -p "$(dirname "$OUT_ZIP")"

# Remove any prior bundle so the mtime is unambiguously fresh and stale
# files inside don't sneak in.
rm -f "$OUT_ZIP"

# zip from the parent directory so the archive carries a top-level
# `<skill-name>/` directory wrapper, which is what claude.ai's upload
# side expects. Excluding macOS resource-fork droppings keeps the
# archive clean.
PARENT_DIR="$(cd "$SRC_DIR/.." && pwd)"
(
  cd "$PARENT_DIR"
  zip -r -q "$OUT_ZIP" "$SKILL_NAME" \
    -x '*.DS_Store' '__MACOSX/*' '*/.git/*'
) || {
  echo "ERROR: zip failed" >&2
  exit 3
}

# ── Verify ──────────────────────────────────────────────────────────────────
# Confirm the bundled SKILL.md round-trips cleanly: frontmatter survived
# the bundle, no archive corruption.
BUNDLED_FRONTMATTER="$(unzip -p "$OUT_ZIP" "$SKILL_NAME/SKILL.md" | awk '/^---$/{c++; next} c==1{print}')"
if [[ -z "$BUNDLED_FRONTMATTER" ]]; then
  echo "ERROR: bundled SKILL.md has no frontmatter — archive may be corrupt." >&2
  exit 3
fi

BUNDLE_SIZE=$(stat -f%z "$OUT_ZIP" 2>/dev/null || stat -c%s "$OUT_ZIP")

# ── Report ──────────────────────────────────────────────────────────────────
echo "✓ bundled $SKILL_NAME → $OUT_ZIP"
echo "  source: $SRC_DIR"
echo "  size: $BUNDLE_SIZE bytes"
echo "  description: $DESC_LEN/1024 chars, frontmatter clean"
echo
echo "Upload via claude.ai → Settings → Skills, then start a fresh session."
