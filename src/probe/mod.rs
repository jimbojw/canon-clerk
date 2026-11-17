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

//! Core domain execution engine and hexagonal endpoint prober for the `probe` diagnostic stage (`docs/architecture/nodes/probe.md`).

mod gemini_decisions;
mod local_decision;
mod typesafe;

pub use gemini_decisions::{GeminiDecisionsEndpointProber, decode_gemini_decisions_probe_response};
pub use local_decision::{LocalEndpointProber, decode_local_decision_probe_response};
pub use typesafe::TypesafeEndpointProber;

use crate::caseload::{
    Caseload, CaseloadProbe, LOCAL_PROVIDER, ProbeEndpointResult, ProbeStatus, TYPESAFE_PROVIDER,
    ThinkingLevel,
};
use crate::configure::{
    ENV_GEMINI_API_KEY, ENV_TYPESAFE_API_KEY, is_discriminative_model, model_provider,
};
use crate::provider::gemini::post_gemini_json;
pub use crate::provider::{
    DEFAULT_GEMINI_BASE_URL, DEFAULT_TYPESAFE_BASE_URL, build_generate_content_url,
    strip_provider_prefix,
};
use crate::provider::{
    GeminiThinkingConfig, build_pooled_http_client, build_probe_thinking_config,
    classify_gemini_http_error, extract_gemini_error_detail,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Default per-endpoint timeout in milliseconds for `canon-clerk probe` (`docs/architecture/nodes/probe.md` §5).
pub const DEFAULT_PROBE_TIMEOUT_MS: u64 = 5000;

/// Environment variable name for overriding the probe timeout in milliseconds.
pub const ENV_PROBE_TIMEOUT_MS: &str = "CANON_CLERK_PROBE_TIMEOUT_MS";

/// Domain input parameters for executing the `probe` Caseload DAG stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOptions {
    /// Google Gemini API key used to authenticate `google:*` probe requests.
    pub api_key: String,
    /// Optional TypeSafe AI API key used to authenticate `typesafe:*` probe requests.
    pub typesafe_api_key: Option<String>,
    /// Per-endpoint request timeout in milliseconds (defaults to [`DEFAULT_PROBE_TIMEOUT_MS`]).
    pub timeout_ms: u64,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            typesafe_api_key: None,
            timeout_ms: DEFAULT_PROBE_TIMEOUT_MS,
        }
    }
}

/// Pre-flight domain errors when `execute_probe` cannot be initiated due to missing prerequisites or invalid options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// Incoming `Caseload` is missing the `.config` prerequisite namespace.
    MissingConfig,
    /// Required Google Gemini API key in `ProbeOptions` is empty or whitespace.
    MissingApiKey,
    /// Required TypeSafe AI API key in `ProbeOptions` is missing, empty, or whitespace.
    MissingTypesafeApiKey,
    /// Timeout budget in `ProbeOptions` is zero (`0ms`).
    InvalidTimeout { timeout_ms: u64 },
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingConfig => write!(
                f,
                "Cannot execute `probe` without a resolved `.config` namespace on the Caseload.\n  \
                 Hint: Run `canon-clerk configure` first or invoke `canon-clerk probe` directly to telescope `[configure, probe]`."
            ),
            Self::MissingApiKey => write!(
                f,
                "Missing required Google Gemini API key (`{ENV_GEMINI_API_KEY}`) for endpoint probing.\n  \
                 Hint: Export `{ENV_GEMINI_API_KEY}=\"<your-api-key>\"`, configure \
                 `providers.google.apiKey` in `~/.config/canon-clerk/config.json` (mode `0600`), \
                 or pass `--gemini-api-key <KEY>`."
            ),
            Self::MissingTypesafeApiKey => write!(
                f,
                "Missing required TypeSafe AI API key (`{ENV_TYPESAFE_API_KEY}`) for endpoint probing.\n  \
                 Hint: Export `{ENV_TYPESAFE_API_KEY}=\"<your-api-key>\"`, configure \
                 `providers.typesafe.apiKey` in `~/.config/canon-clerk/config.json` (mode `0600`), \
                 or pass `--typesafe-api-key <KEY>`."
            ),
            Self::InvalidTimeout { timeout_ms } => write!(
                f,
                "Invalid probe timeout `{timeout_ms}ms`: timeout must be greater than 0ms.\n  \
                 Hint: Pass a positive millisecond timeout via `--timeout <MS>` (default: `{DEFAULT_PROBE_TIMEOUT_MS}`)."
            ),
        }
    }
}

