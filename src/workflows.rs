//! Workflow orchestration shared between the MCP `server` and the
//! `wflow-do` CLI binary.
//!
//! ## Why this module exists
//!
//! Both binaries used to contain duplicate orchestration logic for
//! tools like `create_mirror`, `insert_content`, `transaction`,
//! `bulk_update`, and `smart_insert`: the MCP handler in
//! `server/mod.rs` wrapped the steps in `tool_handler!` + structured
//! error envelopes, while the `wflow-do` subcommand in
//! `bin/wflow_do.rs` repeated the same step sequence with `?`
//! propagation and `println!` output. Adding a new tool meant editing
//! two places, and the failure-report 2026-05-04 follow-up flagged
//! that as a real maintenance hazard ("why do we have two code bases
//! for the CLI and the Server").
//!
//! This module is the third layer that fixes the duplication: a
//! workflow function takes a `&WorkflowyClient` plus typed inputs,
//! orchestrates the steps, and returns a typed result. Both surfaces
//! call it; the MCP handler wraps the result in `tool_error` envelopes,
//! the CLI wraps it in stdout output. The orchestration itself lives
//! once.
//!
//! Existing precedent for the pattern: `audit::audit_mirrors`,
//! `audit::build_review`, `utils::aggregation::*` — all pure functions
//! shared between the two surfaces. This module is the same idea
//! extended to functions that need an API client.
//!
//! ## What lives here vs. elsewhere
//!
//! - **`audit.rs`**: pure analyses (no I/O, no client). Stays.
//! - **`utils/aggregation.rs`**: pure aggregation over node slices. Stays.
//! - **`workflows.rs` (this file)**: orchestration that takes a client.
//!   Add new entries here when extracting duplicated CLI/server logic.
//!
//! The cache, name-index, and op-log live on the MCP side because the
//! CLI doesn't carry that infrastructure. Workflows surface what they
//! mutated through [`MutationFootprint`] so the MCP wrapper can apply
//! invalidations declaratively; the CLI discards the footprint.
//!
//! ## Workflow contract
//!
//! This module hosts two shapes of shared logic, both taking a
//! `client: &WorkflowyClient` (and, when they do time-bounded work, a
//! `ctx: &WorkflowContext<'_>` carrying an optional cancel guard + deadline —
//! the MCP passes the active server context, the CLI passes
//! `WorkflowContext::default()`):
//!
//! 1. **Mutating orchestrations** (`create_mirror_via_convention`,
//!    `insert_content_via_indented`, `run_transaction`, `apply_bulk_op`,
//!    `reorder_nodes_via_priority`, `smart_insert_under_target`) return
//!    `Ok((TypedResult, MutationFootprint))` — the footprint declares which
//!    node IDs need cache + name-index invalidation; the MCP wrapper applies
//!    it, the CLI discards it.
//! 2. **Read-only / pure shared steps** (`create_mirror_dry_run`,
//!    `audit_mirrors_walk`, `resolve_link_via_walk_and_scan`,
//!    `extract_unresolved_mirror_targets`, the `build_resolve_link_*_payload`
//!    builders, `parse_indented_content`, `find_node_by_short_hash`,
//!    `destructive_echo_matches`, `scope_resolved_label`) mutate nothing, so
//!    they return `Result<T>` / `T` and carry no footprint. A `MutationFootprint`
//!    there would always be empty — returning one would be cargo-cult.
//!
//! Either way, failures are `Err(WorkflowyError)`, with `InvalidInput`
//! reserved for caller-supplied parameter problems (mapped to MCP
//! `tool_invalid_params` envelopes by the wrapper).

use std::time::Instant;

use serde::Serialize;
use serde_json::json;
use tracing::error;

use crate::api::{TruncationReason, WorkflowyClient};
use crate::audit::extract_marker;
use crate::defaults;
use crate::error::{Result, WorkflowyError};
use crate::types::WorkflowyNode;
use crate::utils::cancel::CancelGuard;
use crate::utils::truncation_envelope::with_truncation_envelope_and_hint;

/// Context passed into every workflow function: optional cancel guard
/// + optional wall-clock deadline.
///
/// The MCP server creates a guard from its `CancelRegistry` and a
/// deadline from the `ToolKind` budget, then passes both. The CLI
/// passes `WorkflowContext::default()` (None, None) because it's a
/// single-shot process with no cancel-all surface and no
/// kind-keyed budget. Workflows that don't need to honour mid-
/// orchestration cancel or deadline (e.g. `create_mirror`, which is
/// atomic enough that the wrapper's outer cancel suffices) accept the
/// parameter and ignore it; workflows that *do* need it (e.g.
/// `insert_content`, where the per-line resume cursor depends on
/// observing both signals between API calls) use the helpers
/// [`WorkflowContext::is_cancelled`] and
/// [`WorkflowContext::is_past_deadline`] to test each pre-call.
///
/// Borrows the cancel guard with a lifetime so a workflow cannot
/// outlive its cancel observer — the MCP's guard is taken inside
/// `tool_handler!` and lives for the duration of the call.
#[derive(Default)]
pub struct WorkflowContext<'a> {
    pub cancel: Option<&'a CancelGuard>,
    pub deadline: Option<Instant>,
}

impl<'a> WorkflowContext<'a> {
    /// Build a context from an explicit cancel + deadline pair. Both
    /// arguments are optional so the same constructor covers MCP
    /// (Some, Some) and CLI (None, None) call sites.
    pub fn new(cancel: Option<&'a CancelGuard>, deadline: Option<Instant>) -> Self {
        Self { cancel, deadline }
    }

    /// True when a `cancel_all` (or local cancel) flipped the guard
    /// since this context was created. Workflows poll this before each
    /// API call so a long bulk operation honours cancel within ~50 ms.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.map(|g| g.is_cancelled()).unwrap_or(false)
    }

    /// True when the wall-clock deadline (if any) has passed. Workflows
    /// poll this between iterations so a bulk loop bails out
    /// deterministically rather than racing the next HTTP call's own
    /// timeout.
    pub fn is_past_deadline(&self) -> bool {
        match self.deadline {
            Some(dl) => Instant::now() >= dl,
            None => false,
        }
    }
}

/// Side effects a workflow performed that the MCP wrapper cares
/// about. The MCP handler applies these declaratively after the
/// workflow returns; the CLI discards them (no cache, no name index).
///
/// Why declarative: pre-2026-05-04 every MCP handler hand-wrote
/// `invalidate_for_mutation(&[id, ...])` before its mutation, easy to
/// forget and easy to drift between handlers and the operations the
/// workflow actually does. Returning the footprint forces every
/// workflow to declare its side effects up-front, the wrapper applies
/// them uniformly, and a missed invalidation becomes a workflow bug
/// (testable) instead of a cache-poisoning bug (not).
#[derive(Debug, Default, Clone)]
pub struct MutationFootprint {
    /// Node IDs whose cache entry should be invalidated.
    pub invalidated_nodes: Vec<String>,
    /// Node IDs whose name-index entry should be invalidated. Usually
    /// the moved/renamed/deleted node — the index is name-keyed, so
    /// its parent's cache entry isn't affected.
    pub invalidated_name_index: Vec<String>,
}

impl MutationFootprint {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a node ID as needing both cache + name-index invalidation.
    /// The common case for any single-node mutation.
    pub fn invalidate_node(&mut self, id: impl Into<String>) {
        let id = id.into();
        self.invalidated_nodes.push(id.clone());
        self.invalidated_name_index.push(id);
    }

    /// Mark a node ID as needing cache invalidation only — typical
    /// when the workflow touched a node's *children* (e.g. created
    /// under a parent), not the node itself's name.
    pub fn invalidate_cache_only(&mut self, id: impl Into<String>) {
        self.invalidated_nodes.push(id.into());
    }

    /// Merge another footprint's IDs into self. Used when a workflow
    /// composes sub-workflows.
    pub fn extend(&mut self, other: MutationFootprint) {
        self.invalidated_nodes.extend(other.invalidated_nodes);
        self.invalidated_name_index
            .extend(other.invalidated_name_index);
    }
}

// ---------------------------------------------------------------------
// scope_resolved (shared between MCP responses and CLI dry-run output)
// ---------------------------------------------------------------------

/// Render a resolved parent-scope as the canonical `scope_resolved`
/// string the MCP tool family emits and the CLI's `--dry-run` paths
/// surface. The format is stable and case-sensitive: `"workspace_root"`
/// when the resolved scope is `None` (caller passed null or omitted),
/// otherwise `"scoped:<full-uuid>"`. Callers may match on the literal
/// prefix.
///
/// Lifted into `workflows.rs` on 2026-05-09 so the MCP server and the
/// `wflow-do` CLI use the same renderer — the failure-report
/// 2026-05-09 fix added `scope_resolved` to the response shape, and a
/// duplication audit caught the MCP and CLI rendering it
/// independently. Single source of truth means the scope label cannot
/// drift between the two surfaces.
///
/// Pinned by [C-server-006] / [C-server-007].
pub fn scope_resolved_label(resolved: Option<&str>) -> String {
    match resolved {
        None => "workspace_root".to_string(),
        Some(uuid) => format!("scoped:{}", uuid),
    }
}

// ---------------------------------------------------------------------
// path_of (parent-chain walk, shared between MCP and CLI)
// ---------------------------------------------------------------------

/// One step in a root→node path: the node's id and name.
#[derive(Debug, Clone, Serialize)]
pub struct PathSegment {
    pub id: String,
    pub name: String,
}

/// Result of walking a node's parent chain to the root.
///
/// `segments` are ordered root-first (index 0 is the topmost ancestor
/// reached, the last entry is the requested node). `truncated` is true
/// when the walk stopped because it hit `max_depth` rather than the
/// natural root (a `None`/empty parent).
#[derive(Debug, Clone)]
pub struct ParentChain {
    pub segments: Vec<PathSegment>,
    pub truncated: bool,
}

/// Walk the parent-id chain from `node_id` up to the workspace root and
/// return the ordered segments plus a truncation flag.
///
/// Single source of truth for the `path_of` parent walk, shared by the
/// MCP `path_of` handler and the `wflow-do path-of` subcommand. The MCP
/// handler keeps its own JSON shaping (display string, depth, truncated)
/// around this walk; the CLI prints its own shape.
///
/// The walk stops at the first `None`/empty parent (the natural root),
/// the first cycle (a node id already seen — guards against a malformed
/// parent loop), the first `get_node` error (returns the partial path),
/// or `max_depth` (sets `truncated`). Pre-2026-06-16 the CLI walked the
/// chain with NO cycle guard, so a malformed parent loop spun until the
/// depth cap; lifting both surfaces here gives the CLI the same guard
/// the MCP handler already had. Uses `get_node_with_propagation_retry`
/// so a freshly-moved node's stale parent listing doesn't abort the walk
/// — the more robust of the two prior variants, now shared.
pub async fn walk_parent_chain(
    client: &WorkflowyClient,
    node_id: &str,
    max_depth: usize,
    ctx: &WorkflowContext<'_>,
) -> Result<ParentChain> {
    let mut segments: Vec<PathSegment> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut current_id: Option<String> = Some(node_id.to_string());
    let mut depth = 0usize;
    let mut hit_depth_cap = false;

    while let Some(id) = current_id.take() {
        if ctx.is_cancelled() {
            return Err(WorkflowyError::Cancelled);
        }
        if ctx.is_past_deadline() {
            return Err(WorkflowyError::Timeout);
        }
        if !seen.insert(id.clone()) {
            tracing::warn!(node_id = %id, "walk_parent_chain: cycle detected, stopping walk");
            break;
        }
        if depth >= max_depth {
            tracing::warn!(max_depth, "walk_parent_chain: max_depth reached, returning partial path");
            hit_depth_cap = true;
            break;
        }
        depth += 1;
        match client.get_node_with_propagation_retry(&id).await {
            Ok(node) => {
                segments.push(PathSegment { id: node.id, name: node.name });
                current_id = match node.parent_id {
                    Some(pid) if !pid.is_empty() => Some(pid),
                    _ => None,
                };
            }
            Err(e) => {
                tracing::warn!(node_id = %id, error = %e, "walk_parent_chain: get_node failed; returning partial path");
                break;
            }
        }
    }
    // Reverse so index 0 is root, last is the requested node.
    segments.reverse();
    Ok(ParentChain { segments, truncated: hit_depth_cap })
}

// ---------------------------------------------------------------------
// create_mirror
// ---------------------------------------------------------------------

/// Outcome of a successful `create_mirror_via_convention` call. Both
/// surfaces (MCP handler + CLI) project this into their own response
/// shape: the MCP handler returns it as a JSON envelope, the CLI
/// prints either JSON or a one-line summary plus the new mirror's id.
#[derive(Debug, Clone, Serialize)]
pub struct CreateMirrorResult {
    pub mirror_id: String,
    pub canonical_id: String,
    pub target_parent_id: Option<String>,
    pub name: String,
    /// `"OK"` when the canonical carried (or now carries) a
    /// `canonical_of:` marker; `"ORPHAN_canonical_lacks_marker"`
    /// otherwise. Mirrors the four `audit_mirrors` finding kinds —
    /// callers can route on this without re-running the audit.
    pub audit_status: &'static str,
    pub annotated_canonical: bool,
}

/// Outcome of a `create_mirror_dry_run`. The "what would happen"
/// preview the failure-report 2026-05-09 asked for: resolves the
/// canonical (so the mirror name is accurate) plus the canonical's
/// existing `canonical_of:` marker (so the would-annotate decision is
/// authoritative), without writing. Both surfaces serialise this same
/// struct so the dry_run preview is identical wherever it's invoked.
#[derive(Debug, Clone, Serialize)]
pub struct CreateMirrorDryRun {
    pub canonical_id: String,
    pub target_parent_id: Option<String>,
    /// The mirror's name as a future production call would set it
    /// (verbatim copy of the canonical's name).
    pub mirror_name: String,
    pub canonical_already_marked: bool,
    pub would_annotate_canonical: bool,
    /// The pillar token after `pillar.trim().filter(non_empty)`. None
    /// means no annotation regardless of the canonical's marker state.
    pub pillar: Option<String>,
}

/// Create a convention-based mirror of `canonical_id` under
/// `target_parent_id` (or the workspace root when `None`).
///
/// Steps, in order — kept short and explicit so the MCP handler and
/// the CLI subcommand can both wrap this with their own error and
/// output envelopes without duplicating the logic:
///
/// 1. Refuse mirror-of-self up-front (cheap, no API call).
/// 2. Read the canonical so the mirror's name is a verbatim copy
///    (the audit's DRIFTED finding compares names).
/// 3. Create a new node under target_parent with the canonical's
///    name and a description carrying `mirror_of: <canonical_uuid>`.
/// 4. Optionally annotate the canonical with `canonical_of: <pillar>`
///    if the caller supplied a pillar token AND the canonical lacks
///    one. Existing markers are NEVER overwritten (the convention
///    treats pillar tokens as opaque and curated by the user).
///
/// Cancel/deadline are accepted via `ctx` for API uniformity but the
/// orchestration is short enough that the outer wrapper's cancel
/// already covers it; no inline checks needed.
/// Read-only preview of `create_mirror_via_convention`. Same
/// resolution work as the production call (mirror-of-self refusal,
/// canonical name lookup, canonical_of marker probe), zero mutation.
/// Both the MCP handler's `dry_run=true` path and the CLI's
/// `--dry-run` flag delegate here so the preview is identical
/// regardless of surface — the failure-report 2026-05-09 fix asked
/// for "would-be canonical_id and target_parent_id without writing"
/// and this is the single function that produces it.
/// Mirror-of-self refusal — shared by the dry-run and production
/// `create_mirror` workflows so the constraint and its message live once.
fn refuse_self_mirror(canonical_id: &str, target_parent_id: Option<&str>) -> Result<()> {
    if target_parent_id == Some(canonical_id) {
        return Err(WorkflowyError::InvalidInput {
            reason: "target_parent_id cannot equal canonical_node_id — \
                     a node cannot mirror itself into its own subtree"
                .to_string(),
        });
    }
    Ok(())
}

