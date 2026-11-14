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

//! Discriminative `Noul` batch screening for `docket` (`typesafe:jev-*`).

use super::prompt::{append_canon_section, build_bounded_docket_filing_prefix};
use super::{DEFAULT_DOCKET_MAX_CONCURRENT_CALLS, DocketError, DocketOptions};
use crate::caseload::{
    CanonDocketAssessment, Caseload, CaseloadDocket, ColorabilityStatus,
    normalize_colorability_score,
};
use crate::discover::WorkspaceSnapshot;
use crate::provider::{
    DecisionRequest, DecisionResponse, MAX_DECISION_BATCH_CHARS, MAX_DECISION_NOULS_PER_CALL,
    MAX_DECISION_STATE_CHARS, ProviderError, StructuredModelProvider, partition_decision_items,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const DOCKET_NOUL_QUERY_SUFFIX: &str = "\
Question: Does this candidate canon have a colorable claim of subject-matter jurisdiction over \
the in-flight filing in `state`?\n\
- Answer YES when the filing touches, introduces, or modifies the domain construct, architectural \
layer, or workflow governed by this canon.\n\
- NEVER answer NO because the code is compliant: compliant implementations of a governed construct \
still fall under the canon's jurisdiction, and `feat` PRs fall under feature companion/documentation \
rules even when the companion file is omitted.\n\
- Answer NO ONLY when the canon's specific governed construct is absent from the filing (such as \
`fix`-only commit rules on a `feat` PR, or workflow/signal/parser rules when those constructs are untouched).";

pub(super) fn execute_docket_decisions(
    options: &DocketOptions,
    caseload: Caseload,
    threshold: f64,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
) -> Result<Caseload, DocketError> {
    let intake = caseload.intake.as_ref().ok_or(DocketError::MissingIntake)?;
    let discovery = caseload
        .discovery
        .as_ref()
        .ok_or(DocketError::MissingDiscovery)?;
    let config = caseload.config.as_ref().ok_or(DocketError::MissingConfig)?;

    let shared_state =
        build_bounded_docket_filing_prefix(intake, discovery, MAX_DECISION_STATE_CHARS);
    let mut rendered_items = Vec::with_capacity(discovery.candidate_canons.len());
    for canon_ref in &discovery.candidate_canons {
        let canon =
            workspace
                .canons
                .get(canon_ref)
                .ok_or_else(|| DocketError::MissingCandidateCanon {
                    canon_ref: canon_ref.clone(),
                })?;
        let matched = discovery
            .triggers_join
            .get(canon_ref)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut q_text = String::with_capacity(640);
        append_canon_section(&mut q_text, canon_ref, canon, matched);
        let _ = write!(q_text, "{DOCKET_NOUL_QUERY_SUFFIX}");
        rendered_items.push((canon_ref.clone(), q_text));
    }

    let max_per_call = options
        .max_canons_per_call
        .map_or(MAX_DECISION_NOULS_PER_CALL, |n| {
            n.min(MAX_DECISION_NOULS_PER_CALL)
        });
    let batches = partition_decision_items(
        &rendered_items,
        shared_state.len(),
        max_per_call,
        MAX_DECISION_BATCH_CHARS,
        |(_, q)| q.len(),
    );

    let mut prepared = Vec::with_capacity(batches.len());
    for batch_items in batches {
        let mut batch_canons = Vec::with_capacity(batch_items.len());
        let mut questions = BTreeMap::new();
        for (canon_ref, q_text) in batch_items {
            batch_canons.push(canon_ref.clone());
            questions.insert(canon_ref, q_text);
        }
        prepared.push((
            batch_canons,
            DecisionRequest {
                model_specifier: config.screener_model.clone(),
                state: shared_state.clone(),
                questions,
                timeout_ms: options.timeout_ms,
            },
        ));
    }

    let concurrency_limit = options
        .max_concurrent_calls
        .unwrap_or(DEFAULT_DOCKET_MAX_CONCURRENT_CALLS);
    let cases = run_decision_batches(
        &prepared,
        concurrency_limit,
        threshold,
        &config.screener_model,
        provider,
    )?;

    Ok(caseload.with_docket(CaseloadDocket::from_cases(cases)))
}

type BatchResult = Result<BTreeMap<String, CanonDocketAssessment>, DocketError>;

fn run_decision_batches(
    prepared: &[(Vec<String>, DecisionRequest)],
    concurrency_limit: usize,
    threshold: f64,
    model_specifier: &str,
    provider: &dyn StructuredModelProvider,
) -> BatchResult {
    let worker_count = concurrency_limit.min(prepared.len()).max(1);
    if worker_count == 1 {
        let mut combined = BTreeMap::new();
        for (batch_canons, request) in prepared {
            let response = provider.evaluate_decisions(request)?;
            let batch_cases =
                fold_decision_cases(&response, batch_canons, threshold, model_specifier)?;
            combined.extend(batch_cases);
        }
        return Ok(combined);
    }

    let next_index = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let outcomes: Mutex<Vec<Option<BatchResult>>> =
        Mutex::new((0..prepared.len()).map(|_| None).collect());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if aborted.load(Ordering::Relaxed) {
                        break;
                    }
                    let idx = next_index.fetch_add(1, Ordering::SeqCst);
                    if idx >= prepared.len() {
                        break;
                    }
                    let (batch_canons, request) = &prepared[idx];
                    let res = provider
                        .evaluate_decisions(request)
                        .map_err(DocketError::from)
                        .and_then(|resp| {
                            fold_decision_cases(&resp, batch_canons, threshold, model_specifier)
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
        combined.extend(slot?);
    }
    Ok(combined)
}

fn fold_decision_cases(
    response: &DecisionResponse,
    batch_canons: &[String],
    threshold: f64,
    model_specifier: &str,
) -> BatchResult {
    let mut cases = BTreeMap::new();
    for canon_ref in batch_canons {
        let raw_score = response.scores.get(canon_ref).copied().ok_or_else(|| {
            DocketError::Provider(ProviderError::SchemaDecode {
                model_specifier: model_specifier.to_string(),
                message: format!("missing decision score for candidate canon `{canon_ref}`"),
            })
        })?;
        let colorability_score = normalize_colorability_score(raw_score).map_err(|detail| {
            DocketError::Provider(ProviderError::SchemaDecode {
                model_specifier: model_specifier.to_string(),
                message: format!("invalid decision score for `{canon_ref}`: {detail}"),
            })
        })?;
        let colorability_status = ColorabilityStatus::from_score(colorability_score, threshold);
        let colorability_summary = format!(
            "Discriminative jurisdiction screen (`{}`: {:.3} -> {}).",
            response.resolved_model,
            colorability_score,
            colorability_status.as_str()
        );
        cases.insert(
            canon_ref.clone(),
            CanonDocketAssessment {
                colorability_summary,
                colorability_score,
                colorability_status,
            },
        );
    }
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::ParseCanonOptions;
    use crate::caseload::{
        CaseloadConfig, CaseloadDiscovery, CaseloadIntake, CaseloadValidation, DiscoveryMode,
    };
    use crate::configure::DEFAULT_JEV_MODEL;
    use crate::docket::execute_docket;
    use crate::provider::{GenerationRequest, GenerationResponse, TokenUsage};

    struct StubDecisionScreener {
        scores: BTreeMap<String, f64>,
    }

    impl StructuredModelProvider for StubDecisionScreener {
        fn generate_structured(
            &self,
            _request: &GenerationRequest,
        ) -> Result<GenerationResponse, ProviderError> {
            Err(ProviderError::UnsupportedProvider {
                provider: "typesafe".to_string(),
                model_specifier: DEFAULT_JEV_MODEL.to_string(),
            })
        }

        fn evaluate_decisions(
            &self,
            request: &DecisionRequest,
        ) -> Result<DecisionResponse, ProviderError> {
            let mut scores = BTreeMap::new();
            for key in request.questions.keys() {
                scores.insert(
                    key.clone(),
                    self.scores.get(key).copied().unwrap_or_default(),
                );
            }
            Ok(DecisionResponse {
                scores,
                resolved_model: "jev-1.14.0".to_string(),
                latency_ms: 95,
                usage: TokenUsage {
                    prompt_tokens: 150,
                    completion_tokens: 0,
                    reasoning_tokens: 0,
                    total_tokens: 150,
                },
            })
        }
    }

    #[test]
    fn test_execute_docket_decisions_screens_candidates_via_noul_probabilities() {
        let keep_ref = ".canons/cli/keep.md";
        let drop_ref = ".canons/cli/drop.md";
        let ws = WorkspaceSnapshot::new()
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\n---\nCLI flags MUST be kebab-case.\n",
                &ParseCanonOptions::with_source_ref(keep_ref),
            )
            .with_canon_markdown(
                "---\ntriggers:\n  - \"src/cli/**\"\n---\nDatabase migrations MUST include rollback.\n",
                &ParseCanonOptions::with_source_ref(drop_ref),
            );
        let caseload = Caseload::new()
            .with_intake(CaseloadIntake {
                target_paths: vec!["src/cli/mod.rs".to_string()],
                ..CaseloadIntake::default()
            })
            .with_discovery(CaseloadDiscovery {
                mode: DiscoveryMode::Triggered,
                candidate_canons: vec![drop_ref.to_string(), keep_ref.to_string()],
                ..CaseloadDiscovery::default()
            })
            .with_validation(CaseloadValidation::default())
            .with_config(CaseloadConfig {
                screener_model: DEFAULT_JEV_MODEL.to_string(),
                ..CaseloadConfig::default()
            });
        let provider = StubDecisionScreener {
            scores: BTreeMap::from([(keep_ref.to_string(), 0.92), (drop_ref.to_string(), 0.04)]),
        };

        let out = execute_docket(&DocketOptions::default(), caseload, &ws, &provider)
            .expect("docket decision screening");
        let docket = out.docket.expect("docket populated");
        assert_eq!(docket.active_docket, vec![keep_ref.to_string()]);
        assert_eq!(
            docket.cases[drop_ref].colorability_status,
            ColorabilityStatus::Dismissed
        );
    }
}
