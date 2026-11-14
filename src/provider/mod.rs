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

//! Provider-agnostic hexagonal structured-generation port and adapter exports (`docs/architecture/overview.md` §3).

pub mod decision;
pub mod gemini;
mod gemini_wire;
mod typesafe;

pub use decision::{
    DecisionModelProvider, DecisionRequest, DecisionResponse, MultiModelProvider,
    TypesafeDecisionProvider,
};
pub(crate) use decision::{
    MAX_DECISION_BATCH_CHARS, MAX_DECISION_NOULS_PER_CALL, MAX_DECISION_STATE_CHARS,
    partition_decision_items,
};
pub use gemini::{
    DEFAULT_GEMINI_BASE_URL, GeminiModelProvider, build_generate_content_url,
    classify_gemini_http_error, extract_gemini_error_detail, strip_provider_prefix,
};
pub use typesafe::DEFAULT_TYPESAFE_BASE_URL;
pub(crate) use typesafe::{
    TypesafeNoulQuestion, TypesafeSystemOneRequest, TypesafeSystemOneResponse,
    classify_typesafe_http_error, extract_typesafe_error_detail, post_typesafe_json,
    strip_typesafe_prefix,
};

use crate::caseload::{DEFAULT_AUDITOR_MODEL, DEFAULT_SCREENER_MODEL, TYPESAFE_PROVIDER};
use crate::configure::{
    DEFAULT_JEV_MODEL, ENV_GEMINI_API_KEY, ENV_TYPESAFE_API_KEY, model_provider,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Default per-request timeout in milliseconds for structured model generation turns.
pub const DEFAULT_GENERATION_TIMEOUT_MS: u64 = 60_000;

/// Provider-neutral input specification for deterministic, schema-constrained model generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRequest {
    /// Canonical provider-prefixed model identifier (e.g. `"google:gemini-flash-lite-latest"`).
    pub model_specifier: String,
    /// Authoritative system-level role and evaluation instructions.
    pub system_instruction: String,
    /// Stage-specific user prompt containing candidate canons and tendered or admitted exhibits.
    pub user_prompt: String,
    /// JSON Schema enforcing constrained structured output and reason-first property ordering.
    pub response_schema: serde_json::Value,
    /// Optional maximum reasoning token budget for extended thinking (e.g. during `audit`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_budget: Option<u32>,
    /// Per-request timeout ceiling in milliseconds.
    pub timeout_ms: u64,
}

/// Token accounting telemetry returned by a model generation turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens consumed by system instructions, user prompt, and response schema.
    pub prompt_tokens: u64,
    /// Output tokens emitted in the structured JSON completion candidate.
    pub completion_tokens: u64,
    /// Reasoning/thinking tokens expended prior to emitting structured output.
    pub reasoning_tokens: u64,
    /// Cumulative tokens billed across prompt, reasoning, and completion phases.
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Accumulates token counts from `other` into `self` using saturating addition.
    pub fn accumulate(&mut self, other: Self) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

/// Normalized structured output and operational telemetry produced by [`StructuredModelProvider`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationResponse {
    /// Decoded structured JSON object conforming to `request.response_schema`.
    pub payload: serde_json::Value,
    /// Concrete model version identifier reported by the upstream provider endpoint.
    pub resolved_model: String,
    /// End-to-end request roundtrip latency in milliseconds.
    pub latency_ms: u64,
    /// Token accounting metrics for out-of-band telemetry reporting.
    pub usage: TokenUsage,
    /// Extracted intermediate reasoning summary chunks when extended thinking is active.
    pub thoughts: Vec<String>,
}

