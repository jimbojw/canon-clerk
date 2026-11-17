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

//! Localhost OpenAI-compatible logprob discriminator adapter (`POST /v1/chat/completions` with `max_tokens: 1`
//! and two-class `YES`/`NO` softmax) implementing [`DecisionModelProvider`].

use super::decision::{DecisionModelProvider, DecisionRequest, DecisionResponse};
use super::gemini_decisions::is_decision_context_limit_error;
use super::local_wire::{
    LocalChatCompletionRequest, decode_single_completion, extract_local_error_detail,
};
use super::requester::{RetryObserver, RetryablePostRequest, SharedHttpRequester};
use super::retry::RetryPolicy;
use super::runtime::{HttpAuth, JsonPostRequest, RawHttpOutcome, execute_json_post};
use crate::caseload::LOCAL_PROVIDER;
use crate::configure::{
    DEFAULT_LOCAL_DECISION_ENDPOINT, ENV_CANON_CLERK_LOCAL_DECISION_ENDPOINT,
    is_valid_local_model_name,
};
use crate::provider::{ProviderError, TokenUsage};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

/// System instruction constraining the local discriminator to single-token `"YES"` / `"NO"` classification.
pub const LOCAL_DISCRIMINATOR_SYSTEM_PROMPT: &str =
    "You are a precise code governance discriminator. Answer strictly with \"YES\" or \"NO\".";

/// Strips the canonical `"local:"` provider prefix from a normalized model specifier.
pub(crate) fn strip_local_prefix(model_specifier: &str) -> &str {
    match model_specifier.trim().strip_prefix(LOCAL_PROVIDER) {
        Some(rest) => rest.strip_prefix(':').unwrap_or(model_specifier.trim()),
        None => model_specifier.trim(),
    }
}

/// Constructs the OpenAI-compatible `POST /v1/chat/completions` URL from a base endpoint URL.
pub fn build_local_chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if let Some(origin) = trimmed.strip_suffix("/v1/chat/completions") {
        format!("{origin}/v1/chat/completions")
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/chat/completions")
    } else {
        format!("{trimmed}/v1/chat/completions")
    }
}

/// Formats an HTTP error status code from a localhost decision server into an actionable diagnostic.
pub fn classify_local_http_error(status_code: u16, model_specifier: &str, detail: &str) -> String {
    match status_code {
        400 if is_decision_context_limit_error(detail) => format!(
            "HTTP 400: Input context exceeded local discriminator window for `{model_specifier}` ({detail}). \
             Hint: Increase `--ctx-size` on `llama-server` or narrow target scope."
        ),
        404 => format!(
            "HTTP 404: Local `/v1/chat/completions` endpoint or model `{model_specifier}` was not found ({detail}). \
             Hint: Verify `--local-decision-endpoint` / `{ENV_CANON_CLERK_LOCAL_DECISION_ENDPOINT}` (default: `{DEFAULT_LOCAL_DECISION_ENDPOINT}`)."
        ),
        500..=599 => format!(
            "HTTP {status_code}: Local discriminator daemon returned a server error for `{model_specifier}` ({detail}). \
             Hint: Check `llama-server` logs (`curl -s {DEFAULT_LOCAL_DECISION_ENDPOINT}/health`) or increase `--timeout <MS>`."
        ),
        _ => format!(
            "HTTP {status_code}: Local discriminator rejected request for `{model_specifier}` ({detail}). \
             Hint: Verify `llama-server` compatibility with `/v1/chat/completions` and `logprobs: true`."
        ),
    }
}

fn build_local_http_status_error(
    status_code: u16,
    model_specifier: &str,
    body: &str,
) -> ProviderError {
    let detail = extract_local_error_detail(body);
    let diagnostic = classify_local_http_error(status_code, model_specifier, &detail);
    ProviderError::HttpStatus {
        status_code,
        model_specifier: model_specifier.to_string(),
        detail,
        diagnostic,
    }
}

pub(crate) fn post_local_chat_json<T: Serialize>(
    client: &reqwest::Client,
    base_url: &str,
    model_specifier: &str,
    timeout_ms: u64,
    payload: &T,
) -> Result<RawHttpOutcome, (ProviderError, u64)> {
    let url = build_local_chat_completions_url(base_url);
    let req = JsonPostRequest {
        url: &url,
        model_specifier,
        auth: HttpAuth::None,
        timeout_ms,
    };
    execute_json_post(client, &req, payload)
}

