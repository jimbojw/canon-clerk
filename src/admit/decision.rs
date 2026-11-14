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

//! Shared-state discriminative `Noul` exhibit screening for `admit` (`typesafe:jev-*`).

use super::exhibits::append_candidate_exhibit_section;
use super::prompt::append_filing_overview;
use super::{AdmitError, AdmitOptions, DEFAULT_ADMIT_MAX_CONCURRENT_CALLS};
use crate::canon::Canon;
use crate::caseload::{
    AdmissibilityStatus, Caseload, CaseloadDiscovery, CaseloadEvidence, CaseloadIntake,
    ExhibitAssessment, normalize_admissibility_score,
};
use crate::discover::WorkspaceSnapshot;
use crate::provider::{
    DecisionRequest, DecisionResponse, MAX_DECISION_BATCH_CHARS, MAX_DECISION_NOULS_PER_CALL,
    ProviderError, StructuredModelProvider,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone)]
struct PairSubject {
    composite_key: String,
    canon_ref: String,
    exhibit_id: String,
    question_text: String,
}

pub(super) fn execute_admit_decisions(
    options: &AdmitOptions,
    caseload: Caseload,
    threshold: f64,
    empty_cases: BTreeMap<String, Vec<ExhibitAssessment>>,
    non_empty_cases: Vec<(String, &Canon, Vec<String>)>,
    workspace: &WorkspaceSnapshot,
    provider: &dyn StructuredModelProvider,
) -> Result<Caseload, AdmitError> {
    let intake = caseload.intake.as_ref().ok_or(AdmitError::MissingIntake)?;
    let discovery = caseload
        .discovery
        .as_ref()
        .ok_or(AdmitError::MissingDiscovery)?;
    let config = caseload.config.as_ref().ok_or(AdmitError::MissingConfig)?;

    let mut unique_exhibits = BTreeSet::new();
    let mut all_pairs = Vec::new();
    for (canon_ref, canon, candidate_exhibits) in &non_empty_cases {
        for exhibit_id in candidate_exhibits {
            unique_exhibits.insert(exhibit_id.clone());
            let composite_key = format!("{canon_ref}::{exhibit_id}");
            let question_text = build_exhibit_noul_question(canon_ref, canon, exhibit_id);
            all_pairs.push(PairSubject {
                composite_key,
                canon_ref: canon_ref.clone(),
                exhibit_id: exhibit_id.clone(),
                question_text,
            });
        }
    }
    all_pairs.sort_by(|a, b| (&a.exhibit_id, &a.canon_ref).cmp(&(&b.exhibit_id, &b.canon_ref)));

    let prepared = pack_exhibit_grouped_batches(
        &all_pairs,
        &unique_exhibits,
        intake,
        discovery,
        workspace,
        &config.screener_model,
        options.timeout_ms,
    );

    let concurrency_limit = options
        .max_concurrent_calls
        .unwrap_or(DEFAULT_ADMIT_MAX_CONCURRENT_CALLS);
    let pair_assessments = run_pair_batches(
        &prepared,
        concurrency_limit,
        threshold,
        &config.screener_model,
        provider,
    )?;

    let mut all_exhibits: BTreeMap<String, Vec<ExhibitAssessment>> = empty_cases;
    for (canon_ref, item) in pair_assessments {
        all_exhibits.entry(canon_ref).or_default().push(item);
    }
    for items in all_exhibits.values_mut() {
        items.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    }

    Ok(caseload.with_evidence(CaseloadEvidence::from_exhibits(all_exhibits)))
}

