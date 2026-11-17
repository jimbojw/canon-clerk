// Copyright 2026 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Shared closure workspace materialization, metadata merging, and `validate` resolution helpers.

use crate::canon::ParseCanonOptions;
use crate::caseload::{Caseload, CaseloadIntake};
use crate::cli::CliError;
use crate::cli::args::{CanonSelectionArgs, ProviderConfigArgs};
use crate::cli::provenance::StageDisposition;
use crate::cli::runners::map_discover_error;
use crate::discover::{DiscoverOptions, WorkspaceSnapshot, execute_discover};
use crate::intake::{IntakeError, IntakeOptions, execute_intake};
use crate::provider::StructuredModelProvider;
use crate::telemetry::{PipelineStage, TelemetrySession};
use crate::validate::{ValidateError, ValidateOptions, execute_validate};
use std::fs;
use std::path::Path;

/// Execution backend for converged CLI closures (`apprise`, `docket`, `admit`, `audit`).
pub(crate) enum ClosureBackend<'a> {
    /// Reads workspace files from `workspace_root` on disk and invokes live `GeminiModelProvider`.
    LiveDisk { workspace_root: &'a Path },
    /// Evaluates 100% hermetically in-memory against `workspace` and injected `provider`.
    Hermetic {
        workspace: &'a WorkspaceSnapshot,
        provider: &'a dyn StructuredModelProvider,
    },
}

/// Returns `true` if any explicit CLI provider or model override flag was passed.
pub(crate) fn has_provider_overrides(provider: &ProviderConfigArgs) -> bool {
    provider.screener_model.is_some()
        || provider.auditor_model.is_some()
        || provider.thinking_level.is_some()
        || provider.reasoning_budget.is_some()
        || provider.gemini_api_key.is_some()
        || provider.typesafe_api_key.is_some()
        || provider.local_decision_endpoint.is_some()
}

/// Merges supplementary CLI metadata (`intent`, `pr_title`, `pr_body`) onto an existing [`CaseloadIntake`].
pub(crate) fn merge_metadata_onto_intake(
    mut base: CaseloadIntake,
    opts: &IntakeOptions,
) -> CaseloadIntake {
    if let Some(intent) = opts
        .intent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        base.intent = Some(intent.to_string());
    }
    if let Some(title) = opts
        .pr_title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        base.pr_title = Some(title.to_string());
    }
    if let Some(body) = opts
        .pr_body
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        base.pr_body = Some(body.to_string());
    }
    base
}

/// Materializes an in-memory [`WorkspaceSnapshot`] from `backend`, backfilling any pre-populated
/// candidate canons referenced in `caseload.discovery`.
pub(crate) fn materialize_closure_snapshot(
    backend: &ClosureBackend<'_>,
    canon_patterns: &[String],
    caseload: &Caseload,
) -> Result<WorkspaceSnapshot, CliError> {
    match backend {
        ClosureBackend::Hermetic { workspace, .. } => Ok((*workspace).clone()),
        ClosureBackend::LiveDisk { workspace_root } => {
            let mut snapshot = WorkspaceSnapshot::from_disk(workspace_root, canon_patterns)
                .map_err(map_discover_error)?;
            if let Some(disc) = &caseload.discovery {
                for canon_ref in &disc.candidate_canons {
                    if !snapshot.canons.contains_key(canon_ref) {
                        let full_path = workspace_root.join(canon_ref);
                        if full_path.is_file()
                            && let Ok(raw) = fs::read_to_string(&full_path)
                        {
                            let opts = ParseCanonOptions::with_source_ref(canon_ref.clone());
                            snapshot = snapshot.with_canon_markdown(&raw, &opts);
                        }
                    }
                }
            }
            Ok(snapshot)
        }
    }
}

