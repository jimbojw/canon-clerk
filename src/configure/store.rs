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

//! Host user-level configuration file (`~/.config/canon-clerk/config.json`) loader and security validators.

use crate::caseload::{DEFAULT_PROVIDER, TYPESAFE_PROVIDER};
use crate::configure::ConfigureError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Provider-specific authentication and transport configuration entry under `providers.<provider>`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredProviderConfig {
    /// Provider API key (`apiKey` or `api_key`).
    #[serde(alias = "apiKey", alias = "api_key")]
    pub api_key: Option<String>,
}

/// Model tier configuration entry supporting either a specifier string (`"google:gemini-pro-latest"`)
/// or a structured object (`{ "provider": "google", "model": "gemini-pro-latest" }`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum StoredTierModel {
    /// Direct model specifier or alias string.
    Specifier(String),
    /// Structured provider + model object.
    Object {
        /// Optional provider slug (defaults to `"google"`).
        provider: Option<String>,
        /// Model name or alias.
        model: String,
    },
}

impl StoredTierModel {
    /// Converts the stored tier entry into a raw model specifier string.
    pub fn to_specifier_string(&self) -> String {
        match self {
            Self::Specifier(s) => s.clone(),
            Self::Object {
                provider: Some(prov),
                model,
            } if !prov.trim().is_empty() => format!("{}:{}", prov.trim(), model.trim()),
            Self::Object { provider: _, model } => model.trim().to_string(),
        }
    }
}

/// Orthogonal user-level configuration file schema (`~/.config/canon-clerk/config.json`).
///
/// Separates `providers` authentication namespaces from consumer execution tier settings
/// per `.canons/configuration/configuration-file-formats-must-conform-to-industry-standards.md`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredConfig {
    /// Provider-keyed authentication settings (e.g. `providers.google.apiKey`).
    #[serde(default)]
    pub providers: BTreeMap<String, StoredProviderConfig>,

    /// Default screener model tier configuration (`screenerModel` or `screener_model`).
    #[serde(alias = "screenerModel", alias = "screener_model")]
    pub screener_model: Option<StoredTierModel>,

    /// Default auditor model tier configuration (`auditorModel` or `auditor_model`).
    #[serde(alias = "auditorModel", alias = "auditor_model")]
    pub auditor_model: Option<StoredTierModel>,

    /// Default reasoning budget in tokens (`reasoningBudget` or `reasoning_budget`).
    #[serde(alias = "reasoningBudget", alias = "reasoning_budget")]
    pub reasoning_budget: Option<u32>,

    /// Optional default maximum candidate canons per `apprise` screener call (`appriseBatchSize` or `apprise_batch_size`).
    #[serde(alias = "appriseBatchSize", alias = "apprise_batch_size")]
    pub apprise_batch_size: Option<usize>,

    /// Optional default maximum concurrent in-flight `apprise` screener calls (`appriseConcurrency` or `apprise_concurrency`).
    #[serde(alias = "appriseConcurrency", alias = "apprise_concurrency")]
    pub apprise_concurrency: Option<usize>,

    /// Optional default maximum candidate canons per `docket` screener call (`docketBatchSize` or `docket_batch_size`).
    #[serde(
        alias = "docketBatchSize",
        alias = "docket_batch_size",
        alias = "maxCanonsPerCall",
        alias = "max_canons_per_call"
    )]
    pub docket_batch_size: Option<usize>,

    /// Optional default maximum concurrent in-flight `docket` screener calls (`docketConcurrency` or `docket_concurrency`).
    #[serde(
        alias = "docketConcurrency",
        alias = "docket_concurrency",
        alias = "maxConcurrentCalls",
        alias = "max_concurrent_calls"
    )]
    pub docket_concurrency: Option<usize>,

    /// Optional default maximum concurrent in-flight `admit` screener calls (`admitConcurrency` or `admit_concurrency`).
    #[serde(alias = "admitConcurrency", alias = "admit_concurrency")]
    pub admit_concurrency: Option<usize>,

    /// Optional default maximum concurrent in-flight `audit` trial calls (`auditConcurrency` or `audit_concurrency`).
    #[serde(alias = "auditConcurrency", alias = "audit_concurrency")]
    pub audit_concurrency: Option<usize>,
}

