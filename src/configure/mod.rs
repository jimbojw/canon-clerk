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

//! Core domain execution engine for the `configure` Caseload DAG stage (`docs/architecture/nodes/configure.md`).

pub mod store;

pub use store::{
    StoredConfig, StoredProviderConfig, StoredTierModel, load_host_config_file,
    resolve_default_config_path,
};

use crate::caseload::{
    Caseload, CaseloadConfig, DEFAULT_AUDITOR_MODEL, DEFAULT_PROVIDER, DEFAULT_SCREENER_MODEL,
    TYPESAFE_PROVIDER,
};
use std::collections::BTreeMap;
use std::fmt;

/// Environment variable name for the Google Gemini API key.
pub const ENV_GEMINI_API_KEY: &str = "GEMINI_API_KEY";

/// Environment variable name for the TypeSafe AI (Jev) API key.
pub const ENV_TYPESAFE_API_KEY: &str = "TYPESAFE_API_KEY";

/// Environment variable name for overriding the screener model specifier.
pub const ENV_SCREENER_MODEL: &str = "CANON_CLERK_SCREENER_MODEL";

/// Environment variable name for overriding the auditor model specifier.
pub const ENV_AUDITOR_MODEL: &str = "CANON_CLERK_AUDITOR_MODEL";

/// Environment variable name for overriding the maximum reasoning budget in tokens.
pub const ENV_REASONING_BUDGET: &str = "CANON_CLERK_REASONING_BUDGET";

/// Canonical Google Gemini Flash model specifier expanded from the `flash` shorthand alias.
pub const DEFAULT_FLASH_MODEL: &str = "google:gemini-flash-latest";

/// Canonical TypeSafe AI Jev model specifier expanded from the `jev` shorthand alias.
pub const DEFAULT_JEV_MODEL: &str = "typesafe:jev-latest";

const JEV_MODEL_ALIAS: &str = "jev";

/// Domain input parameters, environment snapshot, and optional user config store for `configure`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigureOptions {
    /// Explicit screener model override (`--screener-model`).
    pub screener_model: Option<String>,
    /// Explicit auditor model override (`--auditor-model`).
    pub auditor_model: Option<String>,
    /// Explicit reasoning budget override in tokens (`--reasoning-budget`).
    pub reasoning_budget: Option<u32>,
    /// Explicit Google Gemini API key override (`--gemini-api-key`).
    pub gemini_api_key: Option<String>,
    /// Explicit TypeSafe AI API key override (`--typesafe-api-key`).
    pub typesafe_api_key: Option<String>,
    /// Snapshot of runtime environment variables injected by the driving adapter.
    pub env: BTreeMap<String, String>,
    /// Optional parsed user-level configuration (`~/.config/canon-clerk/config.json`).
    pub stored_config: Option<StoredConfig>,
}

impl ConfigureOptions {
    /// Returns `true` when any explicit CLI or programmatic override flag was supplied.
    pub fn has_explicit_overrides(&self) -> bool {
        self.screener_model.is_some()
            || self.auditor_model.is_some()
            || self.reasoning_budget.is_some()
            || self.gemini_api_key.is_some()
            || self.typesafe_api_key.is_some()
    }
}

/// Domain errors produced during `configure` execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigureError {
    /// Required `GEMINI_API_KEY` credential is missing from flags, environment, and user config.
    MissingApiKey,
    /// Required `TYPESAFE_API_KEY` credential is missing from flags, environment, and user config.
    MissingTypesafeApiKey,
    /// Model specifier uses an unsupported provider prefix (only `google` and `typesafe` are supported).
    UnsupportedProvider { provider: String, specifier: String },
    /// Model specifier is empty, contains whitespace, or does not name a valid supported model.
    InvalidModelSpecifier { specifier: String, reason: String },
    /// Reasoning budget environment variable cannot be parsed as a non-negative integer.
    InvalidReasoningBudget { raw: String },
    /// User configuration file has group or world access permissions on a POSIX host.
    InsecureConfigPermissions { path: String, mode_octal: String },
    /// User configuration file resides inside the repository workspace directory tree.
    ConfigInsideWorkspace { path: String },
    /// User configuration file could not be read or parsed as valid JSON.
    MalformedConfigFile { path: String, message: String },
}