/// Trim a pillar argument and drop it when empty. Shared normalisation so
/// the dry-run and production paths agree on what "no pillar" means.
fn normalise_pillar(pillar: Option<&str>) -> Option<String> {
    pillar
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub async fn create_mirror_dry_run(
    client: &WorkflowyClient,
    canonical_id: &str,
    target_parent_id: Option<&str>,
    pillar: Option<&str>,
) -> Result<CreateMirrorDryRun> {
    refuse_self_mirror(canonical_id, target_parent_id)?;
    let canonical = client.get_node(canonical_id).await?;
    let canonical_desc = canonical.description.clone().unwrap_or_default();
    let canonical_already_marked =
        crate::audit::extract_marker(&canonical_desc, "canonical_of:").is_some();
    let pillar_norm = normalise_pillar(pillar);
    let would_annotate = pillar_norm.is_some() && !canonical_already_marked;
    Ok(CreateMirrorDryRun {
        canonical_id: canonical_id.to_string(),
        target_parent_id: target_parent_id.map(str::to_string),
        mirror_name: canonical.name,
        canonical_already_marked,
        would_annotate_canonical: would_annotate,
        pillar: pillar_norm,
    })
}

pub async fn create_mirror_via_convention(
    client: &WorkflowyClient,
    canonical_id: &str,
    target_parent_id: Option<&str>,
    priority: Option<i32>,
    pillar: Option<&str>,
    _ctx: &WorkflowContext<'_>,
) -> Result<(CreateMirrorResult, MutationFootprint)> {
    let mut footprint = MutationFootprint::new();

    // 1. Mirror-of-self refusal: cheap and produces a clearer error
    //    than letting the API surface the consequence downstream.
    refuse_self_mirror(canonical_id, target_parent_id)?;

    // 2. Read the canonical to copy its name verbatim.
    let canonical = client.get_node(canonical_id).await?;

    // 3. Create the mirror under target_parent. The `mirror_of:`
    //    marker uses the canonical's full UUID even when the caller
    //    passed a short hash, so audits run off a single canonical id
    //    regardless of how the mirror was created.
    let mirror_note = format!("mirror_of: {}", canonical_id);
    let created = client
        .create_node(
            &canonical.name,
            Some(&mirror_note),
            target_parent_id,
            priority,
        )
        .await?;
    footprint.invalidate_node(canonical_id);
    if let Some(pid) = target_parent_id {
        footprint.invalidate_cache_only(pid);
    }

    // 4. Optionally annotate the canonical with a `canonical_of:`
    //    marker. Best-effort: if the edit fails, the mirror itself
    //    already exists and the caller can decide whether to roll back.
    let canonical_desc = canonical.description.clone().unwrap_or_default();
    let canonical_already_marked =
        extract_marker(&canonical_desc, "canonical_of:").is_some();
    let mut annotated = false;
    if let Some(p) = normalise_pillar(pillar) {
        if !canonical_already_marked {
            let marker_line = format!("canonical_of: {}", p);
            let new_desc = if canonical_desc.is_empty() {
                marker_line
            } else {
                format!("{}\n{}", canonical_desc, marker_line)
            };
            if client
                .edit_node(canonical_id, None, Some(&new_desc))
                .await
                .is_ok()
            {
                annotated = true;
                footprint.invalidate_node(canonical_id);
            }
        }
    }

    let audit_status = if canonical_already_marked || annotated {
        "OK"
    } else {
        "ORPHAN_canonical_lacks_marker"
    };

    let result = CreateMirrorResult {
        mirror_id: created.id,
        canonical_id: canonical_id.to_string(),
        target_parent_id: target_parent_id.map(str::to_string),
        name: canonical.name,
        audit_status,
        annotated_canonical: annotated,
    };
    Ok((result, footprint))
}

// ---------------------------------------------------------------------
// insert_content
// ---------------------------------------------------------------------

/// Outcome of an `insert_content_via_indented` call. Two terminal
/// shapes:
///
/// - [`InsertContentOutcome::Complete`] — every line was inserted.
/// - [`InsertContentOutcome::Partial`] — the workflow stopped before
///   inserting every line; the outcome carries enough state for the
///   caller to resume from `last_inserted_id`. Three stop reasons (see
///   [`PartialReason`]): cancel, deadline, OR a hard mid-batch API
///   error. The error case (2026-06-17) used to `return Err(e)` and
///   discard the accumulated `created_count` / `last_inserted_id`,
///   leaving the caller unable to tell what landed without a separate
///   read — exactly the write-path report's Recommendation D gap. The
///   workflow now ALWAYS surfaces its progress; the reason discriminates
///   a clean resume (cancel/timeout) from a hard error (error), and the
///   `error` field carries the underlying failure string so the MCP
///   surface can classify its proximate cause.
///
/// The MCP handler used to compute these shapes inline; the CLI used
/// to skip the partial path entirely (no cancel surface). Lifting it
/// into a typed return means both surfaces emit the same partial-
/// success envelope when applicable, and the spec covers them both.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum InsertContentOutcome {
    Complete {
        parent_id: Option<String>,
        created_count: usize,
        last_inserted_id: Option<String>,
    },
    Partial {
        parent_id: Option<String>,
        reason: PartialReason,
        created_count: usize,
        total_count: usize,
        last_inserted_id: Option<String>,
        stopped_at_line: Option<String>,
        /// Underlying error string when `reason == Error`; `None` for the
        /// clean cancel/timeout stops. The MCP handler runs this through
        /// `classify_operational_error` so the failure envelope carries the
        /// same `proximate_cause` / `retry_after_secs` / `retryable` as a
        /// non-batch failure of the same kind.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Why an `insert_content` call returned partial success rather than
/// complete. `Cancelled` / `Timeout` map to the cancel/deadline signals
/// the workflow observes between API calls; `Error` is a hard mid-batch
/// API failure (e.g. a 429 on the 10th of 24 lines) that stopped the
/// batch with some lines already committed.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PartialReason {
    Cancelled,
    Timeout,
    Error,
}

impl PartialReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PartialReason::Cancelled => "cancelled",
            PartialReason::Timeout => "timeout",
            PartialReason::Error => "error",
        }
    }
}

/// One parsed line of indented content.
///
/// Public so MCP and CLI can share the parser too — the handler used
/// to inline its own `Vec<ParsedLine>` and the CLI inlined its own
/// `Vec<Parsed>`. Same shape, two definitions; merging here.
#[derive(Debug, Clone)]
pub struct ParsedLine {
    pub text: String,
    pub indent: usize,
}

/// Parse 2-space-indented content into `ParsedLine`s. Empty lines are
/// dropped silently; the indent is computed as `leading_whitespace / 2`.
/// Both surfaces call this so the parsing rules can't drift.
pub fn parse_indented_content(content: &str) -> Vec<ParsedLine> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let leading = line.len() - line.trim_start().len();
            Some(ParsedLine {
                text: trimmed.to_string(),
                indent: leading / 2,
            })
        })
        .collect()
}

/// Insert a parsed indented payload under `parent_id` (or workspace
/// root when `None`).
///
/// Contract:
/// - Empty payload returns `Complete { created_count: 0, ... }`.
/// - Payload exceeding [`defaults::MAX_INSERT_CONTENT_LINES`] returns
///   `WorkflowyError::InvalidInput` so the MCP wrapper produces an
///   `invalid_params` envelope and the CLI prints a clear message.
///   The cap is the same value both surfaces enforced before
///   2026-05-04 (`MAX_INSERT_CONTENT_LINES = 80`); centralising it
///   here means a future tuning lands in both surfaces.
/// - Per-line creates honour `ctx.cancel` and `ctx.deadline`. When
///   either fires between lines, the workflow returns
///   `Complete::Partial { reason, ... }` with the resume cursor.
/// - Per-line creates also pass `ctx.cancel` / `ctx.deadline` into
///   `create_node_cancellable`, so an in-flight HTTP send is racing
///   the same signals.
pub async fn insert_content_via_indented(
    client: &WorkflowyClient,
    parent_id: Option<&str>,
    parsed: Vec<ParsedLine>,
    ctx: &WorkflowContext<'_>,
) -> Result<(InsertContentOutcome, MutationFootprint)> {
    let mut footprint = MutationFootprint::new();
    if parsed.is_empty() {
        return Ok((
            InsertContentOutcome::Complete {
                parent_id: parent_id.map(str::to_string),
                created_count: 0,
                last_inserted_id: None,
            },
            footprint,
        ));
    }
    if parsed.len() > defaults::MAX_INSERT_CONTENT_LINES {
        let cap = defaults::MAX_INSERT_CONTENT_LINES;
        return Err(WorkflowyError::InvalidInput {
            reason: format!(
                "payload too large: {} lines exceeds the {}-line cap. Split into \
                 batches of ≤{} lines each and call insert_content once per batch; pass \
                 the previous batch's `last_inserted_id` as `parent_id` of the next call \
                 to keep the hierarchy stitched together. The cap was lowered from 200 to \
                 {} on 2026-05-04 because the failure-report 2026-05-03 observed \
                 ≥80-line payloads failing at the MCP transport layer before the handler \
                 ran — surfacing as undiagnosable 'Tool execution failed' with no per-tool \
                 counter movement. Below the cap is the empirically safe ceiling.",
                parsed.len(), cap, cap, cap,
            ),
        });
    }

    // Index 0 of the stack is the base parent (None = workspace root,
    // Some(uuid) = caller-specified). Children pushed onto the stack
    // are always Some(uuid) because every successful create returns
    // a real id.
    let mut parent_stack: Vec<Option<String>> = vec![parent_id.map(str::to_string)];
    let total = parsed.len();
    let mut created_count: usize = 0;
    let mut last_inserted_id: Option<String> = None;
    let mut bailout_reason: Option<PartialReason> = None;
    let mut bailout_line: Option<String> = None;
    // Set only when bailout_reason == Error; carries the underlying failure
    // so the MCP surface can classify its proximate cause.
    let mut bailout_error: Option<String> = None;

    for line in &parsed {
        // Pre-line cancel + deadline checks. A guard taken before the
        // rate limiter avoids burning a token on a doomed call.
        if ctx.is_cancelled() {
            bailout_reason = Some(PartialReason::Cancelled);
            break;
        }
        if ctx.is_past_deadline() {
            bailout_reason = Some(PartialReason::Timeout);
            bailout_line = Some(line.text.clone());
            break;
        }

        // Clamp indent to valid range.
        let indent = line.indent.min(parent_stack.len().saturating_sub(1));
        let line_parent: Option<String> = parent_stack[indent].clone();

        match client
            .create_node_cancellable(
                &line.text,
                None,
                line_parent.as_deref(),
                None,
                ctx.cancel,
                ctx.deadline,
            )
            .await
        {
            Ok(created) => {
                created_count += 1;
                last_inserted_id = Some(created.id.clone());
                let next_level = indent + 1;
                if next_level < parent_stack.len() {
                    parent_stack[next_level] = Some(created.id);
                    parent_stack.truncate(next_level + 1);
                } else {
                    parent_stack.push(Some(created.id));
                }
            }
            Err(WorkflowyError::Cancelled) => {
                bailout_reason = Some(PartialReason::Cancelled);
                bailout_line = Some(line.text.clone());
                break;
            }
            Err(WorkflowyError::Timeout) => {
                bailout_reason = Some(PartialReason::Timeout);
                bailout_line = Some(line.text.clone());
                break;
            }
            Err(e) => {
                // Deadline takes precedence: if the budget already
                // expired, the error is a downstream consequence (a
                // connection torn down by the racing select arm, an
                // upstream session closed) and the contract is
                // partial-success on timeout — not a hard error the
                // caller can't tell apart from a real failure.
                if ctx.is_cancelled() {
                    bailout_reason = Some(PartialReason::Cancelled);
                    bailout_line = Some(line.text.clone());
                    break;
                }
                if ctx.is_past_deadline() {
                    bailout_reason = Some(PartialReason::Timeout);
                    bailout_line = Some(line.text.clone());
                    break;
                }
                // A hard mid-batch failure (e.g. a 429 on the Nth line).
                // Pre-2026-06-17 this did `return Err(e)`, discarding the
                // `created_count` / `last_inserted_id` already accumulated —
                // the caller learnt nothing about what landed without a
                // separate read (write-path report, Recommendation D). Now we
                // stop the batch but surface the progress: capture the error
                // and break into the Partial outcome below. The cache is
                // invalidated once after the loop (the original early-return
                // invalidation is preserved by the post-loop block).
                error!(error = %e, line = %line.text, "Failed to insert line — stopping batch with partial progress");
                bailout_reason = Some(PartialReason::Error);
                bailout_line = Some(line.text.clone());
                bailout_error = Some(e.to_string());
                break;
            }
        }
    }

    if let Some(pid) = parent_id {
        footprint.invalidate_cache_only(pid);
    }

    let outcome = match bailout_reason {
        Some(reason) => InsertContentOutcome::Partial {
            parent_id: parent_id.map(str::to_string),
            reason,
            created_count,
            total_count: total,
            last_inserted_id,
            stopped_at_line: bailout_line,
            error: bailout_error,
        },
        None => InsertContentOutcome::Complete {
            parent_id: parent_id.map(str::to_string),
            created_count,
            last_inserted_id,
        },
    };
    Ok((outcome, footprint))
}

// ---------------------------------------------------------------------
// transaction
// ---------------------------------------------------------------------

/// Op kinds the transaction workflow recognises. Mirrors what the MCP
/// `transaction` tool's params and the `wflow-do transaction` JSON
/// input both accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnOpKind {
    Create,
    Edit,
    Delete,
    Move,
    Complete,
    Uncomplete,
}

impl TxnOpKind {
    /// Parse from the wire-string form. Returns `None` for unknown
    /// kinds; the wrapper translates this to its own error envelope.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "create" => Self::Create,
            "edit" => Self::Edit,
            "delete" => Self::Delete,
            "move" => Self::Move,
            "complete" => Self::Complete,
            "uncomplete" => Self::Uncomplete,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::Move => "move",
            Self::Complete => "complete",
            Self::Uncomplete => "uncomplete",
        }
    }
}

