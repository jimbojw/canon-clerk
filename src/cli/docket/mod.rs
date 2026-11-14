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

//! CLI driving adapter for the `canon-clerk docket` subcommand (`docs/architecture/nodes/docket.md` §5).

pub mod guards;
mod render;
mod stages;

use crate::caseload::{Caseload, CaseloadDocket};
use crate::cli::CliError;
use crate::cli::args::{CanonSelectionArgs, CaseloadIoArgs, FilingSourceArgs, ProviderConfigArgs};
use crate::cli::closure::{ClosureBackend, materialize_closure_snapshot, resolve_closure_validate};
use crate::cli::io::ensure_single_stdin_source;
use crate::cli::provenance::StageDisposition;
use crate::discover::WorkspaceSnapshot;
use crate::provider::StructuredModelProvider;
use crate::telemetry::TelemetrySession;
use clap::{Args, ValueEnum};
use guards::{
    can_fast_forward_completed_docket, enforce_filing_and_case_guard,
    ensure_stage_prefixed_threshold_flags, resolve_docket_batch_size, resolve_docket_concurrency,
    resolve_docket_threshold,
};
use render::{DocketWatermarks, render_docket_output};
use stages::{execute_jit_configure_and_docket, resolve_docket_discover, resolve_docket_intake};
use std::collections::BTreeMap;
use std::env;
use std::io::{Read, Write};

const DOCKET_EXAMPLES: &str = "\
Examples:
  # Screen candidate canons against a unified diff piped on standard input:
  git diff origin/main | canon-clerk docket --diff -

  # Screen explicit target paths or newline-delimited stdin paths:
  canon-clerk docket src/cli src/docket
  git diff origin/main --name-only | canon-clerk docket -

  # Emit tab-separated compact docket lines for shell scripting:
  git diff origin/main | canon-clerk docket --diff - --format compact

  # Execute macro triage against an upstream Caseload and emit JSON:
  canon-clerk docket --caseload caseload-3.json --json";

/// Output presentation format for `canon-clerk docket`.
#[derive(ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DocketFormat {
    /// Human-readable Pipeline Funnel Provenance receipt and Active Docket summary.
    #[default]
    Stylish,
    /// Full cumulative `Caseload` JSON envelope.
    Json,
    /// Tab-separated (`<canon>\t<status>\t<score>\t<summary>`) one-line-per-canon output.
    Compact,
}

/// Command-line arguments for `canon-clerk docket`.
#[derive(Args, Debug, Clone, Default, PartialEq)]
#[command(
    about = "Macro jurisdiction triage: screen candidate canons to open active cases on the docket",
    long_about = "Macro Jurisdiction Triage stage of the Caseload DAG (Cascade Spine). Executes the guarded [intake, discover, validate, configure, docket] closure to screen candidate canons against an in-flight filing in a single structured turn before micro evidentiary triage.",
    after_help = DOCKET_EXAMPLES
)]
pub struct DocketArgs {
    #[command(flatten)]
    pub filing: FilingSourceArgs,

    #[command(flatten)]
    pub canons: CanonSelectionArgs,

    /// Subject-matter jurisdiction score threshold in `[0.0, 1.0]` (default: `0.5`; env: `CANON_CLERK_DOCKET_THRESHOLD`).
    #[arg(
        long = "docket-threshold",
        value_name = "FLOAT",
        allow_hyphen_values = true,
        help_heading = "Docket Options",
        help = "Subject-matter jurisdiction threshold in [0.0, 1.0] (default: 0.5; fallback env: CANON_CLERK_DOCKET_THRESHOLD)"
    )]
    pub docket_threshold: Option<f64>,

    /// Optional maximum candidate canons evaluated per screener model call (env: `CANON_CLERK_DOCKET_BATCH_SIZE`).
    #[arg(
        long = "docket-batch-size",
        visible_alias = "max-canons-per-call",
        value_name = "COUNT",
        help_heading = "Docket Options",
        help = "Maximum candidate canons per screener model call (fallback env: CANON_CLERK_DOCKET_BATCH_SIZE)"
    )]
    pub docket_batch_size: Option<usize>,

    /// Optional maximum concurrent in-flight screener model calls when batched (default: `4`; env: `CANON_CLERK_DOCKET_CONCURRENCY`).
    #[arg(
        long = "docket-concurrency",
        visible_alias = "max-concurrent-calls",
        value_name = "COUNT",
        help_heading = "Docket Options",
        help = "Maximum concurrent in-flight screener calls when batched (default: 4; fallback env: CANON_CLERK_DOCKET_CONCURRENCY)"
    )]
    pub docket_concurrency: Option<usize>,

    /// Hidden capture rejecting ambiguous unqualified `--threshold` (`scheduling.md` §4 Pattern C).
    #[arg(long = "threshold", hide = true, allow_hyphen_values = true)]
    pub rejected_threshold: Option<String>,

    /// Hidden capture rejecting out-of-closure `--apprise-threshold` (`scheduling.md` §4 Pattern C).
    #[arg(long = "apprise-threshold", hide = true, allow_hyphen_values = true)]
    pub rejected_apprise_threshold: Option<String>,

    /// Hidden capture rejecting out-of-closure `--admit-threshold` (`scheduling.md` §4 Pattern C).
    #[arg(long = "admit-threshold", hide = true, allow_hyphen_values = true)]
    pub rejected_admit_threshold: Option<String>,

    /// Hidden capture rejecting out-of-closure `--apprise-batch-size`.
    #[arg(long = "apprise-batch-size", hide = true, allow_hyphen_values = true)]
    pub rejected_apprise_batch_size: Option<String>,

    /// Hidden capture rejecting out-of-closure `--apprise-concurrency`.
    #[arg(long = "apprise-concurrency", hide = true, allow_hyphen_values = true)]
    pub rejected_apprise_concurrency: Option<String>,

    /// Hidden capture rejecting out-of-closure `--admit-concurrency`.
    #[arg(long = "admit-concurrency", hide = true, allow_hyphen_values = true)]
    pub rejected_admit_concurrency: Option<String>,

    #[command(flatten)]
    pub provider: ProviderConfigArgs,

    /// Output format (`stylish`, `json`, or `compact`).
    #[arg(
        long = "format",
        value_enum,
        default_value_t = DocketFormat::Stylish,
        help_heading = "Caseload & Output",
        help = "Format active docket output"
    )]
    pub format: DocketFormat,

    #[command(flatten)]
    pub io: CaseloadIoArgs,
}

