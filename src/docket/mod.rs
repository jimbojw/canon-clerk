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

//! Core domain execution engine for the `docket` Caseload DAG stage (`docs/architecture/nodes/docket.md`).

mod decision;
mod prompt;

use crate::caseload::{
    CanonDocketAssessment, Caseload, CaseloadDocket, ColorabilityStatus, TYPESAFE_PROVIDER,
    normalize_colorability_score,
};
use crate::configure::model_provider;
use crate::discover::WorkspaceSnapshot;
use crate::provider::{
    DEFAULT_GENERATION_TIMEOUT_MS, GenerationRequest, ProviderError, StructuredModelProvider,
};
use prompt::{
    DOCKET_SYSTEM_INSTRUCTION, build_docket_batch_user_prompt, build_docket_filing_prefix,
    build_docket_schema,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Default subject-matter jurisdiction threshold for `docket` (`docs/architecture/nodes/docket.md` §4.4).
pub const DEFAULT_DOCKET_THRESHOLD: f64 = 0.5;

/// Default maximum concurrent in-flight screener model calls when `docket` executes multiple batches.
pub const DEFAULT_DOCKET_MAX_CONCURRENT_CALLS: usize = 4;

/// Environment variable name for overriding `--docket-threshold` (`docs/architecture/nodes/docket.md` §5).
pub const ENV_DOCKET_THRESHOLD: &str = "CANON_CLERK_DOCKET_THRESHOLD";

/// Environment variable name for overriding `--docket-batch-size` (`docs/architecture/nodes/docket.md` §5).
pub const ENV_DOCKET_BATCH_SIZE: &str = "CANON_CLERK_DOCKET_BATCH_SIZE";

/// Alias environment variable name for overriding `--max-canons-per-call`.
pub const ENV_MAX_CANONS_PER_CALL: &str = "CANON_CLERK_MAX_CANONS_PER_CALL";

/// Environment variable name for overriding `--docket-concurrency` (`docs/architecture/nodes/docket.md` §5).
pub const ENV_DOCKET_CONCURRENCY: &str = "CANON_CLERK_DOCKET_CONCURRENCY";

/// Alias environment variable name for overriding `--max-concurrent-calls`.
pub const ENV_MAX_CONCURRENT_CALLS: &str = "CANON_CLERK_MAX_CONCURRENT_CALLS";

/// Domain input parameters for executing the `docket` pipeline stage.
#[derive(Debug, Clone, PartialEq)]
pub struct DocketOptions {
    /// Subject-matter jurisdiction score threshold in `[0.0, 1.0]` (defaults to [`DEFAULT_DOCKET_THRESHOLD`]).
    pub threshold: f64,
    /// Per-request timeout ceiling in milliseconds (defaults to [`DEFAULT_GENERATION_TIMEOUT_MS`]).
    pub timeout_ms: u64,
    /// Optional maximum candidate canons evaluated per screener model call (`>= 1`; `None` evaluates all in a single turn).
    pub max_canons_per_call: Option<usize>,
    /// Optional maximum concurrent in-flight screener model calls when batched (`>= 1`; defaults to [`DEFAULT_DOCKET_MAX_CONCURRENT_CALLS`]).
    pub max_concurrent_calls: Option<usize>,
}

impl Default for DocketOptions {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_DOCKET_THRESHOLD,
            timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
            max_canons_per_call: None,
            max_concurrent_calls: None,
        }
    }
}

/// Domain errors produced during `docket` execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocketError {
    /// Configured threshold is non-finite or outside `[0.0, 1.0]`.
    InvalidThreshold { detail: String },
    /// Configured `max_canons_per_call` is zero (`0`).
    InvalidBatchSize { detail: String },
    /// Configured `max_concurrent_calls` is zero (`0`).
    InvalidConcurrency { detail: String },
    /// Case-or-Controversy violation: `.intake` is missing from the incoming `Caseload`.
    MissingIntake,
    /// Prerequisite `.discovery` namespace is missing from the incoming `Caseload`.
    MissingDiscovery,
    /// Upstream `.validation` recorded one or more candidate canon lint errors.
    ValidationFailed,
    /// Prerequisite `.config` namespace is missing when non-empty candidate canons require model screening.
    MissingConfig,
    /// A candidate canon listed in `.discovery.candidate_canons` is absent from the workspace snapshot.
    MissingCandidateCanon { canon_ref: String },
    /// Structured model provider returned a transport, HTTP, block, or schema decoding error.
    Provider(ProviderError),
}

