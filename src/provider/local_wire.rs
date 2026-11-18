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

//! Localhost `/v1/rerank` cross-encoder wire envelopes and calibrated `[0.0, 1.0]` score decoding.

use super::local_project::{
    build_compact_admit_query, build_compact_jurisdiction_query, extract_canon_invariant_doc,
    is_admit_screening_batch, is_canon_screening_batch, project_admit_question_doc,
    project_vet_compliance,
};
use crate::provider::{ProviderError, TokenUsage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const COMPLIANCE_SINGLE_QUESTION_KEY: &str = "compliant";
const SCORE_EPSILON: f64 = 1e-6;

/// Request payload for `POST /v1/rerank` against a local rank-head discriminator (`llama-server --reranking --pooling rank`, `vLLM`, `TEI`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LocalRerankRequest {
    pub(crate) model: String,
    pub(crate) query: String,
    pub(crate) documents: Vec<String>,
}

impl LocalRerankRequest {
    /// Maps a [`DecisionRequest`](crate::provider::DecisionRequest) into a batched `(query, documents)`
    /// cross-encoder payload alongside the deterministic `domain_key` ordering corresponding to `documents[index]`.
    ///
    /// - Single-question compliance vetting (`questions == {"compliant": rubric}`): places the compliance
    ///   rubric in `query` and `[state]` (governing canon + admitted exhibits) in `documents` so the
    ///   cross-encoder judges candidate compliance against the query requirement.
    /// - Multi-question or subject-keyed screening (`apprise`, `docket`, `admit`): places the shared
    ///   filing/exhibit `state` in `query` and the ordered `Noul` question prompts in `documents` so all
    ///   items evaluate in one batched forward pass.
    pub(crate) fn from_decision_request(
        model: impl Into<String>,
        state: &str,
        questions: &BTreeMap<String, String>,
    ) -> (Vec<String>, Self) {
        if questions.len() == 1
            && let Some((key, rubric)) = questions.iter().next()
            && key == COMPLIANCE_SINGLE_QUESTION_KEY
        {
            let (query, doc) = project_vet_compliance(state, rubric);
            return (
                vec![key.clone()],
                Self {
                    model: model.into(),
                    query,
                    documents: vec![doc],
                },
            );
        }

        let is_canon_batch = !questions.is_empty() && is_canon_screening_batch(questions.values());
        let is_admit_batch = !is_canon_batch
            && !questions.is_empty()
            && is_admit_screening_batch(questions.values());

        let query = if is_canon_batch {
            build_compact_jurisdiction_query(state)
        } else if is_admit_batch {
            build_compact_admit_query(state)
        } else {
            state.to_string()
        };

        let mut ordered_keys = Vec::with_capacity(questions.len());
        let mut documents = Vec::with_capacity(questions.len());
        for (domain_key, question_text) in questions {
            ordered_keys.push(domain_key.clone());
            let doc = if is_canon_batch {
                extract_canon_invariant_doc(question_text)
            } else if is_admit_batch {
                project_admit_question_doc(question_text)
            } else {
                question_text.clone()
            };
            documents.push(doc);
        }
        (
            ordered_keys,
            Self {
                model: model.into(),
                query,
                documents,
            },
        )
    }

    /// Constructs a minimal 1-document connectivity probe payload for `POST /v1/rerank`.
    pub(crate) fn new_probe(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            query: "Is the candidate document non-empty?".to_string(),
            documents: vec!["ping".to_string()],
        }
    }
}

/// Scored document item returned inside a `/v1/rerank` `results` array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct LocalRerankResultItem {
    pub(crate) index: usize,
    #[serde(alias = "score")]
    pub(crate) relevance_score: f64,
}

/// Token accounting block returned by `/v1/rerank`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct LocalRerankUsage {
    #[serde(default)]
    pub(crate) prompt_tokens: u64,
    #[serde(default)]
    pub(crate) total_tokens: Option<u64>,
}

