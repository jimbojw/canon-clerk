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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_provider_healthy_probe() {
        let client = MockProviderClient::new_healthy("gemini", "gemini-2.5-pro");
        let result = client.probe().await.unwrap();

        assert_eq!(result.status, ProbeStatus::Healthy);
        assert_eq!(result.provider, "gemini");
        assert_eq!(result.model, "gemini-2.5-pro");
        assert!(result.latency_ms >= 4);
    }

    #[tokio::test]
    async fn test_mock_provider_failing_probe() {
        let client = MockProviderClient::new_failing(
            "gemini",
            "gemini-2.5-pro",
            ProbeStatus::AuthError,
            "API key expired",
        );
        let result = client.probe().await.unwrap();

        assert_eq!(result.status, ProbeStatus::AuthError);
        assert_eq!(result.message.as_deref(), Some("API key expired"));
    }
}

