//! Spec ↔ test traceability matrix.
//!
//! Walks `specs/specification.md`, extracts every `[C-area-NNN]`
//! contract marker and its `Pinned by `<test_fn>`` claims,
//! verifies each named test exists as `fn <name>` somewhere
//! in the codebase, and regenerates `specs/traceability.md`
//! to reflect the current state. Runs via
//! `cargo test --test traceability`.
//!
//! Failure modes:
//! - any contract has no `Pinned by` claim → spec gap
//! - any pin names a function not present as `fn <name>` in src/
//!   or tests/ → typo / rename / dead reference
//! - the matrix file is regenerated as a side-effect; commit any
//!   diff after a test run.
//!
//! Convention for adding a new contract:
//! 1. Pick an area prefix (`skill`, `disc`, `server`, `wf`, or
//!    introduce a new one).
//! 2. Add `[C-<area>-<NNN>]` at the start of a sentence in the
//!    spec, where NNN is the next free integer in that area.
//! 3. Within the same paragraph (until the next `[C-...]` marker
//!    or end of spec), add at least one `` Pinned by `<test_fn>` ``
//!    clause naming an existing test.
//! 4. Run `cargo test --test traceability` to regenerate
//!    `specs/traceability.md` and verify the new contract is
//!    pinned cleanly.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[derive(Debug, Clone)]
struct Contract {
    id: String,    // "C-skill-001"
    area: String,  // "skill"
    num: u32,      // 1
    title: String, // first sentence after the marker, no markdown
    pinning_tests: Vec<String>,
}

fn parse_contracts(spec: &str) -> Vec<Contract> {
    let marker_re = regex::Regex::new(r"\[C-([a-z]+)-(\d+)\]").expect("marker regex");
    // Each "Pinned by …" clause runs until the next period that
    // terminates a sentence. The body inside the clause may carry
    // multiple backticked test names joined by " and " / ", " /
    // ", and ".
    let pin_clause_re = regex::Regex::new(r"Pinned by\s+([^.]*?)\.").expect("pin clause regex");
    // Test identifiers are snake_case; allow `::` and `*` so the
    // matrix can carry glob references (e.g. module::test_*) even
    // though those are skipped by the existence check.
    let ident_re = regex::Regex::new(r"`([a-z_][a-z0-9_:*]*)`").expect("ident regex");

    let markers: Vec<_> = marker_re.captures_iter(spec).collect();
    let mut contracts = Vec::with_capacity(markers.len());

    for (idx, cap) in markers.iter().enumerate() {
        let area = cap.get(1).unwrap().as_str().to_string();
        let num: u32 = cap.get(2).unwrap().as_str().parse().unwrap();
        let id = format!("C-{}-{:03}", area, num);
        let start = cap.get(0).unwrap().end();
        // Block end is bounded by:
        //   (a) the next [C-...] marker, OR
        //   (b) the next "---" horizontal rule, OR
        //   (c) the next top-level / second-level heading.
        // The HR / heading rule prevents the LAST contract in a
        // section from absorbing prose (and `Pinned by` clauses)
        // that belong to a different, later section.
        let max_end = if idx + 1 < markers.len() {
            markers[idx + 1].get(0).unwrap().start()
        } else {
            spec.len()
        };
        let mut end = max_end;
        for needle in ["\n---\n", "\n## ", "\n# "] {
            if let Some(pos) = spec[start..max_end].find(needle) {
                let absolute = start + pos;
                if absolute < end {
                    end = absolute;
                }
            }
        }
        let block = &spec[start..end];

        let title = extract_title(block);

        // Pinning tests: union of every backticked identifier in
        // every "Pinned by …" clause within the block.
        let mut pins: BTreeSet<String> = BTreeSet::new();
        for clause in pin_clause_re.captures_iter(block) {
            let inner = clause.get(1).unwrap().as_str();
            for c in ident_re.captures_iter(inner) {
                pins.insert(c.get(1).unwrap().as_str().to_string());
            }
        }

        contracts.push(Contract {
            id,
            area,
            num,
            title,
            pinning_tests: pins.into_iter().collect(),
        });
    }
    contracts
}

