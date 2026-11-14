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

//! Clap CLI driving adapter for the multi-run consistency and flipper benchmark harness.

mod resolve;

use super::prepare::{
    ConfigSweepInputs, UpstreamCaseloadInputs, prepare_upstream_caseload, resolve_config_specs,
};
use super::render::render_consistency_markdown;
use super::{
    BenchmarkStage, ConsistencyBenchOptions, ConsistencyBenchReport, TrialSummary,
    execute_consistency_bench_observed,
};
use crate::canon::ParseCanonOptions;
use crate::caseload::Caseload;
use crate::cli::{
    CanonSelectionArgs, CliError, FilingSourceArgs, ProviderConfigArgs, ensure_single_stdin_source,
    map_discover_error, read_caseload_operand, resolve_branch_b_options, resolve_gemini_api_key,
    write_caseload_file,
};
use crate::discover::WorkspaceSnapshot;
use crate::provider::{
    DEFAULT_GENERATION_TIMEOUT_MS, GeminiModelProvider, StructuredModelProvider,
};
use crate::telemetry::{SystemTelemetryClock, TelemetryClock};
use clap::{Parser, ValueEnum};
use resolve::{ResolvedCliParams, map_bench_error, resolve_cli_parameters};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const BENCH_EXAMPLES: &str = "\
Examples:
  # Run N=5 apprise trials on prospective target scope and print flipper tables:
  ./scripts/bench-consistency.sh --stage apprise --intent \"Add CLI output flag\" src/cli

  # Sweep unbounded vs. batched apprise configurations side-by-side and archive JSON:
  ./scripts/bench-consistency.sh --stage apprise --batch-size unbounded,15,25 --intent \"Add CLI output flag\" src/cli --output-json /tmp/apprise-bench.json

  # Benchmark docket consistency across N=5 trials against a unified diff from stdin:
  git diff origin/main | ./scripts/bench-consistency.sh --stage docket --diff - --batch-size 15

  # Emit structured JSON report directly to stdout:
  ./scripts/bench-consistency.sh --stage apprise --trials 5 --intent \"Refactor parser\" src/canon --json";

/// Output presentation format for the consistency benchmark harness.
#[derive(ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConsistencyBenchFormat {
    /// Human-readable Markdown summary tables and per-trial flipper rationales.
    #[default]
    Markdown,
    /// Structured `ConsistencyBenchReport` JSON payload.
    Json,
}

