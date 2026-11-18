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

//! Google Gemini Decisions API (`POST /v1beta/decisions`) adapter implementing [`DecisionModelProvider`].

use super::decision::{DecisionModelProvider, DecisionRequest, DecisionResponse};
use super::gemini::{DEFAULT_GEMINI_BASE_URL, extract_gemini_error_detail, strip_provider_prefix};
use super::requester::{RetryObserver, RetryablePostRequest, SharedHttpRequester};
use super::retry::RetryPolicy;
use super::runtime::{HttpAuth, JsonPostRequest, RawHttpOutcome, execute_json_post};
use crate::caseload::DEFAULT_PROVIDER;
use crate::configure::{ENV_GEMINI_API_KEY, is_gemini_decision_model_name};
use crate::provider::{ProviderError, TokenUsage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Constructs the Google Gemini Decisions `POST /v1beta/decisions` URL from a base URL.
pub fn build_decisions_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/decisions")
}

/// Returns `true` when an upstream HTTP 400 detail message indicates a token, context, or question limit overrun.
pub(crate) fn is_decision_context_limit_error(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("max_tokens_exceeded")
        || (lower.contains("token") && lower.contains("too large"))
        || ((lower.contains("token") || lower.contains("context") || lower.contains("question"))
            && (lower.contains("exceed") || lower.contains("limit") || lower.contains("maximum")))
}

/// Typed `noul` binary decision question in a Gemini `/v1beta/decisions` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GeminiNoulQuestion {
    /// Question primitive discriminator (always `"noul"` for binary probability questions).
    #[serde(rename = "type")]
    pub(crate) kind: String,
    /// Closed yes/no instruction evaluated against `input`.
    pub(crate) instructions: String,
}

impl GeminiNoulQuestion {
    /// Constructs a `"noul"` binary decision question with the given instructions.
    pub(crate) fn new(instructions: impl Into<String>) -> Self {
        Self {
            kind: "noul".to_string(),
            instructions: instructions.into(),
        }
    }
}

/// Request payload for `POST /v1beta/decisions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GeminiDecisionsRequest {
    /// Unprefixed Gemini Decisions model identifier (e.g. `"gemini-decision-1"`).
    pub(crate) model: String,
    /// Shared input context evaluated across all questions in a single forward pass.
    pub(crate) input: String,
    /// Deterministic map of question keys to `Noul` definitions.
    pub(crate) questions: BTreeMap<String, GeminiNoulQuestion>,
}

/// Decoded answer entry for a single `noul` question in a `/v1beta/decisions` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct GeminiNoulAnswer {
    /// Question primitive discriminator (`"noul"`).
    #[serde(rename = "type", default)]
    pub(crate) kind: Option<String>,
    /// Calibrated probability in `[0.0, 1.0]` that the answer is affirmative.
    pub(crate) noul: f64,
    /// Optional confidence score reported by `/v1beta/decisions`.
    #[serde(default)]
    pub(crate) confidence: Option<f64>,
}

/// Token usage telemetry returned by Google Gemini `/v1beta/decisions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GeminiDecisionsUsage {
    /// Prompt/input tokens billed for `input` and `questions`.
    #[serde(default)]
    pub(crate) total_input_tokens: u64,
    /// Output tokens reported by `/v1beta/decisions` (`0` for discriminative `noul` passes).
    #[serde(default)]
    pub(crate) total_output_tokens: u64,
    /// Total tokens reported by `/v1beta/decisions`.
    #[serde(default)]
    pub(crate) total_tokens: Option<u64>,
}

/// Response envelope returned by `POST /v1beta/decisions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct GeminiDecisionsResponse {
    /// Concrete Gemini Decisions model identifier when echoed by the upstream endpoint.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// Keyed `noul` answers corresponding to `request.questions`.
    #[serde(default)]
    pub(crate) answers: BTreeMap<String, GeminiNoulAnswer>,
    /// Optional token accounting metadata.
    #[serde(default)]
    pub(crate) usage: Option<GeminiDecisionsUsage>,
}

