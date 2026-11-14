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

//! Multi-run consistency and flipper analysis benchmark harness (`docs/architecture/benchmarking.md`).

mod cli;
mod error;
mod extract;
mod prepare;
mod render;
mod stats;

pub use cli::{
    BenchIoStreams, ConsistencyBenchCli, ConsistencyBenchFormat, HermeticBenchDeps,
    run_consistency_bench_cli, run_consistency_bench_hermetic,
};
pub use error::{
    BenchmarkStage, ConsistencyBenchError, DEFAULT_BENCHMARK_TRIALS, ENV_BENCH_BATCH_SIZE,
    ENV_BENCH_CONCURRENCY, ENV_BENCH_FORMAT, ENV_BENCH_INCLUDE_STABLE, ENV_BENCH_OUTPUT_JSON,
    ENV_BENCH_STAGE, ENV_BENCH_THRESHOLD, ENV_BENCH_TRIALS,
};
pub use extract::{ExtractedAssessment, SubjectKey, extract_stage_assessments};
pub use prepare::{
    ConfigSweepInputs, UpstreamCaseloadInputs, parse_batch_tokens, prepare_upstream_caseload,
    resolve_config_specs,
};
pub use render::render_consistency_markdown;
pub use stats::{TrialRecord, analyze_config_trials};

use crate::admit::{AdmitOptions, execute_admit};
use crate::apprise::{AppriseOptions, DEFAULT_APPRISE_THRESHOLD, execute_apprise};
use crate::audit::{AuditOptions, execute_audit};
use crate::caseload::{
    Caseload, DEFAULT_AUDITOR_MODEL, DEFAULT_SCREENER_MODEL, normalize_unit_score,
};
use crate::discover::WorkspaceSnapshot;
use crate::docket::{DocketOptions, execute_docket};
use crate::provider::{DEFAULT_GENERATION_TIMEOUT_MS, StructuredModelProvider, TokenUsage};
use crate::telemetry::{
    PipelineStage, RecordingTelemetrySink, SystemTelemetryClock, TelemetryClock, TelemetryEvent,
    TelemetrySession,
};
use prepare::clear_stage_and_downstream;
use serde::{Deserialize, Serialize};

/// Execution parameters for a single benchmark configuration (e.g. unbounded vs. `batch=15,conc=4`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ConsistencyConfigSpec {
    /// Human-readable configuration label (`"unbounded"`, `"batch=15,conc=4"`, etc.).
    pub label: String,
    /// Classification score threshold in `[0.0, 1.0]`.
    pub threshold: f64,
    /// Optional maximum candidate canons per screener model call (`None` = single-turn unbounded).
    pub max_canons_per_call: Option<usize>,
    /// Optional maximum concurrent in-flight model calls when batched.
    pub max_concurrent_calls: Option<usize>,
    /// Per-request generation timeout ceiling in milliseconds.
    pub timeout_ms: u64,
}

impl Default for ConsistencyConfigSpec {
    fn default() -> Self {
        Self {
            label: "unbounded".to_string(),
            threshold: DEFAULT_APPRISE_THRESHOLD,
            max_canons_per_call: None,
            max_concurrent_calls: None,
            timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
        }
    }
}

/// Input options configuring [`execute_consistency_bench`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ConsistencyBenchOptions {
    /// Target Caseload heuristic stage.
    pub stage: BenchmarkStage,
    /// Number of repeated trials per configuration (`>= 1`; defaults to [`DEFAULT_BENCHMARK_TRIALS`]).
    pub trials: usize,
    /// One or more configurations to evaluate across `trials`.
    pub configs: Vec<ConsistencyConfigSpec>,
}

impl Default for ConsistencyBenchOptions {
    fn default() -> Self {
        Self {
            stage: BenchmarkStage::Apprise,
            trials: DEFAULT_BENCHMARK_TRIALS,
            configs: vec![ConsistencyConfigSpec::default()],
        }
    }
}

