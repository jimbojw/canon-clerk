use crate::pipeline::config_store::{self, ConfigFile};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Gemini,
    Anthropic,
    OpenAi,
    Mock,
}

impl Default for ProviderType {
    fn default() -> Self {
        Self::Gemini
    }
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gemini => write!(f, "google"),
            Self::Anthropic => write!(f, "anthropic"),
            Self::OpenAi => write!(f, "openai"),
            Self::Mock => write!(f, "mock"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: ProviderType,
    pub model_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: ProviderType::Gemini,
            model_name: "gemini-3.8-flash".to_string(),
            temperature: Some(0.2),
            max_tokens: Some(4096),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_endpoint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolvedEnvironment {
    pub model: ModelConfig,
    pub credentials: Credentials,
}

pub struct ConfigResolver;

impl ConfigResolver {
    pub fn resolve(
        cli_provider: Option<ProviderType>,
        cli_model: Option<String>,
        cli_api_key: Option<String>,
        env_vars: &HashMap<String, String>,
    ) -> ResolvedEnvironment {
        let stored_cfg = config_store::load_stored_config();
        Self::resolve_with_config(
            cli_provider,
            cli_model,
            cli_api_key,
            env_vars,
            stored_cfg.as_ref(),
        )
    }

    pub fn resolve_with_config(
        cli_provider: Option<ProviderType>,
        cli_model: Option<String>,
        cli_api_key: Option<String>,
        env_vars: &HashMap<String, String>,
        stored_config: Option<&ConfigFile>,
    ) -> ResolvedEnvironment {
        let provider = cli_provider.unwrap_or_else(|| {
            if env_vars.contains_key("ANTHROPIC_API_KEY") {
                ProviderType::Anthropic
            } else if env_vars.contains_key("OPENAI_API_KEY") {
                ProviderType::OpenAi
            } else if env_vars.contains_key("MOCK_PROVIDER") {
                ProviderType::Mock
            } else if let Some(cfg) = stored_config {
                if cfg.get_provider_key("google").is_some() || cfg.get_provider_key("gemini").is_some() {
                    ProviderType::Gemini
                } else if cfg.get_provider_key("anthropic").is_some() {
                    ProviderType::Anthropic
                } else if cfg.get_provider_key("openai").is_some() {
                    ProviderType::OpenAi
                } else {
                    ProviderType::Gemini
                }
            } else {
                ProviderType::Gemini
            }
        });

        let default_model = match provider {
            ProviderType::Gemini => "gemini-3.8-flash",
            ProviderType::Anthropic => "claude-3-5-sonnet",
            ProviderType::OpenAi => "gpt-4o",
            ProviderType::Mock => "mock-model-v1",
        };

        let model_name = cli_model.unwrap_or_else(|| {
            if let Some(env_m) = env_vars.get("CANON_CLERK_MODEL") {
                env_m.clone()
            } else if let Some(cfg) = stored_config {
                cfg.screener_model
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| default_model.to_string())
            } else {
                default_model.to_string()
            }
        });

        let api_key = cli_api_key.or_else(|| match provider {
            ProviderType::Gemini => env_vars
                .get("GEMINI_API_KEY")
                .or_else(|| env_vars.get("GOOGLE_API_KEY"))
                .cloned()
                .or_else(|| stored_config.and_then(|c| c.get_provider_key("google")).map(String::from)),
            ProviderType::Anthropic => env_vars
                .get("ANTHROPIC_API_KEY")
                .cloned()
                .or_else(|| stored_config.and_then(|c| c.get_provider_key("anthropic")).map(String::from)),
            ProviderType::OpenAi => env_vars
                .get("OPENAI_API_KEY")
                .cloned()
                .or_else(|| stored_config.and_then(|c| c.get_provider_key("openai")).map(String::from)),
            ProviderType::Mock => Some("mock-key".to_string()),
        });

        let api_endpoint = env_vars.get("CANON_CLERK_API_ENDPOINT").cloned().or_else(|| {
            stored_config.and_then(|c| c.get_provider_base_url(&provider.to_string())).map(String::from)
        });

        ResolvedEnvironment {
            model: ModelConfig {
                provider,
                model_name,
                temperature: Some(0.2),
                max_tokens: Some(4096),
            },
            credentials: Credentials {
                api_key,
                api_endpoint,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_resolution() {
        let env_vars = HashMap::new();
        let resolved = ConfigResolver::resolve_with_config(None, None, None, &env_vars, None);
        assert_eq!(resolved.model.provider, ProviderType::Gemini);
        assert_eq!(resolved.model.model_name, "gemini-3.8-flash");
        assert_eq!(resolved.credentials.api_key, None);
    }

    #[test]
    fn test_stored_config_resolution() {
        let mut providers = HashMap::new();
        providers.insert(
            "google".to_string(),
            crate::pipeline::config_store::ProviderConfig {
                api_key: Some("stored-google-key".to_string()),
                base_url: Some("https://proxy.example.com".to_string()),
            },
        );
        let cfg = ConfigFile {
            providers,
            screener_model: Some(serde_json::json!("gemini-1.5-pro")),
            auditor_model: None,
        };

        let env_vars = HashMap::new();
        let resolved = ConfigResolver::resolve_with_config(None, None, None, &env_vars, Some(&cfg));
        assert_eq!(resolved.model.provider, ProviderType::Gemini);
        assert_eq!(resolved.model.model_name, "gemini-1.5-pro");
        assert_eq!(resolved.credentials.api_key.as_deref(), Some("stored-google-key"));
        assert_eq!(resolved.credentials.api_endpoint.as_deref(), Some("https://proxy.example.com"));
    }

    #[test]
    fn test_env_var_override_over_stored_config() {
        let mut providers = HashMap::new();
        providers.insert(
            "google".to_string(),
            crate::pipeline::config_store::ProviderConfig {
                api_key: Some("stored-key".to_string()),
                base_url: None,
            },
        );
        let cfg = ConfigFile {
            providers,
            ..Default::default()
        };

        let mut env_vars = HashMap::new();
        env_vars.insert("GEMINI_API_KEY".to_string(), "env-override-key".to_string());

        let resolved = ConfigResolver::resolve_with_config(None, None, None, &env_vars, Some(&cfg));
        assert_eq!(resolved.credentials.api_key.as_deref(), Some("env-override-key"));
    }
}