fn pack_exhibit_grouped_batches(
    sorted_pairs: &[PairSubject],
    unique_exhibits: &BTreeSet<String>,
    intake: &CaseloadIntake,
    discovery: &CaseloadDiscovery,
    workspace: &WorkspaceSnapshot,
    screener_model: &str,
    timeout_ms: u64,
) -> Vec<(Vec<PairSubject>, DecisionRequest)> {
    if sorted_pairs.is_empty() {
        return Vec::new();
    }

    let mut filing_overview = String::with_capacity(1024);
    append_filing_overview(&mut filing_overview, intake, discovery);
    if filing_overview.len() > MAX_DECISION_BATCH_CHARS / 4
        && let Some((idx, _)) = filing_overview
            .char_indices()
            .nth(MAX_DECISION_BATCH_CHARS / 4)
    {
        filing_overview.truncate(idx);
        filing_overview.push_str("\n... [overview truncated]\n\n");
    }

    let mut rendered_exhibits = BTreeMap::new();
    for exhibit_id in unique_exhibits {
        let mut buf = String::with_capacity(512);
        append_candidate_exhibit_section(&mut buf, exhibit_id, intake, workspace);
        rendered_exhibits.insert(exhibit_id.clone(), buf);
    }

    let base_overhead = filing_overview.len() + 64;
    let mut prepared = Vec::new();
    let mut cur_pairs: Vec<PairSubject> = Vec::new();
    let mut cur_exhibits: BTreeSet<String> = BTreeSet::new();
    let mut cur_chars = base_overhead;

    for item in sorted_pairs {
        let ex_cost = if cur_exhibits.contains(&item.exhibit_id) {
            0
        } else {
            rendered_exhibits
                .get(&item.exhibit_id)
                .map_or(0, String::len)
        };
        let q_cost = item.question_text.len() + 32;
        if !cur_pairs.is_empty()
            && (cur_pairs.len() >= MAX_DECISION_NOULS_PER_CALL
                || cur_chars + ex_cost + q_cost > MAX_DECISION_BATCH_CHARS)
        {
            prepared.push(finalize_admit_batch(
                cur_pairs,
                &cur_exhibits,
                &filing_overview,
                &rendered_exhibits,
                screener_model,
                timeout_ms,
            ));
            cur_pairs = Vec::new();
            cur_exhibits = BTreeSet::new();
            cur_chars = base_overhead;
        }
        if cur_exhibits.insert(item.exhibit_id.clone()) {
            cur_chars += rendered_exhibits
                .get(&item.exhibit_id)
                .map_or(0, String::len);
        }
        cur_chars += q_cost;
        cur_pairs.push(item.clone());
    }

    if !cur_pairs.is_empty() {
        prepared.push(finalize_admit_batch(
            cur_pairs,
            &cur_exhibits,
            &filing_overview,
            &rendered_exhibits,
            screener_model,
            timeout_ms,
        ));
    }
    prepared
}

fn finalize_admit_batch(
    batch_pairs: Vec<PairSubject>,
    batch_exhibits: &BTreeSet<String>,
    filing_overview: &str,
    rendered_exhibits: &BTreeMap<String, String>,
    screener_model: &str,
    timeout_ms: u64,
) -> (Vec<PairSubject>, DecisionRequest) {
    let mut state = String::with_capacity(filing_overview.len() + batch_exhibits.len() * 512);
    state.push_str(filing_overview);
    let _ = writeln!(state, "## Candidate Exhibits ({})\n", batch_exhibits.len());
    for exhibit_id in batch_exhibits {
        if let Some(rendered) = rendered_exhibits.get(exhibit_id) {
            state.push_str(rendered);
        }
    }
    let mut questions = BTreeMap::new();
    for item in &batch_pairs {
        questions.insert(item.composite_key.clone(), item.question_text.clone());
    }
    (
        batch_pairs,
        DecisionRequest {
            model_specifier: screener_model.to_string(),
            state,
            questions,
            timeout_ms,
        },
    )
}

fn build_exhibit_noul_question(canon_ref: &str, canon: &Canon, exhibit_id: &str) -> String {
    format!(
        "Governing Canon: `{canon_ref}`\n\
         Title: {}\n\
         Normative Invariant: {}\n\
         Target Candidate Exhibit: `{exhibit_id}`\n\n\
         Question: Is Candidate Exhibit `{exhibit_id}` admissible as direct, relevant evidence for \
         adjudicating `{canon_ref}`?\n\
         - Answer YES if `{exhibit_id}` itself belongs to the governed stratum and defines, \
           implements, modifies, or directly evidences an instance of this canon's governed \
           construct (including compliant code, or exhibits evidencing a triggering change when \
           a required companion file is omitted).\n\
         - Answer NO if `{exhibit_id}` is prose Markdown documentation (`*.md`) on a source-code \
           canon, belongs to an orthogonal architectural layer, is an irrelevant PR metadata plane \
           on a self-contained source-code rule, or when a commit-type precondition (such as `fix:` \
           vs `feat:`) is unmet.",
        canon.title,
        canon.invariant.trim()
    )
}

type PairBatchResult = Result<Vec<(String, ExhibitAssessment)>, AdmitError>;