/// Per-trial outcome recorded for a single canon or `(canon_path, exhibit_path)` subject.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SubjectObservation {
    /// 1-indexed trial number (`1..=N`).
    pub trial: usize,
    /// Stage status string (`"applicable"`, `"docketed"`, `"admitted"`, `"pass"`, etc.).
    pub status: String,
    /// Whether this trial produced an affirmative classification (`score >= threshold`).
    pub is_positive: bool,
    /// Continuous score in `[0.0, 1.0]`.
    pub score: f64,
    /// Rationale summary emitted by the model in this trial.
    pub summary: String,
}

/// Aggregate operational and count metrics for a single trial run (`1..=N`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TrialSummary {
    /// 1-indexed trial number (`1..=N`).
    pub trial: usize,
    /// Total evaluated subjects in this trial.
    pub subject_count: usize,
    /// Count of subjects classified affirmatively (`applicable`, `docketed`, `admitted`, `pass`).
    pub positive_count: usize,
    /// Count of subjects classified negatively (`dismissed`, `excluded`, `fail`).
    pub negative_count: usize,
    /// Stage wall-clock latency in milliseconds.
    pub duration_ms: u64,
    /// Prompt tokens consumed across the trial.
    pub prompt_tokens: u64,
    /// Completion tokens consumed across the trial.
    pub completion_tokens: u64,
    /// Extended reasoning tokens consumed across the trial.
    pub reasoning_tokens: u64,
    /// Total tokens consumed across the trial.
    pub total_tokens: u64,
}

/// Multi-trial consistency and flipper analysis for a single canon or `(canon_path, exhibit_path)` subject.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SubjectConsistency {
    /// Repository-relative canon path or identifier.
    pub canon_path: String,
    /// Optional repository-relative exhibit path for micro evidentiary triage (`admit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhibit_path: Option<String>,
    /// `true` if classification status varied across the `N` trials.
    pub is_flipper: bool,
    /// Number of trials with an affirmative status (`applicable`, `docketed`, `admitted`, `pass`).
    pub positive_count: usize,
    /// Number of trials with a negative status (`dismissed`, `excluded`, `fail`).
    pub negative_count: usize,
    /// Modal status agreement fraction in `(0.0, 1.0]` (`1.0` when unanimous).
    pub agreement_rate: f64,
    /// Arithmetic mean of continuous scores across trials.
    pub score_mean: f64,
    /// Sample standard deviation (`N - 1` denominator; `0.0` when `N <= 1`) of scores across trials.
    pub score_stdev: f64,
    /// Minimum score observed across trials.
    pub score_min: f64,
    /// Maximum score observed across trials.
    pub score_max: f64,
    /// Chronological per-trial observations (`1..=N`).
    pub observations: Vec<SubjectObservation>,
}

impl SubjectConsistency {
    /// Formats `"<canon_path>"` or `"<canon_path> :: <exhibit_path>"` for human-readable tables.
    pub fn display_id(&self) -> String {
        match &self.exhibit_path {
            Some(exhibit) => format!("{} :: {exhibit}", self.canon_path),
            None => self.canon_path.clone(),
        }
    }
}

/// Multi-trial consistency summary for a single [`ConsistencyConfigSpec`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ConfigConsistencyReport {
    /// Configuration label (`"unbounded"`, `"batch=15,conc=4"`, etc.).
    pub label: String,
    /// Evaluated Caseload stage.
    pub stage: BenchmarkStage,
    /// Resolved model specifier used for the stage.
    pub model: String,
    /// Number of completed trials (`N`).
    pub trials: usize,
    /// Applied classification score threshold.
    pub threshold: f64,
    /// Optional batch ceiling (`max_canons_per_call`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_canons_per_call: Option<usize>,
    /// Optional concurrency ceiling (`max_concurrent_calls`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_calls: Option<usize>,
    /// Total distinct subjects evaluated across trials.
    pub subject_count: usize,
    /// Count of subjects whose classification status flipped at least once.
    pub flipper_count: usize,
    /// Fraction of subjects with 100% unanimous status agreement across all `N` trials.
    pub unanimous_agreement_rate: f64,
    /// Per-trial affirmative counts (`[c_1, c_2, ..., c_N]`).
    pub positive_counts: Vec<usize>,
    /// Minimum affirmative count across trials.
    pub positive_count_min: usize,
    /// Maximum affirmative count across trials.
    pub positive_count_max: usize,
    /// Mean per-subject score sample standard deviation.
    pub mean_score_stdev: f64,
    /// Maximum per-subject score sample standard deviation.
    pub max_score_stdev: f64,
    /// Minimum trial wall-clock latency in milliseconds.
    pub latency_min_ms: u64,
    /// Median trial wall-clock latency in milliseconds.
    pub latency_median_ms: u64,
    /// Mean trial wall-clock latency in milliseconds.
    pub latency_mean_ms: f64,
    /// Maximum trial wall-clock latency in milliseconds.
    pub latency_max_ms: u64,
    /// Cumulative tokens expended across all `N` trials for this configuration.
    pub total_tokens: u64,
    /// Per-trial operational and count summaries.
    pub trial_summaries: Vec<TrialSummary>,
    /// Stably sorted per-subject consistency records.
    pub subjects: Vec<SubjectConsistency>,
}