/// Command-line arguments for the `consistency_bench` developer harness.
#[derive(Parser, Debug, Clone, Default, PartialEq)]
#[command(
    name = "consistency_bench",
    author = "PAIR-code",
    version = env!("CARGO_PKG_VERSION"),
    about = "Multi-run consistency and flipper benchmark harness for Caseload heuristic stages",
    long_about = "Executes N repeated trials of a target Caseload stage (apprise or docket, extensible to admit and audit) against an identical prepared upstream Caseload, reporting status agreement, score standard deviation, latency distribution, and per-run flipper rationales.",
    after_help = BENCH_EXAMPLES
)]
pub struct ConsistencyBenchCli {
    /// Target heuristic stage (`apprise`, `docket`, `admit`, or `audit`; env: `CANON_CLERK_BENCH_STAGE`).
    #[arg(
        short = 's',
        long = "stage",
        value_enum,
        value_name = "STAGE",
        help_heading = "Benchmark Options",
        help = "Target Caseload stage to benchmark (fallback env: CANON_CLERK_BENCH_STAGE)"
    )]
    pub stage: Option<BenchmarkStage>,

    /// Number of repeated trials per configuration (default: `5`; env: `CANON_CLERK_BENCH_TRIALS`).
    #[arg(
        short = 'n',
        long = "trials",
        value_name = "COUNT",
        help_heading = "Benchmark Options",
        help = "Number of repeated trials per configuration (default: 5; fallback env: CANON_CLERK_BENCH_TRIALS)"
    )]
    pub trials: Option<usize>,

    /// Stage score threshold in `[0.0, 1.0]` (default: `0.5`; env: `CANON_CLERK_BENCH_THRESHOLD`).
    #[arg(
        long = "threshold",
        value_name = "FLOAT",
        allow_hyphen_values = true,
        help_heading = "Benchmark Options",
        help = "Stage classification threshold in [0.0, 1.0] (default: 0.5; fallback env: CANON_CLERK_BENCH_THRESHOLD)"
    )]
    pub threshold: Option<f64>,

    /// Candidate canon batch size or comma-separated sweep (`unbounded`, `15`, `unbounded,15,25`; env: `CANON_CLERK_BENCH_BATCH_SIZE`).
    #[arg(
        long = "batch-size",
        value_name = "SPEC",
        help_heading = "Benchmark Options",
        help = "Batch size or comma-separated sweep, e.g. '15' or 'unbounded,15,25' (fallback env: CANON_CLERK_BENCH_BATCH_SIZE)"
    )]
    pub batch_size: Vec<String>,

    /// Maximum concurrent in-flight calls when batched (default: `4`; env: `CANON_CLERK_BENCH_CONCURRENCY`).
    #[arg(
        long = "concurrency",
        value_name = "COUNT",
        help_heading = "Benchmark Options",
        help = "Maximum concurrent in-flight calls when batched (default: 4; fallback env: CANON_CLERK_BENCH_CONCURRENCY)"
    )]
    pub concurrency: Option<usize>,

    /// Include unanimous non-flipper subjects in the Markdown report (`CANON_CLERK_BENCH_INCLUDE_STABLE`).
    #[arg(
        long = "include-stable",
        help_heading = "Benchmark Options",
        help = "Include unanimous non-flipper subjects in Markdown output (fallback env: CANON_CLERK_BENCH_INCLUDE_STABLE)"
    )]
    pub include_stable: bool,

    /// Suppress `[bench]` trial progress lines on standard error.
    #[arg(
        short = 'q',
        long = "quiet",
        help_heading = "Benchmark Options",
        help = "Suppress diagnostic trial progress on stderr"
    )]
    pub quiet: bool,

    #[command(flatten)]
    pub filing: FilingSourceArgs,

    #[command(flatten)]
    pub canons: CanonSelectionArgs,

    #[command(flatten)]
    pub provider: ProviderConfigArgs,

    /// Output format (`markdown` or `json`; env: `CANON_CLERK_BENCH_FORMAT`).
    #[arg(
        long = "format",
        value_enum,
        help_heading = "Caseload & Output",
        help = "Report output format on stdout (default: markdown; fallback env: CANON_CLERK_BENCH_FORMAT)"
    )]
    pub format: Option<ConsistencyBenchFormat>,

    /// Write structured `ConsistencyBenchReport` JSON to a file path independently of stdout (`CANON_CLERK_BENCH_OUTPUT_JSON`).
    #[arg(
        long = "output-json",
        value_name = "PATH",
        help_heading = "Caseload & Output",
        help = "Write structured benchmark report JSON to disk independently of stdout (fallback env: CANON_CLERK_BENCH_OUTPUT_JSON)"
    )]
    pub output_json: Option<PathBuf>,

    /// Emit structured `ConsistencyBenchReport` JSON directly to standard output.
    #[arg(
        long = "json",
        help_heading = "Caseload & Output",
        help = "Emit structured benchmark report JSON to stdout"
    )]
    pub json: bool,

    /// Read an existing upstream `Caseload` JSON from a file path or `-` for stdin.
    #[arg(
        long = "caseload",
        value_name = "PATH|-",
        help_heading = "Caseload & Output",
        help = "Ingest existing upstream Caseload JSON from file or '-' for stdin"
    )]
    pub caseload: Option<String>,

    /// Write the prepared upstream `Caseload` JSON to a file path independently of stdout.
    #[arg(
        long = "output-caseload",
        value_name = "PATH",
        help_heading = "Caseload & Output",
        help = "Write prepared upstream Caseload JSON to disk independently of stdout"
    )]
    pub output_caseload: Option<PathBuf>,
}

impl Eq for ConsistencyBenchCli {}

