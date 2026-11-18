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

//! Localhost cross-encoder discriminator adapter (`POST /v1/rerank` via `llama-server --reranking --pooling rank`)
//! implementing [`DecisionModelProvider`].

use super::decision::{DecisionModelProvider, DecisionRequest, DecisionResponse};
use super::gemini_decisions::is_decision_context_limit_error;
use super::local_wire::{LocalRerankRequest, decode_rerank_batch, extract_local_error_detail};
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

/// Strips the canonical `"local:"` provider prefix from a normalized model specifier.
pub(crate) fn strip_local_prefix(model_specifier: &str) -> &str {
    match model_specifier.trim().strip_prefix(LOCAL_PROVIDER) {
        Some(rest) => rest.strip_prefix(':').unwrap_or(model_specifier.trim()),
        None => model_specifier.trim(),
    }
}

/// Constructs the OpenAI/Jina-compatible `POST /v1/rerank` URL from a base endpoint URL.
pub fn build_local_rerank_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if let Some(origin) = trimmed.strip_suffix("/v1/rerank") {
        format!("{origin}/v1/rerank")
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/rerank")
    } else {
        format!("{trimmed}/v1/rerank")
    }
}

/// Formats an HTTP error status code from a localhost `/v1/rerank` discriminator daemon into an actionable diagnostic.
pub fn classify_local_http_error(status_code: u16, model_specifier: &str, detail: &str) -> String {
    match status_code {
        400 | 500 if is_decision_context_limit_error(detail) => format!(
            "HTTP {status_code}: Input context exceeded local discriminator window for `{model_specifier}` ({detail}). \
             Hint: Increase `--ctx-size` / `--ubatch-size` on `llama-server` or narrow target scope."
        ),
        404 | 501 => format!(
            "HTTP {status_code}: Local `/v1/rerank` endpoint was not found or reranking is disabled for `{model_specifier}` ({detail}). \
             Hint: Start `llama-server` with `--reranking --pooling rank` and verify `--local-decision-endpoint` / `{ENV_CANON_CLERK_LOCAL_DECISION_ENDPOINT}` (default: `{DEFAULT_LOCAL_DECISION_ENDPOINT}`)."
        ),
        500..=599 => format!(
            "HTTP {status_code}: Local discriminator daemon returned a server error for `{model_specifier}` ({detail}). \
             Hint: Check `llama-server` logs (`curl -s {DEFAULT_LOCAL_DECISION_ENDPOINT}/health`) or increase `--timeout <MS>`."
        ),
        _ => format!(
            "HTTP {status_code}: Local discriminator rejected request for `{model_specifier}` ({detail}). \
             Hint: Verify `llama-server --reranking --pooling rank` compatibility on `{DEFAULT_LOCAL_DECISION_ENDPOINT}/v1/rerank`."
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
    let effective_status = if status_code == 500 && is_decision_context_limit_error(&detail) {
        400
    } else {
        status_code
    };
    ProviderError::HttpStatus {
        status_code: effective_status,
        model_specifier: model_specifier.to_string(),
        detail,
        diagnostic,
    }
}

pub(crate) fn post_local_rerank_json<T: Serialize>(
    client: &reqwest::Client,
    base_url: &str,
    model_specifier: &str,
    timeout_ms: u64,
    payload: &T,
) -> Result<RawHttpOutcome, (ProviderError, u64)> {
    let url = build_local_rerank_url(base_url);
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

/// Pooled localhost HTTP adapter implementing [`DecisionModelProvider`] via batched `POST /v1/rerank`.
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

        // Serialize `/v1/rerank` batches so concurrent stage workers do not oversubscribe
        // a single local daemon's unified KV pool (`n_slots = 4`) across simultaneous requests.
        let _batch_guard = self
            .batch_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        let url = build_local_rerank_url(&self.base_url);
        let (ordered_keys, payload) =
            LocalRerankRequest::from_decision_request(model_id, &request.state, &request.questions);
        let fallback_label = ordered_keys.first().map(String::as_str);

        let outcome = self.requester.execute_json_post(
            &RetryablePostRequest {
                url: &url,
                model_specifier: &request.model_specifier,
                auth: HttpAuth::None,
                timeout_ms: request.timeout_ms,
                call_label: request.call_label.as_deref().or(fallback_label),
                format_http_error: build_local_http_status_error,
            },
            &payload,
            on_retry,
        )?;

        let decoded = decode_rerank_batch(&outcome.body, &request.model_specifier, &ordered_keys)?;
        let resolved_model = decoded
            .resolved_model
            .unwrap_or_else(|| request.model_specifier.clone());

        Ok(DecisionResponse {
            scores: decoded.scores,
            resolved_model,
            latency_ms: outcome.latency_ms,
            usage: decoded.usage,
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
    fn test_url_builder_orientation_and_rerank_batch_decoding() {
        assert_eq!(
            build_local_rerank_url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080/v1/rerank"
        );
        assert_eq!(
            build_local_rerank_url("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080/v1/rerank"
        );
        assert_eq!(
            build_local_rerank_url("http://127.0.0.1:8080/v1/rerank/"),
            "http://127.0.0.1:8080/v1/rerank"
        );
        assert_eq!(
            strip_local_prefix("local:qwen3-reranker-0.6b"),
            "qwen3-reranker-0.6b"
        );

        let (vet_keys, vet_req) = LocalRerankRequest::from_decision_request(
            "qwen3-reranker-0.6b",
            "Governing Canon + Exhibit Diff",
            &BTreeMap::from([("compliant".to_string(), "Judge compliance?".to_string())]),
        );
        assert_eq!(vet_keys, vec!["compliant".to_string()]);
        assert_eq!(vet_req.query, "Judge compliance?");
        assert_eq!(vet_req.documents, vec!["Governing Canon + Exhibit Diff"]);

        let out_of_order_json = r#"{
            "model": "/home/user/.cache/models/qwen3-reranker-0.6b-q8_0.gguf",
            "object": "list",
            "usage": { "prompt_tokens": 256, "total_tokens": 256 },
            "results": [
                { "index": 1, "relevance_score": 0.0124 },
                { "index": 0, "relevance_score": 0.9885 }
            ]
        }"#;
        let keys = vec!["cli-flags.md".to_string(), "signal-tests.md".to_string()];
        let decoded = decode_rerank_batch(out_of_order_json, "local:qwen3-reranker-0.6b", &keys)
            .expect("decode out-of-order rerank results");
        assert!((decoded.scores["cli-flags.md"] - 0.9885).abs() < 1e-6);
        assert!((decoded.scores["signal-tests.md"] - 0.0124).abs() < 1e-6);
        assert_eq!(
            decoded.resolved_model.as_deref(),
            Some("qwen3-reranker-0.6b-q8_0.gguf")
        );
        assert_eq!(decoded.usage.prompt_tokens, 256);
        assert_eq!(decoded.usage.total_tokens, 256);
    }

    #[test]
    fn test_loopback_multi_question_batch_executes_single_rerank_post_and_reuses_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept single pooled socket");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set timeout");
            for call_idx in 1..=2 {
                let mut raw = Vec::new();
                let mut buf = [0_u8; 4096];
                let req_body = loop {
                    let n = stream.read(&mut buf).expect("read");
                    assert!(n > 0, "unexpected EOF before HTTP body");
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        assert!(
                            head.starts_with("POST /v1/rerank HTTP/1.1"),
                            "expected POST /v1/rerank, got: {head}"
                        );
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
                            break body[..expected_len].to_string();
                        }
                    }
                };
                let parsed: LocalRerankRequest =
                    serde_json::from_str(&req_body).expect("parse request JSON");
                let body = if call_idx == 1 {
                    assert_eq!(parsed.documents.len(), 2);
                    r#"{"model":"qwen3-reranker-0.6b-q8_0.gguf","results":[{"index":0,"relevance_score":0.974},{"index":1,"relevance_score":0.018}],"usage":{"prompt_tokens":140,"total_tokens":140}}"#
                } else {
                    assert_eq!(parsed.documents.len(), 1);
                    assert_eq!(parsed.query, "Do exhibits comply?");
                    r#"{"model":"qwen3-reranker-0.6b-q8_0.gguf","results":[{"index":0,"relevance_score":0.031}],"usage":{"prompt_tokens":95,"total_tokens":95}}"#
                };
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
        let docket_req = DecisionRequest {
            model_specifier: "local:qwen3-reranker-0.6b".to_string(),
            state: "diff --git a/cli.rs b/cli.rs".to_string(),
            questions: BTreeMap::from([
                ("a.md".to_string(), "Question A?".to_string()),
                ("b.md".to_string(), "Question B?".to_string()),
            ]),
            timeout_ms: 2_000,
            call_label: None,
        };
        let docket_resp = provider
            .evaluate_decisions(&docket_req)
            .expect("evaluate batched docket");
        assert!(docket_resp.scores["a.md"] > 0.95);
        assert!(docket_resp.scores["b.md"] < 0.05);
        assert_eq!(docket_resp.usage.prompt_tokens, 140);

        let vet_req = DecisionRequest {
            model_specifier: "local:qwen3-reranker-0.6b".to_string(),
            state: "Governing Canon + Violating Exhibit".to_string(),
            questions: BTreeMap::from([(
                "compliant".to_string(),
                "Do exhibits comply?".to_string(),
            )]),
            timeout_ms: 2_000,
            call_label: Some("a.md".to_string()),
        };
        let vet_resp = provider
            .evaluate_decisions(&vet_req)
            .expect("evaluate single-question vet");
        assert!(vet_resp.scores["compliant"] < 0.05);
        server.join().expect("server joined");
    }
}
