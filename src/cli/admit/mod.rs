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

//! CLI driving adapter for the `canon-clerk admit` subcommand (`docs/architecture/nodes/admit.md` §5).

pub mod guards;
mod render;
mod stages;

use crate::caseload::{Caseload, CaseloadDocket, CaseloadEvidence};
use crate::cli::CliError;
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
    can_fast_forward_completed_admit, enforce_filing_and_case_guard,
    ensure_stage_prefixed_threshold_flags, resolve_admit_concurrency, resolve_admit_threshold,
};
use render::{AdmitWatermarks, render_admit_output};
use stages::{
    CascadeThresholds, execute_jit_configure_docket_and_admit, resolve_admit_discover,
    resolve_admit_intake,
};
use std::collections::BTreeMap;
use std::env;
use std::io::{Read, Write};

const ADMIT_EXAMPLES: &str = "\
Examples:
  # Screen candidate exhibits against a unified diff piped on standard input:
  git diff origin/main | canon-clerk admit --diff -

  # Screen explicit target paths or newline-delimited stdin paths:
  canon-clerk admit src/cli src/admit
  git diff origin/main --name-only | canon-clerk admit -

  # Emit tab-separated compact exhibit assessments for shell scripting:
  git diff origin/main | canon-clerk admit --diff - --format compact

  # Execute micro evidentiary triage against an upstream Caseload and emit JSON:
  canon-clerk admit --caseload caseload-5.json --json";

/// Output presentation format for `canon-clerk admit`.
#[derive(ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AdmitFormat {
    /// Human-readable Pipeline Funnel Provenance receipt and Evidence Manifest summary.
    #[default]
    Stylish,
    /// Full cumulative `Caseload` JSON envelope.
    Json,
    /// Tab-separated (`<canon>\t<exhibit>\t<status>\t<score>\t<summary>`) output.
    Compact,
}

/// Command-line arguments for `canon-clerk admit`.
#[derive(Args, Debug, Clone, Default, PartialEq)]
#[command(
    about = "Micro evidentiary triage: screen candidate file and metadata exhibits per active case",
    long_about = "Evidence Admissibility Triage stage of the Caseload DAG (Cascade Spine). Executes the guarded [intake, discover, validate, configure, docket, admit] closure to screen candidate file diffs, workspace files, and PR metadata planes per active case prior to judicial adjudication.",
    after_help = ADMIT_EXAMPLES
)]
pub struct AdmitArgs {
    #[command(flatten)]
    pub filing: FilingSourceArgs,

    #[command(flatten)]
    pub canons: CanonSelectionArgs,

    /// Evidentiary admissibility score threshold in `[0.0, 1.0]` (default: `0.5`; env: `CANON_CLERK_ADMIT_THRESHOLD`).
    #[arg(
        long = "admit-threshold",
        value_name = "FLOAT",
        allow_hyphen_values = true,
        help_heading = "Admit Options",
        help = "Evidentiary admissibility threshold in [0.0, 1.0] (default: 0.5; fallback env: CANON_CLERK_ADMIT_THRESHOLD)"
    )]
    pub admit_threshold: Option<f64>,

    /// Optional maximum concurrent in-flight per-case screener calls (default: `4`; env: `CANON_CLERK_ADMIT_CONCURRENCY`).
    #[arg(
        long = "admit-concurrency",
        visible_alias = "max-concurrent-calls",
        value_name = "COUNT",
        help_heading = "Admit Options",
        help = "Maximum concurrent in-flight per-case screener calls (default: 4; fallback env: CANON_CLERK_ADMIT_CONCURRENCY)"
    )]
    pub admit_concurrency: Option<usize>,

    /// Upstream subject-matter jurisdiction threshold in `[0.0, 1.0]` for telescoping runs (default: `0.5`; env: `CANON_CLERK_DOCKET_THRESHOLD`).
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

    /// Output format (`stylish`, `json`, or `compact`).
    #[arg(
        long = "format",
        value_enum,
        default_value_t = AdmitFormat::Stylish,
        help_heading = "Caseload & Output",
        help = "Format evidence manifest output"
    )]
    pub format: AdmitFormat,

    #[command(flatten)]
    pub io: CaseloadIoArgs,
}

