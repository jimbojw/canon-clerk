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

//! Core domain execution engine for the `audit` Caseload DAG stage (`docs/architecture/nodes/audit.md`).

mod decision;
mod decode;
mod exhibits;
mod prompt;

pub use decision::{DEFAULT_AUDIT_FASTPASS_THRESHOLD, ENV_AUDIT_FASTPASS_THRESHOLD};
pub use exhibits::resolve_canon_reference_paths;

use crate::caseload::{
    CanonAdjudication, Caseload, CaseloadVerdict, TYPESAFE_PROVIDER, normalize_compliance_score,
};
use crate::configure::model_provider;
use crate::discover::WorkspaceSnapshot;
use crate::docket::ENV_MAX_CONCURRENT_CALLS;
use crate::provider::{
    DEFAULT_GENERATION_TIMEOUT_MS, GenerationRequest, ProviderError, StructuredModelProvider,
};
use decode::decode_case_adjudication;
use prompt::{AUDIT_SYSTEM_INSTRUCTION, build_audit_case_user_prompt, build_audit_schema};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Default substantive compliance threshold for `audit` (`docs/architecture/nodes/audit.md` §4.5).
pub const DEFAULT_AUDIT_THRESHOLD: f64 = 0.5;

/// Default maximum concurrent in-flight per-case trial calls during `audit`.
pub const DEFAULT_AUDIT_MAX_CONCURRENT_CALLS: usize = 4;

/// Default per-trial timeout ceiling in milliseconds (`180_000ms` / 3 minutes) accommodating extended auditor reasoning.
pub const DEFAULT_AUDIT_TIMEOUT_MS: u64 = 180_000;

/// Environment variable name for overriding `--audit-threshold` (`docs/architecture/nodes/audit.md` §5).
pub const ENV_AUDIT_THRESHOLD: &str = "CANON_CLERK_AUDIT_THRESHOLD";

/// Environment variable name for overriding `--audit-concurrency` (`docs/architecture/nodes/audit.md` §5).
pub const ENV_AUDIT_CONCURRENCY: &str = "CANON_CLERK_AUDIT_CONCURRENCY";

pub(super) const DEFAULT_GENERATION_TIMEOUT_FOR_FASTPASS_MS: u64 = DEFAULT_GENERATION_TIMEOUT_MS;

/// Domain input parameters for executing the `audit` pipeline stage.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditOptions {
    /// Substantive compliance score threshold in `[0.0, 1.0]` (defaults to [`DEFAULT_AUDIT_THRESHOLD`]).
    pub threshold: f64,
    /// Optional confidence threshold in `[0.0, 1.0]` required for discriminative happy-path fast-pass
    /// when `screener_model` uses a decision model (`None` defaults to `threshold.max(0.80)`).
    pub fastpass_threshold: Option<f64>,
    /// Per-request timeout ceiling in milliseconds (defaults to [`DEFAULT_AUDIT_TIMEOUT_MS`]).
    pub timeout_ms: u64,
    /// Optional maximum concurrent in-flight per-case trial calls (`>= 1`; defaults to [`DEFAULT_AUDIT_MAX_CONCURRENT_CALLS`]).
    pub max_concurrent_calls: Option<usize>,
}

impl Default for AuditOptions {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_AUDIT_THRESHOLD,
            fastpass_threshold: None,
            timeout_ms: DEFAULT_AUDIT_TIMEOUT_MS,
            max_concurrent_calls: None,
        }
    }
}

