//! Repository-template portability tests.
//!
//! The repo ships templates that other users clone, populate, and run.
//! These tests guard against three classes of leak that have happened in
//! the past: machine-specific paths sneaking back into the templates,
//! the original author's intellectual frameworks bleeding into the
//! generic skill, and template files containing real per-user data
//! instead of placeholder shapes.
//!
//! Companion to the in-handler envelope pins
//! (`null_required_uuid_field_error_names_the_field`,
//! `handler_body_validation_uses_structured_envelope_not_bare_invalid_params`,
//! `operational_failures_route_through_tool_error_not_bare_internal_error`)
//! in `src/server/mod.rs`. Those pin server-side behaviour; this file
//! pins the user-facing template surface.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_template(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("template not readable at {}: {}", path.display(), e))
}

/// The shipped skill template at `templates/skills/wflow/SKILL.md` must
/// not contain any machine-specific path that would silently route a
/// new user's data into the original author's filesystem layout. The
/// 2026-05-03 incident: a hardcoded `/Users/dromologue/code` reference
/// in the user's local skill copy made an autonomous session scaffold
/// `~/code/SecondBrain/` instead of reading `$SECONDBRAIN_DIR` — the
/// template was already clean, but a regression there would re-ship
/// the leak to every new user.
#[test]
fn template_skill_has_no_machine_specific_user_paths() {
    let src = read_template("templates/skills/wflow/SKILL.md");
    let mut violations: Vec<String> = Vec::new();

    // Patterns that are user-data leaks — `~/code/SecondBrain` was the
    // specific shape of the May-2026 incident; the broader rule is "no
    // hardcoded user-home subdirectory used as if it were a canonical
    // SecondBrain path."
    let banned = [
        "~/code/SecondBrain",
        "~/code/secondBrain",
        "/Users/",
        "/home/", // Linux equivalent
        "C:\\Users\\",
    ];

    for (i, line) in src.lines().enumerate() {
        // Skip lines that explicitly *mention* a banned pattern as
        // an anti-pattern callout — those are the rule-statement, not
        // the rule violation. Recognised by "never assume" / "do not
        // hardcode" framing. Be conservative: if the line contains
        // both a banned pattern and an anti-pattern marker, allow.
        let trimmed = line.trim_start();
        let is_callout = trimmed.contains("never assume")
            || trimmed.contains("don't hardcode")
            || trimmed.contains("do not hardcode")
            || trimmed.contains("anti-pattern")
            || trimmed.contains("not in the repo");
        if is_callout {
            continue;
        }
        for pat in &banned {
            if line.contains(pat) {
                violations.push(format!("line {}: matched `{}`: {}", i + 1, pat, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "templates/skills/wflow/SKILL.md must not embed machine-specific \
         user paths — every user-data location uses `$SECONDBRAIN_DIR` so \
         a new user's clone routes to wherever they wired the env var. \
         Violations:\n  {}",
        violations.join("\n  "),
    );
}

/// The shipped skill template must reference the env-driven path
/// contract — at least one occurrence of `$SECONDBRAIN_DIR`. A skill
/// that talks about user data without naming the env var means data
/// reads have an implicit (probably author-specific) source, which is
/// exactly the leak we're guarding against.
#[test]
fn template_skill_references_env_driven_secondbrain_path() {
    let src = read_template("templates/skills/wflow/SKILL.md");
    assert!(
        src.contains("$SECONDBRAIN_DIR"),
        "templates/skills/wflow/SKILL.md must reference `$SECONDBRAIN_DIR` \
         at least once so user-data locations are explicitly env-driven."
    );
}

/// The skill template must not embed the original author's intellectual
/// frameworks (IOTA, ELSA, How we Lead/Decide/Learn/Build, Drift into
/// Failure, Crucible). Those names are the user-specific data file's
/// concern (`distillation_taxonomy.md`); naming them in the skill makes
/// the skill discoverable only by users who already share the author's
/// vocabulary.
#[test]
fn template_skill_does_not_embed_author_specific_frameworks() {
    let src = read_template("templates/skills/wflow/SKILL.md");
    let banned_frameworks = [
        "IOTA",
        "ELSA",
        "Crucible",
        "Drift into Failure",
        "How we Lead",
        "How we Decide",
        "How we Learn",
        "How we Build",
        // These two are the author's named theme set — same rule.
        "Organisational Prompts",
    ];
    let mut violations: Vec<String> = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        // The frontmatter `description:` field may *describe* that
        // user-specific frameworks live in `distillation_taxonomy.md`
        // — that's the rule statement, not a leak.
        if trimmed.starts_with("description:") {
            continue;
        }
        for f in &banned_frameworks {
            if line.contains(f) {
                violations.push(format!("line {}: framework `{}` named in skill: {}", i + 1, f, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "templates/skills/wflow/SKILL.md must stay neutral about the \
         user's intellectual frameworks. Pillar / theme / framework \
         names live in the user's `distillation_taxonomy.md`, not in \
         the skill body. Violations:\n  {}",
        violations.join("\n  "),
    );
}

/// The shipped `workflowy_node_links.md` template must look like a
/// template, not a real user's cache. Real UUIDs (32-hex-with-hyphens)
/// in a template would silently anchor a new user to the author's
/// Workflowy tree. Placeholders (`<UUID>`, `<paste your UUID here>`,
/// blank cells) are required.
#[test]
fn template_workflowy_node_links_is_placeholder_shaped() {
    let src = read_template("templates/secondbrain/memory/workflowy_node_links.md");
    // The frontmatter `canonical_path` line uses `$SECONDBRAIN_DIR/...`
    // — that's a literal env-var reference, not a hardcoded path.
    // Confirm the file contains no real UUIDs. Real UUIDs match the
    // pattern xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx.
    let real_uuid_re = regex::Regex::new(
        r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
    )
    .expect("static regex");
    let hits: Vec<&str> = real_uuid_re
        .find_iter(&src)
        .map(|m| m.as_str())
        .collect();
    assert!(
        hits.is_empty(),
        "templates/secondbrain/memory/workflowy_node_links.md contains \
         what look like real Workflowy UUIDs. Templates must use \
         placeholders (`<UUID>`, blank cells, or descriptive shapes) \
         so a new user's clone doesn't inherit the author's tree. \
         Hits: {:?}",
        hits,
    );
}

/// Same shape rule for the distillation taxonomy template: no real
/// UUIDs, and at minimum one placeholder marker (`<Pillar`, `<UUID>`,
/// or `<…>`) so the structure is obviously a fill-in-the-blank.
#[test]
fn template_distillation_taxonomy_is_placeholder_shaped() {
    let src = read_template("templates/secondbrain/memory/distillation_taxonomy.md");
    let real_uuid_re = regex::Regex::new(
        r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
    )
    .expect("static regex");
    let hits: Vec<&str> = real_uuid_re
        .find_iter(&src)
        .map(|m| m.as_str())
        .collect();
    assert!(
        hits.is_empty(),
        "templates/secondbrain/memory/distillation_taxonomy.md contains \
         real-looking UUIDs. Use placeholders. Hits: {:?}",
        hits,
    );
    assert!(
        src.contains("<Pillar") || src.contains("<UUID>") || src.contains("<…>"),
        "templates/secondbrain/memory/distillation_taxonomy.md must \
         contain at least one placeholder marker (`<Pillar 1>`, \
         `<UUID>`, or `<…>`) so the file is obviously a fill-in-the-\
         blank shape, not a populated cache."
    );
}

/// The skill template must not embed specific external-service names
/// (reMarkable, Notion, Obsidian, Linear, Readwise, etc.). Each user
/// configures their own additional services in
/// `$SECONDBRAIN_DIR/memory/services.md`; the skill probes whichever
/// services that file declares. Naming a specific service in the
/// skill body forces every user to either use that service or read
/// past instructions that don't apply to them. The 2026-05-03 leak:
/// reMarkable was a first-class integration in the template (10
/// inline references); it now lives in `services.md` like any other
/// optional surface.
#[test]
fn template_skill_does_not_embed_specific_external_services() {
    let src = read_template("templates/skills/wflow/SKILL.md");
    // Common external-service names that have been or might be a
    // user's optional surface. Workflowy is required by the skill
    // and stays. Filesystem is the file-system MCP and stays.
    // `remarkable-mcp` would be a hyphenated server-name reference
    // — also banned at top level since the skill should not assume
    // the service exists.
    let banned_services = [
        "reMarkable",
        "remarkable",
        "Remarkable",
        "Notion",
        "Obsidian",
        "Bear",
        "Roam",
        "Logseq",
        "Linear",
        "Jira",
        "Readwise",
        "Pocket",
        "Instapaper",
    ];
    let mut violations: Vec<String> = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        // Skip doc-comment lines / examples that explicitly *describe
        // the pattern* of declaring services in `services.md` —
        // recognised by mention of "services.md" or "additional
        // service" in the same line.
        if trimmed.contains("services.md") || trimmed.contains("additional service") {
            continue;
        }
        for svc in &banned_services {
            // Word-boundary match — don't flag substrings inside other
            // words (e.g. "Notion" is fine but the substring "notion"
            // appears inside many other words; case-sensitive matching
            // on the proper-noun forms catches the leaks we care
            // about).
            if line.contains(svc) {
                violations.push(format!(
                    "line {}: service name `{}` named in skill body: {}",
                    i + 1,
                    svc,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "templates/skills/wflow/SKILL.md must stay neutral about \
         external services. Specific services (reMarkable, Notion, \
         Linear, etc.) are user-specific and live in \
         `$SECONDBRAIN_DIR/memory/services.md`. The skill's job is to \
         read that file and probe whatever's declared. Violations:\n  {}",
        violations.join("\n  "),
    );
}

/// The skill `description:` frontmatter field has a 1024-character
/// hard limit (Claude skill upload rejects over-length entries).
/// Pin this on the template to prevent regressing — the May-2026
/// incident pushed the user's canonical to 1202 chars, breaking the
/// upload path. Same rule applies to the template ahead of any
/// future expansion.
#[test]
fn template_skill_description_under_1024_chars() {
    let src = read_template("templates/skills/wflow/SKILL.md");
    let mut in_frontmatter = false;
    let mut description_line: Option<String> = None;
    for line in src.lines() {
        if line.trim() == "---" {
            in_frontmatter = !in_frontmatter;
            continue;
        }
        if in_frontmatter && line.trim_start().starts_with("description:") {
            description_line = Some(line.to_string());
            break;
        }
    }
    let description = description_line
        .expect("templates/skills/wflow/SKILL.md must have a `description:` frontmatter field");
    assert!(
        description.len() <= 1024,
        "templates/skills/wflow/SKILL.md description is {} chars, \
         must be ≤ 1024 (Claude skill frontmatter hard limit). Trim \
         and move detail into the body. Current line: `{}...`",
        description.len(),
        &description.chars().take(120).collect::<String>(),
    );
}

/// Both memory-file templates must declare their canonical_path as
/// `$SECONDBRAIN_DIR/memory/<file>.md` — never a hardcoded user path.
/// This was the May-2026 leak in the *user's bundled copies* (which
/// the repo template was already correct on, but a regression here
/// would propagate to new users).
#[test]
fn template_memory_files_declare_env_driven_canonical_path() {
    for rel in [
        "templates/secondbrain/memory/workflowy_node_links.md",
        "templates/secondbrain/memory/distillation_taxonomy.md",
        "templates/secondbrain/memory/services.md",
    ] {
        let src = read_template(rel);
        // Find the frontmatter `canonical_path:` line.
        let canonical_line = src
            .lines()
            .find(|l| l.trim_start().starts_with("canonical_path:"))
            .unwrap_or_else(|| panic!("{}: missing `canonical_path:` frontmatter", rel));
        assert!(
            canonical_line.contains("$SECONDBRAIN_DIR"),
            "{}: canonical_path must reference `$SECONDBRAIN_DIR` not a \
             hardcoded user path. Line: `{}`",
            rel,
            canonical_line.trim(),
        );
        for banned in ["~/code/", "/Users/", "/home/"] {
            assert!(
                !canonical_line.contains(banned),
                "{}: canonical_path leaks `{}` — replace with \
                 `$SECONDBRAIN_DIR/...`. Line: `{}`",
                rel,
                banned,
                canonical_line.trim(),
            );
        }
    }
}
