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

use crate::caseload::{
    Caseload, CaseloadProbe, ProbeEndpointResult, ProbeStatus, TYPESAFE_PROVIDER,
};
use crate::configure::{ENV_GEMINI_API_KEY, ENV_TYPESAFE_API_KEY, model_provider};
use crate::provider::gemini::post_gemini_json;
pub use crate::provider::{
    DEFAULT_GEMINI_BASE_URL, DEFAULT_TYPESAFE_BASE_URL, build_generate_content_url,
    strip_provider_prefix,
};
use crate::provider::{
    TypesafeNoulQuestion, TypesafeSystemOneRequest, TypesafeSystemOneResponse,
    classify_gemini_http_error, classify_typesafe_http_error, extract_gemini_error_detail,
    extract_typesafe_error_detail, post_typesafe_json, strip_typesafe_prefix,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// Probes a single canonical model specifier (e.g. `"google:gemini-flash-lite-latest"`)
    /// using the provided API key and timeout budget in milliseconds.
    ///
    /// Implementations uphold the Non-Throwing Diagnostic Capture Invariant (`probe.md` §3) by
    /// returning a [`ProbeEndpointResult`] with `status: ProbeStatus::Error` and an actionable
    /// `error` message when the endpoint is unreachable, times out, or returns an HTTP error.
    fn probe_endpoint(
        &self,
        model_specifier: &str,
        api_key: &str,
        timeout_ms: u64,
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
    let screener_key = resolve_endpoint_probe_key(&screener_spec, options)?;
    let auditor_key = resolve_endpoint_probe_key(&auditor_spec, options)?;
    let timeout_ms = options.timeout_ms;

    let (screener_result, auditor_result) = std::thread::scope(|scope| {
        let screener_ref = &screener_spec;
        let auditor_ref = &auditor_spec;
        let screener_handle =
            scope.spawn(move || prober.probe_endpoint(screener_ref, screener_key, timeout_ms));
        let auditor_handle =
            scope.spawn(move || prober.probe_endpoint(auditor_ref, auditor_key, timeout_ms));

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
    if model_provider(model_specifier) == TYPESAFE_PROVIDER {
        options
            .typesafe_api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(ProbeError::MissingTypesafeApiKey)
    } else {
        let key = options.api_key.trim();
        if key.is_empty() {
            Err(ProbeError::MissingApiKey)
        } else {
            Ok(key)
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
}

impl Default for GeminiEndpointProber {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiEndpointProber {
    /// Constructs a new live [`GeminiEndpointProber`] targeting the default Google Generative Language API endpoint.
    pub fn new() -> Self {
        Self {
            base_url: DEFAULT_GEMINI_BASE_URL.to_string(),
        }
    }

    /// Constructs a [`GeminiEndpointProber`] with a custom base URL.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl EndpointProber for GeminiEndpointProber {
    fn probe_endpoint(
        &self,
        model_specifier: &str,
        api_key: &str,
        timeout_ms: u64,
    ) -> ProbeEndpointResult {
        let payload = GeminiGenerateContentRequest {
            contents: vec![GeminiContent {
                parts: vec![GeminiPart { text: "ping" }],
            }],
            generation_config: GeminiGenerationConfig {
                max_output_tokens: 1,
                temperature: 0.0,
            },
        };

        match post_gemini_json(
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

/// Decodes and classifies an HTTP status code and response payload from a TypeSafe AI `/v1/systemone` probe.
fn decode_typesafe_probe_response(
    status_code: u16,
    body: &str,
    model_specifier: &str,
    latency_ms: u64,
) -> ProbeEndpointResult {
    if (200..300).contains(&status_code) {
        let resolved_model = serde_json::from_str::<TypesafeSystemOneResponse>(body)
            .ok()
            .and_then(|resp| resp.model)
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

    let upstream_detail = extract_typesafe_error_detail(body);
    let diagnostic = classify_typesafe_http_error(status_code, model_specifier, &upstream_detail);
    ProbeEndpointResult {
        status: ProbeStatus::Error,
        latency_ms,
        model: model_specifier.to_string(),
        error: Some(diagnostic),
    }
}

/// Live TypeSafe AI HTTPS implementation of [`EndpointProber`] (`POST /v1/systemone`).
#[derive(Debug, Clone)]
pub struct TypesafeEndpointProber {
    base_url: String,
}

impl Default for TypesafeEndpointProber {
    fn default() -> Self {
        Self::new()
    }
}

impl TypesafeEndpointProber {
    /// Constructs a new live [`TypesafeEndpointProber`] targeting [`DEFAULT_TYPESAFE_BASE_URL`].
    pub fn new() -> Self {
        Self {
            base_url: DEFAULT_TYPESAFE_BASE_URL.to_string(),
        }
    }

    /// Constructs a [`TypesafeEndpointProber`] with a custom base URL.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl EndpointProber for TypesafeEndpointProber {
    fn probe_endpoint(
        &self,
        model_specifier: &str,
        api_key: &str,
        timeout_ms: u64,
    ) -> ProbeEndpointResult {
        let mut questions = BTreeMap::new();
        questions.insert(
            "ping".to_string(),
            TypesafeNoulQuestion::new("Is the input non-empty?"),
        );
        let payload = TypesafeSystemOneRequest {
            model: strip_typesafe_prefix(model_specifier).to_string(),
            state: "ping".to_string(),
            questions,
        };

        match post_typesafe_json(
            &self.base_url,
            model_specifier,
            api_key,
            timeout_ms,
            &payload,
        ) {
            Ok(outcome) => decode_typesafe_probe_response(
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

/// Composite live [`EndpointProber`] routing `google:*` specifiers to [`GeminiEndpointProber`]
/// and `typesafe:*` specifiers to [`TypesafeEndpointProber`].
#[derive(Debug, Clone, Default)]
pub struct MultiProviderEndpointProber {
    gemini: GeminiEndpointProber,
    typesafe: TypesafeEndpointProber,
}

impl MultiProviderEndpointProber {
    /// Constructs a new [`MultiProviderEndpointProber`] targeting default Gemini and TypeSafe endpoints.
    pub fn new() -> Self {
        Self::default()
    }
}

impl EndpointProber for MultiProviderEndpointProber {
    fn probe_endpoint(
        &self,
        model_specifier: &str,
        api_key: &str,
        timeout_ms: u64,
    ) -> ProbeEndpointResult {
        if model_provider(model_specifier) == TYPESAFE_PROVIDER {
            self.typesafe
                .probe_endpoint(model_specifier, api_key, timeout_ms)
        } else {
            self.gemini
                .probe_endpoint(model_specifier, api_key, timeout_ms)
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

            // Hybrid screener=typesafe:jev-latest + auditor=google:gemini-pro-latest requires typesafe_api_key
            let hybrid_cfg = Caseload::new().with_config(CaseloadConfig {
                screener_model: "typesafe:jev-latest".to_string(),
                auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
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
    }
}