impl std::error::Error for ProbeError {}

/// Hexagonal port for executing a live or simulated connectivity health probe against a model endpoint.
pub trait EndpointProber: Send + Sync {
    /// Probes a single canonical model specifier (e.g. `"google:gemini-4-hydrogen"`)
    /// using the provided API key, timeout budget in milliseconds, and optional thinking overrides (`probe.md` §4).
    ///
    /// Implementations uphold the Non-Throwing Diagnostic Capture Invariant (`probe.md` §3) by
    /// returning a [`ProbeEndpointResult`] with `status: ProbeStatus::Error` and an actionable
    /// `error` message when the endpoint is unreachable, times out, or returns an HTTP error.
    fn probe_endpoint(
        &self,
        model_specifier: &str,
        api_key: &str,
        timeout_ms: u64,
        thinking_level: Option<ThinkingLevel>,
        reasoning_budget: Option<u32>,
    ) -> ProbeEndpointResult;
}

/// Executes the `probe` diagnostic stage concurrently across `.config.screener_model` and
/// `.config.auditor_model` (`std::thread::scope`) and populates `.probe` on the [`Caseload`].
///
/// Fast-forwards without issuing network calls when `caseload.probe` is already populated.
pub fn execute_probe(
    options: &ProbeOptions,
    prober: &dyn EndpointProber,
    caseload: Caseload,
) -> Result<Caseload, ProbeError> {
    if caseload.probe.is_some() {
        return Ok(caseload);
    }

    if options.timeout_ms == 0 {
        return Err(ProbeError::InvalidTimeout {
            timeout_ms: options.timeout_ms,
        });
    }

    let config = caseload.config.as_ref().ok_or(ProbeError::MissingConfig)?;
    let screener_spec = config.screener_model.clone();
    let auditor_spec = config.auditor_model.clone();
    let auditor_thinking_level = config.thinking_level;
    let auditor_reasoning_budget = config.reasoning_budget;
    let screener_key = resolve_endpoint_probe_key(&screener_spec, options)?;
    let auditor_key = resolve_endpoint_probe_key(&auditor_spec, options)?;
    let timeout_ms = options.timeout_ms;

    let (screener_result, auditor_result) = std::thread::scope(|scope| {
        let screener_ref = &screener_spec;
        let auditor_ref = &auditor_spec;
        let screener_handle = scope.spawn(move || {
            prober.probe_endpoint(screener_ref, screener_key, timeout_ms, None, None)
        });
        let auditor_handle = scope.spawn(move || {
            prober.probe_endpoint(
                auditor_ref,
                auditor_key,
                timeout_ms,
                auditor_thinking_level,
                auditor_reasoning_budget,
            )
        });

        let screener = screener_handle
            .join()
            .unwrap_or_else(|_| ProbeEndpointResult {
                status: ProbeStatus::Error,
                latency_ms: 0,
                model: screener_spec.clone(),
                error: Some(
                    "Screener probe worker thread terminated unexpectedly. Hint: Retry `canon-clerk probe`."
                        .to_string(),
                ),
            });
        let auditor = auditor_handle
            .join()
            .unwrap_or_else(|_| ProbeEndpointResult {
                status: ProbeStatus::Error,
                latency_ms: 0,
                model: auditor_spec.clone(),
                error: Some(
                    "Auditor probe worker thread terminated unexpectedly. Hint: Retry `canon-clerk probe`."
                        .to_string(),
                ),
            });

        (screener, auditor)
    });

    Ok(caseload.with_probe(CaseloadProbe {
        screener: screener_result,
        auditor: auditor_result,
    }))
}