impl StoredConfig {
    /// Returns the non-empty Google API key stored under `providers.google.apiKey` (or `providers.gemini.apiKey`).
    pub fn google_api_key(&self) -> Option<&str> {
        self.providers
            .get(DEFAULT_PROVIDER)
            .or_else(|| self.providers.get("gemini"))
            .and_then(|p| p.api_key.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Returns the non-empty TypeSafe AI API key stored under `providers.typesafe.apiKey`.
    pub fn typesafe_api_key(&self) -> Option<&str> {
        self.providers
            .get(TYPESAFE_PROVIDER)
            .and_then(|p| p.api_key.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Returns the stored screener model specifier if present.
    pub fn screener_specifier(&self) -> Option<String> {
        self.screener_model
            .as_ref()
            .map(StoredTierModel::to_specifier_string)
    }

    /// Returns the stored auditor model specifier if present.
    pub fn auditor_specifier(&self) -> Option<String> {
        self.auditor_model
            .as_ref()
            .map(StoredTierModel::to_specifier_string)
    }
}

/// Resolves the canonical user-level configuration file path outside the workspace:
/// - `$XDG_CONFIG_HOME/canon-clerk/config.json` when `XDG_CONFIG_HOME` is set and non-empty
/// - `$HOME/.config/canon-clerk/config.json` on Unix / macOS
/// - `%APPDATA%\canon-clerk\config.json` on Windows
pub fn resolve_default_config_path(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    if let Some(xdg) = env
        .get("XDG_CONFIG_HOME")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return Some(PathBuf::from(xdg).join("canon-clerk").join("config.json"));
    }

    if let Some(home) = env.get("HOME").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("canon-clerk")
                .join("config.json"),
        );
    }

    if let Some(appdata) = env
        .get("APPDATA")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return Some(
            PathBuf::from(appdata)
                .join("canon-clerk")
                .join("config.json"),
        );
    }

    None
}

/// Loads and validates a user-level configuration file from `config_path`.
///
/// Enforces two security invariants:
/// 1. **Workspace Boundary Guard (`credentials-must-reside-outside-workspace`):** Rejects any
///    configuration file located inside `workspace_root`.
/// 2. **POSIX Permission Guard (`credential-readers-must-reject-insecure-permissions`):** Rejects
///    any configuration file whose POSIX permissions grant group or world access (`(mode & 0o077) != 0`).
pub fn load_host_config_file(
    config_path: &Path,
    workspace_root: Option<&Path>,
) -> Result<Option<StoredConfig>, ConfigureError> {
    if let Some(ws_root) = workspace_root
        && is_path_inside_workspace(config_path, ws_root)
    {
        return Err(ConfigureError::ConfigInsideWorkspace {
            path: config_path.display().to_string(),
        });
    }

    if !config_path.exists() {
        return Ok(None);
    }

    verify_posix_permissions(config_path)?;

    let raw =
        fs::read_to_string(config_path).map_err(|err| ConfigureError::MalformedConfigFile {
            path: config_path.display().to_string(),
            message: err.to_string(),
        })?;

    let parsed: StoredConfig =
        serde_json::from_str(&raw).map_err(|err| ConfigureError::MalformedConfigFile {
            path: config_path.display().to_string(),
            message: err.to_string(),
        })?;

    Ok(Some(parsed))
}

fn is_path_inside_workspace(config_path: &Path, workspace_root: &Path) -> bool {
    let canonical_ws =
        fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let canonical_cfg = fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    canonical_cfg.starts_with(&canonical_ws)
}