impl Eq for AdmitArgs {}

/// Executes `canon-clerk admit` hermetically with an explicit [`TelemetrySession`].
pub fn run_admit_with_telemetry(
    args: &AdmitArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    workspace: &WorkspaceSnapshot,
    env: &BTreeMap<String, String>,
    provider: &dyn StructuredModelProvider,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    run_admit_core(
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

/// Executes `canon-clerk admit` hermetically against an in-memory [`WorkspaceSnapshot`],
/// explicit environment map, and hexagonal [`StructuredModelProvider`].
pub fn run_admit_with_snapshot_and_provider(
    args: &AdmitArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    workspace: &WorkspaceSnapshot,
    env: &BTreeMap<String, String>,
    provider: &dyn StructuredModelProvider,
) -> Result<Caseload, CliError> {
    run_admit_with_telemetry(
        args,
        stdin,
        stdout,
        workspace,
        env,
        provider,
        &TelemetrySession::noop(),
    )
}

/// Executes `canon-clerk admit` against the live working directory with an explicit [`TelemetrySession`].
pub fn run_admit_live_with_telemetry(
    args: &AdmitArgs,
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
    run_admit_core(
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

/// Executes `canon-clerk admit` using the current working directory, process environment,
/// and live [`GeminiModelProvider`].
pub fn run_admit(
    args: &AdmitArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    run_admit_live_with_telemetry(args, stdin, stdout, &TelemetrySession::noop())
}

fn run_admit_core(
    args: &AdmitArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    env: &BTreeMap<String, String>,
    backend: ClosureBackend<'_>,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    ensure_stage_prefixed_threshold_flags(args)?;
    ensure_single_stdin_source(&args.filing, args.io.caseload.as_deref())?;

    let docket_threshold = resolve_docket_threshold(args.docket_threshold, env)?;
    let admit_threshold = resolve_admit_threshold(args.admit_threshold, env)?;
    let _validated_docket_batch = resolve_docket_batch_size(args.docket_batch_size, env, None)?;
    let _validated_docket_conc = resolve_docket_concurrency(args.docket_concurrency, env, None)?;
    let _validated_admit_conc = resolve_admit_concurrency(args.admit_concurrency, env, None)?;

    let incoming = args.io.read_caseload(stdin)?;
    let intake_opts = args.filing.resolve_intake_options(stdin)?;

    if can_fast_forward_completed_admit(args, &intake_opts, incoming.as_ref())
        && let Some(completed) = incoming
    {
        telemetry.finish_pipeline();
        args.io.write_output_caseload(&completed)?;
        let wm = pre_populated_watermarks(docket_threshold, admit_threshold);
        render_admit_output(args, &completed, &wm, stdout)?;
        return Ok(completed);
    }

    enforce_filing_and_case_guard(&intake_opts, &args.canons, incoming.as_ref())?;

    let (caseload_after_intake, intake_disp, intake_changed) =
        resolve_admit_intake(&intake_opts, incoming, telemetry)?;

    let mut snapshot =
        materialize_closure_snapshot(&backend, &args.canons.canon, &caseload_after_intake)?;
    let total_workspace_canons = Some(snapshot.canons.len());

    let (caseload_after_discover, discover_disp) = resolve_admit_discover(
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
        return finish_zero_candidate_admit(
            args,
            caseload_after_discover,
            intake_disp,
            total_workspace_canons,
            docket_threshold,
            admit_threshold,
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
            docket_threshold,
            admit_threshold,
            stdout,
        );
    }

    let outcome = execute_jit_configure_docket_and_admit(
        args,
        env,
        CascadeThresholds {
            docket_threshold,
            admit_threshold,
        },
        caseload_after_validate,
        &mut snapshot,
        &backend,
        telemetry,
    )?;
    telemetry.finish_pipeline();

    args.io.write_output_caseload(&outcome.caseload)?;
    let wm = AdmitWatermarks {
        intake: intake_disp,
        discover: discover_disp,
        validate: validate_disp,
        configure: outcome.configure_disp,
        docket: outcome.docket_disp,
        admit: outcome.admit_disp,
        total_workspace_canons,
        docket_threshold,
        admit_threshold,
    };
    render_admit_output(args, &outcome.caseload, &wm, stdout)?;
    Ok(outcome.caseload)
}

fn finish_zero_candidate_admit(
    args: &AdmitArgs,
    caseload_after_discover: Caseload,
    intake_disp: StageDisposition,
    total_workspace_canons: Option<usize>,
    docket_threshold: f64,
    admit_threshold: f64,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    let short_circuited = caseload_after_discover
        .with_docket(CaseloadDocket::default())
        .with_evidence(CaseloadEvidence::default());
    args.io.write_output_caseload(&short_circuited)?;
    let wm = AdmitWatermarks {
        intake: intake_disp,
        discover: StageDisposition::ZeroCandidateShortCircuit,
        validate: StageDisposition::SkippedZeroCandidates,
        configure: StageDisposition::SkippedZeroCandidates,
        docket: StageDisposition::SkippedZeroCandidates,
        admit: StageDisposition::SkippedZeroCandidates,
        total_workspace_canons,
        docket_threshold,
        admit_threshold,
    };
    render_admit_output(args, &short_circuited, &wm, stdout)?;
    Ok(short_circuited)
}

fn pre_populated_watermarks(docket_threshold: f64, admit_threshold: f64) -> AdmitWatermarks {
    AdmitWatermarks {
        intake: StageDisposition::PrePopulated,
        discover: StageDisposition::PrePopulated,
        validate: StageDisposition::PrePopulated,
        configure: StageDisposition::PrePopulated,
        docket: StageDisposition::PrePopulated,
        admit: StageDisposition::PrePopulated,
        total_workspace_canons: None,
        docket_threshold,
        admit_threshold,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Internal CLI helper formatting validation failure provenance watermarks across the 6-stage admit closure"
)]
fn handle_validation_failure(
    args: &AdmitArgs,
    caseload: &Caseload,
    intake_disp: StageDisposition,
    discover_disp: StageDisposition,
    total_workspace_canons: Option<usize>,
    docket_threshold: f64,
    admit_threshold: f64,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    args.io.write_output_caseload(caseload)?;
    let wm = AdmitWatermarks {
        intake: intake_disp,
        discover: discover_disp,
        validate: StageDisposition::ValidationFailed,
        configure: StageDisposition::SkippedValidationFailed,
        docket: StageDisposition::SkippedValidationFailed,
        admit: StageDisposition::SkippedValidationFailed,
        total_workspace_canons,
        docket_threshold,
        admit_threshold,
    };
    if args.io.json || args.format != AdmitFormat::Compact {
        render_admit_output(args, caseload, &wm, stdout)?;
    }

    let total_errors: usize = caseload
        .validation
        .as_ref()
        .map_or(1, |v| v.results.values().map(|m| m.error_count).sum());
    Err(CliError::runtime(format!(
        "Candidate canon validation failed with {total_errors} error(s); aborting `admit` before model spend.\n  \
         Hint: Run `canon-clerk validate` to inspect and repair malformed candidate canons."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admit::ENV_ADMIT_THRESHOLD;
    use crate::canon::ParseCanonOptions;
    use crate::caseload::{
        CanonDocketAssessment, CaseloadConfig, CaseloadDiscovery, ColorabilityStatus,
    };
    use crate::cli::{Cli, Commands};
    use crate::configure::ENV_GEMINI_API_KEY;
    use crate::provider::{GenerationRequest, GenerationResponse, ProviderError, TokenUsage};
    use crate::telemetry::{
        PipelineStage, RecordingTelemetrySink, SteppingTelemetryClock, TelemetryEvent,
    };
    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};
    use std::io::Cursor;
    use std::sync::Mutex;

    struct StageRoutingProvider {
        calls: Mutex<usize>,
        docket_payload: Value,
        admit_payload: Value,
    }

    impl StageRoutingProvider {
        fn new(docket_payload: Value, admit_payload: Value) -> Self {
            Self {
                calls: Mutex::new(0),
                docket_payload,
                admit_payload,
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().expect("lock calls")
        }
    }

    impl StructuredModelProvider for StageRoutingProvider {
        fn generate_structured(
            &self,
            request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            *self.calls.lock().expect("lock calls") += 1;
            let payload = if request.system_instruction.contains("Macro Subject-Matter") {
                self.docket_payload.clone()
            } else {
                self.admit_payload.clone()
            };
            Ok(GenerationResponse {
                payload,
                thoughts: vec!["Evaluating relevance...".to_string()],
                usage: TokenUsage {
                    prompt_tokens: 200,
                    completion_tokens: 50,
                    reasoning_tokens: 0,
                    total_tokens: 250,
                },
                latency_ms: 35,
                resolved_model: "gemini-2.5-flash-lite".to_string(),
            })
        }
    }

    fn sample_workspace() -> WorkspaceSnapshot {
        WorkspaceSnapshot::new()
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ninspect:\n  - diff\n  - pr_title?\n---\n# CLI Flags Kebab Case\n\nCLI flags MUST use kebab-case naming.\n",
                &ParseCanonOptions::with_source_ref(".canons/cli/cli-flags-kebab-case.md"),
            )
            .with_file_content("src/cli/admit.rs", "#[arg(long = \"admit-threshold\")]\n")
    }

    fn parse_admit(argv: &[&str]) -> AdmitArgs {
        let cli = Cli::try_parse_from(argv).expect("parse admit args");
        match cli.command {
            Some(Commands::Admit(args)) => args,
            _ => panic!("expected Admit subcommand"),
        }
    }

    mod cli_flags_and_guards {
        use super::*;

        #[test]
        fn test_help_groups_and_stage_prefixed_flag_scoping() {
            let mut cmd = Cli::command();
            let sub = cmd.find_subcommand_mut("admit").expect("admit subcommand");
            let help = sub.render_help().to_string();
            assert!(help.contains("Admit Options:"));
            assert!(help.contains("Docket Options:"));
            assert!(help.contains("CANON_CLERK_ADMIT_THRESHOLD"));
            assert!(help.contains("CANON_CLERK_ADMIT_CONCURRENCY"));
            assert!(help.contains("Examples:"));

            let ws = sample_workspace();
            let provider = StageRoutingProvider::new(json!({}), json!({}));
            let env = BTreeMap::new();

            for bad_argv in [
                vec![
                    "canon-clerk",
                    "admit",
                    "src/cli/admit.rs",
                    "--threshold",
                    "0.7",
                ],
                vec![
                    "canon-clerk",
                    "admit",
                    "src/cli/admit.rs",
                    "--apprise-threshold",
                    "0.7",
                ],
                vec![
                    "canon-clerk",
                    "admit",
                    "src/cli/admit.rs",
                    "--apprise-batch-size",
                    "10",
                ],
                vec![
                    "canon-clerk",
                    "admit",
                    "src/cli/admit.rs",
                    "--apprise-concurrency",
                    "2",
                ],
                vec!["canon-clerk", "admit"],
                vec!["canon-clerk", "admit", "--all-canons"],
                vec![
                    "canon-clerk",
                    "admit",
                    "src/cli/admit.rs",
                    "--admit-concurrency",
                    "0",
                ],
                vec![
                    "canon-clerk",
                    "admit",
                    "src/cli/admit.rs",
                    "--admit-threshold",
                    "1.5",
                ],
            ] {
                let args = parse_admit(&bad_argv);
                let mut stdin = Cursor::new(b"");
                let mut stdout = Vec::new();
                let err = run_admit_with_snapshot_and_provider(
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
        fn test_zero_candidate_and_invalid_canon_precede_gemini_api_key() {
            let ws = sample_workspace();
            let provider = StageRoutingProvider::new(json!({}), json!({}));
            let mut empty_env = BTreeMap::new();
            empty_env.insert(
                "XDG_CONFIG_HOME".to_string(),
                "/nonexistent-canon-clerk-xdg-dir".to_string(),
            );

            // 1. 0-candidate run succeeds (exit 0) without GEMINI_API_KEY
            let zero_args = parse_admit(&["canon-clerk", "admit", "README.md"]);
            let mut stdin = Cursor::new(b"");
            let mut stdout = Vec::new();
            let zero_out = run_admit_with_snapshot_and_provider(
                &zero_args,
                &mut stdin,
                &mut stdout,
                &ws,
                &empty_env,
                &provider,
            )
            .expect("0-candidate short-circuit must succeed without API key");
            assert_eq!(provider.call_count(), 0);
            assert!(zero_out.evidence.expect("evidence").active_cases.is_empty());
            let text = String::from_utf8(stdout).expect("utf8");
            assert!(text.contains("⊘ admit     : (fast-forward: zero candidate canons)"));

            // 2. Malformed candidate canon fails with exit 1 before checking GEMINI_API_KEY
            let broken_ws = WorkspaceSnapshot::new()
                .with_canon_markdown(
                    "---\nunknown_frontmatter_key: true\ntriggers:\n  - \"src/**\"\n---\n# Bad Canon\n\nRule must hold.\n",
                    &ParseCanonOptions::with_source_ref(".canons/broken.md"),
                )
                .with_file("src/cli/admit.rs");
            let val_args = parse_admit(&["canon-clerk", "admit", "src/cli/admit.rs"]);
            let mut out_val = Vec::new();
            let val_err = run_admit_with_snapshot_and_provider(
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
            let val_rendered = String::from_utf8(out_val).expect("utf8");
            assert!(val_rendered.contains("⊘ admit     : (fast-forward: canon validation failed)"));
        }
    }

    mod telescoping_and_fast_forward_execution {
        use super::*;

        fn default_routing_provider() -> StageRoutingProvider {
            StageRoutingProvider::new(
                json!({
                    "cases": [{
                        "canon_path": ".canons/cli/cli-flags-kebab-case.md",
                        "colorability_summary": "Touches CLI flag definition.",
                        "colorability_score": 0.92
                    }]
                }),
                json!({
                    "exhibits": [
                        {
                            "file_path": "pr_title",
                            "admissibility_summary": "Title is orthogonal to flag casing.",
                            "admissibility_score": 0.15
                        },
                        {
                            "file_path": "src/cli/admit.rs",
                            "admissibility_summary": "Defines `--admit-threshold` flag.",
                            "admissibility_score": 0.95
                        }
                    ]
                }),
            )
        }

        #[test]
        fn test_telescoping_closure_emits_telemetry_and_renders_stylish() {
            let ws = sample_workspace();
            let provider = default_routing_provider();
            let mut env = BTreeMap::new();
            env.insert(ENV_GEMINI_API_KEY.to_string(), "test-key".to_string());
            env.insert(ENV_ADMIT_THRESHOLD.to_string(), "0.60".to_string());

            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 25);
            let session = TelemetrySession::new(&sink, &clock);

            let args = parse_admit(&[
                "canon-clerk",
                "admit",
                "--pr-title",
                "feat(cli): add admit",
                "src/cli/admit.rs",
            ]);
            let mut stdout = Vec::new();
            run_admit_with_telemetry(
                &args,
                &mut Cursor::new(b""),
                &mut stdout,
                &ws,
                &env,
                &provider,
                &session,
            )
            .expect("telescoping admit run");

            assert_eq!(provider.call_count(), 2);
            let rendered = String::from_utf8(stdout).expect("utf8");
            assert!(rendered.contains("✓ docket    : 1 case(s) colorable"));
            assert!(rendered.contains(
                "✓ admit     : 1 exhibit(s) admitted across 1 case(s) (1 excluded, threshold 0.60)"
            ));
            assert!(rendered.contains("Case: .canons/cli/cli-flags-kebab-case.md (1 admitted):"));
            assert!(rendered.contains("- src/cli/admit.rs [score: 0.95]"));

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
                ]
            );
        }

        #[test]
        fn test_completed_caseload_fast_forwards_in_compact_and_json_modes() {
            let ws = sample_workspace();
            let provider = default_routing_provider();
            let mut env = BTreeMap::new();
            env.insert(ENV_GEMINI_API_KEY.to_string(), "test-key".to_string());

            let args = parse_admit(&[
                "canon-clerk",
                "admit",
                "--pr-title",
                "feat(cli): add admit",
                "src/cli/admit.rs",
                "--json",
            ]);
            let mut json_stdout = Vec::new();
            let caseload = run_admit_with_snapshot_and_provider(
                &args,
                &mut Cursor::new(b""),
                &mut json_stdout,
                &ws,
                &env,
                &provider,
            )
            .expect("initial json admit run");
            assert_eq!(provider.call_count(), 2);

            let ff_args = parse_admit(&[
                "canon-clerk",
                "admit",
                "--caseload",
                "-",
                "--format",
                "compact",
            ]);
            let mut compact_out = Vec::new();
            run_admit_with_snapshot_and_provider(
                &ff_args,
                &mut Cursor::new(&json_stdout),
                &mut compact_out,
                &ws,
                &BTreeMap::new(),
                &provider,
            )
            .expect("fast-forward completed admit without API key");
            assert_eq!(provider.call_count(), 2);
            assert!(caseload.evidence.is_some());

            let compact_str = String::from_utf8(compact_out).expect("utf8");
            let lines: Vec<&str> = compact_str.lines().collect();
            assert_eq!(lines.len(), 2);
            assert!(
                lines[0]
                    .starts_with(".canons/cli/cli-flags-kebab-case.md\tpr_title\texcluded\t0.15\t")
            );
            assert!(lines[1].starts_with(
                ".canons/cli/cli-flags-kebab-case.md\tsrc/cli/admit.rs\tadmitted\t0.95\t"
            ));
        }

        #[test]
        fn test_pre_populated_empty_docket_fast_forwards_without_api_key_or_admit_stage_telemetry()
        {
            let ws = sample_workspace();
            let provider = StageRoutingProvider::new(json!({}), json!({}));

            let mut cases = BTreeMap::new();
            cases.insert(
                ".canons/cli/cli-flags-kebab-case.md".to_string(),
                CanonDocketAssessment {
                    colorability_summary: "Dismissed upstream.".to_string(),
                    colorability_score: 0.1,
                    colorability_status: ColorabilityStatus::Dismissed,
                },
            );
            let upstream = Caseload::new()
                .with_intake(crate::caseload::CaseloadIntake {
                    target_paths: vec!["src/cli/admit.rs".to_string()],
                    ..Default::default()
                })
                .with_discovery(CaseloadDiscovery {
                    target_files: vec!["src/cli/admit.rs".to_string()],
                    candidate_canons: vec![".canons/cli/cli-flags-kebab-case.md".to_string()],
                    ..Default::default()
                })
                .with_validation(crate::caseload::CaseloadValidation::default())
                .with_config(CaseloadConfig::default())
                .with_docket(CaseloadDocket::from_cases(cases));
            let upstream_bytes = serde_json::to_vec(&upstream).expect("encode upstream");

            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 10);
            let session = TelemetrySession::new(&sink, &clock);

            let args = parse_admit(&["canon-clerk", "admit", "--caseload", "-"]);
            let mut stdout = Vec::new();
            let out = run_admit_with_telemetry(
                &args,
                &mut Cursor::new(&upstream_bytes),
                &mut stdout,
                &ws,
                &BTreeMap::new(),
                &provider,
                &session,
            )
            .expect("empty pre-populated docket short-circuits cleanly");

            assert_eq!(provider.call_count(), 0);
            assert!(out.evidence.expect("evidence").active_cases.is_empty());
            let text = String::from_utf8(stdout).expect("utf8");
            assert!(text.contains("⊘ admit     : (fast-forward: empty active docket)"));

            // Bypassed `Admit` stage must not emit StageStart/StageFinish events
            assert!(!sink.events().iter().any(|e| matches!(
                e,
                TelemetryEvent::StageStart {
                    stage: PipelineStage::Admit,
                    ..
                }
            )));
        }
    }
}