/// Formats an HTTP error status code from `/v1beta/decisions` into an actionable diagnostic with remediation hints.
pub fn classify_gemini_decisions_http_error(
    status_code: u16,
    model_specifier: &str,
    detail: &str,
) -> String {
    match status_code {
        400 if is_decision_context_limit_error(detail) => format!(
            "HTTP 400: Input context or question batch exceeded Gemini Decisions limit for `{model_specifier}` ({detail}). \
             Hint: Narrow target scope or reduce batch size via `--docket-batch-size` / `--apprise-batch-size`."
        ),
        401 | 403 => format!(
            "HTTP {status_code}: Authentication or permission rejected for `{model_specifier}` ({detail}). \
             Hint: Verify `{ENV_GEMINI_API_KEY}`, `providers.google.apiKey` in `~/.config/canon-clerk/config.json`, or `--gemini-api-key`."
        ),
        404 => format!(
            "HTTP 404: Model or endpoint for `{model_specifier}` was not found ({detail}). \
             Hint: Specify a valid Gemini Decisions model via `--screener-model` (e.g. `google:gemini-decision-1` or `gemini-decision-1`)."
        ),
        429 => format!(
            "HTTP 429: Quota or rate limit exceeded for `{model_specifier}` ({detail}). \
             Hint: Check your Google AI Studio quota tier or wait before retrying."
        ),
        500..=599 => format!(
            "HTTP {status_code}: Upstream Google Gemini Decisions server error for `{model_specifier}` ({detail}). \
             Hint: Retry after a brief backoff or increase `--timeout <MS>`."
        ),
        _ => format!(
            "HTTP {status_code}: Request rejected for `{model_specifier}` ({detail}). \
             Hint: Check Gemini Decisions model compatibility (`gemini-decision-1`) and provider configuration."
        ),
    }
}

pub(crate) fn post_gemini_decisions_json<T: Serialize>(
    client: &reqwest::Client,
    base_url: &str,
    model_specifier: &str,
    api_key: &str,
    timeout_ms: u64,
    payload: &T,
) -> Result<RawHttpOutcome, (ProviderError, u64)> {
    let url = build_decisions_url(base_url);
    let req = JsonPostRequest {
        url: &url,
        model_specifier,
        auth: HttpAuth::GoogApiKey(api_key),
        timeout_ms,
    };
    execute_json_post(client, &req, payload)
}

fn build_gemini_decisions_http_status_error(
    status_code: u16,
    model_specifier: &str,
    body: &str,
) -> ProviderError {
    let detail = extract_gemini_error_detail(body);
    let diagnostic = classify_gemini_decisions_http_error(status_code, model_specifier, &detail);
    ProviderError::HttpStatus {
        status_code,
        model_specifier: model_specifier.to_string(),
        detail,
        diagnostic,
    }
}

/// Live Google Gemini Decisions HTTPS adapter implementing [`DecisionModelProvider`] (`POST /v1beta/decisions`).
#[derive(Clone)]
pub struct GeminiDecisionProvider {
    api_key: String,
    base_url: String,
    requester: SharedHttpRequester,
}