/// One operation in a transaction. The IDs are pre-resolved — short
/// hashes turned into full UUIDs by the wrapper before the workflow
/// runs. The MCP handler resolves through its name index; the CLI
/// passes raw strings (and accepts API 404s for unrecognised hashes).
///
/// `op` is the raw string from the wire (`"create"`, `"move"`, …) —
/// kept untyped so the workflow can treat an unknown op kind the same
/// way it treats any other per-op error: trigger rollback rather than
/// abort the wrapper. The pre-2026-05-04 server's `apply_txn_op` had
/// this behaviour; preserving it across the lift is what
/// `transaction_rejects_unknown_op_kind` pins.
#[derive(Debug, Clone)]
pub struct TxnOp {
    pub op: String,
    pub node_id: Option<String>,
    pub parent_id: Option<String>,
    pub new_parent_id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub priority: Option<i32>,
    /// Optional name-echo guard for `delete` ops. See
    /// [`destructive_echo_matches`] and the `delete_node` handler — when set,
    /// the delete step refuses unless the target's current name matches.
    pub expect_name: Option<String>,
}

/// Destructive-op name-echo comparison. Single source of truth shared by the
/// `delete_node` MCP handler and the `transaction` delete step so the two
/// surfaces cannot drift. Both sides are trimmed; the comparison is
/// case-sensitive because node names are user content — a case difference is a
/// real difference, not noise. The guard exists because a host can coerce a
/// `null`/placeholder `node_id` into a plausible-but-unintended UUID *before*
/// the server's deserializer sees it (host-coercion hazard, 2026-06-16); the
/// wire-level `NodeId` null-rejection cannot catch that, but a name mismatch
/// against the caller's echo can — and a delete is irreversible.
pub fn destructive_echo_matches(current_name: &str, echoed_name: &str) -> bool {
    current_name.trim() == echoed_name.trim()
}

/// Inverse of a successful transaction step. Applied in LIFO order
/// during rollback. Matches the previous `server::TxnInverse` enum
/// 1:1; `wflow-do` used to carry an isomorphic but separately-defined
/// JSON encoding which is now collapsed into this type.
#[derive(Debug, Clone)]
pub enum TxnInverse {
    DeleteCreated {
        node_id: String,
        parent_id: Option<String>,
    },
    RestoreEdit {
        node_id: String,
        prev_name: Option<String>,
        prev_description: Option<String>,
    },
    UnMove {
        node_id: String,
        prev_parent_id: Option<String>,
        prev_priority: Option<i32>,
    },
    RestoreCompletion {
        node_id: String,
        prev_completed: bool,
    },
}

/// Outcome of `run_transaction`. Both shapes return `Ok(_)` from the
/// workflow — `RolledBack` is a successful execution of the rollback
/// path, not an error. The wrapper renders either to its envelope.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TransactionOutcome {
    Applied {
        operations: Vec<serde_json::Value>,
    },
    RolledBack {
        failed_at_index: usize,
        error: String,
        applied_before_failure: Vec<serde_json::Value>,
        rollback: Vec<serde_json::Value>,
    },
}

/// Apply a sequence of operations with best-effort atomicity. On the
/// first per-op error, replay each prior op's inverse in LIFO order,
/// log the result of every rollback step, and return
/// `TransactionOutcome::RolledBack`. On full success, return
/// `TransactionOutcome::Applied`.
///
/// The workflow returns `Ok(_)` for both shapes because rollback is
/// part of the contract, not an error case. `WorkflowyError` is only
/// surfaced for inputs the workflow refuses up-front (empty ops list,
/// unknown op kind, missing required fields) — those map to the
/// MCP wrapper's `invalid_params` envelope.
///
/// Ops list cancel/deadline checks are not interleaved; the underlying
/// API calls already honour `ctx.cancel`/`ctx.deadline` via the
/// client's cancellable variants. Adding pre-iteration checks here is
/// future work for cooperative-bail behaviour.
pub async fn run_transaction(
    client: &WorkflowyClient,
    ops: Vec<TxnOp>,
    _ctx: &WorkflowContext<'_>,
) -> Result<(TransactionOutcome, MutationFootprint)> {
    if ops.is_empty() {
        return Err(WorkflowyError::InvalidInput {
            reason: "operations must not be empty".to_string(),
        });
    }

    let mut applied: Vec<TxnInverse> = Vec::new();
    let mut applied_results: Vec<serde_json::Value> = Vec::new();
    let mut footprint = MutationFootprint::new();

    for (idx, op) in ops.into_iter().enumerate() {
        match apply_txn_step(client, op).await {
            Ok((summary, inverse, fp)) => {
                applied_results
                    .push(json!({ "index": idx, "ok": true, "summary": summary }));
                footprint.extend(fp);
                if let Some(inv) = inverse {
                    applied.push(inv);
                }
            }
            Err(err) => {
                let mut rollback_log: Vec<serde_json::Value> = Vec::new();
                while let Some(inv) = applied.pop() {
                    match run_txn_inverse(client, inv).await {
                        Ok((summary, fp)) => {
                            footprint.extend(fp);
                            rollback_log
                                .push(json!({ "ok": true, "summary": summary }));
                        }
                        Err(re) => rollback_log
                            .push(json!({ "ok": false, "error": re.to_string() })),
                    }
                }
                return Ok((
                    TransactionOutcome::RolledBack {
                        failed_at_index: idx,
                        error: err.to_string(),
                        applied_before_failure: applied_results,
                        rollback: rollback_log,
                    },
                    footprint,
                ));
            }
        }
    }
    Ok((
        TransactionOutcome::Applied {
            operations: applied_results,
        },
        footprint,
    ))
}

/// Apply one transaction step. Returns the JSON summary the wrapper
/// will surface, an optional inverse for rollback (delete is
/// intentionally not invertible), and a [`MutationFootprint`].
async fn apply_txn_step(
    client: &WorkflowyClient,
    op: TxnOp,
) -> Result<(serde_json::Value, Option<TxnInverse>, MutationFootprint)> {
    let mut footprint = MutationFootprint::new();
    let kind = TxnOpKind::parse(&op.op).ok_or_else(|| WorkflowyError::InvalidInput {
        reason: format!(
            "unknown transaction op '{}'; expected create/edit/delete/move/complete/uncomplete",
            op.op
        ),
    })?;
    match kind {
        TxnOpKind::Create => {
            let name = op.name.as_deref().ok_or_else(|| WorkflowyError::InvalidInput {
                reason: "create requires `name`".to_string(),
            })?;
            let created = client
                .create_node(
                    name,
                    op.description.as_deref(),
                    op.parent_id.as_deref(),
                    op.priority,
                )
                .await?;
            if let Some(pid) = op.parent_id.as_deref() {
                footprint.invalidate_cache_only(pid);
            }
            let summary = json!({
                "op": "create",
                "id": created.id.clone(),
                "name": created.name.clone(),
            });
            let inverse = TxnInverse::DeleteCreated {
                node_id: created.id,
                parent_id: op.parent_id,
            };
            Ok((summary, Some(inverse), footprint))
        }
        TxnOpKind::Edit => {
            let node_id = op.node_id.ok_or_else(|| WorkflowyError::InvalidInput {
                reason: "edit requires `node_id`".to_string(),
            })?;
            if op.name.is_none() && op.description.is_none() {
                return Err(WorkflowyError::InvalidInput {
                    reason: "edit requires at least one of `name`/`description`".to_string(),
                });
            }
            // Pre-read for rollback. A failed pre-read disables the
            // rollback for this op rather than aborting — partial
            // rollback is better than none.
            let prev = client.get_node(&node_id).await.ok();
            client
                .edit_node(&node_id, op.name.as_deref(), op.description.as_deref())
                .await?;
            footprint.invalidate_node(&node_id);
            let summary = json!({ "op": "edit", "id": node_id.clone() });
            let inverse = prev.map(|n| TxnInverse::RestoreEdit {
                node_id,
                prev_name: Some(n.name),
                prev_description: n.description,
            });
            Ok((summary, inverse, footprint))
        }
        TxnOpKind::Delete => {
            let node_id = op.node_id.ok_or_else(|| WorkflowyError::InvalidInput {
                reason: "delete requires `node_id`".to_string(),
            })?;
            // Name-echo guard (host-coercion defence; see
            // `destructive_echo_matches`). When the op carries `expect_name`,
            // refuse unless the target's current name matches — a coerced
            // `node_id` pointing at the wrong node fails this check before the
            // irreversible delete lands.
            if let Some(expected) = op.expect_name.as_deref() {
                let current = client.get_node(&node_id).await?;
                if !destructive_echo_matches(&current.name, expected) {
                    return Err(WorkflowyError::InvalidInput {
                        reason: format!(
                            "delete refused: node `{}` is named {:?} but expect_name was {:?} — the node_id may have been coerced to a different node than intended; re-resolve and confirm before retrying",
                            node_id, current.name, expected
                        ),
                    });
                }
            }
            client.delete_node(&node_id).await?;
            footprint.invalidate_node(&node_id);
            let summary = json!({ "op": "delete", "id": node_id });
            // Delete is intentionally not invertible — recreating a
            // deleted subtree with stable ids and modification
            // timestamps is not something this server can promise.
            Ok((summary, None, footprint))
        }
        TxnOpKind::Move => {
            let node_id = op.node_id.ok_or_else(|| WorkflowyError::InvalidInput {
                reason: "move requires `node_id`".to_string(),
            })?;
            let new_parent = op.new_parent_id.ok_or_else(|| WorkflowyError::InvalidInput {
                reason: "move requires `new_parent_id`".to_string(),
            })?;
            let prev = client.get_node(&node_id).await.ok();
            let prev_parent_id = prev.as_ref().and_then(|n| n.parent_id.clone());
            let prev_priority = prev.as_ref().and_then(|n| n.priority).map(|p| p as i32);
            client.move_node(&node_id, &new_parent, op.priority).await?;
            footprint.invalidate_cache_only(&new_parent);
            if let Some(pid) = &prev_parent_id {
                footprint.invalidate_cache_only(pid);
            }
            footprint.invalidate_node(&node_id);
            let summary =
                json!({ "op": "move", "id": node_id.clone(), "to": new_parent.clone() });
            let inverse = TxnInverse::UnMove {
                node_id,
                prev_parent_id,
                prev_priority,
            };
            Ok((summary, Some(inverse), footprint))
        }
        TxnOpKind::Complete | TxnOpKind::Uncomplete => {
            let target_state = matches!(kind, TxnOpKind::Complete);
            let node_id = op.node_id.ok_or_else(|| WorkflowyError::InvalidInput {
                reason: format!("{} requires `node_id`", kind.as_str()),
            })?;
            let prev = client.get_node(&node_id).await.ok();
            let prev_completed = prev.as_ref().map(|n| n.completed_at.is_some());
            client.set_completion(&node_id, target_state).await?;
            footprint.invalidate_node(&node_id);
            let summary = json!({ "op": kind.as_str(), "id": node_id.clone() });
            let inverse = prev_completed.map(|p| TxnInverse::RestoreCompletion {
                node_id,
                prev_completed: p,
            });
            Ok((summary, inverse, footprint))
        }
    }
}

/// Apply one inverse during rollback. Same best-effort policy as the
/// previous `server::run_inverse`: failures during rollback are logged
/// but don't stop the rollback queue.
async fn run_txn_inverse(
    client: &WorkflowyClient,
    inv: TxnInverse,
) -> Result<(serde_json::Value, MutationFootprint)> {
    let mut footprint = MutationFootprint::new();
    match inv {
        TxnInverse::DeleteCreated { node_id, parent_id } => {
            client.delete_node(&node_id).await?;
            if let Some(pid) = &parent_id {
                footprint.invalidate_cache_only(pid);
            }
            footprint.invalidate_node(&node_id);
            Ok((json!({ "rolled_back": "create", "id": node_id }), footprint))
        }
        TxnInverse::RestoreEdit {
            node_id,
            prev_name,
            prev_description,
        } => {
            client
                .edit_node(&node_id, prev_name.as_deref(), prev_description.as_deref())
                .await?;
            footprint.invalidate_node(&node_id);
            Ok((json!({ "rolled_back": "edit", "id": node_id }), footprint))
        }
        TxnInverse::UnMove {
            node_id,
            prev_parent_id,
            prev_priority,
        } => {
            if let Some(pid) = prev_parent_id {
                client.move_node(&node_id, &pid, prev_priority).await?;
                footprint.invalidate_cache_only(&node_id);
                footprint.invalidate_cache_only(&pid);
                Ok((
                    json!({ "rolled_back": "move", "id": node_id, "to": pid }),
                    footprint,
                ))
            } else {
                Ok((
                    json!({
                        "skipped": "move",
                        "id": node_id,
                        "reason": "previous parent unknown",
                    }),
                    footprint,
                ))
            }
        }
        TxnInverse::RestoreCompletion {
            node_id,
            prev_completed,
        } => {
            client.set_completion(&node_id, prev_completed).await?;
            footprint.invalidate_node(&node_id);
            Ok((
                json!({
                    "rolled_back": "completion",
                    "id": node_id,
                    "restored_to": prev_completed,
                }),
                footprint,
            ))
        }
    }
}

// ---------------------------------------------------------------------
// bulk_update (op-apply step)
// ---------------------------------------------------------------------

/// The five operations `bulk_update` can apply to a filtered node set.
/// Pre-2026-05-04 both surfaces parsed the operation as a free string
/// and matched in two separate `match op.as_str()` blocks; centralising
/// the enum here means a new op kind lands once and an unknown kind
/// fails validation symmetrically across MCP and CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkOp {
    Delete,
    Complete,
    Uncomplete,
    AddTag,
    RemoveTag,
}

impl BulkOp {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "delete" => Self::Delete,
            "complete" => Self::Complete,
            "uncomplete" => Self::Uncomplete,
            "add_tag" => Self::AddTag,
            "remove_tag" => Self::RemoveTag,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Complete => "complete",
            Self::Uncomplete => "uncomplete",
            Self::AddTag => "add_tag",
            Self::RemoveTag => "remove_tag",
        }
    }

    /// Whether the op needs an `operation_tag` argument. The wrapper
    /// shells the requirement up to the user as a parameter; the
    /// workflow refuses missing tags up-front so the wrapper can pick
    /// one error envelope (`InvalidInput`) over a half-applied bulk.
    pub fn requires_tag(self) -> bool {
        matches!(self, Self::AddTag | Self::RemoveTag)
    }
}

/// Outcome of an `apply_bulk_op` call. The wrapper combines this with
/// its truncation envelope to produce the response shape.
#[derive(Debug, Clone)]
pub struct BulkOpResult {
    pub matched_count: usize,
    pub affected_count: usize,
    /// IDs of the nodes the operation actually mutated. The wrapper
    /// uses these to build a JSON list of `{ id, name, path }` items
    /// when the response shape calls for it.
    pub affected_ids: Vec<String>,
}

