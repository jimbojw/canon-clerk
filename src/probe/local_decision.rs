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

//! Localhost cross-encoder (`/v1/rerank`) diagnostic endpoint prober implementation.

use crate::caseload::{ProbeEndpointResult, ProbeStatus, ThinkingLevel};
use crate::configure::DEFAULT_LOCAL_DECISION_ENDPOINT;
use crate::probe::EndpointProber;
use crate::provider::{
    LocalRerankRequest, LocalRerankResponse, build_pooled_http_client, classify_local_http_error,
    extract_local_error_detail, normalize_local_model_label, post_local_rerank_json,
    strip_local_prefix,
};

/// Decodes and classifies an HTTP status code and response payload from a local `/v1/rerank` probe.
pub fn decode_local_decision_probe_response(
    status_code: u16,
    body: &str,
    model_specifier: &str,
    latency_ms: u64,
) -> ProbeEndpointResult {
    if (200..300).contains(&status_code) {
        let Ok(parsed) = serde_json::from_str::<LocalRerankResponse>(body) else {
            return ProbeEndpointResult {
                status: ProbeStatus::Error,
                latency_ms,
                model: model_specifier.to_string(),
                error: Some(format!(
                    "Malformed `/v1/rerank` JSON response from `{model_specifier}`. \
                     Hint: Ensure `llama-server` is started with `--reranking --pooling rank`."
                )),
            };
        };
        if parsed.results.is_empty() {
            return ProbeEndpointResult {
                status: ProbeStatus::Error,
                latency_ms,
                model: model_specifier.to_string(),
                error: Some(format!(
                    "Empty `results` array returned by `/v1/rerank` for `{model_specifier}`. \
                     Hint: Ensure `llama-server` is started with `--reranking --pooling rank`."
                )),
            };
        }

        let resolved_model = parsed
            .model
            .as_deref()
            .map(normalize_local_model_label)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| model_specifier.to_string());

        return ProbeEndpointResult {
            status: ProbeStatus::Ok,
            latency_ms,
            model: resolved_model,
            error: None,
        };
    }

    let upstream_detail = extract_local_error_detail(body);
    let diagnostic = classify_local_http_error(status_code, model_specifier, &upstream_detail);
    ProbeEndpointResult {
        status: ProbeStatus::Error,
        latency_ms,
        model: model_specifier.to_string(),
        error: Some(diagnostic),
    }
}

/// Live localhost HTTP implementation of [`EndpointProber`] (`POST /v1/rerank`).
#[derive(Debug, Clone)]
pub struct LocalEndpointProber {
    base_url: String,
    client: reqwest::Client,
}

impl Default for LocalEndpointProber {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalEndpointProber {
    /// Constructs a new [`LocalEndpointProber`] targeting [`DEFAULT_LOCAL_DECISION_ENDPOINT`].
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_LOCAL_DECISION_ENDPOINT)
    }

    /// Constructs a [`LocalEndpointProber`] with a custom base URL.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self::with_client_and_base_url(build_pooled_http_client(), base_url)
    }

    /// Constructs a [`LocalEndpointProber`] with a custom [`reqwest::Client`] and base URL.
    pub fn with_client_and_base_url(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client,
        }
    }
}

impl EndpointProber for LocalEndpointProber {
    fn probe_endpoint(
        &self,
        model_specifier: &str,
        _api_key: &str,
        timeout_ms: u64,
        _thinking_level: Option<ThinkingLevel>,
        _reasoning_budget: Option<u32>,
    ) -> ProbeEndpointResult {
        let payload = LocalRerankRequest::new_probe(strip_local_prefix(model_specifier));

        match post_local_rerank_json(
            &self.client,
            &self.base_url,
            model_specifier,
            timeout_ms,
            &payload,
        ) {
            Ok(outcome) => decode_local_decision_probe_response(
                outcome.status_code,
                &outcome.body,
                model_specifier,
                outcome.latency_ms,
            ),
            Err((err, latency_ms)) => ProbeEndpointResult {
                status: ProbeStatus::Error,
                latency_ms,
                model: model_specifier.to_string(),
                error: Some(err.to_string()),
            },
        }
    }
}
