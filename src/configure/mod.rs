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

mod providers;
pub mod store;
mod thinking;

pub use providers::{
    MissingCredentialsReason, PROVIDER_CAPABILITIES, ProviderCapability, ProviderRole,
    resolve_local_decision_endpoint,
};
pub(crate) use providers::{
    resolve_default_tiers, resolve_provider_api_key, verified_credential_env_vars,
};
pub use store::{
    StoredConfig, StoredProviderConfig, StoredTierModel, load_host_config_file,
    resolve_default_config_path,
};
pub use thinking::parse_thinking_level;

use crate::caseload::{
    Caseload, CaseloadConfig, DEFAULT_AUDITOR_MODEL, DEFAULT_PROVIDER, DEFAULT_SCREENER_MODEL,
    LOCAL_PROVIDER, TYPESAFE_PROVIDER, ThinkingLevel,
};
use std::collections::BTreeMap;
use std::fmt;
use thinking::resolve_thinking_options;

/// Environment variable name for the Google Gemini API key.
pub const ENV_GEMINI_API_KEY: &str = "GEMINI_API_KEY";

/// Environment variable name for the TypeSafe AI (Jev) API key.
pub const ENV_TYPESAFE_API_KEY: &str = "TYPESAFE_API_KEY";

/// Environment variable name for overriding the screener model specifier.
pub const ENV_SCREENER_MODEL: &str = "CANON_CLERK_SCREENER_MODEL";

/// Environment variable name for overriding the auditor model specifier.
pub const ENV_AUDITOR_MODEL: &str = "CANON_CLERK_AUDITOR_MODEL";

/// Environment variable name for overriding the qualitative thinking level (`minimal|low|medium|high`).
pub const ENV_THINKING_LEVEL: &str = "CANON_CLERK_THINKING_LEVEL";

/// Environment variable name for overriding the maximum reasoning budget in tokens.
pub const ENV_REASONING_BUDGET: &str = "CANON_CLERK_REASONING_BUDGET";

/// Default canonical TypeSafe AI Jev model specifier (`docs/architecture/nodes/configure.md` §4).
pub const DEFAULT_JEV_MODEL: &str = "typesafe:jev-latest";

/// Default localhost HTTP endpoint for `local:<model>` discriminative daemons.
pub const DEFAULT_LOCAL_DECISION_ENDPOINT: &str = "http://127.0.0.1:8080";

/// Primary namespace-qualified environment variable overriding `--local-decision-endpoint`.
pub const ENV_CANON_CLERK_LOCAL_DECISION_ENDPOINT: &str = "CANON_CLERK_LOCAL_DECISION_ENDPOINT";

/// Fallback provider-prefixed environment variable overriding `--local-decision-endpoint`.
pub const ENV_LOCAL_DECISION_ENDPOINT: &str = "LOCAL_DECISION_ENDPOINT";

/// Domain input parameters, environment snapshot, and optional user config store for `configure`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigureOptions {
    /// Explicit screener model override (`--screener-model`).
    pub screener_model: Option<String>,
    /// Explicit auditor model override (`--auditor-model`).
    pub auditor_model: Option<String>,
    /// Explicit qualitative thinking level override (`--thinking-level`).
    pub thinking_level: Option<ThinkingLevel>,
    /// Explicit reasoning budget override in tokens (`--reasoning-budget`).
    pub reasoning_budget: Option<u32>,
    /// Explicit Google Gemini API key override (`--gemini-api-key`).
    pub gemini_api_key: Option<String>,
    /// Explicit TypeSafe AI API key override (`--typesafe-api-key`).
    pub typesafe_api_key: Option<String>,
    /// Explicit localhost decision daemon endpoint override (`--local-decision-endpoint`).
    pub local_decision_endpoint: Option<String>,
    /// When `true` (screener-only closures: `apprise`, `docket`, `admit`, `vet`), verifies active
    /// credentials strictly for `screener_model` unless `auditor_model` was explicitly overridden.
    pub screener_only_closure: bool,
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
            || self.thinking_level.is_some()
            || self.reasoning_budget.is_some()
            || self.gemini_api_key.is_some()
            || self.typesafe_api_key.is_some()
            || self.local_decision_endpoint.is_some()
    }

    pub(crate) fn cli_api_key_for(&self, provider_slug: &str) -> Option<&str> {
        match provider_slug {
            DEFAULT_PROVIDER => self.gemini_api_key.as_deref(),
            TYPESAFE_PROVIDER => self.typesafe_api_key.as_deref(),
            _ => None,
        }
    }
}

