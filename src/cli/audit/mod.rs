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

//! CLI driving adapter for the `canon-clerk audit` subcommand (`docs/architecture/nodes/audit.md` §5).

pub mod guards;
mod render;
mod stages;

use crate::caseload::{Caseload, CaseloadDocket, CaseloadEvidence, CaseloadVerdict, VerdictStatus};
use crate::cli::CliError;
use crate::cli::admit::guards::{resolve_admit_concurrency, resolve_admit_threshold};
use crate::cli::args::{CanonSelectionArgs, CaseloadIoArgs, FilingSourceArgs, ProviderConfigArgs};
use crate::cli::closure::{ClosureBackend, materialize_closure_snapshot, resolve_closure_validate};
use crate::cli::docket::guards::{
    resolve_docket_batch_size, resolve_docket_concurrency, resolve_docket_threshold,
};
use crate::cli::io::ensure_single_stdin_source;
use crate::cli::provenance::StageDisposition;
use crate::discover::WorkspaceSnapshot;
use crate::provider::StructuredModelProvider;
use crate::telemetry::TelemetrySession;
use clap::{Args, ValueEnum};
use guards::{
    can_fast_forward_completed_audit, enforce_filing_and_case_guard,
    ensure_stage_prefixed_threshold_flags, resolve_audit_concurrency, resolve_audit_threshold,
};
use render::{AuditWatermarks, render_audit_output, resolve_effective_format};
use stages::{
    AuditCascadeThresholds, execute_jit_configure_docket_admit_and_audit, resolve_audit_discover,
    resolve_audit_intake,
};
use std::collections::BTreeMap;
use std::env;
use std::io::{Read, Write};

const AUDIT_EXAMPLES: &str = "\
Examples:
  # Audit a unified diff piped on standard input across the full Caseload cascade:
  git diff origin/main | canon-clerk audit --diff -

  # Audit explicit target paths or newline-delimited stdin paths:
  canon-clerk audit src/cli src/audit
  git diff origin/main --name-only | canon-clerk audit -

  # Adjudicate an upstream admitted Caseload and emit JSON:
  canon-clerk audit --caseload caseload-6.json --json

  # Run in GitHub Actions annotation mode while archiving caseload.json for CI forensics:
  git diff origin/main | canon-clerk audit --diff - --format github --output-caseload caseload.json";

/// Output presentation format for `canon-clerk audit`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditFormat {
    /// Human-readable 7-stage Pipeline Funnel Provenance receipt and Verdict summary.
    Stylish,
    /// Full cumulative `Caseload` JSON envelope.
    Json,
    /// Tab-separated adjudication and diagnostic lines.
    Compact,
    /// GitHub Actions workflow command annotations (`::error file=...::...`) and `$GITHUB_STEP_SUMMARY` report.
    Github,
}

/// Command-line arguments for `canon-clerk audit`.
#[derive(Args, Debug, Clone, Default, PartialEq)]
#[command(
    about = "Judicial adjudication: evaluate admitted exhibits against canon statutes and render verdicts",
    long_about = "Judicial Adjudication stage of the Caseload DAG (Cascade Spine terminus). Executes the guarded [intake, discover, validate, configure, docket, admit, audit] closure to evaluate admitted exhibits in isolated per-case reasoning trials, rendering compliance decrees and source line annotations.",
    after_help = AUDIT_EXAMPLES
)]
pub struct AuditArgs {
    #[command(flatten)]
    pub filing: FilingSourceArgs,

    #[command(flatten)]
    pub canons: CanonSelectionArgs,

