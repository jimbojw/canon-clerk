use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    #[serde(rename = "baseURL")]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(rename = "screenerModel")]
    pub screener_model: Option<serde_json::Value>,
    #[serde(rename = "auditorModel")]
    pub auditor_model: Option<serde_json::Value>,
}

impl ConfigFile {
    pub fn get_provider_key(&self, provider: &str) -> Option<&str> {
        let normalized = provider.to_lowercase();
        // Check exact match, or 'google'/'gemini' aliases
        let p_cfg = self.providers.get(&normalized).or_else(|| {
            if normalized == "gemini" {
                self.providers.get("google")
            } else if normalized == "google" {
                self.providers.get("gemini")
            } else {
                None
            }
        })?;

        p_cfg.api_key.as_deref().filter(|k| !k.trim().is_empty())
    }

    pub fn get_provider_base_url(&self, provider: &str) -> Option<&str> {
        let normalized = provider.to_lowercase();
        let p_cfg = self.providers.get(&normalized).or_else(|| {
            if normalized == "gemini" {
                self.providers.get("google")
            } else if normalized == "google" {
                self.providers.get("gemini")
            } else {
                None
            }
        })?;

        p_cfg.base_url.as_deref().filter(|u| !u.trim().is_empty())
    }
}

pub fn get_default_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let p = PathBuf::from(xdg).join("canon-clerk").join("config.json");
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".config").join("canon-clerk").join("config.json");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

pub fn load_config_from_path(path: &Path) -> Option<ConfigFile> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn load_stored_config() -> Option<ConfigFile> {
    let path = get_default_config_path()?;
    load_config_from_path(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_config() {
        let raw = r#"{
            "providers": {
                "google": {
                    "apiKey": "test-key-123",
                    "baseURL": "https://custom.googleapis.com"
                }
            }
        }"#;

        let cfg: ConfigFile = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.get_provider_key("google"), Some("test-key-123"));
        assert_eq!(cfg.get_provider_key("gemini"), Some("test-key-123"));
        assert_eq!(cfg.get_provider_base_url("google"), Some("https://custom.googleapis.com"));
    }
}