/// Standard I/O stream bundle decoupling benchmark CLI execution from OS file descriptors.
pub struct BenchIoStreams<'a> {
    /// Standard input reader for `-` operands.
    pub stdin: &'a mut dyn Read,
    /// Standard output writer receiving Markdown or JSON report data.
    pub stdout: &'a mut dyn Write,
    /// Standard error writer receiving `[bench]` progress diagnostics.
    pub stderr: &'a mut dyn Write,
}

/// Hermetic execution dependencies for deterministic testing of [`ConsistencyBenchCli`].
pub struct HermeticBenchDeps<'a> {
    /// Pre-materialized workspace snapshot.
    pub workspace: &'a WorkspaceSnapshot,
    /// Ambient environment variable map.
    pub env: &'a BTreeMap<String, String>,
    /// Injected structured model provider.
    pub provider: &'a dyn StructuredModelProvider,
    /// Monotonic telemetry clock.
    pub clock: &'a dyn TelemetryClock,
}

/// Executes [`ConsistencyBenchCli`] against the live workspace directory and `GeminiModelProvider`.
pub fn run_consistency_bench_cli(
    cli: &ConsistencyBenchCli,
    streams: &mut BenchIoStreams<'_>,
) -> Result<ConsistencyBenchReport, CliError> {
    let cwd = env::current_dir().map_err(|err| {
        CliError::runtime(format!(
            "Failed to determine current working directory: {err}"
        ))
    })?;
    let env_map: BTreeMap<String, String> = env::vars().collect();
    ensure_single_stdin_source(&cli.filing, cli.caseload.as_deref())?;

    let resolved = resolve_cli_parameters(cli, &env_map)?;
    let incoming = read_caseload_operand(cli.caseload.as_deref(), streams.stdin)?;
    let intake_opts = cli.filing.resolve_intake_options(streams.stdin)?;
    let configure_opts = resolve_branch_b_options(&cli.provider, &env_map)?;

    let mut snapshot = materialize_disk_snapshot(&cwd, &cli.canons.canon, incoming.as_ref())?;
    let caseload = prepare_upstream_caseload(&UpstreamCaseloadInputs {
        stage: resolved.stage,
        intake_opts: &intake_opts,
        canons: &cli.canons,
        configure_opts: &configure_opts,
        incoming,
        workspace: &snapshot,
    })
    .map_err(map_bench_error)?;
    if resolved.stage == BenchmarkStage::Admit
        && let Some(disc) = &caseload.discovery
    {
        snapshot.populate_file_contents_from_disk(&cwd, &disc.target_files);
    }

    let specs = resolve_config_specs(&ConfigSweepInputs {
        stage: resolved.stage,
        threshold: resolved.threshold,
        raw_batch_specs: &cli.batch_size,
        flag_concurrency: cli.concurrency,
        env: &env_map,
        stored_config: configure_opts.stored_config.as_ref(),
        timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
    })
    .map_err(map_bench_error)?;

    let bench_opts = ConsistencyBenchOptions {
        stage: resolved.stage,
        trials: resolved.trials,
        configs: specs,
    };

    let has_candidates = caseload
        .discovery
        .as_ref()
        .is_some_and(|d| !d.candidate_canons.is_empty());
    let api_key = if has_candidates {
        resolve_gemini_api_key(&configure_opts)?
    } else {
        String::new()
    };
    let live_provider = GeminiModelProvider::new(api_key);
    let clock = SystemTelemetryClock::new();

    run_prepared_bench_and_emit(
        cli,
        &resolved,
        &bench_opts,
        &caseload,
        &snapshot,
        &live_provider,
        &clock,
        streams,
    )
}