    /// Substantive compliance score threshold in `[0.0, 1.0]` (default: `0.5`; env: `CANON_CLERK_AUDIT_THRESHOLD`).
    #[arg(
        long = "audit-threshold",
        value_name = "FLOAT",
        allow_hyphen_values = true,
        help_heading = "Audit Options",
        help = "Substantive compliance threshold in [0.0, 1.0] (default: 0.5; fallback env: CANON_CLERK_AUDIT_THRESHOLD)"
    )]
    pub audit_threshold: Option<f64>,

    /// Optional maximum concurrent in-flight per-case trial calls (default: `4`; env: `CANON_CLERK_AUDIT_CONCURRENCY`).
    #[arg(
        long = "audit-concurrency",
        value_name = "COUNT",
        help_heading = "Audit Options",
        help = "Maximum concurrent in-flight per-case trial calls (default: 4; fallback env: CANON_CLERK_AUDIT_CONCURRENCY)"
    )]
    pub audit_concurrency: Option<usize>,

    /// Upstream evidentiary admissibility score threshold in `[0.0, 1.0]` (default: `0.5`; env: `CANON_CLERK_ADMIT_THRESHOLD`).
    #[arg(
        long = "admit-threshold",
        value_name = "FLOAT",
        allow_hyphen_values = true,
        help_heading = "Admit Options",
        help = "Upstream evidentiary admissibility threshold in [0.0, 1.0] (default: 0.5; fallback env: CANON_CLERK_ADMIT_THRESHOLD)"
    )]
    pub admit_threshold: Option<f64>,

    /// Upstream maximum concurrent in-flight per-case `admit` screener calls (default: `4`; env: `CANON_CLERK_ADMIT_CONCURRENCY`).
    #[arg(
        long = "admit-concurrency",
        visible_alias = "max-concurrent-calls",
        value_name = "COUNT",
        help_heading = "Admit Options",
        help = "Maximum concurrent upstream admit screener calls (default: 4; fallback env: CANON_CLERK_ADMIT_CONCURRENCY)"
    )]
    pub admit_concurrency: Option<usize>,

    /// Upstream subject-matter jurisdiction threshold in `[0.0, 1.0]` (default: `0.5`; env: `CANON_CLERK_DOCKET_THRESHOLD`).
    #[arg(
        long = "docket-threshold",
        value_name = "FLOAT",
        allow_hyphen_values = true,
        help_heading = "Docket Options",
        help = "Upstream macro jurisdiction threshold in [0.0, 1.0] (default: 0.5; fallback env: CANON_CLERK_DOCKET_THRESHOLD)"
    )]
    pub docket_threshold: Option<f64>,

    /// Upstream maximum candidate canons per `docket` screener call (env: `CANON_CLERK_DOCKET_BATCH_SIZE`).
    #[arg(
        long = "docket-batch-size",
        visible_alias = "max-canons-per-call",
        value_name = "COUNT",
        help_heading = "Docket Options",
        help = "Maximum candidate canons per upstream docket screener call (fallback env: CANON_CLERK_DOCKET_BATCH_SIZE)"
    )]
    pub docket_batch_size: Option<usize>,

    /// Upstream maximum concurrent in-flight `docket` calls when batched (default: `4`; env: `CANON_CLERK_DOCKET_CONCURRENCY`).
    #[arg(
        long = "docket-concurrency",
        value_name = "COUNT",
        help_heading = "Docket Options",
        help = "Maximum concurrent upstream docket calls when batched (default: 4; fallback env: CANON_CLERK_DOCKET_CONCURRENCY)"
    )]
    pub docket_concurrency: Option<usize>,

    /// Hidden capture rejecting ambiguous unqualified `--threshold` (`scheduling.md` §4 Pattern C).
    #[arg(long = "threshold", hide = true, allow_hyphen_values = true)]
    pub rejected_threshold: Option<String>,

    /// Hidden capture rejecting out-of-closure `--apprise-threshold` (`scheduling.md` §4 Pattern C).
    #[arg(long = "apprise-threshold", hide = true, allow_hyphen_values = true)]
    pub rejected_apprise_threshold: Option<String>,

    /// Hidden capture rejecting out-of-closure `--apprise-batch-size`.
    #[arg(long = "apprise-batch-size", hide = true, allow_hyphen_values = true)]
    pub rejected_apprise_batch_size: Option<String>,

    /// Hidden capture rejecting out-of-closure `--apprise-concurrency`.
    #[arg(long = "apprise-concurrency", hide = true, allow_hyphen_values = true)]
    pub rejected_apprise_concurrency: Option<String>,

    #[command(flatten)]
    pub provider: ProviderConfigArgs,

    /// Output format (`stylish`, `json`, `compact`, or `github`; defaults to `github` when `GITHUB_ACTIONS=true`).
    #[arg(
        long = "format",
        value_enum,
        help_heading = "Caseload & Output",
        help = "Output formatting choice (defaults to 'github' when GITHUB_ACTIONS=true, else 'stylish')"
    )]
    pub format: Option<AuditFormat>,

    #[command(flatten)]
    pub io: CaseloadIoArgs,
}