/// Apply `op` to each node in `nodes` and return how many succeeded.
/// `nodes` is the post-filter set the wrapper produced from its own
/// walk; this workflow is the apply-step alone.
///
/// Why split the apply from the walk: walk + filter use surface-
/// specific machinery (the MCP server's `walk_subtree` honours its
/// cancel registry + truncation envelope; the CLI's
/// `get_subtree_with_controls` is plain). The apply step, on the other
/// hand, is identical between the two and was duplicated 1:1
/// pre-2026-05-04. Lifting just this step is the smallest change that
/// unifies the actual mutation logic.
pub async fn apply_bulk_op(
    client: &WorkflowyClient,
    op: BulkOp,
    nodes: &[WorkflowyNode],
    operation_tag: Option<&str>,
    _ctx: &WorkflowContext<'_>,
) -> Result<(BulkOpResult, MutationFootprint)> {
    if op.requires_tag() && operation_tag.is_none() {
        return Err(WorkflowyError::InvalidInput {
            reason: format!("operation_tag required for {}", op.as_str()),
        });
    }
    let mut footprint = MutationFootprint::new();
    let mut affected = 0usize;
    let mut affected_ids = Vec::with_capacity(nodes.len());
    // The remove-tag pattern depends only on `operation_tag`, which is fixed
    // for the whole bulk operation — compile it ONCE here rather than per node
    // inside the loop. `None` when the op isn't RemoveTag or the tag is empty.
    let strip_re = if matches!(op, BulkOp::RemoveTag) {
        operation_tag.and_then(crate::utils::tag_parser::compile_tag_strip_regex)
    } else {
        None
    };
    for node in nodes {
        let success = match op {
            BulkOp::Delete => client.delete_node(&node.id).await.is_ok(),
            BulkOp::Complete => client.set_completion(&node.id, true).await.is_ok(),
            BulkOp::Uncomplete => client.set_completion(&node.id, false).await.is_ok(),
            BulkOp::AddTag => {
                let tag = operation_tag.expect("validated by requires_tag check above");
                // Whole-tag idempotency via the shared helper: `None` means
                // the node already carries the tag (skip the write, count as
                // success); `Some(new_name)` is the appended form.
                match crate::utils::tag_parser::add_tag_to_name(&node.name, tag) {
                    None => true,
                    Some(new_name) => {
                        client.edit_node(&node.id, Some(&new_name), None).await.is_ok()
                    }
                }
            }
            BulkOp::RemoveTag => {
                // Reuse the once-compiled pattern; `None` (empty tag) is a no-op
                // rename, matching `remove_tag_from_name`'s empty-tag contract.
                let new_name = match &strip_re {
                    Some(re) => crate::utils::tag_parser::strip_tag_with_regex(re, &node.name),
                    None => node.name.clone(),
                };
                client.edit_node(&node.id, Some(&new_name), None).await.is_ok()
            }
        };
        if success {
            affected += 1;
            affected_ids.push(node.id.clone());
            footprint.invalidate_node(&node.id);
            if let Some(pid) = &node.parent_id {
                footprint.invalidate_cache_only(pid);
            }
        }
    }
    Ok((
        BulkOpResult {
            matched_count: nodes.len(),
            affected_count: affected,
            affected_ids,
        },
        footprint,
    ))
}

// ---------------------------------------------------------------------
// smart_insert (post-disambiguation insertion)
// ---------------------------------------------------------------------

/// Insert content under an already-resolved target node, parsed as
/// 2-space indented hierarchy.
///
/// Pre-2026-05-04 the MCP `smart_insert` handler inserted lines as
/// flat children (no indent awareness), while the `wflow-do
/// smart-insert` subcommand respected indentation. Aligning both
/// surfaces on indent-aware insertion (via `insert_content_via_indented`)
/// closes the divergence in the user-visible behaviour. The "find target
/// by query" walk stays in the wrapper because it uses surface-specific
/// machinery (the MCP server's `walk_subtree`, the CLI's
/// `get_subtree_with_controls`).
pub async fn smart_insert_under_target(
    client: &WorkflowyClient,
    target_node_id: &str,
    content: &str,
    ctx: &WorkflowContext<'_>,
) -> Result<(InsertContentOutcome, MutationFootprint)> {
    let parsed = parse_indented_content(content);
    if parsed.is_empty() {
        return Err(WorkflowyError::InvalidInput {
            reason: "Content cannot be empty".to_string(),
        });
    }
    insert_content_via_indented(client, Some(target_node_id), parsed, ctx).await
}

// ---------------------------------------------------------------------
// reorder_nodes
// ---------------------------------------------------------------------

/// Why a `reorder_nodes_via_priority` call returned partial success
/// rather than complete. Maps 1:1 to the cancel/deadline signals the
/// workflow observes between API calls. Reuses the same vocabulary
/// as `PartialReason` so callers can route on a uniform set of
/// strings across the workflow surface, but kept distinct because a
/// reorder partial does NOT carry a resume cursor: the caller can
/// safely re-issue the full ordered list (each reverse-priority-0
/// move is idempotent) and the workflow will re-converge.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReorderPartialReason {
    Cancelled,
    Timeout,
}

impl ReorderPartialReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ReorderPartialReason::Cancelled => "cancelled",
            ReorderPartialReason::Timeout => "timeout",
        }
    }
}

/// Per-id outcome of a reorder. The workflow walks the desired list
/// in reverse so the first element of `node_ids` is processed last;
/// callers reading this back per-id should remember that the order
/// of `results` matches the input list (head-first), not the order
/// of execution.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ReorderEntry {
    /// The move POST returned 2xx. The id is now under `parent_id`
    /// at position 0 at the moment the move ran (subsequent reverse-
    /// order moves shift it later, by design).
    Ok { node_id: String },
    /// The move POST failed. `error` is the upstream message; the
    /// node may or may not be at its previous position depending on
    /// where the failure occurred. Other ids in the list still ran.
    Error { node_id: String, error: String },
    /// Cancelled or past deadline before this id was reached. The
    /// node was not touched. Reissue the full list to converge.
    Skipped { node_id: String },
}

/// Outcome of a `reorder_nodes_via_priority` call. Two terminal shapes:
///
/// - [`ReorderOutcome::Complete`] — every id was attempted (success or
///   per-id failure recorded in `results`).
/// - [`ReorderOutcome::Partial`] — the workflow bailed mid-walk because
///   the cancel guard flipped or the deadline passed. `results`
///   carries entries up to the bail point (with `Skipped` placeholders
///   for the un-reached ids); the caller can re-issue the full list.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ReorderOutcome {
    Complete {
        parent_id: String,
        attempted: usize,
        succeeded: usize,
        failed: usize,
        results: Vec<ReorderEntry>,
    },
    Partial {
        parent_id: String,
        reason: ReorderPartialReason,
        attempted: usize,
        succeeded: usize,
        failed: usize,
        skipped: usize,
        results: Vec<ReorderEntry>,
    },
}

/// Place the listed `node_ids` in the given order under `parent_id`.
///
/// ## Algorithm
///
/// Workflowy's `move_node` priority is *position-relative-to-siblings*
/// and re-normalises after every call: a naive forward loop with
/// `priority = i` (for i = 0..N) interacts with the renormalisation
/// in ways that depend on which other siblings the parent carries,
/// and on huge subtrees a batched reorder can fight itself. The
/// robust trick is to walk the desired list in **reverse** and pass
/// `priority = 0` on every move:
///
/// ```text
/// desired order: [A, B, C, D]
/// step 1: move D parent_id priority=0  → [D, …other siblings]
/// step 2: move C parent_id priority=0  → [C, D, …]
/// step 3: move B parent_id priority=0  → [B, C, D, …]
/// step 4: move A parent_id priority=0  → [A, B, C, D, …]
/// ```
///
/// Each move plants its node at position 0; the previously-planted
/// nodes shift one step right. After N moves the head of the parent's
/// children is the desired sequence, regardless of how many other
/// siblings were already there or how the upstream renormalises
/// priorities between calls. Other siblings are pushed after the
/// reordered set, in their original relative order — a stable side
/// effect callers should be aware of.
///
/// ## Side effects
///
/// Each move is a real `move_node` POST — nodes not currently under
/// `parent_id` are reparented as a side effect. This makes
/// `reorder_nodes_via_priority` simultaneously a reorder AND a
/// "gather these nodes here in this order" operation, which matches
/// the move-based primitive Workflowy exposes. Callers who only want
/// to reorder strict siblings should pre-validate; the workflow
/// itself does not refetch the parent's children to enforce that.
///
/// ## Validation
///
/// - `node_ids` must be non-empty.
/// - `node_ids` must not contain duplicates (the order would be
///   undefined, and re-moving the same node to priority 0 in the
///   same call is just slow with no net effect).
/// - `node_ids.len()` must not exceed [`defaults::MAX_REORDER_NODES`].
/// - No node id may equal `parent_id` (would attempt to make a node
///   its own child — the API rejects, but we do too, with a clearer
///   message and no API touch).
///
/// ## Cancel / deadline
///
/// Checked between iterations. The workflow returns
/// [`ReorderOutcome::Partial`] with the un-reached ids marked
/// `Skipped` so the caller can re-issue the full list.
pub async fn reorder_nodes_via_priority(
    client: &WorkflowyClient,
    parent_id: &str,
    node_ids: &[String],
    ctx: &WorkflowContext<'_>,
) -> Result<(ReorderOutcome, MutationFootprint)> {
    let mut footprint = MutationFootprint::new();

    if node_ids.is_empty() {
        return Err(WorkflowyError::InvalidInput {
            reason: "node_ids must not be empty".to_string(),
        });
    }
    if node_ids.len() > defaults::MAX_REORDER_NODES {
        return Err(WorkflowyError::InvalidInput {
            reason: format!(
                "node_ids length {} exceeds the {}-id cap. Split into batches \
                 of ≤{} ids and call reorder_nodes once per batch (each batch \
                 lands at the head of the parent's children at call time, so \
                 the LAST batch ends up first; order your batches accordingly).",
                node_ids.len(),
                defaults::MAX_REORDER_NODES,
                defaults::MAX_REORDER_NODES,
            ),
        });
    }
    let mut seen = std::collections::HashSet::with_capacity(node_ids.len());
    for id in node_ids {
        if !seen.insert(id.as_str()) {
            return Err(WorkflowyError::InvalidInput {
                reason: format!(
                    "node_ids contains duplicate `{}` — order is undefined for \
                     duplicates and re-moving the same node within one call is \
                     a no-op. Pass each id at most once.",
                    id
                ),
            });
        }
        if id == parent_id {
            return Err(WorkflowyError::InvalidInput {
                reason: format!(
                    "node_ids contains parent_id `{}` — a node cannot be its \
                     own child.",
                    id
                ),
            });
        }
    }

    // Pre-declare the footprint: we will touch the parent (its children
    // listing changes) and every node in the list (its parent_id may
    // change, and its position certainly does). The wrapper applies
    // these post-call.
    footprint.invalidate_cache_only(parent_id);
    for id in node_ids {
        footprint.invalidate_node(id.clone());
    }

    // Walk in reverse so the first id ends up at position 0 last.
    // Build the per-id results in input order so the response shape
    // matches the request.
    let mut results: Vec<Option<ReorderEntry>> = (0..node_ids.len()).map(|_| None).collect();
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut bailed: Option<ReorderPartialReason> = None;

    for (idx, id) in node_ids.iter().enumerate().rev() {
        if ctx.is_cancelled() {
            bailed = Some(ReorderPartialReason::Cancelled);
            break;
        }
        if ctx.is_past_deadline() {
            bailed = Some(ReorderPartialReason::Timeout);
            break;
        }
        match client.move_node(id, parent_id, Some(0)).await {
            Ok(()) => {
                results[idx] = Some(ReorderEntry::Ok {
                    node_id: id.clone(),
                });
                succeeded += 1;
            }
            Err(e) => {
                error!(
                    node_id = %id,
                    parent_id = %parent_id,
                    error = %e,
                    "reorder_nodes: move failed"
                );
                results[idx] = Some(ReorderEntry::Error {
                    node_id: id.clone(),
                    error: e.to_string(),
                });
                failed += 1;
            }
        }
    }

    // Fill in skipped placeholders for any ids that the loop never
    // reached (only possible on a cancel / deadline bail-out).
    let mut skipped = 0usize;
    for (idx, id) in node_ids.iter().enumerate() {
        if results[idx].is_none() {
            results[idx] = Some(ReorderEntry::Skipped {
                node_id: id.clone(),
            });
            skipped += 1;
        }
    }
    let results: Vec<ReorderEntry> = results.into_iter().map(|r| r.expect("filled")).collect();

    let attempted = succeeded + failed;
    let outcome = match bailed {
        None => ReorderOutcome::Complete {
            parent_id: parent_id.to_string(),
            attempted,
            succeeded,
            failed,
            results,
        },
        Some(reason) => ReorderOutcome::Partial {
            parent_id: parent_id.to_string(),
            reason,
            attempted,
            succeeded,
            failed,
            skipped,
            results,
        },
    };
    Ok((outcome, footprint))
}

// ---------------------------------------------------------------------
// duplicate_subtree / instantiate_template (deep-copy a subtree)
// ---------------------------------------------------------------------

// Pre-2026-06-16 the deep-copy orchestration that backs both
// `duplicate_node` and `create_from_template` was inlined twice on each
// surface — four copies in total — and they had silently diverged:
//
// - The MCP `duplicate_node` walked at depth 10, ordered children via a
//   `children_of` map + BFS (deterministic sibling order), supported a
//   `name_prefix`, and refused to run against a truncated subtree. The
//   CLI `duplicate` walked at depth 10/0, recreated via a frontier-stack
//   DFS (non-deterministic sibling order under the API's ordering), had
//   no `name_prefix`, and DID refuse truncation.
// - The MCP `create_from_template` substituted `{{var}}` via a regex with
//   unmatched-variable passthrough (an unknown `{{x}}` is left intact),
//   BFS-ordered, refused truncation. The CLI `template` substituted via
//   literal `str::replace("{{k}}", v)` per known var (a substring replace
//   with NO passthrough behaviour distinct from the regex — the two agree
//   for known vars but the CLI silently dropped nothing and could corrupt
//   on overlapping keys), DFS-ordered, did NOT refuse truncation.
//
// The lift collapses all four into one private BFS deep-copy helper
// (`deep_copy_subtree`) parameterised by a per-node name/description
// transform, with two thin public wrappers. CANONICAL behaviour is the
// MCP form throughout: BFS ordering (deterministic), regex substitution
// with unmatched-variable passthrough, truncated-subtree refusal. The CLI
// GAINS `name_prefix` + truncation refusal on `duplicate`, and regex
// substitution + unmatched-var passthrough on `template`.

/// Outcome of a [`duplicate_subtree`] call. Carries the new root id, the
/// original source id, and the count of nodes created.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateOutcome {
    /// The source node id that was duplicated.
    pub original_id: String,
    /// The id of the freshly-created root copy.
    pub new_root_id: String,
    /// Total nodes created (root + descendants when `include_children`).
    pub nodes_created: usize,
}

/// Outcome of an [`instantiate_template`] call. Like [`DuplicateOutcome`]
/// but names the template source and lists the variable keys supplied.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateOutcome {
    /// The template node id that was instantiated.
    pub template_id: String,
    /// The id of the freshly-created root copy.
    pub new_root_id: String,
    /// Total nodes created (root + descendants).
    pub nodes_created: usize,
    /// The variable keys the caller supplied (for the response shape —
    /// not every key is necessarily present in the template text).
    pub variables_applied: Vec<String>,
}

/// Compile the canonical template-variable pattern once. The pattern is a
/// static literal, so compilation cannot fail at runtime; we surface it
/// as a `WorkflowyError` rather than `expect()` to keep the workflow free
/// of panics per the no-unwrap-outside-tests rule.
fn template_var_regex() -> Result<regex::Regex> {
    regex::Regex::new(r"\{\{(\w+)\}\}")
        .map_err(|e| WorkflowyError::Internal(format!("template-variable pattern failed to compile: {e}")))
}