impl Eq for DocketArgs {}

/// Executes `canon-clerk docket` hermetically with an explicit [`TelemetrySession`].
pub fn run_docket_with_telemetry(
    args: &DocketArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    workspace: &WorkspaceSnapshot,
    env: &BTreeMap<String, String>,
    provider: &dyn StructuredModelProvider,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    run_docket_core(
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

/// Executes `canon-clerk docket` hermetically against an in-memory [`WorkspaceSnapshot`],
/// explicit environment map, and hexagonal [`StructuredModelProvider`].
pub fn run_docket_with_snapshot_and_provider(
    args: &DocketArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    workspace: &WorkspaceSnapshot,
    env: &BTreeMap<String, String>,
    provider: &dyn StructuredModelProvider,
) -> Result<Caseload, CliError> {
    run_docket_with_telemetry(
        args,
        stdin,
        stdout,
        workspace,
        env,
        provider,
        &TelemetrySession::noop(),
    )
}

/// Executes `canon-clerk docket` against the live working directory with an explicit [`TelemetrySession`].
pub fn run_docket_live_with_telemetry(
    args: &DocketArgs,
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
    run_docket_core(
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

/// Executes `canon-clerk docket` using the current working directory, process environment,
/// and live [`GeminiModelProvider`].
pub fn run_docket(
    args: &DocketArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    run_docket_live_with_telemetry(args, stdin, stdout, &TelemetrySession::noop())
}

fn run_docket_core(
    args: &DocketArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    env: &BTreeMap<String, String>,
    backend: ClosureBackend<'_>,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    ensure_stage_prefixed_threshold_flags(args)?;
    ensure_single_stdin_source(&args.filing, args.io.caseload.as_deref())?;
    let threshold = resolve_docket_threshold(args.docket_threshold, env)?;
    let _validated_batch_size = resolve_docket_batch_size(args.docket_batch_size, env, None)?;
    let _validated_concurrency = resolve_docket_concurrency(args.docket_concurrency, env, None)?;

    let incoming = args.io.read_caseload(stdin)?;
    let intake_opts = args.filing.resolve_intake_options(stdin)?;

    if can_fast_forward_completed_docket(args, &intake_opts, incoming.as_ref())
        && let Some(completed) = incoming
    {
        telemetry.finish_pipeline();
        args.io.write_output_caseload(&completed)?;
        let wm = pre_populated_watermarks(threshold);
        render_docket_output(args, &completed, &wm, stdout)?;
        return Ok(completed);
    }

    enforce_filing_and_case_guard(&intake_opts, &args.canons, incoming.as_ref())?;

    let (caseload_after_intake, intake_disp, intake_changed) =
        resolve_docket_intake(&intake_opts, incoming, telemetry)?;

    let snapshot =
        materialize_closure_snapshot(&backend, &args.canons.canon, &caseload_after_intake)?;
    let total_workspace_canons = Some(snapshot.canons.len());

    let (caseload_after_discover, discover_disp) = resolve_docket_discover(
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
        return finish_zero_candidate_docket(
            args,
            caseload_after_discover,
            intake_disp,
            total_workspace_canons,
            threshold,
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
            threshold,
            stdout,
        );
    }

    let (final_caseload, configure_disp) = execute_jit_configure_and_docket(
        args,
        env,
        threshold,
        caseload_after_validate,
        &snapshot,
        &backend,
        telemetry,
    )?;
    telemetry.finish_pipeline();

    args.io.write_output_caseload(&final_caseload)?;
    let wm = DocketWatermarks {
        intake: intake_disp,
        discover: discover_disp,
        validate: validate_disp,
        configure: configure_disp,
        docket: StageDisposition::Executed,
        total_workspace_canons,
        threshold,
    };
    render_docket_output(args, &final_caseload, &wm, stdout)?;
    Ok(final_caseload)
}

fn finish_zero_candidate_docket(
    args: &DocketArgs,
    caseload_after_discover: Caseload,
    intake_disp: StageDisposition,
    total_workspace_canons: Option<usize>,
    threshold: f64,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    let short_circuited = caseload_after_discover.with_docket(CaseloadDocket::default());
    args.io.write_output_caseload(&short_circuited)?;
    let wm = DocketWatermarks {
        intake: intake_disp,
        discover: StageDisposition::ZeroCandidateShortCircuit,
        validate: StageDisposition::SkippedZeroCandidates,
        configure: StageDisposition::SkippedZeroCandidates,
        docket: StageDisposition::SkippedZeroCandidates,
        total_workspace_canons,
        threshold,
    };
    render_docket_output(args, &short_circuited, &wm, stdout)?;
    Ok(short_circuited)
}

fn pre_populated_watermarks(threshold: f64) -> DocketWatermarks {
    DocketWatermarks {
        intake: StageDisposition::PrePopulated,
        discover: StageDisposition::PrePopulated,
        validate: StageDisposition::PrePopulated,
        configure: StageDisposition::PrePopulated,
        docket: StageDisposition::PrePopulated,
        total_workspace_canons: None,
        threshold,
    }
}

fn handle_validation_failure(
    args: &DocketArgs,
    caseload: &Caseload,
    intake_disp: StageDisposition,
    discover_disp: StageDisposition,
    total_workspace_canons: Option<usize>,
    threshold: f64,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    args.io.write_output_caseload(caseload)?;
    let wm = DocketWatermarks {
        intake: intake_disp,
        discover: discover_disp,
        validate: StageDisposition::ValidationFailed,
        configure: StageDisposition::SkippedValidationFailed,
        docket: StageDisposition::SkippedValidationFailed,
        total_workspace_canons,
        threshold,
    };
    if args.io.json || args.format != DocketFormat::Compact {
        render_docket_output(args, caseload, &wm, stdout)?;
    }

    let total_errors: usize = caseload
        .validation
        .as_ref()
        .map_or(1, |v| v.results.values().map(|m| m.error_count).sum());
    Err(CliError::runtime(format!(
        "Candidate canon validation failed with {total_errors} error(s); aborting `docket` before model spend.\n  \
         Hint: Run `canon-clerk validate` to inspect and repair malformed candidate canons."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::ParseCanonOptions;
    use crate::caseload::{
        CaseloadConfig, CaseloadDiscovery, CaseloadIntake, CaseloadValidation,
        DEFAULT_SCREENER_MODEL,
    };
    use crate::cli::{Cli, Commands};
    use crate::configure::ENV_GEMINI_API_KEY;
    use crate::docket::ENV_DOCKET_THRESHOLD;
    use crate::provider::{GenerationRequest, GenerationResponse, ProviderError, TokenUsage};
    use crate::telemetry::{
        PipelineStage, RecordingTelemetrySink, SteppingTelemetryClock, TelemetryEvent,
    };
    use clap::{CommandFactory, Parser};
    use serde_json::{Value, json};
    use std::io::Cursor;
    use std::sync::Mutex;

    struct StubProvider {
        calls: Mutex<usize>,
        response: Result<Value, ProviderError>,
        thoughts: Vec<String>,
    }

    impl StubProvider {
        fn succeeding(payload: Value) -> Self {
            Self {
                calls: Mutex::new(0),
                response: Ok(payload),
                thoughts: Vec::new(),
            }
        }

        fn succeeding_with_thoughts(payload: Value, thoughts: Vec<String>) -> Self {
            Self {
                calls: Mutex::new(0),
                response: Ok(payload),
                thoughts,
            }
        }

        fn failing(err: ProviderError) -> Self {
            Self {
                calls: Mutex::new(0),
                response: Err(err),
                thoughts: Vec::new(),
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().expect("lock calls")
        }
    }

    impl StructuredModelProvider for StubProvider {
        fn generate_structured(
            &self,
            _request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            *self.calls.lock().expect("lock calls") += 1;
            match &self.response {
                Ok(payload) => Ok(GenerationResponse {
                    payload: payload.clone(),
                    thoughts: self.thoughts.clone(),
                    usage: TokenUsage {
                        prompt_tokens: 240,
                        completion_tokens: 60,
                        reasoning_tokens: 0,
                        total_tokens: 300,
                    },
                    latency_ms: 42,
                    resolved_model: "gemini-2.5-flash-lite".to_string(),
                }),
                Err(err) => Err(err.clone()),
            }
        }
    }

    fn parse_docket_args(argv: &[&str]) -> DocketArgs {
        let cli = Cli::try_parse_from(argv).expect("parse docket args");
        match cli.command {
            Some(Commands::Docket(args)) => args,
            other => panic!("expected Docket command, got {other:?}"),
        }
    }

    fn env_with_key() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            ENV_GEMINI_API_KEY.to_string(),
            "test-hermetic-gemini-key".to_string(),
        );
        env.insert(
            "XDG_CONFIG_HOME".to_string(),
            "/nonexistent-canon-clerk-xdg-dir".to_string(),
        );
        env
    }

    fn env_without_key() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            "XDG_CONFIG_HOME".to_string(),
            "/nonexistent-canon-clerk-xdg-dir".to_string(),
        );
        env
    }

    fn sample_workspace() -> WorkspaceSnapshot {
        WorkspaceSnapshot::new()
            .with_file("src/cli/docket.rs")
            .with_file("README.md")
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\n---\n# CLI Flags Must Support Env Var Fallbacks\n\nCLI flags MUST define an environment variable fallback.\n",
                &ParseCanonOptions::with_source_ref(
                    ".canons/cli/cli-flags-must-support-env-var-fallbacks.md",
                ),
            )
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/**\"\n---\n# Database Migrations Must Include Rollback\n\nDatabase schema migrations MUST define rollback steps.\n",
                &ParseCanonOptions::with_source_ref(".canons/db/migrations-rollback.md"),
            )
    }

    const SAMPLE_DIFF: &str = "\
diff --git a/src/cli/docket.rs b/src/cli/docket.rs
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/src/cli/docket.rs
@@ -0,0 +1,4 @@
+pub struct DocketArgs {
+    pub docket_threshold: Option<f64>,
+}
";

    mod help_and_flag_scoping {
        use super::*;

        #[test]
        fn test_help_screen_groups_options_by_concern_and_includes_examples() {
            let mut root = Cli::command();
            let sub = root
                .find_subcommand_mut("docket")
                .expect("docket subcommand registered");
            let help = sub.render_long_help().to_string();

            for expected in [
                "Filing Sources:",
                "Canon Selection:",
                "Docket Options:",
                "Model & Reasoning Options:",
                "Provider Credentials:",
                "Caseload & Output:",
                "--docket-threshold <FLOAT>",
                "CANON_CLERK_DOCKET_THRESHOLD",
                "--docket-batch-size <COUNT>",
                "CANON_CLERK_DOCKET_BATCH_SIZE",
                "--docket-concurrency <COUNT>",
                "CANON_CLERK_DOCKET_CONCURRENCY",
                "Examples:",
                "canon-clerk docket --diff -",
            ] {
                assert!(help.contains(expected), "missing `{expected}` in help");
            }
            assert!(!help.contains("--apprise-threshold"));
            assert!(!help.contains("--admit-threshold"));
        }

        #[test]
        fn test_rejects_unqualified_and_out_of_closure_threshold_flags_with_exit_2() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding(json!({}));

            for (flag, val, expected_hint) in [
                ("--threshold", "0.6", "--docket-threshold"),
                ("--apprise-threshold", "0.6", "--docket-threshold"),
                ("--admit-threshold", "0.6", "--docket-threshold"),
                ("--apprise-batch-size", "15", "--docket-batch-size"),
                ("--apprise-concurrency", "4", "--docket-concurrency"),
            ] {
                let args =
                    parse_docket_args(&["canon-clerk", "docket", "src/cli/docket.rs", flag, val]);
                let mut stdin = Cursor::new(b"");
                let mut stdout = Vec::new();
                let err = run_docket_with_snapshot_and_provider(
                    &args,
                    &mut stdin,
                    &mut stdout,
                    &ws,
                    &env_with_key(),
                    &provider,
                )
                .expect_err("disallowed threshold/batching flag must fail");
                assert_eq!(err.exit_code(), 2);
                assert!(err.to_string().contains(expected_hint));
            }
            assert_eq!(provider.call_count(), 0);
        }

        #[test]
        fn test_validates_docket_threshold_flag_and_env_fallback() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding(json!({
                "cases": [
                    {
                        "canon_path": ".canons/cli/cli-flags-must-support-env-var-fallbacks.md",
                        "colorability_summary": "Adds CLI flag.",
                        "colorability_score": 0.72
                    },
                    {
                        "canon_path": ".canons/db/migrations-rollback.md",
                        "colorability_summary": "No DB changes.",
                        "colorability_score": 0.05
                    }
                ]
            }));

            // Invalid negative flag
            let neg_args = parse_docket_args(&[
                "canon-clerk",
                "docket",
                "src/cli/docket.rs",
                "--docket-threshold",
                "-0.2",
            ]);
            let err = run_docket_with_snapshot_and_provider(
                &neg_args,
                &mut Cursor::new(b""),
                &mut Vec::new(),
                &ws,
                &env_with_key(),
                &provider,
            )
            .expect_err("negative threshold must fail");
            assert_eq!(err.exit_code(), 2);

            // Invalid env fallback
            let mut bad_env = env_with_key();
            bad_env.insert(ENV_DOCKET_THRESHOLD.to_string(), "not-a-float".to_string());
            let plain_args = parse_docket_args(&["canon-clerk", "docket", "src/cli/docket.rs"]);
            let err = run_docket_with_snapshot_and_provider(
                &plain_args,
                &mut Cursor::new(b""),
                &mut Vec::new(),
                &ws,
                &bad_env,
                &provider,
            )
            .expect_err("non-numeric env threshold must fail");
            assert_eq!(err.exit_code(), 2);

            // Env fallback = 0.80 dismisses score 0.72, while CLI flag 0.70 overrides env and dockets it
            let mut strict_env = env_with_key();
            strict_env.insert(ENV_DOCKET_THRESHOLD.to_string(), "0.80".to_string());
            let env_res = run_docket_with_snapshot_and_provider(
                &plain_args,
                &mut Cursor::new(b""),
                &mut Vec::new(),
                &ws,
                &strict_env,
                &provider,
            )
            .expect("env threshold 0.80 succeeds");
            assert!(env_res.docket.expect("docket").active_docket.is_empty());

            let override_args = parse_docket_args(&[
                "canon-clerk",
                "docket",
                "src/cli/docket.rs",
                "--docket-threshold",
                "0.70",
            ]);
            let flag_res = run_docket_with_snapshot_and_provider(
                &override_args,
                &mut Cursor::new(b""),
                &mut Vec::new(),
                &ws,
                &strict_env,
                &provider,
            )
            .expect("flag overrides env");
            assert_eq!(
                flag_res.docket.expect("docket").active_docket,
                vec![".canons/cli/cli-flags-must-support-env-var-fallbacks.md".to_string()]
            );
        }

        #[test]
        fn test_validates_docket_batch_size_and_concurrency_flags_and_env() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding(json!({}));
            let plain_args = parse_docket_args(&["canon-clerk", "docket", "src/cli/docket.rs"]);

            // Validate zero batch size and zero concurrency flags and env vars fail with exit 2
            for bad_argv in [
                vec![
                    "canon-clerk",
                    "docket",
                    "src/cli/docket.rs",
                    "--docket-batch-size",
                    "0",
                ],
                vec![
                    "canon-clerk",
                    "docket",
                    "src/cli/docket.rs",
                    "--docket-concurrency",
                    "0",
                ],
            ] {
                let bad_args = parse_docket_args(&bad_argv);
                let err = run_docket_with_snapshot_and_provider(
                    &bad_args,
                    &mut Cursor::new(b""),
                    &mut Vec::new(),
                    &ws,
                    &env_with_key(),
                    &provider,
                )
                .expect_err("zero batch/concurrency flag must fail with exit 2");
                assert_eq!(err.exit_code(), 2);
                assert!(err.to_string().contains("Hint:"));
            }

            for env_key in [
                crate::docket::ENV_DOCKET_BATCH_SIZE,
                crate::docket::ENV_DOCKET_CONCURRENCY,
            ] {
                let mut zero_env = env_with_key();
                zero_env.insert(env_key.to_string(), "0".to_string());
                let err = run_docket_with_snapshot_and_provider(
                    &plain_args,
                    &mut Cursor::new(b""),
                    &mut Vec::new(),
                    &ws,
                    &zero_env,
                    &provider,
                )
                .expect_err("zero env batch/concurrency must fail with exit 2");
                assert_eq!(err.exit_code(), 2);
                assert!(err.to_string().contains("Hint:"));
            }
        }
    }

    mod case_or_controversy_and_lazy_scheduling {
        use super::*;

        #[test]
        fn test_rejects_naked_canon_only_and_corpus_only_caseload_with_exit_2() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding(json!({}));

            for argv in [
                vec!["canon-clerk", "docket"],
                vec!["canon-clerk", "docket", "--all-canons"],
                vec![
                    "canon-clerk",
                    "docket",
                    "--canon",
                    ".canons/cli/cli-flags-must-support-env-var-fallbacks.md",
                ],
                vec![
                    "canon-clerk",
                    "docket",
                    "--intent",
                    "Only prospective intent",
                ],
            ] {
                let args = parse_docket_args(&argv);
                let err = run_docket_with_snapshot_and_provider(
                    &args,
                    &mut Cursor::new(b""),
                    &mut Vec::new(),
                    &ws,
                    &env_without_key(),
                    &provider,
                )
                .expect_err("missing concrete filing must fail with exit 2");
                assert_eq!(err.exit_code(), 2);
                assert!(err.to_string().contains("Hint:"));
            }

            // Abstract corpus-only caseload (no `.intake`, e.g. from `validate --all-canons`)
            let corpus_only = Caseload::new()
                .with_discovery(CaseloadDiscovery {
                    candidate_canons: vec![
                        ".canons/cli/cli-flags-must-support-env-var-fallbacks.md".to_string(),
                    ],
                    ..CaseloadDiscovery::default()
                })
                .with_validation(CaseloadValidation::default());
            let caseload_bytes =
                serde_json::to_vec(&corpus_only).expect("serialize corpus caseload");
            let caseload_args = parse_docket_args(&["canon-clerk", "docket", "--caseload", "-"]);
            let err = run_docket_with_snapshot_and_provider(
                &caseload_args,
                &mut Cursor::new(caseload_bytes),
                &mut Vec::new(),
                &ws,
                &env_with_key(),
                &provider,
            )
            .expect_err("corpus-only caseload without .intake must fail with exit 2");
            assert_eq!(err.exit_code(), 2);
            assert!(err.to_string().contains("Case-or-Controversy"));
            assert_eq!(provider.call_count(), 0);
        }

        #[test]
        fn test_empty_diff_and_zero_candidate_streams_exit_0_without_gemini_api_key() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding(json!({}));

            // 1. Empty unified diff stream on stdin (`--diff -`) without GEMINI_API_KEY
            let diff_args = parse_docket_args(&["canon-clerk", "docket", "--diff", "-"]);
            let mut out = Vec::new();
            let res = run_docket_with_snapshot_and_provider(
                &diff_args,
                &mut Cursor::new(b""),
                &mut out,
                &ws,
                &env_without_key(),
                &provider,
            )
            .expect("empty diff stream must short-circuit cleanly with exit 0");
            assert_eq!(res.docket, Some(CaseloadDocket::default()));
            assert!(res.validation.is_none());
            assert!(res.config.is_none());
            assert_eq!(provider.call_count(), 0);
            let rendered = String::from_utf8(out).expect("utf8");
            assert!(rendered.contains("0 modified files ingested"));
            assert!(rendered.contains("0 candidate canons matched"));
            assert!(rendered.contains("active docket empty"));

            // 2. Ungoverned target file (`README.md`) without GEMINI_API_KEY
            let readme_args = parse_docket_args(&["canon-clerk", "docket", "README.md", "--json"]);
            let mut json_out = Vec::new();
            let readme_res = run_docket_with_snapshot_and_provider(
                &readme_args,
                &mut Cursor::new(b""),
                &mut json_out,
                &ws,
                &env_without_key(),
                &provider,
            )
            .expect("ungoverned target file must exit 0 without API key");
            assert_eq!(readme_res.docket, Some(CaseloadDocket::default()));
            assert_eq!(provider.call_count(), 0);
        }

        #[test]
        fn test_invalid_candidate_canon_exits_1_before_checking_gemini_api_key() {
            let ws = WorkspaceSnapshot::new()
                .with_file("src/cli/docket.rs")
                .with_canon_markdown(
                    "---\nunknown_frontmatter_key: true\ntriggers:\n  - \"src/**\"\n---\n# Bad Canon\n\nRule must hold.\n",
                    &ParseCanonOptions::with_source_ref(".canons/bad-canon.md"),
                );
            let provider = StubProvider::succeeding(json!({}));
            let args = parse_docket_args(&["canon-clerk", "docket", "src/cli/docket.rs"]);
            let mut out = Vec::new();

            let err = run_docket_with_snapshot_and_provider(
                &args,
                &mut Cursor::new(b""),
                &mut out,
                &ws,
                &env_without_key(),
                &provider,
            )
            .expect_err("malformed canon must fail with exit 1 before credential check");
            assert_eq!(err.exit_code(), 1);
            assert_eq!(provider.call_count(), 0);

            let rendered = String::from_utf8(out).expect("utf8");
            assert!(rendered.contains("✗ validate"));
            assert!(rendered.contains("⊘ configure : (fast-forward: canon validation failed)"));
            assert!(rendered.contains("⊘ docket    : (fast-forward: canon validation failed)"));
        }
    }

    mod execution_formats_fast_forward_and_telemetry {
        use super::*;

        fn two_canon_payload() -> Value {
            json!({
                "cases": [
                    {
                        "canon_path": ".canons/db/migrations-rollback.md",
                        "colorability_summary": "Diff adds CLI arguments only;\nzero schema migrations.",
                        "colorability_score": 0.06
                    },
                    {
                        "canon_path": ".canons/cli/cli-flags-must-support-env-var-fallbacks.md",
                        "colorability_summary": "Diff introduces `docket_threshold` CLI option.",
                        "colorability_score": 0.94
                    }
                ]
            })
        }

        #[test]
        fn test_stylish_funnel_receipt_and_telemetry_event_stream() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding_with_thoughts(
                two_canon_payload(),
                vec!["Checking CLI flag canon against diff...".to_string()],
            );

            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 10);
            let session = TelemetrySession::new(&sink, &clock);

            let stylish_args = parse_docket_args(&["canon-clerk", "docket", "--diff", "-"]);
            let mut stylish_out = Vec::new();
            run_docket_with_telemetry(
                &stylish_args,
                &mut Cursor::new(SAMPLE_DIFF.as_bytes()),
                &mut stylish_out,
                &ws,
                &env_with_key(),
                &provider,
                &session,
            )
            .expect("stylish docket succeeds");

            let text = String::from_utf8(stylish_out).expect("utf8");
            assert!(text.contains("Pipeline Provenance:"));
            assert!(text.contains("✓ intake    : 1 modified file(s) ingested (+3 / -0 lines)"));
            assert!(text.contains("✓ discover  : 2 candidate canon(s) matched (0 un-triggered)"));
            assert!(
                text.contains("✓ validate  : 2 candidate canon(s) valid (0 errors, 0 warnings)")
            );
            assert!(
                text.contains("✓ configure : screener model 'google:gemini-flash-lite-latest'")
            );
            assert!(
                text.contains("✓ docket    : 1 case(s) colorable (1 dismissed, threshold 0.50)")
            );
            assert!(text.contains("Active Docket (1 docketed, 1 dismissed):"));
            assert!(text.contains(
                "- .canons/cli/cli-flags-must-support-env-var-fallbacks.md [score: 0.94]"
            ));

            let events = sink.events();
            let started_stages: Vec<PipelineStage> = events
                .iter()
                .filter_map(|e| match e {
                    TelemetryEvent::StageStart { stage, .. } => Some(*stage),
                    _ => None,
                })
                .collect();
            assert_eq!(
                started_stages,
                vec![
                    PipelineStage::Intake,
                    PipelineStage::Discover,
                    PipelineStage::Validate,
                    PipelineStage::Configure,
                    PipelineStage::Docket,
                ]
            );
            assert!(events.iter().any(|e| matches!(
                e,
                TelemetryEvent::Thought {
                    stage: PipelineStage::Docket,
                    ..
                }
            )));
            assert!(events.iter().any(|e| matches!(
                e,
                TelemetryEvent::StageFinish {
                    stage: PipelineStage::Docket,
                    usage: Some(TokenUsage {
                        total_tokens: 300,
                        ..
                    }),
                    ..
                }
            )));
            assert!(matches!(
                events.last(),
                Some(TelemetryEvent::PipelineFinish {
                    total_tokens: 300,
                    ..
                })
            ));
        }

        #[test]
        fn test_compact_output_and_completed_docket_fast_forward() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding(two_canon_payload());

            let compact_args = parse_docket_args(&[
                "canon-clerk",
                "docket",
                "src/cli/docket.rs",
                "--format",
                "compact",
            ]);
            let mut compact_out = Vec::new();
            let caseload = run_docket_with_snapshot_and_provider(
                &compact_args,
                &mut Cursor::new(b""),
                &mut compact_out,
                &ws,
                &env_with_key(),
                &provider,
            )
            .expect("compact docket succeeds");
            let compact_text = String::from_utf8(compact_out).expect("utf8");
            let lines: Vec<&str> = compact_text.lines().collect();
            assert_eq!(lines.len(), 2);
            assert_eq!(
                lines[0],
                ".canons/cli/cli-flags-must-support-env-var-fallbacks.md\tdocketed\t0.94\tDiff introduces `docket_threshold` CLI option."
            );
            assert_eq!(
                lines[1],
                ".canons/db/migrations-rollback.md\tdismissed\t0.06\tDiff adds CLI arguments only; zero schema migrations."
            );

            // Fast-forward pre-populated completed `.docket` without API key or model call
            let prev_calls = provider.call_count();
            let completed_json = serde_json::to_vec(&caseload).expect("serialize completed");
            let ff_args = parse_docket_args(&["canon-clerk", "docket", "--caseload", "-"]);
            let mut ff_out = Vec::new();
            let ff_res = run_docket_with_snapshot_and_provider(
                &ff_args,
                &mut Cursor::new(completed_json),
                &mut ff_out,
                &ws,
                &env_without_key(),
                &provider,
            )
            .expect("pre-populated docket fast-forwards without API key");
            assert_eq!(ff_res, caseload);
            assert_eq!(provider.call_count(), prev_calls);
            let ff_text = String::from_utf8(ff_out).expect("utf8");
            assert!(ff_text.contains(
                "↷ docket    : 1 case(s) colorable (1 dismissed, threshold 0.50) (fast-forward: pre-populated)"
            ));
        }

        #[test]
        fn test_empty_docket_watermark_when_all_scores_below_threshold() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding(two_canon_payload());
            let high_thresh_args = parse_docket_args(&[
                "canon-clerk",
                "docket",
                "src/cli/docket.rs",
                "--docket-threshold",
                "0.99",
            ]);
            let mut empty_docket_out = Vec::new();
            let empty_res = run_docket_with_snapshot_and_provider(
                &high_thresh_args,
                &mut Cursor::new(b""),
                &mut empty_docket_out,
                &ws,
                &env_with_key(),
                &provider,
            )
            .expect("empty docket exits 0");
            assert!(empty_res.docket.expect("docket").active_docket.is_empty());
            let empty_text = String::from_utf8(empty_docket_out).expect("utf8");
            assert!(
                empty_text
                    .contains("⊘ docket    : 0 case(s) colorable (2 dismissed, threshold 0.99)")
            );
            assert!(
                empty_text.contains("0 colorable cases at threshold 0.99; active docket empty")
            );
        }

        #[test]
        fn test_partial_upstream_caseload_fast_forwards_branch_a_stages() {
            let ws = sample_workspace();
            let provider = StubProvider::succeeding(json!({
                "cases": [
                    {
                        "canon_path": ".canons/cli/cli-flags-must-support-env-var-fallbacks.md",
                        "colorability_summary": "Governs CLI flag.",
                        "colorability_score": 0.91
                    }
                ]
            }));

            let upstream = Caseload::new()
                .with_intake(CaseloadIntake {
                    target_paths: vec!["src/cli/docket.rs".to_string()],
                    ..CaseloadIntake::default()
                })
                .with_discovery(CaseloadDiscovery {
                    target_files: vec!["src/cli/docket.rs".to_string()],
                    candidate_canons: vec![
                        ".canons/cli/cli-flags-must-support-env-var-fallbacks.md".to_string(),
                    ],
                    ..CaseloadDiscovery::default()
                })
                .with_validation(CaseloadValidation::default())
                .with_config(CaseloadConfig {
                    screener_model: DEFAULT_SCREENER_MODEL.to_string(),
                    ..CaseloadConfig::default()
                });

            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 5);
            let session = TelemetrySession::new(&sink, &clock);

            let args = parse_docket_args(&["canon-clerk", "docket", "--caseload", "-", "--json"]);
            let mut out = Vec::new();
            let enriched = run_docket_with_telemetry(
                &args,
                &mut Cursor::new(serde_json::to_vec(&upstream).expect("json")),
                &mut out,
                &ws,
                &env_with_key(),
                &provider,
                &session,
            )
            .expect("fast-forward upstream Branch A + B");

            let decoded: Caseload = serde_json::from_slice(&out).expect("valid Caseload JSON");
            assert_eq!(decoded, enriched);

            let started: Vec<PipelineStage> = sink
                .events()
                .iter()
                .filter_map(|e| match e {
                    TelemetryEvent::StageStart { stage, .. } => Some(*stage),
                    _ => None,
                })
                .collect();
            assert_eq!(
                started,
                vec![PipelineStage::Docket],
                "pre-populated intake/discover/validate/configure must fast-forward without emitting StageStart"
            );
        }

        #[test]
        fn test_log_file_jsonl_and_output_caseload_write_cleanly_without_polluting_stdout() {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let temp_dir = std::env::temp_dir().join(format!("canon-clerk-docket-test-{unique}"));
            let log_path = temp_dir.join("docket-events.jsonl");
            let out_caseload_path = temp_dir.join("docket-caseload.json");

            let ws = sample_workspace();
            let provider = StubProvider::succeeding(two_canon_payload());
            let sink = crate::cli::build_cli_telemetry_sink(
                Some(&log_path),
                crate::cli::StderrTelemetryOptions::from_env(false, false, false, &BTreeMap::new()),
            )
            .expect("open log sink");
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 12);
            let session = TelemetrySession::new(&sink, &clock);

            let out_path_str = out_caseload_path.to_string_lossy().to_string();
            let args = parse_docket_args(&[
                "canon-clerk",
                "docket",
                "src/cli/docket.rs",
                "--json",
                "--output-caseload",
                &out_path_str,
            ]);
            let mut stdout = Vec::new();
            let returned = run_docket_with_telemetry(
                &args,
                &mut Cursor::new(b""),
                &mut stdout,
                &ws,
                &env_with_key(),
                &provider,
                &session,
            )
            .expect("docket with --log-file and --output-caseload succeeds");
            drop(sink);

            let stdout_caseload: Caseload =
                serde_json::from_slice(&stdout).expect("stdout must remain pure Caseload JSON");
            let disk_caseload: Caseload = serde_json::from_str(
                &std::fs::read_to_string(&out_caseload_path).expect("read output caseload"),
            )
            .expect("valid output caseload JSON");
            assert_eq!(stdout_caseload, returned);
            assert_eq!(disk_caseload, returned);

            let raw_jsonl = std::fs::read_to_string(&log_path).expect("read jsonl log");
            let events: Vec<TelemetryEvent> = raw_jsonl
                .lines()
                .map(|l| serde_json::from_str(l).expect("valid TelemetryEvent JSONL line"))
                .collect();
            assert!(events.iter().any(|e| matches!(
                e,
                TelemetryEvent::StageStart {
                    stage: PipelineStage::Docket,
                    ..
                }
            )));
            assert!(events.iter().any(|e| matches!(
                e,
                TelemetryEvent::StageFinish {
                    stage: PipelineStage::Docket,
                    ..
                }
            )));

            let _ = std::fs::remove_dir_all(&temp_dir);
        }

        #[test]
        fn test_batched_cli_execution_aggregates_multi_batch_telemetry_tokens() {
            struct PerBatchProvider {
                calls: Mutex<usize>,
            }
            impl StructuredModelProvider for PerBatchProvider {
                fn generate_structured(
                    &self,
                    request: &GenerationRequest,
                ) -> Result<GenerationResponse, ProviderError> {
                    *self.calls.lock().expect("lock") += 1;
                    let mut cases = Vec::new();
                    for line in request.user_prompt.lines() {
                        if let Some(rest) = line.strip_prefix("### Canon: `")
                            && let Some(canon_ref) = rest.strip_suffix('`')
                        {
                            cases.push(json!({
                                "canon_path": canon_ref,
                                "colorability_summary": "Evaluated in batch.",
                                "colorability_score": 0.85
                            }));
                        }
                    }
                    Ok(GenerationResponse {
                        payload: json!({ "cases": cases }),
                        thoughts: Vec::new(),
                        usage: TokenUsage {
                            prompt_tokens: 150,
                            completion_tokens: 50,
                            reasoning_tokens: 0,
                            total_tokens: 200,
                        },
                        latency_ms: 20,
                        resolved_model: "gemini-2.5-flash-lite".to_string(),
                    })
                }
            }

            let ws = sample_workspace();
            let provider = PerBatchProvider {
                calls: Mutex::new(0),
            };
            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 10);
            let session = TelemetrySession::new(&sink, &clock);

            let args = parse_docket_args(&[
                "canon-clerk",
                "docket",
                "src/cli/docket.rs",
                "--docket-batch-size",
                "1",
                "--docket-concurrency",
                "2",
                "--json",
            ]);
            let mut stdout = Vec::new();
            let caseload = run_docket_with_telemetry(
                &args,
                &mut Cursor::new(b""),
                &mut stdout,
                &ws,
                &env_with_key(),
                &provider,
                &session,
            )
            .expect("batched docket succeeds");

            assert_eq!(*provider.calls.lock().expect("lock"), 2);
            let docket = caseload.docket.expect("docket populated");
            assert_eq!(docket.cases.len(), 2);
            assert_eq!(docket.docketed_count(), 2);

            let events = sink.events();
            assert!(events.iter().any(|e| matches!(
                e,
                TelemetryEvent::StageFinish {
                    stage: PipelineStage::Docket,
                    usage: Some(TokenUsage {
                        prompt_tokens: 300,
                        completion_tokens: 100,
                        reasoning_tokens: 0,
                        total_tokens: 400,
                    }),
                    ..
                }
            )));
            assert!(events.iter().any(|e| matches!(
                e,
                TelemetryEvent::PipelineFinish {
                    total_tokens: 400,
                    ..
                }
            )));
        }
    }

    mod provider_and_schema_errors {
        use super::*;

        #[test]
        fn test_maps_missing_credentials_provider_auth_and_schema_errors_to_exit_2() {
            let ws = sample_workspace();
            let args = parse_docket_args(&["canon-clerk", "docket", "src/cli/docket.rs"]);

            // Missing GEMINI_API_KEY when candidates > 0
            let ok_provider = StubProvider::succeeding(json!({}));
            let no_key_err = run_docket_with_snapshot_and_provider(
                &args,
                &mut Cursor::new(b""),
                &mut Vec::new(),
                &ws,
                &env_without_key(),
                &ok_provider,
            )
            .expect_err("missing key must fail with exit 2");
            assert_eq!(no_key_err.exit_code(), 2);

            // Schema decode error (incomplete response)
            let incomplete_provider = StubProvider::succeeding(json!({ "cases": [] }));
            let schema_err = run_docket_with_snapshot_and_provider(
                &args,
                &mut Cursor::new(b""),
                &mut Vec::new(),
                &ws,
                &env_with_key(),
                &incomplete_provider,
            )
            .expect_err("incomplete response must fail with exit 2");
            assert_eq!(schema_err.exit_code(), 2);

            // Provider HTTP auth error
            let auth_provider = StubProvider::failing(ProviderError::HttpStatus {
                status_code: 401,
                model_specifier: DEFAULT_SCREENER_MODEL.to_string(),
                detail: "API key invalid".to_string(),
                diagnostic: "HTTP 401: Unauthorized".to_string(),
            });
            let auth_err = run_docket_with_snapshot_and_provider(
                &args,
                &mut Cursor::new(b""),
                &mut Vec::new(),
                &ws,
                &env_with_key(),
                &auth_provider,
            )
            .expect_err("auth failure must exit 2");
            assert_eq!(auth_err.exit_code(), 2);
        }
    }
}