impl Eq for AuditArgs {}

/// Executes `canon-clerk audit` hermetically with an explicit [`TelemetrySession`].
pub fn run_audit_with_telemetry(
    args: &AuditArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    workspace: &WorkspaceSnapshot,
    env: &BTreeMap<String, String>,
    provider: &dyn StructuredModelProvider,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    run_audit_core(
        args,
        stdin,
        stdout,
        env,
        ClosureBackend::Hermetic {
            workspace,
            provider,
        },
        telemetry,
    )
}

/// Executes `canon-clerk audit` hermetically against an in-memory [`WorkspaceSnapshot`],
/// explicit environment map, and hexagonal [`StructuredModelProvider`].
pub fn run_audit_with_snapshot_and_provider(
    args: &AuditArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    workspace: &WorkspaceSnapshot,
    env: &BTreeMap<String, String>,
    provider: &dyn StructuredModelProvider,
) -> Result<Caseload, CliError> {
    run_audit_with_telemetry(
        args,
        stdin,
        stdout,
        workspace,
        env,
        provider,
        &TelemetrySession::noop(),
    )
}

/// Executes `canon-clerk audit` against the live working directory with an explicit [`TelemetrySession`].
pub fn run_audit_live_with_telemetry(
    args: &AuditArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    let cwd = env::current_dir().map_err(|err| {
        CliError::runtime(format!(
            "Failed to determine current working directory: {err}"
        ))
    })?;
    let env_map: BTreeMap<String, String> = env::vars().collect();
    run_audit_core(
        args,
        stdin,
        stdout,
        &env_map,
        ClosureBackend::LiveDisk {
            workspace_root: &cwd,
        },
        telemetry,
    )
}

/// Executes `canon-clerk audit` using the current working directory, process environment,
/// and live [`GeminiModelProvider`].
pub fn run_audit(
    args: &AuditArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    run_audit_live_with_telemetry(args, stdin, stdout, &TelemetrySession::noop())
}