fn resolve_endpoint_probe_key<'a>(
    model_specifier: &str,
    options: &'a ProbeOptions,
) -> Result<&'a str, ProbeError> {
    match model_provider(model_specifier) {
        LOCAL_PROVIDER => Ok(""),
        TYPESAFE_PROVIDER => options
            .typesafe_api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(ProbeError::MissingTypesafeApiKey),
        _ => {
            let key = options.api_key.trim();
            if key.is_empty() {
                Err(ProbeError::MissingApiKey)
            } else {
                Ok(key)
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct GeminiGenerateContentRequest<'a> {
    contents: Vec<GeminiContent<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GeminiGenerationConfig,
}

#[derive(Debug, Serialize)]
struct GeminiContent<'a> {
    parts: Vec<GeminiPart<'a>>,
}

#[derive(Debug, Serialize)]
struct GeminiPart<'a> {
    text: &'a str,
}

#[derive(Debug, Serialize)]
struct GeminiGenerationConfig {
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
    temperature: f32,
    #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
    thinking_config: Option<GeminiThinkingConfig>,
}

#[derive(Debug, Deserialize)]
struct GeminiGenerateContentResponse {
    #[serde(rename = "modelVersion")]
    model_version: Option<String>,
}

/// Decodes and classifies an HTTP status code and response payload from a Gemini `:generateContent` probe.
pub fn decode_gemini_probe_response(
    status_code: u16,
    body: &str,
    model_specifier: &str,
    latency_ms: u64,
) -> ProbeEndpointResult {
    if (200..300).contains(&status_code) {
        let resolved_model = serde_json::from_str::<GeminiGenerateContentResponse>(body)
            .ok()
            .and_then(|resp| resp.model_version)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| model_specifier.to_string());

        return ProbeEndpointResult {
            status: ProbeStatus::Ok,
            latency_ms,
            model: resolved_model,
            error: None,
        };
    }

    let upstream_detail = extract_gemini_error_detail(body);
    let diagnostic = classify_gemini_http_error(status_code, model_specifier, &upstream_detail);
    ProbeEndpointResult {
        status: ProbeStatus::Error,
        latency_ms,
        model: model_specifier.to_string(),
        error: Some(diagnostic),
    }
}

/// Live Google Gemini HTTPS implementation of [`EndpointProber`] (`POST /v1beta/models/{model}:generateContent`).
#[derive(Debug, Clone)]
pub struct GeminiEndpointProber {
    base_url: String,
    client: reqwest::Client,
}

impl Default for GeminiEndpointProber {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiEndpointProber {
    /// Constructs a new live [`GeminiEndpointProber`] targeting the default Google Generative Language API endpoint.
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_GEMINI_BASE_URL)
    }

    /// Constructs a [`GeminiEndpointProber`] with a custom base URL.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self::with_client_and_base_url(build_pooled_http_client(), base_url)
    }

    /// Constructs a [`GeminiEndpointProber`] with a custom [`reqwest::Client`] and base URL.
    pub fn with_client_and_base_url(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client,
        }
    }
}

