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

//! Hexagonal outbound port, per-model decision budget profiles, and composite decision routing.

use super::gemini_decisions::GeminiDecisionProvider;
use super::local_decision::LocalDecisionProvider;
use super::requester::{RetryObserver, SharedHttpRequester};
use super::retry::RetryPolicy;
pub use super::typesafe::TypesafeDecisionProvider;
use crate::caseload::{DEFAULT_PROVIDER, LOCAL_PROVIDER, TYPESAFE_PROVIDER};
use crate::configure::{is_discriminative_model, model_provider};
use crate::provider::{
    GeminiModelProvider, GenerationRequest, GenerationResponse, ProviderError,
    StructuredModelProvider, TokenUsage,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Per-model context character and `Noul` question-batching budget profile (`docs/architecture/nodes/docket.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionBudgetProfile {
    /// Maximum shared `state` / `input` character budget for macro discriminative screening.
    pub max_state_chars: usize,
    /// Maximum combined `state` + `questions` character budget per forward pass.
    pub max_batch_chars: usize,
    /// Maximum `Noul` questions packed into a single forward pass by default.
    pub max_nouls_per_call: usize,
}

impl DecisionBudgetProfile {
    /// Expanded profile for Google Gemini Decisions (`google:gemini-decision-*`: `262,144` input tokens, `1,000` questions/call).
    pub const GEMINI_DECISION: Self = Self {
        max_state_chars: 240_000,
        max_batch_chars: 480_000,
        max_nouls_per_call: 256,
    };

    /// Calibrated profile for TypeSafe AI Jev (`typesafe:jev-*`: `32,768` input tokens).
    pub const TYPESAFE_JEV: Self = Self {
        max_state_chars: 36_000,
        max_batch_chars: 64_000,
        max_nouls_per_call: 48,
    };

    /// Calibrated profile for local `8,192`-token discriminative daemons (`local:*`).
    pub const LOCAL_DECISION: Self = Self {
        max_state_chars: 16_000,
        max_batch_chars: 24_000,
        max_nouls_per_call: 24,
    };

    /// Resolves the deterministic [`DecisionBudgetProfile`] for a discriminative model specifier.
    pub fn for_model(specifier: &str) -> Self {
        let prov = model_provider(specifier);
        if prov == LOCAL_PROVIDER {
            Self::LOCAL_DECISION
        } else if prov == TYPESAFE_PROVIDER || specifier.trim().starts_with("jev-") {
            Self::TYPESAFE_JEV
        } else {
            Self::GEMINI_DECISION
        }
    }
}

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
    /// Optional concise work-unit identifier (e.g. `canon_ref` or batch label) for per-call telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_label: Option<String>,
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

    /// Evaluates `request.questions` while streaming real-time retry/cooldown notifications to `on_retry`.
    fn evaluate_decisions_observed(
        &self,
        request: &DecisionRequest,
        _on_retry: Option<RetryObserver<'_>>,
    ) -> Result<DecisionResponse, ProviderError> {
        self.evaluate_decisions(request)
    }
}

/// Composite provider routing generative `google:*` turns to [`GeminiModelProvider`],
/// `google:gemini-decision-*` discriminative `Noul` passes to [`GeminiDecisionProvider`],
/// and `typesafe:*` discriminative `Noul` passes to [`TypesafeDecisionProvider`].
#[derive(Debug, Clone)]
pub struct MultiModelProvider {
    gemini: Option<GeminiModelProvider>,
    gemini_decisions: Option<GeminiDecisionProvider>,
    typesafe: Option<TypesafeDecisionProvider>,
    local: LocalDecisionProvider,
}

impl MultiModelProvider {
    /// Constructs a [`MultiModelProvider`] from optional Gemini and TypeSafe credentials,
    /// sharing a single [`SharedHttpRequester`] across all active provider adapters.
    pub fn new(gemini_api_key: Option<String>, typesafe_api_key: Option<String>) -> Self {
        Self::with_retry_policy(gemini_api_key, typesafe_api_key, RetryPolicy::default())
    }

    /// Constructs a [`MultiModelProvider`] sharing a centralized [`SharedHttpRequester`] configured with `retry_policy`.
    pub fn with_retry_policy(
        gemini_api_key: Option<String>,
        typesafe_api_key: Option<String>,
        retry_policy: RetryPolicy,
    ) -> Self {
        let requester = SharedHttpRequester::new(retry_policy);
        Self::with_requester(gemini_api_key, typesafe_api_key, requester)
    }

    /// Constructs a [`MultiModelProvider`] sharing a caller-supplied [`SharedHttpRequester`].
    pub fn with_requester(
        gemini_api_key: Option<String>,
        typesafe_api_key: Option<String>,
        requester: SharedHttpRequester,
    ) -> Self {
        let (gemini, gemini_decisions) = match gemini_api_key.filter(|k| !k.trim().is_empty()) {
            Some(key) => {
                let structured =
                    GeminiModelProvider::with_requester(key.clone(), requester.clone());
                let decisions = GeminiDecisionProvider::with_requester(key, requester.clone());
                (Some(structured), Some(decisions))
            }
            None => (None, None),
        };
        let typesafe = typesafe_api_key
            .filter(|k| !k.trim().is_empty())
            .map(|key| TypesafeDecisionProvider::with_requester(key, requester.clone()));
        let local = LocalDecisionProvider::with_requester(requester);
        Self {
            gemini,
            gemini_decisions,
            typesafe,
            local,
        }
    }

    /// Constructs a [`MultiModelProvider`] from pre-configured provider adapters (for loopback tests or custom base URLs).
    pub fn with_providers(
        gemini: Option<GeminiModelProvider>,
        gemini_decisions: Option<GeminiDecisionProvider>,
        typesafe: Option<TypesafeDecisionProvider>,
    ) -> Self {
        Self {
            gemini,
            gemini_decisions,
            typesafe,
            local: LocalDecisionProvider::new(),
        }
    }

    /// Overrides the localhost base URL used for `local:<model>` decision calls while preserving the shared requester.
    pub fn with_local_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        let requester = self.local.requester().clone();
        self.local = LocalDecisionProvider::with_requester_and_base_url(requester, endpoint);
        self
    }
}

impl DecisionModelProvider for MultiModelProvider {
    fn evaluate_decisions(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, ProviderError> {
        <Self as DecisionModelProvider>::evaluate_decisions_observed(self, request, None)
    }

    fn evaluate_decisions_observed(
        &self,
        request: &DecisionRequest,
        on_retry: Option<RetryObserver<'_>>,
    ) -> Result<DecisionResponse, ProviderError> {
        let prov = model_provider(&request.model_specifier);
        match prov {
            DEFAULT_PROVIDER if is_discriminative_model(&request.model_specifier) => {
                let adapter = self
                    .gemini_decisions
                    .as_ref()
                    .ok_or(ProviderError::MissingApiKey)?;
                adapter.evaluate_decisions_observed(request, on_retry)
            }
            DEFAULT_PROVIDER => Err(ProviderError::InvalidModelSpecifier {
                model_specifier: request.model_specifier.clone(),
                reason: "generative Gemini models do not support discriminative `Noul` evaluation"
                    .to_string(),
            }),
            TYPESAFE_PROVIDER => {
                let adapter = self
                    .typesafe
                    .as_ref()
                    .ok_or(ProviderError::MissingTypesafeApiKey)?;
                adapter.evaluate_decisions_observed(request, on_retry)
            }
            LOCAL_PROVIDER => self.local.evaluate_decisions_observed(request, on_retry),
            _ => Err(ProviderError::UnsupportedProvider {
                provider: prov.to_string(),
                model_specifier: request.model_specifier.clone(),
            }),
        }
    }
}

impl StructuredModelProvider for MultiModelProvider {
    fn generate_structured(
        &self,
        request: &GenerationRequest,
    ) -> Result<GenerationResponse, ProviderError> {
        self.generate_structured_observed(request, None)
    }

    fn generate_structured_observed(
        &self,
        request: &GenerationRequest,
        on_retry: Option<RetryObserver<'_>>,
    ) -> Result<GenerationResponse, ProviderError> {
        let prov = model_provider(&request.model_specifier);
        if prov != DEFAULT_PROVIDER {
            return Err(ProviderError::UnsupportedProvider {
                provider: prov.to_string(),
                model_specifier: request.model_specifier.clone(),
            });
        }
        if is_discriminative_model(&request.model_specifier) {
            return Err(ProviderError::InvalidModelSpecifier {
                model_specifier: request.model_specifier.clone(),
                reason: "discriminative Gemini Decisions models use `/v1beta/decisions` (`evaluate_decisions`) rather than `:generateContent`".to_string(),
            });
        }
        let adapter = self.gemini.as_ref().ok_or(ProviderError::MissingApiKey)?;
        adapter.generate_structured_observed(request, on_retry)
    }

    fn evaluate_decisions(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, ProviderError> {
        DecisionModelProvider::evaluate_decisions(self, request)
    }

    fn evaluate_decisions_observed(
        &self,
        request: &DecisionRequest,
        on_retry: Option<RetryObserver<'_>>,
    ) -> Result<DecisionResponse, ProviderError> {
        DecisionModelProvider::evaluate_decisions_observed(self, request, on_retry)
    }

    fn clear_abort(&self) {
        if let Some(g) = &self.gemini {
            g.requester().clear_abort();
        }
        if let Some(d) = &self.gemini_decisions {
            d.requester().clear_abort();
        }
        if let Some(t) = &self.typesafe {
            t.requester().clear_abort();
        }
        self.local.requester().clear_abort();
    }

    fn notify_abort(&self) {
        if let Some(g) = &self.gemini {
            g.requester().notify_abort();
        }
        if let Some(d) = &self.gemini_decisions {
            d.requester().notify_abort();
        }
        if let Some(t) = &self.typesafe {
            t.requester().notify_abort();
        }
        self.local.requester().notify_abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