fn run_audit_core(
    args: &AuditArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    env: &BTreeMap<String, String>,
    backend: ClosureBackend<'_>,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    ensure_stage_prefixed_threshold_flags(args)?;
    ensure_single_stdin_source(&args.filing, args.io.caseload.as_deref())?;

    let thresholds = AuditCascadeThresholds {
        docket_threshold: resolve_docket_threshold(args.docket_threshold, env)?,
        admit_threshold: resolve_admit_threshold(args.admit_threshold, env)?,
        audit_threshold: resolve_audit_threshold(args.audit_threshold, env)?,
    };
    let _validated_docket_batch = resolve_docket_batch_size(args.docket_batch_size, env, None)?;
    let _validated_docket_conc = resolve_docket_concurrency(args.docket_concurrency, env, None)?;
    let _validated_admit_conc = resolve_admit_concurrency(args.admit_concurrency, env, None)?;
    let _validated_audit_conc = resolve_audit_concurrency(args.audit_concurrency, env, None)?;

    let incoming = args.io.read_caseload(stdin)?;
    let intake_opts = args.filing.resolve_intake_options(stdin)?;

    if can_fast_forward_completed_audit(args, &intake_opts, incoming.as_ref())
        && let Some(completed) = incoming
    {
        telemetry.finish_pipeline();
        let wm = pre_populated_watermarks(thresholds);
        return finalize_verdict_and_exit_code(args, completed, &wm, env, stdout);
    }

    enforce_filing_and_case_guard(&intake_opts, &args.canons, incoming.as_ref())?;

    let (caseload_after_intake, intake_disp, intake_changed) =
        resolve_audit_intake(&intake_opts, incoming, telemetry)?;

    let mut snapshot =
        materialize_closure_snapshot(&backend, &args.canons.canon, &caseload_after_intake)?;
    let total_workspace_canons = Some(snapshot.canons.len());

    let (caseload_after_discover, discover_disp) = resolve_audit_discover(
        &args.canons,
        caseload_after_intake,
        &snapshot,
        intake_changed,
        telemetry,
    )?;

    if caseload_after_discover
        .discovery
        .as_ref()
        .is_some_and(|d| d.candidate_canons.is_empty())
    {
        telemetry.finish_pipeline();
        return finish_zero_candidate_audit(
            args,
            caseload_after_discover,
            intake_disp,
            total_workspace_canons,
            thresholds,
            env,
            stdout,
        );
    }

    let (caseload_after_validate, validate_disp) =
        resolve_closure_validate(caseload_after_discover, &snapshot, telemetry)?;

    if caseload_after_validate
        .validation
        .as_ref()
        .is_some_and(|v| v.has_errors)
    {
        telemetry.finish_pipeline();
        return handle_validation_failure(
            args,
            &caseload_after_validate,
            intake_disp,
            discover_disp,
            total_workspace_canons,
            thresholds,
            env,
            stdout,
        );
    }

    let outcome = execute_jit_configure_docket_admit_and_audit(
        args,
        env,
        thresholds,
        caseload_after_validate,
        &mut snapshot,
        &backend,
        telemetry,
    )?;
    telemetry.finish_pipeline();

    let wm = AuditWatermarks {
        intake: intake_disp,
        discover: discover_disp,
        validate: validate_disp,
        configure: outcome.configure_disp,
        docket: outcome.docket_disp,
        admit: outcome.admit_disp,
        audit: outcome.audit_disp,
        total_workspace_canons,
        docket_threshold: thresholds.docket_threshold,
        admit_threshold: thresholds.admit_threshold,
        audit_threshold: thresholds.audit_threshold,
    };
    finalize_verdict_and_exit_code(args, outcome.caseload, &wm, env, stdout)
}

fn finalize_verdict_and_exit_code(
    args: &AuditArgs,
    caseload: Caseload,
    wm: &AuditWatermarks,
    env: &BTreeMap<String, String>,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    args.io.write_output_caseload(&caseload)?;
    render_audit_output(args, &caseload, wm, env, stdout)?;
    if caseload
        .verdict
        .as_ref()
        .is_some_and(|v| v.status == VerdictStatus::Fail)
    {
        return Err(CliError::silent(1));
    }
    Ok(caseload)
}

fn finish_zero_candidate_audit(
    args: &AuditArgs,
    caseload_after_discover: Caseload,
    intake_disp: StageDisposition,
    total_workspace_canons: Option<usize>,
    thresholds: AuditCascadeThresholds,
    env: &BTreeMap<String, String>,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    let short_circuited = caseload_after_discover
        .with_docket(CaseloadDocket::default())
        .with_evidence(CaseloadEvidence::default())
        .with_verdict(CaseloadVerdict::default());
    args.io.write_output_caseload(&short_circuited)?;
    let wm = AuditWatermarks {
        intake: intake_disp,
        discover: StageDisposition::ZeroCandidateShortCircuit,
        validate: StageDisposition::SkippedZeroCandidates,
        configure: StageDisposition::SkippedZeroCandidates,
        docket: StageDisposition::SkippedZeroCandidates,
        admit: StageDisposition::SkippedZeroCandidates,
        audit: StageDisposition::SkippedZeroCandidates,
        total_workspace_canons,
        docket_threshold: thresholds.docket_threshold,
        admit_threshold: thresholds.admit_threshold,
        audit_threshold: thresholds.audit_threshold,
    };
    render_audit_output(args, &short_circuited, &wm, env, stdout)?;
    Ok(short_circuited)
}

