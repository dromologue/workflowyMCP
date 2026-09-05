#!/bin/sh
# check-method-sync — the wflow skills must state the same method.
#
# protects: the claim-based method's governing rules, which are restated in
#           every wflow skill (the generic template here, and each personal
#           skill derived from it). They must not drift: a model follows
#           whichever copy it loaded, and two copies stating different rules
#           is worse than one copy stating none.
# reads:    the block delimited by BEGIN/END SHARED METHOD BLOCK in each
#           SKILL.md, compared byte-for-byte.
# blind_to: whether the block is a GOOD statement of the method, whether the
#           per-file detail sections below it agree with it, and any skill
#           outside the paths listed in TARGETS (a copy this script has never
#           heard of drifts silently).
#
# Skips a target that is not checked out, like bin/check-twin-schema: a
# missing sibling repo is a normal clone, not a fault.
# Run with --self-test to prove it goes red.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
CANONICAL="$root/templates/skills/wflow/SKILL.md"
TARGETS="$HOME/code/secondBrain/skills/wflow/SKILL.md"

extract() {
  awk '/BEGIN SHARED METHOD BLOCK/{f=1;next} /END SHARED METHOD BLOCK/{f=0} f' "$1"
}

if [ "$1" = "--self-test" ]; then
  tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
  printf 'x\n<!-- BEGIN SHARED METHOD BLOCK -->\nRULE A\n<!-- END SHARED METHOD BLOCK -->\n' > "$tmp/a.md"
  printf 'y\n<!-- BEGIN SHARED METHOD BLOCK -->\nRULE B\n<!-- END SHARED METHOD BLOCK -->\n' > "$tmp/b.md"
  printf 'z\nno markers here\n' > "$tmp/c.md"
  [ "$(extract "$tmp/a.md")" != "$(extract "$tmp/b.md")" ] || { echo "self-test FAIL: drift not detected"; exit 1; }
  [ "$(extract "$tmp/a.md")" = "RULE A" ] || { echo "self-test FAIL: bad extraction"; exit 1; }
  [ -z "$(extract "$tmp/c.md")" ] || { echo "self-test FAIL: missing block should extract empty"; exit 1; }
  echo "check-method-sync: self-test OK (drift detected, extraction exact, absent block empty)"
  exit 0
fi

want=$(extract "$CANONICAL")
if [ -z "$want" ]; then
  echo "check-method-sync: FAIL — no SHARED METHOD BLOCK in $CANONICAL" >&2
  exit 1
fi

status=0
for t in $TARGETS; do
  [ -f "$t" ] || { echo "check-method-sync: skipped (not checked out) $t"; continue; }
  got=$(extract "$t")
  if [ -z "$got" ]; then
    echo "check-method-sync: FAIL — $t carries no SHARED METHOD BLOCK" >&2
    status=1
  elif [ "$got" != "$want" ]; then
    echo "check-method-sync: FAIL — method block drifted in $t" >&2
    wf=$(mktemp); gf=$(mktemp)
    printf '%s\n' "$want" > "$wf"; printf '%s\n' "$got" > "$gf"
    diff "$wf" "$gf" | head -20 >&2 || true
    rm -f "$wf" "$gf"
    echo "  fix: copy the block from $CANONICAL (it is canonical); do not edit one copy alone." >&2
    status=1
  else
    echo "check-method-sync: OK — $t matches"
  fi
done
exit $status
