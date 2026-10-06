use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Healthy,
    Unreachable,
    AuthError,
    RateLimited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub status: ProbeStatus,
    pub latency_ms: u64,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("Network connection error: {0}")]
    Network(String),
    #[error("Authentication failed: {0}")]
    Auth(String),
    #[error("Provider timed out after {0:?}")]
    Timeout(Duration),
}

#[allow(async_fn_in_trait)]
pub trait ProviderClient: Send + Sync {
    async fn probe(&self) -> Result<ProbeResult, ProbeError>;
}

// ---------------------------------------------------------
// Live Google / Gemini Provider Client
// ---------------------------------------------------------

#[derive(Debug, Serialize)]
struct GeminiGenerateContentRequest<'a> {
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContentRequest<'a>>,
    contents: Vec<GeminiContentRequest<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GeminiGenerationConfig<'a>,
}

#[derive(Debug, Serialize)]
struct GeminiContentRequest<'a> {
    parts: Vec<GeminiPartRequest<'a>>,
}

#[derive(Debug, Serialize)]
struct GeminiPartRequest<'a> {
    text: &'a str,
}

#[derive(Debug, Serialize)]
struct GeminiGenerationConfig<'a> {
    #[serde(rename = "responseMimeType")]
    response_mime_type: &'a str,
}

#[derive(Debug, Deserialize)]
struct GeminiGenerateContentResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidateResponse>,
    #[serde(rename = "modelVersion")]
    model_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidateResponse {
    content: Option<GeminiContentResponse>,
}

#[derive(Debug, Deserialize)]
struct GeminiContentResponse {
    #[serde(default)]
    parts: Vec<GeminiPartResponse>,
}

#[derive(Debug, Deserialize)]
struct GeminiPartResponse {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiErrorEnvelope {
    error: Option<GeminiErrorDetails>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiErrorDetails {
    code: Option<u16>,
    message: Option<String>,
    status: Option<String>,
}

pub struct GoogleProviderClient {
    pub api_key: String,
    pub base_url: Option<String>,
    pub model: String,
    client: reqwest::Client,
}

impl GoogleProviderClient {
    pub fn new(
        api_key: impl Into<String>,
        base_url: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url,
            model: model.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    fn normalized_model_name(&self) -> &str {
        let m = self.model.as_str();
        if let Some(stripped) = m.strip_prefix("google:") {
            stripped
        } else if let Some(stripped) = m.strip_prefix("gemini:") {
            stripped
        } else {
            m
        }
    }

    pub async fn generate_structured_json(
        &self,
        system_instruction: &str,
        prompt: &str,
    ) -> Result<String, ProbeError> {
        let raw_base = self
            .base_url
            .as_deref()
            .unwrap_or("https://generativelanguage.googleapis.com");
        let base = raw_base.trim_end_matches('/');
        let model_name = self.normalized_model_name();

        let url = format!("{}/v1beta/models/{}:generateContent", base, model_name);

        let payload = GeminiGenerateContentRequest {
            system_instruction: Some(GeminiContentRequest {
                parts: vec![GeminiPartRequest {
                    text: system_instruction,
                }],
            }),
            contents: vec![GeminiContentRequest {
                parts: vec![GeminiPartRequest {
                    text: prompt,
                }],
            }],
            generation_config: GeminiGenerationConfig {
                response_mime_type: "application/json",
            },
        };

        let response = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| ProbeError::Network(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProbeError::Network(format!("Google API HTTP {}: {}", status, text)));
        }

        let parsed: GeminiGenerateContentResponse = response
            .json()
            .await
            .map_err(|e| ProbeError::Network(format!("Failed to parse Google API response: {}", e)))?;

        let first_text = parsed
            .candidates
            .into_iter()
            .next()
            .and_then(|c| c.content)
            .and_then(|c| c.parts.into_iter().next())
            .and_then(|p| p.text)
            .ok_or_else(|| ProbeError::Network("No text candidate returned by Gemini".to_string()))?;

        Ok(first_text)
    }
}

impl ProviderClient for GoogleProviderClient {
    async fn probe(&self) -> Result<ProbeResult, ProbeError> {
        let raw_base = self
            .base_url
            .as_deref()
            .unwrap_or("https://generativelanguage.googleapis.com");
        let base = raw_base.trim_end_matches('/');
        let model_name = self.normalized_model_name();

        let url = format!("{}/v1beta/models/{}:generateContent", base, model_name);

        let payload = GeminiGenerateContentRequest {
            system_instruction: None,
            contents: vec![GeminiContentRequest {
                parts: vec![GeminiPartRequest {
                    text: "Respond with a JSON object containing \"ok\": true to verify connectivity.",
                }],
            }],
            generation_config: GeminiGenerationConfig {
                response_mime_type: "application/json",
            },
        };

        let start = Instant::now();
        let resp_result = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await;
        let elapsed = start.elapsed().as_millis() as u64;

        let response = match resp_result {
            Ok(r) => r,
            Err(e) => {
                let status = if e.is_timeout() {
                    ProbeStatus::Unreachable
                } else {
                    ProbeStatus::Unreachable
                };
                return Ok(ProbeResult {
                    status,
                    latency_ms: elapsed,
                    provider: "google".to_string(),
                    model: self.model.clone(),
                    message: Some(e.to_string()),
                });
            }
        };

        let http_status = response.status();
        let status_code = http_status.as_u16();

        if http_status.is_success() {
            let parsed: Result<GeminiGenerateContentResponse, _> = response.json().await;
            let resolved_model = match parsed {
                Ok(gemini_resp) => gemini_resp.model_version.unwrap_or_else(|| self.model.clone()),
                Err(_) => self.model.clone(),
            };

            Ok(ProbeResult {
                status: ProbeStatus::Healthy,
                latency_ms: elapsed,
                provider: "google".to_string(),
                model: resolved_model,
                message: Some("Reachable (OK)".to_string()),
            })
        } else {
            let error_text = response.text().await.unwrap_or_default();
            let parsed_error: Option<GeminiErrorEnvelope> = serde_json::from_str(&error_text).ok();

            let err_msg = parsed_error
                .and_then(|e| e.error)
                .and_then(|d| d.message)
                .unwrap_or_else(|| format!("HTTP {}: {}", status_code, error_text));

            let probe_status = match status_code {
                401 | 403 => ProbeStatus::AuthError,
                429 => ProbeStatus::RateLimited,
                _ => ProbeStatus::Unreachable,
            };

            Ok(ProbeResult {
                status: probe_status,
                latency_ms: elapsed,
                provider: "google".to_string(),
                model: self.model.clone(),
                message: Some(err_msg),
            })
        }
    }
}

// ---------------------------------------------------------
// Mock Provider Client
// ---------------------------------------------------------

pub struct MockProviderClient {
    pub provider: String,
    pub model: String,
    pub canned_status: ProbeStatus,
    pub simulated_latency: Duration,
    pub error_message: Option<String>,
}

impl MockProviderClient {
    pub fn new_healthy(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            canned_status: ProbeStatus::Healthy,
            simulated_latency: Duration::from_millis(5),
            error_message: None,
        }
    }

