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

//! Guarded Branch A, JIT Branch B, upstream `docket`, and `admit` execution steps for `canon-clerk admit`.

use super::AdmitArgs;
use super::guards::{has_docket_cli_overrides, resolve_admit_concurrency};
use crate::admit::{AdmitOptions, execute_admit};
use crate::caseload::{Caseload, CaseloadEvidence};
use crate::cli::CliError;
use crate::cli::args::CanonSelectionArgs;
use crate::cli::closure::{ClosureBackend, has_provider_overrides, merge_metadata_onto_intake};
use crate::cli::docket::guards::{resolve_docket_batch_size, resolve_docket_concurrency};
use crate::cli::provenance::StageDisposition;
use crate::cli::runners::{
    build_live_model_provider, map_discover_error, resolve_branch_b_options, run_branch_b_configure,
};
use crate::discover::{DiscoverOptions, WorkspaceSnapshot, execute_discover};
use crate::docket::{DocketOptions, execute_docket};
use crate::intake::{IntakeError, IntakeOptions, execute_intake};
use crate::provider::{DEFAULT_GENERATION_TIMEOUT_MS, StructuredModelProvider};
use crate::telemetry::{PipelineStage, TelemetrySession};
use std::collections::BTreeMap;

pub(super) struct CascadeOutcome {
    pub(super) caseload: Caseload,
    pub(super) configure_disp: StageDisposition,
    pub(super) docket_disp: StageDisposition,
    pub(super) admit_disp: StageDisposition,
}

pub(super) struct CascadeThresholds {
    pub(super) docket_threshold: f64,
    pub(super) admit_threshold: f64,
}

pub(super) fn resolve_admit_intake(
    intake_opts: &IntakeOptions,
    incoming: Option<Caseload>,
    telemetry: &TelemetrySession<'_>,
) -> Result<(Caseload, StageDisposition, bool), CliError> {
    if !intake_opts.has_designated_source() {
        let existing = incoming.ok_or_else(|| {
            CliError::usage("Missing upstream caseload or filing inputs for `admit`.")
        })?;
        return Ok((existing, StageDisposition::PrePopulated, false));
    }

    telemetry.run_stage(PipelineStage::Intake, |_scope| {
        execute_admit_intake_body(intake_opts, incoming)
    })
}

fn execute_admit_intake_body(
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
    Ok((updated, StageDisposition::Executed, true))
}

pub(super) fn resolve_admit_discover(
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

    let discover_opts = DiscoverOptions {
        all_canons: canons.all_canons,
        canon_patterns: canons.canon.clone(),
    };

    let updated = telemetry.run_stage(PipelineStage::Discover, |_scope| {
        execute_discover(&discover_opts, caseload, snapshot).map_err(map_discover_error)
    })?;
    Ok((updated, StageDisposition::Executed))
}

fn try_pre_populated_empty_docket(
    caseload: &Caseload,
    docket_pre_populated: bool,
) -> Option<CascadeOutcome> {
    if docket_pre_populated
        && caseload
            .docket
            .as_ref()
            .is_some_and(|d| d.active_docket.is_empty())
    {
        let cfg_disp = if caseload.config.is_some() {
            StageDisposition::PrePopulated
        } else {
            StageDisposition::SkippedEmptyDocket
        };
        return Some(CascadeOutcome {
            caseload: caseload.clone().with_evidence(CaseloadEvidence::default()),
            configure_disp: cfg_disp,
            docket_disp: StageDisposition::PrePopulated,
            admit_disp: StageDisposition::SkippedEmptyDocket,
        });
    }
    None
}