impl ConfigConsistencyReport {
    /// Returns all subjects whose classification status flipped across trials (`is_flipper == true`).
    pub fn flippers(&self) -> Vec<&SubjectConsistency> {
        self.subjects.iter().filter(|s| s.is_flipper).collect()
    }
}

/// Top-level structured report produced by [`execute_consistency_bench`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ConsistencyBenchReport {
    /// Target Caseload heuristic stage.
    pub stage: BenchmarkStage,
    /// Number of trials executed per configuration.
    pub trials: usize,
    /// Number of candidate canons in `.discovery.candidate_canons`.
    pub candidate_canon_count: usize,
    /// Consistency results for each evaluated configuration.
    pub configs: Vec<ConfigConsistencyReport>,
}

/// Executes `N` repeated trials per configuration against `caseload` using wall-clock telemetry.
pub fn execute_consistency_bench(
    options: &ConsistencyBenchOptions,
    caseload: &Caseload,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
) -> Result<ConsistencyBenchReport, ConsistencyBenchError> {
    let clock = SystemTelemetryClock::new();
    let mut noop_observer = |_label: &str, _summary: &TrialSummary| {};
    execute_consistency_bench_observed(
        options,
        caseload,
        workspace,
        provider,
        &clock,
        &mut noop_observer,
    )
}

/// Executes `N` repeated trials per configuration with an explicit [`TelemetryClock`] and per-trial callback.
pub fn execute_consistency_bench_observed(
    options: &ConsistencyBenchOptions,
    caseload: &Caseload,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
    clock: &dyn TelemetryClock,
    on_trial: &mut dyn FnMut(&str, &TrialSummary),
) -> Result<ConsistencyBenchReport, ConsistencyBenchError> {
    validate_bench_options(options)?;

    let candidate_canon_count = caseload
        .discovery
        .as_ref()
        .map_or(0, |d| d.candidate_canons.len());
    let model = resolved_model_for_stage(options.stage, caseload);
    let mut base_caseload = clear_stage_and_downstream(options.stage, caseload.clone());
    if matches!(options.stage, BenchmarkStage::Admit | BenchmarkStage::Audit)
        && base_caseload.docket.is_none()
        && !(options.stage == BenchmarkStage::Audit && base_caseload.evidence.is_some())
    {
        base_caseload = execute_docket(
            &DocketOptions::default(),
            base_caseload,
            workspace,
            provider,
        )
        .map_err(ConsistencyBenchError::Docket)?;
    }
    if options.stage == BenchmarkStage::Audit && base_caseload.evidence.is_none() {
        base_caseload = execute_admit(&AdmitOptions::default(), base_caseload, workspace, provider)
            .map_err(ConsistencyBenchError::Admit)?;
    }

    let mut config_reports = Vec::with_capacity(options.configs.len());
    for spec in &options.configs {
        let mut records = Vec::with_capacity(options.trials);
        for trial_idx in 1..=options.trials {
            let record = execute_single_trial(
                options.stage,
                spec,
                trial_idx,
                &base_caseload,
                workspace,
                provider,
                clock,
            )?;
            let positive_count = record
                .assessments
                .values()
                .filter(|a| a.is_positive)
                .count();
            let subject_count = record.assessments.len();
            let summary = TrialSummary {
                trial: trial_idx,
                subject_count,
                positive_count,
                negative_count: subject_count.saturating_sub(positive_count),
                duration_ms: record.duration_ms,
                prompt_tokens: record.usage.prompt_tokens,
                completion_tokens: record.usage.completion_tokens,
                reasoning_tokens: record.usage.reasoning_tokens,
                total_tokens: record.usage.total_tokens,
            };
            on_trial(&spec.label, &summary);
            records.push(record);
        }
        config_reports.push(analyze_config_trials(spec, options.stage, &model, &records));
    }

    Ok(ConsistencyBenchReport {
        stage: options.stage,
        trials: options.trials,
        candidate_canon_count,
        configs: config_reports,
    })
}