    pub fn new_failing(
        provider: impl Into<String>,
        model: impl Into<String>,
        status: ProbeStatus,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            canned_status: status,
            simulated_latency: Duration::from_millis(5),
            error_message: Some(message.into()),
        }
    }
}

impl ProviderClient for MockProviderClient {
    async fn probe(&self) -> Result<ProbeResult, ProbeError> {
        let start = Instant::now();
        if self.simulated_latency > Duration::ZERO {
            tokio::time::sleep(self.simulated_latency).await;
        }
        let elapsed = start.elapsed().as_millis() as u64;

        Ok(ProbeResult {
            status: self.canned_status.clone(),
            latency_ms: elapsed,
            provider: self.provider.clone(),
            model: self.model.clone(),
            message: self.error_message.clone(),
        })
    }
}

impl MockProviderClient {
    pub async fn generate_structured_json(
        &self,
        system_instruction: &str,
        _prompt: &str,
    ) -> Result<String, ProbeError> {
        if system_instruction.contains("Docket") {
            Ok(r#"{"assessments":[{"canonPath":".canons/cli/cli-arguments-must-represent-primary-operands.md","colorabilitySummary":"Touches CLI positional arguments","colorabilityScore":0.85}]}"#.to_string())
        } else if system_instruction.contains("admissibility") {
            Ok(r#"{"exhibits":[{"filePath":"src/main.rs","admissibilitySummary":"Contains CLI argument parser","admissibilityScore":0.9}]}"#.to_string())
        } else {
            Ok(r#"{"complianceScore":0.95,"complianceSummary":"The CLI arguments correctly adhere to positional operand requirements.","annotations":[]}"#.to_string())
        }
    }
}

pub enum AnyProviderClient {
    Google(GoogleProviderClient),
    Mock(MockProviderClient),
}

impl ProviderClient for AnyProviderClient {
    async fn probe(&self) -> Result<ProbeResult, ProbeError> {
        match self {
            Self::Google(client) => client.probe().await,
            Self::Mock(client) => client.probe().await,
        }
    }
}

impl AnyProviderClient {
    pub async fn generate_structured_json(
        &self,
        system_instruction: &str,
        prompt: &str,
    ) -> Result<String, ProbeError> {
        match self {
            Self::Google(client) => client.generate_structured_json(system_instruction, prompt).await,
            Self::Mock(client) => client.generate_structured_json(system_instruction, prompt).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_provider_healthy_probe() {
        let client = MockProviderClient::new_healthy("gemini", "gemini-2.0-flash");
        let result = client.probe().await.unwrap();

        assert_eq!(result.status, ProbeStatus::Healthy);
        assert_eq!(result.provider, "gemini");
        assert_eq!(result.model, "gemini-2.0-flash");
        assert!(result.latency_ms >= 4);
    }

    #[tokio::test]
    async fn test_mock_provider_failing_probe() {
        let client = MockProviderClient::new_failing(
            "gemini",
            "gemini-2.0-flash",
            ProbeStatus::AuthError,
            "API key expired",
        );
        let result = client.probe().await.unwrap();

        assert_eq!(result.status, ProbeStatus::AuthError);
        assert_eq!(result.message.as_deref(), Some("API key expired"));
    }

    #[test]
    fn test_google_client_model_normalization() {
        let client = GoogleProviderClient::new("key", None, "google:gemini-2.0-flash");
        assert_eq!(client.normalized_model_name(), "gemini-2.0-flash");

        let client2 = GoogleProviderClient::new("key", None, "gemini-1.5-flash");
        assert_eq!(client2.normalized_model_name(), "gemini-1.5-flash");
    }
}
