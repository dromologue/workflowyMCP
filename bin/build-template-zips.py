#!/usr/bin/env python3
"""build-template-zips — package every generic skill under templates/skills/
into dist/<skill>.skill.zip, ready for a user to upload to claude.ai.

Deterministic (sorted members, fixed timestamps, fixed compression), so
rebuilding when nothing changed produces identical bytes. Run by the
pre-commit hook so the shipped zip can never lag templates/skills/<name>/.

Public-repo scope only: this walks templates/skills/, never anything
personal. A user's own populated wflow (node IDs, taxonomy) lives in their
own secondBrain-equivalent, never here — see BOOTSTRAP.md / docs/SETUP.md.
"""
from __future__ import annotations

import os
import sys
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SKILLS_DIR = os.path.join(REPO, "templates", "skills")
DIST_DIR = os.path.join(REPO, "dist")
FIXED_DATE = (2026, 1, 1, 0, 0, 0)  # reproducibility, not provenance

SKIP_NAMES = {".DS_Store"}


def files_under(root):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if not d.startswith("."))
        for f in sorted(filenames):
            if f in SKIP_NAMES or f.startswith("."):
                continue
            yield os.path.relpath(os.path.join(dirpath, f), root)


def build_one(skill: str) -> str:
    src = os.path.join(SKILLS_DIR, skill)
    out = os.path.join(DIST_DIR, f"{skill}.skill.zip")
    prefix = f"{skill}/"
    rels = list(files_under(src))
    if not any(r == "SKILL.md" for r in rels):
        print(f"build-template-zips: skipping {skill} — no SKILL.md", file=sys.stderr)
        return ""
    written_dirs = set()
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zf:
        for rel in rels:
            d = os.path.dirname(rel)
            if d and d not in written_dirs:
                parts = d.split(os.sep)
                for i in range(1, len(parts) + 1):
                    sub = os.sep.join(parts[:i])
                    if sub not in written_dirs:
                        zf.writestr(zipfile.ZipInfo(prefix + sub + "/", date_time=FIXED_DATE), b"")
                        written_dirs.add(sub)
            zi = zipfile.ZipInfo(prefix + rel, date_time=FIXED_DATE)
            zi.compress_type = zipfile.ZIP_DEFLATED
            zi.external_attr = 0o644 << 16
            with open(os.path.join(src, rel), "rb") as fh:
                zf.writestr(zi, fh.read())
    return out


def main() -> int:
    if not os.path.isdir(SKILLS_DIR):
        print(f"build-template-zips: no {SKILLS_DIR}", file=sys.stderr)
        return 1
    os.makedirs(DIST_DIR, exist_ok=True)
    built = []
    for skill in sorted(os.listdir(SKILLS_DIR)):
        if skill.startswith(".") or not os.path.isdir(os.path.join(SKILLS_DIR, skill)):
            continue
        out = build_one(skill)
        if out:
            built.append(out)
    for path in built:
        print(f"build-template-zips: wrote {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
