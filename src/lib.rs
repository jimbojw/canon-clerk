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

//! Canon Clerk: Automated review gate enforcing declarative engineering rule packs.

pub mod admit;
pub mod apprise;
pub mod bench;
pub mod canon;
pub mod caseload;
pub mod cli;
pub mod configure;
pub mod discover;
pub mod docket;
pub mod intake;
pub mod probe;
pub mod provider;
pub mod telemetry;
pub mod validate;

pub use admit::{
    AdmitError, AdmitOptions, DEFAULT_ADMIT_MAX_CONCURRENT_CALLS, DEFAULT_ADMIT_THRESHOLD,
    ENV_ADMIT_CONCURRENCY, ENV_ADMIT_THRESHOLD, execute_admit,
};
pub use apprise::{
    AppriseError, AppriseOptions, DEFAULT_APPRISE_MAX_CONCURRENT_CALLS, DEFAULT_APPRISE_THRESHOLD,
    ENV_APPRISE_BATCH_SIZE, ENV_APPRISE_CONCURRENCY, ENV_APPRISE_THRESHOLD, execute_apprise,
};
pub use bench::{
    BenchIoStreams, BenchmarkStage, ConfigConsistencyReport, ConfigSweepInputs,
    ConsistencyBenchCli, ConsistencyBenchError, ConsistencyBenchFormat, ConsistencyBenchOptions,
    ConsistencyBenchReport, ConsistencyConfigSpec, DEFAULT_BENCHMARK_TRIALS, ENV_BENCH_BATCH_SIZE,
    ENV_BENCH_CONCURRENCY, ENV_BENCH_FORMAT, ENV_BENCH_INCLUDE_STABLE, ENV_BENCH_OUTPUT_JSON,
    ENV_BENCH_STAGE, ENV_BENCH_THRESHOLD, ENV_BENCH_TRIALS, ExtractedAssessment, HermeticBenchDeps,
    SubjectConsistency, SubjectKey, SubjectObservation, TrialRecord, TrialSummary,
    UpstreamCaseloadInputs, analyze_config_trials, execute_consistency_bench,
    execute_consistency_bench_observed, extract_stage_assessments, parse_batch_tokens,
    prepare_upstream_caseload, render_consistency_markdown, resolve_config_specs,
    run_consistency_bench_cli, run_consistency_bench_hermetic,
};
pub use canon::{
    Canon, CanonBody, CanonMetadata, CanonParseError, CanonScope, CanonSourceInfo, CanonSourceKind,
    CanonToken, CanonTokenKind, DEFAULT_FALLBACK_ID, DEFAULT_GLOBAL_TRIGGER_PATTERN,
    DEFAULT_INSPECT_PLANES, DirectiveName, DirectiveOccurrence, ExtractedFrontmatter,
    FrontmatterKeySpan, InspectPlane, InspectToken, ParseCanonOptions, RawFrontmatter, SourceSpan,
    assemble_canon_from_tokens, derive_diagnostic_path, derive_exists, derive_id, derive_inspect,
    derive_metadata, derive_references, derive_scope, derive_tags, derive_title, derive_triggers,
    extract_first_heading, extract_first_heading_from_tokens, extract_frontmatter,
    extract_frontmatter_from_tokens, has_directory_traversal, id_to_title_case,
    normalize_canon_content, parse_body, parse_body_from_tokens, parse_canon, resolve_source_info,
    scope_pattern, to_kebab_case, tokenize_canon,
};
pub use caseload::{
    ActiveExhibits, AdmissibilityStatus, AnnotationLevel, ApprisalStatus, CASELOAD_SCHEMA_VERSION,
    CanonApprisalAssessment, CanonDocketAssessment, Caseload, CaseloadApprisal, CaseloadConfig,
    CaseloadDiscovery, CaseloadDocket, CaseloadEvidence, CaseloadIntake, CaseloadProbe,
    CaseloadValidation, CodeAnnotation, ColorabilityStatus, ContentOmissionReason,
    DEFAULT_AUDITOR_MODEL, DEFAULT_PROVIDER, DEFAULT_SCREENER_MODEL, DiscoveryMode,
    ExhibitAssessment, FileArtifact, FileChangeStatus, IntakeScope, LinkedIssueContext,
    PatchOmissionReason, ProbeEndpointResult, ProbeStatus, ValidatedCanonMetadata,
    ValidationOutcome, normalize_admissibility_score, normalize_colorability_score,
    normalize_unit_score,
};
pub use cli::{
    AdmitArgs, AdmitFormat, AppriseArgs, AppriseFormat, CanonSelectionArgs, CaseloadIoArgs, Cli,
    CliError, Commands, ConfigureArgs, ConfigureFormat, DiscoverArgs, DiscoverFormat, DocketArgs,
    DocketFormat, ENV_LOG_FILE, ENV_VERBOSE, FilingSourceArgs, IntakeArgs, ProbeArgs, ProbeFormat,
    ProviderConfigArgs, StageDisposition, StderrProgressSink, StderrTelemetryOptions, ValidateArgs,
    ValidateFormat, build_cli_telemetry_sink, init_logging, open_jsonl_log_sink,
    resolve_log_file_path, resolve_verbose_flag, run, run_admit, run_admit_live_with_telemetry,
    run_admit_with_snapshot_and_provider, run_admit_with_telemetry, run_apprise,
    run_apprise_live_with_telemetry, run_apprise_with_snapshot_and_provider,
    run_apprise_with_telemetry, run_configure, run_configure_with_env,
    run_configure_with_telemetry, run_discover, run_discover_in_workspace,
    run_discover_with_telemetry, run_docket, run_docket_live_with_telemetry,
    run_docket_with_snapshot_and_provider, run_docket_with_telemetry, run_intake,
    run_intake_with_telemetry, run_probe, run_probe_with_env_and_prober, run_probe_with_telemetry,
    run_validate, run_validate_in_workspace, run_validate_with_telemetry,
};
pub use configure::{
    ConfigureError, ConfigureOptions, DEFAULT_FLASH_MODEL, ENV_AUDITOR_MODEL, ENV_GEMINI_API_KEY,
    ENV_REASONING_BUDGET, ENV_SCREENER_MODEL, StoredConfig, StoredProviderConfig, StoredTierModel,
    execute_configure, load_host_config_file, normalize_model_specifier,
    resolve_default_config_path, resolve_gemini_api_key,
};
pub use discover::{
    DEFAULT_CANON_GLOB, DEFAULT_IGNORES, DiscoverError, DiscoverOptions, GlobPattern,
    GlobSyntaxError, WorkspaceSnapshot, execute_discover, execute_discover_on_disk,
    is_ignored_path, is_wildcard_pattern, normalize_workspace_path, parse_canon_or_fallback,
    scope_relative_path,
};
pub use docket::{
    DEFAULT_DOCKET_MAX_CONCURRENT_CALLS, DEFAULT_DOCKET_THRESHOLD, DocketError, DocketOptions,
    ENV_DOCKET_BATCH_SIZE, ENV_DOCKET_CONCURRENCY, ENV_DOCKET_THRESHOLD, ENV_MAX_CANONS_PER_CALL,
    ENV_MAX_CONCURRENT_CALLS, execute_docket, partition_candidate_canons,
};
pub use intake::{IntakeError, IntakeOptions, execute_intake, parse_unified_diff};
pub use probe::{
    DEFAULT_PROBE_TIMEOUT_MS, ENV_PROBE_TIMEOUT_MS, EndpointProber, GeminiEndpointProber,
    ProbeError, ProbeOptions, decode_gemini_probe_response, execute_probe,
};
pub use provider::{
    DEFAULT_GEMINI_BASE_URL, DEFAULT_GENERATION_TIMEOUT_MS, GeminiModelProvider, GenerationRequest,
    GenerationResponse, ProviderError, StructuredModelProvider, TokenUsage,
    build_generate_content_url, classify_gemini_http_error, extract_gemini_error_detail,
    strip_provider_prefix,
};
pub use telemetry::{
    CompositeTelemetrySink, InstrumentedModelProvider, JsonlWriterSink, NoopTelemetryClock,
    NoopTelemetrySink, PipelineStage, RecordingTelemetrySink, StageScope, SteppingTelemetryClock,
    SystemTelemetryClock, TelemetryClock, TelemetryEvent, TelemetrySession, TelemetrySink,
    format_unix_millis_iso8601,
};
pub use validate::{
    ValidateError, ValidateOptions, execute_validate, execute_validate_on_disk, lint_canon_source,
};

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct BaselineManifest {
        name: String,
        version: String,
    }

    #[test]
    fn test_foundational_serde_serialization() {
        let manifest = BaselineManifest {
            name: "canon-clerk".to_string(),
            version: "0.1.0".to_string(),
        };

        let json = serde_json::to_string(&manifest).expect("serialization must succeed");
        let decoded: BaselineManifest =
            serde_json::from_str(&json).expect("deserialization must succeed");

        assert_eq!(manifest, decoded);
    }
}
