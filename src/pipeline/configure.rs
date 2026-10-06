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
            model_name: "gemini-2.5-pro".to_string(),
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
        let provider = cli_provider.unwrap_or_else(|| {
            if env_vars.contains_key("ANTHROPIC_API_KEY") {
                ProviderType::Anthropic
            } else if env_vars.contains_key("OPENAI_API_KEY") {
                ProviderType::OpenAi
            } else if env_vars.contains_key("MOCK_PROVIDER") {
                ProviderType::Mock
            } else {
                ProviderType::Gemini
            }
        });

        let default_model = match provider {
            ProviderType::Gemini => "gemini-2.5-pro",
            ProviderType::Anthropic => "claude-3-5-sonnet",
            ProviderType::OpenAi => "gpt-4o",
            ProviderType::Mock => "mock-model-v1",
        };

        let model_name = cli_model.unwrap_or_else(|| {
            env_vars
                .get("CANON_CLERK_MODEL")
                .cloned()
                .unwrap_or_else(|| default_model.to_string())
        });

        let api_key = cli_api_key.or_else(|| match provider {
            ProviderType::Gemini => env_vars
                .get("GEMINI_API_KEY")
                .or_else(|| env_vars.get("GOOGLE_API_KEY"))
                .cloned(),
            ProviderType::Anthropic => env_vars.get("ANTHROPIC_API_KEY").cloned(),
            ProviderType::OpenAi => env_vars.get("OPENAI_API_KEY").cloned(),
            ProviderType::Mock => Some("mock-key".to_string()),
        });

        let api_endpoint = env_vars.get("CANON_CLERK_API_ENDPOINT").cloned();

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
        let resolved = ConfigResolver::resolve(None, None, None, &env_vars);
        assert_eq!(resolved.model.provider, ProviderType::Gemini);
        assert_eq!(resolved.model.model_name, "gemini-2.5-pro");
        assert_eq!(resolved.credentials.api_key, None);
    }

    #[test]
    fn test_env_var_resolution() {
        let mut env_vars = HashMap::new();
        env_vars.insert("GEMINI_API_KEY".to_string(), "secret-gemini-key".to_string());
        env_vars.insert("CANON_CLERK_MODEL".to_string(), "gemini-1.5-flash".to_string());

        let resolved = ConfigResolver::resolve(None, None, None, &env_vars);
        assert_eq!(resolved.model.provider, ProviderType::Gemini);
        assert_eq!(resolved.model.model_name, "gemini-1.5-flash");
        assert_eq!(resolved.credentials.api_key.as_deref(), Some("secret-gemini-key"));
    }

    #[test]
    fn test_cli_override_precedence() {
        let mut env_vars = HashMap::new();
        env_vars.insert("GEMINI_API_KEY".to_string(), "env-key".to_string());

        let resolved = ConfigResolver::resolve(
            Some(ProviderType::Mock),
            Some("custom-model".to_string()),
            Some("cli-key".to_string()),
            &env_vars,
        );
        assert_eq!(resolved.model.provider, ProviderType::Mock);
        assert_eq!(resolved.model.model_name, "custom-model");
        assert_eq!(resolved.credentials.api_key.as_deref(), Some("cli-key"));
    }
}
