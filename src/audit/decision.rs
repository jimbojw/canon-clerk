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

//! Happy-path discriminative `Noul` fast-pass & System-2 escalation for `audit` (`typesafe:jev-*`).

use super::decode::decode_case_adjudication;
use super::{AuditError, DEFAULT_GENERATION_TIMEOUT_FOR_FASTPASS_MS, PreparedTrialCall};
use crate::caseload::{
    CanonAdjudication, TYPESAFE_PROVIDER, VerdictStatus, normalize_compliance_score,
};
use crate::configure::model_provider;
use crate::provider::{
    DecisionRequest, MAX_DECISION_BATCH_CHARS, ProviderError, StructuredModelProvider,
};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Default confidence threshold in `[0.0, 1.0]` required to fast-pass a case on the happy path
/// via discriminative compliance screening without escalating to the System-2 auditor model.
pub const DEFAULT_AUDIT_FASTPASS_THRESHOLD: f64 = 0.80;

/// Environment variable name for overriding the happy-path fast-pass threshold in `audit`.
pub const ENV_AUDIT_FASTPASS_THRESHOLD: &str = "CANON_CLERK_AUDIT_FASTPASS_THRESHOLD";

const COMPLIANCE_QUESTION_KEY: &str = "compliant";

const AUDIT_FASTPASS_NOUL_INSTRUCTION: &str = "\
Evaluate the Admitted Exhibits (and any Statutory Grounding References) in `state` strictly \
against the Governing Canon Statute's Normative Invariant (`What`) and Declared `Exception` clauses:\n\
- `Rationale` (`Why`) and `Remediation` (`How`) are non-normative; NEVER fail code that satisfies \
  the Normative Invariant via a valid alternative mechanism.\n\
- Do NOT invent extra requirements beyond the written Normative Invariant or speculate about un-admitted files.\n\
- If a prima facie violation satisfies the verifiable conditions of a declared `Exception`, treat the case as compliant.\n\n\
Question: Do the Admitted Exhibits comply with the Governing Canon Statute with zero unexcused violations?\n\
- Answer YES only if the Admitted Exhibits comply (or satisfy a declared `Exception`) and require zero violation annotations.\n\
- Answer NO if any unexcused statute violation or missing required companion update is present.";

pub(super) struct DecisionTrialParams<'a> {
    pub(super) screener_model: &'a str,
    pub(super) auditor_model: &'a str,
    pub(super) threshold: f64,
    pub(super) fastpass_threshold: f64,
    pub(super) concurrency_limit: usize,
    pub(super) trial_timeout_ms: u64,
}