pub(super) fn execute_jit_configure_docket_and_admit(
    args: &AdmitArgs,
    env: &BTreeMap<String, String>,
    thresholds: CascadeThresholds,
    mut caseload: Caseload,
    snapshot: &mut WorkspaceSnapshot,
    backend: &ClosureBackend<'_>,
    telemetry: &TelemetrySession<'_>,
) -> Result<CascadeOutcome, CliError> {
    let docket_pre_populated = caseload.docket.is_some()
        && !has_docket_cli_overrides(args)
        && !has_provider_overrides(&args.provider);

    if let Some(outcome) = try_pre_populated_empty_docket(&caseload, docket_pre_populated) {
        return Ok(outcome);
    }

    let configure_options = resolve_branch_b_options(&args.provider, env)?;
    let docket_batch_size = resolve_docket_batch_size(
        args.docket_batch_size,
        env,
        configure_options.stored_config.as_ref(),
    )?;
    let docket_concurrency = resolve_docket_concurrency(
        args.docket_concurrency,
        env,
        configure_options.stored_config.as_ref(),
    )?;
    let admit_concurrency = resolve_admit_concurrency(
        args.admit_concurrency,
        env,
        configure_options.stored_config.as_ref(),
    )?;

    let config_was_present =
        caseload.config.is_some() && !configure_options.has_explicit_overrides();
    caseload.evidence = None;

    let configured = run_branch_b_configure(&configure_options, Some(caseload), telemetry)?;
    let cfg_snapshot = configured.config.clone().unwrap_or_default();
    let live_instance;
    let active_provider: &dyn StructuredModelProvider = match backend {
        ClosureBackend::Hermetic { provider, .. } => *provider,
        ClosureBackend::LiveDisk { .. } => {
            live_instance = build_live_model_provider(&configure_options, &cfg_snapshot)?;
            &live_instance
        }
    };
    let configure_disp = if config_was_present {
        StageDisposition::PrePopulated
    } else {
        StageDisposition::Executed
    };

    let (after_docket, docket_disp) = if docket_pre_populated {
        (configured, StageDisposition::PrePopulated)
    } else {
        let mut cleared = configured;
        cleared.docket = None;
        let docket_opts = DocketOptions {
            threshold: thresholds.docket_threshold,
            timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
            max_canons_per_call: docket_batch_size,
            max_concurrent_calls: docket_concurrency,
        };
        let docketed =
            run_docket_stage(&docket_opts, cleared, snapshot, active_provider, telemetry)?;
        (docketed, StageDisposition::Executed)
    };

    if after_docket
        .docket
        .as_ref()
        .is_some_and(|d| d.active_docket.is_empty())
    {
        return Ok(CascadeOutcome {
            caseload: after_docket.with_evidence(CaseloadEvidence::default()),
            configure_disp,
            docket_disp,
            admit_disp: StageDisposition::SkippedEmptyDocket,
        });
    }

    populate_live_target_file_snapshots(&after_docket, snapshot, backend);

    let admit_opts = AdmitOptions {
        threshold: thresholds.admit_threshold,
        timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
        max_concurrent_calls: admit_concurrency,
    };
    let after_admit = run_admit_stage(
        &admit_opts,
        after_docket,
        snapshot,
        active_provider,
        telemetry,
    )?;

    Ok(CascadeOutcome {
        caseload: after_admit,
        configure_disp,
        docket_disp,
        admit_disp: StageDisposition::Executed,
    })
}

fn populate_live_target_file_snapshots(
    caseload: &Caseload,
    snapshot: &mut WorkspaceSnapshot,
    backend: &ClosureBackend<'_>,
) {
    let ClosureBackend::LiveDisk { workspace_root } = backend else {
        return;
    };
    let Some(discovery) = &caseload.discovery else {
        return;
    };
    let diffs = caseload.intake.as_ref().map(|i| &i.diffs);
    let paths_to_load: Vec<String> = discovery
        .target_files
        .iter()
        .filter(|path| {
            diffs.is_none_or(|d| {
                d.get(*path)
                    .is_none_or(|art| art.patch.is_none() && art.content.is_none())
            })
        })
        .cloned()
        .collect();
    if !paths_to_load.is_empty() {
        snapshot.populate_file_contents_from_disk(workspace_root, &paths_to_load);
    }
}

fn run_docket_stage(
    opts: &DocketOptions,
    caseload: Caseload,
    snapshot: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    telemetry
        .run_stage(PipelineStage::Docket, |scope| {
            let instrumented = scope.instrument_provider(provider);
            execute_docket(opts, caseload, snapshot, &instrumented)
        })
        .map_err(|err| CliError::usage(err.to_string()))
}

fn run_admit_stage(
    opts: &AdmitOptions,
    caseload: Caseload,
    snapshot: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    telemetry
        .run_stage(PipelineStage::Admit, |scope| {
            let instrumented = scope.instrument_provider(provider);
            execute_admit(opts, caseload, snapshot, &instrumented)
        })
        .map_err(|err| CliError::usage(err.to_string()))
}