impl EndpointProber for GeminiEndpointProber {
    fn probe_endpoint(
        &self,
        model_specifier: &str,
        api_key: &str,
        timeout_ms: u64,
        thinking_level: Option<ThinkingLevel>,
        reasoning_budget: Option<u32>,
    ) -> ProbeEndpointResult {
        let payload = GeminiGenerateContentRequest {
            contents: vec![GeminiContent {
                parts: vec![GeminiPart { text: "ping" }],
            }],
            generation_config: GeminiGenerationConfig {
                max_output_tokens: 16,
                temperature: 0.0,
                thinking_config: build_probe_thinking_config(
                    model_specifier,
                    thinking_level,
                    reasoning_budget,
                ),
            },
        };

        match post_gemini_json(
            &self.client,
            &self.base_url,
            model_specifier,
            api_key,
            timeout_ms,
            &payload,
        ) {
            Ok(outcome) => decode_gemini_probe_response(
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

/// Composite live [`EndpointProber`] routing generative `google:*` specifiers to [`GeminiEndpointProber`],
/// `google:gemini-decision-*` specifiers to [`GeminiDecisionsEndpointProber`],
/// and `typesafe:*` specifiers to [`TypesafeEndpointProber`].
#[derive(Debug, Clone)]
pub struct MultiProviderEndpointProber {
    gemini: GeminiEndpointProber,
    gemini_decisions: GeminiDecisionsEndpointProber,
    typesafe: TypesafeEndpointProber,
    local: LocalEndpointProber,
}

impl Default for MultiProviderEndpointProber {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiProviderEndpointProber {
    /// Constructs a new [`MultiProviderEndpointProber`] targeting default Gemini and TypeSafe endpoints.
    pub fn new() -> Self {
        let client = build_pooled_http_client();
        Self {
            gemini: GeminiEndpointProber::with_client_and_base_url(
                client.clone(),
                DEFAULT_GEMINI_BASE_URL,
            ),
            gemini_decisions: GeminiDecisionsEndpointProber::with_client_and_base_url(
                client.clone(),
                DEFAULT_GEMINI_BASE_URL,
            ),
            typesafe: TypesafeEndpointProber::new(),
            local: LocalEndpointProber::with_client_and_base_url(
                client,
                crate::configure::DEFAULT_LOCAL_DECISION_ENDPOINT,
            ),
        }
    }

    /// Constructs a [`MultiProviderEndpointProber`] targeting custom base URLs (for hermetic loopback tests).
    pub fn with_base_urls(
        gemini_base_url: impl Into<String>,
        typesafe_base_url: impl Into<String>,
    ) -> Self {
        let gemini_base = gemini_base_url.into();
        let client = build_pooled_http_client();
        Self {
            gemini: GeminiEndpointProber::with_client_and_base_url(
                client.clone(),
                gemini_base.clone(),
            ),
            gemini_decisions: GeminiDecisionsEndpointProber::with_client_and_base_url(
                client.clone(),
                gemini_base,
            ),
            typesafe: TypesafeEndpointProber::with_base_url(typesafe_base_url),
            local: LocalEndpointProber::with_client_and_base_url(
                client,
                crate::configure::DEFAULT_LOCAL_DECISION_ENDPOINT,
            ),
        }
    }

    /// Overrides the localhost decision endpoint probed for `local:*` model specifiers.
    pub fn with_local_endpoint(mut self, local_base_url: impl Into<String>) -> Self {
        self.local = LocalEndpointProber::with_base_url(local_base_url);
        self
    }
}

impl EndpointProber for MultiProviderEndpointProber {
    fn probe_endpoint(
        &self,
        model_specifier: &str,
        api_key: &str,
        timeout_ms: u64,
        thinking_level: Option<ThinkingLevel>,
        reasoning_budget: Option<u32>,
    ) -> ProbeEndpointResult {
        let prov = model_provider(model_specifier);
        if prov == LOCAL_PROVIDER {
            self.local.probe_endpoint(
                model_specifier,
                api_key,
                timeout_ms,
                thinking_level,
                reasoning_budget,
            )
        } else if prov == TYPESAFE_PROVIDER {
            self.typesafe.probe_endpoint(
                model_specifier,
                api_key,
                timeout_ms,
                thinking_level,
                reasoning_budget,
            )
        } else if is_discriminative_model(model_specifier) {
            self.gemini_decisions.probe_endpoint(
                model_specifier,
                api_key,
                timeout_ms,
                thinking_level,
                reasoning_budget,
            )
        } else {
            self.gemini.probe_endpoint(
                model_specifier,
                api_key,
                timeout_ms,
                thinking_level,
                reasoning_budget,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caseload::{
        CaseloadConfig, CaseloadDiscovery, CaseloadIntake, CaseloadValidation,
        DEFAULT_AUDITOR_MODEL, DEFAULT_SCREENER_MODEL,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubEndpointProber {
        responses: BTreeMap<String, ProbeEndpointResult>,
        call_count: AtomicUsize,
    }

    impl StubEndpointProber {
        fn new(responses: BTreeMap<String, ProbeEndpointResult>) -> Self {
            Self {
                responses,
                call_count: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl EndpointProber for StubEndpointProber {
        fn probe_endpoint(
            &self,
            model_specifier: &str,
            _api_key: &str,
            _timeout_ms: u64,
            _thinking_level: Option<ThinkingLevel>,
            _reasoning_budget: Option<u32>,
        ) -> ProbeEndpointResult {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.responses
                .get(model_specifier)
                .cloned()
                .unwrap_or_else(|| ProbeEndpointResult {
                    status: ProbeStatus::Ok,
                    latency_ms: 42,
                    model: model_specifier.to_string(),
                    error: None,
                })
        }
    }

    mod url_and_response_decoding {
        use super::*;

        #[test]
        fn test_strip_provider_prefix_and_build_generate_content_url() {
            assert_eq!(
                strip_provider_prefix("google:gemini-flash-lite-latest"),
                "gemini-flash-lite-latest"
            );
            assert_eq!(strip_provider_prefix("gemini-2.5-pro"), "gemini-2.5-pro");
            assert_eq!(
                build_generate_content_url(
                    "https://generativelanguage.googleapis.com/v1beta/",
                    "google:gemini-flash-lite-latest"
                ),
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-flash-lite-latest:generateContent"
            );
        }

        #[test]
        fn test_decode_200_ok_extracts_model_version_or_falls_back_to_specifier() {
            let body = r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]}}],"modelVersion":"gemini-2.5-flash-lite-preview-06-17"}"#;
            let res = decode_gemini_probe_response(200, body, DEFAULT_SCREENER_MODEL, 118);
            assert_eq!(res.status, ProbeStatus::Ok);
            assert_eq!(res.latency_ms, 118);
            assert_eq!(res.model, "gemini-2.5-flash-lite-preview-06-17");
            assert_eq!(res.error, None);

            let empty_json = r#"{}"#;
            let fallback = decode_gemini_probe_response(200, empty_json, DEFAULT_AUDITOR_MODEL, 95);
            assert_eq!(fallback.status, ProbeStatus::Ok);
            assert_eq!(fallback.model, DEFAULT_AUDITOR_MODEL);
            assert_eq!(fallback.error, None);
        }

        #[test]
        fn test_decode_http_auth_and_not_found_errors_401_403_404() {
            let auth_body = r#"{"error":{"code":401,"message":"API key not valid. Please pass a valid API key.","status":"UNAUTHENTICATED"}}"#;
            let r401 = decode_gemini_probe_response(401, auth_body, DEFAULT_SCREENER_MODEL, 55);
            assert_eq!(r401.status, ProbeStatus::Error);
            let err401 = r401.error.expect("401 error message");
            assert!(err401.contains("HTTP 401"));
            assert!(err401.contains("API key not valid"));
            assert!(err401.contains("Hint:"));

            let r403 =
                decode_gemini_probe_response(403, "Permission denied", DEFAULT_AUDITOR_MODEL, 61);
            assert_eq!(r403.status, ProbeStatus::Error);
            assert!(r403.error.expect("403").contains("HTTP 403"));

            let r404 = decode_gemini_probe_response(
                404,
                r#"{"error":{"message":"models/gemini-nonexistent is not found"}}"#,
                "google:gemini-nonexistent",
                40,
            );
            assert_eq!(r404.status, ProbeStatus::Error);
            let err404 = r404.error.expect("404 error");
            assert!(err404.contains("HTTP 404"));
            assert!(err404.contains("--screener-model"));
        }

        #[test]
        fn test_probe_thinking_config_defaults_gemini_4_to_low_and_omits_legacy_when_unspecified() {
            assert_eq!(
                build_probe_thinking_config("google:gemini-4-carbon", None, None),
                Some(GeminiThinkingConfig {
                    include_thoughts: false,
                    thinking_level: Some("LOW"),
                    thinking_budget: None,
                })
            );
            assert_eq!(
                build_probe_thinking_config(
                    "google:gemini-4-carbon",
                    Some(ThinkingLevel::Medium),
                    None
                ),
                Some(GeminiThinkingConfig {
                    include_thoughts: false,
                    thinking_level: Some("MEDIUM"),
                    thinking_budget: None,
                })
            );
            assert_eq!(
                build_probe_thinking_config("google:gemini-pro-latest", None, None),
                None
            );
        }

        #[test]
        fn test_decode_http_rate_limit_and_server_errors_429_5xx() {
            let r429 = decode_gemini_probe_response(
                429,
                r#"{"error":{"message":"Resource has been exhausted (e.g. check quota)."}}"#,
                DEFAULT_AUDITOR_MODEL,
                77,
            );
            assert_eq!(r429.status, ProbeStatus::Error);
            let err429 = r429.error.expect("429 error");
            assert!(err429.contains("HTTP 429"));
            assert!(err429.contains("quota"));

            let r503 = decode_gemini_probe_response(
                503,
                "Service Unavailable",
                DEFAULT_AUDITOR_MODEL,
                310,
            );
            assert_eq!(r503.status, ProbeStatus::Error);
            let err503 = r503.error.expect("503 error");
            assert!(err503.contains("HTTP 503"));
            assert!(err503.contains("Hint:"));
        }
    }

    mod concurrent_execution_and_invariants {
        use super::*;

        #[test]
        fn test_execute_probe_populates_both_endpoints_concurrently() {
            let mut responses = BTreeMap::new();
            responses.insert(
                DEFAULT_SCREENER_MODEL.to_string(),
                ProbeEndpointResult {
                    status: ProbeStatus::Ok,
                    latency_ms: 110,
                    model: "gemini-2.5-flash-lite".to_string(),
                    error: None,
                },
            );
            responses.insert(
                DEFAULT_AUDITOR_MODEL.to_string(),
                ProbeEndpointResult {
                    status: ProbeStatus::Ok,
                    latency_ms: 245,
                    model: "gemini-2.5-pro".to_string(),
                    error: None,
                },
            );
            let prober = StubEndpointProber::new(responses);
            let caseload = Caseload::new().with_config(CaseloadConfig::default());
            let opts = ProbeOptions {
                api_key: "synthetic-test-key".to_string(),
                typesafe_api_key: None,
                timeout_ms: DEFAULT_PROBE_TIMEOUT_MS,
            };

            let result = execute_probe(&opts, &prober, caseload).expect("execute_probe");
            assert_eq!(prober.calls(), 2);
            let probe = result.probe.expect("probe populated");
            assert!(probe.is_healthy());
            assert_eq!(probe.screener.latency_ms, 110);
            assert_eq!(probe.screener.model, "gemini-2.5-flash-lite");
            assert_eq!(probe.auditor.latency_ms, 245);
            assert_eq!(probe.auditor.model, "gemini-2.5-pro");
        }

        #[test]
        fn test_non_throwing_capture_records_partial_and_full_endpoint_failures_on_caseload() {
            let mut responses = BTreeMap::new();
            responses.insert(
                DEFAULT_SCREENER_MODEL.to_string(),
                ProbeEndpointResult {
                    status: ProbeStatus::Ok,
                    latency_ms: 95,
                    model: "gemini-2.5-flash-lite".to_string(),
                    error: None,
                },
            );
            responses.insert(
                DEFAULT_AUDITOR_MODEL.to_string(),
                ProbeEndpointResult {
                    status: ProbeStatus::Error,
                    latency_ms: 5001,
                    model: DEFAULT_AUDITOR_MODEL.to_string(),
                    error: Some(
                        "Request to `google:gemini-pro-latest` timed out after 5000ms. Hint: Increase `--timeout <MS>`."
                            .to_string(),
                    ),
                },
            );
            let prober = StubEndpointProber::new(responses);
            let caseload = Caseload::new().with_config(CaseloadConfig::default());
            let opts = ProbeOptions {
                api_key: "synthetic-test-key".to_string(),
                typesafe_api_key: None,
                timeout_ms: DEFAULT_PROBE_TIMEOUT_MS,
            };

            let enriched =
                execute_probe(&opts, &prober, caseload).expect("non-throwing capture must succeed");
            let probe = enriched.probe.expect("probe namespace populated");
            assert!(!probe.is_healthy());
            assert_eq!(probe.screener.status, ProbeStatus::Ok);
            assert_eq!(probe.auditor.status, ProbeStatus::Error);
            assert!(
                probe
                    .auditor
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("timed out")
            );
        }

        #[test]
        fn test_fast_forwards_when_probe_namespace_already_populated() {
            let existing_probe = CaseloadProbe {
                screener: ProbeEndpointResult {
                    status: ProbeStatus::Ok,
                    latency_ms: 50,
                    model: DEFAULT_SCREENER_MODEL.to_string(),
                    error: None,
                },
                auditor: ProbeEndpointResult {
                    status: ProbeStatus::Ok,
                    latency_ms: 80,
                    model: DEFAULT_AUDITOR_MODEL.to_string(),
                    error: None,
                },
            };
            let caseload = Caseload::new()
                .with_config(CaseloadConfig::default())
                .with_probe(existing_probe.clone());
            let prober = StubEndpointProber::new(BTreeMap::new());
            let opts = ProbeOptions::default();

            let result = execute_probe(&opts, &prober, caseload).expect("fast-forward");
            assert_eq!(prober.calls(), 0);
            assert_eq!(result.probe, Some(existing_probe));
        }

        #[test]
        fn test_preserves_upstream_namespaces_and_validates_prerequisites() {
            let prober = StubEndpointProber::new(BTreeMap::new());

            let no_cfg = Caseload::new();
            let err_cfg = execute_probe(
                &ProbeOptions {
                    api_key: "key".to_string(),
                    typesafe_api_key: None,
                    timeout_ms: 1000,
                },
                &prober,
                no_cfg,
            )
            .expect_err("missing config must error");
            assert_eq!(err_cfg, ProbeError::MissingConfig);
            assert!(err_cfg.to_string().contains("Hint:"));

            let with_cfg = Caseload::new().with_config(CaseloadConfig::default());
            let err_key = execute_probe(
                &ProbeOptions {
                    api_key: "   ".to_string(),
                    typesafe_api_key: None,
                    timeout_ms: 1000,
                },
                &prober,
                with_cfg.clone(),
            )
            .expect_err("blank key must error");
            assert_eq!(err_key, ProbeError::MissingApiKey);

            let err_timeout = execute_probe(
                &ProbeOptions {
                    api_key: "key".to_string(),
                    typesafe_api_key: None,
                    timeout_ms: 0,
                },
                &prober,
                with_cfg,
            )
            .expect_err("zero timeout must error");
            assert_eq!(err_timeout, ProbeError::InvalidTimeout { timeout_ms: 0 });

            let full_upstream = Caseload::new()
                .with_intake(CaseloadIntake {
                    pr_title: Some("feat: probe".to_string()),
                    ..CaseloadIntake::default()
                })
                .with_discovery(CaseloadDiscovery::default())
                .with_validation(CaseloadValidation::default())
                .with_config(CaseloadConfig::default());

            let enriched = execute_probe(
                &ProbeOptions {
                    api_key: "valid-key".to_string(),
                    typesafe_api_key: None,
                    timeout_ms: 2500,
                },
                &prober,
                full_upstream.clone(),
            )
            .expect("enrich full caseload");
            assert_eq!(enriched.intake, full_upstream.intake);
            assert_eq!(enriched.discovery, full_upstream.discovery);
            assert_eq!(enriched.validation, full_upstream.validation);
            assert_eq!(enriched.config, full_upstream.config);
            assert!(enriched.probe.is_some());

            // Hybrid screener=typesafe:jev-latest + auditor=google:gemini-4-carbon requires typesafe_api_key
            let hybrid_cfg = Caseload::new().with_config(CaseloadConfig {
                screener_model: "typesafe:jev-latest".to_string(),
                auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
                thinking_level: None,
                reasoning_budget: None,
            });
            let err_ts = execute_probe(
                &ProbeOptions {
                    api_key: "valid-google-key".to_string(),
                    typesafe_api_key: None,
                    timeout_ms: 2500,
                },
                &prober,
                hybrid_cfg.clone(),
            )
            .expect_err("missing typesafe key must error");
            assert_eq!(err_ts, ProbeError::MissingTypesafeApiKey);

            let ok_hybrid = execute_probe(
                &ProbeOptions {
                    api_key: "valid-google-key".to_string(),
                    typesafe_api_key: Some("valid-typesafe-key".to_string()),
                    timeout_ms: 2500,
                },
                &prober,
                hybrid_cfg,
            )
            .expect("hybrid probe with both keys");
            assert!(ok_hybrid.probe.expect("probe").is_healthy());
        }

        #[test]
        fn test_multi_provider_prober_routes_gemini_decision_to_decisions_and_carbon_to_generate_content()
         {
            use std::io::{Read, Write};
            use std::net::TcpListener;
            use std::sync::{Arc, Mutex};
            use std::thread;
            use std::time::Duration;

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("local addr");
            let seen_paths = Arc::new(Mutex::new(Vec::new()));
            let seen_for_server = Arc::clone(&seen_paths);

            let server = thread::spawn(move || {
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().expect("accept probe connection");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .expect("read timeout");
                    let mut raw = Vec::new();
                    let mut buf = [0_u8; 4096];
                    let head = loop {
                        let n = stream.read(&mut buf).expect("read");
                        assert!(n > 0);
                        raw.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&raw);
                        if let Some((h, body)) = text.split_once("\r\n\r\n") {
                            let expected_len = h
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if body.len() >= expected_len {
                                break h.to_string();
                            }
                        }
                    };
                    let req_line = head.lines().next().unwrap_or("").to_string();
                    seen_for_server.lock().unwrap().push(req_line.clone());

                    let resp_body = if req_line.contains("/v1beta/decisions") {
                        r#"{"model":"gemini-decision-1","answers":{"ping":{"type":"noul","noul":0.99}}}"#
                    } else {
                        r#"{"modelVersion":"gemini-4-carbon"}"#
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    stream.write_all(resp.as_bytes()).expect("write");
                    stream.flush().expect("flush");
                }
            });

            let prober = MultiProviderEndpointProber::with_base_urls(
                format!("http://{addr}/v1beta"),
                format!("http://{addr}/v1"),
            );
            let out = execute_probe(
                &ProbeOptions {
                    api_key: "synthetic-gemini-key".to_string(),
                    typesafe_api_key: None,
                    timeout_ms: 2000,
                },
                &prober,
                Caseload::new().with_config(CaseloadConfig::default()),
            )
            .expect("live loopback probe succeeds");

            server.join().expect("server joined");
            let probe = out.probe.expect("probe populated");
            assert!(probe.is_healthy());
            assert_eq!(probe.screener.model, "gemini-decision-1");
            assert_eq!(probe.auditor.model, "gemini-4-carbon");

            let mut paths = seen_paths.lock().unwrap().clone();
            paths.sort();
            assert_eq!(
                paths,
                vec![
                    "POST /v1beta/decisions HTTP/1.1".to_string(),
                    "POST /v1beta/models/gemini-4-carbon:generateContent HTTP/1.1".to_string(),
                ]
            );
        }
    }
}