/// Domain errors produced during `configure` execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigureError {
    /// Required `GEMINI_API_KEY` credential is missing from flags, environment, and user config.
    MissingApiKey,
    /// Required `TYPESAFE_API_KEY` credential is missing from flags, environment, and user config.
    MissingTypesafeApiKey,
    /// Unpinned or closure-scoped provider credentials are missing from flags, environment, and user config.
    MissingCredentials(MissingCredentialsReason),
    /// Model specifier uses an unsupported provider prefix (only `google` and `typesafe` are supported).
    UnsupportedProvider { provider: String, specifier: String },
    /// Model specifier is empty, contains whitespace, or does not name a valid supported model.
    InvalidModelSpecifier { specifier: String, reason: String },
    /// Discriminative decision model (`google:gemini-decision-*` or `typesafe:*`) was configured on `auditor_model`.
    DiscriminativeAuditorUnsupported { specifier: String },
    /// Thinking level cannot be parsed as `minimal`, `low`, `medium`, or `high`.
    InvalidThinkingLevel { raw: String },
    /// Reasoning budget environment variable cannot be parsed as a non-negative integer.
    InvalidReasoningBudget { raw: String },
    /// Mutually exclusive `thinking_level` and `reasoning_budget` were specified at the same layer.
    ConflictingThinkingOptions { source: String },
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
            Self::MissingCredentials(reason) => providers::format_missing_credentials(reason, f),
            Self::UnsupportedProvider {
                provider,
                specifier,
            } => write!(
                f,
                "Unsupported model provider `{provider}` in specifier `{specifier}`.\n  \
                 Hint: Canon Clerk supports `google` (e.g. `{DEFAULT_SCREENER_MODEL}`, `{DEFAULT_AUDITOR_MODEL}`), \
                 `typesafe` (e.g. `{DEFAULT_JEV_MODEL}`), and `local` (e.g. `local:qwen2.5-coder-1.5b-instruct`)."
            ),
            Self::InvalidModelSpecifier { specifier, reason } => write!(
                f,
                "Invalid model specifier `{specifier}`: {reason}\n  \
                 Hint: Specify an exact upstream model identifier such as `{DEFAULT_SCREENER_MODEL}`, \
                 `{DEFAULT_AUDITOR_MODEL}`, `{DEFAULT_JEV_MODEL}`, or unprefixed `gemini-decision-1`, `gemini-4-hydrogen`, `gemini-4-carbon`, `jev-latest`."
            ),
            Self::DiscriminativeAuditorUnsupported { specifier } => write!(
                f,
                "Discriminative model `{specifier}` cannot be configured as `auditor_model` \
                 (`audit` requires structured generative output for exception rulings, remediation guidance, and line annotations).\n  \
                 Hint: Pass `--screener-model {specifier}` (e.g. `canon-clerk vet --screener-model {specifier}`) \
                 and keep a generative auditor such as `{DEFAULT_AUDITOR_MODEL}` (`gemini-4-carbon`) on `--auditor-model`."
            ),
            Self::InvalidThinkingLevel { raw } => write!(
                f,
                "Invalid thinking level `{raw}`: expected one of `minimal`, `low`, `medium`, or `high`.\n  \
                 Hint: Pass `--thinking-level <minimal|low|medium|high>` or set `{ENV_THINKING_LEVEL}=<LEVEL>`."
            ),
            Self::InvalidReasoningBudget { raw } => write!(
                f,
                "Invalid reasoning budget `{raw}`: expected a non-negative integer token count.\n  \
                 Hint: Pass an integer token ceiling via `--reasoning-budget <TOKENS>` \
                 or `{ENV_REASONING_BUDGET}=<TOKENS>`."
            ),
            Self::ConflictingThinkingOptions { source } => write!(
                f,
                "Conflicting extended-thinking options in {source}: `--thinking-level` and `--reasoning-budget` are mutually exclusive.\n  \
                 Hint: Specify either `--thinking-level <minimal|low|medium|high>` (preferred for Gemini 4 models) or `--reasoning-budget <TOKENS>`, not both."
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

/// Normalizes a raw upstream model identifier into a canonical `<provider>:<model>` specifier.
///
/// Supported forms:
/// - Unqualified upstream model IDs: `gemini-*` -> `google:gemini-*`, `jev-*` -> `typesafe:jev-*`
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
            name if is_valid_gemini_model_name(name) => Ok(format!("{DEFAULT_PROVIDER}:{name}")),
            name if is_valid_jev_model_name(name) => Ok(format!("{TYPESAFE_PROVIDER}:{name}")),
            _ => Err(ConfigureError::InvalidModelSpecifier {
                specifier: raw.to_string(),
                reason: format!(
                    "unrecognized model `{trimmed}` (expected `gemini-*`, `jev-*`, `google:gemini-*`, or `typesafe:jev-*`)."
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
        DEFAULT_PROVIDER => {
            if is_valid_gemini_model_name(model_part) {
                Ok(format!("{DEFAULT_PROVIDER}:{model_part}"))
            } else {
                Err(ConfigureError::InvalidModelSpecifier {
                    specifier: raw.to_string(),
                    reason: format!(
                        "unrecognized Google model `{model_part}` (expected `gemini-decision-1`, `gemini-4-hydrogen`, `gemini-4-carbon`, or another exact `gemini-*` upstream ID)."
                    ),
                })
            }
        }
        TYPESAFE_PROVIDER => match model_part {
            name if is_valid_jev_model_name(name) => Ok(format!("{TYPESAFE_PROVIDER}:{name}")),
            _ => Err(ConfigureError::InvalidModelSpecifier {
                specifier: raw.to_string(),
                reason: format!(
                    "unrecognized TypeSafe model `{model_part}` (expected `jev-*`, e.g. `jev-latest`)."
                ),
            }),
        },
        LOCAL_PROVIDER => {
            if is_valid_local_model_name(model_part) {
                Ok(format!("{LOCAL_PROVIDER}:{model_part}"))
            } else {
                Err(ConfigureError::InvalidModelSpecifier {
                    specifier: raw.to_string(),
                    reason: format!(
                        "invalid local model `{model_part}` (expected `local:<model>`, e.g. `local:qwen2.5-coder-1.5b-instruct`)."
                    ),
                })
            }
        }
        _ => Err(ConfigureError::UnsupportedProvider {
            provider: provider.to_string(),
            specifier: trimmed.to_string(),
        }),
    }
}

pub(crate) fn is_gemini_decision_model_name(name: &str) -> bool {
    is_valid_prefixed_model_name(name, "gemini-decision-")
}

/// Returns `true` when `specifier` names a discriminative `Noul` decision model
/// (`google:gemini-decision-*`, `gemini-decision-*`, `typesafe:*`, `jev-*`, or `local:*`).
pub fn is_discriminative_model(specifier: &str) -> bool {
    let trimmed = specifier.trim();
    match trimmed.split_once(':') {
        Some((DEFAULT_PROVIDER, model_part)) => is_gemini_decision_model_name(model_part),
        Some((TYPESAFE_PROVIDER | LOCAL_PROVIDER, model_part)) => !model_part.is_empty(),
        Some(_) => false,
        None => is_gemini_decision_model_name(trimmed) || is_valid_jev_model_name(trimmed),
    }
}

pub(crate) fn is_valid_local_model_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.ends_with('/')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '/')
}