impl fmt::Display for DocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidThreshold { detail } => write!(
                f,
                "Invalid docket threshold: {detail}.\n  \
                 Hint: Pass a finite threshold in `[0.0, 1.0]` via `--docket-threshold <FLOAT>` \
                 or `{ENV_DOCKET_THRESHOLD}` (default: `{DEFAULT_DOCKET_THRESHOLD}`)."
            ),
            Self::InvalidBatchSize { detail } => write!(
                f,
                "Invalid docket batch size: {detail}.\n  \
                 Hint: Pass a positive integer (`>= 1`) via `--docket-batch-size <COUNT>` \
                 or `{ENV_DOCKET_BATCH_SIZE}`."
            ),
            Self::InvalidConcurrency { detail } => write!(
                f,
                "Invalid docket concurrency limit: {detail}.\n  \
                 Hint: Pass a positive integer (`>= 1`) via `--docket-concurrency <COUNT>` \
                 or `{ENV_DOCKET_CONCURRENCY}` (default: `{DEFAULT_DOCKET_MAX_CONCURRENT_CALLS}`)."
            ),
            Self::MissingIntake => write!(
                f,
                "Macro jurisdiction triage (`docket`) requires an underlying filing in `.intake` (Case-or-Controversy invariant).\n  \
                 Hint: Pipe a diff (`--diff -`), specify target paths (`src/...`, `-`, `--all-targets`), \
                 or pass `--caseload <PATH|->` with `.intake` populated."
            ),
            Self::MissingDiscovery => write!(
                f,
                "Macro jurisdiction triage (`docket`) requires a resolved `.discovery` namespace.\n  \
                 Hint: Run `canon-clerk discover` first or invoke `canon-clerk docket` with a filing source."
            ),
            Self::ValidationFailed => write!(
                f,
                "Cannot execute `docket` because one or more candidate canons failed static validation.\n  \
                 Hint: Run `canon-clerk validate` to inspect and repair malformed candidate canons."
            ),
            Self::MissingConfig => write!(
                f,
                "Cannot execute `docket` screening without a resolved `.config` namespace on the Caseload.\n  \
                 Hint: Run `canon-clerk configure` first or invoke `canon-clerk docket` directly to resolve configuration JIT."
            ),
            Self::MissingCandidateCanon { canon_ref } => write!(
                f,
                "Candidate canon `{canon_ref}` referenced in `.discovery` was not found in the workspace snapshot.\n  \
                 Hint: Re-run `canon-clerk discover` against the current workspace."
            ),
            Self::Provider(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for DocketError {}

impl From<ProviderError> for DocketError {
    fn from(err: ProviderError) -> Self {
        Self::Provider(err)
    }
}

fn validate_batching_options(options: &DocketOptions) -> Result<(), DocketError> {
    if options.max_canons_per_call == Some(0) {
        return Err(DocketError::InvalidBatchSize {
            detail: "max_canons_per_call must be at least 1, got 0".to_string(),
        });
    }
    if options.max_concurrent_calls == Some(0) {
        return Err(DocketError::InvalidConcurrency {
            detail: "max_concurrent_calls must be at least 1, got 0".to_string(),
        });
    }
    Ok(())
}

/// Partitions `candidate_canons` into deterministic, balanced contiguous batches of at most `max_canons_per_call`.
pub fn partition_candidate_canons(
    candidate_canons: &[String],
    max_canons_per_call: Option<usize>,
) -> Vec<Vec<String>> {
    let total = candidate_canons.len();
    if total == 0 {
        return Vec::new();
    }
    match max_canons_per_call {
        Some(batch_size) if batch_size >= 1 && batch_size < total => {
            let num_batches = total.div_ceil(batch_size);
            let base_len = total / num_batches;
            let remainder = total % num_batches;
            let mut batches = Vec::with_capacity(num_batches);
            let mut offset = 0;
            for idx in 0..num_batches {
                let len = base_len + usize::from(idx < remainder);
                batches.push(candidate_canons[offset..offset + len].to_vec());
                offset += len;
            }
            batches
        }
        _ => vec![candidate_canons.to_vec()],
    }
}

/// Executes the pure `docket` domain stage against an in-memory [`WorkspaceSnapshot`] and
/// hexagonal [`StructuredModelProvider`] (`docs/architecture/nodes/docket.md` §4).
pub fn execute_docket(
    options: &DocketOptions,
    caseload: Caseload,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
) -> Result<Caseload, DocketError> {
    let threshold = normalize_colorability_score(options.threshold)
        .map_err(|detail| DocketError::InvalidThreshold { detail })?;
    validate_batching_options(options)?;

    if caseload.docket.is_some() {
        return Ok(caseload);
    }

    let intake = caseload.intake.as_ref().ok_or(DocketError::MissingIntake)?;
    let discovery = caseload
        .discovery
        .as_ref()
        .ok_or(DocketError::MissingDiscovery)?;

    if discovery.candidate_canons.is_empty() {
        return Ok(caseload.with_docket(CaseloadDocket::default()));
    }

    if caseload
        .validation
        .as_ref()
        .is_some_and(|val| val.has_errors)
    {
        return Err(DocketError::ValidationFailed);
    }

    let config = caseload.config.as_ref().ok_or(DocketError::MissingConfig)?;
    if model_provider(&config.screener_model) == TYPESAFE_PROVIDER {
        return decision::execute_docket_decisions(
            options, caseload, threshold, workspace, provider,
        );
    }

    let filing_prefix = build_docket_filing_prefix(intake, discovery);
    let response_schema = build_docket_schema();
    let batches =
        partition_candidate_canons(&discovery.candidate_canons, options.max_canons_per_call);

    let mut prepared_batches = Vec::with_capacity(batches.len());
    for batch_canons in batches {
        let user_prompt =
            build_docket_batch_user_prompt(&filing_prefix, &batch_canons, discovery, workspace)
                .map_err(|canon_ref| DocketError::MissingCandidateCanon { canon_ref })?;
        let request = GenerationRequest {
            model_specifier: config.screener_model.clone(),
            system_instruction: DOCKET_SYSTEM_INSTRUCTION.to_string(),
            user_prompt,
            response_schema: response_schema.clone(),
            reasoning_budget: None,
            timeout_ms: options.timeout_ms,
        };
        prepared_batches.push((batch_canons, request));
    }

    let concurrency_limit = options
        .max_concurrent_calls
        .unwrap_or(DEFAULT_DOCKET_MAX_CONCURRENT_CALLS);
    let cases = execute_docket_batches(
        &prepared_batches,
        concurrency_limit,
        threshold,
        &config.screener_model,
        provider,
    )?;

    Ok(caseload.with_docket(CaseloadDocket::from_cases(cases)))
}

type BatchCasesResult = Result<BTreeMap<String, CanonDocketAssessment>, DocketError>;

fn execute_docket_batches(
    prepared_batches: &[(Vec<String>, GenerationRequest)],
    concurrency_limit: usize,
    threshold: f64,
    model_specifier: &str,
    provider: &dyn StructuredModelProvider,
) -> BatchCasesResult {
    let worker_count = concurrency_limit.min(prepared_batches.len()).max(1);
    if worker_count == 1 {
        let mut combined = BTreeMap::new();
        for (batch_canons, request) in prepared_batches {
            let response = provider.generate_structured(request)?;
            let batch_cases =
                decode_docket_cases(&response.payload, batch_canons, threshold, model_specifier)?;
            combined.extend(batch_cases);
        }
        return Ok(combined);
    }

    let next_index = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let outcomes: Mutex<Vec<Option<BatchCasesResult>>> =
        Mutex::new((0..prepared_batches.len()).map(|_| None).collect());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if aborted.load(Ordering::Relaxed) {
                        break;
                    }
                    let idx = next_index.fetch_add(1, Ordering::SeqCst);
                    if idx >= prepared_batches.len() {
                        break;
                    }
                    let (batch_canons, request) = &prepared_batches[idx];
                    let res = provider
                        .generate_structured(request)
                        .map_err(Into::into)
                        .and_then(|response| {
                            decode_docket_cases(
                                &response.payload,
                                batch_canons,
                                threshold,
                                model_specifier,
                            )
                        });
                    if res.is_err() {
                        aborted.store(true, Ordering::Relaxed);
                    }
                    if let Ok(mut guard) = outcomes.lock() {
                        guard[idx] = Some(res);
                    }
                }
            });
        }
    });

    let collected = outcomes.into_inner().unwrap_or_default();
    let mut combined = BTreeMap::new();
    for slot in collected.into_iter().flatten() {
        let batch_cases = slot?;
        combined.extend(batch_cases);
    }
    Ok(combined)
}

