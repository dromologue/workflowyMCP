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
//! Every workflow function takes:
//! 1. `client: &WorkflowyClient` — the shared HTTP layer.
//! 2. The typed inputs the operation needs (resolved IDs preferred).
//! 3. `ctx: &WorkflowContext<'_>` — optional cancel guard + deadline.
//!    The MCP passes the active server context; the CLI passes
//!    `WorkflowContext::default()`. Workflows that don't need
//!    mid-orchestration cancel/deadline ignore the field.
//!
//! And returns:
//! - `Ok((TypedResult, MutationFootprint))` on success.
//! - `Err(WorkflowyError)` on failure, with `InvalidInput` reserved for
//!   caller-supplied parameter problems (mapped to MCP
//!   `tool_invalid_params` envelopes by the wrapper).

use std::time::Instant;

use serde::Serialize;
use serde_json::json;
use tracing::error;

use crate::api::WorkflowyClient;
use crate::audit::extract_marker;
use crate::defaults;
use crate::error::{Result, WorkflowyError};
use crate::types::WorkflowyNode;
use crate::utils::cancel::CancelGuard;

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
    if let Some(parent) = target_parent_id {
        if parent == canonical_id {
            return Err(WorkflowyError::InvalidInput {
                reason: "target_parent_id cannot equal canonical_node_id — \
                         a node cannot mirror itself into its own subtree"
                    .to_string(),
            });
        }
    }

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
    if let Some(p) = pillar.map(str::trim).filter(|s| !s.is_empty()) {
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
/// - [`InsertContentOutcome::Partial`] — the workflow bailed because
///   the cancel guard flipped or the deadline passed; the outcome
///   carries enough state for the caller to resume from
///   `last_inserted_id`.
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
    },
}

/// Why an `insert_content` call returned partial success rather than
/// complete. Maps 1:1 to the cancel/deadline signals the workflow
/// observes between API calls.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PartialReason {
    Cancelled,
    Timeout,
}

impl PartialReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PartialReason::Cancelled => "cancelled",
            PartialReason::Timeout => "timeout",
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
                error!(error = %e, line = %line.text, "Failed to insert line");
                if let Some(pid) = parent_id {
                    footprint.invalidate_cache_only(pid);
                }
                return Err(e);
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
    for node in nodes {
        let success = match op {
            BulkOp::Delete => client.delete_node(&node.id).await.is_ok(),
            BulkOp::Complete => client.set_completion(&node.id, true).await.is_ok(),
            BulkOp::Uncomplete => client.set_completion(&node.id, false).await.is_ok(),
            BulkOp::AddTag => {
                let tag = operation_tag.expect("validated by requires_tag check above");
                let new_name = format!("{} #{}", node.name, tag.trim_start_matches('#'));
                client.edit_node(&node.id, Some(&new_name), None).await.is_ok()
            }
            BulkOp::RemoveTag => {
                let tag = operation_tag
                    .expect("validated by requires_tag check above")
                    .trim_start_matches('#');
                let pat = regex::Regex::new(&format!(r"\s*#{}(?:\b|$)", regex::escape(tag)))
                    .expect("escaped pattern is always valid regex");
                let new_name = pat.replace_all(&node.name, "").to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