fn is_valid_gemini_model_name(name: &str) -> bool {
    if name.starts_with("gemini-decision") {
        return is_gemini_decision_model_name(name);
    }
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
    resolve_provider_api_key(DEFAULT_PROVIDER, options)
}

/// Resolves the TypeSafe AI API key from explicit overrides, `TYPESAFE_API_KEY` environment variable,
/// or the user-level configuration store (`providers.typesafe.apiKey`).
pub(crate) fn resolve_typesafe_api_key(
    options: &ConfigureOptions,
) -> Result<String, ConfigureError> {
    resolve_provider_api_key(TYPESAFE_PROVIDER, options)
}

fn resolve_pinned_tier_specifier(
    cli_override: Option<&str>,
    env_override: Option<&str>,
    stored_override: Option<&str>,
    existing_caseload_model: Option<&str>,
) -> Result<Option<String>, ConfigureError> {
    if let Some(raw_cli) = cli_override {
        return normalize_model_specifier(raw_cli).map(Some);
    }
    if let Some(raw_env) = env_override.map(str::trim).filter(|s| !s.is_empty()) {
        return normalize_model_specifier(raw_env).map(Some);
    }
    if let Some(raw_stored) = stored_override.map(str::trim).filter(|s| !s.is_empty()) {
        return normalize_model_specifier(raw_stored).map(Some);
    }
    if let Some(raw_existing) = existing_caseload_model
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return normalize_model_specifier(raw_existing).map(Some);
    }
    Ok(None)
}