fn validate_local_specifier(raw: &str) -> Result<&str, ProviderError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return Err(ProviderError::InvalidModelSpecifier {
            model_specifier: raw.to_string(),
            reason: "model specifier must be non-empty and contain no whitespace".to_string(),
        });
    }
    let Some((prov, model_id)) = trimmed.split_once(':') else {
        return Err(ProviderError::InvalidModelSpecifier {
            model_specifier: raw.to_string(),
            reason: "expected `local:<model>` specifier".to_string(),
        });
    };
    if prov != LOCAL_PROVIDER {
        return Err(ProviderError::UnsupportedProvider {
            provider: prov.to_string(),
            model_specifier: trimmed.to_string(),
        });
    }
    if model_id.contains(':') || !is_valid_local_model_name(model_id) {
        return Err(ProviderError::InvalidModelSpecifier {
            model_specifier: raw.to_string(),
            reason: "expected `local:<model>` with valid model characters".to_string(),
        });
    }
    Ok(model_id)
}

/// Pooled localhost HTTP adapter implementing [`DecisionModelProvider`] via `/v1/chat/completions` logprobs.
#[derive(Debug, Clone)]
pub struct LocalDecisionProvider {
    base_url: String,
    requester: SharedHttpRequester,
    batch_lock: Arc<Mutex<()>>,
}

impl Default for LocalDecisionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalDecisionProvider {
    /// Constructs a [`LocalDecisionProvider`] targeting [`DEFAULT_LOCAL_DECISION_ENDPOINT`].
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_LOCAL_DECISION_ENDPOINT)
    }

    /// Constructs a [`LocalDecisionProvider`] targeting a custom localhost/LAN base URL.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self::with_requester_and_base_url(SharedHttpRequester::default(), base_url)
    }

    /// Constructs a [`LocalDecisionProvider`] sharing an explicit [`SharedHttpRequester`].
    pub fn with_requester(requester: SharedHttpRequester) -> Self {
        Self::with_requester_and_base_url(requester, DEFAULT_LOCAL_DECISION_ENDPOINT)
    }

    /// Constructs a [`LocalDecisionProvider`] sharing an explicit [`SharedHttpRequester`] and custom base URL.
    pub fn with_requester_and_base_url(
        requester: SharedHttpRequester,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            requester,
            batch_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Constructs a [`LocalDecisionProvider`] wrapping an explicit [`reqwest::Client`] and base URL.
    pub fn with_client_and_base_url(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self::with_requester_and_base_url(
            SharedHttpRequester::with_client(client, RetryPolicy::default()),
            base_url,
        )
    }

    /// Returns the configured local endpoint base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns a reference to the shared [`SharedHttpRequester`].
    pub fn requester(&self) -> &SharedHttpRequester {
        &self.requester
    }
}

impl DecisionModelProvider for LocalDecisionProvider {
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
        let model_id = validate_local_specifier(&request.model_specifier)?;
        if request.questions.is_empty() {
            return Ok(DecisionResponse {
                scores: BTreeMap::new(),
                resolved_model: request.model_specifier.clone(),
                latency_ms: 0,
                usage: TokenUsage::default(),
            });
        }

        // Serialize shared-`state` question batches so concurrent stage workers do not evict
        // a single-slot local server's KV prefix cache mid-batch.
        let _batch_guard = self
            .batch_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        let url = build_local_chat_completions_url(&self.base_url);
        let mut scores = BTreeMap::new();
        let mut resolved_model = request.model_specifier.clone();
        let mut total_latency_ms = 0_u64;
        let mut total_usage = TokenUsage::default();

        for (domain_key, instructions) in &request.questions {
            let payload = LocalChatCompletionRequest::new(model_id, &request.state, instructions);
            let outcome = self.requester.execute_json_post(
                &RetryablePostRequest {
                    url: &url,
                    model_specifier: &request.model_specifier,
                    auth: HttpAuth::None,
                    timeout_ms: request.timeout_ms,
                    call_label: request.call_label.as_deref().or(Some(domain_key.as_str())),
                    format_http_error: build_local_http_status_error,
                },
                &payload,
                on_retry,
            )?;

            let (score, upstream_model, usage) =
                decode_single_completion(&outcome.body, &request.model_specifier, domain_key)?;
            if let Some(m) = upstream_model {
                resolved_model = m;
            }
            total_latency_ms = total_latency_ms.saturating_add(outcome.latency_ms);
            total_usage.accumulate(usage);
            scores.insert(domain_key.clone(), score);
        }