impl fmt::Debug for GeminiDecisionProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key_repr = if self.api_key.trim().is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        };
        f.debug_struct("GeminiDecisionProvider")
            .field("api_key", &key_repr)
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl GeminiDecisionProvider {
    /// Constructs a [`GeminiDecisionProvider`] targeting [`DEFAULT_GEMINI_BASE_URL`].
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_GEMINI_BASE_URL)
    }

    /// Constructs a [`GeminiDecisionProvider`] with a custom base URL (for loopback tests or proxies).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_requester_and_base_url(api_key, SharedHttpRequester::default(), base_url)
    }

    /// Constructs a [`GeminiDecisionProvider`] with a caller-supplied reusable [`reqwest::Client`].
    pub fn with_client(api_key: impl Into<String>, client: reqwest::Client) -> Self {
        Self::with_client_and_base_url(api_key, client, DEFAULT_GEMINI_BASE_URL)
    }

    /// Constructs a [`GeminiDecisionProvider`] with a custom [`reqwest::Client`] and base URL.
    pub fn with_client_and_base_url(
        api_key: impl Into<String>,
        client: reqwest::Client,
        base_url: impl Into<String>,
    ) -> Self {
        Self::with_requester_and_base_url(
            api_key,
            SharedHttpRequester::with_client(client, RetryPolicy::default()),
            base_url,
        )
    }

    /// Constructs a [`GeminiDecisionProvider`] sharing an explicit [`SharedHttpRequester`].
    pub fn with_requester(api_key: impl Into<String>, requester: SharedHttpRequester) -> Self {
        Self::with_requester_and_base_url(api_key, requester, DEFAULT_GEMINI_BASE_URL)
    }

    /// Constructs a [`GeminiDecisionProvider`] sharing an explicit [`SharedHttpRequester`] and custom base URL.
    pub fn with_requester_and_base_url(
        api_key: impl Into<String>,
        requester: SharedHttpRequester,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            requester,
        }
    }

    /// Returns the configured Google Generative Language v1beta base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns a reference to the reusable, pooled [`reqwest::Client`].
    pub fn client(&self) -> &reqwest::Client {
        self.requester.client()
    }

    /// Returns a reference to the shared [`SharedHttpRequester`].
    pub fn requester(&self) -> &SharedHttpRequester {
        &self.requester
    }
}

impl DecisionModelProvider for GeminiDecisionProvider {
    fn evaluate_decisions(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, ProviderError> {
        self.evaluate_decisions_observed(request, None)
    }

    fn evaluate_decisions_observed(
        &self,
        request: &DecisionRequest,
        on_retry: Option<RetryObserver<'_>>,
    ) -> Result<DecisionResponse, ProviderError> {
        if request.timeout_ms == 0 {
            return Err(ProviderError::InvalidTimeout { timeout_ms: 0 });
        }

        let api_key = self.api_key.trim();
        if api_key.is_empty() {
            return Err(ProviderError::MissingApiKey);
        }

        let model_id = validate_gemini_decision_specifier(&request.model_specifier)?;
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
            wire_questions.insert(wire_key.clone(), GeminiNoulQuestion::new(instructions));
            wire_to_domain.push((wire_key, domain_key.clone()));
        }

        let payload = GeminiDecisionsRequest {
            model: model_id.to_string(),
            input: request.state.clone(),
            questions: wire_questions,
        };
        let url = build_decisions_url(&self.base_url);
        let outcome = self.requester.execute_json_post(
            &RetryablePostRequest {
                url: &url,
                model_specifier: &request.model_specifier,
                auth: HttpAuth::GoogApiKey(api_key),
                timeout_ms: request.timeout_ms,
                call_label: request.call_label.as_deref(),
                format_http_error: build_gemini_decisions_http_status_error,
            },
            &payload,
            on_retry,
        )?;

        decode_gemini_decisions_response(
            &outcome.body,
            outcome.latency_ms,
            &request.model_specifier,
            model_id,
            &wire_to_domain,
        )
    }
}

fn validate_gemini_decision_specifier(raw: &str) -> Result<&str, ProviderError> {
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
                reason: "expected `google:<model>` with non-empty segments".to_string(),
            });
        }
        if prov != DEFAULT_PROVIDER {
            return Err(ProviderError::UnsupportedProvider {
                provider: prov.to_string(),
                model_specifier: trimmed.to_string(),
            });
        }
        if !is_gemini_decision_model_name(model_id) {
            return Err(ProviderError::InvalidModelSpecifier {
                model_specifier: raw.to_string(),
                reason: format!(
                    "expected a Gemini Decisions model (`gemini-decision-*`), got `{model_id}`"
                ),
            });
        }
        return Ok(model_id);
    }

    if !is_gemini_decision_model_name(trimmed) {
        return Err(ProviderError::InvalidModelSpecifier {
            model_specifier: raw.to_string(),
            reason: format!(
                "expected a Gemini Decisions model (`gemini-decision-*`), got `{trimmed}`"
            ),
        });
    }
    Ok(strip_provider_prefix(trimmed))
}

