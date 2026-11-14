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

//! Guarded Branch A, JIT Branch B, and `docket` → `admit` → `audit` execution steps for `canon-clerk audit`.

use super::AuditArgs;
use super::guards::{has_admit_cli_overrides, has_docket_cli_overrides, resolve_audit_concurrency};
use crate::admit::{AdmitOptions, execute_admit};
use crate::audit::{
    AuditOptions, DEFAULT_AUDIT_TIMEOUT_MS, execute_audit, resolve_canon_reference_paths,
};
use crate::caseload::{Caseload, CaseloadEvidence, CaseloadVerdict};
use crate::cli::CliError;
use crate::cli::admit::guards::resolve_admit_concurrency;
use crate::cli::args::CanonSelectionArgs;
use crate::cli::closure::{ClosureBackend, has_provider_overrides, merge_metadata_onto_intake};
use crate::cli::docket::guards::{resolve_docket_batch_size, resolve_docket_concurrency};
use crate::cli::provenance::StageDisposition;
use crate::cli::runners::{
    map_discover_error, resolve_branch_b_options, resolve_gemini_api_key, run_branch_b_configure,
};
use crate::discover::{DiscoverOptions, WorkspaceSnapshot, execute_discover};
use crate::docket::{DocketOptions, execute_docket};
use crate::intake::{IntakeError, IntakeOptions, execute_intake};
use crate::provider::{
    DEFAULT_GENERATION_TIMEOUT_MS, GeminiModelProvider, StructuredModelProvider,
};
use crate::telemetry::{PipelineStage, TelemetrySession};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct AuditCascadeOutcome {
    pub(super) caseload: Caseload,
    pub(super) configure_disp: StageDisposition,
    pub(super) docket_disp: StageDisposition,
    pub(super) admit_disp: StageDisposition,
    pub(super) audit_disp: StageDisposition,
}

#[derive(Clone, Copy)]
pub(super) struct AuditCascadeThresholds {
    pub(super) docket_threshold: f64,
    pub(super) admit_threshold: f64,
    pub(super) audit_threshold: f64,
}

pub(super) fn resolve_audit_intake(
    intake_opts: &IntakeOptions,
    incoming: Option<Caseload>,
    telemetry: &TelemetrySession<'_>,
) -> Result<(Caseload, StageDisposition, bool), CliError> {
    if !intake_opts.has_designated_source() {
        let existing = incoming.ok_or_else(|| {
            CliError::usage("Missing upstream caseload or filing inputs for `audit`.")
        })?;
        return Ok((existing, StageDisposition::PrePopulated, false));
    }

    telemetry.run_stage(PipelineStage::Intake, |_scope| {
        execute_audit_intake_body(intake_opts, incoming)
    })
}

fn execute_audit_intake_body(
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
    updated.verdict = None;
    Ok((updated, StageDisposition::Executed, true))
}

pub(super) fn resolve_audit_discover(
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

fn try_pre_populated_short_circuit(
    caseload: &Caseload,
    docket_pre_populated: bool,
    evidence_pre_populated: bool,
) -> Option<AuditCascadeOutcome> {
    if evidence_pre_populated
        && caseload
            .evidence
            .as_ref()
            .is_some_and(|ev| ev.active_cases.is_empty())
    {
        let cfg_disp = if caseload.config.is_some() {
            StageDisposition::PrePopulated
        } else {
            StageDisposition::SkippedZeroEvidence
        };
        let doc_disp = if caseload.docket.is_some() {
            StageDisposition::PrePopulated
        } else {
            StageDisposition::SkippedZeroEvidence
        };
        return Some(AuditCascadeOutcome {
            caseload: caseload.clone().with_verdict(CaseloadVerdict::default()),
            configure_disp: cfg_disp,
            docket_disp: doc_disp,
            admit_disp: StageDisposition::PrePopulated,
            audit_disp: StageDisposition::SkippedZeroEvidence,
        });
    }

    if !evidence_pre_populated
        && docket_pre_populated
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
        return Some(AuditCascadeOutcome {
            caseload: caseload
                .clone()
                .with_evidence(CaseloadEvidence::default())
                .with_verdict(CaseloadVerdict::default()),
            configure_disp: cfg_disp,
            docket_disp: StageDisposition::PrePopulated,
            admit_disp: StageDisposition::SkippedEmptyDocket,
            audit_disp: StageDisposition::SkippedEmptyDocket,
        });
    }

    None
}