impl fmt::Display for ConfigureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => write!(
                f,
                "Missing required Google Gemini API key (`{ENV_GEMINI_API_KEY}`).\n  \
                 Hint: Export `{ENV_GEMINI_API_KEY}=\"<your-api-key>\"`, configure \
                 `providers.google.apiKey` in `~/.config/canon-clerk/config.json` (mode `0600`), \
                 or pass `--gemini-api-key <KEY>`."
            ),
            Self::MissingTypesafeApiKey => write!(
                f,
                "Missing required TypeSafe AI API key (`{ENV_TYPESAFE_API_KEY}`).\n  \
                 Hint: Export `{ENV_TYPESAFE_API_KEY}=\"<your-api-key>\"`, configure \
                 `providers.typesafe.apiKey` in `~/.config/canon-clerk/config.json` (mode `0600`), \
                 or pass `--typesafe-api-key <KEY>`."
            ),
            Self::UnsupportedProvider {
                provider,
                specifier,
            } => write!(
                f,
                "Unsupported model provider `{provider}` in specifier `{specifier}`.\n  \
                 Hint: Canon Clerk supports `google` (e.g. `{DEFAULT_SCREENER_MODEL}`, `{DEFAULT_AUDITOR_MODEL}`) \
                 and `typesafe` (e.g. `{DEFAULT_JEV_MODEL}`)."
            ),
            Self::InvalidModelSpecifier { specifier, reason } => write!(
                f,
                "Invalid model specifier `{specifier}`: {reason}\n  \
                 Hint: Specify a canonical model such as `{DEFAULT_SCREENER_MODEL}`, \
                 `{DEFAULT_AUDITOR_MODEL}`, `{DEFAULT_JEV_MODEL}`, or shorthand aliases `flash-lite`, `flash`, `pro`, `jev`."
            ),
            Self::InvalidReasoningBudget { raw } => write!(
                f,
                "Invalid reasoning budget `{raw}`: expected a non-negative integer token count.\n  \
                 Hint: Pass an integer token ceiling via `--reasoning-budget <TOKENS>` \
                 or `{ENV_REASONING_BUDGET}=<TOKENS>`."
            ),
            Self::InsecureConfigPermissions { path, mode_octal } => write!(
                f,
                "Configuration file `{path}` has insecure permissions (`{mode_octal}`).\n  \
                 Hint: Restrict file access to owner-only by running: `chmod 600 \"{path}\"`."
            ),
            Self::ConfigInsideWorkspace { path } => write!(
                f,
                "Configuration file `{path}` resides inside the repository workspace.\n  \
                 Hint: Store credentials exclusively outside the repository in \
                 `~/.config/canon-clerk/config.json` (mode `0600`) or via `{ENV_GEMINI_API_KEY}` / `{ENV_TYPESAFE_API_KEY}`."
            ),
            Self::MalformedConfigFile { path, message } => write!(
                f,
                "Failed to parse configuration file `{path}`: {message}\n  \
                 Hint: Ensure `{path}` contains valid JSON matching the Canon Clerk config schema."
            ),
        }
    }
}

impl std::error::Error for ConfigureError {}

/// Extracts the canonical provider slug (`"google"`, `"typesafe"`, etc.) from a model specifier.
pub(crate) fn model_provider(specifier: &str) -> &str {
    match specifier.trim().split_once(':') {
        Some((prov, _)) if !prov.is_empty() => prov,
        _ => DEFAULT_PROVIDER,
    }
}