fn decode_gemini_decisions_response(
    body: &str,
    latency_ms: u64,
    model_specifier: &str,
    fallback_model_id: &str,
    wire_to_domain: &[(String, String)],
) -> Result<DecisionResponse, ProviderError> {
    let parsed: GeminiDecisionsResponse =
        serde_json::from_str(body).map_err(|err| ProviderError::MalformedResponse {
            model_specifier: model_specifier.to_string(),
            reason: format!("invalid Gemini `/v1beta/decisions` JSON envelope ({err})"),
        })?;

    let mut scores = BTreeMap::new();
    for (wire_key, domain_key) in wire_to_domain {
        let answer =
            parsed
                .answers
                .get(wire_key)
                .ok_or_else(|| ProviderError::SchemaDecode {
                    model_specifier: model_specifier.to_string(),
                    message: format!(
                        "missing `noul` answer `{wire_key}` for subject `{domain_key}` in `/v1beta/decisions` response"
                    ),
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
        .unwrap_or_else(|| fallback_model_id.to_string());

    let usage = match parsed.usage {
        Some(u) => TokenUsage {
            prompt_tokens: u.total_input_tokens,
            completion_tokens: u.total_output_tokens,
            reasoning_tokens: 0,
            total_tokens: u
                .total_tokens
                .unwrap_or_else(|| u.total_input_tokens.saturating_add(u.total_output_tokens)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::build_pooled_http_client;

    mod wire_decoding_and_error_classification {
        use super::*;

        #[test]
        fn test_decode_decisions_response_maps_wire_keys_confidence_and_usage() {
            let raw = r#"{
                "answers": {
                    "q_0": { "type": "noul", "noul": 0.97, "confidence": 0.99 },
                    "q_1": { "type": "noul", "noul": 0.03, "confidence": 0.98 }
                },
                "usage": {
                    "total_input_tokens": 1240,
                    "total_output_tokens": 0,
                    "total_tokens": 1240
                }
            }"#;
            let mapping = vec![
                ("q_0".to_string(), ".canons/cli/rule-a.md".to_string()),
                ("q_1".to_string(), ".canons/cli/rule-b.md".to_string()),
            ];
            let decoded = decode_gemini_decisions_response(
                raw,
                165,
                "google:gemini-decision-1",
                "gemini-decision-1",
                &mapping,
            )
            .expect("decode valid /v1beta/decisions response");
            assert_eq!(decoded.resolved_model, "gemini-decision-1");
            assert_eq!(decoded.latency_ms, 165);
            assert_eq!(decoded.scores[".canons/cli/rule-a.md"], 0.97);
            assert_eq!(decoded.scores[".canons/cli/rule-b.md"], 0.03);
            assert_eq!(decoded.usage.prompt_tokens, 1240);
            assert_eq!(decoded.usage.completion_tokens, 0);
            assert_eq!(decoded.usage.total_tokens, 1240);
        }

        #[test]
        fn test_url_and_http_error_classification_helpers() {
            assert_eq!(
                build_decisions_url("https://generativelanguage.googleapis.com/v1beta/"),
                "https://generativelanguage.googleapis.com/v1beta/decisions"
            );

            let diag400 = classify_gemini_decisions_http_error(
                400,
                "google:gemini-decision-1",
                "Request input exceeds maximum token limit of 262144",
            );
            assert!(diag400.contains("HTTP 400"));
            assert!(diag400.contains("Hint:"));

            let diag401 = classify_gemini_decisions_http_error(
                401,
                "google:gemini-decision-1",
                "API key not valid",
            );
            assert!(diag401.contains("GEMINI_API_KEY"));
            assert!(diag401.contains("Hint:"));

            let diag404 = classify_gemini_decisions_http_error(
                404,
                "google:gemini-decision-99",
                "Model not found",
            );
            assert!(diag404.contains("gemini-decision-1"));
            assert!(diag404.contains("Hint:"));
        }
    }

    mod provider_validation_and_pooled_transport {
        use super::*;

        #[test]
        fn test_gemini_decision_provider_redacts_secret_and_validates_specifiers() {
            let secret = "super-secret-gemini-api-key";
            let provider = GeminiDecisionProvider::new(secret);
            let dbg = format!("{provider:?}");
            assert!(!dbg.contains(secret));
            assert!(dbg.contains("<redacted>"));

            let empty_provider = GeminiDecisionProvider::new("   ");
            let req = DecisionRequest {
                model_specifier: "google:gemini-decision-1".to_string(),
                state: "diff".to_string(),
                questions: BTreeMap::from([("k".to_string(), "q?".to_string())]),
                timeout_ms: 5000,
                call_label: None,
            };
            assert_eq!(
                empty_provider.evaluate_decisions(&req),
                Err(ProviderError::MissingApiKey)
            );

            // Rejects generative Gemini model on GeminiDecisionProvider
            let bad_model_req = DecisionRequest {
                model_specifier: "google:gemini-4-hydrogen".to_string(),
                ..req
            };
            assert!(matches!(
                provider.evaluate_decisions(&bad_model_req),
                Err(ProviderError::InvalidModelSpecifier { .. })
            ));
        }

        #[test]
        fn test_gemini_decision_provider_executes_and_reuses_pooled_socket_across_threads() {
            use std::io::{Read, Write};
            use std::net::TcpListener;
            use std::thread;
            use std::time::Duration;

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("local addr");

            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept single pooled socket");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set read timeout");
                for idx in 1..=2 {
                    let mut raw = Vec::new();
                    let mut buf = [0_u8; 4096];
                    let (headers, body_str) = loop {
                        let n = stream.read(&mut buf).expect("read request");
                        assert!(n > 0, "unexpected EOF before request {idx}");
                        raw.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&raw);
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            let expected_len = head
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if body.len() >= expected_len {
                                break (head.to_string(), body[..expected_len].to_string());
                            }
                        }
                    };

                    assert!(
                        headers.starts_with("POST /v1beta/decisions HTTP/1.1"),
                        "expected POST /v1beta/decisions, got: {headers}"
                    );
                    assert!(
                        headers
                            .to_ascii_lowercase()
                            .contains("x-goog-api-key: synthetic-gemini-key"),
                        "expected x-goog-api-key header, got: {headers}"
                    );
                    let wire_req: serde_json::Value =
                        serde_json::from_str(&body_str).expect("parse request JSON");
                    assert_eq!(wire_req["model"], "gemini-decision-1");
                    assert_eq!(wire_req["input"], "shared diff context");
                    assert_eq!(wire_req["questions"]["q_0"]["type"], "noul");

                    let score = if idx == 1 { 0.96 } else { 0.09 };
                    let resp_body = format!(
                        r#"{{"model":"gemini-decision-1","answers":{{"q_0":{{"type":"noul","noul":{score},"confidence":0.99}}}},"usage":{{"total_input_tokens":84,"total_output_tokens":0,"total_tokens":84}}}}"#
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    stream.write_all(resp.as_bytes()).expect("write response");
                    stream.flush().expect("flush response");
                }
            });

            let shared_client = build_pooled_http_client();
            let provider = GeminiDecisionProvider::with_client_and_base_url(
                "synthetic-gemini-key",
                shared_client,
                format!("http://{addr}/v1beta"),
            );
            assert_eq!(provider.base_url(), format!("http://{addr}/v1beta"));

            let req = DecisionRequest {
                model_specifier: "google:gemini-decision-1".to_string(),
                state: "shared diff context".to_string(),
                questions: BTreeMap::from([(
                    ".canons/cli/rule.md".to_string(),
                    "Is it colorable?".to_string(),
                )]),
                timeout_ms: 1000,
                call_label: None,
            };

            let prov1 = provider.clone();
            let req1 = req.clone();
            let res1 = thread::spawn(move || prov1.evaluate_decisions(&req1))
                .join()
                .expect("worker 1 joined")
                .expect("call 1 succeeded");
            assert_eq!(res1.scores[".canons/cli/rule.md"], 0.96);
            assert_eq!(res1.usage.prompt_tokens, 84);
            assert_eq!(res1.usage.completion_tokens, 0);

            let res2 = thread::spawn(move || provider.evaluate_decisions(&req))
                .join()
                .expect("worker 2 joined")
                .expect("call 2 reused pooled socket after worker 1 exited");
            assert_eq!(res2.scores[".canons/cli/rule.md"], 0.09);

            server.join().expect("server joined");
        }
    }
}