pub(super) fn execute_decision_assisted_trials(
    prepared_calls: &[PreparedTrialCall],
    params: &DecisionTrialParams<'_>,
    provider: &dyn StructuredModelProvider,
) -> Result<Vec<CanonAdjudication>, AuditError> {
    let pure_discriminative = model_provider(params.auditor_model) == TYPESAFE_PROVIDER;
    let decision_model = if pure_discriminative {
        params.auditor_model
    } else {
        params.screener_model
    };
    let fastpass_timeout_ms = params
        .trial_timeout_ms
        .min(DEFAULT_GENERATION_TIMEOUT_FOR_FASTPASS_MS);

    let worker_count = params.concurrency_limit.min(prepared_calls.len()).max(1);
    if worker_count == 1 {
        let mut results = Vec::with_capacity(prepared_calls.len());
        for call in prepared_calls {
            let adj = evaluate_single_case(
                call,
                decision_model,
                pure_discriminative,
                fastpass_timeout_ms,
                params,
                provider,
            )?;
            results.push(adj);
        }
        return Ok(results);
    }

    let next_index = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let outcomes: Mutex<Vec<Option<Result<CanonAdjudication, AuditError>>>> =
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
                    let res = evaluate_single_case(
                        call,
                        decision_model,
                        pure_discriminative,
                        fastpass_timeout_ms,
                        params,
                        provider,
                    );
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

fn evaluate_single_case(
    call: &PreparedTrialCall,
    decision_model: &str,
    pure_discriminative: bool,
    fastpass_timeout_ms: u64,
    params: &DecisionTrialParams<'_>,
    provider: &dyn StructuredModelProvider,
) -> Result<CanonAdjudication, AuditError> {
    // In two-tier hybrid mode, cases whose admitted exhibits + references exceed the discriminative
    // context window bypass the Jev fast-pass and escalate directly to the long-context generative auditor.
    if !pure_discriminative
        && call.request.user_prompt.len() + AUDIT_FASTPASS_NOUL_INSTRUCTION.len() + 64
            > MAX_DECISION_BATCH_CHARS
    {
        return escalate_to_generative_auditor(call, params, provider);
    }

    let mut questions = BTreeMap::new();
    questions.insert(
        COMPLIANCE_QUESTION_KEY.to_string(),
        AUDIT_FASTPASS_NOUL_INSTRUCTION.to_string(),
    );
    let decision_req = DecisionRequest {
        model_specifier: decision_model.to_string(),
        state: call.request.user_prompt.clone(),
        questions,
        timeout_ms: fastpass_timeout_ms,
    };

    let decision_resp = match provider.evaluate_decisions(&decision_req) {
        Ok(resp) => resp,
        Err(ProviderError::HttpStatus {
            status_code: 400,
            ref detail,
            ..
        }) if !pure_discriminative && detail.contains("max_tokens_exceeded") => {
            return escalate_to_generative_auditor(call, params, provider);
        }
        Err(err) => return Err(AuditError::Provider(err)),
    };
    let raw_score = decision_resp
        .scores
        .get(COMPLIANCE_QUESTION_KEY)
        .copied()
        .ok_or_else(|| {
            AuditError::Provider(ProviderError::SchemaDecode {
                model_specifier: decision_model.to_string(),
                message: format!(
                    "missing compliance `noul` score for active case `{}`",
                    call.canon_ref
                ),
            })
        })?;
    let compliance_score = normalize_compliance_score(raw_score).map_err(|detail| {
        AuditError::Provider(ProviderError::SchemaDecode {
            model_specifier: decision_model.to_string(),
            message: format!(
                "invalid compliance `noul` score for `{}`: {detail}",
                call.canon_ref
            ),
        })
    })?;

    if pure_discriminative {
        let status = VerdictStatus::from_score(compliance_score, params.threshold);
        let summary = match status {
            VerdictStatus::Pass => format!(
                "Compliant via discriminative audit (`{}`: {:.3}).",
                decision_resp.resolved_model, compliance_score
            ),
            VerdictStatus::Fail => format!(
                "Non-compliant via discriminative audit (`{}`: {:.3} < {:.2}); run with a generative `--auditor-model` (e.g. `pro`) for line annotations and remediation guidance.",
                decision_resp.resolved_model, compliance_score, params.threshold
            ),
        };
        return Ok(CanonAdjudication {
            canon_path: call.canon_ref.clone(),
            compliance_summary: summary,
            compliance_score,
            status,
            annotations: Vec::new(),
        });
    }

    // Two-tier hybrid mode: fast-pass happy-path compliant cases; escalate < fastpass_threshold to Tier-2 auditor!
    if compliance_score >= params.fastpass_threshold {
        return Ok(CanonAdjudication {
            canon_path: call.canon_ref.clone(),
            compliance_summary: format!(
                "Happy-path compliant via discriminative fast-pass screen (`{}`: {:.3} >= {:.2}).",
                decision_resp.resolved_model, compliance_score, params.fastpass_threshold
            ),
            compliance_score,
            status: VerdictStatus::Pass,
            annotations: Vec::new(),
        });
    }

    escalate_to_generative_auditor(call, params, provider)
}

fn escalate_to_generative_auditor(
    call: &PreparedTrialCall,
    params: &DecisionTrialParams<'_>,
    provider: &dyn StructuredModelProvider,
) -> Result<CanonAdjudication, AuditError> {
    let gen_response = provider.generate_structured(&call.request)?;
    Ok(decode_case_adjudication(
        &gen_response.payload,
        &call.canon_ref,
        &call.canon_id,
        &call.admitted_file_paths,
        params.threshold,
        params.auditor_model,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditOptions, execute_audit};
    use crate::canon::ParseCanonOptions;
    use crate::caseload::{
        AdmissibilityStatus, CanonDocketAssessment, Caseload, CaseloadConfig, CaseloadDiscovery,
        CaseloadDocket, CaseloadEvidence, CaseloadIntake, CaseloadValidation, ColorabilityStatus,
        DEFAULT_AUDITOR_MODEL, DiscoveryMode, ExhibitAssessment, FileArtifact, FileChangeStatus,
    };
    use crate::configure::DEFAULT_JEV_MODEL;
    use crate::discover::WorkspaceSnapshot;
    use crate::provider::{DecisionResponse, GenerationRequest, GenerationResponse, TokenUsage};
    use serde_json::json;

    struct TwoTierMockProvider {
        decision_scores: BTreeMap<String, f64>,
        structured_calls: Mutex<Vec<String>>,
    }

    impl StructuredModelProvider for TwoTierMockProvider {
        fn generate_structured(
            &self,
            request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            self.structured_calls
                .lock()
                .expect("lock")
                .push(request.user_prompt.clone());
            Ok(GenerationResponse {
                payload: json!({
                    "compliance_summary": "Escalated violation detected: missing env fallback.",
                    "compliance_score": 0.10,
                    "annotations": [{
                        "path": "src/cli/app.rs",
                        "start_line": 2,
                        "end_line": 2,
                        "annotation_level": "failure",
                        "message": "Add fallback env support."
                    }]
                }),
                resolved_model: "gemini-3.1-pro-preview".to_string(),
                latency_ms: 900,
                usage: TokenUsage {
                    prompt_tokens: 300,
                    completion_tokens: 50,
                    reasoning_tokens: 100,
                    total_tokens: 450,
                },
                thoughts: Vec::new(),
            })
        }

        fn evaluate_decisions(
            &self,
            request: &DecisionRequest,
        ) -> Result<DecisionResponse, ProviderError> {
            let score = self
                .decision_scores
                .iter()
                .find(|(canon_ref, _)| request.state.contains(canon_ref.as_str()))
                .map(|(_, s)| *s)
                .unwrap_or(0.95);
            Ok(DecisionResponse {
                scores: BTreeMap::from([(COMPLIANCE_QUESTION_KEY.to_string(), score)]),
                resolved_model: "jev-1.14.0".to_string(),
                latency_ms: 95,
                usage: TokenUsage {
                    prompt_tokens: 180,
                    completion_tokens: 0,
                    reasoning_tokens: 0,
                    total_tokens: 180,
                },
            })
        }
    }

    #[test]
    fn test_two_tier_audit_fast_passes_happy_path_and_escalates_violations_for_annotations() {
        let clean_ref = ".canons/cli/clean-rule.md";
        let fail_ref = ".canons/cli/violated-rule.md";
        let ws = WorkspaceSnapshot::new()
            .with_file_content("src/cli/app.rs", "fn main() {\n    let flag = true;\n}\n")
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ntags:\n  - cli\n---\nClean CLI rule.\n",
                &ParseCanonOptions::with_source_ref(clean_ref),
            )
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\ntags:\n  - cli\n---\nViolated CLI rule.\n",
                &ParseCanonOptions::with_source_ref(fail_ref),
            );

        let mut diffs = BTreeMap::new();
        diffs.insert(
            "src/cli/app.rs".to_string(),
            FileArtifact {
                path: "src/cli/app.rs".to_string(),
                previous_path: None,
                status: FileChangeStatus::Modified,
                lines_added: 1,
                lines_deleted: 0,
                patch: Some(
                    "@@ -1,2 +1,3 @@\n fn main() {\n+    let flag = true;\n }\n".to_string(),
                ),
                patch_omission_reason: None,
                content: None,
                content_omission_reason: None,
            },
        );
        let docket_cases = BTreeMap::from([
            (
                clean_ref.to_string(),
                CanonDocketAssessment {
                    colorability_summary: "ok".to_string(),
                    colorability_score: 0.9,
                    colorability_status: ColorabilityStatus::Docketed,
                },
            ),
            (
                fail_ref.to_string(),
                CanonDocketAssessment {
                    colorability_summary: "ok".to_string(),
                    colorability_score: 0.9,
                    colorability_status: ColorabilityStatus::Docketed,
                },
            ),
        ]);
        let admitted_item = ExhibitAssessment {
            file_path: "src/cli/app.rs".to_string(),
            admissibility_summary: "relevant".to_string(),
            admissibility_score: 0.92,
            admissibility_status: AdmissibilityStatus::Admitted,
        };
        let exhibits = BTreeMap::from([
            (clean_ref.to_string(), vec![admitted_item.clone()]),
            (fail_ref.to_string(), vec![admitted_item]),
        ]);

        let caseload = Caseload::new()
            .with_intake(CaseloadIntake {
                diffs,
                ..CaseloadIntake::default()
            })
            .with_discovery(CaseloadDiscovery {
                mode: DiscoveryMode::Triggered,
                candidate_canons: vec![clean_ref.to_string(), fail_ref.to_string()],
                ..CaseloadDiscovery::default()
            })
            .with_validation(CaseloadValidation::default())
            .with_config(CaseloadConfig {
                screener_model: DEFAULT_JEV_MODEL.to_string(),
                auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
                reasoning_budget: None,
            })
            .with_docket(CaseloadDocket::from_cases(docket_cases))
            .with_evidence(CaseloadEvidence::from_exhibits(exhibits));

        let provider = TwoTierMockProvider {
            decision_scores: BTreeMap::from([
                (clean_ref.to_string(), 0.94),
                (fail_ref.to_string(), 0.18),
            ]),
            structured_calls: Mutex::new(Vec::new()),
        };

        let result = execute_audit(&AuditOptions::default(), caseload, &ws, &provider)
            .expect("two-tier audit should succeed");
        let verdict = result.verdict.expect("verdict populated");
        assert_eq!(verdict.status, VerdictStatus::Fail);
        assert_eq!(verdict.adjudications.len(), 2);

        // Only 1 case escalated to Gemini structured generation!
        let escalated = provider.structured_calls.lock().expect("lock");
        assert_eq!(escalated.len(), 1);
        assert!(escalated[0].contains(fail_ref));

        assert_eq!(verdict.adjudications[0].canon_path, clean_ref);
        assert_eq!(verdict.adjudications[0].status, VerdictStatus::Pass);
        assert!(verdict.adjudications[0].annotations.is_empty());

        assert_eq!(verdict.adjudications[1].canon_path, fail_ref);
        assert_eq!(verdict.adjudications[1].status, VerdictStatus::Fail);
        assert_eq!(verdict.adjudications[1].annotations.len(), 1);
        assert_eq!(verdict.adjudications[1].annotations[0].start_line, 2);
    }
}
