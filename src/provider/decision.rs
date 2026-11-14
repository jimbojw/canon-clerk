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

//! Hexagonal outbound port and TypeSafe AI Jev adapter for discriminative `Noul` evaluations.

use super::typesafe::{
    DEFAULT_TYPESAFE_BASE_URL, TypesafeNoulQuestion, TypesafeSystemOneRequest,
    TypesafeSystemOneResponse, classify_typesafe_http_error, extract_typesafe_error_detail,
    post_typesafe_json, strip_typesafe_prefix,
};
use crate::caseload::{DEFAULT_PROVIDER, TYPESAFE_PROVIDER};
use crate::configure::model_provider;
use crate::provider::{
    GeminiModelProvider, GenerationRequest, GenerationResponse, ProviderError,
    StructuredModelProvider, TokenUsage,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum `Noul` questions packed into a single `/v1/systemone` forward pass by default.
pub(crate) const MAX_DECISION_NOULS_PER_CALL: usize = 48;

/// Maximum `state` character budget for macro discriminative screening (`~13.5k` code tokens).
pub(crate) const MAX_DECISION_STATE_CHARS: usize = 36_000;

/// Maximum combined `state` + `questions` character budget per `/v1/systemone` call (`~24k` code
/// tokens, staying safely below the upstream `32,768` input token ceiling on punctuation-dense diffs).
pub(crate) const MAX_DECISION_BATCH_CHARS: usize = 64_000;

const MIN_QUESTION_BUDGET_CHARS: usize = 4_096;
const WIRE_KEY_OVERHEAD_CHARS: usize = 32;

/// Partitions discriminative `Noul` items into deterministic contiguous batches bounded by both
/// `max_per_call` and `(max_batch_chars - state_chars)`.
pub(crate) fn partition_decision_items<T: Clone>(
    items: &[T],
    state_chars: usize,
    max_per_call: usize,
    max_batch_chars: usize,
    item_text_len: impl Fn(&T) -> usize,
) -> Vec<Vec<T>> {
    if items.is_empty() {
        return Vec::new();
    }
    let count_cap = max_per_call.max(1);
    let question_budget = max_batch_chars
        .saturating_sub(state_chars)
        .max(MIN_QUESTION_BUDGET_CHARS);
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_chars = 0usize;

    for item in items {
        let item_chars = item_text_len(item).saturating_add(WIRE_KEY_OVERHEAD_CHARS);
        if !current.is_empty()
            && (current.len() >= count_cap || current_chars + item_chars > question_budget)
        {
            batches.push(current);
            current = Vec::new();
            current_chars = 0;
        }
        current_chars += item_chars;
        current.push(item.clone());
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// Provider-neutral input specification for batched binary `Noul` probability evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRequest {
    /// Canonical provider-prefixed model identifier (e.g. `"typesafe:jev-latest"`).
    pub model_specifier: String,
    /// Shared input state evaluated in a single forward pass across all `questions`.
    pub state: String,
    /// Deterministic map from domain subject identifier to closed yes/no `Noul` instruction text.
    pub questions: BTreeMap<String, String>,
    /// Per-request timeout ceiling in milliseconds.
    pub timeout_ms: u64,
}

/// Normalized calibrated probability scores and operational telemetry from a decision model pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    /// Calibrated affirmative probabilities in `[0.0, 1.0]` keyed by domain subject identifier.
    pub scores: BTreeMap<String, f64>,
    /// Concrete model version identifier reported by the upstream provider endpoint.
    pub resolved_model: String,
    /// End-to-end request roundtrip latency in milliseconds.
    pub latency_ms: u64,
    /// Token accounting metrics for out-of-band telemetry reporting.
    pub usage: TokenUsage,
}

/// Hexagonal outbound port for discriminative decision models (`Noul` binary probability passes).
pub trait DecisionModelProvider: Send + Sync {
    /// Evaluates all `request.questions` in parallel against `request.state` and returns calibrated
    /// `[0.0, 1.0]` affirmative probabilities.
    fn evaluate_decisions(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, ProviderError>;
}

/// Live TypeSafe AI HTTPS adapter implementing [`DecisionModelProvider`] (`POST /v1/systemone`).
#[derive(Clone)]
pub struct TypesafeDecisionProvider {
    api_key: String,
    base_url: String,
}

impl fmt::Debug for TypesafeDecisionProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key_repr = if self.api_key.trim().is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        };
        f.debug_struct("TypesafeDecisionProvider")
            .field("api_key", &key_repr)
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl TypesafeDecisionProvider {
    /// Constructs a [`TypesafeDecisionProvider`] targeting [`DEFAULT_TYPESAFE_BASE_URL`].
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_TYPESAFE_BASE_URL.to_string(),
        }
    }

    /// Constructs a [`TypesafeDecisionProvider`] with a custom base URL (for loopback tests or proxies).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// Returns the configured TypeSafe AI v1 base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl DecisionModelProvider for TypesafeDecisionProvider {
    fn evaluate_decisions(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, ProviderError> {
        if request.timeout_ms == 0 {
            return Err(ProviderError::InvalidTimeout { timeout_ms: 0 });
        }

        let api_key = self.api_key.trim();
        if api_key.is_empty() {
            return Err(ProviderError::MissingTypesafeApiKey);
        }

        validate_typesafe_specifier(&request.model_specifier)?;
        if request.questions.is_empty() {
            return Ok(DecisionResponse {
                scores: BTreeMap::new(),
                resolved_model: request.model_specifier.clone(),
                latency_ms: 0,
                usage: TokenUsage::default(),
            });
        }

        let mut wire_questions = BTreeMap::new();
        let mut wire_to_domain = Vec::with_capacity(request.questions.len());
        for (idx, (domain_key, instructions)) in request.questions.iter().enumerate() {
            let wire_key = format!("q_{idx}");
            wire_questions.insert(wire_key.clone(), TypesafeNoulQuestion::new(instructions));
            wire_to_domain.push((wire_key, domain_key.clone()));
        }

        let payload = TypesafeSystemOneRequest {
            model: strip_typesafe_prefix(request.model_specifier.trim()).to_string(),
            state: request.state.clone(),
            questions: wire_questions,
        };

        let outcome = post_typesafe_json(
            &self.base_url,
            &request.model_specifier,
            api_key,
            request.timeout_ms,
            &payload,
        )
        .map_err(|(err, _latency)| err)?;

        if !(200..300).contains(&outcome.status_code) {
            let detail = extract_typesafe_error_detail(&outcome.body);
            let diagnostic = classify_typesafe_http_error(
                outcome.status_code,
                &request.model_specifier,
                &detail,
            );
            return Err(ProviderError::HttpStatus {
                status_code: outcome.status_code,
                model_specifier: request.model_specifier.clone(),
                detail,
                diagnostic,
            });
        }

        decode_systemone_response(
            &outcome.body,
            outcome.latency_ms,
            &request.model_specifier,
            &wire_to_domain,
        )
    }
}

