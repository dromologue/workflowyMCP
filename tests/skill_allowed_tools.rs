//! Pin test: the public wflow skill template's `allowed-tools` frontmatter
//! must list every non-diagnostic MCP tool.
//!
//! WHY (todo 2026-05-26): the 2026-05-26 fix extended the skill's
//! `allowed-tools` to the full non-diagnostic surface, but nothing pinned it —
//! `cli_covers_every_non_diagnostic_mcp_tool` only guards CLI drift. Without a
//! pin test, a new MCP tool ships and the skill silently can't call it (the
//! claude.ai upload enforces `allowed-tools`). This test parses the in-repo
//! template frontmatter and asserts coverage against the single-source
//! `defaults::NON_DIAGNOSTIC_MCP_TOOLS` catalogue.
//!
//! Scope note (template-vs-personal split): this guards only the **public**
//! template at `templates/skills/wflow/SKILL.md` — the only skill artefact the
//! repo ships. The personal skill lives outside the repo
//! (`$SECONDBRAIN_DIR/skills/wflow/SKILL.md`) and is the user's to keep in sync.

use workflowy_mcp_server::defaults::NON_DIAGNOSTIC_MCP_TOOLS;

/// Extract the `allowed-tools:` value from the YAML frontmatter (the block
/// between the first two `---` fences). Frontmatter keys can't span lines here,
/// so a single-line scan is sufficient and avoids a YAML dependency.
fn allowed_tools_line(skill_md: &str) -> String {
    let mut in_frontmatter = false;
    for line in skill_md.lines() {
        if line.trim() == "---" {
            if in_frontmatter {
                break; // closing fence
            }
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter {
            if let Some(rest) = line.strip_prefix("allowed-tools:") {
                return rest.to_string();
            }
        }
    }
    panic!("no `allowed-tools:` key found in SKILL.md frontmatter");
}

#[test]
fn skill_allowed_tools_covers_every_non_diagnostic_mcp_tool() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/templates/skills/wflow/SKILL.md"
    );
    let skill_md = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let allowed = allowed_tools_line(&skill_md);

    let missing: Vec<&str> = NON_DIAGNOSTIC_MCP_TOOLS
        .iter()
        .copied()
        .filter(|tool| {
            let entry = format!("mcp__workflowy__{tool}");
            // Match on a whole-token boundary so `find_node` can't be satisfied
            // by `find_node_by_x` were such a tool ever added.
            !allowed
                .split([',', ' '])
                .map(str::trim)
                .any(|t| t == entry)
        })
        .collect();

    assert!(
        missing.is_empty(),
        "templates/skills/wflow/SKILL.md `allowed-tools` is missing non-diagnostic \
         MCP tools: {missing:?}. Add `mcp__workflowy__<tool>` for each to the \
         frontmatter (and to the personal skill copy).",
    );
}
