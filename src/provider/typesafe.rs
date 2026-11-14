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

//! TypeSafe AI System One (`POST /v1/systemone`) wire transport and Jev `Noul` helpers.

use super::gemini::RawHttpOutcome;
use crate::caseload::TYPESAFE_PROVIDER;
use crate::configure::ENV_TYPESAFE_API_KEY;
use crate::provider::ProviderError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Default TypeSafe AI v1 REST API base URL.
pub const DEFAULT_TYPESAFE_BASE_URL: &str = "https://api.typesafe.ai/v1";

const JSON_MIME_TYPE: &str = "application/json";

/// Strips the canonical `"typesafe:"` provider prefix from a normalized model specifier.
pub(crate) fn strip_typesafe_prefix(model_specifier: &str) -> &str {
    match model_specifier.strip_prefix(TYPESAFE_PROVIDER) {
        Some(rest) => rest.strip_prefix(':').unwrap_or(model_specifier),
        None => model_specifier,
    }
}

/// Constructs the TypeSafe AI `POST /v1/systemone` URL from a base URL.
pub(crate) fn build_systemone_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/systemone")
}

/// Typed `noul` binary decision question in a TypeSafe AI `/v1/systemone` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TypesafeNoulQuestion {
    /// Question primitive discriminator (always `"noul"` for binary probability questions).
    #[serde(rename = "type")]
    pub(crate) kind: String,
    /// Closed yes/no instruction evaluated against `state`.
    pub(crate) instructions: String,
}

impl TypesafeNoulQuestion {
    /// Constructs a `"noul"` binary decision question with the given instructions.
    pub(crate) fn new(instructions: impl Into<String>) -> Self {
        Self {
            kind: "noul".to_string(),
            instructions: instructions.into(),
        }
    }
}

/// Request payload for `POST /v1/systemone`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TypesafeSystemOneRequest {
    /// Unprefixed Jev model identifier (e.g. `"jev-latest"`).
    pub(crate) model: String,
    /// Shared text state evaluated across all questions in a single forward pass.
    pub(crate) state: String,
    /// Deterministic map of question keys to `Noul` definitions.
    pub(crate) questions: BTreeMap<String, TypesafeNoulQuestion>,
}

/// Decoded answer entry for a single `noul` question in a `/v1/systemone` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TypesafeNoulAnswer {
    /// Question primitive discriminator (`"noul"`).
    #[serde(rename = "type", default)]
    pub(crate) kind: Option<String>,
    /// Calibrated probability in `[0.0, 1.0]` that the answer is affirmative.
    pub(crate) noul: f64,
}

/// Token usage telemetry returned by TypeSafe AI `/v1/systemone`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TypesafeUsage {
    /// Prompt/input tokens billed for `state` and `questions`.
    #[serde(default)]
    pub(crate) input_tokens: u64,
    /// Output tokens reported by the endpoint (typically `0` or minimal framing).
    #[serde(default)]
    pub(crate) output_tokens: u64,
}

/// Response envelope returned by `POST /v1/systemone`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TypesafeSystemOneResponse {
    /// Concrete Jev model identifier reported by the upstream endpoint.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// Keyed `noul` answers corresponding to `request.questions`.
    #[serde(default)]
    pub(crate) answers: BTreeMap<String, TypesafeNoulAnswer>,
    /// Optional token accounting metadata.
    #[serde(default)]
    pub(crate) usage: Option<TypesafeUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TypesafeErrorField {
    Object { message: Option<String> },
    Text(String),
}

#[derive(Debug, Deserialize)]
struct TypesafeErrorEnvelope {
    error: Option<TypesafeErrorField>,
    detail: Option<String>,
    message: Option<String>,
}