fn run_pair_batches(
    prepared: &[(Vec<PairSubject>, DecisionRequest)],
    concurrency_limit: usize,
    threshold: f64,
    model_specifier: &str,
    provider: &dyn StructuredModelProvider,
) -> PairBatchResult {
    let worker_count = concurrency_limit.min(prepared.len()).max(1);
    if worker_count == 1 {
        let mut combined = Vec::new();
        for (batch_pairs, request) in prepared {
            let response = provider.evaluate_decisions(request)?;
            let decoded = fold_pair_batch(&response, batch_pairs, threshold, model_specifier)?;
            combined.extend(decoded);
        }
        return Ok(combined);
    }

    let next_index = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let outcomes: Mutex<Vec<Option<PairBatchResult>>> =
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
                    let (batch_pairs, request) = &prepared[idx];
                    let res = provider
                        .evaluate_decisions(request)
                        .map_err(AdmitError::from)
                        .and_then(|resp| {
                            fold_pair_batch(&resp, batch_pairs, threshold, model_specifier)
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
    let mut combined = Vec::new();
    for slot in collected.into_iter().flatten() {
        combined.extend(slot?);
    }
    Ok(combined)
}

fn fold_pair_batch(
    response: &DecisionResponse,
    batch_pairs: &[PairSubject],
    threshold: f64,
    model_specifier: &str,
) -> PairBatchResult {
    let mut out = Vec::with_capacity(batch_pairs.len());
    for item in batch_pairs {
        let raw_score = response
            .scores
            .get(&item.composite_key)
            .copied()
            .ok_or_else(|| {
                AdmitError::Provider(ProviderError::SchemaDecode {
                    model_specifier: model_specifier.to_string(),
                    message: format!(
                        "missing decision score for exhibit `{}` on case `{}`",
                        item.exhibit_id, item.canon_ref
                    ),
                })
            })?;
        let admissibility_score = normalize_admissibility_score(raw_score).map_err(|detail| {
            AdmitError::Provider(ProviderError::SchemaDecode {
                model_specifier: model_specifier.to_string(),
                message: format!(
                    "invalid decision score for exhibit `{}` on `{}`: {detail}",
                    item.exhibit_id, item.canon_ref
                ),
            })
        })?;
        let admissibility_status = AdmissibilityStatus::from_score(admissibility_score, threshold);
        let admissibility_summary = format!(
            "Discriminative admissibility screen (`{}`: {:.3} -> {}).",
            response.resolved_model,
            admissibility_score,
            admissibility_status.as_str()
        );
        out.push((
            item.canon_ref.clone(),
            ExhibitAssessment {
                file_path: item.exhibit_id.clone(),
                admissibility_summary,
                admissibility_score,
                admissibility_status,
            },
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admit::execute_admit;
    use crate::canon::ParseCanonOptions;
    use crate::caseload::{
        CanonDocketAssessment, CaseloadConfig, CaseloadDocket, CaseloadValidation,
        ColorabilityStatus, DiscoveryMode,
    };
    use crate::configure::DEFAULT_JEV_MODEL;
    use crate::provider::{GenerationRequest, GenerationResponse, TokenUsage};

    struct StubAdmitScreener;

    impl StructuredModelProvider for StubAdmitScreener {
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
                let score = if key.ends_with("::src/cli/app.rs") {
                    0.95
                } else {
                    0.05
                };
                scores.insert(key.clone(), score);
            }
            Ok(DecisionResponse {
                scores,
                resolved_model: "jev-1.14.0".to_string(),
                latency_ms: 112,
                usage: TokenUsage::default(),
            })
        }
    }

    #[test]
    fn test_execute_admit_decisions_packs_pairs_over_shared_exhibit_state() {
        let canon_ref = ".canons/cli/rule.md";
        let ws = WorkspaceSnapshot::new()
            .with_file_content("src/cli/app.rs", "fn main() {}\n")
            .with_file_content("docs/guide.md", "# Guide\n")
            .with_canon_markdown(
                "---\ntriggers:\n  - \"**\"\n---\nCLI code MUST validate flags.\n",
                &ParseCanonOptions::with_source_ref(canon_ref),
            );
        let caseload = Caseload::new()
            .with_intake(CaseloadIntake {
                target_paths: vec!["docs/guide.md".to_string(), "src/cli/app.rs".to_string()],
                ..CaseloadIntake::default()
            })
            .with_discovery(CaseloadDiscovery {
                mode: DiscoveryMode::Triggered,
                candidate_canons: vec![canon_ref.to_string()],
                triggers_join: BTreeMap::from([(
                    canon_ref.to_string(),
                    vec!["docs/guide.md".to_string(), "src/cli/app.rs".to_string()],
                )]),
                ..CaseloadDiscovery::default()
            })
            .with_validation(CaseloadValidation::default())
            .with_config(CaseloadConfig {
                screener_model: DEFAULT_JEV_MODEL.to_string(),
                ..CaseloadConfig::default()
            })
            .with_docket(CaseloadDocket::from_cases(BTreeMap::from([(
                canon_ref.to_string(),
                CanonDocketAssessment {
                    colorability_summary: "ok".to_string(),
                    colorability_score: 0.9,
                    colorability_status: ColorabilityStatus::Docketed,
                },
            )])));

        let out = execute_admit(&AdmitOptions::default(), caseload, &ws, &StubAdmitScreener)
            .expect("admit decision screening");
        let evidence = out.evidence.expect("evidence populated");
        assert_eq!(evidence.active_cases, vec![canon_ref.to_string()]);
        let items = &evidence.exhibits[canon_ref];
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].file_path, "docs/guide.md");
        assert_eq!(items[0].admissibility_status, AdmissibilityStatus::Excluded);
        assert_eq!(items[1].file_path, "src/cli/app.rs");
        assert_eq!(items[1].admissibility_status, AdmissibilityStatus::Admitted);
    }
}