/// Actionable errors emitted by [`StructuredModelProvider`] and [`DecisionModelProvider`] implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// Required Google Gemini API key is missing or blank.
    MissingApiKey,
    /// Required TypeSafe AI API key is missing or blank.
    MissingTypesafeApiKey,
    /// Model specifier targets a provider prefix not supported by the adapter.
    UnsupportedProvider {
        provider: String,
        model_specifier: String,
    },
    /// Model specifier is empty, contains whitespace, or omits the model identifier.
    InvalidModelSpecifier {
        model_specifier: String,
        reason: String,
    },
    /// Request `timeout_ms` is zero (`0ms`).
    InvalidTimeout { timeout_ms: u64 },
    /// Upstream HTTP request exceeded `timeout_ms`.
    Timeout {
        model_specifier: String,
        timeout_ms: u64,
    },
    /// Network, DNS, TLS, or local async runtime failure before receiving an HTTP response.
    Transport {
        model_specifier: String,
        message: String,
    },
    /// Upstream endpoint returned a non-2xx HTTP status (`401`/`403`, `404`, `429`, `5xx`, etc.).
    HttpStatus {
        status_code: u16,
        model_specifier: String,
        detail: String,
        diagnostic: String,
    },
    /// Upstream model blocked the prompt or candidate (safety filter, recitation, empty candidates, or token exhaustion).
    BlockedResponse {
        model_specifier: String,
        reason: String,
    },
    /// Upstream HTTP 2xx payload could not be parsed as a valid provider response envelope or lacked output content.
    MalformedResponse {
        model_specifier: String,
        reason: String,
    },
    /// Model text output could not be decoded into a structured JSON object matching schema expectations.
    SchemaDecode {
        model_specifier: String,
        message: String,
    },
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => write!(
                f,
                "Missing required Google Gemini API key (`{ENV_GEMINI_API_KEY}`) for model generation.\n  \
                 Hint: Export `{ENV_GEMINI_API_KEY}=\"<your-api-key>\"`, configure \
                 `providers.google.apiKey` in `~/.config/canon-clerk/config.json` (mode `0600`), \
                 or pass `--gemini-api-key <KEY>`."
            ),
            Self::MissingTypesafeApiKey => write!(
                f,
                "Missing required TypeSafe AI API key (`{ENV_TYPESAFE_API_KEY}`) for decision evaluation.\n  \
                 Hint: Export `{ENV_TYPESAFE_API_KEY}=\"<your-api-key>\"`, configure \
                 `providers.typesafe.apiKey` in `~/.config/canon-clerk/config.json` (mode `0600`), \
                 or pass `--typesafe-api-key <KEY>`."
            ),
            Self::UnsupportedProvider {
                provider,
                model_specifier,
            } => write!(
                f,
                "Unsupported model provider `{provider}` in specifier `{model_specifier}`.\n  \
                 Hint: Canon Clerk supports `google` (`{DEFAULT_SCREENER_MODEL}`, `{DEFAULT_AUDITOR_MODEL}`) \
                 and `typesafe` (`{DEFAULT_JEV_MODEL}`)."
            ),
            Self::InvalidModelSpecifier {
                model_specifier,
                reason,
            } => write!(
                f,
                "Invalid model specifier `{model_specifier}`: {reason}\n  \
                 Hint: Specify a canonical model such as `{DEFAULT_SCREENER_MODEL}`, \
                 `{DEFAULT_AUDITOR_MODEL}`, or `{DEFAULT_JEV_MODEL}`."
            ),
            Self::InvalidTimeout { timeout_ms } => write!(
                f,
                "Invalid generation timeout `{timeout_ms}ms`: timeout must be greater than 0ms.\n  \
                 Hint: Configure a positive millisecond timeout (default: `{DEFAULT_GENERATION_TIMEOUT_MS}`)."
            ),
            Self::Timeout {
                model_specifier,
                timeout_ms,
            } => write!(
                f,
                "Request to `{model_specifier}` timed out after {timeout_ms}ms.\n  \
                 Hint: Check network connectivity or increase the request timeout."
            ),
            Self::Transport {
                model_specifier,
                message,
            } => {
                let host = if model_provider(model_specifier) == TYPESAFE_PROVIDER {
                    "api.typesafe.ai"
                } else {
                    "generativelanguage.googleapis.com"
                };
                write!(
                    f,
                    "Network connection failed for `{model_specifier}`: {message}\n  \
                     Hint: Verify internet connectivity and DNS/TLS access to `{host}`."
                )
            }
            Self::HttpStatus { diagnostic, .. } => f.write_str(diagnostic),
            Self::BlockedResponse {
                model_specifier,
                reason,
            } => write!(
                f,
                "Model response from `{model_specifier}` was blocked or empty: {reason}.\n  \
                 Hint: Inspect prompt inputs for policy triggers or increase `--reasoning-budget` if tokens were exhausted."
            ),
            Self::MalformedResponse {
                model_specifier,
                reason,
            } => write!(
                f,
                "Malformed response envelope from `{model_specifier}`: {reason}.\n  \
                 Hint: Retry the command or verify that `{model_specifier}` supports structured JSON generation."
            ),
            Self::SchemaDecode {
                model_specifier,
                message,
            } => write!(
                f,
                "Failed to decode structured JSON output from `{model_specifier}`: {message}.\n  \
                 Hint: Ensure `{model_specifier}` supports `responseSchema` constrained decoding and retry."
            ),
        }
    }
}

impl std::error::Error for ProviderError {}

/// Hexagonal outbound port for deterministic, schema-constrained structured LLM generation
/// (with optional discriminative `Noul` batch support for decision-model tiers).
pub trait StructuredModelProvider: Send + Sync {
    /// Executes a single structured generation turn and returns the decoded JSON object payload
    /// alongside token accounting and extracted reasoning thoughts.
    fn generate_structured(
        &self,
        request: &GenerationRequest,
    ) -> Result<GenerationResponse, ProviderError>;