/// Domain errors produced during `audit` execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditError {
    /// Configured threshold is non-finite or outside `[0.0, 1.0]`.
    InvalidThreshold { detail: String },
    /// Configured `max_concurrent_calls` is zero (`0`).
    InvalidConcurrency { detail: String },
    /// Case-or-Controversy violation: `.intake` is missing from the incoming `Caseload`.
    MissingIntake,
    /// Prerequisite `.discovery` namespace is missing from the incoming `Caseload`.
    MissingDiscovery,
    /// Upstream `.validation` recorded one or more candidate canon lint errors.
    ValidationFailed,
    /// Direct prerequisite `.evidence` namespace is missing from the incoming `Caseload`.
    MissingEvidence,
    /// Prerequisite `.config` namespace is missing when active cases require trial adjudication.
    MissingConfig,
    /// An active canon listed in `.evidence.active_cases` is absent from the workspace snapshot.
    MissingActiveCanon { canon_ref: String },
    /// Structured model provider returned a transport, HTTP, block, or schema decoding error.
    Provider(ProviderError),
}

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidThreshold { detail } => write!(
                f,
                "Invalid audit threshold: {detail}.\n  \
                 Hint: Pass a finite threshold in `[0.0, 1.0]` via `--audit-threshold <FLOAT>` \
                 or `{ENV_AUDIT_THRESHOLD}` (default: `{DEFAULT_AUDIT_THRESHOLD}`)."
            ),
            Self::InvalidConcurrency { detail } => write!(
                f,
                "Invalid audit concurrency limit: {detail}.\n  \
                 Hint: Pass a positive integer (`>= 1`) via `--audit-concurrency <COUNT>` \
                 or `{ENV_AUDIT_CONCURRENCY}` (default: `{DEFAULT_AUDIT_MAX_CONCURRENT_CALLS}`)."
            ),
            Self::MissingIntake => write!(
                f,
                "Substantive adjudication (`audit`) requires an underlying filing in `.intake` (Case-or-Controversy invariant).\n  \
                 Hint: Pipe a diff (`--diff -`), specify target paths (`src/...`, `-`, `--all-targets`), \
                 or pass `--caseload <PATH|->` with `.intake` populated."
            ),
            Self::MissingDiscovery => write!(
                f,
                "Substantive adjudication (`audit`) requires a resolved `.discovery` namespace.\n  \
                 Hint: Run `canon-clerk discover` first or invoke `canon-clerk audit` with a filing source."
            ),
            Self::ValidationFailed => write!(
                f,
                "Cannot execute `audit` because one or more candidate canons failed static validation.\n  \
                 Hint: Run `canon-clerk validate` to inspect and repair malformed candidate canons."
            ),
            Self::MissingEvidence => write!(
                f,
                "Substantive adjudication (`audit`) requires a resolved `.evidence` namespace from `admit`.\n  \
                 Hint: Run `canon-clerk admit` first or invoke `canon-clerk audit` directly to execute screening upstream."
            ),
            Self::MissingConfig => write!(
                f,
                "Cannot execute `audit` trials without a resolved `.config` namespace on the Caseload.\n  \
                 Hint: Run `canon-clerk configure` first or invoke `canon-clerk audit` directly to resolve configuration JIT."
            ),
            Self::MissingActiveCanon { canon_ref } => write!(
                f,
                "Active case canon `{canon_ref}` referenced in `.evidence.active_cases` was not found in the workspace snapshot.\n  \
                 Hint: Re-run `canon-clerk admit` or `canon-clerk audit` against the current workspace."
            ),
            Self::Provider(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for AuditError {}

impl From<ProviderError> for AuditError {
    fn from(err: ProviderError) -> Self {
        Self::Provider(err)
    }
}

pub(super) struct PreparedTrialCall {
    pub(super) canon_ref: String,
    pub(super) canon_id: String,
    pub(super) admitted_file_paths: BTreeSet<String>,
    pub(super) request: GenerationRequest,
}

fn validate_audit_options(options: &AuditOptions) -> Result<(f64, f64), AuditError> {
    let threshold = normalize_compliance_score(options.threshold)
        .map_err(|detail| AuditError::InvalidThreshold { detail })?;
    let fastpass_threshold = match options.fastpass_threshold {
        Some(raw_fp) => normalize_compliance_score(raw_fp)
            .map_err(|detail| AuditError::InvalidThreshold { detail })?,
        None => threshold.max(DEFAULT_AUDIT_FASTPASS_THRESHOLD),
    };
    if options.max_concurrent_calls == Some(0) {
        return Err(AuditError::InvalidConcurrency {
            detail: format!(
                "max_concurrent_calls must be at least 1, got 0 (see `{ENV_AUDIT_CONCURRENCY}` / `{ENV_MAX_CONCURRENT_CALLS}`)"
            ),
        });
    }
    Ok((threshold, fastpass_threshold))
}

/// Executes the pure `audit` domain stage against an in-memory [`WorkspaceSnapshot`] and
/// hexagonal [`StructuredModelProvider`] (`docs/architecture/nodes/audit.md` §4).
pub fn execute_audit(
    options: &AuditOptions,
    caseload: Caseload,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
) -> Result<Caseload, AuditError> {
    let (threshold, fastpass_threshold) = validate_audit_options(options)?;

    if caseload.verdict.is_some() {
        return Ok(caseload);
    }

    let intake = caseload.intake.as_ref().ok_or(AuditError::MissingIntake)?;
    let discovery = caseload
        .discovery
        .as_ref()
        .ok_or(AuditError::MissingDiscovery)?;

    if caseload
        .validation
        .as_ref()
        .is_some_and(|val| val.has_errors)
    {
        return Err(AuditError::ValidationFailed);
    }

    let evidence = caseload
        .evidence
        .as_ref()
        .ok_or(AuditError::MissingEvidence)?;
    if evidence.active_cases.is_empty() {
        return Ok(caseload.with_verdict(CaseloadVerdict::default()));
    }

    let mut active_trials = Vec::new();
    for canon_ref in &evidence.active_cases {
        let canon =
            workspace
                .canons
                .get(canon_ref)
                .ok_or_else(|| AuditError::MissingActiveCanon {
                    canon_ref: canon_ref.clone(),
                })?;
        let admitted = evidence.admitted_exhibits_for(canon_ref);
        if !admitted.is_empty() {
            active_trials.push((canon_ref.clone(), canon, admitted));
        }
    }

    if active_trials.is_empty() {
        return Ok(caseload.with_verdict(CaseloadVerdict::default()));
    }

    let config = caseload.config.as_ref().ok_or(AuditError::MissingConfig)?;
    let response_schema = build_audit_schema();

    let mut prepared_calls = Vec::with_capacity(active_trials.len());
    for (canon_ref, canon, admitted_exhibits) in active_trials {
        let (user_prompt, admitted_file_paths) = build_audit_case_user_prompt(
            &canon_ref,
            canon,
            &admitted_exhibits,
            intake,
            discovery,
            workspace,
        );
        let request = GenerationRequest {
            model_specifier: config.auditor_model.clone(),
            system_instruction: AUDIT_SYSTEM_INSTRUCTION.to_string(),
            user_prompt,
            response_schema: response_schema.clone(),
            reasoning_budget: config.reasoning_budget,
            timeout_ms: options.timeout_ms,
        };
        prepared_calls.push(PreparedTrialCall {
            canon_ref,
            canon_id: canon.id.clone(),
            admitted_file_paths,
            request,
        });
    }

    let concurrency_limit = options
        .max_concurrent_calls
        .unwrap_or(DEFAULT_AUDIT_MAX_CONCURRENT_CALLS);
    let uses_decision_tier = model_provider(&config.screener_model) == TYPESAFE_PROVIDER
        || model_provider(&config.auditor_model) == TYPESAFE_PROVIDER;
    let adjudications = if uses_decision_tier {
        decision::execute_decision_assisted_trials(
            &prepared_calls,
            &decision::DecisionTrialParams {
                screener_model: &config.screener_model,
                auditor_model: &config.auditor_model,
                threshold,
                fastpass_threshold,
                concurrency_limit,
                trial_timeout_ms: options.timeout_ms,
            },
            provider,
        )?
    } else {
        execute_trial_calls(
            &prepared_calls,
            concurrency_limit,
            threshold,
            &config.auditor_model,
            provider,
        )?
    };

    Ok(caseload.with_verdict(CaseloadVerdict::from_adjudications(adjudications)))
}

type TrialResult = Result<CanonAdjudication, AuditError>;

fn execute_trial_calls(
    prepared_calls: &[PreparedTrialCall],
    concurrency_limit: usize,
    threshold: f64,
    model_specifier: &str,
    provider: &dyn StructuredModelProvider,
) -> Result<Vec<CanonAdjudication>, AuditError> {
    let worker_count = concurrency_limit.min(prepared_calls.len()).max(1);
    if worker_count == 1 {
        let mut results = Vec::with_capacity(prepared_calls.len());
        for call in prepared_calls {
            let response = provider.generate_structured(&call.request)?;
            let adjudication = decode_case_adjudication(
                &response.payload,
                &call.canon_ref,
                &call.canon_id,
                &call.admitted_file_paths,
                threshold,
                model_specifier,
            )?;
            results.push(adjudication);
        }
        return Ok(results);
    }

    let next_index = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let outcomes: Mutex<Vec<Option<TrialResult>>> =
        Mutex::new((0..prepared_calls.len()).map(|_| None).collect());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if aborted.load(Ordering::Relaxed) {
                        break;
                    }
                    let idx = next_index.fetch_add(1, Ordering::SeqCst);
                    if idx >= prepared_calls.len() {
                        break;
                    }
                    let call = &prepared_calls[idx];
                    let res = provider
                        .generate_structured(&call.request)
                        .map_err(Into::into)
                        .and_then(|response| {
                            decode_case_adjudication(
                                &response.payload,
                                &call.canon_ref,
                                &call.canon_id,
                                &call.admitted_file_paths,
                                threshold,
                                model_specifier,
                            )
                            .map_err(Into::into)
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
    let mut results = Vec::with_capacity(prepared_calls.len());
    for slot in collected.into_iter().flatten() {
        results.push(slot?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::ParseCanonOptions;
    use crate::caseload::{
        AdmissibilityStatus, AnnotationLevel, CanonDocketAssessment, CaseloadConfig,
        CaseloadDiscovery, CaseloadDocket, CaseloadEvidence, CaseloadIntake, CaseloadValidation,
        ColorabilityStatus, DEFAULT_AUDITOR_MODEL, DEFAULT_SCREENER_MODEL, DiscoveryMode,
        ExhibitAssessment, FileArtifact, FileChangeStatus, VerdictStatus,
    };
    use crate::provider::{GenerationResponse, TokenUsage};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    struct RoutingAuditProvider {
        requests: Mutex<Vec<GenerationRequest>>,
        routes: BTreeMap<String, Result<Value, ProviderError>>,
    }

    impl RoutingAuditProvider {
        fn new(routes: BTreeMap<String, Result<Value, ProviderError>>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                routes,
            }
        }

        fn empty() -> Self {
            Self::new(BTreeMap::new())
        }

        fn captured_requests(&self) -> Vec<GenerationRequest> {
            self.requests.lock().expect("lock requests").clone()
        }
    }

    impl StructuredModelProvider for RoutingAuditProvider {
        fn generate_structured(
            &self,
            request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            self.requests
                .lock()
                .expect("lock requests")
                .push(request.clone());
            for (needle, outcome) in &self.routes {
                if request.user_prompt.contains(needle) {
                    return match outcome {
                        Ok(payload) => Ok(GenerationResponse {
                            payload: payload.clone(),
                            thoughts: vec!["Step 1: Checking invariant...".to_string()],
                            usage: TokenUsage {
                                prompt_tokens: 410,
                                completion_tokens: 95,
                                reasoning_tokens: 220,
                                total_tokens: 725,
                            },
                            latency_ms: 340,
                            resolved_model: "gemini-2.5-pro".to_string(),
                        }),
                        Err(err) => Err(err.clone()),
                    };
                }
            }
            Err(ProviderError::SchemaDecode {
                model_specifier: request.model_specifier.clone(),
                message: "no mock route matched trial prompt".to_string(),
            })
        }
    }

    fn sample_workspace() -> WorkspaceSnapshot {
        WorkspaceSnapshot::new()
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ninspect:\n  - diff\n  - pr_title?\n---\n# CLI Flags Kebab Case\n\nCLI flags MUST use kebab-case naming.\n\n**Exception:** Legacy v0 aliases explicitly marked `hide = true`.\n",
                &ParseCanonOptions::with_source_ref(".canons/cli/cli-flags-kebab-case.md"),
            )
            .with_canon_markdown(
                "---\ntriggers:\n  - \"**/*\"\nreferences:\n  - \"README.md\"\ninspect:\n  - diff\n  - pr_title\n---\n# Feat PRs Must Update README\n\nPull requests introducing new user-facing features (`feat`) MUST update `README.md`.\n",
                &ParseCanonOptions::with_source_ref(".canons/readme/feat-updates-readme.md"),
            )
            .with_canon_markdown(
                "---\ntriggers:\n  - \"**/*\"\ninspect:\n  - pr_title\n---\n# Conventional Commit Title\n\nPR titles MUST use lowercase type prefixes.\n",
                &ParseCanonOptions::with_source_ref(".canons/git/pr-title-lowercase.md"),
            )
            .with_file_content("README.md", "# Canon Clerk\n\nExisting documentation.\n")
    }

    fn sample_intake_diffs() -> BTreeMap<String, FileArtifact> {
        let mut diffs = BTreeMap::new();
        diffs.insert(
            "src/cli/audit.rs".to_string(),
            FileArtifact {
                path: "src/cli/audit.rs".to_string(),
                status: FileChangeStatus::Added,
                previous_path: None,
                lines_added: 3,
                lines_deleted: 0,
                patch: Some(
                    "@@ -0,0 +1,3 @@\n+pub struct AuditArgs {\n+    #[arg(long = \"auditThreshold\")]\n+}\n"
                        .to_string(),
                ),
                patch_omission_reason: None,
                content: None,
                content_omission_reason: None,
            },
        );
        diffs.insert(
            "src/internal/excluded_helper.rs".to_string(),
            FileArtifact {
                path: "src/internal/excluded_helper.rs".to_string(),
                status: FileChangeStatus::Modified,
                previous_path: None,
                lines_added: 1,
                lines_deleted: 0,
                patch: Some("@@ -1,1 +1,2 @@\n+// SECRET_EXCLUDED_SENTINEL\n".to_string()),
                patch_omission_reason: None,
                content: None,
                content_omission_reason: None,
            },
        );
        diffs
    }

    fn sample_admitted_caseload() -> Caseload {
        let diffs = sample_intake_diffs();
        let mut exhibits = BTreeMap::new();
        exhibits.insert(
            ".canons/cli/cli-flags-kebab-case.md".to_string(),
            vec![
                ExhibitAssessment {
                    file_path: "src/cli/audit.rs".to_string(),
                    admissibility_summary: "Defines `--auditThreshold` flag.".to_string(),
                    admissibility_score: 0.95,
                    admissibility_status: AdmissibilityStatus::Admitted,
                },
                ExhibitAssessment {
                    file_path: "src/internal/excluded_helper.rs".to_string(),
                    admissibility_summary: "Unrelated internal helper.".to_string(),
                    admissibility_score: 0.08,
                    admissibility_status: AdmissibilityStatus::Excluded,
                },
            ],
        );
        exhibits.insert(
            ".canons/readme/feat-updates-readme.md".to_string(),
            vec![
                ExhibitAssessment {
                    file_path: "pr_title".to_string(),
                    admissibility_summary: "`feat(cli):` triggers README requirement.".to_string(),
                    admissibility_score: 0.92,
                    admissibility_status: AdmissibilityStatus::Admitted,
                },
                ExhibitAssessment {
                    file_path: "src/cli/audit.rs".to_string(),
                    admissibility_summary: "Adds new user-facing CLI command.".to_string(),
                    admissibility_score: 0.89,
                    admissibility_status: AdmissibilityStatus::Admitted,
                },
            ],
        );

        let mut docket_cases = BTreeMap::new();
        for c in [
            ".canons/cli/cli-flags-kebab-case.md",
            ".canons/readme/feat-updates-readme.md",
        ] {
            docket_cases.insert(
                c.to_string(),
                CanonDocketAssessment {
                    colorability_summary: "Docketed.".to_string(),
                    colorability_score: 0.9,
                    colorability_status: ColorabilityStatus::Docketed,
                },
            );
        }

        Caseload::new()
            .with_intake(CaseloadIntake {
                pr_title: Some("feat(cli): add audit subcommand".to_string()),
                diffs,
                ..CaseloadIntake::default()
            })
            .with_discovery(CaseloadDiscovery {
                mode: DiscoveryMode::Triggered,
                target_files: vec![
                    "src/cli/audit.rs".to_string(),
                    "src/internal/excluded_helper.rs".to_string(),
                ],
                candidate_canons: vec![
                    ".canons/cli/cli-flags-kebab-case.md".to_string(),
                    ".canons/readme/feat-updates-readme.md".to_string(),
                ],
                ..CaseloadDiscovery::default()
            })
            .with_validation(CaseloadValidation::default())
            .with_config(CaseloadConfig {
                screener_model: DEFAULT_SCREENER_MODEL.to_string(),
                auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
                reasoning_budget: Some(4096),
            })
            .with_docket(CaseloadDocket::from_cases(docket_cases))
            .with_evidence(CaseloadEvidence::from_exhibits(exhibits))
    }

    mod guards_and_short_circuits {
        use super::*;

        #[test]
        fn test_empty_active_cases_short_circuits_with_zero_calls_and_no_config_needed() {
            let ws = sample_workspace();
            let provider = RoutingAuditProvider::empty();
            let mut caseload = sample_admitted_caseload();
            caseload.config = None;
            caseload.evidence = Some(CaseloadEvidence::default());

            let out = execute_audit(&AuditOptions::default(), caseload, &ws, &provider)
                .expect("empty active_cases must short-circuit cleanly");
            assert!(provider.captured_requests().is_empty());
            let verdict = out.verdict.expect("verdict populated");
            assert!(verdict.is_pass());
            assert!(verdict.adjudications.is_empty());
        }

        #[test]
        fn test_enforces_prerequisites_and_fast_forwards_populated_verdict() {
            let ws = sample_workspace();
            let provider = RoutingAuditProvider::empty();

            assert!(matches!(
                execute_audit(
                    &AuditOptions {
                        threshold: -0.1,
                        ..AuditOptions::default()
                    },
                    Caseload::new(),
                    &ws,
                    &provider
                )
                .unwrap_err(),
                AuditError::InvalidThreshold { .. }
            ));
            assert!(matches!(
                execute_audit(
                    &AuditOptions {
                        max_concurrent_calls: Some(0),
                        ..AuditOptions::default()
                    },
                    Caseload::new(),
                    &ws,
                    &provider
                )
                .unwrap_err(),
                AuditError::InvalidConcurrency { .. }
            ));
            assert_eq!(
                execute_audit(&AuditOptions::default(), Caseload::new(), &ws, &provider)
                    .unwrap_err(),
                AuditError::MissingIntake
            );

            let pre_populated = Caseload::new()
                .with_intake(CaseloadIntake::default())
                .with_verdict(CaseloadVerdict::default());
            let ff = execute_audit(
                &AuditOptions::default(),
                pre_populated.clone(),
                &ws,
                &provider,
            )
            .expect("fast-forward");
            assert_eq!(ff, pre_populated);
            assert!(provider.captured_requests().is_empty());
        }
    }

    mod trial_isolation_references_and_annotation_guardrails {
        use super::*;

        fn two_violation_routes() -> BTreeMap<String, Result<Value, ProviderError>> {
            let mut routes = BTreeMap::new();
            routes.insert(
                ".canons/cli/cli-flags-kebab-case.md".to_string(),
                Ok(json!({
                    "compliance_summary": "Flag `--auditThreshold` uses camelCase instead of kebab-case `--audit-threshold`.",
                    "compliance_score": 0.08,
                    "annotations": [{
                        "path": "src/cli/audit.rs",
                        "start_line": 2,
                        "end_line": 2,
                        "start_column": 20,
                        "end_column": 12,
                        "annotation_level": "failure",
                        "message": "Rename `--auditThreshold` to `--audit-threshold`."
                    }]
                })),
            );
            routes.insert(
                ".canons/readme/feat-updates-readme.md".to_string(),
                Ok(json!({
                    "compliance_summary": "`feat(cli):` adds a user-facing subcommand without updating `README.md`.",
                    "compliance_score": 0.10,
                    "annotations": [{
                        "path": "README.md",
                        "start_line": 1,
                        "end_line": 1,
                        "annotation_level": "failure",
                        "title": "Missing README Update",
                        "message": "Document `canon-clerk audit` in `README.md`."
                    }]
                })),
            );
            routes
        }

        #[test]
        fn test_concurrent_trials_enforce_zero_leakage_and_inject_references() {
            let ws = sample_workspace();
            let provider = RoutingAuditProvider::new(two_violation_routes());
            let _ = execute_audit(
                &AuditOptions {
                    max_concurrent_calls: Some(2),
                    ..AuditOptions::default()
                },
                sample_admitted_caseload(),
                &ws,
                &provider,
            )
            .expect("concurrent audit trials");

            let requests = provider.captured_requests();
            assert_eq!(requests.len(), 2);
            for req in &requests {
                assert_eq!(req.model_specifier, DEFAULT_AUDITOR_MODEL);
                assert_eq!(req.reasoning_budget, Some(4096));
                assert!(!req.user_prompt.contains("SECRET_EXCLUDED_SENTINEL"));
                assert!(req.user_prompt.contains("+   2 |"));
            }
            assert!(
                requests
                    .iter()
                    .any(|r| r.user_prompt.contains("Existing documentation."))
            );
        }

        #[test]
        fn test_decoder_reanchors_omission_paths_and_normalizes_columns() {
            let ws = sample_workspace();
            let provider = RoutingAuditProvider::new(two_violation_routes());
            let enriched = execute_audit(
                &AuditOptions::default(),
                sample_admitted_caseload(),
                &ws,
                &provider,
            )
            .expect("audit trials");

            let verdict = enriched.verdict.expect("verdict populated");
            assert_eq!(verdict.status, VerdictStatus::Fail);
            assert_eq!(verdict.failed_count(), 2);

            let cli_adj = &verdict.adjudications[0];
            assert_eq!(cli_adj.annotations[0].path, "src/cli/audit.rs");
            assert_eq!(cli_adj.annotations[0].start_column, Some(20));
            assert_eq!(cli_adj.annotations[0].end_column, Some(20));
            assert_eq!(
                cli_adj.annotations[0].title.as_deref(),
                Some("cli-flags-kebab-case")
            );

            let readme_adj = &verdict.adjudications[1];
            assert_eq!(readme_adj.annotations[0].path, "src/cli/audit.rs");
            assert_eq!(readme_adj.annotations[0].start_line, 1);
            assert_eq!(
                readme_adj.annotations[0].annotation_level,
                AnnotationLevel::Failure
            );
        }

        #[test]
        fn test_exception_pass_and_metadata_only_violation_emit_empty_annotations() {
            let ws = sample_workspace();
            let mut exhibits = BTreeMap::new();
            exhibits.insert(
                ".canons/cli/cli-flags-kebab-case.md".to_string(),
                vec![ExhibitAssessment {
                    file_path: "src/cli/audit.rs".to_string(),
                    admissibility_summary: "Legacy hidden alias.".to_string(),
                    admissibility_score: 0.90,
                    admissibility_status: AdmissibilityStatus::Admitted,
                }],
            );
            exhibits.insert(
                ".canons/git/pr-title-lowercase.md".to_string(),
                vec![ExhibitAssessment {
                    file_path: "pr_title".to_string(),
                    admissibility_summary: "PR title prefix.".to_string(),
                    admissibility_score: 0.95,
                    admissibility_status: AdmissibilityStatus::Admitted,
                }],
            );

            let mut caseload = sample_admitted_caseload();
            caseload.evidence = Some(CaseloadEvidence::from_exhibits(exhibits));

            let mut routes = BTreeMap::new();
            routes.insert(
                ".canons/cli/cli-flags-kebab-case.md".to_string(),
                Ok(json!({
                    "compliance_summary": "Satisfies declared Exception: legacy alias is marked `hide = true`.",
                    "compliance_score": 0.88,
                    "annotations": []
                })),
            );
            routes.insert(
                ".canons/git/pr-title-lowercase.md".to_string(),
                Ok(json!({
                    "compliance_summary": "PR title `FEAT: ...` uses uppercase prefix; change to `feat: ...`.",
                    "compliance_score": 0.05,
                    "annotations": [{
                        "path": "pr_title",
                        "start_line": 1,
                        "end_line": 1,
                        "annotation_level": "failure",
                        "message": "Lowercase `FEAT` to `feat`."
                    }]
                })),
            );

            let provider = RoutingAuditProvider::new(routes);
            let out = execute_audit(&AuditOptions::default(), caseload, &ws, &provider)
                .expect("exception pass + metadata fail");
            let verdict = out.verdict.expect("verdict");
            assert_eq!(verdict.status, VerdictStatus::Fail);
            assert_eq!(verdict.passed_count(), 1);
            assert_eq!(verdict.failed_count(), 1);
            assert!(verdict.adjudications[0].annotations.is_empty());
            assert!(
                verdict.adjudications[1].annotations.is_empty(),
                "metadata-only violations must strip non-file `pr_title` annotations"
            );
        }
    }
}
