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
use crate::cli::args::ProviderConfigArgs;
use crate::cli::provenance::StageDisposition;
use crate::cli::runners::map_discover_error;
use crate::discover::WorkspaceSnapshot;
use crate::intake::IntakeOptions;
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
        || provider.reasoning_budget.is_some()
        || provider.gemini_api_key.is_some()
        || provider.typesafe_api_key.is_some()
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