/// Build the per-node transform that performs `{{var}}` substitution on a
/// string. UNMATCHED variables (keys not present in `vars`) are left
/// intact — `{{unknown}}` survives verbatim. This is the canonical
/// (MCP) substitution behaviour; the CLI's pre-lift literal-replace had
/// no passthrough concept and could not be made to agree on overlapping
/// keys. Applied to both name and description by [`instantiate_template`].
fn substitute_vars(re: &regex::Regex, vars: &std::collections::HashMap<String, String>, text: &str) -> String {
    re.replace_all(text, |caps: &regex::Captures| {
        vars.get(&caps[1]).cloned().unwrap_or_else(|| caps[0].to_string())
    })
    .to_string()
}

/// Shared BFS deep-copy of a subtree. Both [`duplicate_subtree`] and
/// [`instantiate_template`] route through this; they differ only in the
/// per-node `transform` closure that maps a source node to the
/// `(name, description)` the copy should carry.
///
/// CANONICAL traversal is BFS via a `children_of` map keyed off the
/// source parent ids — this preserves the source's sibling order
/// deterministically (a frontier-stack DFS does not, and the CLI's
/// pre-lift DFS could reorder siblings). The walk, truncation refusal,
/// and the empty-subtree check happen here so both wrappers inherit them.
///
/// `walk_depth` is the subtree depth to fetch; callers pass
/// [`defaults::MAX_TREE_DEPTH`] for a full copy or `0` to copy only the
/// root (the `include_children=false` path).
///
/// Returns `(new_root_id, nodes_created, footprint)`. The footprint
/// declares the target parent (its children listing changed) for cache
/// invalidation — the created nodes are brand-new ids the caller doesn't
/// hold, so there is nothing stale to invalidate for them.
async fn deep_copy_subtree<F>(
    client: &WorkflowyClient,
    source_id: &str,
    target_parent_id: &str,
    walk_depth: usize,
    truncation_refusal_label: &str,
    not_found_label: &str,
    ctx: &WorkflowContext<'_>,
    transform: F,
) -> Result<(String, usize, MutationFootprint)>
where
    F: Fn(&WorkflowyNode) -> (String, Option<String>),
{
    let mut controls =
        crate::api::client::FetchControls::with_timeout(std::time::Duration::from_millis(
            defaults::SUBTREE_FETCH_TIMEOUT_MS,
        ));
    if let Some(guard) = ctx.cancel {
        controls = controls.and_cancel(guard.clone());
    }
    // An explicit deadline on the context tightens the walk budget when
    // the wrapping ToolKind budget is shorter than the default subtree
    // timeout. The CLI passes no deadline, so it keeps the full budget.
    if let Some(dl) = ctx.deadline {
        controls.deadline = Some(dl);
    }

    let fetch = client
        .get_subtree_with_controls(
            Some(source_id),
            walk_depth,
            defaults::MAX_SUBTREE_NODES,
            controls,
        )
        .await?;

    // Refuse a partial copy outright: producing a silently-incomplete
    // duplicate is worse than failing loudly. This is the MCP contract;
    // the CLI gains it via this shared path.
    if fetch.truncated {
        return Err(WorkflowyError::InvalidInput {
            reason: format!(
                "Cannot {}: source subtree exceeds {} nodes. Refusing to produce a partial copy. \
                 Narrow the source or copy sub-branches individually.",
                truncation_refusal_label, fetch.limit,
            ),
        });
    }

    let nodes = &fetch.nodes;
    let root = nodes.iter().find(|n| n.id == source_id).ok_or_else(|| {
        WorkflowyError::InvalidInput {
            reason: format!("{} '{}' not found", not_found_label, source_id),
        }
    })?;

    let mut footprint = MutationFootprint::new();
    // The target parent's children listing changes; the created nodes are
    // new ids with nothing stale to invalidate.
    footprint.invalidate_cache_only(target_parent_id);

    // Create the root copy first so children can reparent under it.
    let (root_name, root_desc) = transform(root);
    let created_root = client
        .create_node(&root_name, root_desc.as_deref(), Some(target_parent_id), None)
        .await?;
    let new_root_id = created_root.id.clone();
    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    id_map.insert(root.id.clone(), created_root.id);
    let mut created_count = 1usize;

    // Only walk descendants when the fetch actually returned any (depth 0
    // returns just the root, so the children map is empty and the loop is
    // a no-op — covering the `include_children=false` path uniformly).
    let mut children_of: std::collections::HashMap<&str, Vec<&WorkflowyNode>> =
        std::collections::HashMap::new();
    for n in nodes {
        if let Some(pid) = &n.parent_id {
            children_of.entry(pid.as_str()).or_default().push(n);
        }
    }

    let mut queue = std::collections::VecDeque::new();
    if let Some(children) = children_of.get(root.id.as_str()) {
        for child in children {
            queue.push_back(*child);
        }
    }

    while let Some(node) = queue.pop_front() {
        // The underlying create_node honours ctx.cancel via the client's
        // request path; we additionally bail between creates so a
        // cancel/deadline stops the copy promptly rather than after one
        // more round-trip. A bailed-out copy is an Internal error (the
        // partial tree is already written) so the caller re-reads.
        if ctx.is_cancelled() {
            return Err(WorkflowyError::Internal(format!(
                "copy cancelled after creating {} of {} nodes; re-read the target to inspect the partial tree",
                created_count,
                nodes.len(),
            )));
        }
        if ctx.is_past_deadline() {
            return Err(WorkflowyError::Internal(format!(
                "copy timed out after creating {} of {} nodes; re-read the target to inspect the partial tree",
                created_count,
                nodes.len(),
            )));
        }

        let new_parent = node
            .parent_id
            .as_ref()
            .and_then(|pid| id_map.get(pid))
            .cloned()
            .unwrap_or_else(|| new_root_id.clone());

        let (name, desc) = transform(node);
        let created = client
            .create_node(&name, desc.as_deref(), Some(&new_parent), None)
            .await?;
        id_map.insert(node.id.clone(), created.id);
        created_count += 1;
        if let Some(children) = children_of.get(node.id.as_str()) {
            for child in children {
                queue.push_back(*child);
            }
        }
    }

    Ok((new_root_id, created_count, footprint))
}

/// Deep-copy a node subtree to a new parent.
///
/// Both the MCP `duplicate_node` handler and the CLI `duplicate`
/// subcommand route through this so they cannot drift. `include_children`
/// selects a full copy (BFS over the whole subtree) vs. root-only;
/// `name_prefix`, when set, prepends to the ROOT node's name only
/// (descendants keep their names). The truncated-subtree refusal lives in
/// the shared [`deep_copy_subtree`] helper.
///
/// IDs are expected pre-resolved by the caller (the MCP wrapper resolves
/// short-hashes / links; the CLI passes through whatever the user gave).
pub async fn duplicate_subtree(
    client: &WorkflowyClient,
    source_id: &str,
    target_parent_id: &str,
    include_children: bool,
    name_prefix: Option<&str>,
    ctx: &WorkflowContext<'_>,
) -> Result<(DuplicateOutcome, MutationFootprint)> {
    let walk_depth = if include_children { defaults::MAX_TREE_DEPTH } else { 0 };
    let root_id = source_id.to_string();
    let prefix = name_prefix.map(|p| p.to_string());

    let transform = |node: &WorkflowyNode| -> (String, Option<String>) {
        // The prefix applies to the ROOT only — identified by id equality
        // with the source. Descendants copy verbatim.
        let name = if node.id == root_id {
            match &prefix {
                Some(p) => format!("{}{}", p, node.name),
                None => node.name.clone(),
            }
        } else {
            node.name.clone()
        };
        (name, node.description.clone())
    };

    let (new_root_id, nodes_created, footprint) = deep_copy_subtree(
        client,
        source_id,
        target_parent_id,
        walk_depth,
        "duplicate",
        "Node",
        ctx,
        transform,
    )
    .await?;

    Ok((
        DuplicateOutcome {
            original_id: source_id.to_string(),
            new_root_id,
            nodes_created,
        },
        footprint,
    ))
}

/// Instantiate a template subtree under a new parent, substituting
/// `{{variable}}` tokens in every copied node's name and description.
///
/// Both the MCP `create_from_template` handler and the CLI `template`
/// subcommand route through this. CANONICAL substitution is regex
/// `\{\{(\w+)\}\}` with UNMATCHED variables left intact — the CLI's
/// pre-lift literal `str::replace` had no passthrough concept; routing it
/// here is a behavioural improvement (an unknown `{{x}}` now survives
/// verbatim instead of being silently left as-is only because no
/// replace rule matched). Always a full BFS copy.
pub async fn instantiate_template(
    client: &WorkflowyClient,
    template_id: &str,
    target_parent_id: &str,
    variables: &std::collections::HashMap<String, String>,
    ctx: &WorkflowContext<'_>,
) -> Result<(TemplateOutcome, MutationFootprint)> {
    let re = template_var_regex()?;
    let transform = |node: &WorkflowyNode| -> (String, Option<String>) {
        let name = substitute_vars(&re, variables, &node.name);
        let desc = node.description.as_ref().map(|d| substitute_vars(&re, variables, d));
        (name, desc)
    };

    let (new_root_id, nodes_created, footprint) = deep_copy_subtree(
        client,
        template_id,
        target_parent_id,
        defaults::MAX_TREE_DEPTH,
        "instantiate template",
        "Template",
        ctx,
        transform,
    )
    .await?;

    let mut variables_applied: Vec<String> = variables.keys().cloned().collect();
    variables_applied.sort();

    Ok((
        TemplateOutcome {
            template_id: template_id.to_string(),
            new_root_id,
            nodes_created,
            variables_applied,
        },
        footprint,
    ))
}

// ---------------------------------------------------------------------
// audit_mirrors
// ---------------------------------------------------------------------

/// Outcome of an [`audit_mirrors_walk`] call. Carries the union of
/// nodes visited (deduped by id), an aggregated truncation envelope
/// (`truncated` + `truncation_reason`), and a per-chunk envelope when
/// the walk was chunked.
///
/// Pre-2026-05-16 the MCP server defined an `AuditWalkOutcome` struct
/// in `server/mod.rs` with the same fields and the CLI inlined its
/// own version. Lifting both into this single typed shape closes the
/// drift the architecture review surfaced: the MCP decremented
/// `child_depth` via `saturating_sub(1)` while the CLI hardcoded
/// `7`, and the two surfaces ordered the "include the root" step
/// differently. Both surfaces now share this single orchestration.
#[derive(Debug, Clone, Serialize)]
pub struct AuditMirrorsWalkOutcome {
    pub nodes: Vec<WorkflowyNode>,
    pub truncated: bool,
    /// Stable string form (`"node_limit"`/`"timeout"`/`"cancelled"`)
    /// or `None`. The string form is what every existing JSON
    /// payload emits — keeping the typed enum here would force a
    /// second translation at the wire.
    pub truncation_reason: Option<&'static str>,
    /// Per-chunk envelope when the walk was chunked (one entry per
    /// direct child of the root). Empty when `chunked = false`.
    pub chunks: Vec<serde_json::Value>,
}

/// Walk a subtree for the `audit_mirrors` workflow.
///
/// When `chunked = true`, lists `root_id`'s direct children and
/// walks each as its own subtree with `max_depth - 1` so each pillar
/// fits comfortably under the [`defaults::MAX_SUBTREE_NODES`] walk
/// cap. The root node itself is best-effort fetched and included so
/// a `mirror_of:` marker living directly under the root is observed.
/// Returned nodes are deduped by id (a node visited via two chunks
/// is counted once).
///
/// When `chunked = false`, fetches the subtree under `root_id` as a
/// single walk with `max_depth`.
///
/// Cancel/deadline are observed via `_ctx` for API uniformity; the
/// per-walk timeout from [`defaults::SUBTREE_FETCH_TIMEOUT_MS`]
/// already bounds each `get_subtree_with_controls` call.
pub async fn audit_mirrors_walk(
    client: &WorkflowyClient,
    root_id: &str,
    max_depth: usize,
    chunked: bool,
    _ctx: &WorkflowContext<'_>,
) -> Result<AuditMirrorsWalkOutcome> {
    let make_controls = || {
        crate::api::FetchControls::with_timeout(std::time::Duration::from_millis(
            defaults::SUBTREE_FETCH_TIMEOUT_MS,
        ))
    };

    if !chunked {
        let fetch = client
            .get_subtree_with_controls(
                Some(root_id),
                max_depth,
                defaults::MAX_SUBTREE_NODES,
                make_controls(),
            )
            .await?;
        return Ok(AuditMirrorsWalkOutcome {
            nodes: fetch.nodes,
            truncated: fetch.truncated,
            truncation_reason: fetch.truncation_reason.map(|r| r.as_str()),
            chunks: Vec::new(),
        });
    }

    // Chunked path. Include the root itself first (so a mirror_of
    // marker directly under the root is observed), then walk each
    // child. Best-effort on the root fetch — a failure there is
    // non-fatal; the audit still runs across children.
    let mut all_nodes: Vec<WorkflowyNode> = Vec::new();
    if let Ok(root_node) = client.get_node(root_id).await {
        all_nodes.push(root_node);
    }
    let children = client.get_children(root_id).await?;
    let child_depth = max_depth.saturating_sub(1);
    let mut chunks: Vec<serde_json::Value> = Vec::new();
    let mut truncated_any = false;
    let mut top_truncation: Option<&'static str> = None;
    for child in &children {
        let fetch = client
            .get_subtree_with_controls(
                Some(&child.id),
                child_depth,
                defaults::MAX_SUBTREE_NODES,
                make_controls(),
            )
            .await?;
        let truncated = fetch.truncated;
        let scanned = fetch.nodes.len();
        let reason = fetch.truncation_reason.map(|r| r.as_str());
        if truncated {
            truncated_any = true;
            if top_truncation.is_none() {
                top_truncation = reason;
            }
        }
        chunks.push(json!({
            "id": child.id,
            "name": child.name,
            "scanned": scanned,
            "truncated": truncated,
            "truncation_reason": reason,
        }));
        all_nodes.extend(fetch.nodes);
    }
    all_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    all_nodes.dedup_by(|a, b| a.id == b.id);
    Ok(AuditMirrorsWalkOutcome {
        nodes: all_nodes,
        truncated: truncated_any,
        truncation_reason: top_truncation,
        chunks,
    })
}

/// Extract the set of `mirror_of:` UUIDs encountered in `nodes` that
/// are not resolved within the walked scope. The caller resolves each
/// returned UUID through its surface-appropriate resolver (the MCP
/// server uses its persistent name index; the CLI issues a live
/// `get_node` against the API) and assembles the `external_canonicals`
/// map for [`crate::audit::audit_mirrors_with_external`].
///
/// "Not in scope" is end-matched in both directions so a short-hash
/// `mirror_of:` resolves against a full-UUID node in scope and vice
/// versa — mirrors the MCP and CLI inline checks pre-2026-05-16.
pub fn extract_unresolved_mirror_targets(nodes: &[WorkflowyNode]) -> Vec<String> {
    use std::collections::HashSet;
    let in_scope: HashSet<String> = nodes.iter().map(|n| n.id.to_lowercase()).collect();
    let mut targets: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for node in nodes {
        let desc = node.description.as_deref().unwrap_or("");
        if let Some(target) = extract_marker(desc, "mirror_of:") {
            let t = target.to_lowercase();
            if in_scope.iter().any(|s| s.ends_with(&t) || t.ends_with(s)) {
                continue;
            }
            if seen.insert(t.clone()) {
                targets.push(t);
            }
        }
    }
    targets
}