fn pre_populated_watermarks(thresholds: AuditCascadeThresholds) -> AuditWatermarks {
    AuditWatermarks {
        intake: StageDisposition::PrePopulated,
        discover: StageDisposition::PrePopulated,
        validate: StageDisposition::PrePopulated,
        configure: StageDisposition::PrePopulated,
        docket: StageDisposition::PrePopulated,
        admit: StageDisposition::PrePopulated,
        audit: StageDisposition::PrePopulated,
        total_workspace_canons: None,
        docket_threshold: thresholds.docket_threshold,
        admit_threshold: thresholds.admit_threshold,
        audit_threshold: thresholds.audit_threshold,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Internal CLI helper formatting validation failure provenance watermarks across the 7-stage audit closure"
)]
fn handle_validation_failure(
    args: &AuditArgs,
    caseload: &Caseload,
    intake_disp: StageDisposition,
    discover_disp: StageDisposition,
    total_workspace_canons: Option<usize>,
    thresholds: AuditCascadeThresholds,
    env: &BTreeMap<String, String>,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    args.io.write_output_caseload(caseload)?;
    let wm = AuditWatermarks {
        intake: intake_disp,
        discover: discover_disp,
        validate: StageDisposition::ValidationFailed,
        configure: StageDisposition::SkippedValidationFailed,
        docket: StageDisposition::SkippedValidationFailed,
        admit: StageDisposition::SkippedValidationFailed,
        audit: StageDisposition::SkippedValidationFailed,
        total_workspace_canons,
        docket_threshold: thresholds.docket_threshold,
        admit_threshold: thresholds.admit_threshold,
        audit_threshold: thresholds.audit_threshold,
    };
    if resolve_effective_format(args, env) != AuditFormat::Compact {
        render_audit_output(args, caseload, &wm, env, stdout)?;
    }

    let total_errors: usize = caseload
        .validation
        .as_ref()
        .map_or(1, |v| v.results.values().map(|m| m.error_count).sum());
    Err(CliError::runtime(format!(
        "Candidate canon validation failed with {total_errors} error(s); aborting `audit` before model spend.\n  \
         Hint: Run `canon-clerk validate` to inspect and repair malformed candidate canons."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::ParseCanonOptions;
    use crate::cli::{Cli, Commands};
    use crate::configure::ENV_GEMINI_API_KEY;
    use crate::provider::{GenerationRequest, GenerationResponse, ProviderError, TokenUsage};
    use crate::telemetry::{
        PipelineStage, RecordingTelemetrySink, SteppingTelemetryClock, TelemetryEvent,
    };
    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};
    use std::fs;
    use std::io::Cursor;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FullCascadeProvider {
        calls: Mutex<usize>,
        docket_payload: Value,
        admit_payload: Value,
        audit_payload: Value,
    }

    impl FullCascadeProvider {
        fn new(docket_payload: Value, admit_payload: Value, audit_payload: Value) -> Self {
            Self {
                calls: Mutex::new(0),
                docket_payload,
                admit_payload,
                audit_payload,
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().expect("lock calls")
        }
    }

    impl StructuredModelProvider for FullCascadeProvider {
        fn generate_structured(
            &self,
            request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            *self.calls.lock().expect("lock calls") += 1;
            let payload = if request.system_instruction.contains("Macro Subject-Matter") {
                self.docket_payload.clone()
            } else if request
                .system_instruction
                .contains("Micro Evidentiary Admissibility")
            {
                self.admit_payload.clone()
            } else {
                self.audit_payload.clone()
            };
            Ok(GenerationResponse {
                payload,
                thoughts: vec!["Deliberating on canon compliance...".to_string()],
                usage: TokenUsage {
                    prompt_tokens: 250,
                    completion_tokens: 60,
                    reasoning_tokens: 90,
                    total_tokens: 400,
                },
                latency_ms: 80,
                resolved_model: "gemini-2.5-pro".to_string(),
            })
        }
    }

    fn sample_workspace() -> WorkspaceSnapshot {
        WorkspaceSnapshot::new()
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ninspect:\n  - diff\n  - pr_title?\n---\n# CLI Flags Kebab Case\n\nCLI flags MUST use kebab-case naming.\n",
                &ParseCanonOptions::with_source_ref(".canons/cli/cli-flags-kebab-case.md"),
            )
            .with_file_content(
                "src/cli/audit.rs",
                "pub struct AuditArgs {\n    #[arg(long = \"audit-threshold\")]\n    pub threshold: f64,\n}\n",
            )
    }

    fn parse_audit(argv: &[&str]) -> AuditArgs {
        let cli = Cli::try_parse_from(argv).expect("parse audit args");
        match cli.command {
            Some(Commands::Audit(args)) => args,
            _ => panic!("expected Audit subcommand"),
        }
    }

    mod cli_flags_and_guards {
        use super::*;

        #[test]
        fn test_help_groups_and_stage_prefixed_flag_scoping() {
            let mut cmd = Cli::command();
            let sub = cmd.find_subcommand_mut("audit").expect("audit subcommand");
            let help = sub.render_help().to_string();
            assert!(help.contains("Audit Options:"));
            assert!(help.contains("Admit Options:"));
            assert!(help.contains("Docket Options:"));
            assert!(help.contains("CANON_CLERK_AUDIT_THRESHOLD"));
            assert!(help.contains("CANON_CLERK_AUDIT_CONCURRENCY"));
            assert!(help.contains("Examples:"));

            let ws = sample_workspace();
            let provider = FullCascadeProvider::new(json!({}), json!({}), json!({}));
            let env = BTreeMap::new();

            for bad_argv in [
                vec![
                    "canon-clerk",
                    "audit",
                    "src/cli/audit.rs",
                    "--threshold",
                    "0.7",
                ],
                vec![
                    "canon-clerk",
                    "audit",
                    "src/cli/audit.rs",
                    "--apprise-threshold",
                    "0.7",
                ],
                vec![
                    "canon-clerk",
                    "audit",
                    "src/cli/audit.rs",
                    "--apprise-batch-size",
                    "10",
                ],
                vec![
                    "canon-clerk",
                    "audit",
                    "src/cli/audit.rs",
                    "--apprise-concurrency",
                    "2",
                ],
                vec!["canon-clerk", "audit"],
                vec!["canon-clerk", "audit", "--all-canons"],
                vec![
                    "canon-clerk",
                    "audit",
                    "src/cli/audit.rs",
                    "--audit-concurrency",
                    "0",
                ],
                vec![
                    "canon-clerk",
                    "audit",
                    "src/cli/audit.rs",
                    "--audit-threshold",
                    "1.4",
                ],
            ] {
                let args = parse_audit(&bad_argv);
                let mut stdin = Cursor::new(b"");
                let mut stdout = Vec::new();
                let err = run_audit_with_snapshot_and_provider(
                    &args,
                    &mut stdin,
                    &mut stdout,
                    &ws,
                    &env,
                    &provider,
                )
                .expect_err("guard must fail with usage error");
                assert_eq!(err.exit_code(), 2, "failed on argv: {bad_argv:?}");
                assert!(err.to_string().contains("Hint:"));
            }
        }

        #[test]
        fn test_zero_candidate_and_validation_failure_precede_gemini_api_key() {
            let ws = sample_workspace();
            let provider = FullCascadeProvider::new(json!({}), json!({}), json!({}));
            let mut empty_env = BTreeMap::new();
            empty_env.insert(
                "XDG_CONFIG_HOME".to_string(),
                "/nonexistent-canon-clerk-xdg-dir".to_string(),
            );

            let zero_args = parse_audit(&["canon-clerk", "audit", "README.md"]);
            let mut stdout = Vec::new();
            let zero_out = run_audit_with_snapshot_and_provider(
                &zero_args,
                &mut Cursor::new(b""),
                &mut stdout,
                &ws,
                &empty_env,
                &provider,
            )
            .expect("0-candidate short-circuit must succeed without API key");
            assert_eq!(provider.call_count(), 0);
            assert!(zero_out.verdict.expect("verdict").is_pass());
            let rendered = String::from_utf8(stdout).expect("utf8");
            assert!(rendered.contains("⊘ audit     : (fast-forward: zero candidate canons)"));

            let broken_ws = WorkspaceSnapshot::new()
                .with_canon_markdown(
                    "---\nunknown_frontmatter_key: true\ntriggers:\n  - \"src/**\"\n---\n# Bad Canon\n\nRule must hold.\n",
                    &ParseCanonOptions::with_source_ref(".canons/broken.md"),
                )
                .with_file("src/cli/audit.rs");
            let val_args = parse_audit(&["canon-clerk", "audit", "src/cli/audit.rs"]);
            let mut out_val = Vec::new();
            let val_err = run_audit_with_snapshot_and_provider(
                &val_args,
                &mut Cursor::new(b""),
                &mut out_val,
                &broken_ws,
                &empty_env,
                &provider,
            )
            .expect_err("malformed canon must exit 1");
            assert_eq!(val_err.exit_code(), 1);
            assert_eq!(provider.call_count(), 0);
            assert!(
                String::from_utf8(out_val)
                    .expect("utf8")
                    .contains("⊘ audit     : (fast-forward: canon validation failed)")
            );
        }
    }

    mod cascade_execution_and_github_ci_output {
        use super::*;

        #[test]
        fn test_passing_seven_stage_cascade_emits_telemetry_and_fast_forwards() {
            let ws = sample_workspace();
            let provider = FullCascadeProvider::new(
                json!({
                    "cases": [{
                        "canon_path": ".canons/cli/cli-flags-kebab-case.md",
                        "colorability_summary": "Touches CLI flag definition.",
                        "colorability_score": 0.94
                    }]
                }),
                json!({
                    "exhibits": [{
                        "file_path": "src/cli/audit.rs",
                        "admissibility_summary": "Defines `--audit-threshold` flag.",
                        "admissibility_score": 0.96
                    }]
                }),
                json!({
                    "compliance_summary": "`--audit-threshold` adheres strictly to kebab-case.",
                    "compliance_score": 0.97,
                    "annotations": []
                }),
            );
            let mut env = BTreeMap::new();
            env.insert(ENV_GEMINI_API_KEY.to_string(), "test-key".to_string());

            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 30);
            let session = TelemetrySession::new(&sink, &clock);

            let args = parse_audit(&["canon-clerk", "audit", "src/cli/audit.rs"]);
            let mut stdout = Vec::new();
            let caseload = run_audit_with_telemetry(
                &args,
                &mut Cursor::new(b""),
                &mut stdout,
                &ws,
                &env,
                &provider,
                &session,
            )
            .expect("passing 7-stage audit cascade");

            assert_eq!(provider.call_count(), 3);
            let text = String::from_utf8(stdout).expect("utf8");
            assert!(text.contains("✓ audit     : 1 passed, 0 failed across 1 trial(s)"));
            assert!(text.contains("Verdict: PASS"));

            let stages: Vec<PipelineStage> = sink
                .events()
                .into_iter()
                .filter_map(|e| match e {
                    TelemetryEvent::StageStart { stage, .. } => Some(stage),
                    _ => None,
                })
                .collect();
            assert_eq!(
                stages,
                vec![
                    PipelineStage::Intake,
                    PipelineStage::Discover,
                    PipelineStage::Validate,
                    PipelineStage::Configure,
                    PipelineStage::Docket,
                    PipelineStage::Admit,
                    PipelineStage::Audit,
                ]
            );

            // Fast-forward completed Caseload in compact mode without API key
            let serialized = serde_json::to_vec(&caseload).expect("serialize completed caseload");
            let ff_args = parse_audit(&[
                "canon-clerk",
                "audit",
                "--caseload",
                "-",
                "--format",
                "compact",
            ]);
            let mut compact_out = Vec::new();
            run_audit_with_snapshot_and_provider(
                &ff_args,
                &mut Cursor::new(&serialized),
                &mut compact_out,
                &ws,
                &BTreeMap::new(),
                &provider,
            )
            .expect("fast-forward completed audit");
            assert_eq!(provider.call_count(), 3);
            assert!(
                String::from_utf8(compact_out)
                    .expect("utf8")
                    .contains(".canons/cli/cli-flags-kebab-case.md\t0:0\tpass\t0.97")
            );
        }

        #[test]
        fn test_failing_verdict_exits_1_and_satisfies_github_actions_tripartite_contract() {
            let ws = sample_workspace();
            let provider = FullCascadeProvider::new(
                json!({
                    "cases": [{
                        "canon_path": ".canons/cli/cli-flags-kebab-case.md",
                        "colorability_summary": "Touches CLI flag definition.",
                        "colorability_score": 0.94
                    }]
                }),
                json!({
                    "exhibits": [{
                        "file_path": "src/cli/audit.rs",
                        "admissibility_summary": "Defines `--auditThreshold` flag.",
                        "admissibility_score": 0.96
                    }]
                }),
                json!({
                    "compliance_summary": "`--auditThreshold` violates kebab-case; rename to `--audit-threshold`.",
                    "compliance_score": 0.08,
                    "annotations": [{
                        "path": "src/cli/audit.rs",
                        "start_line": 2,
                        "end_line": 2,
                        "start_column": 5,
                        "end_column": 34,
                        "annotation_level": "failure",
                        "title": "CLI Flags Kebab Case",
                        "message": "Rename `--auditThreshold` to `--audit-threshold`."
                    }]
                }),
            );

            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let tmp_dir = std::env::temp_dir().join(format!("canon-clerk-audit-ci-{nanos}"));
            let step_summary = tmp_dir.join("step-summary.md");
            let archived_caseload = tmp_dir.join("caseload.json");

            let mut env = BTreeMap::new();
            env.insert(ENV_GEMINI_API_KEY.to_string(), "test-key".to_string());
            env.insert("GITHUB_ACTIONS".to_string(), "true".to_string());
            env.insert(
                "GITHUB_STEP_SUMMARY".to_string(),
                step_summary.display().to_string(),
            );

            let archived_str = archived_caseload.display().to_string();
            let args = parse_audit(&[
                "canon-clerk",
                "audit",
                "src/cli/audit.rs",
                "--output-caseload",
                &archived_str,
            ]);
            let mut stdout = Vec::new();
            let err = run_audit_with_snapshot_and_provider(
                &args,
                &mut Cursor::new(b""),
                &mut stdout,
                &ws,
                &env,
                &provider,
            )
            .expect_err("failing verdict must exit 1");

            assert_eq!(err.exit_code(), 1);
            assert!(err.is_silent());

            // 1. Inline workflow command annotation on stdout
            let gh_stdout = String::from_utf8(stdout).expect("utf8");
            assert!(gh_stdout.contains(
                "::error file=src/cli/audit.rs,line=2,endLine=2,col=5,endColumn=34,title=CLI Flags Kebab Case::Rename `--auditThreshold` to `--audit-threshold`."
            ));

            // 2. Step Summary Markdown report written to $GITHUB_STEP_SUMMARY
            let summary_md = fs::read_to_string(&step_summary).expect("read GITHUB_STEP_SUMMARY");
            assert!(summary_md.contains("### Canon Clerk Audit Gate: FAIL"));
            assert!(summary_md.contains("✗ audit     : 0 passed, 1 failed across 1 trial(s)"));
            assert!(
                summary_md.contains("| `.canons/cli/cli-flags-kebab-case.md` | `fail` | `0.08` |")
            );

            // 3. Full Caseload JSON archived to --output-caseload despite exit code 1
            let archived_raw =
                fs::read_to_string(&archived_caseload).expect("read archived caseload.json");
            let archived: Caseload =
                serde_json::from_str(&archived_raw).expect("parse archived Caseload");
            assert_eq!(
                archived.verdict.expect("verdict").status,
                VerdictStatus::Fail
            );

            let _ = fs::remove_dir_all(&tmp_dir);
        }
    }
}