/// Executes the pure, deterministic `configure` stage and populates `.config` on the [`Caseload`].
///
/// Precedence hierarchy (`docs/architecture/nodes/configure.md` §4):
/// 1. Explicit CLI flags / programmatic overrides
/// 2. Environment variables (`CANON_CLERK_*`, `GEMINI_API_KEY`, `TYPESAFE_API_KEY`)
/// 3. User-level configuration file (`~/.config/canon-clerk/config.json`)
/// 4. Pre-populated `caseload.config` or credential-aware default matrix
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
    let (inferred_screener, inferred_auditor) = resolve_default_tiers(options);

    let pinned_screener = resolve_pinned_tier_specifier(
        options.screener_model.as_deref(),
        options.env.get(ENV_SCREENER_MODEL).map(String::as_str),
        stored_screener.as_deref(),
        existing_config.map(|c| c.screener_model.as_str()),
    )?;
    let screener_was_pinned = pinned_screener.is_some();
    let screener_model = pinned_screener.unwrap_or(inferred_screener);

    let pinned_auditor = resolve_pinned_tier_specifier(
        options.auditor_model.as_deref(),
        options.env.get(ENV_AUDITOR_MODEL).map(String::as_str),
        stored_auditor.as_deref(),
        existing_config.map(|c| c.auditor_model.as_str()),
    )?;
    let auditor_was_pinned = pinned_auditor.is_some();
    let auditor_model = pinned_auditor.unwrap_or(inferred_auditor);

    if is_discriminative_model(&auditor_model) {
        return Err(ConfigureError::DiscriminativeAuditorUnsupported {
            specifier: auditor_model,
        });
    }

    let (thinking_level, reasoning_budget) =
        resolve_thinking_options(options, stored, existing_config)?;

    let check_auditor_creds = !options.screener_only_closure || options.auditor_model.is_some();
    providers::verify_closure_credentials(
        &providers::ResolvedTierPinState {
            screener_model: &screener_model,
            screener_was_pinned,
            auditor_model: &auditor_model,
            auditor_was_pinned,
            check_auditor_creds,
        },
        options,
    )?;

    let config = CaseloadConfig {
        screener_model,
        auditor_model,
        thinking_level,
        reasoning_budget,
    };

    Ok(caseload.with_config(config))
}