        Ok(DecisionResponse {
            scores,
            resolved_model,
            latency_ms: total_latency_ms,
            usage: total_usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_url_builder_and_two_class_logprob_softmax() {
        assert_eq!(
            build_local_chat_completions_url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
        assert_eq!(
            build_local_chat_completions_url("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
        assert_eq!(
            strip_local_prefix("local:qwen2.5-coder-1.5b-instruct"),
            "qwen2.5-coder-1.5b-instruct"
        );

        let pos_json = r#"{
            "model": "qwen2.5-coder-1.5b-instruct-q4_k_m.gguf",
            "choices": [{
                "message": { "content": "YES" },
                "logprobs": {
                    "content": [{
                        "token": "YES",
                        "logprob": -0.208,
                        "top_logprobs": [
                            { "token": "YES", "logprob": -0.208 },
                            { "token": "NO", "logprob": -1.737 }
                        ]
                    }]
                }
            }],
            "usage": { "prompt_tokens": 92, "completion_tokens": 1, "total_tokens": 93 }
        }"#;
        let (pos_score, resolved, usage) = decode_single_completion(
            pos_json,
            "local:qwen2.5-coder-1.5b-instruct",
            "cli-flags.md",
        )
        .expect("decode positive completion");
        assert!((pos_score - 0.822).abs() < 0.002);
        assert_eq!(
            resolved.as_deref(),
            Some("qwen2.5-coder-1.5b-instruct-q4_k_m.gguf")
        );
        assert_eq!(usage.prompt_tokens, 92);
        assert_eq!(usage.total_tokens, 93);

        let neg_json = r#"{
            "choices": [{
                "message": { "content": "NO" },
                "logprobs": {
                    "content": [{
                        "token": "NO",
                        "logprob": -0.212,
                        "top_logprobs": [
                            { "token": "NO", "logprob": -0.212 },
                            { "token": "YES", "logprob": -1.696 }
                        ]
                    }]
                }
            }]
        }"#;
        let (neg_score, _, _) = decode_single_completion(
            neg_json,
            "local:qwen2.5-coder-1.5b-instruct",
            "cli-flags.md",
        )
        .expect("decode negative completion");
        assert!((neg_score - 0.185).abs() < 0.005);
    }

    #[test]
    fn test_loopback_multi_question_batch_reuses_pooled_socket_without_auth_header() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept single pooled socket");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set timeout");
            for idx in 1..=2 {
                let mut raw = Vec::new();
                let mut buf = [0_u8; 4096];
                loop {
                    let n = stream.read(&mut buf).expect("read");
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        assert!(
                            !head.to_ascii_lowercase().contains("authorization:")
                                && !head.to_ascii_lowercase().contains("x-goog-api-key:"),
                            "localhost request must not send auth headers: {head}"
                        );
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
                            break;
                        }
                    }
                }
                let (yes_lp, no_lp) = if idx == 1 {
                    (-0.10, -2.40)
                } else {
                    (-2.50, -0.09)
                };
                let body = format!(
                    r#"{{"model":"qwen2.5-coder-1.5b","choices":[{{"message":{{"content":"YES"}},"logprobs":{{"content":[{{"token":"YES","logprob":{yes_lp},"top_logprobs":[{{"token":"YES","logprob":{yes_lp}}},{{"token":"NO","logprob":{no_lp}}}]}}]}}}}],"usage":{{"prompt_tokens":40,"completion_tokens":1,"total_tokens":41}}}}"#
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).expect("write");
                stream.flush().expect("flush");
            }
        });

        let provider = LocalDecisionProvider::with_base_url(format!("http://{addr}"));
        let req = DecisionRequest {
            model_specifier: "local:qwen2.5-coder-1.5b-instruct".to_string(),
            state: "diff --git a/cli.rs b/cli.rs".to_string(),
            questions: BTreeMap::from([
                ("a.md".to_string(), "Question A?".to_string()),
                ("b.md".to_string(), "Question B?".to_string()),
            ]),
            timeout_ms: 2_000,
            call_label: None,
        };
        let resp = provider.evaluate_decisions(&req).expect("evaluate local");
        assert!(resp.scores["a.md"] > 0.90);
        assert!(resp.scores["b.md"] < 0.10);
        assert_eq!(resp.usage.prompt_tokens, 80);
        server.join().expect("server joined");
    }
}