// ---------------------------------------------------------------------
// resolve_link (lifted 2026-05-22)
// ---------------------------------------------------------------------
//
// The MCP `resolve_link` handler and the `wflow-do resolve-link` CLI
// subcommand share three lifted pieces:
//
// 1. `find_node_by_short_hash` — pure scan over walked nodes for a
//    matching UUID (full-form or 12-char trailing form).
// 2. `resolve_link_via_walk_and_scan` — async function that walks a
//    scope with the supplied `FetchControls`, then runs (1) over the
//    returned nodes. Returns `ResolveLinkWalkResult { found, nodes_walked,
//    truncated, truncation_reason, elapsed_ms }`.
// 3. `build_resolve_link_hit_payload` / `build_resolve_link_miss_payload`
//    — JSON envelope constructors that both surfaces use so the wire
//    shape is byte-identical.
//
// The MCP handler layers server-only concerns ON TOP of these (preflight
// name-index lookup, single-flight scope marker via
// `inflight_resolve_walk_scopes`, name-index ingestion of walked nodes).
// The CLI omits those because it has no persistent name index and no
// concurrent caller. Both surfaces share the envelope shape through the
// `build_resolve_link_*_payload` builders.
//
// Pre-2026-05-22 the two surfaces diverged: the CLI emitted a thin
// `{link, node}` payload on hit and an Err on miss; the MCP emitted the
// full four-field truncation envelope with `resolved_via` /
// `name_index_size` / tool-specific `hint`. The lift makes the CLI a
// facade over the lifted helpers; both surfaces now emit the same
// envelope on miss and a hit shape that differs only in
// `resolved_via` values reachable from each transport (the MCP can
// hit the persistent name index for `cache_hit`; the CLI cannot, and
// reports `primary_walk` after walking).

/// Pure scan: find a node by its short-hash form in a list of walked
/// nodes. Matches when the candidate is the full UUID OR the trailing
/// 12-char short-hash form. The match is case-insensitive on hex.
///
/// Used by both the MCP server's `resolve_link` (post-walk scan when
/// the name-index didn't already resolve the hash) and the CLI
/// `resolve-link` subcommand (no name-index — the walk is the only
/// data source). Pre-2026-05-22 each surface inlined this scan and
/// they diverged on case sensitivity (CLI was case-sensitive; MCP
/// went through the lowercase `name_index.resolve_short_hash`).
pub fn find_node_by_short_hash<'a>(
    nodes: &'a [WorkflowyNode],
    candidate: &str,
) -> Option<&'a WorkflowyNode> {
    let cand_lc = candidate.to_lowercase();
    nodes.iter().find(|n| {
        let nid = n.id.to_lowercase();
        nid == cand_lc || nid.ends_with(&cand_lc)
    })
}

/// Result of the walk-and-scan step shared between MCP and CLI. The
/// caller (MCP handler or CLI subcommand) projects this into the
/// `resolve_link` JSON envelope via the `build_resolve_link_*_payload`
/// helpers.
#[derive(Debug, Clone)]
pub struct ResolveLinkWalkResult {
    /// The matching node, when the walk reached it.
    pub found: Option<WorkflowyNode>,
    /// Number of nodes the walk actually returned. On a primary walk
    /// this is the true count; the secondary-attach path in the MCP
    /// handler doesn't go through this function (the secondary polls
    /// the name index instead of walking).
    pub nodes_walked: usize,
    /// Whether the walk truncated under the FetchControls budget.
    pub truncated: bool,
    /// Why the walk truncated, when it did.
    pub truncation_reason: Option<TruncationReason>,
    pub elapsed_ms: u64,
    /// The full list of walked nodes — exposed so the MCP handler can
    /// ingest them into the persistent name index AFTER this function
    /// returns. The CLI ignores this field. (Returning the nodes here
    /// rather than re-walking from the handler keeps the API surface
    /// to one call.)
    pub nodes: Vec<WorkflowyNode>,
}

/// Walk a scope with the supplied `FetchControls`, then scan the
/// returned nodes for a short-hash match. Used by both the MCP server
/// and the CLI as the canonical resolve-link walk step.
///
/// The function is intentionally narrow: walk + scan + return. The
/// caller decides what to do with truncation, what to do on a miss,
/// and (for the MCP server) whether to ingest the walked nodes into
/// the persistent name index. This keeps the workflow framework-agnostic.
pub async fn resolve_link_via_walk_and_scan(
    client: &WorkflowyClient,
    short_hash: &str,
    parent_id: Option<&str>,
    controls: crate::api::client::FetchControls,
) -> Result<ResolveLinkWalkResult> {
    let fetch = client
        .get_subtree_with_controls(
            parent_id,
            defaults::MAX_TREE_DEPTH,
            defaults::RESOLVE_WALK_NODE_CAP,
            controls,
        )
        .await?;
    let found = find_node_by_short_hash(&fetch.nodes, short_hash).cloned();
    Ok(ResolveLinkWalkResult {
        found,
        nodes_walked: fetch.nodes.len(),
        truncated: fetch.truncated,
        truncation_reason: fetch.truncation_reason,
        elapsed_ms: fetch.elapsed_ms,
        nodes: fetch.nodes,
    })
}

/// Recovery-hint string for the truncation envelope on a `resolve_link`
/// miss. Tool-specific — the generic `TRUNCATION_RECOVERY_HINT` points
/// at `find_node`/`search_nodes` with `use_index=true`, which is the
/// wrong tool for a short-hash resolution failure (those search by
/// *name*, not by hash). Both the MCP handler and the CLI subcommand
/// thread this constant through `with_truncation_envelope_and_hint`.
pub const RESOLVE_LINK_RECOVERY_HINT: &str = "Supply `search_parent_path` to scope the walk to a smaller subtree (seconds rather than the full workspace budget). If the short hash isn't in the persistent name index, the node may have been deleted — open the link in Workflowy to verify it still exists.";

/// Strip a small set of HTML tags Workflowy emits in node names so the
/// `resolve_link` payload returns a plain-text `name`. Mirrors the
/// `strip_html` helper in `server/mod.rs`; lifted here so the workflow
/// can be called by surfaces that don't import server internals.
/// Build the JSON payload a `resolve_link` HIT returns on either
/// surface. Both transports emit the same five fields so a caller
/// migrating between MCP and CLI sees the same shape.
///
/// `resolved_via` is the discriminator naming the resolution path: the
/// MCP handler emits `cache_hit` / `scoped_walk` /
/// `secondary_attached_index_hit` / `full_uuid_passthrough`; the CLI
/// emits `scoped_walk` (its only walk path) or `full_uuid_passthrough`.
pub fn build_resolve_link_hit_payload(
    node: &WorkflowyNode,
    resolved_via: &str,
) -> serde_json::Value {
    json!({
        "resolved_via": resolved_via,
        "id": node.id,
        "name": crate::utils::html::strip_html(&node.name),
        "description": node.description,
        "parent_id": node.parent_id,
    })
}

/// Build the human-readable `hint` string for a `resolve_link` miss,
/// matched to the `resolved_via` value. The MCP and CLI both call this
/// so the diagnostic text cannot drift across surfaces. `name_index_size`
/// is `None` for the CLI (which has no persistent name index) and
/// `Some(size)` for the MCP handler — when `None`, the hint omits the
/// "persistent index has N entries" sentence.
pub fn build_resolve_link_miss_hint(
    short_hash: &str,
    scope_str: &str,
    nodes_walked: usize,
    elapsed_ms: u64,
    resolved_via: &str,
    name_index_size: Option<usize>,
) -> String {
    let index_clause = match name_index_size {
        Some(n) => format!(" The persistent name index has {} entries; if it covers the whole workspace, the hash likely refers to a deleted node or a link from a different account.", n),
        None => String::new(),
    };
    match resolved_via {
        "secondary_attached" => format!(
            "Short-hash '{}' not found in the persistent name index{} after attaching to a concurrent walk for {} that finished in {} ms without resolving. Try: (a) opening the link in Workflowy to verify the node still exists — the URL may refer to a deleted node or be from a different account; (b) supplying `search_parent_path` if you know roughly where the node lives so a scoped walk can find it; (c) waiting ~30 min for the background index refresher to widen coverage, then retrying.",
            short_hash,
            name_index_size.map(|n| format!(" ({} entries)", n)).unwrap_or_default(),
            scope_str,
            elapsed_ms,
        ),
        "walk_error" => format!(
            "Short-hash '{}' could not be resolved under {} because the resolution walk errored before completion.{} Try: (a) calling `health_check` to see whether the server is currently degraded; (b) supplying `search_parent_path` for a smaller scope that won't stress the rate limiter; (c) retrying in a few seconds.",
            short_hash, scope_str, index_clause,
        ),
        _ /* "primary_walk" or CLI's equivalent */ => format!(
            "Short-hash '{}' not found under {} after walking {} nodes in {} ms.{} Try: (a) supplying a more specific `search_parent_path` that contains the target; (b) opening the node in Workflowy and copying the URL bar to get a full URL the server can resolve directly; (c) calling `node_at_path` with the full hierarchical path if you know it.",
            short_hash, scope_str, nodes_walked, elapsed_ms, index_clause,
        ),
    }
}