/// Executes [`ConsistencyBenchCli`] hermetically against an in-memory workspace and mock provider.
pub fn run_consistency_bench_hermetic(
    cli: &ConsistencyBenchCli,
    deps: &HermeticBenchDeps<'_>,
    streams: &mut BenchIoStreams<'_>,
) -> Result<ConsistencyBenchReport, CliError> {
    ensure_single_stdin_source(&cli.filing, cli.caseload.as_deref())?;
    let resolved = resolve_cli_parameters(cli, deps.env)?;
    let incoming = read_caseload_operand(cli.caseload.as_deref(), streams.stdin)?;
    let intake_opts = cli.filing.resolve_intake_options(streams.stdin)?;
    let configure_opts = resolve_branch_b_options(&cli.provider, deps.env)?;

    let caseload = prepare_upstream_caseload(&UpstreamCaseloadInputs {
        stage: resolved.stage,
        intake_opts: &intake_opts,
        canons: &cli.canons,
        configure_opts: &configure_opts,
        incoming,
        workspace: deps.workspace,
    })
    .map_err(map_bench_error)?;

    let specs = resolve_config_specs(&ConfigSweepInputs {
        stage: resolved.stage,
        threshold: resolved.threshold,
        raw_batch_specs: &cli.batch_size,
        flag_concurrency: cli.concurrency,
        env: deps.env,
        stored_config: configure_opts.stored_config.as_ref(),
        timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
    })
    .map_err(map_bench_error)?;

    let bench_opts = ConsistencyBenchOptions {
        stage: resolved.stage,
        trials: resolved.trials,
        configs: specs,
    };

    run_prepared_bench_and_emit(
        cli,
        &resolved,
        &bench_opts,
        &caseload,
        deps.workspace,
        deps.provider,
        deps.clock,
        streams,
    )
}

fn materialize_disk_snapshot(
    workspace_root: &Path,
    canon_patterns: &[String],
    incoming: Option<&Caseload>,
) -> Result<WorkspaceSnapshot, CliError> {
    let mut snapshot =
        WorkspaceSnapshot::from_disk(workspace_root, canon_patterns).map_err(map_discover_error)?;
    if let Some(disc) = incoming.and_then(|c| c.discovery.as_ref()) {
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

#[allow(
    clippy::too_many_arguments,
    reason = "Internal bridge passing resolved CLI flags, prepared caseload, workspace, provider, clock, and stream handles"
)]
fn run_prepared_bench_and_emit(
    cli: &ConsistencyBenchCli,
    resolved: &ResolvedCliParams,
    bench_opts: &ConsistencyBenchOptions,
    caseload: &Caseload,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
    clock: &dyn TelemetryClock,
    streams: &mut BenchIoStreams<'_>,
) -> Result<ConsistencyBenchReport, CliError> {
    if let Some(path) = &cli.output_caseload {
        write_caseload_file(path, caseload)?;
    }

    let candidate_count = caseload
        .discovery
        .as_ref()
        .map_or(0, |d| d.candidate_canons.len());
    if !cli.quiet {
        let _ = writeln!(
            streams.stderr,
            "[bench] stage={} candidates={} trials={} configs={}",
            resolved.stage.as_str(),
            candidate_count,
            resolved.trials,
            bench_opts.configs.len()
        );
    }

    let pos_label = resolved.stage.positive_status_label();
    let quiet = cli.quiet;
    let total_trials = resolved.trials;
    let stderr_ref = &mut *streams.stderr;
    let mut observer = |cfg_label: &str, summary: &TrialSummary| {
        if !quiet {
            let _ = writeln!(
                stderr_ref,
                "[bench] [{}] trial {}/{}: {}/{} {} ({}ms, {} tokens)",
                cfg_label,
                summary.trial,
                total_trials,
                summary.positive_count,
                summary.subject_count,
                pos_label,
                summary.duration_ms,
                summary.total_tokens
            );
        }
    };

    let report = execute_consistency_bench_observed(
        bench_opts,
        caseload,
        workspace,
        provider,
        clock,
        &mut observer,
    )
    .map_err(map_bench_error)?;

    if let Some(json_path) = &resolved.output_json {
        write_report_json_file(json_path, &report)?;
    }

    emit_stdout_report(
        &report,
        resolved.format,
        resolved.include_stable,
        streams.stdout,
    )?;
    Ok(report)
}