#[cfg(unix)]
fn verify_posix_permissions(config_path: &Path) -> Result<(), ConfigureError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata =
        fs::metadata(config_path).map_err(|err| ConfigureError::MalformedConfigFile {
            path: config_path.display().to_string(),
            message: err.to_string(),
        })?;
    let mode = metadata.permissions().mode() & 0o777;
    if (mode & 0o077) != 0 {
        return Err(ConfigureError::InsecureConfigPermissions {
            path: config_path.display().to_string(),
            mode_octal: format!("0o{mode:03o}"),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_posix_permissions(_config_path: &Path) -> Result<(), ConfigureError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("canon-clerk-cfg-{label}-{nanos}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[cfg(unix)]
    fn write_config_with_mode(path: &Path, content: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write config");
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
    }

    mod path_and_schema {
        use super::*;

        #[test]
        fn test_resolve_default_config_path_prefers_xdg_then_home() {
            let mut env = BTreeMap::new();
            env.insert("HOME".to_string(), "/home/alice".to_string());
            assert_eq!(
                resolve_default_config_path(&env),
                Some(PathBuf::from("/home/alice/.config/canon-clerk/config.json"))
            );

            env.insert("XDG_CONFIG_HOME".to_string(), "/custom/xdg".to_string());
            assert_eq!(
                resolve_default_config_path(&env),
                Some(PathBuf::from("/custom/xdg/canon-clerk/config.json"))
            );
        }

        #[test]
        fn test_nonexistent_config_file_returns_none() {
            let dir = unique_temp_dir("missing");
            let cfg_path = dir.join("config.json");
            let loaded = load_host_config_file(&cfg_path, None).expect("missing file is ok");
            assert!(loaded.is_none());
            let _ = fs::remove_dir_all(&dir);
        }

        #[cfg(unix)]
        #[test]
        fn test_loads_valid_0600_config_file_with_orthogonal_namespaces() {
            let dir = unique_temp_dir("valid0600");
            let cfg_path = dir.join("config.json");
            let json = r#"{
                "providers": {
                    "google": {
                        "apiKey": "synthetic-stored-key-from-config-json"
                    },
                    "typesafe": {
                        "apiKey": "synthetic-stored-typesafe-key"
                    }
                },
                "screenerModel": "flash-lite",
                "auditorModel": {
                    "provider": "google",
                    "model": "gemini-2.5-pro"
                },
                "reasoningBudget": 4096,
                "appriseBatchSize": 15,
                "appriseConcurrency": 3,
                "admitConcurrency": 6,
                "auditConcurrency": 5
            }"#;
            write_config_with_mode(&cfg_path, json, 0o600);

            let loaded = load_host_config_file(&cfg_path, None)
                .expect("load valid 0600 config")
                .expect("config present");
            assert_eq!(
                loaded.google_api_key(),
                Some("synthetic-stored-key-from-config-json")
            );
            assert_eq!(
                loaded.typesafe_api_key(),
                Some("synthetic-stored-typesafe-key")
            );
            assert_eq!(loaded.screener_specifier().as_deref(), Some("flash-lite"));
            assert_eq!(
                loaded.auditor_specifier().as_deref(),
                Some("google:gemini-2.5-pro")
            );
            assert_eq!(loaded.reasoning_budget, Some(4096));
            assert_eq!(loaded.apprise_batch_size, Some(15));
            assert_eq!(loaded.apprise_concurrency, Some(3));
            assert_eq!(loaded.admit_concurrency, Some(6));
            assert_eq!(loaded.audit_concurrency, Some(5));

            let _ = fs::remove_dir_all(&dir);
        }
    }

    mod security_guards {
        use super::*;

        #[cfg(unix)]
        #[test]
        fn test_rejects_world_or_group_readable_config_file() {
            let dir = unique_temp_dir("insecure0644");
            let cfg_path = dir.join("config.json");
            write_config_with_mode(
                &cfg_path,
                r#"{"providers":{"google":{"apiKey":"secret"}}}"#,
                0o644,
            );

            let err =
                load_host_config_file(&cfg_path, None).expect_err("0o644 config must be rejected");
            assert!(matches!(
                err,
                ConfigureError::InsecureConfigPermissions { .. }
            ));
            assert!(err.to_string().contains("0o644"));
            assert!(err.to_string().contains("chmod 600"));

            let _ = fs::remove_dir_all(&dir);
        }

        #[cfg(unix)]
        #[test]
        fn test_rejects_config_file_located_inside_workspace() {
            let ws_dir = unique_temp_dir("workspace");
            let inside_path = ws_dir.join(".canon-clerk").join("config.json");
            write_config_with_mode(
                &inside_path,
                r#"{"providers":{"google":{"apiKey":"secret"}}}"#,
                0o600,
            );

            let err = load_host_config_file(&inside_path, Some(&ws_dir))
                .expect_err("workspace-resident config must be rejected");
            assert!(matches!(err, ConfigureError::ConfigInsideWorkspace { .. }));
            assert!(err.to_string().contains("Hint:"));

            let _ = fs::remove_dir_all(&ws_dir);
        }
    }
}
