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

//! Guarded Branch A and JIT Branch B execution steps for `canon-clerk apprise`.

use super::AppriseArgs;
use super::guards::{resolve_apprise_batch_size, resolve_apprise_concurrency};
use crate::apprise::{AppriseOptions, execute_apprise};
use crate::caseload::{Caseload, IntakeScope};
use crate::cli::CliError;
use crate::cli::args::CanonSelectionArgs;
use crate::cli::closure::{ClosureBackend, merge_metadata_onto_intake};
use crate::cli::provenance::StageDisposition;
use crate::cli::runners::{
    build_live_model_provider, map_discover_error, resolve_branch_b_options, run_branch_b_configure,
};
use crate::discover::{DiscoverOptions, WorkspaceSnapshot, execute_discover};
use crate::intake::{IntakeError, IntakeOptions, execute_intake};
use crate::provider::DEFAULT_GENERATION_TIMEOUT_MS;
use crate::telemetry::{PipelineStage, TelemetrySession};
use std::collections::BTreeMap;

pub(super) fn resolve_apprise_intake(
    intake_opts: &IntakeOptions,
    incoming: Option<Caseload>,
    telemetry: &TelemetrySession<'_>,
) -> Result<(Caseload, StageDisposition, bool), CliError> {
    if !intake_opts.has_designated_source() {
        let existing = incoming.ok_or_else(|| {
            CliError::usage("Missing upstream caseload or filing inputs for `apprise`.")
        })?;
        return Ok((existing, StageDisposition::PrePopulated, false));
    }

    telemetry.run_stage(PipelineStage::Intake, |_scope| {
        execute_apprise_intake_body(intake_opts, incoming)
    })
}

fn execute_apprise_intake_body(
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
        let merged = merge_metadata_onto_intake(prev_intake, intake_opts);
        existing.apprisal = None;
        existing.intake = Some(merged);
        return Ok((existing.clone(), StageDisposition::Executed, false));
    }

    let mut effective_opts = intake_opts.clone();
    if effective_opts.intent.is_none() {
        effective_opts.intent = incoming
            .as_ref()
            .and_then(|c| c.intake.as_ref())
            .and_then(|i| i.intent.clone());
    }

    let mut updated = execute_intake(&effective_opts, incoming).map_err(|err| match err {
        IntakeError::MissingInputSource | IntakeError::MalformedDiff(_) => {
            CliError::usage(err.to_string())
        }
    })?;
    if cli_has_targets {
        updated.discovery = None;
        updated.validation = None;
        updated.apprisal = None;
    }
    Ok((updated, StageDisposition::Executed, cli_has_targets))
}

pub(super) fn resolve_apprise_discover(
    canons: &CanonSelectionArgs,
    mut caseload: Caseload,
    snapshot: &WorkspaceSnapshot,
    target_changed: bool,
    telemetry: &TelemetrySession<'_>,
) -> Result<(Caseload, StageDisposition), CliError> {
    if caseload.discovery.is_some() && !target_changed && canons.is_empty() {
        return Ok((caseload, StageDisposition::PrePopulated));
    }

    caseload.discovery = None;
    caseload.validation = None;
    caseload.apprisal = None;

    let intake_has_targets = caseload.intake.as_ref().is_some_and(|i| {
        i.scope == IntakeScope::AllTargets || !i.target_paths.is_empty() || !i.diffs.is_empty()
    });
    let discover_opts = DiscoverOptions {
        all_canons: canons.all_canons || (!intake_has_targets && !canons.canon.is_empty()),
        canon_patterns: canons.canon.clone(),
    };

    let updated = telemetry.run_stage(PipelineStage::Discover, |_scope| {
        execute_discover(&discover_opts, caseload, snapshot).map_err(map_discover_error)
    })?;
    Ok((updated, StageDisposition::Executed))
}

pub(super) fn execute_jit_configure_and_apprise(
    args: &AppriseArgs,
    env: &BTreeMap<String, String>,
    threshold: f64,
    mut caseload: Caseload,
    snapshot: &WorkspaceSnapshot,
    backend: &ClosureBackend<'_>,
    telemetry: &TelemetrySession<'_>,
) -> Result<(Caseload, StageDisposition), CliError> {
    let configure_options = resolve_branch_b_options(&args.provider, env)?;
    let max_canons_per_call = resolve_apprise_batch_size(
        args.apprise_batch_size,
        env,
        configure_options.stored_config.as_ref(),
    )?;
    let max_concurrent_calls = resolve_apprise_concurrency(
        args.apprise_concurrency,
        env,
        configure_options.stored_config.as_ref(),
    )?;
    let config_was_present =
        caseload.config.is_some() && !configure_options.has_explicit_overrides();
    caseload.apprisal = None;

    let configured = run_branch_b_configure(&configure_options, Some(caseload), telemetry)?;
    let cfg_snapshot = configured.config.clone().unwrap_or_default();
    let live_provider = match backend {
        ClosureBackend::Hermetic { .. } => None,
        ClosureBackend::LiveDisk { .. } => Some(build_live_model_provider(
            &configure_options,
            &cfg_snapshot,
        )?),
    };
    let apprise_opts = AppriseOptions {
        threshold,
        timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
        max_canons_per_call,
        max_concurrent_calls,
    };

    let enriched = telemetry
        .run_stage(PipelineStage::Apprise, |scope| match backend {
            ClosureBackend::Hermetic { provider, .. } => {
                let instrumented = scope.instrument_provider(*provider);
                execute_apprise(&apprise_opts, configured, snapshot, &instrumented)
            }
            ClosureBackend::LiveDisk { .. } => {
                let active = live_provider
                    .as_ref()
                    .ok_or(crate::apprise::AppriseError::MissingConfig)?;
                let instrumented = scope.instrument_provider(active);
                execute_apprise(&apprise_opts, configured, snapshot, &instrumented)
            }
        })
        .map_err(|err| CliError::usage(err.to_string()))?;

    let disp = if config_was_present {
        StageDisposition::PrePopulated
    } else {
        StageDisposition::Executed
    };
    Ok((enriched, disp))
}