/// Normalizes a raw model alias or specifier into a canonical `<provider>:<model>` identifier.
///
/// Supported forms:
/// - Shorthand aliases: `flash-lite`, `flash`, `pro` (-> `google:*`), `jev` (-> `typesafe:jev-latest`)
/// - Unqualified models: `gemini-*` -> `google:gemini-*`, `jev-*` -> `typesafe:jev-*`
/// - Canonical specifiers: `google:gemini-*`, `typesafe:jev-*`
pub fn normalize_model_specifier(raw: &str) -> Result<String, ConfigureError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ConfigureError::InvalidModelSpecifier {
            specifier: raw.to_string(),
            reason: "model specifier cannot be empty.".to_string(),
        });
    }

    if trimmed.chars().any(char::is_whitespace) {
        return Err(ConfigureError::InvalidModelSpecifier {
            specifier: raw.to_string(),
            reason: "model specifier must not contain whitespace.".to_string(),
        });
    }

    let Some((provider, model_part)) = trimmed.split_once(':') else {
        return match trimmed {
            "flash-lite" => Ok(DEFAULT_SCREENER_MODEL.to_string()),
            "flash" => Ok(DEFAULT_FLASH_MODEL.to_string()),
            "pro" => Ok(DEFAULT_AUDITOR_MODEL.to_string()),
            JEV_MODEL_ALIAS => Ok(DEFAULT_JEV_MODEL.to_string()),
            name if is_valid_gemini_model_name(name) => Ok(format!("{DEFAULT_PROVIDER}:{name}")),
            name if is_valid_jev_model_name(name) => Ok(format!("{TYPESAFE_PROVIDER}:{name}")),
            _ => Err(ConfigureError::InvalidModelSpecifier {
                specifier: raw.to_string(),
                reason: format!(
                    "unrecognized model `{trimmed}` (expected `gemini-*`, `jev-*`, or alias `flash-lite`, `flash`, `pro`, `jev`)."
                ),
            }),
        };
    };

    if provider.is_empty() || model_part.is_empty() {
        return Err(ConfigureError::InvalidModelSpecifier {
            specifier: raw.to_string(),
            reason: "provider and model segments around `:` must both be non-empty.".to_string(),
        });
    }
    if model_part.contains(':') {
        return Err(ConfigureError::InvalidModelSpecifier {
            specifier: raw.to_string(),
            reason: "model specifier must contain at most one `:` delimiter.".to_string(),
        });
    }

    match provider {
        DEFAULT_PROVIDER => match model_part {
            "flash-lite" => Ok(DEFAULT_SCREENER_MODEL.to_string()),
            "flash" => Ok(DEFAULT_FLASH_MODEL.to_string()),
            "pro" => Ok(DEFAULT_AUDITOR_MODEL.to_string()),
            name if is_valid_gemini_model_name(name) => Ok(format!("{DEFAULT_PROVIDER}:{name}")),
            _ => Err(ConfigureError::InvalidModelSpecifier {
                specifier: raw.to_string(),
                reason: format!(
                    "unrecognized Google model `{model_part}` (expected `gemini-*` or alias `flash-lite`, `flash`, `pro`)."
                ),
            }),
        },
        TYPESAFE_PROVIDER => match model_part {
            JEV_MODEL_ALIAS => Ok(DEFAULT_JEV_MODEL.to_string()),
            name if is_valid_jev_model_name(name) => Ok(format!("{TYPESAFE_PROVIDER}:{name}")),
            _ => Err(ConfigureError::InvalidModelSpecifier {
                specifier: raw.to_string(),
                reason: format!(
                    "unrecognized TypeSafe model `{model_part}` (expected `jev-*` or alias `jev`)."
                ),
            }),
        },
        _ => Err(ConfigureError::UnsupportedProvider {
            provider: provider.to_string(),
            specifier: trimmed.to_string(),
        }),
    }
}

fn is_valid_gemini_model_name(name: &str) -> bool {
    is_valid_prefixed_model_name(name, "gemini-")
}

fn is_valid_jev_model_name(name: &str) -> bool {
    is_valid_prefixed_model_name(name, "jev-")
}