fn validate_bench_options(options: &ConsistencyBenchOptions) -> Result<(), ConsistencyBenchError> {
    if options.trials == 0 {
        return Err(ConsistencyBenchError::InvalidTrials);
    }
    if options.configs.is_empty() {
        return Err(ConsistencyBenchError::EmptyConfigs);
    }
    for spec in &options.configs {
        normalize_unit_score(spec.threshold)
            .map_err(|detail| ConsistencyBenchError::InvalidThreshold { detail })?;
        if spec.max_canons_per_call == Some(0) {
            return Err(ConsistencyBenchError::InvalidBatchSpec {
                detail: "max_canons_per_call must be at least 1, got 0".to_string(),
            });
        }
        if spec.max_concurrent_calls == Some(0) {
            return Err(ConsistencyBenchError::InvalidConcurrency {
                detail: "max_concurrent_calls must be at least 1, got 0".to_string(),
            });
        }
    }
    Ok(())
}

fn resolved_model_for_stage(stage: BenchmarkStage, caseload: &Caseload) -> String {
    match stage {
        BenchmarkStage::Apprise | BenchmarkStage::Docket | BenchmarkStage::Admit => {
            caseload.config.as_ref().map_or_else(
                || DEFAULT_SCREENER_MODEL.to_string(),
                |c| c.screener_model.clone(),
            )
        }
        BenchmarkStage::Audit => caseload.config.as_ref().map_or_else(
            || DEFAULT_AUDITOR_MODEL.to_string(),
            |c| c.auditor_model.clone(),
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Internal single-trial helper binding stage, config spec, trial index, base caseload, workspace, provider, and clock"
)]
fn execute_single_trial(
    stage: BenchmarkStage,
    spec: &ConsistencyConfigSpec,
    trial: usize,
    base_caseload: &Caseload,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
    clock: &dyn TelemetryClock,
) -> Result<TrialRecord, ConsistencyBenchError> {
    let sink = RecordingTelemetrySink::new();
    let session = TelemetrySession::new(&sink, clock);

    let enriched = match stage {
        BenchmarkStage::Apprise => {
            let opts = AppriseOptions {
                threshold: spec.threshold,
                timeout_ms: spec.timeout_ms,
                max_canons_per_call: spec.max_canons_per_call,
                max_concurrent_calls: spec.max_concurrent_calls,
            };
            session
                .run_stage(PipelineStage::Apprise, |scope| {
                    let instrumented = scope.instrument_provider(provider);
                    execute_apprise(&opts, base_caseload.clone(), workspace, &instrumented)
                })
                .map_err(ConsistencyBenchError::Apprise)?
        }
        BenchmarkStage::Docket => {
            let opts = DocketOptions {
                threshold: spec.threshold,
                timeout_ms: spec.timeout_ms,
                max_canons_per_call: spec.max_canons_per_call,
                max_concurrent_calls: spec.max_concurrent_calls,
            };
            session
                .run_stage(PipelineStage::Docket, |scope| {
                    let instrumented = scope.instrument_provider(provider);
                    execute_docket(&opts, base_caseload.clone(), workspace, &instrumented)
                })
                .map_err(ConsistencyBenchError::Docket)?
        }
        BenchmarkStage::Admit => {
            let opts = AdmitOptions {
                threshold: spec.threshold,
                timeout_ms: spec.timeout_ms,
                max_concurrent_calls: spec.max_concurrent_calls,
            };
            session
                .run_stage(PipelineStage::Admit, |scope| {
                    let instrumented = scope.instrument_provider(provider);
                    execute_admit(&opts, base_caseload.clone(), workspace, &instrumented)
                })
                .map_err(ConsistencyBenchError::Admit)?
        }
        BenchmarkStage::Audit => {
            let opts = AuditOptions {
                threshold: spec.threshold,
                fastpass_threshold: None,
                timeout_ms: spec.timeout_ms,
                max_concurrent_calls: spec.max_concurrent_calls,
            };
            session
                .run_stage(PipelineStage::Audit, |scope| {
                    let instrumented = scope.instrument_provider(provider);
                    execute_audit(&opts, base_caseload.clone(), workspace, &instrumented)
                })
                .map_err(ConsistencyBenchError::Audit)?
        }
    };

    let (duration_ms, usage) = extract_stage_finish_metrics(&sink, stage.pipeline_stage());
    let assessments = extract_stage_assessments(stage, &enriched, spec.threshold)?;

    Ok(TrialRecord {
        trial,
        duration_ms,
        usage,
        assessments,
    })
}

fn extract_stage_finish_metrics(
    sink: &RecordingTelemetrySink,
    expected_stage: PipelineStage,
) -> (u64, TokenUsage) {
    for event in sink.events() {
        if let TelemetryEvent::StageFinish {
            stage,
            duration_ms,
            usage,
        } = event
            && stage == expected_stage
        {
            return (duration_ms, usage.unwrap_or_default());
        }
    }
    (0, TokenUsage::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::ParseCanonOptions;
    use crate::cli::CanonSelectionArgs;
    use crate::configure::ConfigureOptions;
    use crate::docket::DEFAULT_DOCKET_THRESHOLD;
    use crate::intake::IntakeOptions;
    use crate::provider::{GenerationRequest, GenerationResponse, ProviderError};
    use crate::telemetry::SteppingTelemetryClock;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScriptedDocketProvider {
        turn: AtomicUsize,
    }

    impl ScriptedDocketProvider {
        fn new() -> Self {
            Self {
                turn: AtomicUsize::new(0),
            }
        }
    }

    impl StructuredModelProvider for ScriptedDocketProvider {
        fn generate_structured(
            &self,
            _request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            let idx = self.turn.fetch_add(1, Ordering::SeqCst);
            let flip_score = if idx == 2 { 0.20 } else { 0.88 };
            let payload = json!({
                "cases": [
                    {
                        "canon_path": ".canons/cli/flipper.md",
                        "colorability_summary": format!("Docket turn {idx} summary."),
                        "colorability_score": flip_score
                    },
                    {
                        "canon_path": ".canons/cli/steady.md",
                        "colorability_summary": "Orthogonal to diff.",
                        "colorability_score": 0.05
                    }
                ]
            });
            Ok(GenerationResponse {
                payload,
                resolved_model: "gemini-2.5-flash-lite".to_string(),
                thoughts: Vec::new(),
                usage: TokenUsage {
                    prompt_tokens: 200,
                    completion_tokens: 50,
                    reasoning_tokens: 0,
                    total_tokens: 250,
                },
                latency_ms: 110,
            })
        }
    }

    fn docket_fixture() -> (Caseload, WorkspaceSnapshot) {
        let ws = WorkspaceSnapshot::new()
            .with_file("src/cli/mod.rs")
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ntags:\n  - cli\n---\nCLI flags MUST use kebab-case.\n\nRationale: POSIX consistency.\n",
                &ParseCanonOptions::with_source_ref(".canons/cli/flipper.md"),
            )
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ntags:\n  - cli\n---\nCLI errors MUST suggest remediations.\n\nRationale: Actionable recovery.\n",
                &ParseCanonOptions::with_source_ref(".canons/cli/steady.md"),
            );
        let intake_opts = IntakeOptions {
            target_paths: Some(vec!["src/cli/mod.rs".to_string()]),
            ..IntakeOptions::default()
        };
        let cfg_opts = ConfigureOptions {
            gemini_api_key: Some("test-key".to_string()),
            ..ConfigureOptions::default()
        };
        let caseload = prepare_upstream_caseload(&UpstreamCaseloadInputs {
            stage: BenchmarkStage::Docket,
            intake_opts: &intake_opts,
            canons: &CanonSelectionArgs::default(),
            configure_opts: &cfg_opts,
            incoming: None,
            workspace: &ws,
        })
        .expect("prepare docket caseload");
        (caseload, ws)
    }

    mod docket_multi_trial_execution {
        use super::*;

        #[test]
        fn test_executes_five_docket_trials_and_captures_flippers_and_telemetry() {
            let (caseload, ws) = docket_fixture();
            let provider = ScriptedDocketProvider::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 125);
            let options = ConsistencyBenchOptions {
                stage: BenchmarkStage::Docket,
                trials: DEFAULT_BENCHMARK_TRIALS,
                configs: vec![ConsistencyConfigSpec {
                    label: "unbounded".to_string(),
                    threshold: DEFAULT_DOCKET_THRESHOLD,
                    max_canons_per_call: None,
                    max_concurrent_calls: None,
                    timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
                }],
            };

            let mut observed_trials = Vec::new();
            let report = execute_consistency_bench_observed(
                &options,
                &caseload,
                &ws,
                &provider,
                &clock,
                &mut |label, summary| observed_trials.push((label.to_string(), summary.trial)),
            )
            .expect("execute docket consistency bench");

            assert_eq!(observed_trials.len(), 5);
            assert_eq!(report.candidate_canon_count, 2);
            let cfg = &report.configs[0];
            assert_eq!(cfg.positive_counts, vec![1, 1, 0, 1, 1]);
            assert_eq!(cfg.flipper_count, 1);
            assert_eq!(cfg.total_tokens, 1250);
            assert_eq!(cfg.latency_median_ms, 125);

            let flippers = cfg.flippers();
            assert_eq!(flippers.len(), 1);
            assert_eq!(flippers[0].canon_path, ".canons/cli/flipper.md");
            assert_eq!(flippers[0].positive_count, 4);
            assert_eq!(flippers[0].negative_count, 1);
            assert!((flippers[0].agreement_rate - 0.8).abs() < 1e-9);
        }
    }

    mod validation_and_error_handling {
        use super::*;

        #[test]
        fn test_rejects_zero_trials_empty_configs_and_invalid_threshold() {
            let (caseload, ws) = docket_fixture();
            let provider = ScriptedDocketProvider::new();

            let err_zero = execute_consistency_bench(
                &ConsistencyBenchOptions {
                    trials: 0,
                    ..ConsistencyBenchOptions::default()
                },
                &caseload,
                &ws,
                &provider,
            )
            .expect_err("0 trials must fail");
            assert_eq!(err_zero.exit_code(), 2);

            let err_empty = execute_consistency_bench(
                &ConsistencyBenchOptions {
                    configs: Vec::new(),
                    ..ConsistencyBenchOptions::default()
                },
                &caseload,
                &ws,
                &provider,
            )
            .expect_err("empty configs must fail");
            assert_eq!(err_empty.exit_code(), 2);

            let err_thresh = execute_consistency_bench(
                &ConsistencyBenchOptions {
                    configs: vec![ConsistencyConfigSpec {
                        threshold: 1.5,
                        ..ConsistencyConfigSpec::default()
                    }],
                    ..ConsistencyBenchOptions::default()
                },
                &caseload,
                &ws,
                &provider,
            )
            .expect_err("out-of-bounds threshold must fail");
            assert_eq!(err_thresh.exit_code(), 2);
        }
    }

    mod audit_multi_trial_execution {
        use super::*;
        use crate::audit::DEFAULT_AUDIT_THRESHOLD;

        struct ScriptedAuditProvider {
            docket_calls: AtomicUsize,
            admit_calls: AtomicUsize,
            audit_steady_calls: AtomicUsize,
            audit_flip_calls: AtomicUsize,
        }

        impl ScriptedAuditProvider {
            fn new() -> Self {
                Self {
                    docket_calls: AtomicUsize::new(0),
                    admit_calls: AtomicUsize::new(0),
                    audit_steady_calls: AtomicUsize::new(0),
                    audit_flip_calls: AtomicUsize::new(0),
                }
            }
        }

        impl StructuredModelProvider for ScriptedAuditProvider {
            fn generate_structured(
                &self,
                request: &GenerationRequest,
            ) -> Result<GenerationResponse, ProviderError> {
                let payload = if request.system_instruction.contains("Macro Subject-Matter") {
                    self.docket_calls.fetch_add(1, Ordering::SeqCst);
                    json!({
                        "cases": [
                            {
                                "canon_path": ".canons/cli/flipper.md",
                                "colorability_summary": "Docketed CLI case.",
                                "colorability_score": 0.92
                            },
                            {
                                "canon_path": ".canons/cli/steady.md",
                                "colorability_summary": "Docketed CLI remediation case.",
                                "colorability_score": 0.90
                            }
                        ]
                    })
                } else if request
                    .system_instruction
                    .contains("Micro Evidentiary Admissibility")
                {
                    self.admit_calls.fetch_add(1, Ordering::SeqCst);
                    json!({
                        "exhibits": [{
                            "file_path": "src/cli/mod.rs",
                            "admissibility_summary": "Admitted CLI exhibit.",
                            "admissibility_score": 0.94
                        }]
                    })
                } else if request.user_prompt.contains(".canons/cli/steady.md") {
                    // Steady `fail` across all 3 trials, even though remediation prose and line spans vary!
                    let turn = self.audit_steady_calls.fetch_add(1, Ordering::SeqCst);
                    json!({
                        "compliance_summary": format!("Remediation wording variant #{turn}: add a Hint line."),
                        "compliance_score": 0.08 + (turn as f64) * 0.03,
                        "annotations": [{
                            "path": "src/cli/mod.rs",
                            "start_line": 10 + turn,
                            "end_line": 12 + turn * 2,
                            "annotation_level": "failure",
                            "message": format!("Line-span variant #{turn}")
                        }]
                    })
                } else {
                    let turn = self.audit_flip_calls.fetch_add(1, Ordering::SeqCst);
                    let score = if turn == 1 { 0.88 } else { 0.12 };
                    json!({
                        "compliance_summary": format!("Flipper trial {turn}."),
                        "compliance_score": score,
                        "annotations": []
                    })
                };

                Ok(GenerationResponse {
                    payload,
                    resolved_model: "gemini-2.5-pro".to_string(),
                    thoughts: Vec::new(),
                    usage: TokenUsage {
                        prompt_tokens: 300,
                        completion_tokens: 70,
                        reasoning_tokens: 130,
                        total_tokens: 500,
                    },
                    latency_ms: 210,
                })
            }
        }

        #[test]
        fn test_audit_bench_prepares_upstream_once_and_ignores_prose_and_span_variance_on_stable_verdicts()
         {
            let (caseload, ws) = docket_fixture();
            let provider = ScriptedAuditProvider::new();
            let report = execute_consistency_bench(
                &ConsistencyBenchOptions {
                    stage: BenchmarkStage::Audit,
                    trials: 3,
                    configs: vec![ConsistencyConfigSpec {
                        label: "unbounded".to_string(),
                        threshold: DEFAULT_AUDIT_THRESHOLD,
                        max_canons_per_call: None,
                        max_concurrent_calls: None,
                        timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
                    }],
                },
                &caseload,
                &ws,
                &provider,
            )
            .expect("run 3-trial audit bench");

            assert_eq!(provider.docket_calls.load(Ordering::SeqCst), 1);
            assert_eq!(provider.admit_calls.load(Ordering::SeqCst), 2);
            assert_eq!(provider.audit_steady_calls.load(Ordering::SeqCst), 3);
            assert_eq!(provider.audit_flip_calls.load(Ordering::SeqCst), 3);

            let cfg = &report.configs[0];
            assert_eq!(cfg.subject_count, 2);
            assert_eq!(
                cfg.flipper_count, 1,
                "only 1D pass/fail verdict flip counts as a flipper; varying prose/spans on steady fail must produce 0 flippers"
            );
            assert_eq!(cfg.flippers()[0].canon_path, ".canons/cli/flipper.md");

            let steady = cfg
                .subjects
                .iter()
                .find(|s| s.canon_path == ".canons/cli/steady.md")
                .expect("steady subject");
            assert!(!steady.is_flipper);
            assert_eq!(steady.agreement_rate, 1.0);
            assert_eq!(steady.negative_count, 3);
        }
    }

    mod admit_multi_trial_execution {
        use super::*;
        use crate::admit::DEFAULT_ADMIT_THRESHOLD;

        struct ScriptedAdmitProvider {
            docket_calls: AtomicUsize,
            admit_calls: AtomicUsize,
        }

        impl ScriptedAdmitProvider {
            fn new() -> Self {
                Self {
                    docket_calls: AtomicUsize::new(0),
                    admit_calls: AtomicUsize::new(0),
                }
            }
        }

        impl StructuredModelProvider for ScriptedAdmitProvider {
            fn generate_structured(
                &self,
                request: &GenerationRequest,
            ) -> Result<GenerationResponse, ProviderError> {
                let payload = if request.system_instruction.contains("Macro Subject-Matter") {
                    self.docket_calls.fetch_add(1, Ordering::SeqCst);
                    json!({
                        "cases": [
                            {
                                "canon_path": ".canons/cli/flipper.md",
                                "colorability_summary": "Docketed CLI case.",
                                "colorability_score": 0.90
                            },
                            {
                                "canon_path": ".canons/cli/steady.md",
                                "colorability_summary": "Dismissed.",
                                "colorability_score": 0.05
                            }
                        ]
                    })
                } else {
                    let idx = self.admit_calls.fetch_add(1, Ordering::SeqCst);
                    let score = if idx == 1 { 0.30 } else { 0.85 };
                    json!({
                        "exhibits": [{
                            "file_path": "src/cli/mod.rs",
                            "admissibility_summary": format!("Admit turn {idx}."),
                            "admissibility_score": score
                        }]
                    })
                };
                Ok(GenerationResponse {
                    payload,
                    resolved_model: "gemini-2.5-flash-lite".to_string(),
                    thoughts: Vec::new(),
                    usage: TokenUsage {
                        prompt_tokens: 150,
                        completion_tokens: 40,
                        reasoning_tokens: 0,
                        total_tokens: 190,
                    },
                    latency_ms: 80,
                })
            }
        }

        #[test]
        fn test_executes_admit_trials_against_single_prepared_docket_and_tracks_2d_flippers() {
            let (caseload, ws) = docket_fixture();
            let provider = ScriptedAdmitProvider::new();
            let report = execute_consistency_bench(
                &ConsistencyBenchOptions {
                    stage: BenchmarkStage::Admit,
                    trials: 3,
                    configs: vec![ConsistencyConfigSpec {
                        label: "unbounded".to_string(),
                        threshold: DEFAULT_ADMIT_THRESHOLD,
                        max_canons_per_call: None,
                        max_concurrent_calls: None,
                        timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
                    }],
                },
                &caseload,
                &ws,
                &provider,
            )
            .expect("run 3-trial admit bench");

            assert_eq!(provider.docket_calls.load(Ordering::SeqCst), 1);
            assert_eq!(provider.admit_calls.load(Ordering::SeqCst), 3);
            let cfg = &report.configs[0];
            assert_eq!(cfg.flipper_count, 1);
            assert_eq!(
                cfg.flippers()[0].exhibit_path.as_deref(),
                Some("src/cli/mod.rs")
            );
            assert_eq!(cfg.flippers()[0].positive_count, 2);
            assert_eq!(cfg.flippers()[0].negative_count, 1);
        }
    }

    mod report_json_round_trip {
        use super::*;

        #[test]
        fn test_report_serializes_clean_json_omitting_none_exhibit_path() {
            let (caseload, ws) = docket_fixture();
            let provider = ScriptedDocketProvider::new();
            let report = execute_consistency_bench(
                &ConsistencyBenchOptions {
                    stage: BenchmarkStage::Docket,
                    trials: 2,
                    configs: vec![ConsistencyConfigSpec::default()],
                },
                &caseload,
                &ws,
                &provider,
            )
            .expect("run 2-trial docket bench");

            let encoded = serde_json::to_string(&report).expect("serialize report");
            assert!(
                !encoded.contains("exhibit_path"),
                "1D canon subjects must omit `exhibit_path` on wire"
            );
            let decoded: ConsistencyBenchReport =
                serde_json::from_str(&encoded).expect("deserialize report");
            assert_eq!(decoded, report);
        }
    }
}
