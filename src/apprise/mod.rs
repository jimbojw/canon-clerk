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

//! Core domain execution engine for the `apprise` Caseload DAG stage (`docs/architecture/nodes/apprise.md`).

mod decision;
mod prompt;

use crate::caseload::{
    ApprisalStatus, CanonApprisalAssessment, Caseload, CaseloadApprisal, TYPESAFE_PROVIDER,
    normalize_unit_score,
};
use crate::configure::model_provider;
use crate::discover::WorkspaceSnapshot;
use crate::docket::{partition_candidate_canons, resolve_canon_path};
use crate::provider::{
    DEFAULT_GENERATION_TIMEOUT_MS, GenerationRequest, ProviderError, StructuredModelProvider,
};
use prompt::{
    APPRISE_SYSTEM_INSTRUCTION, build_apprise_batch_user_prompt, build_apprise_intent_prefix,
    build_apprise_schema,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Default prospective applicability score threshold for `apprise` (`docs/architecture/nodes/apprise.md` §4.4).
pub const DEFAULT_APPRISE_THRESHOLD: f64 = 0.5;

/// Default maximum concurrent in-flight screener model calls when `apprise` executes multiple batches.
pub const DEFAULT_APPRISE_MAX_CONCURRENT_CALLS: usize = 4;

/// Environment variable name for overriding `--apprise-threshold` (`docs/architecture/nodes/apprise.md` §5).
pub const ENV_APPRISE_THRESHOLD: &str = "CANON_CLERK_APPRISE_THRESHOLD";

/// Environment variable name for overriding `--apprise-batch-size` (`docs/architecture/nodes/apprise.md` §5).
pub const ENV_APPRISE_BATCH_SIZE: &str = "CANON_CLERK_APPRISE_BATCH_SIZE";

/// Environment variable name for overriding `--apprise-concurrency` (`docs/architecture/nodes/apprise.md` §5).
pub const ENV_APPRISE_CONCURRENCY: &str = "CANON_CLERK_APPRISE_CONCURRENCY";

/// Domain input parameters for executing the `apprise` pipeline stage.
#[derive(Debug, Clone, PartialEq)]
pub struct AppriseOptions {
    /// Prospective applicability score threshold in `[0.0, 1.0]` (defaults to [`DEFAULT_APPRISE_THRESHOLD`]).
    pub threshold: f64,
    /// Per-request timeout ceiling in milliseconds (defaults to [`DEFAULT_GENERATION_TIMEOUT_MS`]).
    pub timeout_ms: u64,
    /// Optional maximum candidate canons evaluated per screener model call (`>= 1`; `None` evaluates all in a single turn).
    pub max_canons_per_call: Option<usize>,
    /// Optional maximum concurrent in-flight screener model calls when batched (`>= 1`; defaults to [`DEFAULT_APPRISE_MAX_CONCURRENT_CALLS`]).
    pub max_concurrent_calls: Option<usize>,
}

impl Default for AppriseOptions {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_APPRISE_THRESHOLD,
            timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
            max_canons_per_call: None,
            max_concurrent_calls: None,
        }
    }
}

/// Domain errors produced during `apprise` execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppriseError {
    /// Configured threshold is non-finite or outside `[0.0, 1.0]`.
    InvalidThreshold { detail: String },
    /// Configured `max_canons_per_call` is zero (`0`).
    InvalidBatchSize { detail: String },
    /// Configured `max_concurrent_calls` is zero (`0`).
    InvalidConcurrency { detail: String },
    /// Prospective design `intent` is missing or blank on `.intake`.
    MissingIntent,
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