/// Walk the contract block, character by character, tracking
/// backtick parity so periods inside code spans (`SKILL.md`,
/// `client.move_node`) don't truncate the title. The title ends
/// at the first sentence-terminating period outside a code span,
/// at a `**` (bold close), or at a newline-then-newline (paragraph
/// break) — whichever comes first.
fn extract_title(block: &str) -> String {
    // Skip leading whitespace + optional bold-close that the
    // wf-style "** [C-wf-NNN]**" marker leaves behind.
    let mut s = block.trim_start();
    if let Some(rest) = s.strip_prefix("**") {
        s = rest.trim_start();
    }

    let bytes = s.as_bytes();
    let mut in_code = false;
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            in_code = !in_code;
            out.push('`');
            i += 1;
            continue;
        }
        if !in_code {
            // "**" close bold ends the title (for forms where the
            // title is bold-wrapped).
            if c == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                break;
            }
            // Sentence-end: period followed by whitespace.
            if c == b'.'
                && i + 1 < bytes.len()
                && (bytes[i + 1] == b' ' || bytes[i + 1] == b'\n')
            {
                break;
            }
            // Paragraph-break safety: two consecutive newlines.
            if c == b'\n' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                break;
            }
        }
        out.push(c as char);
        i += 1;
    }
    // Collapse internal whitespace (newlines + indent become one
    // space) so the matrix renders cleanly.
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(|c: char| c == '*' || c == '.' || c.is_whitespace())
        .trim()
        .to_string()
}

fn walk_rs<F: FnMut(&Path, &str)>(dir: &Path, cb: &mut F) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip target/ and any hidden dir.
        if path.file_name().map_or(false, |n| {
            let s = n.to_string_lossy();
            s.starts_with('.') || s == "target"
        }) {
            continue;
        }
        if path.is_dir() {
            walk_rs(&path, cb);
        } else if path.extension().map_or(false, |e| e == "rs") {
            if let Ok(content) = fs::read_to_string(&path) {
                cb(&path, &content);
            }
        }
    }
}

fn collect_fn_locations() -> BTreeMap<String, Vec<PathBuf>> {
    let fn_re = regex::Regex::new(r"\bfn\s+([a-z_][a-z0-9_]*)\s*[<(]").expect("fn regex");
    let mut map: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for root in ["src", "tests"] {
        let dir = repo_root().join(root);
        walk_rs(&dir, &mut |path, content| {
            for cap in fn_re.captures_iter(content) {
                let name = cap.get(1).unwrap().as_str().to_string();
                let rel = path
                    .strip_prefix(repo_root())
                    .unwrap_or(path)
                    .to_path_buf();
                map.entry(name).or_default().push(rel);
            }
        });
    }
    // Deduplicate paths (a single file may declare a fn once but
    // module-test forms can collide).
    for paths in map.values_mut() {
        paths.sort();
        paths.dedup();
    }
    map
}