/// Extracts a human-readable upstream error detail from a TypeSafe AI JSON error body or raw text.
pub(crate) fn extract_typesafe_error_detail(body: &str) -> String {
    if let Ok(env) = serde_json::from_str::<TypesafeErrorEnvelope>(body) {
        let extracted = match env.error {
            Some(TypesafeErrorField::Object { message }) => message,
            Some(TypesafeErrorField::Text(text)) => Some(text),
            None => None,
        }
        .or(env.detail)
        .or(env.message)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

        if let Some(msg) = extracted {
            return msg;
        }
    }

    let trimmed = body.trim();
    if trimmed.is_empty() {
        "empty error response".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Formats an HTTP error status code from the TypeSafe AI API into an actionable diagnostic with remediation hints.
pub(crate) fn classify_typesafe_http_error(
    status_code: u16,
    model_specifier: &str,
    detail: &str,
) -> String {
    match status_code {
        400 if detail.contains("max_tokens_exceeded") => format!(
            "HTTP 400: Input context exceeded TypeSafe Jev token limit for `{model_specifier}` ({detail}). \
             Hint: Pair `--screener-model jev` with a long-context `--auditor-model` (e.g. `pro`) or narrow target scope."
        ),
        401 | 403 => format!(
            "HTTP {status_code}: Authentication or permission rejected for `{model_specifier}` ({detail}). \
             Hint: Verify `{ENV_TYPESAFE_API_KEY}`, `providers.typesafe.apiKey` in `~/.config/canon-clerk/config.json`, or `--typesafe-api-key`."
        ),
        404 => format!(
            "HTTP 404: Model or endpoint for `{model_specifier}` was not found ({detail}). \
             Hint: Specify a valid TypeSafe Jev model via `--screener-model` (e.g. `typesafe:jev-latest` or `jev`)."
        ),
        429 => format!(
            "HTTP 429: Quota or rate limit exceeded for `{model_specifier}` ({detail}). \
             Hint: Check your TypeSafe AI quota tier or wait before retrying."
        ),
        500..=599 => format!(
            "HTTP {status_code}: Upstream TypeSafe AI server error for `{model_specifier}` ({detail}). \
             Hint: Retry after a brief backoff or increase `--timeout <MS>`."
        ),
        _ => format!(
            "HTTP {status_code}: Request rejected for `{model_specifier}` ({detail}). \
             Hint: Check Jev model compatibility and TypeSafe AI configuration."
        ),
    }
}

pub(crate) fn post_typesafe_json<T: Serialize>(
    base_url: &str,
    model_specifier: &str,
    api_key: &str,
    timeout_ms: u64,
    payload: &T,
) -> Result<RawHttpOutcome, (ProviderError, u64)> {
    let url = build_systemone_url(base_url);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            (
                ProviderError::Transport {
                    model_specifier: model_specifier.to_string(),
                    message: format!("failed to initialize async HTTP runtime ({err})"),
                },
                0,
            )
        })?;

    let start = Instant::now();
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|err| {
                let elapsed = start.elapsed().as_millis() as u64;
                (
                    ProviderError::Transport {
                        model_specifier: model_specifier.to_string(),
                        message: format!("failed to build HTTPS client ({err})"),
                    },
                    elapsed,
                )
            })?;

        let send_res = client
            .post(&url)
            .bearer_auth(api_key)
            .header("Content-Type", JSON_MIME_TYPE)
            .json(payload)
            .send()
            .await;
        let latency_ms = start.elapsed().as_millis() as u64;

        let response = send_res.map_err(|err| {
            let provider_err = if err.is_timeout() {
                ProviderError::Timeout {
                    model_specifier: model_specifier.to_string(),
                    timeout_ms,
                }
            } else {
                ProviderError::Transport {
                    model_specifier: model_specifier.to_string(),
                    message: err.to_string(),
                }
            };
            (provider_err, latency_ms)
        })?;

        let status_code = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(RawHttpOutcome {
            status_code,
            body,
            latency_ms,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_prefix_and_error_classification_helpers() {
        assert_eq!(strip_typesafe_prefix("typesafe:jev-latest"), "jev-latest");
        assert_eq!(strip_typesafe_prefix("jev-1.14.0"), "jev-1.14.0");
        assert_eq!(
            build_systemone_url("https://api.typesafe.ai/v1/"),
            "https://api.typesafe.ai/v1/systemone"
        );

        assert_eq!(
            extract_typesafe_error_detail(r#"{"error":{"message":"Invalid token"}}"#),
            "Invalid token"
        );
        assert_eq!(
            extract_typesafe_error_detail(r#"{"detail":"Unauthorized"}"#),
            "Unauthorized"
        );

        let diag401 = classify_typesafe_http_error(401, "typesafe:jev-latest", "Invalid token");
        assert!(diag401.contains("HTTP 401"));
        assert!(diag401.contains("TYPESAFE_API_KEY"));
        assert!(diag401.contains("Hint:"));
    }

    #[test]
    fn test_systemone_noul_request_and_response_wire_round_trip() {
        let mut questions = BTreeMap::new();
        questions.insert(
            "ping".to_string(),
            TypesafeNoulQuestion::new("Is the input non-empty?"),
        );
        let req = TypesafeSystemOneRequest {
            model: "jev-latest".to_string(),
            state: "ping".to_string(),
            questions,
        };
        let encoded = serde_json::to_value(&req).expect("serialize request");
        assert_eq!(encoded["questions"]["ping"]["type"], "noul");

        let resp_raw = r#"{
            "model": "jev-1.14.0",
            "answers": {
                "ping": { "type": "noul", "noul": 0.99 }
            },
            "usage": { "input_tokens": 18, "output_tokens": 0 }
        }"#;
        let decoded: TypesafeSystemOneResponse =
            serde_json::from_str(resp_raw).expect("deserialize response");
        assert_eq!(decoded.model.as_deref(), Some("jev-1.14.0"));
        assert_eq!(decoded.answers["ping"].noul, 0.99);
    }
}