    /// Evaluates batched binary `Noul` questions against a shared `state` when backed by a
    /// discriminative decision model (such as `typesafe:jev-*`).
    fn evaluate_decisions(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, ProviderError> {
        let provider = crate::configure::model_provider(&request.model_specifier).to_string();
        Err(ProviderError::UnsupportedProvider {
            provider,
            model_specifier: request.model_specifier.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    mod port_contracts_and_error_formatting {
        use super::*;

        struct StubStructuredProvider {
            canned: GenerationResponse,
        }

        impl StructuredModelProvider for StubStructuredProvider {
            fn generate_structured(
                &self,
                _request: &GenerationRequest,
            ) -> Result<GenerationResponse, ProviderError> {
                Ok(self.canned.clone())
            }
        }

        #[test]
        fn test_hexagonal_port_round_trips_structured_request_and_response() {
            let provider: Box<dyn StructuredModelProvider> = Box::new(StubStructuredProvider {
                canned: GenerationResponse {
                    payload: json!({
                        "apprisal_summary": "Governs cache expiration rules.",
                        "apprisal_score": 0.91
                    }),
                    resolved_model: "gemini-2.5-flash-lite".to_string(),
                    latency_ms: 142,
                    usage: TokenUsage {
                        prompt_tokens: 310,
                        completion_tokens: 45,
                        reasoning_tokens: 0,
                        total_tokens: 355,
                    },
                    thoughts: Vec::new(),
                },
            });

            let request = GenerationRequest {
                model_specifier: DEFAULT_SCREENER_MODEL.to_string(),
                system_instruction: "Evaluate statutory jurisdiction.".to_string(),
                user_prompt: "Intent: add auth cache".to_string(),
                response_schema: json!({"type": "object"}),
                reasoning_budget: None,
                timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
            };

            let encoded_req = serde_json::to_string(&request).expect("serialize request");
            assert!(!encoded_req.contains("reasoning_budget"));
            let decoded_req: GenerationRequest =
                serde_json::from_str(&encoded_req).expect("deserialize request");
            assert_eq!(decoded_req, request);

            let response = provider
                .generate_structured(&request)
                .expect("stub generation must succeed");
            assert_eq!(response.payload["apprisal_score"], json!(0.91));
            assert_eq!(response.usage.total_tokens, 355);
        }

        #[test]
        fn test_all_provider_error_variants_include_actionable_remediation_hints() {
            let errors = [
                ProviderError::MissingApiKey,
                ProviderError::MissingTypesafeApiKey,
                ProviderError::UnsupportedProvider {
                    provider: "openai".to_string(),
                    model_specifier: "openai:gpt-4o".to_string(),
                },
                ProviderError::InvalidModelSpecifier {
                    model_specifier: "google:".to_string(),
                    reason: "missing model id".to_string(),
                },
                ProviderError::InvalidTimeout { timeout_ms: 0 },
                ProviderError::Timeout {
                    model_specifier: DEFAULT_SCREENER_MODEL.to_string(),
                    timeout_ms: 5000,
                },
                ProviderError::Transport {
                    model_specifier: DEFAULT_SCREENER_MODEL.to_string(),
                    message: "connection refused".to_string(),
                },
                ProviderError::Transport {
                    model_specifier: DEFAULT_JEV_MODEL.to_string(),
                    message: "dns lookup failed".to_string(),
                },
                ProviderError::HttpStatus {
                    status_code: 429,
                    model_specifier: DEFAULT_AUDITOR_MODEL.to_string(),
                    detail: "quota exceeded".to_string(),
                    diagnostic: classify_gemini_http_error(
                        429,
                        DEFAULT_AUDITOR_MODEL,
                        "quota exceeded",
                    ),
                },
                ProviderError::BlockedResponse {
                    model_specifier: DEFAULT_AUDITOR_MODEL.to_string(),
                    reason: "SAFETY".to_string(),
                },
                ProviderError::MalformedResponse {
                    model_specifier: DEFAULT_SCREENER_MODEL.to_string(),
                    reason: "invalid envelope".to_string(),
                },
                ProviderError::SchemaDecode {
                    model_specifier: DEFAULT_SCREENER_MODEL.to_string(),
                    message: "expected JSON object".to_string(),
                },
            ];

            for err in errors {
                let rendered = err.to_string();
                assert!(
                    rendered.contains("Hint:"),
                    "ProviderError display must provide an actionable Hint: {rendered}"
                );
            }
        }
    }
}