/// Executes `intake` within a contentious cascade (`vet`, `audit`), fast-forwarding if `.intake` is pre-populated.
pub(crate) fn resolve_cascade_intake(
    stage_name: &str,
    intake_opts: &IntakeOptions,
    incoming: Option<Caseload>,
    telemetry: &TelemetrySession<'_>,
) -> Result<(Caseload, StageDisposition, bool), CliError> {
    if !intake_opts.has_designated_source() {
        let existing = incoming.ok_or_else(|| {
            CliError::usage(format!(
                "Missing upstream caseload or filing inputs for `{stage_name}`."
            ))
        })?;
        return Ok((existing, StageDisposition::PrePopulated, false));
    }

    telemetry.run_stage(PipelineStage::Intake, |_scope| {
        execute_cascade_intake_body(intake_opts, incoming)
    })
}

fn execute_cascade_intake_body(
    intake_opts: &IntakeOptions,
    mut incoming: Option<Caseload>,
) -> Result<(Caseload, StageDisposition, bool), CliError> {
    let cli_has_targets = intake_opts.all_targets
        || intake_opts.patch_content.is_some()
        || intake_opts.target_paths.is_some();

    if !cli_has_targets
        && let Some(existing) = incoming.as_mut()
        && let Some(prev_intake) = existing.intake.take()
    {
        let pr_meta_added = intake_opts
            .pr_title
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            || intake_opts
                .pr_body
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty());
        let merged = merge_metadata_onto_intake(prev_intake, intake_opts);
        existing.docket = None;
        existing.evidence = None;
        existing.vetting = None;
        existing.verdict = None;
        existing.intake = Some(merged);
        if pr_meta_added {
            existing.discovery = None;
            existing.validation = None;
        }
        return Ok((existing.clone(), StageDisposition::Executed, pr_meta_added));
    }

    let mut updated = execute_intake(intake_opts, incoming).map_err(|err| match err {
        IntakeError::MissingInputSource | IntakeError::MalformedDiff(_) => {
            CliError::usage(err.to_string())
        }
    })?;
    updated.discovery = None;
    updated.validation = None;
    updated.docket = None;
    updated.evidence = None;
    updated.vetting = None;
    updated.verdict = None;
    Ok((updated, StageDisposition::Executed, true))
}

/// Executes `discover` within a contentious cascade (`vet`, `audit`), fast-forwarding if `.discovery` is pre-populated.
pub(crate) fn resolve_cascade_discover(
    canons: &CanonSelectionArgs,
    mut caseload: Caseload,
    snapshot: &WorkspaceSnapshot,
    intake_changed: bool,
    telemetry: &TelemetrySession<'_>,
) -> Result<(Caseload, StageDisposition), CliError> {
    if caseload.discovery.is_some() && !intake_changed && canons.is_empty() {
        return Ok((caseload, StageDisposition::PrePopulated));
    }

    caseload.discovery = None;
    caseload.validation = None;
    caseload.docket = None;
    caseload.evidence = None;
    caseload.vetting = None;
    caseload.verdict = None;

    let discover_opts = DiscoverOptions {
        all_canons: canons.all_canons,
        canon_patterns: canons.canon.clone(),
    };

    let updated = telemetry.run_stage(PipelineStage::Discover, |_scope| {
        execute_discover(&discover_opts, caseload, snapshot).map_err(map_discover_error)
    })?;
    Ok((updated, StageDisposition::Executed))
}

/// Executes `validate` within a converged closure, fast-forwarding if `.validation` is already populated.
pub(crate) fn resolve_closure_validate(
    caseload: Caseload,
    snapshot: &WorkspaceSnapshot,
    telemetry: &TelemetrySession<'_>,
) -> Result<(Caseload, StageDisposition), CliError> {
    if caseload.validation.is_some() {
        return Ok((caseload, StageDisposition::PrePopulated));
    }
    let updated = telemetry.run_stage(PipelineStage::Validate, |_scope| {
        execute_validate(&ValidateOptions::default(), caseload, snapshot).map_err(|err| match err {
            ValidateError::MissingDiscovery => CliError::usage(err.to_string()),
            ValidateError::Discover(d) => map_discover_error(d),
        })
    })?;
    Ok((updated, StageDisposition::Executed))
}