fn validate_typesafe_specifier(raw: &str) -> Result<(), ProviderError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return Err(ProviderError::InvalidModelSpecifier {
            model_specifier: raw.to_string(),
            reason: "model specifier must be non-empty and contain no whitespace".to_string(),
        });
    }
    if let Some((prov, model_id)) = trimmed.split_once(':') {
        if prov.is_empty() || model_id.is_empty() || model_id.contains(':') {
            return Err(ProviderError::InvalidModelSpecifier {
                model_specifier: raw.to_string(),
                reason: "expected `typesafe:<model>` with non-empty segments".to_string(),
            });
        }
        if prov != TYPESAFE_PROVIDER {
            return Err(ProviderError::UnsupportedProvider {
                provider: prov.to_string(),
                model_specifier: trimmed.to_string(),
            });
        }
    }
    Ok(())
}

fn decode_systemone_response(
    body: &str,
    latency_ms: u64,
    model_specifier: &str,
    wire_to_domain: &[(String, String)],
) -> Result<DecisionResponse, ProviderError> {
    let parsed: TypesafeSystemOneResponse =
        serde_json::from_str(body).map_err(|err| ProviderError::MalformedResponse {
            model_specifier: model_specifier.to_string(),
            reason: format!("invalid TypeSafe `/v1/systemone` JSON envelope ({err})"),
        })?;

    let mut scores = BTreeMap::new();
    for (wire_key, domain_key) in wire_to_domain {
        let answer = parsed.answers.get(wire_key).ok_or_else(|| {
            ProviderError::SchemaDecode {
                model_specifier: model_specifier.to_string(),
                message: format!(
                    "missing `noul` answer `{wire_key}` for subject `{domain_key}` in `/v1/systemone` response"
                ),
            }
        })?;
        if !answer.noul.is_finite() || !(-1e-6..=1.0 + 1e-6).contains(&answer.noul) {
            return Err(ProviderError::SchemaDecode {
                model_specifier: model_specifier.to_string(),
                message: format!(
                    "out-of-range `noul` probability `{}` for subject `{domain_key}`",
                    answer.noul
                ),
            });
        }
        scores.insert(domain_key.clone(), answer.noul.clamp(0.0, 1.0));
    }

    let resolved_model = parsed
        .model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| model_specifier.to_string());

    let usage = match parsed.usage {
        Some(u) => TokenUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            reasoning_tokens: 0,
            total_tokens: u.input_tokens.saturating_add(u.output_tokens),
        },
        None => TokenUsage::default(),
    };

    Ok(DecisionResponse {
        scores,
        resolved_model,
        latency_ms,
        usage,
    })
}