pub(super) fn execute_jit_configure_docket_admit_and_audit(
    args: &AuditArgs,
    env: &BTreeMap<String, String>,
    thresholds: AuditCascadeThresholds,
    mut caseload: Caseload,
    snapshot: &mut WorkspaceSnapshot,
    backend: &ClosureBackend<'_>,
    telemetry: &TelemetrySession<'_>,
) -> Result<AuditCascadeOutcome, CliError> {
    let provider_overridden = has_provider_overrides(&args.provider);
    let docket_pre_populated =
        caseload.docket.is_some() && !has_docket_cli_overrides(args) && !provider_overridden;
    let evidence_pre_populated = caseload.evidence.is_some()
        && !has_admit_cli_overrides(args)
        && !has_docket_cli_overrides(args)
        && !provider_overridden;

    if let Some(short_circuit) =
        try_pre_populated_short_circuit(&caseload, docket_pre_populated, evidence_pre_populated)
    {
        return Ok(short_circuit);
    }

    let configure_options = resolve_branch_b_options(&args.provider, env)?;
    let stored = configure_options.stored_config.as_ref();
    let docket_opts = DocketOptions {
        threshold: thresholds.docket_threshold,
        timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
        max_canons_per_call: resolve_docket_batch_size(args.docket_batch_size, env, stored)?,
        max_concurrent_calls: resolve_docket_concurrency(args.docket_concurrency, env, stored)?,
    };
    let admit_opts = AdmitOptions {
        threshold: thresholds.admit_threshold,
        timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
        max_concurrent_calls: resolve_admit_concurrency(args.admit_concurrency, env, stored)?,
    };
    let audit_opts = AuditOptions {
        threshold: thresholds.audit_threshold,
        timeout_ms: DEFAULT_AUDIT_TIMEOUT_MS,
        max_concurrent_calls: resolve_audit_concurrency(args.audit_concurrency, env, stored)?,
    };

    let config_was_present =
        caseload.config.is_some() && !configure_options.has_explicit_overrides();
    caseload.verdict = None;

    let configured = run_branch_b_configure(&configure_options, Some(caseload), telemetry)?;
    let api_key = resolve_gemini_api_key(&configure_options)?;
    let live_instance;
    let active_provider: &dyn StructuredModelProvider = match backend {
        ClosureBackend::Hermetic { provider, .. } => *provider,
        ClosureBackend::LiveDisk { .. } => {
            live_instance = GeminiModelProvider::new(api_key);
            &live_instance
        }
    };
    let configure_disp = if config_was_present {
        StageDisposition::PrePopulated
    } else {
        StageDisposition::Executed
    };

    execute_configured_spine(
        configured,
        configure_disp,
        docket_pre_populated,
        evidence_pre_populated,
        (&docket_opts, &admit_opts, &audit_opts),
        snapshot,
        backend,
        active_provider,
        telemetry,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "Internal spine runner threading pre-resolved stage options, workspace snapshot, backend, provider, and telemetry"
)]
fn execute_configured_spine(
    configured: Caseload,
    configure_disp: StageDisposition,
    docket_pre_populated: bool,
    evidence_pre_populated: bool,
    stage_opts: (&DocketOptions, &AdmitOptions, &AuditOptions),
    snapshot: &mut WorkspaceSnapshot,
    backend: &ClosureBackend<'_>,
    provider: &dyn StructuredModelProvider,
    telemetry: &TelemetrySession<'_>,
) -> Result<AuditCascadeOutcome, CliError> {
    let (docket_opts, admit_opts, audit_opts) = stage_opts;

    let (after_docket, docket_disp) = if evidence_pre_populated || docket_pre_populated {
        let disp = if configured.docket.is_some() {
            StageDisposition::PrePopulated
        } else {
            StageDisposition::SkippedZeroEvidence
        };
        (configured, disp)
    } else {
        let mut cleared = configured;
        cleared.docket = None;
        cleared.evidence = None;
        let docketed = telemetry
            .run_stage(PipelineStage::Docket, |scope| {
                let instrumented = scope.instrument_provider(provider);
                execute_docket(docket_opts, cleared, snapshot, &instrumented)
            })
            .map_err(|err| CliError::usage(err.to_string()))?;
        (docketed, StageDisposition::Executed)
    };

    if !evidence_pre_populated
        && after_docket
            .docket
            .as_ref()
            .is_some_and(|d| d.active_docket.is_empty())
    {
        return Ok(AuditCascadeOutcome {
            caseload: after_docket
                .with_evidence(CaseloadEvidence::default())
                .with_verdict(CaseloadVerdict::default()),
            configure_disp,
            docket_disp,
            admit_disp: StageDisposition::SkippedEmptyDocket,
            audit_disp: StageDisposition::SkippedEmptyDocket,
        });
    }

    let (after_admit, admit_disp) = if evidence_pre_populated {
        (after_docket, StageDisposition::PrePopulated)
    } else {
        let mut cleared = after_docket;
        cleared.evidence = None;
        populate_live_target_file_snapshots(&cleared, snapshot, backend);
        let admitted = telemetry
            .run_stage(PipelineStage::Admit, |scope| {
                let instrumented = scope.instrument_provider(provider);
                execute_admit(admit_opts, cleared, snapshot, &instrumented)
            })
            .map_err(|err| CliError::usage(err.to_string()))?;
        (admitted, StageDisposition::Executed)
    };

    if after_admit
        .evidence
        .as_ref()
        .is_some_and(|ev| ev.active_cases.is_empty())
    {
        return Ok(AuditCascadeOutcome {
            caseload: after_admit.with_verdict(CaseloadVerdict::default()),
            configure_disp,
            docket_disp,
            admit_disp,
            audit_disp: StageDisposition::SkippedZeroEvidence,
        });
    }

    populate_live_audit_file_snapshots(&after_admit, snapshot, backend);
    let after_audit = telemetry
        .run_stage(PipelineStage::Audit, |scope| {
            let instrumented = scope.instrument_provider(provider);
            execute_audit(audit_opts, after_admit, snapshot, &instrumented)
        })
        .map_err(|err| CliError::usage(err.to_string()))?;

    Ok(AuditCascadeOutcome {
        caseload: after_audit,
        configure_disp,
        docket_disp,
        admit_disp,
        audit_disp: StageDisposition::Executed,
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

fn populate_live_audit_file_snapshots(
    caseload: &Caseload,
    snapshot: &mut WorkspaceSnapshot,
    backend: &ClosureBackend<'_>,
) {
    populate_live_target_file_snapshots(caseload, snapshot, backend);
    let ClosureBackend::LiveDisk { workspace_root } = backend else {
        return;
    };
    let Some(evidence) = &caseload.evidence else {
        return;
    };
    let mut paths_to_load = BTreeSet::new();
    for canon_ref in &evidence.active_cases {
        for admitted in evidence.admitted_exhibits_for(canon_ref) {
            paths_to_load.insert(admitted.file_path.clone());
        }
        if let Some(canon) = snapshot.canons.get(canon_ref) {
            for ref_path in resolve_canon_reference_paths(canon, snapshot) {
                paths_to_load.insert(ref_path);
            }
        }
    }
    let collected: Vec<String> = paths_to_load.into_iter().collect();
    if !collected.is_empty() {
        snapshot.populate_file_contents_from_disk(workspace_root, &collected);
    }
}