fn is_valid_prefixed_model_name(name: &str, prefix: &str) -> bool {
    let Some(suffix) = name.strip_prefix(prefix) else {
        return false;
    };
    !suffix.is_empty()
        && suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

/// Resolves the Google Gemini API key from explicit overrides, `GEMINI_API_KEY` environment variable,
/// or the user-level configuration store (`providers.google.apiKey`).
pub fn resolve_gemini_api_key(options: &ConfigureOptions) -> Result<String, ConfigureError> {
    let stored = options.stored_config.as_ref();
    resolve_non_empty(
        options.gemini_api_key.as_deref(),
        options.env.get(ENV_GEMINI_API_KEY).map(String::as_str),
    )
    .or_else(|| stored.and_then(StoredConfig::google_api_key))
    .map(str::to_string)
    .ok_or(ConfigureError::MissingApiKey)
}

/// Resolves the TypeSafe AI API key from explicit overrides, `TYPESAFE_API_KEY` environment variable,
/// or the user-level configuration store (`providers.typesafe.apiKey`).
pub(crate) fn resolve_typesafe_api_key(
    options: &ConfigureOptions,
) -> Result<String, ConfigureError> {
    let stored = options.stored_config.as_ref();
    resolve_non_empty(
        options.typesafe_api_key.as_deref(),
        options.env.get(ENV_TYPESAFE_API_KEY).map(String::as_str),
    )
    .or_else(|| stored.and_then(StoredConfig::typesafe_api_key))
    .map(str::to_string)
    .ok_or(ConfigureError::MissingTypesafeApiKey)
}

/// Executes the pure, deterministic `configure` stage and populates `.config` on the [`Caseload`].
///
/// Precedence hierarchy:
/// 1. Explicit CLI flags / programmatic overrides
/// 2. Environment variables (`CANON_CLERK_*`, `GEMINI_API_KEY`, `TYPESAFE_API_KEY`)
/// 3. User-level configuration file (`~/.config/canon-clerk/config.json`)
/// 4. Existing `caseload.config` or static defaults
pub fn execute_configure(
    options: &ConfigureOptions,
    caseload: Option<Caseload>,
) -> Result<Caseload, ConfigureError> {
    let caseload = caseload.unwrap_or_default();

    if caseload.config.is_some() && !options.has_explicit_overrides() {
        return Ok(caseload);
    }

    let stored = options.stored_config.as_ref();
    let existing_config = caseload.config.as_ref();
    let stored_screener = stored.and_then(StoredConfig::screener_specifier);
    let stored_auditor = stored.and_then(StoredConfig::auditor_specifier);

    let raw_screener = resolve_non_empty(
        options.screener_model.as_deref(),
        options.env.get(ENV_SCREENER_MODEL).map(String::as_str),
    )
    .or_else(|| {
        stored_screener
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    })
    .or_else(|| existing_config.map(|c| c.screener_model.as_str()))
    .unwrap_or(DEFAULT_SCREENER_MODEL);
    let screener_model = normalize_model_specifier(raw_screener)?;

    let raw_auditor = resolve_non_empty(
        options.auditor_model.as_deref(),
        options.env.get(ENV_AUDITOR_MODEL).map(String::as_str),
    )
    .or_else(|| {
        stored_auditor
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    })
    .or_else(|| existing_config.map(|c| c.auditor_model.as_str()))
    .unwrap_or(DEFAULT_AUDITOR_MODEL);
    let auditor_model = normalize_model_specifier(raw_auditor)?;

    let uses_google = model_provider(&screener_model) == DEFAULT_PROVIDER
        || model_provider(&auditor_model) == DEFAULT_PROVIDER;
    let uses_typesafe = model_provider(&screener_model) == TYPESAFE_PROVIDER
        || model_provider(&auditor_model) == TYPESAFE_PROVIDER;

    if uses_google {
        let _verified = resolve_gemini_api_key(options)?.len();
    }
    if uses_typesafe {
        let _verified = resolve_typesafe_api_key(options)?.len();
    }

    let reasoning_budget = resolve_reasoning_budget(options, stored, existing_config)?;

    let config = CaseloadConfig {
        screener_model,
        auditor_model,
        reasoning_budget,
    };

    Ok(caseload.with_config(config))
}

fn resolve_non_empty<'a>(
    override_val: Option<&'a str>,
    env_val: Option<&'a str>,
) -> Option<&'a str> {
    override_val
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| env_val.map(str::trim).filter(|s| !s.is_empty()))
}