/// Composite provider routing `google:*` structured generation turns to [`GeminiModelProvider`]
/// and `typesafe:*` discriminative `Noul` passes to [`TypesafeDecisionProvider`].
#[derive(Debug, Clone)]
pub struct MultiModelProvider {
    gemini: Option<GeminiModelProvider>,
    typesafe: Option<TypesafeDecisionProvider>,
}

impl MultiModelProvider {
    /// Constructs a [`MultiModelProvider`] from optional Gemini and TypeSafe credentials.
    pub fn new(gemini_api_key: Option<String>, typesafe_api_key: Option<String>) -> Self {
        let gemini = gemini_api_key
            .filter(|k| !k.trim().is_empty())
            .map(GeminiModelProvider::new);
        let typesafe = typesafe_api_key
            .filter(|k| !k.trim().is_empty())
            .map(TypesafeDecisionProvider::new);
        Self { gemini, typesafe }
    }
}

impl DecisionModelProvider for MultiModelProvider {
    fn evaluate_decisions(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, ProviderError> {
        let prov = model_provider(&request.model_specifier);
        if prov != TYPESAFE_PROVIDER {
            return Err(ProviderError::UnsupportedProvider {
                provider: prov.to_string(),
                model_specifier: request.model_specifier.clone(),
            });
        }
        let adapter = self
            .typesafe
            .as_ref()
            .ok_or(ProviderError::MissingTypesafeApiKey)?;
        adapter.evaluate_decisions(request)
    }
}

impl StructuredModelProvider for MultiModelProvider {
    fn generate_structured(
        &self,
        request: &GenerationRequest,
    ) -> Result<GenerationResponse, ProviderError> {
        let prov = model_provider(&request.model_specifier);
        if prov != DEFAULT_PROVIDER {
            return Err(ProviderError::UnsupportedProvider {
                provider: prov.to_string(),
                model_specifier: request.model_specifier.clone(),
            });
        }
        let adapter = self.gemini.as_ref().ok_or(ProviderError::MissingApiKey)?;
        adapter.generate_structured(request)
    }

    fn evaluate_decisions(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, ProviderError> {
        DecisionModelProvider::evaluate_decisions(self, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_systemone_response_maps_wire_keys_to_domain_subjects() {
        let raw = r#"{
            "model": "jev-1.14.0",
            "answers": {
                "q_0": { "type": "noul", "noul": 0.94 },
                "q_1": { "type": "noul", "noul": 0.08 }
            },
            "usage": { "input_tokens": 410, "output_tokens": 0 }
        }"#;
        let mapping = vec![
            ("q_0".to_string(), ".canons/cli/rule-a.md".to_string()),
            ("q_1".to_string(), ".canons/cli/rule-b.md".to_string()),
        ];
        let decoded = decode_systemone_response(raw, 105, "typesafe:jev-latest", &mapping)
            .expect("decode valid systemone response");
        assert_eq!(decoded.resolved_model, "jev-1.14.0");
        assert_eq!(decoded.latency_ms, 105);
        assert_eq!(decoded.scores[".canons/cli/rule-a.md"], 0.94);
        assert_eq!(decoded.scores[".canons/cli/rule-b.md"], 0.08);
        assert_eq!(decoded.usage.prompt_tokens, 410);
        assert_eq!(decoded.usage.total_tokens, 410);
    }

    #[test]
    fn test_typesafe_provider_debug_redacts_secret_and_validates_inputs() {
        let secret = "super-secret-typesafe-token";
        let provider = TypesafeDecisionProvider::new(secret);
        let dbg = format!("{provider:?}");
        assert!(!dbg.contains(secret));
        assert!(dbg.contains("<redacted>"));

        let empty_provider = TypesafeDecisionProvider::new("  ");
        let req = DecisionRequest {
            model_specifier: "typesafe:jev-latest".to_string(),
            state: "diff".to_string(),
            questions: BTreeMap::from([("k".to_string(), "q?".to_string())]),
            timeout_ms: 5000,
        };
        assert_eq!(
            empty_provider.evaluate_decisions(&req),
            Err(ProviderError::MissingTypesafeApiKey)
        );
    }

    #[test]
    fn test_partition_decision_items_respects_count_and_character_budgets() {
        let items: Vec<(String, String)> = (0..5)
            .map(|i| (format!("c_{i}"), "x".repeat(2_900)))
            .collect();
        let by_chars = partition_decision_items(&items, 90_000, 48, 96_000, |(_, q)| q.len());
        assert_eq!(by_chars.len(), 3);
        assert_eq!(by_chars[0].len(), 2);
        assert_eq!(by_chars[1].len(), 2);
        assert_eq!(by_chars[2].len(), 1);

        let by_count = partition_decision_items(&items, 1_000, 2, 96_000, |(_, q)| q.len());
        assert_eq!(by_count.len(), 3);
        assert_eq!(by_count[0].len(), 2);
        assert_eq!(by_count[1].len(), 2);
        assert_eq!(by_count[2].len(), 1);
    }
}