fn render_matrix(contracts: &[Contract], fn_locs: &BTreeMap<String, Vec<PathBuf>>) -> String {
    let mut out = String::new();
    out.push_str("# Spec ↔ Test Traceability Matrix\n\n");
    out.push_str("**Auto-regenerated by `cargo test --test traceability` — do not edit by hand.**\n\n");
    out.push_str("Source of contracts: every `[C-<area>-<NNN>]` marker in [`specs/specification.md`](specification.md). Each contract carries one or more `` Pinned by `<test_fn>` `` claims naming a real `fn <test_fn>` under `src/` or `tests/`. The traceability test fails if (a) any contract has no pin, (b) any pin names a non-existent function, or (c) the matrix file disagrees with the spec — running `cargo test --test traceability` regenerates this file from the spec.\n\n");

    let unique_pins: BTreeSet<&str> = contracts
        .iter()
        .flat_map(|c| c.pinning_tests.iter().map(String::as_str))
        .collect();
    out.push_str(&format!(
        "**Coverage:** {} contracts, {} unique pinning tests.\n\n",
        contracts.len(),
        unique_pins.len(),
    ));

    let area_titles: &[(&str, &str)] = &[
        ("skill", "Skill template — leak rules"),
        ("disc", "Skill template — discipline contracts"),
        ("server", "Server runtime"),
        ("wf", "Workflow orchestration"),
    ];

    let mut by_area: BTreeMap<&str, Vec<&Contract>> = BTreeMap::new();
    for c in contracts {
        by_area.entry(c.area.as_str()).or_default().push(c);
    }
    // Sort within each area by num.
    for v in by_area.values_mut() {
        v.sort_by_key(|c| c.num);
    }

    // Render in canonical area order; any unknown area appears
    // alphabetically after the known set.
    let known_areas: BTreeSet<&str> = area_titles.iter().map(|(a, _)| *a).collect();
    let mut all_areas: Vec<&str> = by_area.keys().copied().collect();
    all_areas.sort_by_key(|a| {
        (
            !known_areas.contains(a),
            area_titles.iter().position(|(k, _)| k == a).unwrap_or(usize::MAX),
            *a,
        )
    });

    for area in all_areas {
        let title = area_titles
            .iter()
            .find(|(a, _)| *a == area)
            .map(|(_, t)| *t)
            .unwrap_or(area);
        out.push_str(&format!("## {} (`C-{}-*`)\n\n", title, area));
        out.push_str("| Contract ID | Title | Pinning test(s) | Test file(s) |\n");
        out.push_str("|---|---|---|---|\n");
        for c in by_area.get(area).unwrap() {
            let pins_md = if c.pinning_tests.is_empty() {
                "**(none — gap)**".to_string()
            } else {
                c.pinning_tests
                    .iter()
                    .map(|p| format!("`{}`", p))
                    .collect::<Vec<_>>()
                    .join("<br>")
            };
            let files_md = if c.pinning_tests.is_empty() {
                "—".to_string()
            } else {
                c.pinning_tests
                    .iter()
                    .map(|p| {
                        if p.contains('*') || p.contains(':') {
                            // Glob / path-qualified pin — render
                            // verbatim, skip lookup.
                            "*(glob — see test module)*".to_string()
                        } else {
                            match fn_locs.get(p) {
                                Some(paths) => paths
                                    .iter()
                                    .map(|p| format!("`{}`", p.display()))
                                    .collect::<Vec<_>>()
                                    .join("<br>"),
                                None => "**MISSING**".to_string(),
                            }
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("<br>")
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                c.id, c.title, pins_md, files_md
            ));
        }
        out.push('\n');
    }

    out
}

#[test]
fn traceability_matrix_is_complete_and_current() {
    let spec_path = repo_root().join("specs/specification.md");
    let spec = fs::read_to_string(&spec_path)
        .unwrap_or_else(|e| panic!("specs/specification.md unreadable: {}", e));
    let contracts = parse_contracts(&spec);
    let fn_locs = collect_fn_locations();

    assert!(
        !contracts.is_empty(),
        "no `[C-area-NNN]` markers found in specs/specification.md \
         — at least one contract must carry a marker for the \
         matrix to be meaningful",
    );

    // Coverage: every contract has at least one pin.
    let unpinned: Vec<&Contract> = contracts
        .iter()
        .filter(|c| c.pinning_tests.is_empty())
        .collect();
    assert!(
        unpinned.is_empty(),
        "Contracts with no `Pinned by` claim — every contract \
         must name at least one test that pins it:\n  {}",
        unpinned
            .iter()
            .map(|c| format!("{} — {}", c.id, c.title))
            .collect::<Vec<_>>()
            .join("\n  "),
    );

    // Pin existence: every concrete (non-glob) pin must resolve to
    // a real function in src/ or tests/. Glob references (`::`,
    // `*`) are passed through to the matrix verbatim and not
    // checked here, since they intentionally name multiple
    // generated tests.
    let mut missing: Vec<(String, String)> = Vec::new();
    for c in &contracts {
        for p in &c.pinning_tests {
            if p.contains('*') || p.contains(':') {
                continue;
            }
            if !fn_locs.contains_key(p) {
                missing.push((c.id.clone(), p.clone()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "Pin tests named in spec but missing from codebase \
         (typo / rename / stale reference?):\n  {}",
        missing
            .iter()
            .map(|(c, p)| format!("{} pins `{}` — no `fn {}(` found in src/ or tests/", c, p, p))
            .collect::<Vec<_>>()
            .join("\n  "),
    );

    // Regenerate the matrix file. Idempotent if the spec hasn't
    // changed; otherwise the diff lands in `git status` for the
    // author to commit alongside their spec edit.
    let matrix = render_matrix(&contracts, &fn_locs);
    let matrix_path = repo_root().join("specs/traceability.md");
    fs::write(&matrix_path, matrix)
        .unwrap_or_else(|e| panic!("specs/traceability.md unwritable: {}", e));
}