/// Response envelope returned by `POST /v1/rerank`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct LocalRerankResponse {
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) results: Vec<LocalRerankResultItem>,
    #[serde(default)]
    pub(crate) usage: Option<LocalRerankUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LocalErrorField {
    Object { message: Option<String> },
    Text(String),
}

#[derive(Debug, Deserialize)]
struct LocalErrorEnvelope {
    error: Option<LocalErrorField>,
    message: Option<String>,
}

pub(crate) fn extract_local_error_detail(body: &str) -> String {
    if let Ok(env) = serde_json::from_str::<LocalErrorEnvelope>(body) {
        let msg = match env.error {
            Some(LocalErrorField::Object { message }) => message,
            Some(LocalErrorField::Text(text)) => Some(text),
            None => None,
        }
        .or(env.message)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

        if let Some(found) = msg {
            return found;
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "empty error response".to_string()
    } else {
        trimmed.to_string()
    }
}

fn calibrate_relevance_score(
    raw_score: f64,
    model_specifier: &str,
    domain_key: &str,
) -> Result<f64, ProviderError> {
    if !raw_score.is_finite() || !(-SCORE_EPSILON..=1.0 + SCORE_EPSILON).contains(&raw_score) {
        return Err(ProviderError::SchemaDecode {
            model_specifier: model_specifier.to_string(),
            message: format!(
                "local `/v1/rerank` returned non-calibrated score `{raw_score}` outside `[0.0, 1.0]` for `{domain_key}` \
                 (verify `--pooling rank` and `cls.output.weight` on the GGUF model)"
            ),
        });
    }
    Ok(raw_score.clamp(0.0, 1.0))
}

pub(crate) struct DecodedRerankBatch {
    pub(crate) scores: BTreeMap<String, f64>,
    pub(crate) resolved_model: Option<String>,
    pub(crate) usage: TokenUsage,
}

pub(crate) fn decode_rerank_batch(
    body: &str,
    model_specifier: &str,
    ordered_keys: &[String],
) -> Result<DecodedRerankBatch, ProviderError> {
    let parsed: LocalRerankResponse =
        serde_json::from_str(body).map_err(|err| ProviderError::MalformedResponse {
            model_specifier: model_specifier.to_string(),
            reason: format!("invalid local `/v1/rerank` JSON envelope ({err})"),
        })?;

    if parsed.results.len() != ordered_keys.len() {
        return Err(ProviderError::MalformedResponse {
            model_specifier: model_specifier.to_string(),
            reason: format!(
                "expected {} `/v1/rerank` result item(s), got {}",
                ordered_keys.len(),
                parsed.results.len()
            ),
        });
    }

    let mut scores = BTreeMap::new();
    for item in &parsed.results {
        let Some(domain_key) = ordered_keys.get(item.index) else {
            return Err(ProviderError::SchemaDecode {
                model_specifier: model_specifier.to_string(),
                message: format!(
                    "out-of-bounds `/v1/rerank` document index {} for batch of len {}",
                    item.index,
                    ordered_keys.len()
                ),
            });
        };
        let calibrated =
            calibrate_relevance_score(item.relevance_score, model_specifier, domain_key)?;
        if scores.insert(domain_key.clone(), calibrated).is_some() {
            return Err(ProviderError::SchemaDecode {
                model_specifier: model_specifier.to_string(),
                message: format!(
                    "duplicate `/v1/rerank` result index {} (`{domain_key}`)",
                    item.index
                ),
            });
        }
    }

    let resolved = parsed
        .model
        .as_deref()
        .map(normalize_local_model_label)
        .filter(|m| !m.is_empty());

    let usage = match parsed.usage {
        Some(u) => TokenUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: u.total_tokens.unwrap_or(u.prompt_tokens),
        },
        None => TokenUsage::default(),
    };

    Ok(DecodedRerankBatch {
        scores,
        resolved_model: resolved,
        usage,
    })
}

pub(crate) fn normalize_local_model_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if (trimmed.contains('/') || trimmed.contains('\\'))
        && trimmed.to_ascii_lowercase().ends_with(".gguf")
    {
        return trimmed
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(trimmed)
            .to_string();
    }
    trimmed.to_string()
}