fn schema_decode_error(model_specifier: &str, message: impl Into<String>) -> DocketError {
    DocketError::Provider(ProviderError::SchemaDecode {
        model_specifier: model_specifier.to_string(),
        message: message.into(),
    })
}

fn decode_docket_cases(
    payload: &Value,
    candidate_canons: &[String],
    threshold: f64,
    model_specifier: &str,
) -> Result<BTreeMap<String, CanonDocketAssessment>, DocketError> {
    let cases_arr = payload
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            schema_decode_error(
                model_specifier,
                "missing array field `cases` in structured docket response",
            )
        })?;

    let expected: BTreeSet<&str> = candidate_canons.iter().map(String::as_str).collect();
    let same_len = cases_arr.len() == candidate_canons.len();
    let mut out = BTreeMap::new();

    for (index, raw_item) in cases_arr.iter().enumerate() {
        let pos_expected = same_len
            .then(|| candidate_canons.get(index).map(String::as_str))
            .flatten();
        let (canon_path, assessment) = decode_single_case(
            raw_item,
            index,
            &expected,
            pos_expected,
            threshold,
            model_specifier,
        )?;
        if out.insert(canon_path.clone(), assessment).is_some() {
            return Err(schema_decode_error(
                model_specifier,
                format!("duplicate `canon_path` `{canon_path}` in structured docket response"),
            ));
        }
    }

    let missing: Vec<&str> = candidate_canons
        .iter()
        .map(String::as_str)
        .filter(|canon_ref| !out.contains_key(*canon_ref))
        .collect();
    if !missing.is_empty() {
        return Err(schema_decode_error(
            model_specifier,
            format!(
                "missing case entry for candidate canon(s): {}",
                missing.join(", ")
            ),
        ));
    }

    Ok(out)
}