impl fmt::Display for AppriseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidThreshold { detail } => write!(
                f,
                "Invalid apprise threshold: {detail}.\n  \
                 Hint: Pass a finite threshold in `[0.0, 1.0]` via `--apprise-threshold <FLOAT>` \
                 or `{ENV_APPRISE_THRESHOLD}` (default: `{DEFAULT_APPRISE_THRESHOLD}`)."
            ),
            Self::InvalidBatchSize { detail } => write!(
                f,
                "Invalid apprise batch size: {detail}.\n  \
                 Hint: Pass a positive integer (`>= 1`) via `--apprise-batch-size <COUNT>` \
                 or `{ENV_APPRISE_BATCH_SIZE}`."
            ),
            Self::InvalidConcurrency { detail } => write!(
                f,
                "Invalid apprise concurrency limit: {detail}.\n  \
                 Hint: Pass a positive integer (`>= 1`) via `--apprise-concurrency <COUNT>` \
                 or `{ENV_APPRISE_CONCURRENCY}` (default: `{DEFAULT_APPRISE_MAX_CONCURRENT_CALLS}`)."
            ),
            Self::MissingIntent => write!(
                f,
                "Prospective apprisal requires a non-empty architectural design intent.\n  \
                 Hint: Pass `--intent <TEXT>` alongside target paths or canon scope, \
                 or provide an upstream caseload (`--caseload <PATH|->`) with `.intake.intent` populated."
            ),
            Self::MissingDiscovery => write!(
                f,
                "Prospective apprisal requires a resolved target or canon discovery scope.\n  \
                 Hint: Provide prospective target paths (`src/...`, `-`, `--diff <PATH|->`, `--all-targets`), \
                 canon scope (`--all-canons`, `--canon <PATH|GLOB>`), or `--caseload <PATH|->`."
            ),
            Self::ValidationFailed => write!(
                f,
                "Cannot execute `apprise` because one or more candidate canons failed static validation.\n  \
                 Hint: Run `canon-clerk validate` to inspect and repair malformed candidate canons."
            ),
            Self::MissingConfig => write!(
                f,
                "Cannot execute `apprise` screening without a resolved `.config` namespace on the Caseload.\n  \
                 Hint: Run `canon-clerk configure` first or invoke `canon-clerk apprise` directly to resolve configuration JIT."
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

impl std::error::Error for AppriseError {}

impl From<ProviderError> for AppriseError {
    fn from(err: ProviderError) -> Self {
        Self::Provider(err)
    }
}

fn validate_batching_options(options: &AppriseOptions) -> Result<(), AppriseError> {
    if options.max_canons_per_call == Some(0) {
        return Err(AppriseError::InvalidBatchSize {
            detail: "max_canons_per_call must be at least 1, got 0".to_string(),
        });
    }
    if options.max_concurrent_calls == Some(0) {
        return Err(AppriseError::InvalidConcurrency {
            detail: "max_concurrent_calls must be at least 1, got 0".to_string(),
        });
    }
    Ok(())
}

/// Executes the pure `apprise` domain stage against an in-memory [`WorkspaceSnapshot`] and
/// hexagonal [`StructuredModelProvider`] (`docs/architecture/nodes/apprise.md` §4).
pub fn execute_apprise(
    options: &AppriseOptions,
    caseload: Caseload,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
) -> Result<Caseload, AppriseError> {
    let threshold = normalize_unit_score(options.threshold)
        .map_err(|detail| AppriseError::InvalidThreshold { detail })?;
    validate_batching_options(options)?;

    if caseload.apprisal.is_some() {
        return Ok(caseload);
    }

    let intake = caseload
        .intake
        .as_ref()
        .ok_or(AppriseError::MissingIntent)?;
    if !intake
        .intent
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
    {
        return Err(AppriseError::MissingIntent);
    }

    let discovery = caseload
        .discovery
        .as_ref()
        .ok_or(AppriseError::MissingDiscovery)?;

    if discovery.candidate_canons.is_empty() {
        return Ok(caseload.with_apprisal(CaseloadApprisal::default()));
    }

    if caseload
        .validation
        .as_ref()
        .is_some_and(|val| val.has_errors)
    {
        return Err(AppriseError::ValidationFailed);
    }

    let config = caseload
        .config
        .as_ref()
        .ok_or(AppriseError::MissingConfig)?;
    if model_provider(&config.screener_model) == TYPESAFE_PROVIDER {
        return decision::execute_apprise_decisions(
            options, caseload, threshold, workspace, provider,
        );
    }

    let intent_prefix = build_apprise_intent_prefix(intake, discovery);
    let response_schema = build_apprise_schema();
    let batches =
        partition_candidate_canons(&discovery.candidate_canons, options.max_canons_per_call);

    let mut prepared_batches = Vec::with_capacity(batches.len());
    for batch_canons in batches {
        let user_prompt =
            build_apprise_batch_user_prompt(&intent_prefix, &batch_canons, discovery, workspace)
                .map_err(|canon_ref| AppriseError::MissingCandidateCanon { canon_ref })?;
        let request = GenerationRequest {
            model_specifier: config.screener_model.clone(),
            system_instruction: APPRISE_SYSTEM_INSTRUCTION.to_string(),
            user_prompt,
            response_schema: response_schema.clone(),
            reasoning_budget: None,
            timeout_ms: options.timeout_ms,
        };
        prepared_batches.push((batch_canons, request));
    }

    let concurrency_limit = options
        .max_concurrent_calls
        .unwrap_or(DEFAULT_APPRISE_MAX_CONCURRENT_CALLS);
    let assessments = execute_apprise_batches(
        &prepared_batches,
        concurrency_limit,
        threshold,
        &config.screener_model,
        provider,
    )?;

    Ok(caseload.with_apprisal(CaseloadApprisal { assessments }))
}

type BatchAssessmentsResult = Result<BTreeMap<String, CanonApprisalAssessment>, AppriseError>;

fn execute_apprise_batches(
    prepared_batches: &[(Vec<String>, GenerationRequest)],
    concurrency_limit: usize,
    threshold: f64,
    model_specifier: &str,
    provider: &dyn StructuredModelProvider,
) -> BatchAssessmentsResult {
    let worker_count = concurrency_limit.min(prepared_batches.len()).max(1);
    if worker_count == 1 {
        let mut combined = BTreeMap::new();
        for (batch_canons, request) in prepared_batches {
            let response = provider.generate_structured(request)?;
            let batch_assessments = decode_apprise_assessments(
                &response.payload,
                batch_canons,
                threshold,
                model_specifier,
            )?;
            combined.extend(batch_assessments);
        }
        return Ok(combined);
    }

    let next_index = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let outcomes: Mutex<Vec<Option<BatchAssessmentsResult>>> =
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
                            decode_apprise_assessments(
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
        let batch_assessments = slot?;
        combined.extend(batch_assessments);
    }
    Ok(combined)
}

fn schema_decode_error(model_specifier: &str, message: impl Into<String>) -> AppriseError {
    AppriseError::Provider(ProviderError::SchemaDecode {
        model_specifier: model_specifier.to_string(),
        message: message.into(),
    })
}

fn decode_apprise_assessments(
    payload: &Value,
    candidate_canons: &[String],
    threshold: f64,
    model_specifier: &str,
) -> Result<BTreeMap<String, CanonApprisalAssessment>, AppriseError> {
    let assessments_arr = payload
        .get("assessments")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            schema_decode_error(
                model_specifier,
                "missing array field `assessments` in structured apprise response",
            )
        })?;

    let expected: BTreeSet<&str> = candidate_canons.iter().map(String::as_str).collect();
    let same_len = assessments_arr.len() == candidate_canons.len();
    let mut out = BTreeMap::new();

    for (index, raw_item) in assessments_arr.iter().enumerate() {
        let pos_expected = same_len
            .then(|| candidate_canons.get(index).map(String::as_str))
            .flatten();
        let (canon_path, assessment) = decode_single_assessment(
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
                format!("duplicate `canon_path` `{canon_path}` in structured apprise response"),
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
                "missing assessment entry for candidate canon(s): {}",
                missing.join(", ")
            ),
        ));
    }

    Ok(out)
}