fn resolve_reasoning_budget(
    options: &ConfigureOptions,
    stored: Option<&StoredConfig>,
    existing_config: Option<&CaseloadConfig>,
) -> Result<Option<u32>, ConfigureError> {
    if let Some(budget) = options.reasoning_budget {
        return Ok(Some(budget));
    }

    if let Some(raw_env) = options
        .env
        .get(ENV_REASONING_BUDGET)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let parsed =
            raw_env
                .parse::<u32>()
                .map_err(|_| ConfigureError::InvalidReasoningBudget {
                    raw: raw_env.to_string(),
                })?;
        return Ok(Some(parsed));
    }

    if let Some(stored_budget) = stored.and_then(|s| s.reasoning_budget) {
        return Ok(Some(stored_budget));
    }

    Ok(existing_config.and_then(|c| c.reasoning_budget))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caseload::{CaseloadDiscovery, CaseloadIntake, CaseloadValidation};

    fn env_with_key() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            ENV_GEMINI_API_KEY.to_string(),
            "test-synthetic-gemini-key".to_string(),
        );
        env
    }

    mod model_normalization {
        use super::*;

        #[test]
        fn test_shorthand_aliases_normalize_to_canonical_google_specifiers() {
            assert_eq!(
                normalize_model_specifier("flash-lite").expect("flash-lite alias"),
                DEFAULT_SCREENER_MODEL
            );
            assert_eq!(
                normalize_model_specifier("google:flash-lite").expect("prefixed flash-lite"),
                DEFAULT_SCREENER_MODEL
            );
            assert_eq!(
                normalize_model_specifier("flash").expect("flash alias"),
                DEFAULT_FLASH_MODEL
            );
            assert_eq!(
                normalize_model_specifier("pro").expect("pro alias"),
                DEFAULT_AUDITOR_MODEL
            );
            assert_eq!(
                normalize_model_specifier("google:pro").expect("prefixed pro"),
                DEFAULT_AUDITOR_MODEL
            );
        }

        #[test]
        fn test_unqualified_and_canonical_gemini_models_normalize() {
            assert_eq!(
                normalize_model_specifier("gemini-2.5-pro").expect("unqualified gemini"),
                "google:gemini-2.5-pro"
            );
            assert_eq!(
                normalize_model_specifier("  google:gemini-2.5-flash-lite  ")
                    .expect("trimmed canonical"),
                "google:gemini-2.5-flash-lite"
            );
        }

        #[test]
        fn test_typesafe_jev_aliases_and_specifiers_normalize() {
            assert_eq!(
                normalize_model_specifier("jev").expect("jev shorthand alias"),
                DEFAULT_JEV_MODEL
            );
            assert_eq!(
                normalize_model_specifier("typesafe:jev").expect("prefixed jev alias"),
                DEFAULT_JEV_MODEL
            );
            assert_eq!(
                normalize_model_specifier("jev-latest").expect("unqualified jev-latest"),
                "typesafe:jev-latest"
            );
            assert_eq!(
                normalize_model_specifier("jev-1.14.0").expect("unqualified versioned jev"),
                "typesafe:jev-1.14.0"
            );
            assert_eq!(
                normalize_model_specifier("  typesafe:jev-preview  ")
                    .expect("trimmed canonical typesafe"),
                "typesafe:jev-preview"
            );
            assert_eq!(model_provider("typesafe:jev-latest"), "typesafe");
            assert_eq!(model_provider("google:gemini-2.5-pro"), "google");
        }

        #[test]
        fn test_unsupported_providers_rejected_with_remediation_hint() {
            let err = normalize_model_specifier("openai:gpt-4o").expect_err("openai must fail");
            assert_eq!(
                err,
                ConfigureError::UnsupportedProvider {
                    provider: "openai".to_string(),
                    specifier: "openai:gpt-4o".to_string(),
                }
            );
            assert!(err.to_string().contains("Hint:"));
        }

        #[test]
        fn test_malformed_or_unrecognized_specifiers_rejected() {
            for invalid in [
                "",
                "   ",
                ":gemini-pro-latest",
                "google:",
                "typesafe:",
                "google:gemini:pro",
                "google:gemini pro",
                "google:claude-3-5-sonnet",
                "google:jev-latest",
                "typesafe:gemini-2.5-pro",
                "typesafe:jev-",
                "unknown-model",
                "gemini-",
                "jev-",
            ] {
                let err = normalize_model_specifier(invalid)
                    .expect_err(&format!("`{invalid}` must be rejected"));
                assert!(matches!(err, ConfigureError::InvalidModelSpecifier { .. }));
                assert!(err.to_string().contains("Hint:"));
            }
        }
    }

    mod precedence_and_resolution {
        use super::*;

        #[test]
        fn test_defaults_applied_when_only_api_key_is_present() {
            let opts = ConfigureOptions {
                env: env_with_key(),
                ..ConfigureOptions::default()
            };
            let caseload = execute_configure(&opts, None).expect("configure should succeed");
            let cfg = caseload.config.expect("config populated");
            assert_eq!(cfg.screener_model, DEFAULT_SCREENER_MODEL);
            assert_eq!(cfg.auditor_model, DEFAULT_AUDITOR_MODEL);
            assert_eq!(cfg.reasoning_budget, None);
        }

        #[test]
        fn test_stored_config_provides_credentials_and_tier_defaults_below_env_and_flags() {
            let mut providers = BTreeMap::new();
            providers.insert(
                "google".to_string(),
                StoredProviderConfig {
                    api_key: Some("stored-google-key".to_string()),
                },
            );
            let stored = StoredConfig {
                providers,
                screener_model: Some(StoredTierModel::Specifier("flash".to_string())),
                auditor_model: Some(StoredTierModel::Specifier("gemini-2.5-pro".to_string())),
                reasoning_budget: Some(2048),
                ..StoredConfig::default()
            };

            // 1. Stored config used when env and CLI flags are absent
            let from_store = execute_configure(
                &ConfigureOptions {
                    stored_config: Some(stored.clone()),
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("configure from stored config");
            assert_eq!(
                from_store.config,
                Some(CaseloadConfig {
                    screener_model: DEFAULT_FLASH_MODEL.to_string(),
                    auditor_model: "google:gemini-2.5-pro".to_string(),
                    reasoning_budget: Some(2048),
                })
            );

            // 2. Environment variables override stored config
            let mut env = BTreeMap::new();
            env.insert(ENV_SCREENER_MODEL.to_string(), "flash-lite".to_string());
            env.insert(ENV_REASONING_BUDGET.to_string(), "4096".to_string());
            let from_env = execute_configure(
                &ConfigureOptions {
                    env,
                    stored_config: Some(stored),
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("env overrides stored config");
            assert_eq!(
                from_env.config,
                Some(CaseloadConfig {
                    screener_model: DEFAULT_SCREENER_MODEL.to_string(),
                    auditor_model: "google:gemini-2.5-pro".to_string(),
                    reasoning_budget: Some(4096),
                })
            );
        }

        #[test]
        fn test_explicit_cli_overrides_take_precedence_over_environment_variables() {
            let mut env = env_with_key();
            env.insert(
                ENV_SCREENER_MODEL.to_string(),
                "gemini-2.0-flash".to_string(),
            );
            env.insert(ENV_AUDITOR_MODEL.to_string(), "gemini-2.0-pro".to_string());
            env.insert(ENV_REASONING_BUDGET.to_string(), "1024".to_string());

            let opts = ConfigureOptions {
                screener_model: Some("flash-lite".to_string()),
                auditor_model: Some("pro".to_string()),
                reasoning_budget: Some(4096),
                gemini_api_key: None,
                typesafe_api_key: None,
                env,
                stored_config: None,
            };
            let caseload = execute_configure(&opts, None).expect("configure with overrides");
            let cfg = caseload.config.expect("config populated");
            assert_eq!(cfg.screener_model, DEFAULT_SCREENER_MODEL);
            assert_eq!(cfg.auditor_model, DEFAULT_AUDITOR_MODEL);
            assert_eq!(cfg.reasoning_budget, Some(4096));
        }

        #[test]
        fn test_invalid_reasoning_budget_env_fails_with_hint() {
            let mut env = env_with_key();
            env.insert(ENV_REASONING_BUDGET.to_string(), "not-a-number".to_string());
            let opts = ConfigureOptions {
                env,
                ..ConfigureOptions::default()
            };
            let err = execute_configure(&opts, None).expect_err("invalid budget must fail");
            assert_eq!(
                err,
                ConfigureError::InvalidReasoningBudget {
                    raw: "not-a-number".to_string()
                }
            );
            assert!(err.to_string().contains("Hint:"));
        }
    }

    mod credential_validation_and_security {
        use super::*;

        #[test]
        fn test_missing_or_blank_api_key_fails_fast_with_remediation() {
            let empty_opts = ConfigureOptions::default();
            let err = execute_configure(&empty_opts, None).expect_err("missing key must fail");
            assert_eq!(err, ConfigureError::MissingApiKey);
            assert!(err.to_string().contains(ENV_GEMINI_API_KEY));
            assert!(err.to_string().contains("Hint:"));

            let mut blank_env = BTreeMap::new();
            blank_env.insert(ENV_GEMINI_API_KEY.to_string(), "   ".to_string());
            let blank_opts = ConfigureOptions {
                env: blank_env,
                ..ConfigureOptions::default()
            };
            assert_eq!(
                execute_configure(&blank_opts, None),
                Err(ConfigureError::MissingApiKey)
            );
        }

        #[test]
        fn test_api_key_never_persisted_in_serialized_caseload() {
            let secret = "synthetic-secret-value-do-not-persist-12345";
            let typesafe_secret = "synthetic-typesafe-secret-never-persist-67890";
            let opts = ConfigureOptions {
                screener_model: Some("jev".to_string()),
                gemini_api_key: Some(secret.to_string()),
                typesafe_api_key: Some(typesafe_secret.to_string()),
                ..ConfigureOptions::default()
            };
            let caseload = execute_configure(&opts, None).expect("configure with flag keys");
            let serialized = serde_json::to_string(&caseload).expect("serialize caseload");
            assert!(
                !serialized.contains(secret) && !serialized.contains(typesafe_secret),
                "Caseload JSON must never contain raw API key material"
            );
        }

        #[test]
        fn test_active_provider_credential_gating_for_hybrid_and_typesafe_only_configs() {
            // 1. Hybrid screener=jev + auditor=pro requires BOTH Gemini and TypeSafe keys
            let missing_typesafe = ConfigureOptions {
                screener_model: Some("jev".to_string()),
                env: env_with_key(),
                ..ConfigureOptions::default()
            };
            let err_ts = execute_configure(&missing_typesafe, None)
                .expect_err("hybrid config without TYPESAFE_API_KEY must fail");
            assert_eq!(err_ts, ConfigureError::MissingTypesafeApiKey);
            assert!(err_ts.to_string().contains(ENV_TYPESAFE_API_KEY));
            assert!(err_ts.to_string().contains("Hint:"));

            // 2. Hybrid succeeds when TYPESAFE_API_KEY is supplied via env
            let mut both_env = env_with_key();
            both_env.insert(
                ENV_TYPESAFE_API_KEY.to_string(),
                "synthetic-typesafe-env-key".to_string(),
            );
            let hybrid_ok = execute_configure(
                &ConfigureOptions {
                    screener_model: Some("jev".to_string()),
                    env: both_env,
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("hybrid configure should succeed");
            assert_eq!(
                hybrid_ok.config,
                Some(CaseloadConfig {
                    screener_model: DEFAULT_JEV_MODEL.to_string(),
                    auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
                    reasoning_budget: None,
                })
            );

            // 3. Pure TypeSafe config does NOT require GEMINI_API_KEY
            let mut ts_only_env = BTreeMap::new();
            ts_only_env.insert(
                ENV_TYPESAFE_API_KEY.to_string(),
                "synthetic-typesafe-only-key".to_string(),
            );
            let ts_only_ok = execute_configure(
                &ConfigureOptions {
                    screener_model: Some("jev".to_string()),
                    auditor_model: Some("typesafe:jev-latest".to_string()),
                    env: ts_only_env,
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("typesafe-only config should succeed without GEMINI_API_KEY");
            assert_eq!(
                ts_only_ok.config.expect("config").screener_model,
                DEFAULT_JEV_MODEL
            );
        }
    }

    mod fast_forwarding_and_immutability {
        use super::*;

        #[test]
        fn test_fast_forwards_when_config_already_present_and_no_overrides() {
            let preconfigured = Caseload::new().with_config(CaseloadConfig {
                screener_model: "google:gemini-2.5-flash-lite".to_string(),
                auditor_model: "google:gemini-2.5-pro".to_string(),
                reasoning_budget: Some(2048),
            });

            let opts = ConfigureOptions::default();
            let result =
                execute_configure(&opts, Some(preconfigured.clone())).expect("fast-forward");
            assert_eq!(result, preconfigured);
        }

        #[test]
        fn test_preserves_branch_a_namespaces_on_incoming_caseload() {
            let upstream = Caseload::new()
                .with_intake(CaseloadIntake {
                    pr_title: Some("feat: branch a".to_string()),
                    ..CaseloadIntake::default()
                })
                .with_discovery(CaseloadDiscovery {
                    candidate_canons: vec![".canons/cli/rule.md".to_string()],
                    ..CaseloadDiscovery::default()
                })
                .with_validation(CaseloadValidation::default());

            let opts = ConfigureOptions {
                env: env_with_key(),
                ..ConfigureOptions::default()
            };
            let enriched =
                execute_configure(&opts, Some(upstream.clone())).expect("configure upstream");
            assert_eq!(enriched.intake, upstream.intake);
            assert_eq!(enriched.discovery, upstream.discovery);
            assert_eq!(enriched.validation, upstream.validation);
            assert_eq!(enriched.config, Some(CaseloadConfig::default()));
        }
    }
}