pub(crate) fn resolve_canon_path<'a>(
    raw_path: &'a str,
    expected: &BTreeSet<&'a str>,
    pos: Option<&'a str>,
) -> Option<&'a str> {
    if let Some(&exact) = expected.get(raw_path) {
        return Some(exact);
    }
    let pos = pos?;
    if raw_path.rsplit('/').next() == pos.rsplit('/').next() {
        return Some(pos);
    }
    let pref = raw_path
        .bytes()
        .zip(pos.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suff = raw_path
        .bytes()
        .rev()
        .zip(pos.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    ((pref + suff) * 5 >= raw_path.len().max(pos.len()).max(1) * 4).then_some(pos)
}

fn decode_single_case(
    raw_item: &Value,
    index: usize,
    expected: &BTreeSet<&str>,
    positional_expected: Option<&str>,
    threshold: f64,
    model: &str,
) -> Result<(String, CanonDocketAssessment), DocketError> {
    let entry = raw_item.as_object().ok_or_else(|| {
        schema_decode_error(
            model,
            format!("expected object at `cases[{index}]` in structured docket response"),
        )
    })?;
    let raw_canon_path = entry["canon_path"].as_str().ok_or_else(|| {
        schema_decode_error(
            model,
            format!("missing string `canon_path` at `cases[{index}]`"),
        )
    })?;
    let canon_path =
        resolve_canon_path(raw_canon_path, expected, positional_expected).ok_or_else(|| {
            schema_decode_error(
                model,
                format!("unknown `canon_path` `{raw_canon_path}` in structured docket response"),
            )
        })?;
    let raw_summary = entry["colorability_summary"].as_str().ok_or_else(|| {
        schema_decode_error(
            model,
            format!("missing string `colorability_summary` for `{canon_path}`"),
        )
    })?;
    let raw_score = entry["colorability_score"].as_f64().ok_or_else(|| {
        schema_decode_error(
            model,
            format!("missing numeric `colorability_score` for `{canon_path}`"),
        )
    })?;
    let colorability_score = normalize_colorability_score(raw_score).map_err(|detail| {
        schema_decode_error(
            model,
            format!("invalid `colorability_score` for `{canon_path}`: {detail}"),
        )
    })?;

    Ok((
        canon_path.to_string(),
        CanonDocketAssessment {
            colorability_summary: raw_summary.trim().to_string(),
            colorability_score,
            colorability_status: ColorabilityStatus::from_score(colorability_score, threshold),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::ParseCanonOptions;
    use crate::caseload::{
        ActiveExhibits, CaseloadConfig, CaseloadDiscovery, CaseloadIntake, CaseloadValidation,
        DEFAULT_SCREENER_MODEL, FileArtifact, FileChangeStatus, LinkedIssueContext,
    };
    use crate::provider::{GenerationResponse, TokenUsage};
    use serde_json::json;
    use std::sync::Mutex;

    struct RecordingMockProvider {
        requests: Mutex<Vec<GenerationRequest>>,
        outcome: Result<Value, ProviderError>,
    }

    impl RecordingMockProvider {
        fn with_payload(payload: Value) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                outcome: Ok(payload),
            }
        }

        fn with_error(err: ProviderError) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                outcome: Err(err),
            }
        }

        fn captured_requests(&self) -> Vec<GenerationRequest> {
            self.requests.lock().expect("lock requests").clone()
        }
    }

    impl StructuredModelProvider for RecordingMockProvider {
        fn generate_structured(
            &self,
            request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            self.requests
                .lock()
                .expect("lock requests")
                .push(request.clone());
            match &self.outcome {
                Ok(payload) => Ok(GenerationResponse {
                    payload: payload.clone(),
                    thoughts: Vec::new(),
                    usage: TokenUsage {
                        prompt_tokens: 310,
                        completion_tokens: 72,
                        reasoning_tokens: 0,
                        total_tokens: 382,
                    },
                    latency_ms: 48,
                    resolved_model: "gemini-2.5-flash-lite".to_string(),
                }),
                Err(err) => Err(err.clone()),
            }
        }
    }

    fn sample_workspace() -> WorkspaceSnapshot {
        let mut ws = WorkspaceSnapshot::new();
        ws = ws.with_canon_markdown(
            "---\ntriggers:\n  - \"src/cli/**\"\n---\n# CLI Flags Kebab Case\n\nCLI flags MUST use kebab-case naming.\n",
            &ParseCanonOptions::with_source_ref(".canons/cli/cli-flags-kebab-case.md"),
        );
        ws = ws.with_canon_markdown(
            "---\ntriggers:\n  - \"src/**\"\n---\n# Database Migrations Must Include Rollback\n\nDatabase schema migrations MUST define rollback steps.\n",
            &ParseCanonOptions::with_source_ref(".canons/db/migrations-rollback.md"),
        );
        ws
    }

    fn sample_filing_caseload(candidates: Vec<String>) -> Caseload {
        let mut diffs = BTreeMap::new();
        diffs.insert(
            "src/cli/docket.rs".to_string(),
            FileArtifact {
                path: "src/cli/docket.rs".to_string(),
                status: FileChangeStatus::Added,
                previous_path: None,
                lines_added: 18,
                lines_deleted: 0,
                patch: Some("@@ -0,0 +1,18 @@\n+#[arg(long = \"docket-threshold\")]\n".to_string()),
                patch_omission_reason: None,
                content: None,
                content_omission_reason: None,
            },
        );
        let mut triggers_join = BTreeMap::new();
        for c in &candidates {
            triggers_join.insert(c.clone(), vec!["src/cli/docket.rs".to_string()]);
        }

        Caseload::new()
            .with_intake(CaseloadIntake {
                pr_title: Some("feat(cli): add docket subcommand".to_string()),
                pr_body: Some("Implements macro jurisdiction screening.".to_string()),
                intent: None,
                diffs,
                linked_issues: Some(vec![LinkedIssueContext {
                    number: 260,
                    title: Some("Implement docket stage".to_string()),
                    body: Some("Add CaseloadDocket and CLI.".to_string()),
                    url: None,
                }]),
                ..CaseloadIntake::default()
            })
            .with_discovery(CaseloadDiscovery {
                target_files: vec!["src/cli/docket.rs".to_string()],
                candidate_canons: candidates,
                active_exhibits: ActiveExhibits {
                    files: vec!["src/cli/docket.rs".to_string()],
                    pr_title: Some(true),
                    pr_body: Some(true),
                    commit_messages: None,
                    linked_issues: Some(true),
                },
                triggers_join,
                ..CaseloadDiscovery::default()
            })
            .with_validation(CaseloadValidation {
                has_errors: false,
                results: BTreeMap::new(),
            })
            .with_config(CaseloadConfig::default())
    }

    mod zero_candidate_and_prerequisite_guards {
        use super::*;

        #[test]
        fn test_empty_candidate_canons_short_circuits_with_zero_model_calls_and_no_config_needed() {
            let ws = sample_workspace();
            let provider = RecordingMockProvider::with_payload(json!({}));
            let caseload = Caseload::new()
                .with_intake(CaseloadIntake::default())
                .with_discovery(CaseloadDiscovery::default());

            let enriched = execute_docket(&DocketOptions::default(), caseload, &ws, &provider)
                .expect("zero-candidate docket must short-circuit cleanly");

            assert!(provider.captured_requests().is_empty());
            assert!(enriched.config.is_none());
            let docket = enriched.docket.expect("docket namespace populated");
            assert!(docket.cases.is_empty());
            assert!(docket.active_docket.is_empty());
            assert!(docket.anomalies.is_empty());
        }

        #[test]
        fn test_fast_forwards_when_docket_already_populated() {
            let ws = sample_workspace();
            let provider = RecordingMockProvider::with_payload(json!({}));
            let pre_populated = Caseload::new()
                .with_intake(CaseloadIntake::default())
                .with_docket(CaseloadDocket::default());

            let out = execute_docket(
                &DocketOptions::default(),
                pre_populated.clone(),
                &ws,
                &provider,
            )
            .expect("fast-forward completed docket");
            assert_eq!(out, pre_populated);
            assert!(provider.captured_requests().is_empty());
        }

        #[test]
        fn test_enforces_case_or_controversy_and_prerequisite_guards() {
            let ws = sample_workspace();
            let provider = RecordingMockProvider::with_payload(json!({}));

            // Abstract corpus-only caseload (from `validate --all-canons`) lacks `.intake`
            let abstract_corpus = Caseload::new()
                .with_discovery(CaseloadDiscovery {
                    candidate_canons: vec![".canons/cli/cli-flags-kebab-case.md".to_string()],
                    ..CaseloadDiscovery::default()
                })
                .with_validation(CaseloadValidation::default());
            let err = execute_docket(&DocketOptions::default(), abstract_corpus, &ws, &provider)
                .expect_err("null intake must violate Case-or-Controversy");
            assert_eq!(err, DocketError::MissingIntake);
            assert!(err.to_string().contains("Case-or-Controversy"));

            // Missing discovery
            let no_disc = Caseload::new().with_intake(CaseloadIntake::default());
            assert_eq!(
                execute_docket(&DocketOptions::default(), no_disc, &ws, &provider).unwrap_err(),
                DocketError::MissingDiscovery
            );

            // Validation failed
            let mut failed_val =
                sample_filing_caseload(vec![".canons/cli/cli-flags-kebab-case.md".to_string()]);
            failed_val.validation = Some(CaseloadValidation {
                has_errors: true,
                results: BTreeMap::new(),
            });
            assert_eq!(
                execute_docket(&DocketOptions::default(), failed_val, &ws, &provider).unwrap_err(),
                DocketError::ValidationFailed
            );

            // Missing config when candidates > 0
            let mut no_cfg =
                sample_filing_caseload(vec![".canons/cli/cli-flags-kebab-case.md".to_string()]);
            no_cfg.config = None;
            assert_eq!(
                execute_docket(&DocketOptions::default(), no_cfg, &ws, &provider).unwrap_err(),
                DocketError::MissingConfig
            );

            // Missing candidate canon in snapshot
            let missing_canon = sample_filing_caseload(vec![".canons/nonexistent.md".to_string()]);
            assert_eq!(
                execute_docket(&DocketOptions::default(), missing_canon, &ws, &provider)
                    .unwrap_err(),
                DocketError::MissingCandidateCanon {
                    canon_ref: ".canons/nonexistent.md".to_string()
                }
            );

            // Invalid thresholds
            for bad in [f64::NAN, -0.1, 1.1] {
                let err = execute_docket(
                    &DocketOptions {
                        threshold: bad,
                        ..DocketOptions::default()
                    },
                    Caseload::new(),
                    &ws,
                    &provider,
                )
                .expect_err("out-of-bounds threshold must fail");
                assert!(matches!(err, DocketError::InvalidThreshold { .. }));
            }
        }

        #[test]
        fn test_rejects_zero_batch_size_and_zero_concurrency_with_hints() {
            let ws = sample_workspace();
            let provider = RecordingMockProvider::with_payload(json!({}));

            let zero_batch = execute_docket(
                &DocketOptions {
                    max_canons_per_call: Some(0),
                    ..DocketOptions::default()
                },
                Caseload::new(),
                &ws,
                &provider,
            )
            .expect_err("zero batch size must fail");
            assert!(matches!(zero_batch, DocketError::InvalidBatchSize { .. }));
            assert!(zero_batch.to_string().contains("Hint:"));

            let zero_conc = execute_docket(
                &DocketOptions {
                    max_concurrent_calls: Some(0),
                    ..DocketOptions::default()
                },
                Caseload::new(),
                &ws,
                &provider,
            )
            .expect_err("zero concurrency must fail");
            assert!(matches!(zero_conc, DocketError::InvalidConcurrency { .. }));
            assert!(zero_conc.to_string().contains("Hint:"));
        }
    }

    mod aggregate_screening_and_threshold_gating {
        use super::*;

        #[test]
        fn test_single_turn_domain_indirected_reason_first_schema_and_deterministic_active_docket()
        {
            let ws = sample_workspace();
            // Emit items in reverse lexicographical order to verify deterministic BTreeMap + active_docket sorting
            let provider = RecordingMockProvider::with_payload(json!({
                "cases": [
                    {
                        "canon_path": ".canons/db/migrations-rollback.md",
                        "colorability_summary": " Filing only adds CLI flag handling and touches zero database migrations. ",
                        "colorability_score": 0.04
                    },
                    {
                        "canon_path": ".canons/cli/cli-flags-kebab-case.md",
                        "colorability_summary": "Filing introduces `--docket-threshold` flag in `src/cli/docket.rs`.",
                        "colorability_score": 0.95
                    }
                ]
            }));

            let caseload = sample_filing_caseload(vec![
                ".canons/cli/cli-flags-kebab-case.md".to_string(),
                ".canons/db/migrations-rollback.md".to_string(),
            ]);

            let enriched = execute_docket(&DocketOptions::default(), caseload, &ws, &provider)
                .expect("execute docket screening");

            let requests = provider.captured_requests();
            assert_eq!(requests.len(), 1, "docket must execute in a single turn");
            let req = &requests[0];
            assert_eq!(req.model_specifier, DEFAULT_SCREENER_MODEL);
            assert_eq!(req.reasoning_budget, None);
            assert!(req.user_prompt.contains("feat(cli): add docket subcommand"));
            assert!(req.user_prompt.contains("#260: Implement docket stage"));
            assert!(req.user_prompt.contains("src/cli/docket.rs"));
            assert!(req.user_prompt.contains("docket-threshold"));

            let ordering = &req.response_schema["properties"]["cases"]["items"]["propertyOrdering"];
            assert_eq!(
                ordering,
                &json!(["canon_path", "colorability_summary", "colorability_score"])
            );

            let docket = enriched.docket.expect("docket attached");
            assert_eq!(
                docket.active_docket,
                vec![".canons/cli/cli-flags-kebab-case.md".to_string()]
            );
            assert_eq!(docket.docketed_count(), 1);
            assert_eq!(docket.dismissed_count(), 1);
            assert_eq!(
                docket.cases[".canons/db/migrations-rollback.md"].colorability_summary,
                "Filing only adds CLI flag handling and touches zero database migrations."
            );
        }

        #[test]
        fn test_scoped_canon_omits_redundant_scope_line_from_docket_prompt() {
            let mut ws = WorkspaceSnapshot::new();
            let scoped_ref = "packages/ui/.canons/accessibility.md";
            ws = ws.with_canon_markdown(
                "---\ntriggers:\n  - \"**/*.tsx\"\n---\n# Accessible Buttons\n\nButtons MUST provide an accessible label.\n",
                &ParseCanonOptions::with_source_ref(scoped_ref),
            );
            assert_eq!(
                ws.canons[scoped_ref].scope.as_deref(),
                Some("packages/ui"),
                "internal monorepo package scope must remain intact on Canon"
            );

            let provider = RecordingMockProvider::with_payload(json!({
                "cases": [
                    {
                        "canon_path": scoped_ref,
                        "colorability_summary": "Touches UI button component.",
                        "colorability_score": 0.90
                    }
                ]
            }));
            let caseload = sample_filing_caseload(vec![scoped_ref.to_string()]);
            execute_docket(&DocketOptions::default(), caseload, &ws, &provider)
                .expect("execute docket for scoped canon");

            let prompt = &provider.captured_requests()[0].user_prompt;
            assert!(prompt.contains("### Canon: `packages/ui/.canons/accessibility.md`"));
            assert!(
                !prompt.contains("- Scope:"),
                "docket prompt must not emit redundant `- Scope:` line: {prompt}"
            );
        }

        #[test]
        fn test_constant_size_wire_schema_across_60_candidate_canons_and_threshold_boundary() {
            let mut ws = WorkspaceSnapshot::new();
            let mut candidates = Vec::new();
            let mut wire_cases = Vec::new();
            for i in 0..60 {
                let path = format!(".canons/pack/rule-{i:03}.md");
                ws = ws.with_canon_markdown(
                    &format!("# Rule {i}\n\nRule {i} MUST hold.\n"),
                    &ParseCanonOptions::with_source_ref(path.clone()),
                );
                let score = if i == 30 { 0.70 } else { 0.69 };
                wire_cases.push(json!({
                    "canon_path": path,
                    "colorability_summary": format!("Evaluated rule {i}."),
                    "colorability_score": score
                }));
                candidates.push(path);
            }

            let baseline_schema = build_docket_schema();
            let provider = RecordingMockProvider::with_payload(json!({ "cases": wire_cases }));
            let caseload = sample_filing_caseload(candidates);

            let enriched = execute_docket(
                &DocketOptions {
                    threshold: 0.70,
                    ..DocketOptions::default()
                },
                caseload,
                &ws,
                &provider,
            )
            .expect("60-canon docket succeeds");

            let req = &provider.captured_requests()[0];
            assert_eq!(
                req.response_schema, baseline_schema,
                "wire schema must remain O(1) constant size regardless of candidate canon count"
            );

            let docket = enriched.docket.expect("docket present");
            assert_eq!(docket.cases.len(), 60);
            assert_eq!(
                docket.active_docket,
                vec![".canons/pack/rule-030.md".to_string()],
                "exact threshold boundary score == 0.70 must be docketed"
            );
        }
    }

    mod malformed_and_failed_provider_responses {
        use super::*;

        #[test]
        fn test_rejects_non_array_missing_unknown_duplicate_and_out_of_range_payloads() {
            let ws = sample_workspace();
            let two_canons = vec![
                ".canons/cli/cli-flags-kebab-case.md".to_string(),
                ".canons/db/migrations-rollback.md".to_string(),
            ];

            let bad_payloads = vec![
                // Non-array cases
                json!({ "cases": {} }),
                // Non-object element
                json!({ "cases": ["not-an-object"] }),
                // Unknown canon_path
                json!({
                    "cases": [{
                        "canon_path": ".canons/unknown.md",
                        "colorability_summary": "Unknown",
                        "colorability_score": 0.5
                    }]
                }),
                // Duplicate canon_path
                json!({
                    "cases": [
                        {
                            "canon_path": ".canons/cli/cli-flags-kebab-case.md",
                            "colorability_summary": "First",
                            "colorability_score": 0.8
                        },
                        {
                            "canon_path": ".canons/cli/cli-flags-kebab-case.md",
                            "colorability_summary": "Duplicate",
                            "colorability_score": 0.8
                        }
                    ]
                }),
                // Missing second candidate canon
                json!({
                    "cases": [{
                        "canon_path": ".canons/cli/cli-flags-kebab-case.md",
                        "colorability_summary": "Only one",
                        "colorability_score": 0.8
                    }]
                }),
                // Out-of-range score > 1.0
                json!({
                    "cases": [
                        {
                            "canon_path": ".canons/cli/cli-flags-kebab-case.md",
                            "colorability_summary": "Too high",
                            "colorability_score": 1.2
                        },
                        {
                            "canon_path": ".canons/db/migrations-rollback.md",
                            "colorability_summary": "Ok",
                            "colorability_score": 0.1
                        }
                    ]
                }),
            ];

            for payload in bad_payloads {
                let provider = RecordingMockProvider::with_payload(payload);
                let caseload = sample_filing_caseload(two_canons.clone());
                let err = execute_docket(&DocketOptions::default(), caseload, &ws, &provider)
                    .expect_err("malformed payload must fail");
                assert!(
                    matches!(
                        err,
                        DocketError::Provider(ProviderError::SchemaDecode { .. })
                    ),
                    "expected SchemaDecode error, got: {err:?}"
                );
            }

            let transport_provider = RecordingMockProvider::with_error(ProviderError::Timeout {
                model_specifier: DEFAULT_SCREENER_MODEL.to_string(),
                timeout_ms: 5_000,
            });
            let err = execute_docket(
                &DocketOptions::default(),
                sample_filing_caseload(two_canons),
                &ws,
                &transport_provider,
            )
            .expect_err("provider timeout must propagate");
            assert!(matches!(
                err,
                DocketError::Provider(ProviderError::Timeout { .. })
            ));
        }
    }

    mod batching_and_concurrency {
        use super::*;
        use crate::telemetry::{
            PipelineStage, RecordingTelemetrySink, SteppingTelemetryClock, TelemetryEvent,
            TelemetrySession,
        };
        use std::time::Duration;

        struct OutOfOrderBatchProvider {
            call_order: Mutex<Vec<String>>,
            fail_on_canon: Option<String>,
        }

        impl StructuredModelProvider for OutOfOrderBatchProvider {
            fn generate_structured(
                &self,
                request: &GenerationRequest,
            ) -> Result<GenerationResponse, ProviderError> {
                let mut batch_canons = Vec::new();
                for line in request.user_prompt.lines() {
                    if let Some(rest) = line.strip_prefix("### Canon: `")
                        && let Some(canon_ref) = rest.strip_suffix('`')
                    {
                        batch_canons.push(canon_ref.to_string());
                    }
                }

                // Force early batches to sleep longer so later batches complete first out of order
                if batch_canons.iter().any(|c| c.contains("rule-000")) {
                    std::thread::sleep(Duration::from_millis(35));
                } else if batch_canons.iter().any(|c| c.contains("rule-002")) {
                    std::thread::sleep(Duration::from_millis(15));
                }

                if let Some(fail_target) = &self.fail_on_canon
                    && batch_canons.iter().any(|c| c == fail_target)
                {
                    return Err(ProviderError::Timeout {
                        model_specifier: request.model_specifier.clone(),
                        timeout_ms: request.timeout_ms,
                    });
                }

                self.call_order
                    .lock()
                    .expect("lock call_order")
                    .push(batch_canons.first().cloned().unwrap_or_default());

                let mut cases = Vec::new();
                for canon_ref in batch_canons.into_iter().rev() {
                    let is_even = canon_ref
                        .chars()
                        .rfind(|c| c.is_ascii_digit())
                        .is_some_and(|d| (d as u8).is_multiple_of(2));
                    let score = if is_even { 0.85 } else { 0.15 };
                    cases.push(json!({
                        "canon_path": canon_ref,
                        "colorability_summary": format!("Assessed {canon_ref}."),
                        "colorability_score": score
                    }));
                }

                Ok(GenerationResponse {
                    payload: json!({ "cases": cases }),
                    thoughts: vec!["Batch thought chunk.".to_string()],
                    usage: TokenUsage {
                        prompt_tokens: 100,
                        completion_tokens: 25,
                        reasoning_tokens: 0,
                        total_tokens: 125,
                    },
                    latency_ms: 15,
                    resolved_model: "gemini-2.5-flash-lite".to_string(),
                })
            }
        }

        #[test]
        fn test_partition_candidate_canons_handles_empty_unbounded_and_remainder_slices() {
            assert!(partition_candidate_canons(&[], Some(5)).is_empty());
            let five: Vec<String> = (0..5).map(|i| format!("c{i}")).collect();
            assert_eq!(partition_candidate_canons(&five, None), vec![five.clone()]);
            assert_eq!(
                partition_candidate_canons(&five, Some(10)),
                vec![five.clone()]
            );
            assert_eq!(
                partition_candidate_canons(&five, Some(2)),
                vec![
                    vec!["c0".to_string(), "c1".to_string()],
                    vec!["c2".to_string(), "c3".to_string()],
                    vec!["c4".to_string()],
                ]
            );
        }

        #[test]
        fn test_out_of_order_concurrent_batches_fold_deterministically_and_aggregate_telemetry() {
            let mut ws = WorkspaceSnapshot::new();
            let mut candidates = Vec::new();
            for i in 0..5 {
                let path = format!(".canons/pack/rule-{i:03}.md");
                ws = ws.with_canon_markdown(
                    &format!("# Rule {i}\n\nRule {i} MUST hold.\n"),
                    &ParseCanonOptions::with_source_ref(path.clone()),
                );
                candidates.push(path);
            }
            let caseload = sample_filing_caseload(candidates);

            // 1. Unbounded single-turn baseline
            let single_provider = OutOfOrderBatchProvider {
                call_order: Mutex::new(Vec::new()),
                fail_on_canon: None,
            };
            let single_res = execute_docket(
                &DocketOptions::default(),
                caseload.clone(),
                &ws,
                &single_provider,
            )
            .expect("single-turn baseline");

            // 2. Batched (batch size 2 => 3 batches: [0,1], [2,3], [4]) with concurrency 3 + telemetry
            let batched_provider = OutOfOrderBatchProvider {
                call_order: Mutex::new(Vec::new()),
                fail_on_canon: None,
            };
            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 10);
            let session = TelemetrySession::new(&sink, &clock);

            let batched_res = session
                .run_stage(PipelineStage::Docket, |scope| {
                    let instrumented = scope.instrument_provider(&batched_provider);
                    execute_docket(
                        &DocketOptions {
                            max_canons_per_call: Some(2),
                            max_concurrent_calls: Some(3),
                            ..DocketOptions::default()
                        },
                        caseload.clone(),
                        &ws,
                        &instrumented,
                    )
                })
                .expect("concurrent batched docket");

            // Completion order was out-of-order (batch 2 finished before batch 0), yet Caseload is byte-for-byte identical
            let completion_order = batched_provider.call_order.lock().expect("lock").clone();
            assert_eq!(completion_order.len(), 3);
            assert_ne!(
                completion_order[0], ".canons/pack/rule-000.md",
                "batch 0 slept longest and must not finish first"
            );
            assert_eq!(batched_res, single_res);
            assert_eq!(
                serde_json::to_string(&batched_res).expect("json"),
                serde_json::to_string(&single_res).expect("json")
            );

            // Verify telemetry accumulated all 3 batch calls (3 * 125 = 375 total_tokens) and 3 Thought events
            let events = sink.events();
            let thought_count = events
                .iter()
                .filter(|e| matches!(e, TelemetryEvent::Thought { .. }))
                .count();
            assert_eq!(thought_count, 3);
            let stage_finish = events
                .iter()
                .find_map(|e| match e {
                    TelemetryEvent::StageFinish {
                        stage: PipelineStage::Docket,
                        usage,
                        ..
                    } => *usage,
                    _ => None,
                })
                .expect("Docket StageFinish usage");
            assert_eq!(
                stage_finish,
                TokenUsage {
                    prompt_tokens: 300,
                    completion_tokens: 75,
                    reasoning_tokens: 0,
                    total_tokens: 375,
                }
            );
        }

        #[test]
        fn test_concurrent_batch_error_propagates_cleanly() {
            let mut ws = WorkspaceSnapshot::new();
            let mut candidates = Vec::new();
            for i in 0..4 {
                let path = format!(".canons/pack/rule-{i:03}.md");
                ws = ws.with_canon_markdown(
                    &format!("# Rule {i}\n\nRule {i} MUST hold.\n"),
                    &ParseCanonOptions::with_source_ref(path.clone()),
                );
                candidates.push(path);
            }
            let caseload = sample_filing_caseload(candidates);
            let failing_provider = OutOfOrderBatchProvider {
                call_order: Mutex::new(Vec::new()),
                fail_on_canon: Some(".canons/pack/rule-002.md".to_string()),
            };

            let err = execute_docket(
                &DocketOptions {
                    max_canons_per_call: Some(2),
                    max_concurrent_calls: Some(2),
                    ..DocketOptions::default()
                },
                caseload,
                &ws,
                &failing_provider,
            )
            .expect_err("failing second batch must fail execute_docket");
            assert!(matches!(
                err,
                DocketError::Provider(ProviderError::Timeout { .. })
            ));
        }
    }
}