fn decode_single_assessment(
    raw_item: &Value,
    index: usize,
    expected: &BTreeSet<&str>,
    positional_expected: Option<&str>,
    threshold: f64,
    model_specifier: &str,
) -> Result<(String, CanonApprisalAssessment), AppriseError> {
    let entry = raw_item.as_object().ok_or_else(|| {
        schema_decode_error(
            model_specifier,
            format!("expected object at `assessments[{index}]` in structured apprise response"),
        )
    })?;

    let raw_canon_path = entry
        .get("canon_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            schema_decode_error(
                model_specifier,
                format!("missing string `canon_path` at `assessments[{index}]`"),
            )
        })?;

    let canon_path =
        resolve_canon_path(raw_canon_path, expected, positional_expected).ok_or_else(|| {
            schema_decode_error(
                model_specifier,
                format!("unknown `canon_path` `{raw_canon_path}` in structured apprise response"),
            )
        })?;

    let raw_summary = entry
        .get("apprisal_summary")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            schema_decode_error(
                model_specifier,
                format!("missing string `apprisal_summary` for `{canon_path}`"),
            )
        })?;

    let raw_score = entry
        .get("apprisal_score")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            schema_decode_error(
                model_specifier,
                format!("missing numeric `apprisal_score` for `{canon_path}`"),
            )
        })?;

    let apprisal_score = normalize_unit_score(raw_score).map_err(|detail| {
        schema_decode_error(
            model_specifier,
            format!("invalid `apprisal_score` for `{canon_path}`: {detail}"),
        )
    })?;

    Ok((
        canon_path.to_string(),
        CanonApprisalAssessment {
            apprisal_summary: raw_summary.trim().to_string(),
            apprisal_score,
            apprisal_status: ApprisalStatus::from_score(apprisal_score, threshold),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::ParseCanonOptions;
    use crate::caseload::{
        CaseloadConfig, CaseloadDiscovery, CaseloadIntake, CaseloadValidation,
        DEFAULT_SCREENER_MODEL,
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

        fn recorded_requests(&self) -> Vec<GenerationRequest> {
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
                    resolved_model: "gemini-2.5-flash-lite".to_string(),
                    latency_ms: 95,
                    usage: TokenUsage {
                        prompt_tokens: 410,
                        completion_tokens: 90,
                        reasoning_tokens: 0,
                        total_tokens: 500,
                    },
                    thoughts: Vec::new(),
                }),
                Err(err) => Err(err.clone()),
            }
        }
    }

    fn fixture_snapshot() -> WorkspaceSnapshot {
        let cache_canon = "---\ntriggers:\n  - \"src/auth/**\"\n---\nCaching layers MUST configure explicit TTL expiration.\n";
        let db_canon = "---\ntriggers:\n  - \"src/auth/**\"\n---\nDatabase schema migrations MUST provide rollback scripts.\n";
        WorkspaceSnapshot::new()
            .with_file("src/auth/provider.rs")
            .with_canon_markdown(
                cache_canon,
                &ParseCanonOptions::with_source_ref(".canons/auth/cache-expiry.md"),
            )
            .with_canon_markdown(
                db_canon,
                &ParseCanonOptions::with_source_ref(".canons/auth/db-rollback.md"),
            )
    }

    fn fixture_caseload(candidate_canons: Vec<String>) -> Caseload {
        Caseload::new()
            .with_intake(CaseloadIntake {
                intent: Some(
                    "Refactor auth provider dispatch with in-memory TTL cache".to_string(),
                ),
                target_paths: vec!["src/auth".to_string()],
                ..CaseloadIntake::default()
            })
            .with_discovery(CaseloadDiscovery {
                target_files: vec!["src/auth/provider.rs".to_string()],
                candidate_canons,
                ..CaseloadDiscovery::default()
            })
            .with_validation(CaseloadValidation::default())
            .with_config(CaseloadConfig::default())
    }

    mod zero_candidate_and_prerequisite_guards {
        use super::*;

        #[test]
        fn test_empty_candidate_canons_short_circuits_with_zero_model_calls_and_no_config_needed() {
            let caseload = Caseload::new()
                .with_intake(CaseloadIntake {
                    intent: Some("Update docs".to_string()),
                    target_paths: vec!["docs/faq.md".to_string()],
                    ..CaseloadIntake::default()
                })
                .with_discovery(CaseloadDiscovery {
                    candidate_canons: Vec::new(),
                    ..CaseloadDiscovery::default()
                });
            let provider = RecordingMockProvider::with_error(ProviderError::MissingApiKey);

            let enriched = execute_apprise(
                &AppriseOptions::default(),
                caseload,
                &WorkspaceSnapshot::new(),
                &provider,
            )
            .expect("zero candidate canons must short-circuit cleanly");

            assert!(provider.recorded_requests().is_empty());
            assert_eq!(
                enriched.apprisal,
                Some(CaseloadApprisal {
                    assessments: BTreeMap::new()
                })
            );
        }

        #[test]
        fn test_rejects_missing_intent_discovery_config_validation_failure_and_bad_threshold() {
            let snapshot = fixture_snapshot();
            let provider = RecordingMockProvider::with_payload(json!({"assessments": []}));

            let bad_threshold_err = execute_apprise(
                &AppriseOptions {
                    threshold: 1.25,
                    ..AppriseOptions::default()
                },
                fixture_caseload(Vec::new()),
                &snapshot,
                &provider,
            )
            .expect_err("threshold > 1.0 must fail");
            assert!(matches!(
                bad_threshold_err,
                AppriseError::InvalidThreshold { .. }
            ));
            assert!(bad_threshold_err.to_string().contains("Hint:"));

            let no_intent_err = execute_apprise(
                &AppriseOptions::default(),
                Caseload::new().with_discovery(CaseloadDiscovery::default()),
                &snapshot,
                &provider,
            )
            .expect_err("missing intent must fail");
            assert_eq!(no_intent_err, AppriseError::MissingIntent);

            let no_disc_err = execute_apprise(
                &AppriseOptions::default(),
                Caseload::new().with_intake(CaseloadIntake {
                    intent: Some("Add cache".to_string()),
                    ..CaseloadIntake::default()
                }),
                &snapshot,
                &provider,
            )
            .expect_err("missing discovery must fail");
            assert_eq!(no_disc_err, AppriseError::MissingDiscovery);

            let invalid_val = Caseload::new()
                .with_intake(CaseloadIntake {
                    intent: Some("Add cache".to_string()),
                    ..CaseloadIntake::default()
                })
                .with_discovery(CaseloadDiscovery {
                    candidate_canons: vec![".canons/auth/cache-expiry.md".to_string()],
                    ..CaseloadDiscovery::default()
                })
                .with_validation(CaseloadValidation {
                    has_errors: true,
                    results: BTreeMap::new(),
                });
            let val_err = execute_apprise(
                &AppriseOptions::default(),
                invalid_val,
                &snapshot,
                &provider,
            )
            .expect_err("validation failure must abort before provider call");
            assert_eq!(val_err, AppriseError::ValidationFailed);

            let no_cfg = Caseload::new()
                .with_intake(CaseloadIntake {
                    intent: Some("Add cache".to_string()),
                    ..CaseloadIntake::default()
                })
                .with_discovery(CaseloadDiscovery {
                    candidate_canons: vec![".canons/auth/cache-expiry.md".to_string()],
                    ..CaseloadDiscovery::default()
                });
            let cfg_err = execute_apprise(&AppriseOptions::default(), no_cfg, &snapshot, &provider)
                .expect_err("missing config must fail when candidates exist");
            assert_eq!(cfg_err, AppriseError::MissingConfig);
        }
    }

    mod aggregate_screening_and_threshold_gating {
        use super::*;

        fn assert_domain_indirected_array_schema(response_schema: &Value) {
            let assessments_schema = &response_schema["properties"]["assessments"];
            assert_eq!(assessments_schema["type"], json!("array"));
            let item_schema = &assessments_schema["items"];
            assert_eq!(
                item_schema["propertyOrdering"],
                json!(["canon_path", "apprisal_summary", "apprisal_score"]),
                "schema must enforce domain-indirected, reason-first ordering"
            );
            assert_eq!(
                item_schema["required"],
                json!(["canon_path", "apprisal_summary", "apprisal_score"])
            );
            assert!(
                item_schema["properties"]["canon_path"]
                    .get("enum")
                    .is_none(),
                "canon_path schema must omit dynamic enum lists for O(1) scaling"
            );
        }

        #[test]
        fn test_single_turn_domain_indirected_reason_first_schema_and_deterministic_threshold_derivation()
         {
            let snapshot = fixture_snapshot();
            let candidates = vec![
                ".canons/auth/cache-expiry.md".to_string(),
                ".canons/auth/db-rollback.md".to_string(),
            ];
            let caseload = fixture_caseload(candidates);
            // Return items in reverse order to verify deterministic BTreeMap folding
            let provider = RecordingMockProvider::with_payload(json!({
                "assessments": [
                    {
                        "canon_path": ".canons/auth/db-rollback.md",
                        "apprisal_summary": "No SQL migrations or schema modifications are planned.",
                        "apprisal_score": 0.49
                    },
                    {
                        "canon_path": ".canons/auth/cache-expiry.md",
                        "apprisal_summary": "In-memory TTL cache directly requires explicit expiration rules.",
                        "apprisal_score": 0.91
                    }
                ]
            }));

            let result =
                execute_apprise(&AppriseOptions::default(), caseload, &snapshot, &provider)
                    .expect("apprise screening must succeed");

            let requests = provider.recorded_requests();
            assert_eq!(
                requests.len(),
                1,
                "must screen all candidates in a single turn"
            );
            let req = &requests[0];
            assert_eq!(req.model_specifier, DEFAULT_SCREENER_MODEL);
            assert_eq!(req.reasoning_budget, None);
            assert!(req.user_prompt.contains("Refactor auth provider dispatch"));
            assert!(req.user_prompt.contains(".canons/auth/cache-expiry.md"));
            assert!(req.user_prompt.contains(".canons/auth/db-rollback.md"));
            assert_domain_indirected_array_schema(&req.response_schema);

            let apprisal = result.apprisal.expect("apprisal populated");
            let keys: Vec<&String> = apprisal.assessments.keys().collect();
            assert_eq!(
                keys,
                vec![
                    ".canons/auth/cache-expiry.md",
                    ".canons/auth/db-rollback.md"
                ],
                "folded CaseloadApprisal.assessments must maintain deterministic lexicographical order"
            );
            assert_eq!(apprisal.applicable_count(), 1);
            assert_eq!(apprisal.dismissed_count(), 1);
            assert_eq!(
                apprisal.assessments[".canons/auth/cache-expiry.md"].apprisal_status,
                ApprisalStatus::Applicable
            );
            assert_eq!(
                apprisal.assessments[".canons/auth/db-rollback.md"].apprisal_status,
                ApprisalStatus::Dismissed
            );
        }

        #[test]
        fn test_constant_size_schema_scales_across_60_candidate_canons() {
            let mut snapshot = WorkspaceSnapshot::new().with_file("src/auth/provider.rs");
            let mut candidates = Vec::new();
            let mut wire_items = Vec::new();

            for idx in 0..60 {
                let canon_ref = format!(".canons/scale/canon-{idx:03}.md");
                let md = format!(
                    "---\ntriggers:\n  - \"src/**\"\n---\nScale test invariant number {idx} MUST hold.\n"
                );
                snapshot = snapshot
                    .with_canon_markdown(&md, &ParseCanonOptions::with_source_ref(&canon_ref));
                let score = if idx % 10 == 0 { 0.85 } else { 0.10 };
                wire_items.push(json!({
                    "canon_path": canon_ref,
                    "apprisal_summary": format!("Scale rationale {idx}."),
                    "apprisal_score": score
                }));
                candidates.push(canon_ref);
            }
            wire_items.reverse();

            let one_canon_provider = RecordingMockProvider::with_payload(json!({
                "assessments": [
                    {
                        "canon_path": candidates[0],
                        "apprisal_summary": "Single canon run.",
                        "apprisal_score": 0.85
                    }
                ]
            }));
            execute_apprise(
                &AppriseOptions::default(),
                fixture_caseload(vec![candidates[0].clone()]),
                &snapshot,
                &one_canon_provider,
            )
            .expect("1-canon execution");

            let sixty_canon_provider =
                RecordingMockProvider::with_payload(json!({ "assessments": wire_items }));
            let result = execute_apprise(
                &AppriseOptions::default(),
                fixture_caseload(candidates.clone()),
                &snapshot,
                &sixty_canon_provider,
            )
            .expect("60-canon execution");

            assert_eq!(
                one_canon_provider.recorded_requests()[0].response_schema,
                sixty_canon_provider.recorded_requests()[0].response_schema,
                "response_schema must remain O(1) constant-size independent of candidate canon count"
            );

            let apprisal = result.apprisal.expect("apprisal populated");
            assert_eq!(apprisal.assessments.len(), 60);
            assert_eq!(apprisal.applicable_count(), 6);
            assert_eq!(apprisal.dismissed_count(), 54);
        }

        #[test]
        fn test_custom_threshold_boundary_inclusivity() {
            let snapshot = fixture_snapshot();
            let candidates = vec![
                ".canons/auth/cache-expiry.md".to_string(),
                ".canons/auth/db-rollback.md".to_string(),
            ];
            let provider = RecordingMockProvider::with_payload(json!({
                "assessments": [
                    {
                        "canon_path": ".canons/auth/cache-expiry.md",
                        "apprisal_summary": "Exact threshold match.",
                        "apprisal_score": 0.75
                    },
                    {
                        "canon_path": ".canons/auth/db-rollback.md",
                        "apprisal_summary": "Below custom threshold.",
                        "apprisal_score": 0.74
                    }
                ]
            }));

            let result = execute_apprise(
                &AppriseOptions {
                    threshold: 0.75,
                    ..AppriseOptions::default()
                },
                fixture_caseload(candidates),
                &snapshot,
                &provider,
            )
            .expect("custom threshold execution");

            let apprisal = result.apprisal.expect("apprisal populated");
            assert_eq!(
                apprisal.assessments[".canons/auth/cache-expiry.md"].apprisal_status,
                ApprisalStatus::Applicable
            );
            assert_eq!(
                apprisal.assessments[".canons/auth/db-rollback.md"].apprisal_status,
                ApprisalStatus::Dismissed
            );
        }

        fn multi_realm_snapshot(
            local_ref: &str,
            builtin_ref: &str,
            pack_ref: &str,
        ) -> WorkspaceSnapshot {
            let local_md = "\
---
triggers:
  - \"src/**\"
exists:
  - \"README.md\"
inspect:
  - diff
  - pr_title
  - linked_issues?
references:
  - \"README.md\"
tags:
  - documentation
  - features
---
# README Must Reflect Developed Features

Feature changes MUST update `README.md` when introducing user-facing capabilities.
";
            let builtin_md = "\
---
triggers:
  - \"src/**\"
inspect:
  - diff
tags:
  - security
---
# No Hardcoded Secrets

Source files MUST NOT embed raw API keys or credentials.
";
            let pack_md = "\
---
triggers:
  - \"**/*.tsx\"
---
# Accessible Buttons

Interactive UI controls MUST provide accessible labels.
";

            WorkspaceSnapshot::new()
                .with_file("src/cli/apprise.rs")
                .with_file("README.md")
                .with_canon_markdown(local_md, &ParseCanonOptions::with_source_ref(local_ref))
                .with_canon_markdown(builtin_md, &ParseCanonOptions::with_source_ref(builtin_ref))
                .with_canon_markdown(pack_md, &ParseCanonOptions::with_source_ref(pack_ref))
        }

        fn assert_multi_realm_prompt_grounding(req: &GenerationRequest) {
            assert!(req.system_instruction.contains("0.75 - 0.95"));
            assert!(req.system_instruction.contains("0.05 - 0.25"));
            assert!(
                req.system_instruction
                    .contains("candidate canon identifier")
            );

            let prompt = &req.user_prompt;
            for expected in [
                "- Triggers: src/**",
                "- Exists: README.md",
                "- Inspect: diff, pr_title, linked_issues?",
                "- References: README.md",
                "- Tags: documentation, features",
                "- Triggered Exhibits: src/cli/apprise.rs",
                "- Exact `canon_path`: `.canons/docs/readme-must-reflect-developed-features.md`",
                "- Exact `canon_path`: `acme-corp:packages/ui/.canons/accessibility.md`",
                "- Exact `canon_path`: `builtin:security/no-hardcoded-secrets`",
                "## Required `canon_path` Identifiers (3)\n\
                 1. `.canons/docs/readme-must-reflect-developed-features.md`\n\
                 2. `acme-corp:packages/ui/.canons/accessibility.md`\n\
                 3. `builtin:security/no-hardcoded-secrets`\n",
            ] {
                assert!(
                    prompt.contains(expected),
                    "prompt missing `{expected}`:\n{prompt}"
                );
            }
            assert!(!prompt.contains("- Triggered Scope:"));
            assert!(!prompt.contains("- Scope:"));
        }

        #[test]
        fn test_prompt_renders_normalized_frontmatter_triggered_exhibits_and_realm_neutral_identifiers()
         {
            let local_ref = ".canons/docs/readme-must-reflect-developed-features.md";
            let builtin_ref = "builtin:security/no-hardcoded-secrets";
            let pack_ref = "acme-corp:packages/ui/.canons/accessibility.md";
            let snapshot = multi_realm_snapshot(local_ref, builtin_ref, pack_ref);

            let candidates = vec![
                local_ref.to_string(),
                pack_ref.to_string(),
                builtin_ref.to_string(),
            ];
            let mut triggers_join = BTreeMap::new();
            triggers_join.insert(
                local_ref.to_string(),
                vec!["src/cli/apprise.rs".to_string()],
            );
            triggers_join.insert(
                builtin_ref.to_string(),
                vec!["src/cli/apprise.rs".to_string()],
            );

            let caseload = Caseload::new()
                .with_intake(CaseloadIntake {
                    intent: Some("Add new CLI subcommand flag for apprise".to_string()),
                    target_paths: vec!["src/cli".to_string()],
                    ..CaseloadIntake::default()
                })
                .with_discovery(CaseloadDiscovery {
                    target_files: vec!["src/cli/apprise.rs".to_string()],
                    candidate_canons: candidates,
                    triggers_join,
                    ..CaseloadDiscovery::default()
                })
                .with_validation(CaseloadValidation::default())
                .with_config(CaseloadConfig::default());

            let provider = RecordingMockProvider::with_payload(json!({
                "assessments": [
                    {
                        "canon_path": local_ref,
                        "apprisal_summary": "New CLI subcommand flag requires updating README.md.",
                        "apprisal_score": 0.90
                    },
                    {
                        "canon_path": pack_ref,
                        "apprisal_summary": "No TSX UI components are touched.",
                        "apprisal_score": 0.10
                    },
                    {
                        "canon_path": builtin_ref,
                        "apprisal_summary": "CLI flag parsing touches credential flags.",
                        "apprisal_score": 0.80
                    }
                ]
            }));

            execute_apprise(&AppriseOptions::default(), caseload, &snapshot, &provider)
                .expect("execute apprise across multi-realm canons");

            assert_multi_realm_prompt_grounding(&provider.recorded_requests()[0]);
        }
    }

    mod malformed_and_failed_provider_responses {
        use super::*;

        #[test]
        fn test_propagates_provider_errors_and_rejects_incomplete_or_out_of_range_payloads() {
            let snapshot = fixture_snapshot();
            let candidates = vec![".canons/auth/cache-expiry.md".to_string()];

            let auth_fail_provider = RecordingMockProvider::with_error(ProviderError::HttpStatus {
                status_code: 401,
                model_specifier: DEFAULT_SCREENER_MODEL.to_string(),
                detail: "API key not valid".to_string(),
                diagnostic: "HTTP 401: Invalid API key.\n  Hint: Check GEMINI_API_KEY.".to_string(),
            });
            let err = execute_apprise(
                &AppriseOptions::default(),
                fixture_caseload(candidates.clone()),
                &snapshot,
                &auth_fail_provider,
            )
            .expect_err("HTTP 401 must fail");
            assert!(err.to_string().contains("HTTP 401"));

            let out_of_range_provider = RecordingMockProvider::with_payload(json!({
                "assessments": [
                    {
                        "canon_path": ".canons/auth/cache-expiry.md",
                        "apprisal_summary": "Score exceeds 1.0",
                        "apprisal_score": 1.4
                    }
                ]
            }));
            let score_err = execute_apprise(
                &AppriseOptions::default(),
                fixture_caseload(candidates),
                &snapshot,
                &out_of_range_provider,
            )
            .expect_err("score > 1.0 in model response must fail schema decode");
            assert!(matches!(
                score_err,
                AppriseError::Provider(ProviderError::SchemaDecode { .. })
            ));
        }

        #[test]
        fn test_rejects_non_array_missing_unknown_and_duplicate_canon_path_entries() {
            let snapshot = fixture_snapshot();
            let two_candidates = vec![
                ".canons/auth/cache-expiry.md".to_string(),
                ".canons/auth/db-rollback.md".to_string(),
            ];

            let bad_payloads = [
                (
                    "non-array assessments object",
                    json!({
                        "assessments": {
                            ".canons/auth/cache-expiry.md": {
                                "apprisal_summary": "Legacy object wire shape.",
                                "apprisal_score": 0.8
                            }
                        }
                    }),
                ),
                (
                    "missing second candidate canon",
                    json!({
                        "assessments": [
                            {
                                "canon_path": ".canons/auth/cache-expiry.md",
                                "apprisal_summary": "Only one returned.",
                                "apprisal_score": 0.8
                            }
                        ]
                    }),
                ),
                (
                    "unknown canon_path entry",
                    json!({
                        "assessments": [
                            {
                                "canon_path": ".canons/auth/cache-expiry.md",
                                "apprisal_summary": "Valid.",
                                "apprisal_score": 0.8
                            },
                            {
                                "canon_path": ".canons/unknown/hallucinated-canon.md",
                                "apprisal_summary": "Hallucinated identifier.",
                                "apprisal_score": 0.2
                            }
                        ]
                    }),
                ),
                (
                    "duplicate canon_path entry",
                    json!({
                        "assessments": [
                            {
                                "canon_path": ".canons/auth/cache-expiry.md",
                                "apprisal_summary": "First copy.",
                                "apprisal_score": 0.8
                            },
                            {
                                "canon_path": ".canons/auth/cache-expiry.md",
                                "apprisal_summary": "Duplicate copy.",
                                "apprisal_score": 0.8
                            },
                            {
                                "canon_path": ".canons/auth/db-rollback.md",
                                "apprisal_summary": "Valid second canon.",
                                "apprisal_score": 0.1
                            }
                        ]
                    }),
                ),
            ];

            for (case_label, payload) in bad_payloads {
                let provider = RecordingMockProvider::with_payload(payload);
                let err = execute_apprise(
                    &AppriseOptions::default(),
                    fixture_caseload(two_candidates.clone()),
                    &snapshot,
                    &provider,
                )
                .expect_err(case_label);
                assert!(
                    matches!(
                        err,
                        AppriseError::Provider(ProviderError::SchemaDecode { .. })
                    ),
                    "case `{case_label}` must return ProviderError::SchemaDecode, got: {err:?}"
                );
            }
        }
    }

    mod batching_and_concurrency {
        use super::*;
        use crate::telemetry::{
            PipelineStage, RecordingTelemetrySink, SteppingTelemetryClock, TelemetryEvent,
            TelemetrySession,
        };
        use std::time::Duration;

        struct OutOfOrderAppriseProvider {
            call_order: Mutex<Vec<String>>,
            fail_on_canon: Option<String>,
        }

        impl StructuredModelProvider for OutOfOrderAppriseProvider {
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

                let mut assessments = Vec::new();
                for canon_ref in batch_canons.into_iter().rev() {
                    let is_even = canon_ref
                        .chars()
                        .rfind(|c| c.is_ascii_digit())
                        .is_some_and(|d| (d as u8).is_multiple_of(2));
                    let score = if is_even { 0.85 } else { 0.15 };
                    assessments.push(json!({
                        "canon_path": canon_ref,
                        "apprisal_summary": format!("Assessed {canon_ref}."),
                        "apprisal_score": score
                    }));
                }

                Ok(GenerationResponse {
                    payload: json!({ "assessments": assessments }),
                    thoughts: vec!["Apprise batch thought chunk.".to_string()],
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
        fn test_rejects_zero_batch_size_and_zero_concurrency_with_hints() {
            let snapshot = fixture_snapshot();
            let provider = RecordingMockProvider::with_payload(json!({"assessments": []}));

            let zero_batch = execute_apprise(
                &AppriseOptions {
                    max_canons_per_call: Some(0),
                    ..AppriseOptions::default()
                },
                Caseload::new(),
                &snapshot,
                &provider,
            )
            .expect_err("zero batch size must fail");
            assert!(matches!(zero_batch, AppriseError::InvalidBatchSize { .. }));
            assert!(zero_batch.to_string().contains("Hint:"));

            let zero_conc = execute_apprise(
                &AppriseOptions {
                    max_concurrent_calls: Some(0),
                    ..AppriseOptions::default()
                },
                Caseload::new(),
                &snapshot,
                &provider,
            )
            .expect_err("zero concurrency must fail");
            assert!(matches!(zero_conc, AppriseError::InvalidConcurrency { .. }));
            assert!(zero_conc.to_string().contains("Hint:"));
        }

        #[test]
        fn test_out_of_order_concurrent_batches_fold_deterministically_and_aggregate_telemetry() {
            let mut snapshot = WorkspaceSnapshot::new().with_file("src/auth/provider.rs");
            let mut candidates = Vec::new();
            for i in 0..5 {
                let path = format!(".canons/pack/rule-{i:03}.md");
                snapshot = snapshot.with_canon_markdown(
                    &format!("# Rule {i}\n\nRule {i} MUST hold.\n"),
                    &ParseCanonOptions::with_source_ref(path.clone()),
                );
                candidates.push(path);
            }
            let caseload = fixture_caseload(candidates);

            // 1. Unbounded single-turn baseline
            let single_provider = OutOfOrderAppriseProvider {
                call_order: Mutex::new(Vec::new()),
                fail_on_canon: None,
            };
            let single_res = execute_apprise(
                &AppriseOptions::default(),
                caseload.clone(),
                &snapshot,
                &single_provider,
            )
            .expect("single-turn baseline");

            // 2. Batched (batch size 2 => 3 batches: [0,1], [2,3], [4]) with concurrency 3 + telemetry
            let batched_provider = OutOfOrderAppriseProvider {
                call_order: Mutex::new(Vec::new()),
                fail_on_canon: None,
            };
            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 10);
            let session = TelemetrySession::new(&sink, &clock);

            let batched_res = session
                .run_stage(PipelineStage::Apprise, |scope| {
                    let instrumented = scope.instrument_provider(&batched_provider);
                    execute_apprise(
                        &AppriseOptions {
                            max_canons_per_call: Some(2),
                            max_concurrent_calls: Some(3),
                            ..AppriseOptions::default()
                        },
                        caseload.clone(),
                        &snapshot,
                        &instrumented,
                    )
                })
                .expect("concurrent batched apprise");

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
                        stage: PipelineStage::Apprise,
                        usage,
                        ..
                    } => *usage,
                    _ => None,
                })
                .expect("Apprise StageFinish usage");
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
            let mut snapshot = WorkspaceSnapshot::new().with_file("src/auth/provider.rs");
            let mut candidates = Vec::new();
            for i in 0..4 {
                let path = format!(".canons/pack/rule-{i:03}.md");
                snapshot = snapshot.with_canon_markdown(
                    &format!("# Rule {i}\n\nRule {i} MUST hold.\n"),
                    &ParseCanonOptions::with_source_ref(path.clone()),
                );
                candidates.push(path);
            }
            let caseload = fixture_caseload(candidates);
            let failing_provider = OutOfOrderAppriseProvider {
                call_order: Mutex::new(Vec::new()),
                fail_on_canon: Some(".canons/pack/rule-002.md".to_string()),
            };

            let err = execute_apprise(
                &AppriseOptions {
                    max_canons_per_call: Some(2),
                    max_concurrent_calls: Some(2),
                    ..AppriseOptions::default()
                },
                caseload,
                &snapshot,
                &failing_provider,
            )
            .expect_err("failing second batch must fail execute_apprise");
            assert!(matches!(
                err,
                AppriseError::Provider(ProviderError::Timeout { .. })
            ));
        }
    }
}