fn write_report_json_file(path: &Path, report: &ConsistencyBenchReport) -> Result<(), CliError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::usage(format!(
                "Failed to create parent directory `{}` for `--output-json`: {err}\n  \
                 Hint: Verify directory write permissions for `{}`.",
                parent.display(),
                path.display()
            ))
        })?;
    }
    let payload = serde_json::to_string_pretty(report)
        .map_err(|err| CliError::runtime(format!("Failed to serialize benchmark JSON: {err}")))?;
    fs::write(path, format!("{payload}\n")).map_err(|err| {
        CliError::usage(format!(
            "Failed to write benchmark JSON to `{}`: {err}\n  \
             Hint: Verify that `{}` is writable.",
            path.display(),
            path.display()
        ))
    })
}

fn emit_stdout_report(
    report: &ConsistencyBenchReport,
    format: ConsistencyBenchFormat,
    include_stable: bool,
    stdout: &mut dyn Write,
) -> Result<(), CliError> {
    match format {
        ConsistencyBenchFormat::Json => {
            let payload = serde_json::to_string_pretty(report).map_err(|err| {
                CliError::runtime(format!("Failed to serialize benchmark JSON: {err}"))
            })?;
            writeln!(stdout, "{payload}")
                .map_err(|err| CliError::runtime(format!("Failed to write stdout: {err}")))
        }
        ConsistencyBenchFormat::Markdown => {
            let rendered = render_consistency_markdown(report, include_stable);
            write!(stdout, "{rendered}")
                .map_err(|err| CliError::runtime(format!("Failed to write stdout: {err}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{GenerationRequest, GenerationResponse, ProviderError, TokenUsage};
    use crate::telemetry::SteppingTelemetryClock;
    use clap::CommandFactory;
    use serde_json::json;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FlippingAppriseProvider {
        calls: AtomicUsize,
    }

    impl FlippingAppriseProvider {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl StructuredModelProvider for FlippingAppriseProvider {
        fn generate_structured(
            &self,
            _request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            let call_idx = self.calls.fetch_add(1, Ordering::SeqCst);
            let flip_score = if call_idx.is_multiple_of(2) {
                0.85
            } else {
                0.15
            };
            let payload = json!({
                "assessments": [
                    {
                        "canon_path": ".canons/cli/flipper.md",
                        "apprisal_summary": format!("Call {call_idx} rationale."),
                        "apprisal_score": flip_score
                    },
                    {
                        "canon_path": ".canons/cli/stable.md",
                        "apprisal_summary": "Always applicable.",
                        "apprisal_score": 0.95
                    }
                ]
            });
            Ok(GenerationResponse {
                payload,
                resolved_model: "gemini-2.5-flash-lite".to_string(),
                thoughts: Vec::new(),
                usage: TokenUsage {
                    prompt_tokens: 120,
                    completion_tokens: 40,
                    reasoning_tokens: 0,
                    total_tokens: 160,
                },
                latency_ms: 95,
            })
        }
    }

    fn demo_workspace() -> WorkspaceSnapshot {
        WorkspaceSnapshot::new()
            .with_file("src/cli/mod.rs")
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ntags:\n  - cli\n---\nCLI flags MUST support env fallbacks.\n\nRationale: Scriptability.\n",
                &ParseCanonOptions::with_source_ref(".canons/cli/flipper.md"),
            )
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ntags:\n  - cli\n---\nCLI errors MUST suggest remediations.\n\nRationale: Recovery.\n",
                &ParseCanonOptions::with_source_ref(".canons/cli/stable.md"),
            )
    }

    mod help_and_usage_validation {
        use super::*;

        #[test]
        fn test_help_screen_groups_options_and_includes_runnable_examples() {
            let mut cmd = ConsistencyBenchCli::command();
            let help = cmd.render_long_help().to_string();
            assert!(help.contains("Benchmark Options:"));
            assert!(help.contains("Filing Sources:"));
            assert!(help.contains("Caseload & Output:"));
            assert!(help.contains("Examples:"));
            assert!(help.contains("./scripts/bench-consistency.sh --stage apprise"));
        }

        #[test]
        fn test_missing_stage_and_zero_trials_exit_with_code_2_and_remediation() {
            let ws = demo_workspace();
            let env = BTreeMap::new();
            let provider = FlippingAppriseProvider::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 100);
            let deps = HermeticBenchDeps {
                workspace: &ws,
                env: &env,
                provider: &provider,
                clock: &clock,
            };

            let no_stage =
                ConsistencyBenchCli::try_parse_from(["consistency_bench"]).expect("parse");
            let mut stdin = Cursor::new(b"");
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let err = run_consistency_bench_hermetic(
                &no_stage,
                &deps,
                &mut BenchIoStreams {
                    stdin: &mut stdin,
                    stdout: &mut stdout,
                    stderr: &mut stderr,
                },
            )
            .expect_err("missing stage");
            assert_eq!(err.exit_code(), 2);
            assert!(err.to_string().contains("Hint:"));

            let zero_trials = ConsistencyBenchCli::try_parse_from([
                "consistency_bench",
                "--stage",
                "apprise",
                "--trials",
                "0",
            ])
            .expect("parse");
            let err_trials = run_consistency_bench_hermetic(
                &zero_trials,
                &deps,
                &mut BenchIoStreams {
                    stdin: &mut stdin,
                    stdout: &mut stdout,
                    stderr: &mut stderr,
                },
            )
            .expect_err("zero trials");
            assert_eq!(err_trials.exit_code(), 2);
            assert!(err_trials.to_string().contains("--trials"));
        }
    }

    mod hermetic_end_to_end_execution {
        use super::*;

        #[test]
        fn test_executes_markdown_and_json_modes_with_stderr_separation() {
            let ws = demo_workspace();
            let mut env = BTreeMap::new();
            env.insert("GEMINI_API_KEY".to_string(), "test-key".to_string());
            let provider = FlippingAppriseProvider::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 150);
            let deps = HermeticBenchDeps {
                workspace: &ws,
                env: &env,
                provider: &provider,
                clock: &clock,
            };

            let cli = ConsistencyBenchCli::try_parse_from([
                "consistency_bench",
                "--stage",
                "apprise",
                "--trials",
                "3",
                "--intent",
                "Add CLI flag",
                "src/cli/mod.rs",
            ])
            .expect("parse args");

            let mut stdin = Cursor::new(b"");
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let report = run_consistency_bench_hermetic(
                &cli,
                &deps,
                &mut BenchIoStreams {
                    stdin: &mut stdin,
                    stdout: &mut stdout,
                    stderr: &mut stderr,
                },
            )
            .expect("run hermetic bench");

            assert_eq!(report.trials, 3);
            assert_eq!(report.configs[0].flipper_count, 1);

            let stdout_text = String::from_utf8(stdout).expect("utf8 stdout");
            let stderr_text = String::from_utf8(stderr).expect("utf8 stderr");
            assert!(
                stdout_text
                    .contains("## Consistency Benchmark (`apprise`, N = 3, 2 candidate canons)")
            );
            assert!(stdout_text.contains(".canons/cli/flipper.md"));
            assert!(!stdout_text.contains("[bench]"));
            assert!(stderr_text.contains("[bench] [unbounded] trial 1/3:"));

            let json_cli = ConsistencyBenchCli::try_parse_from([
                "consistency_bench",
                "-s",
                "apprise",
                "-n",
                "2",
                "-q",
                "--json",
                "--intent",
                "Add CLI flag",
                "src/cli/mod.rs",
            ])
            .expect("parse json args");
            let mut json_stdin = Cursor::new(b"");
            let mut json_stdout = Vec::new();
            let mut json_stderr = Vec::new();
            run_consistency_bench_hermetic(
                &json_cli,
                &deps,
                &mut BenchIoStreams {
                    stdin: &mut json_stdin,
                    stdout: &mut json_stdout,
                    stderr: &mut json_stderr,
                },
            )
            .expect("run json bench");

            assert!(json_stderr.is_empty());
            let decoded: ConsistencyBenchReport =
                serde_json::from_slice(&json_stdout).expect("valid JSON stdout");
            assert_eq!(decoded.trials, 2);
            assert_eq!(decoded.configs[0].subjects.len(), 2);
        }
    }
}