pub(crate) fn resolve_non_empty<'a>(
    override_val: Option<&'a str>,
    env_val: Option<&'a str>,
) -> Option<&'a str> {
    override_val
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| env_val.map(str::trim).filter(|s| !s.is_empty()))
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
        fn test_unqualified_and_canonical_gemini_models_normalize() {
            assert_eq!(
                normalize_model_specifier("gemini-4-carbon").expect("unqualified gemini-4-carbon"),
                DEFAULT_AUDITOR_MODEL
            );
            assert_eq!(
                normalize_model_specifier("google:gemini-4-carbon")
                    .expect("canonical gemini-4-carbon"),
                DEFAULT_AUDITOR_MODEL
            );
            assert_eq!(
                normalize_model_specifier("gemini-decision-1")
                    .expect("unqualified gemini-decision-1"),
                DEFAULT_SCREENER_MODEL
            );
            assert_eq!(
                normalize_model_specifier("google:gemini-decision-1")
                    .expect("canonical gemini-decision-1"),
                DEFAULT_SCREENER_MODEL
            );
            assert_eq!(
                normalize_model_specifier("gemini-4-hydrogen")
                    .expect("unqualified gemini-4-hydrogen"),
                "google:gemini-4-hydrogen"
            );
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
        fn test_typesafe_jev_specifiers_normalize() {
            assert_eq!(
                normalize_model_specifier("jev-latest").expect("unqualified jev-latest"),
                DEFAULT_JEV_MODEL
            );
            assert_eq!(
                normalize_model_specifier("typesafe:jev-latest").expect("canonical jev-latest"),
                DEFAULT_JEV_MODEL
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
        fn test_retired_pet_name_aliases_are_rejected_with_remediation_hint() {
            for retired in [
                "carbon",
                "argon",
                "hydrogen",
                "flash-lite",
                "flash",
                "pro",
                "jev",
                "google:carbon",
                "google:argon",
                "google:hydrogen",
                "google:flash-lite",
                "google:flash",
                "google:pro",
                "typesafe:jev",
            ] {
                let err = normalize_model_specifier(retired)
                    .expect_err(&format!("retired alias `{retired}` must be rejected"));
                assert!(matches!(err, ConfigureError::InvalidModelSpecifier { .. }));
                assert!(err.to_string().contains("Hint:"));
            }
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
                "gemini-decision",
                "google:gemini-decision",
                "gemini-decision-",
                "google:gemini-decision-",
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
                    ..StoredProviderConfig::default()
                },
            );
            let stored = StoredConfig {
                providers,
                screener_model: Some(StoredTierModel::Specifier(
                    "gemini-2.5-flash-lite".to_string(),
                )),
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
                    screener_model: "google:gemini-2.5-flash-lite".to_string(),
                    auditor_model: "google:gemini-2.5-pro".to_string(),
                    thinking_level: None,
                    reasoning_budget: Some(2048),
                })
            );

            // 2. Environment variables override stored config
            let mut env = BTreeMap::new();
            env.insert(
                ENV_SCREENER_MODEL.to_string(),
                "gemini-4-hydrogen".to_string(),
            );
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
                    screener_model: "google:gemini-4-hydrogen".to_string(),
                    auditor_model: "google:gemini-2.5-pro".to_string(),
                    thinking_level: None,
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
                screener_model: Some("gemini-4-hydrogen".to_string()),
                auditor_model: Some("gemini-4-carbon".to_string()),
                reasoning_budget: Some(4096),
                env,
                ..ConfigureOptions::default()
            };
            let caseload = execute_configure(&opts, None).expect("configure with overrides");
            let cfg = caseload.config.expect("config populated");
            assert_eq!(cfg.screener_model, "google:gemini-4-hydrogen");
            assert_eq!(cfg.auditor_model, DEFAULT_AUDITOR_MODEL);
            assert_eq!(cfg.thinking_level, None);
            assert_eq!(cfg.reasoning_budget, Some(4096));
        }

        #[test]
        fn test_thinking_level_precedence_and_mutual_exclusivity() {
            // 1. Conflicting CLI flags fail fast with exit-2 ConfigureError::ConflictingThinkingOptions
            let conflict_cli = ConfigureOptions {
                thinking_level: Some(ThinkingLevel::Low),
                reasoning_budget: Some(2048),
                env: env_with_key(),
                ..ConfigureOptions::default()
            };
            let err_cli =
                execute_configure(&conflict_cli, None).expect_err("conflicting CLI flags fail");
            assert!(matches!(
                err_cli,
                ConfigureError::ConflictingThinkingOptions { .. }
            ));
            assert!(err_cli.to_string().contains("Hint:"));

            // 2. Conflicting env vars fail fast
            let mut conflict_env = env_with_key();
            conflict_env.insert(ENV_THINKING_LEVEL.to_string(), "medium".to_string());
            conflict_env.insert(ENV_REASONING_BUDGET.to_string(), "2048".to_string());
            let err_env = execute_configure(
                &ConfigureOptions {
                    env: conflict_env,
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect_err("conflicting env vars fail");
            assert!(matches!(
                err_env,
                ConfigureError::ConflictingThinkingOptions { .. }
            ));

            // 3. Higher-precedence CLI thinking_level overrides lower-precedence env reasoning_budget
            let mut env_budget = env_with_key();
            env_budget.insert(ENV_REASONING_BUDGET.to_string(), "2048".to_string());
            let overridden = execute_configure(
                &ConfigureOptions {
                    thinking_level: Some(ThinkingLevel::Low),
                    env: env_budget,
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("CLI thinking_level supersedes env reasoning_budget");
            let cfg = overridden.config.expect("config");
            assert_eq!(cfg.thinking_level, Some(ThinkingLevel::Low));
            assert_eq!(cfg.reasoning_budget, None);

            // 4. Invalid thinking level in env fails with Hint
            let mut bad_lvl_env = env_with_key();
            bad_lvl_env.insert(ENV_THINKING_LEVEL.to_string(), "turbo".to_string());
            let err_lvl = execute_configure(
                &ConfigureOptions {
                    env: bad_lvl_env,
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect_err("invalid thinking level fails");
            assert_eq!(
                err_lvl,
                ConfigureError::InvalidThinkingLevel {
                    raw: "turbo".to_string()
                }
            );
            assert!(err_lvl.to_string().contains("Hint:"));
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
        fn test_missing_or_blank_credentials_fail_fast_with_closure_aware_remediation() {
            // 1. Unpinned full-bench closure with zero keys lists supported provider env vars & flags
            let empty_opts = ConfigureOptions::default();
            let err = execute_configure(&empty_opts, None).expect_err("missing key must fail");
            assert_eq!(
                err,
                ConfigureError::MissingCredentials(MissingCredentialsReason::FullBenchClosure)
            );
            let msg = err.to_string();
            assert!(msg.contains(ENV_GEMINI_API_KEY));
            assert!(msg.contains(ENV_TYPESAFE_API_KEY));
            assert!(msg.contains("--gemini-api-key"));
            assert!(msg.contains("Hint:"));

            // 2. Unpinned screener-only closure with zero keys lists both screener providers
            let screener_empty = ConfigureOptions {
                screener_only_closure: true,
                ..ConfigureOptions::default()
            };
            let screener_err = execute_configure(&screener_empty, None)
                .expect_err("missing screener credentials must fail");
            assert_eq!(
                screener_err,
                ConfigureError::MissingCredentials(MissingCredentialsReason::ScreenerClosure)
            );
            let screener_msg = screener_err.to_string();
            assert!(screener_msg.contains(ENV_TYPESAFE_API_KEY));
            assert!(screener_msg.contains(ENV_GEMINI_API_KEY));
            assert!(screener_msg.contains("--typesafe-api-key"));
            assert!(screener_msg.contains("Hint:"));

            // 3. Blank env key is treated as absent; explicit Google pin fails with MissingApiKey
            let mut blank_env = BTreeMap::new();
            blank_env.insert(ENV_GEMINI_API_KEY.to_string(), "   ".to_string());
            let blank_pinned_opts = ConfigureOptions {
                screener_model: Some("gemini-4-hydrogen".to_string()),
                env: blank_env,
                ..ConfigureOptions::default()
            };
            assert_eq!(
                execute_configure(&blank_pinned_opts, None),
                Err(ConfigureError::MissingApiKey)
            );
        }

        #[test]
        fn test_api_key_never_persisted_in_serialized_caseload() {
            let secret = "synthetic-secret-value-do-not-persist-12345";
            let typesafe_secret = "synthetic-typesafe-secret-never-persist-67890";
            let opts = ConfigureOptions {
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
            assert_eq!(
                caseload.config,
                Some(CaseloadConfig {
                    screener_model: DEFAULT_SCREENER_MODEL.to_string(),
                    auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
                    thinking_level: None,
                    reasoning_budget: None,
                })
            );
        }

        #[test]
        fn test_hybrid_auto_selection_and_explicit_pin_precedence() {
            // 1. Explicit screener=jev-latest without TYPESAFE_API_KEY fails fast with MissingTypesafeApiKey
            let missing_typesafe = ConfigureOptions {
                screener_model: Some("jev-latest".to_string()),
                env: env_with_key(),
                ..ConfigureOptions::default()
            };
            let err_ts = execute_configure(&missing_typesafe, None)
                .expect_err("explicit jev-latest without TYPESAFE_API_KEY must fail");
            assert_eq!(err_ts, ConfigureError::MissingTypesafeApiKey);
            assert!(err_ts.to_string().contains(ENV_TYPESAFE_API_KEY));
            assert!(err_ts.to_string().contains("Hint:"));

            // 2. Unpinned hybrid env (GEMINI_API_KEY + TYPESAFE_API_KEY) auto-selects gemini-decision-1 + gemini-4-carbon
            let mut both_env = env_with_key();
            both_env.insert(
                ENV_TYPESAFE_API_KEY.to_string(),
                "synthetic-typesafe-env-key".to_string(),
            );
            let hybrid_ok = execute_configure(
                &ConfigureOptions {
                    env: both_env.clone(),
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect(
                "unpinned hybrid configure should auto-select gemini-decision-1 + gemini-4-carbon",
            );
            assert_eq!(
                hybrid_ok.config,
                Some(CaseloadConfig {
                    screener_model: DEFAULT_SCREENER_MODEL.to_string(),
                    auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
                    thinking_level: None,
                    reasoning_budget: None,
                })
            );

            // 3. Explicit pins (CLI, env, or config.json) override credential-inferred hybrid defaults
            let cli_pinned = execute_configure(
                &ConfigureOptions {
                    screener_model: Some("jev-latest".to_string()),
                    env: both_env.clone(),
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("CLI --screener-model jev-latest overrides default gemini-decision-1");
            assert_eq!(
                cli_pinned.config.expect("config").screener_model,
                DEFAULT_JEV_MODEL
            );

            let mut env_pinned_map = both_env.clone();
            env_pinned_map.insert(
                ENV_SCREENER_MODEL.to_string(),
                "gemini-2.5-flash-lite".to_string(),
            );
            let env_pinned = execute_configure(
                &ConfigureOptions {
                    env: env_pinned_map,
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("CANON_CLERK_SCREENER_MODEL overrides ambient TYPESAFE_API_KEY");
            assert_eq!(
                env_pinned.config.expect("config").screener_model,
                "google:gemini-2.5-flash-lite"
            );

            let store_pinned = execute_configure(
                &ConfigureOptions {
                    env: both_env,
                    stored_config: Some(StoredConfig {
                        screener_model: Some(StoredTierModel::Specifier(
                            "gemini-4-argon".to_string(),
                        )),
                        ..StoredConfig::default()
                    }),
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("config.json screenerModel overrides ambient TYPESAFE_API_KEY");
            assert_eq!(
                store_pinned.config.expect("config").screener_model,
                "google:gemini-4-argon"
            );
        }

        #[test]
        fn test_typesafe_only_screener_closure_vs_full_bench_gating() {
            let mut ts_only_env = BTreeMap::new();
            ts_only_env.insert(
                ENV_TYPESAFE_API_KEY.to_string(),
                "synthetic-typesafe-only-key".to_string(),
            );

            // 1. Discriminative models (typesafe:jev-* and google:gemini-decision-*) on auditor_model are rejected
            for bad_auditor in [
                "typesafe:jev-latest",
                "gemini-decision-1",
                "google:gemini-decision-1",
            ] {
                let err_disc_auditor = execute_configure(
                    &ConfigureOptions {
                        auditor_model: Some(bad_auditor.to_string()),
                        env: env_with_key(),
                        ..ConfigureOptions::default()
                    },
                    None,
                )
                .expect_err("discriminative auditor_model must be rejected");
                assert!(matches!(
                    err_disc_auditor,
                    ConfigureError::DiscriminativeAuditorUnsupported { .. }
                ));
                assert!(err_disc_auditor.to_string().contains("canon-clerk vet"));
                assert!(err_disc_auditor.to_string().contains("Hint:"));
            }

            // 2. Screener-only closure with ONLY TYPESAFE_API_KEY succeeds with zero model flags
            let screener_only_ok = execute_configure(
                &ConfigureOptions {
                    screener_only_closure: true,
                    env: ts_only_env.clone(),
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect("screener-only closure with TYPESAFE_API_KEY alone must auto-select jev");
            assert_eq!(
                screener_only_ok.config.expect("config").screener_model,
                DEFAULT_JEV_MODEL
            );

            // 3. Explicit Google screener pin on screener-only closure with ONLY TYPESAFE_API_KEY fails fast
            let err_explicit_google = execute_configure(
                &ConfigureOptions {
                    screener_model: Some("gemini-4-hydrogen".to_string()),
                    screener_only_closure: true,
                    env: ts_only_env.clone(),
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect_err("explicit gemini-4-hydrogen pin without GEMINI_API_KEY must fail");
            assert_eq!(err_explicit_google, ConfigureError::MissingApiKey);

            // 4. Full closure (audit/configure/probe) with ONLY TYPESAFE_API_KEY fails fast with generative auditor hint
            let err_full_missing_gen = execute_configure(
                &ConfigureOptions {
                    screener_only_closure: false,
                    env: ts_only_env,
                    ..ConfigureOptions::default()
                },
                None,
            )
            .expect_err("full closure without generative auditor credential must fail");
            assert_eq!(
                err_full_missing_gen,
                ConfigureError::MissingCredentials(
                    MissingCredentialsReason::GenerativeAuditorRequired {
                        active_screener_model: DEFAULT_JEV_MODEL.to_string(),
                    }
                )
            );
            let full_msg = err_full_missing_gen.to_string();
            assert!(full_msg.contains("generative auditor"));
            assert!(full_msg.contains(ENV_GEMINI_API_KEY));
            assert!(full_msg.contains("canon-clerk vet"));
            assert!(full_msg.contains("Hint:"));
        }
    }

    mod fast_forwarding_and_immutability {
        use super::*;

        #[test]
        fn test_fast_forwards_when_config_already_present_and_no_overrides() {
            let preconfigured = Caseload::new().with_config(CaseloadConfig {
                screener_model: "google:gemini-2.5-flash-lite".to_string(),
                auditor_model: "google:gemini-2.5-pro".to_string(),
                thinking_level: None,
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