/// Build the JSON payload a `resolve_link` MISS returns on either
/// surface — including the four-field truncation envelope. Both the
/// MCP handler and the CLI route through this function so they cannot
/// drift on envelope shape, hint contents, or `resolved_via` values.
///
/// `name_index_size` is `Some(size)` for the MCP handler (which has a
/// persistent name index) and `None` for the CLI; the miss hint
/// adjusts accordingly.
pub fn build_resolve_link_miss_payload(
    short_hash: &str,
    scope_str: &str,
    nodes_walked: usize,
    elapsed_ms: u64,
    truncated: bool,
    truncation_reason: Option<TruncationReason>,
    resolved_via: &str,
    name_index_size: Option<usize>,
) -> serde_json::Value {
    let hint = build_resolve_link_miss_hint(
        short_hash,
        scope_str,
        nodes_walked,
        elapsed_ms,
        resolved_via,
        name_index_size,
    );
    let payload = json!({
        "resolved": serde_json::Value::Null,
        "short_hash": short_hash,
        "scope": scope_str,
        "nodes_walked": nodes_walked,
        "elapsed_ms": elapsed_ms,
        "resolved_via": resolved_via,
        "name_index_size": name_index_size,
        "hint": hint,
    });
    with_truncation_envelope_and_hint(
        payload,
        truncated,
        defaults::RESOLVE_WALK_NODE_CAP,
        truncation_reason,
        RESOLVE_LINK_RECOVERY_HINT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `destructive_echo_matches` is the shared comparison both delete
    /// surfaces use. Trim-tolerant on both sides, case-SENSITIVE (names are
    /// user content), and exact on the trimmed core — so a different node's
    /// name fails the guard.
    #[test]
    fn destructive_echo_matches_trims_but_is_case_sensitive_and_exact() {
        assert!(destructive_echo_matches("Real Project", "Real Project"));
        assert!(destructive_echo_matches("  Real Project  ", "Real Project"));
        assert!(destructive_echo_matches("Real Project", "  Real Project"));
        // Case difference is a real difference.
        assert!(!destructive_echo_matches("Real Project", "real project"));
        // A different node entirely.
        assert!(!destructive_echo_matches("Real Project", "Archive"));
        // Internal whitespace is significant (only the ends are trimmed).
        assert!(!destructive_echo_matches("Real  Project", "Real Project"));
    }

    /// Self-mirror is rejected at the validation step without an API
    /// call. Pinned because both surfaces depend on this preflight to
    /// keep their error envelopes structured rather than network-shaped.
    #[tokio::test]
    async fn create_mirror_via_convention_rejects_self_mirror_without_api_call() {
        let client = WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds");
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let ctx = WorkflowContext::default();
        let err = create_mirror_via_convention(&client, id, Some(id), None, None, &ctx)
            .await
            .expect_err("self-mirror must reject");
        // The InvalidInput variant is the structured signal both
        // surfaces map to their own error envelopes. A network-shaped
        // variant would mean the preflight didn't catch the case
        // before the API call.
        assert!(
            matches!(err, WorkflowyError::InvalidInput { .. }),
            "self-mirror must surface as InvalidInput, got: {err:?}"
        );
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("cannot mirror itself") || msg.contains("cannot equal"),
            "validation message must explain the constraint: {msg}"
        );
    }

    /// [C-wf-013] `create_mirror_dry_run` rejects self-mirror with the
    /// same `InvalidInput` envelope as the production call, before any
    /// API touch. Pinned so the dry-run preview is as authoritative as
    /// the real call — failure-report 2026-05-09 fix asked for a
    /// preview that catches the same constraints.
    #[tokio::test]
    async fn create_mirror_dry_run_rejects_self_mirror_without_api_call() {
        let client = WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds");
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let err = create_mirror_dry_run(&client, id, Some(id), None)
            .await
            .expect_err("dry-run self-mirror must reject");
        assert!(
            matches!(err, WorkflowyError::InvalidInput { .. }),
            "dry-run self-mirror must surface as InvalidInput, got: {err:?}"
        );
    }

    /// [C-wf-014] `scope_resolved_label` produces the stable two-form
    /// label every MCP tool emits: `workspace_root` for None,
    /// `scoped:<uuid>` for Some. Pinned because a future contributor
    /// changing the format silently breaks every diagnostic surface
    /// that callers rely on to audit null-parent resolution.
    #[test]
    fn scope_resolved_label_two_branches_render_stable_format() {
        assert_eq!(scope_resolved_label(None), "workspace_root");
        assert_eq!(
            scope_resolved_label(Some("550e8400-e29b-41d4-a716-446655440000")),
            "scoped:550e8400-e29b-41d4-a716-446655440000",
        );
    }

    /// `WorkflowContext::default()` has no cancel and no deadline —
    /// `is_cancelled()` and `is_past_deadline()` both return false.
    /// This is the CLI's normal case. Pinned because regressions in
    /// the helpers would trigger spurious bailouts in workflows that
    /// rely on the cheap "did anything happen" check.
    #[test]
    fn workflow_context_default_signals_no_cancel_and_no_deadline() {
        let ctx = WorkflowContext::default();
        assert!(!ctx.is_cancelled());
        assert!(!ctx.is_past_deadline());
    }

    /// A past deadline trips `is_past_deadline()`; a future one
    /// doesn't. The boundary is inclusive (Instant::now() >= deadline)
    /// because workflows check before each API call and "we're at the
    /// deadline" should bail rather than fire one more request that
    /// will then race the deadline's own enforcement.
    #[test]
    fn workflow_context_deadline_is_past_when_now_is_after() {
        let past = Instant::now() - std::time::Duration::from_millis(1);
        let ctx = WorkflowContext::new(None, Some(past));
        assert!(ctx.is_past_deadline());

        let future = Instant::now() + std::time::Duration::from_secs(60);
        let ctx = WorkflowContext::new(None, Some(future));
        assert!(!ctx.is_past_deadline());
    }

    /// The footprint accumulates IDs and lets workflows declare their
    /// side effects. The MCP wrapper applies them post-call.
    #[test]
    fn mutation_footprint_records_invalidations() {
        let mut fp = MutationFootprint::new();
        fp.invalidate_node("node-a");
        fp.invalidate_cache_only("node-b");
        assert_eq!(fp.invalidated_nodes, vec!["node-a", "node-b"]);
        assert_eq!(fp.invalidated_name_index, vec!["node-a"]);

        let mut other = MutationFootprint::new();
        other.invalidate_node("node-c");
        fp.extend(other);
        assert_eq!(fp.invalidated_nodes, vec!["node-a", "node-b", "node-c"]);
    }

    /// Empty payload is a zero-cost pass-through: the workflow never
    /// touches the API, returns Complete with zero creates, and emits
    /// no footprint. Pinned because both surfaces used to handle the
    /// empty case inline; lifting must preserve the no-op contract.
    #[tokio::test]
    async fn insert_content_empty_payload_is_zero_cost_complete() {
        let client = WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds");
        let ctx = WorkflowContext::default();
        let (outcome, footprint) =
            insert_content_via_indented(&client, Some("any"), Vec::new(), &ctx)
                .await
                .expect("empty payload succeeds without API call");
        match outcome {
            InsertContentOutcome::Complete { created_count, .. } => {
                assert_eq!(created_count, 0);
            }
            other => panic!("empty payload must be Complete; got {other:?}"),
        }
        assert!(
            footprint.invalidated_nodes.is_empty(),
            "empty payload must not invalidate anything: {footprint:?}"
        );
    }

    /// Above the hard cap, the workflow returns InvalidInput so the
    /// MCP wrapper produces an `invalid_params` envelope and the CLI
    /// prints a clear message — not a partial-success payload, not a
    /// silent transport drop.
    #[tokio::test]
    async fn insert_content_above_cap_returns_invalid_input() {
        let client = WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds");
        let ctx = WorkflowContext::default();
        let lines: Vec<ParsedLine> = (0..(defaults::MAX_INSERT_CONTENT_LINES + 1))
            .map(|i| ParsedLine {
                text: format!("line {i}"),
                indent: 0,
            })
            .collect();
        let err = insert_content_via_indented(&client, Some("any"), lines, &ctx)
            .await
            .expect_err("above-cap payload must reject");
        assert!(
            matches!(err, WorkflowyError::InvalidInput { .. }),
            "must surface as InvalidInput: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(&defaults::MAX_INSERT_CONTENT_LINES.to_string()),
            "rejection must name the cap: {msg}"
        );
    }

    /// A pre-cancelled context bails out before the first API call,
    /// returning `Partial { reason: Cancelled, created_count: 0, ... }`.
    /// Demonstrates that the workflow honours `ctx.is_cancelled()`
    /// uniformly between iterations.
    #[tokio::test]
    async fn insert_content_pre_cancelled_returns_partial_zero() {
        let client = WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds");
        let registry = crate::utils::cancel::CancelRegistry::new();
        let guard = registry.guard();
        registry.cancel_all();
        let ctx = WorkflowContext::new(Some(&guard), None);
        let parsed = parse_indented_content("a\nb\nc");
        let (outcome, footprint) =
            insert_content_via_indented(&client, Some("any"), parsed, &ctx)
                .await
                .expect("pre-cancelled returns Ok(Partial)");
        match outcome {
            InsertContentOutcome::Partial {
                reason,
                created_count,
                total_count,
                ..
            } => {
                assert!(matches!(reason, PartialReason::Cancelled));
                assert_eq!(created_count, 0);
                assert_eq!(total_count, 3);
            }
            other => panic!("must surface as Partial cancelled; got {other:?}"),
        }
        // No creates → no parent invalidation needed beyond cache hint.
        assert_eq!(footprint.invalidated_nodes, vec!["any"]);
    }

    /// A past deadline bails out before the first API call, returning
    /// `Partial { reason: Timeout, ... }`.
    #[tokio::test]
    async fn insert_content_past_deadline_returns_partial_timeout() {
        let client = WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds");
        let past = Instant::now() - std::time::Duration::from_millis(1);
        let ctx = WorkflowContext::new(None, Some(past));
        let parsed = parse_indented_content("a\nb");
        let (outcome, _footprint) = insert_content_via_indented(
            &client,
            None, // workspace root
            parsed,
            &ctx,
        )
        .await
        .expect("past-deadline returns Ok(Partial)");
        match outcome {
            InsertContentOutcome::Partial {
                reason,
                created_count,
                ..
            } => {
                assert!(matches!(reason, PartialReason::Timeout));
                assert_eq!(created_count, 0);
            }
            other => panic!("must surface as Partial timeout; got {other:?}"),
        }
    }

    /// Write-path report Recommendation D (2026-06-17): a hard mid-batch API
    /// error must NOT discard the lines already committed. It returns
    /// `Ok(Partial { reason: Error, .. })` carrying the accumulated
    /// `created_count` + `last_inserted_id` + the underlying error string,
    /// instead of the pre-fix `return Err(e)` that left the caller unable to
    /// tell what landed without a separate read. First line creates OK;
    /// second line's create returns 500 → the batch stops with one commit
    /// recorded.
    #[tokio::test]
    async fn insert_content_hard_error_returns_partial_with_committed_count() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        // Line "a" → 200 with an id.
        Mock::given(method("POST"))
            .and(path("/nodes"))
            .and(body_partial_json(serde_json::json!({"name": "a"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"item_id": "00000000-0000-0000-0000-0000000000aa"})),
            )
            .mount(&mock)
            .await;
        // Line "b" → 500 (hard error), no retry (fast_retry max_attempts=1).
        Mock::given(method("POST"))
            .and(path("/nodes"))
            .and(body_partial_json(serde_json::json!({"name": "b"})))
            .respond_with(ResponseTemplate::new(500).set_body_string("backend boom"))
            .mount(&mock)
            .await;

        let client = WorkflowyClient::new_with_configs(
            mock.uri(),
            "test-key".to_string(),
            crate::config::RetryConfig {
                max_attempts: 1,
                base_delay_ms: 10,
                max_delay_ms: 20,
                retryable_statuses: defaults::RETRY_STATUSES,
            },
            crate::config::RateLimitConfig {
                requests_per_second: 200,
                burst_size: 100,
            },
        )
        .expect("client builds against mock");

        let parsed = parse_indented_content("a\nb");
        let ctx = WorkflowContext::default();
        let (outcome, _footprint) = insert_content_via_indented(&client, None, parsed, &ctx)
            .await
            .expect("hard error must surface as Ok(Partial), not Err");

        match outcome {
            InsertContentOutcome::Partial {
                reason,
                created_count,
                total_count,
                last_inserted_id,
                error,
                ..
            } => {
                assert!(matches!(reason, PartialReason::Error), "reason must be Error");
                assert_eq!(created_count, 1, "the one committed line must be reported");
                assert_eq!(total_count, 2);
                assert_eq!(
                    last_inserted_id.as_deref(),
                    Some("00000000-0000-0000-0000-0000000000aa"),
                    "resume cursor must point at the last committed node"
                );
                assert!(
                    error.is_some(),
                    "the underlying error must travel so the surface can classify it"
                );
            }
            other => panic!("must surface as Partial error; got {other:?}"),
        }
    }

    /// `BulkOp::parse` accepts the exact wire strings both surfaces
    /// passed pre-lift; unknown kinds return None so the wrapper can
    /// surface InvalidInput. Pinned because the wire-string set is
    /// the public contract.
    #[test]
    fn bulk_op_parse_accepts_exact_wire_strings_only() {
        for (s, expected) in [
            ("delete", BulkOp::Delete),
            ("complete", BulkOp::Complete),
            ("uncomplete", BulkOp::Uncomplete),
            ("add_tag", BulkOp::AddTag),
            ("remove_tag", BulkOp::RemoveTag),
        ] {
            assert_eq!(BulkOp::parse(s), Some(expected));
            assert_eq!(BulkOp::parse(s).unwrap().as_str(), s);
        }
        assert!(BulkOp::parse("frobnicate").is_none());
        assert!(BulkOp::parse("").is_none());
        assert!(BulkOp::parse("DELETE").is_none(), "case-sensitive");
    }

    /// `apply_bulk_op` rejects add_tag/remove_tag without an
    /// `operation_tag` argument. The check fires before any API call,
    /// so the workflow returns InvalidInput; the wrappers translate
    /// to their own envelopes.
    #[tokio::test]
    async fn apply_bulk_op_rejects_tag_op_without_operation_tag() {
        let client = WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds");
        let ctx = WorkflowContext::default();
        let nodes: Vec<WorkflowyNode> = Vec::new();
        let err = apply_bulk_op(&client, BulkOp::AddTag, &nodes, None, &ctx)
            .await
            .expect_err("add_tag without operation_tag must reject");
        assert!(
            matches!(err, WorkflowyError::InvalidInput { .. }),
            "must surface as InvalidInput: {err:?}"
        );

        // remove_tag has the same requirement.
        let err = apply_bulk_op(&client, BulkOp::RemoveTag, &nodes, None, &ctx)
            .await
            .expect_err("remove_tag without operation_tag must reject");
        assert!(matches!(err, WorkflowyError::InvalidInput { .. }));

        // Non-tag ops are fine without operation_tag.
        let (result, _fp) = apply_bulk_op(&client, BulkOp::Delete, &nodes, None, &ctx)
            .await
            .expect("delete with empty node list is a no-op");
        assert_eq!(result.matched_count, 0);
        assert_eq!(result.affected_count, 0);
    }

    /// `smart_insert_under_target` rejects empty content with
    /// InvalidInput before touching the API. Pre-lift, the MCP and
    /// CLI both checked this inline; the workflow centralises the
    /// rule.
    #[tokio::test]
    async fn smart_insert_under_target_rejects_empty_content() {
        let client = WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds");
        let ctx = WorkflowContext::default();
        for body in ["", "   ", "\n\n\n"] {
            let err = smart_insert_under_target(&client, "550e8400-e29b-41d4-a716-446655440000", body, &ctx)
                .await
                .expect_err("empty body must reject");
            assert!(
                matches!(err, WorkflowyError::InvalidInput { .. }),
                "must surface as InvalidInput for body {body:?}: {err:?}"
            );
        }
    }

    /// Indented parsing matches the rules both surfaces relied on:
    /// 2-space-per-level indent, empty lines dropped, leading/trailing
    /// whitespace trimmed off the text.
    #[test]
    fn parse_indented_content_handles_indents_and_blanks() {
        let parsed = parse_indented_content(
            "Top\n  Child\n    Grandchild\n\nSecond top\n  Its child\n",
        );
        assert_eq!(parsed.len(), 5);
        assert_eq!(parsed[0].text, "Top");
        assert_eq!(parsed[0].indent, 0);
        assert_eq!(parsed[1].text, "Child");
        assert_eq!(parsed[1].indent, 1);
        assert_eq!(parsed[2].indent, 2);
        assert_eq!(parsed[3].text, "Second top");
        assert_eq!(parsed[3].indent, 0);
    }

    // -----------------------------------------------------------------
    // reorder_nodes_via_priority — validation pinned without API touch
    // -----------------------------------------------------------------

    fn test_client() -> WorkflowyClient {
        WorkflowyClient::new(
            "http://invalid.local".to_string(),
            "test-key".to_string(),
        )
        .expect("test client builds")
    }

    /// Empty `node_ids` is rejected with `InvalidInput` before any API
    /// touch. Pinned because both the MCP envelope and the CLI error
    /// message rely on the workflow surfacing this as `InvalidInput`
    /// rather than a network-shaped error.
    #[tokio::test]
    async fn reorder_nodes_rejects_empty_list_without_api_call() {
        let client = test_client();
        let ctx = WorkflowContext::default();
        let err = reorder_nodes_via_priority(&client, "parent-uuid", &[], &ctx)
            .await
            .expect_err("empty node_ids must reject");
        assert!(
            matches!(err, WorkflowyError::InvalidInput { .. }),
            "empty node_ids must surface as InvalidInput, got: {err:?}"
        );
    }

    /// Duplicate ids are rejected with `InvalidInput` and a message
    /// that names the duplicated id, before any API touch. Re-moving
    /// the same node within one call is a no-op slow path; refusing
    /// it up-front keeps the contract honest.
    #[tokio::test]
    async fn reorder_nodes_rejects_duplicates_without_api_call() {
        let client = test_client();
        let ctx = WorkflowContext::default();
        let ids = vec![
            "node-a".to_string(),
            "node-b".to_string(),
            "node-a".to_string(),
        ];
        let err = reorder_nodes_via_priority(&client, "parent-uuid", &ids, &ctx)
            .await
            .expect_err("duplicate ids must reject");
        let WorkflowyError::InvalidInput { reason } = err else {
            panic!("duplicates must surface as InvalidInput");
        };
        assert!(
            reason.contains("duplicate") && reason.contains("node-a"),
            "validation message must name the duplicated id: {reason}"
        );
    }

    /// `node_ids` containing the parent id is rejected up-front so the
    /// caller gets a clear "node cannot be its own child" message
    /// rather than the upstream's downstream symptom.
    #[tokio::test]
    async fn reorder_nodes_rejects_parent_in_list_without_api_call() {
        let client = test_client();
        let ctx = WorkflowContext::default();
        let ids = vec!["node-a".to_string(), "parent-uuid".to_string()];
        let err = reorder_nodes_via_priority(&client, "parent-uuid", &ids, &ctx)
            .await
            .expect_err("parent in node_ids must reject");
        assert!(
            matches!(err, WorkflowyError::InvalidInput { .. }),
            "parent-as-child must surface as InvalidInput, got: {err:?}"
        );
    }

    /// `node_ids.len() > MAX_REORDER_NODES` is rejected up-front with
    /// a chunking instruction. Pinned so the cap is visible at the
    /// workflow boundary, matching the way `insert_content` exposes
    /// its own line cap.
    #[tokio::test]
    async fn reorder_nodes_rejects_oversized_list_without_api_call() {
        let client = test_client();
        let ctx = WorkflowContext::default();
        let ids: Vec<String> = (0..(defaults::MAX_REORDER_NODES + 1))
            .map(|i| format!("node-{i:04}"))
            .collect();
        let err = reorder_nodes_via_priority(&client, "parent-uuid", &ids, &ctx)
            .await
            .expect_err("oversized node_ids must reject");
        let WorkflowyError::InvalidInput { reason } = err else {
            panic!("oversize must surface as InvalidInput");
        };
        assert!(
            reason.contains("exceeds")
                && reason.contains(&defaults::MAX_REORDER_NODES.to_string()),
            "validation message must surface the cap: {reason}"
        );
    }

    /// A pre-cancelled context bails out before the first move, so
    /// every id is `Skipped` and the outcome is `Partial { reason: cancelled }`.
    /// No API call required to test this path because the cancel check
    /// is the very first thing each iteration does. Pinned so the
    /// partial-shape contract is callable from both surfaces with no
    /// upstream needed.
    #[tokio::test]
    async fn reorder_nodes_pre_cancelled_returns_partial_all_skipped() {
        use crate::utils::cancel::CancelRegistry;
        let client = test_client();
        let registry = CancelRegistry::new();
        let guard = registry.guard();
        registry.cancel_all();
        let ctx = WorkflowContext::new(Some(&guard), None);

        let ids = vec!["node-a".to_string(), "node-b".to_string(), "node-c".to_string()];
        let (outcome, footprint) =
            reorder_nodes_via_priority(&client, "parent-uuid", &ids, &ctx)
                .await
                .expect("pre-cancel returns Ok with Partial outcome");

        match outcome {
            ReorderOutcome::Partial {
                reason,
                attempted,
                succeeded,
                failed,
                skipped,
                results,
                ..
            } => {
                assert!(matches!(reason, ReorderPartialReason::Cancelled));
                assert_eq!(attempted, 0);
                assert_eq!(succeeded, 0);
                assert_eq!(failed, 0);
                assert_eq!(skipped, 3);
                assert_eq!(results.len(), 3);
                for entry in &results {
                    assert!(matches!(entry, ReorderEntry::Skipped { .. }));
                }
            }
            ReorderOutcome::Complete { .. } => {
                panic!("pre-cancel must yield Partial, not Complete");
            }
        }
        // Footprint is declared up front (parent + every id) regardless
        // of whether the moves ran — the wrapper invalidates aggressively
        // because a partial run may have touched some nodes already.
        assert!(footprint.invalidated_nodes.contains(&"parent-uuid".to_string()));
        assert!(footprint.invalidated_name_index.contains(&"node-a".to_string()));
    }

    /// A past deadline bails out the same way as a pre-cancel, but
    /// surfaces `ReorderPartialReason::Timeout`. Pinned because the
    /// two reasons map to different recovery hints in the response
    /// envelope and a regression flipping the variants would mask
    /// the wrong cause to the caller.
    #[tokio::test]
    async fn reorder_nodes_past_deadline_returns_partial_timeout() {
        let client = test_client();
        let past = Instant::now() - std::time::Duration::from_millis(1);
        let ctx = WorkflowContext::new(None, Some(past));

        let ids = vec!["node-a".to_string()];
        let (outcome, _) =
            reorder_nodes_via_priority(&client, "parent-uuid", &ids, &ctx)
                .await
                .expect("past-deadline returns Ok with Partial outcome");

        match outcome {
            ReorderOutcome::Partial { reason, skipped, .. } => {
                assert!(matches!(reason, ReorderPartialReason::Timeout));
                assert_eq!(skipped, 1);
            }
            ReorderOutcome::Complete { .. } => {
                panic!("past-deadline must yield Partial, not Complete");
            }
        }
    }

    /// `extract_unresolved_mirror_targets` is pure: it walks the node
    /// set, finds `mirror_of:` markers whose target UUID is not in
    /// scope, and returns the unique unresolved set. The MCP server
    /// uses its name index to resolve each one; the CLI issues a
    /// live `get_node`. Both surfaces share this extraction so the
    /// "not in scope" rule cannot drift.
    #[test]
    fn extract_unresolved_mirror_targets_finds_external_uuids_only() {
        let mut canonical = WorkflowyNode::default();
        canonical.id = "canonical-in-scope".to_string();
        canonical.name = "Canonical".to_string();

        let mut mirror_local = WorkflowyNode::default();
        mirror_local.id = "mirror-local".to_string();
        mirror_local.description = Some("mirror_of: canonical-in-scope".to_string());

        let mut mirror_external_a = WorkflowyNode::default();
        mirror_external_a.id = "mirror-ext-a".to_string();
        mirror_external_a.description = Some("mirror_of: external-uuid-A".to_string());

        let mut mirror_external_b = WorkflowyNode::default();
        mirror_external_b.id = "mirror-ext-b".to_string();
        mirror_external_b.description = Some("mirror_of: external-uuid-B".to_string());

        // Duplicate marker for A — should dedupe in the result.
        let mut mirror_external_a_dup = WorkflowyNode::default();
        mirror_external_a_dup.id = "mirror-ext-a-dup".to_string();
        mirror_external_a_dup.description = Some("mirror_of: external-uuid-A".to_string());

        let nodes = vec![
            canonical,
            mirror_local,
            mirror_external_a,
            mirror_external_b,
            mirror_external_a_dup,
        ];

        let targets = extract_unresolved_mirror_targets(&nodes);
        assert_eq!(targets.len(), 2, "in-scope target and dupe both excluded");
        assert!(targets.contains(&"external-uuid-a".to_string()));
        assert!(targets.contains(&"external-uuid-b".to_string()));
    }

    /// The audit walk's chunked vs single behaviour is mostly an
    /// orchestration over `client.get_subtree_with_controls`, so the
    /// unit test focuses on the per-bucket extraction logic which is
    /// the only part this layer can verify without a live API. The
    /// end-to-end behaviour (chunked walk includes the root, dedupes
    /// across chunks, surfaces per-chunk envelope) is exercised by
    /// the live-integration test in `tests/live_insert.rs` when
    /// `WORKFLOWY_API_KEY` is set.
    #[test]
    fn extract_unresolved_mirror_targets_end_matches_short_hashes() {
        // Full-UUID node in scope; mirror references the trailing
        // 12-char short hash. The end-match rule means the target
        // should be considered in-scope and NOT returned.
        let full = "550e8400-e29b-41d4-a716-446655440000";
        let short = &full[full.len() - 12..]; // "446655440000"

        let mut canonical = WorkflowyNode::default();
        canonical.id = full.to_string();

        let mut mirror = WorkflowyNode::default();
        mirror.id = "mirror".to_string();
        mirror.description = Some(format!("mirror_of: {}", short));

        let targets = extract_unresolved_mirror_targets(&[canonical, mirror]);
        assert!(
            targets.is_empty(),
            "short-hash mirror targeting an in-scope full UUID must be considered resolved",
        );
    }

    // --- resolve_link lift (2026-05-22) ---

    #[test]
    fn find_node_by_short_hash_matches_full_uuid_and_trailing_form() {
        let mut n = WorkflowyNode::default();
        n.id = "550e8400-e29b-41d4-a716-446655440000".to_string();
        let nodes = vec![n];
        assert!(
            find_node_by_short_hash(&nodes, "550e8400-e29b-41d4-a716-446655440000").is_some(),
            "full-UUID candidate must match",
        );
        assert!(
            find_node_by_short_hash(&nodes, "446655440000").is_some(),
            "12-char trailing short-hash candidate must match by `ends_with`",
        );
        assert!(
            find_node_by_short_hash(&nodes, "deadbeefcafe").is_none(),
            "non-matching short-hash candidate returns None",
        );
    }

    #[test]
    fn find_node_by_short_hash_is_case_insensitive() {
        let mut n = WorkflowyNode::default();
        n.id = "550E8400-E29B-41D4-A716-446655440000".to_string();
        let nodes = vec![n];
        assert!(
            find_node_by_short_hash(&nodes, "446655440000").is_some(),
            "lowercase candidate must match an uppercase-ID node — match is case-insensitive",
        );
    }

    /// Both surfaces share the hit payload shape. Pre-2026-05-22 the
    /// CLI's hit payload was `{link, node}` while the MCP's was
    /// `{id, name, description, parent_id, resolved_via}`. After the
    /// CLI-as-facade refactor both call this builder; the shape is
    /// now a single source of truth pinned by this test.
    #[test]
    fn build_resolve_link_hit_payload_emits_the_canonical_five_fields() {
        let mut n = WorkflowyNode::default();
        n.id = "550e8400-e29b-41d4-a716-446655440000".to_string();
        n.name = "Hello <b>World</b>".to_string();
        n.description = Some("notes".to_string());
        n.parent_id = Some("parent-uuid".to_string());

        let payload = build_resolve_link_hit_payload(&n, "scoped_walk");
        let obj = payload.as_object().expect("hit payload is an object");
        assert_eq!(obj["resolved_via"], "scoped_walk");
        assert_eq!(obj["id"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(obj["name"], "Hello World", "HTML must be stripped from name");
        assert_eq!(obj["description"], "notes");
        assert_eq!(obj["parent_id"], "parent-uuid");
    }

    /// The miss payload's envelope shape (and the truncation envelope
    /// it embeds) must match across surfaces. This test exercises the
    /// builder directly so any drift on field names, hint text, or
    /// envelope merging surfaces in CI before the CLI and MCP diverge.
    #[test]
    fn build_resolve_link_miss_payload_emits_the_canonical_envelope_shape() {
        let payload = build_resolve_link_miss_payload(
            "abcdef012345",
            "the workspace root",
            123,
            456,
            true,
            Some(TruncationReason::Timeout),
            "primary_walk",
            Some(50_000),
        );
        let obj = payload.as_object().expect("miss payload is an object");

        // Core miss fields.
        assert!(obj["resolved"].is_null(), "miss must carry resolved: null");
        assert_eq!(obj["short_hash"], "abcdef012345");
        assert_eq!(obj["scope"], "the workspace root");
        assert_eq!(obj["nodes_walked"], 123);
        assert_eq!(obj["elapsed_ms"], 456);
        assert_eq!(
            obj["resolved_via"], "primary_walk",
            "discriminator naming the walk path must be present",
        );
        assert_eq!(obj["name_index_size"], 50_000);
        assert!(
            obj["hint"].as_str().unwrap().contains("search_parent_path"),
            "human hint must steer caller to a tighter scope",
        );
        assert!(
            obj["hint"].as_str().unwrap().contains("50000 entries"),
            "when name_index_size is Some, hint mentions the index size so caller can judge dead-link probability: {:?}",
            obj["hint"],
        );

        // Four-field truncation envelope.
        assert_eq!(obj["truncated"], true);
        assert_eq!(obj["truncation_limit"], defaults::RESOLVE_WALK_NODE_CAP as u64);
        assert_eq!(obj["truncation_reason"], "timeout");
        assert!(
            obj["truncation_recovery_hint"]
                .as_str()
                .unwrap()
                .contains("search_parent_path"),
            "tool-specific recovery hint must point at search_parent_path, NOT the generic name-index hint that's misleading for short-hash failures",
        );
    }

    /// When the CLI builds a miss payload it passes `name_index_size:
    /// None` (the CLI has no persistent index). The hint must adjust:
    /// the "persistent index has N entries" sentence must NOT appear,
    /// so the CLI's output doesn't claim a feature it doesn't have.
    #[test]
    fn build_resolve_link_miss_hint_omits_index_clause_when_size_is_none() {
        let hint = build_resolve_link_miss_hint(
            "abcdef012345",
            "the workspace root",
            123,
            456,
            "primary_walk",
            None,
        );
        assert!(
            !hint.contains("persistent name index"),
            "CLI's miss hint (name_index_size=None) must not reference the persistent name index it doesn't have: {hint}",
        );
        assert!(
            hint.contains("search_parent_path"),
            "miss hint must still steer caller to a tighter scope: {hint}",
        );
    }

    // ------------------------------------------------------------------
    // instantiate_template / duplicate_subtree transform tests
    //
    // These exercise the pure substitution transform directly (no
    // client). The canonical (MCP) behaviour is regex `{{var}}` with
    // UNMATCHED variables left intact — the property the CLI's pre-lift
    // literal `str::replace` could not express.
    // ------------------------------------------------------------------

    fn vars(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// A known variable is substituted; an UNKNOWN `{{x}}` is left
    /// verbatim (passthrough). Pinned because the passthrough is the
    /// behavioural improvement the CLI gains from the lift — a literal
    /// replace silently leaves unknown tokens too, but only by accident
    /// of "no rule matched"; the regex form makes passthrough explicit
    /// and total.
    #[test]
    fn substitute_vars_replaces_known_and_passes_through_unknown() {
        let re = template_var_regex().expect("static pattern compiles");
        let v = vars(&[("name", "Alice")]);
        assert_eq!(substitute_vars(&re, &v, "Hi {{name}}"), "Hi Alice");
        // Unknown variable survives verbatim.
        assert_eq!(substitute_vars(&re, &v, "Hi {{name}} {{role}}"), "Hi Alice {{role}}");
        // No variables at all — text is unchanged.
        assert_eq!(substitute_vars(&re, &v, "plain text"), "plain text");
    }

    /// Multiple distinct variables in one string all substitute, and a
    /// repeated variable substitutes every occurrence.
    #[test]
    fn substitute_vars_handles_multi_var_and_repeats() {
        let re = template_var_regex().expect("static pattern compiles");
        let v = vars(&[("a", "1"), ("b", "2")]);
        assert_eq!(
            substitute_vars(&re, &v, "{{a}}-{{b}}-{{a}}"),
            "1-2-1",
        );
    }

    /// The transform [`instantiate_template`] hands to [`deep_copy_subtree`]
    /// substitutes BOTH the name and the description of a node. Verified
    /// by replaying the closure the workflow builds (kept in sync with
    /// the workflow body). The `None` description path is preserved.
    #[test]
    fn template_transform_applies_to_name_and_description() {
        let re = template_var_regex().expect("static pattern compiles");
        let v = vars(&[("who", "Bob")]);
        let transform = |node: &WorkflowyNode| -> (String, Option<String>) {
            let name = substitute_vars(&re, &v, &node.name);
            let desc = node.description.as_ref().map(|d| substitute_vars(&re, &v, d));
            (name, desc)
        };
        let with_desc = WorkflowyNode {
            id: "n1".to_string(),
            name: "Hello {{who}}".to_string(),
            description: Some("note for {{who}} and {{unknown}}".to_string()),
            parent_id: None,
            ..Default::default()
        };
        let (name, desc) = transform(&with_desc);
        assert_eq!(name, "Hello Bob");
        assert_eq!(desc.as_deref(), Some("note for Bob and {{unknown}}"));

        let no_desc = WorkflowyNode {
            id: "n2".to_string(),
            name: "{{who}}'s task".to_string(),
            description: None,
            parent_id: None,
            ..Default::default()
        };
        let (name, desc) = transform(&no_desc);
        assert_eq!(name, "Bob's task");
        assert_eq!(desc, None);
    }

    /// The duplicate transform prepends `name_prefix` to the ROOT only
    /// (id-matched), leaving descendants untouched and never touching
    /// descriptions. Replays the closure [`duplicate_subtree`] builds.
    #[test]
    fn duplicate_transform_prefixes_root_only() {
        let root_id = "root-1".to_string();
        let prefix = Some("Copy of ".to_string());
        let transform = |node: &WorkflowyNode| -> (String, Option<String>) {
            let name = if node.id == root_id {
                match &prefix {
                    Some(p) => format!("{}{}", p, node.name),
                    None => node.name.clone(),
                }
            } else {
                node.name.clone()
            };
            (name, node.description.clone())
        };
        let root = WorkflowyNode {
            id: "root-1".to_string(),
            name: "Project".to_string(),
            description: Some("keep me".to_string()),
            parent_id: None,
            ..Default::default()
        };
        let child = WorkflowyNode {
            id: "child-1".to_string(),
            name: "Task".to_string(),
            description: None,
            parent_id: Some("root-1".to_string()),
            ..Default::default()
        };
        assert_eq!(transform(&root), ("Copy of Project".to_string(), Some("keep me".to_string())));
        assert_eq!(transform(&child), ("Task".to_string(), None));
    }
}
