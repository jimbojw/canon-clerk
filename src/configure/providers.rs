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

//! Declarative provider capability registry and credential-aware default resolution matrix
//! (`docs/architecture/nodes/configure.md` §4).

use crate::caseload::{
    CaseloadConfig, DEFAULT_AUDITOR_MODEL, DEFAULT_PROVIDER, DEFAULT_SCREENER_MODEL,
    LOCAL_PROVIDER, TYPESAFE_PROVIDER,
};
use crate::configure::{
    ConfigureError, ConfigureOptions, DEFAULT_JEV_MODEL, DEFAULT_LOCAL_DECISION_ENDPOINT,
    ENV_CANON_CLERK_LOCAL_DECISION_ENDPOINT, ENV_GEMINI_API_KEY, ENV_LOCAL_DECISION_ENDPOINT,
    ENV_TYPESAFE_API_KEY, StoredConfig, is_discriminative_model, model_provider, resolve_non_empty,
};
use std::collections::BTreeSet;
use std::fmt;

/// Architectural role classification of a model provider in the Caseload pipeline (`configure.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRole {
    /// Discriminative classifier preferred for default `screener_model` when credentials are present;
    /// never auto-selected or accepted as `auditor_model`.
    DiscriminativeScreener,
    /// Full-stack generative structured model provider eligible for both default `screener_model`
    /// (fallback when discriminative screener credentials are absent) and default `auditor_model`.
    GenerativeFullStack,
}

/// Declarative capability descriptor mapping a supported provider slug to its credential sources,
/// default tier models, role capabilities, and deterministic preference rank (`configure.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapability {
    /// Canonical provider slug (`"typesafe"`, `"google"`).
    pub slug: &'static str,
    /// Human-readable provider display name (`"TypeSafe AI"`, `"Google Gemini"`).
    pub display_name: &'static str,
    /// Provider role classification (`DiscriminativeScreener` vs. `GenerativeFullStack`).
    pub role: ProviderRole,
    /// Deterministic preference rank (lower rank wins when multiple providers have credentials).
    pub rank: u8,
    /// CLI flag overriding the provider's API key (`"--typesafe-api-key"`, `"--gemini-api-key"`).
    pub cli_flag: &'static str,
    /// Environment variable providing the provider's API key (`"TYPESAFE_API_KEY"`, `"GEMINI_API_KEY"`).
    pub env_var: &'static str,
    /// Canonical `~/.config/canon-clerk/config.json` key path (`"providers.<slug>.apiKey"`).
    pub config_key_path: &'static str,
    /// Default canonical `screener_model` specifier when this provider is selected for screening.
    pub default_screener_model: Option<&'static str>,
    /// Default canonical generative `auditor_model` specifier when this provider is selected for auditing.
    pub default_auditor_model: Option<&'static str>,
}

/// Authoritative declarative provider capability matrix (`docs/architecture/nodes/configure.md` §4).
///
/// Registering a new provider requires adding one entry here; credential discovery, default tier
/// selection, tie-breaking, missing-credential diagnostics, and CLI credential summaries derive
/// directly from this table.
pub const PROVIDER_CAPABILITIES: &[ProviderCapability] = &[
    ProviderCapability {
        slug: DEFAULT_PROVIDER,
        display_name: "Google Gemini",
        role: ProviderRole::GenerativeFullStack,
        rank: 1,
        cli_flag: "--gemini-api-key",
        env_var: ENV_GEMINI_API_KEY,
        config_key_path: "providers.google.apiKey",
        default_screener_model: Some(DEFAULT_SCREENER_MODEL),
        default_auditor_model: Some(DEFAULT_AUDITOR_MODEL),
    },
    ProviderCapability {
        slug: TYPESAFE_PROVIDER,
        display_name: "TypeSafe AI",
        role: ProviderRole::DiscriminativeScreener,
        rank: 2,
        cli_flag: "--typesafe-api-key",
        env_var: ENV_TYPESAFE_API_KEY,
        config_key_path: "providers.typesafe.apiKey",
        default_screener_model: Some(DEFAULT_JEV_MODEL),
        default_auditor_model: None,
    },
];

/// Structured reason explaining why unpinned or closure-scoped provider credentials are insufficient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingCredentialsReason {
    /// Screener-only closure (`apprise`, `docket`, `admit`, `vet`) invoked with no screener credentials.
    ScreenerClosure,
    /// Full-bench closure (`configure`, `probe`, `audit`) invoked with zero provider credentials.
    FullBenchClosure,
    /// Full-bench closure invoked with discriminative screener credentials only (no generative auditor key).
    GenerativeAuditorRequired {
        /// Canonical screener model resolved from available discriminative credentials.
        active_screener_model: String,
    },
    /// Explicit tier pin targeting an extensible provider whose credential is missing.
    ProviderKey {
        /// Canonical provider slug whose credential was not found.
        provider: String,
    },
}

/// Looks up the declarative [`ProviderCapability`] entry for a canonical provider slug.
fn provider_capability(slug: &str) -> Option<&'static ProviderCapability> {
    PROVIDER_CAPABILITIES.iter().find(|cap| cap.slug == slug)
}

/// Resolves the non-empty API key for `cap` across CLI flags, environment variables, and `config.json`.
fn resolve_provider_credential<'a>(
    cap: &ProviderCapability,
    options: &'a ConfigureOptions,
) -> Option<&'a str> {
    resolve_non_empty(
        options.cli_api_key_for(cap.slug),
        options.env.get(cap.env_var).map(String::as_str),
    )
    .or_else(|| {
        options
            .stored_config
            .as_ref()
            .and_then(|s| s.provider_api_key(cap.slug))
    })
}

/// Resolves and validates the API key for `slug` against [`PROVIDER_CAPABILITIES`].
pub(crate) fn resolve_provider_api_key(
    slug: &str,
    options: &ConfigureOptions,
) -> Result<String, ConfigureError> {
    if let Some(cap) = provider_capability(slug)
        && let Some(secret) = resolve_provider_credential(cap, options)
    {
        return Ok(secret.to_string());
    }
    Err(missing_explicit_provider_key_error(slug))
}

/// Resolves the localhost decision endpoint following standard precedence (`CLI flag -> env -> config.json -> default`).
pub fn resolve_local_decision_endpoint(options: &ConfigureOptions) -> String {
    resolve_non_empty(
        options.local_decision_endpoint.as_deref(),
        options
            .env
            .get(ENV_CANON_CLERK_LOCAL_DECISION_ENDPOINT)
            .map(String::as_str),
    )
    .or_else(|| {
        options
            .env
            .get(ENV_LOCAL_DECISION_ENDPOINT)
            .map(String::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    })
    .or_else(|| {
        options
            .stored_config
            .as_ref()
            .and_then(StoredConfig::local_decision_endpoint)
    })
    .unwrap_or(DEFAULT_LOCAL_DECISION_ENDPOINT)
    .to_string()
}

fn missing_explicit_provider_key_error(slug: &str) -> ConfigureError {
    match slug {
        DEFAULT_PROVIDER => ConfigureError::MissingApiKey,
        TYPESAFE_PROVIDER => ConfigureError::MissingTypesafeApiKey,
        other => ConfigureError::MissingCredentials(MissingCredentialsReason::ProviderKey {
            provider: other.to_string(),
        }),
    }
}

/// Deterministically resolves `(screener_model, auditor_model)` from `table` and available credentials
/// in `options` (`docs/architecture/nodes/configure.md` §4).
fn resolve_default_tiers_with_table(
    table: &[ProviderCapability],
    options: &ConfigureOptions,
) -> (String, String) {
    let screener_model = table
        .iter()
        .filter(|cap| {
            cap.default_screener_model.is_some()
                && resolve_provider_credential(cap, options).is_some()
        })
        .min_by_key(|cap| {
            (
                !cap.default_screener_model
                    .is_some_and(is_discriminative_model),
                cap.rank,
                cap.slug,
            )
        })
        .or_else(|| {
            table
                .iter()
                .filter(|cap| {
                    cap.role == ProviderRole::GenerativeFullStack
                        && cap.default_screener_model.is_some()
                })
                .min_by_key(|cap| (cap.rank, cap.slug))
        })
        .and_then(|cap| cap.default_screener_model)
        .unwrap_or(DEFAULT_SCREENER_MODEL)
        .to_string();

    let auditor_model = table
        .iter()
        .filter(|cap| {
            cap.role == ProviderRole::GenerativeFullStack
                && cap.default_auditor_model.is_some()
                && resolve_provider_credential(cap, options).is_some()
        })
        .min_by_key(|cap| (cap.rank, cap.slug))
        .or_else(|| {
            table
                .iter()
                .filter(|cap| {
                    cap.role == ProviderRole::GenerativeFullStack
                        && cap.default_auditor_model.is_some()
                })
                .min_by_key(|cap| (cap.rank, cap.slug))
        })
        .and_then(|cap| cap.default_auditor_model)
        .unwrap_or(DEFAULT_AUDITOR_MODEL)
        .to_string();

    (screener_model, auditor_model)
}

/// Deterministically resolves default `(screener_model, auditor_model)` from [`PROVIDER_CAPABILITIES`].
pub(crate) fn resolve_default_tiers(options: &ConfigureOptions) -> (String, String) {
    resolve_default_tiers_with_table(PROVIDER_CAPABILITIES, options)
}

/// Returns the deduplicated, lexicographically sorted credential environment variable identifiers
/// (`"GEMINI_API_KEY"`, `"TYPESAFE_API_KEY"`, etc.) verified for the active tiers on `config`.
pub(crate) fn verified_credential_env_vars(config: &CaseloadConfig) -> Vec<&'static str> {
    let mut vars = BTreeSet::new();
    for slug in [
        model_provider(&config.screener_model),
        model_provider(&config.auditor_model),
    ] {
        if let Some(cap) = provider_capability(slug) {
            vars.insert(cap.env_var);
        }
    }
    vars.into_iter().collect()
}

pub(crate) struct ResolvedTierPinState<'a> {
    pub screener_model: &'a str,
    pub screener_was_pinned: bool,
    pub auditor_model: &'a str,
    pub auditor_was_pinned: bool,
    pub check_auditor_creds: bool,
}

pub(crate) fn verify_closure_credentials(
    state: &ResolvedTierPinState<'_>,
    options: &ConfigureOptions,
) -> Result<(), ConfigureError> {
    let screener_prov = model_provider(state.screener_model);
    if screener_prov != LOCAL_PROVIDER
        && let Err(screener_err) = resolve_provider_api_key(screener_prov, options)
    {
        if state.screener_was_pinned {
            return Err(screener_err);
        }
        if state.check_auditor_creds && state.auditor_was_pinned {
            let auditor_prov = model_provider(state.auditor_model);
            return Err(resolve_provider_api_key(auditor_prov, options)
                .err()
                .unwrap_or(screener_err));
        }
        if !state.check_auditor_creds {
            return Err(ConfigureError::MissingCredentials(
                MissingCredentialsReason::ScreenerClosure,
            ));
        }
        return Err(ConfigureError::MissingCredentials(
            MissingCredentialsReason::FullBenchClosure,
        ));
    }

    if state.check_auditor_creds {
        let auditor_prov = model_provider(state.auditor_model);
        if let Err(auditor_err) = resolve_provider_api_key(auditor_prov, options) {
            if state.auditor_was_pinned {
                return Err(auditor_err);
            }
            return Err(ConfigureError::MissingCredentials(
                MissingCredentialsReason::GenerativeAuditorRequired {
                    active_screener_model: state.screener_model.to_string(),
                },
            ));
        }
    }

    Ok(())
}

fn join_or_backticked(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [only] => format!("`{only}`"),
        [first, second] => format!("`{first}` or `{second}`"),
        _ => {
            let head: Vec<String> = items[..items.len() - 1]
                .iter()
                .map(|s| format!("`{s}`"))
                .collect();
            format!("{}, or `{}`", head.join(", "), items[items.len() - 1])
        }
    }
}

fn format_provider_sources(caps: &[&ProviderCapability]) -> String {
    let parts: Vec<String> = caps
        .iter()
        .map(|cap| {
            format!(
                "`{}=\"<your-api-key>\"` (`{} <KEY>` / `{}`)",
                cap.env_var, cap.cli_flag, cap.config_key_path
            )
        })
        .collect();
    parts.join(" or ")
}

pub(crate) fn format_missing_credentials(
    reason: &MissingCredentialsReason,
    f: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    let screener_caps: Vec<&ProviderCapability> = PROVIDER_CAPABILITIES
        .iter()
        .filter(|cap| cap.default_screener_model.is_some())
        .collect();
    let generative_caps: Vec<&ProviderCapability> = PROVIDER_CAPABILITIES
        .iter()
        .filter(|cap| {
            cap.role == ProviderRole::GenerativeFullStack && cap.default_auditor_model.is_some()
        })
        .collect();
    let disc_caps: Vec<&ProviderCapability> = PROVIDER_CAPABILITIES
        .iter()
        .filter(|cap| cap.role == ProviderRole::DiscriminativeScreener)
        .collect();

    let screener_envs: Vec<&str> = screener_caps.iter().map(|c| c.env_var).collect();
    let generative_envs: Vec<&str> = generative_caps.iter().map(|c| c.env_var).collect();
    let disc_envs: Vec<&str> = disc_caps.iter().map(|c| c.env_var).collect();

    match reason {
        MissingCredentialsReason::ScreenerClosure => write!(
            f,
            "Missing required screener model credentials ({}).\n  \
             Hint: Export {} in `~/.config/canon-clerk/config.json` (mode `0600`).",
            join_or_backticked(&screener_envs),
            format_provider_sources(&screener_caps),
        ),
        MissingCredentialsReason::FullBenchClosure => {
            let disc_suffix = if disc_envs.is_empty() {
                String::new()
            } else {
                format!(
                    "; optionally paired with {} for discriminative screening",
                    join_or_backticked(&disc_envs)
                )
            };
            let disc_hint = if disc_caps.is_empty() {
                String::new()
            } else {
                format!(
                    ", and optionally set {} to auto-select discriminative screening and `vet`",
                    format_provider_sources(&disc_caps)
                )
            };
            write!(
                f,
                "Missing required model provider credentials ({}{disc_suffix}).\n  \
                 Hint: Export {} in `~/.config/canon-clerk/config.json` (mode `0600`){disc_hint}.",
                join_or_backticked(&generative_envs),
                format_provider_sources(&generative_caps),
            )
        }
        MissingCredentialsReason::GenerativeAuditorRequired {
            active_screener_model,
        } => write!(
            f,
            "Missing required generative auditor credentials ({}): discriminative screener `{active_screener_model}` \
             covers screening stages (`apprise`, `docket`, `admit`, `vet`), but full readiness and substantive `audit` \
             adjudication require a generative auditor model.\n  \
             Hint: Export {} in `~/.config/canon-clerk/config.json` (mode `0600`), \
             or run `canon-clerk vet` for screener-only prima facie vetting.",
            join_or_backticked(&generative_envs),
            format_provider_sources(&generative_caps),
        ),
        MissingCredentialsReason::ProviderKey { provider } => {
            if let Some(cap) = provider_capability(provider) {
                write!(
                    f,
                    "Missing required {} API key (`{}`).\n  \
                     Hint: Export `{}=\"<your-api-key>\"`, configure `{}` in \
                     `~/.config/canon-clerk/config.json` (mode `0600`), or pass `{} <KEY>`.",
                    cap.display_name, cap.env_var, cap.env_var, cap.config_key_path, cap.cli_flag
                )
            } else {
                write!(
                    f,
                    "Missing required API key for provider `{provider}`.\n  \
                     Hint: Configure `providers.{provider}.apiKey` in `~/.config/canon-clerk/config.json` (mode `0600`)."
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configure::{StoredConfig, StoredProviderConfig};
    use std::collections::BTreeMap;

    mod matrix_resolution {
        use super::*;

        #[test]
        fn test_google_only_credentials_select_gemini_decision_and_carbon() {
            let mut env = BTreeMap::new();
            env.insert(ENV_GEMINI_API_KEY.to_string(), "gemini-key".to_string());
            let opts = ConfigureOptions {
                env,
                ..ConfigureOptions::default()
            };
            assert_eq!(
                resolve_default_tiers(&opts),
                (
                    DEFAULT_SCREENER_MODEL.to_string(),
                    DEFAULT_AUDITOR_MODEL.to_string()
                )
            );
        }

        #[test]
        fn test_hybrid_google_and_typesafe_credentials_select_gemini_decision_and_carbon_across_sources()
         {
            // 1. Both via env -> google:gemini-decision-1 wins rank 1 ahead of typesafe:jev-latest
            let mut env = BTreeMap::new();
            env.insert(ENV_GEMINI_API_KEY.to_string(), "gemini-key".to_string());
            env.insert(ENV_TYPESAFE_API_KEY.to_string(), "typesafe-key".to_string());
            assert_eq!(
                resolve_default_tiers(&ConfigureOptions {
                    env,
                    ..ConfigureOptions::default()
                }),
                (
                    DEFAULT_SCREENER_MODEL.to_string(),
                    DEFAULT_AUDITOR_MODEL.to_string()
                )
            );

            // 2. TypeSafe in config.json + Gemini via CLI flag -> google:gemini-decision-1 wins rank 1
            let mut providers = BTreeMap::new();
            providers.insert(
                TYPESAFE_PROVIDER.to_string(),
                StoredProviderConfig {
                    api_key: Some("stored-ts-key".to_string()),
                    ..StoredProviderConfig::default()
                },
            );
            let opts = ConfigureOptions {
                gemini_api_key: Some("cli-gemini-key".to_string()),
                stored_config: Some(StoredConfig {
                    providers,
                    ..StoredConfig::default()
                }),
                ..ConfigureOptions::default()
            };
            assert_eq!(
                resolve_default_tiers(&opts),
                (
                    DEFAULT_SCREENER_MODEL.to_string(),
                    DEFAULT_AUDITOR_MODEL.to_string()
                )
            );
        }

        #[test]
        fn test_typesafe_only_selects_jev_screener_and_default_generative_auditor() {
            let mut env = BTreeMap::new();
            env.insert(ENV_TYPESAFE_API_KEY.to_string(), "ts-only-key".to_string());
            let opts = ConfigureOptions {
                env,
                ..ConfigureOptions::default()
            };
            assert_eq!(
                resolve_default_tiers(&opts),
                (
                    DEFAULT_JEV_MODEL.to_string(),
                    DEFAULT_AUDITOR_MODEL.to_string()
                )
            );
        }
    }

    mod multi_provider_extensibility_and_determinism {
        use super::*;

        const EXTENDED_TABLE: &[ProviderCapability] = &[
            ProviderCapability {
                slug: "acme",
                display_name: "Acme AI",
                role: ProviderRole::GenerativeFullStack,
                rank: 3,
                cli_flag: "--acme-api-key",
                env_var: "ACME_API_KEY",
                config_key_path: "providers.acme.apiKey",
                default_screener_model: Some("acme:swift-1"),
                default_auditor_model: Some("acme:reasoner-1"),
            },
            PROVIDER_CAPABILITIES[0], // google (rank 1)
            PROVIDER_CAPABILITIES[1], // typesafe (rank 2)
        ];

        #[test]
        fn test_declarative_third_provider_works_solo_and_pairs_with_jev() {
            // Solo 3rd generative provider (via env or config.json) selects both acme tiers
            let mut acme_only = BTreeMap::new();
            acme_only.insert("ACME_API_KEY".to_string(), "acme-secret".to_string());
            assert_eq!(
                resolve_default_tiers_with_table(
                    EXTENDED_TABLE,
                    &ConfigureOptions {
                        env: acme_only.clone(),
                        ..ConfigureOptions::default()
                    }
                ),
                ("acme:swift-1".to_string(), "acme:reasoner-1".to_string())
            );

            // TypeSafe + 3rd generative provider pairs jev-latest with acme:reasoner-1
            let mut ts_and_acme = acme_only;
            ts_and_acme.insert(ENV_TYPESAFE_API_KEY.to_string(), "ts-secret".to_string());
            assert_eq!(
                resolve_default_tiers_with_table(
                    EXTENDED_TABLE,
                    &ConfigureOptions {
                        env: ts_and_acme,
                        ..ConfigureOptions::default()
                    }
                ),
                (DEFAULT_JEV_MODEL.to_string(), "acme:reasoner-1".to_string())
            );
        }

        #[test]
        fn test_multi_generative_provider_tie_breaking_is_deterministic_independent_of_table_or_env_order()
         {
            // Both google (rank 2) and acme (rank 3) present; google wins deterministically even when
            // acme appears earlier in EXTENDED_TABLE and alphabetically earlier in BTreeMap env!
            let mut both_gen = BTreeMap::new();
            both_gen.insert("ACME_API_KEY".to_string(), "acme-secret".to_string());
            both_gen.insert(ENV_GEMINI_API_KEY.to_string(), "google-secret".to_string());

            let reversed_table = [
                EXTENDED_TABLE[1], // google
                EXTENDED_TABLE[2], // typesafe
                EXTENDED_TABLE[0], // acme
            ];

            let opts = ConfigureOptions {
                env: both_gen,
                ..ConfigureOptions::default()
            };
            let forward = resolve_default_tiers_with_table(EXTENDED_TABLE, &opts);
            let reverse = resolve_default_tiers_with_table(&reversed_table, &opts);
            assert_eq!(forward, reverse);
            assert_eq!(
                forward,
                (
                    DEFAULT_SCREENER_MODEL.to_string(),
                    DEFAULT_AUDITOR_MODEL.to_string()
                )
            );
        }

        #[test]
        fn test_local_provider_credential_free_and_hybrid_closures() {
            use crate::configure::execute_configure;

            let offline_opts = ConfigureOptions {
                screener_model: Some("local:qwen2.5-coder-1.5b-instruct".to_string()),
                screener_only_closure: true,
                ..ConfigureOptions::default()
            };
            let offline_cfg = execute_configure(&offline_opts, None)
                .expect("local screener-only closure must succeed offline without API keys")
                .config
                .expect("config");
            assert_eq!(
                offline_cfg.screener_model,
                "local:qwen2.5-coder-1.5b-instruct"
            );

            let full_no_key = ConfigureOptions {
                screener_model: Some("local:qwen2.5-coder-1.5b-instruct".to_string()),
                screener_only_closure: false,
                ..ConfigureOptions::default()
            };
            assert!(matches!(
                execute_configure(&full_no_key, None),
                Err(ConfigureError::MissingCredentials(
                    MissingCredentialsReason::GenerativeAuditorRequired { .. }
                ))
            ));

            let mut gemini_env = BTreeMap::new();
            gemini_env.insert(ENV_GEMINI_API_KEY.to_string(), "gemini-key".to_string());
            let hybrid_cfg = execute_configure(
                &ConfigureOptions {
                    env: gemini_env,
                    ..full_no_key
                },
                None,
            )
            .expect("hybrid local screener + gemini auditor must succeed")
            .config
            .expect("config");
            assert_eq!(
                verified_credential_env_vars(&hybrid_cfg),
                vec![ENV_GEMINI_API_KEY]
            );

            let bad_auditor = ConfigureOptions {
                auditor_model: Some("local:qwen2.5-coder-1.5b-instruct".to_string()),
                screener_only_closure: true,
                ..ConfigureOptions::default()
            };
            assert!(matches!(
                execute_configure(&bad_auditor, None),
                Err(ConfigureError::DiscriminativeAuditorUnsupported { .. })
            ));
        }

        #[test]
        fn test_local_provider_endpoint_precedence() {
            assert_eq!(
                resolve_local_decision_endpoint(&ConfigureOptions::default()),
                DEFAULT_LOCAL_DECISION_ENDPOINT
            );
            let stored = StoredConfig {
                providers: BTreeMap::from([(
                    LOCAL_PROVIDER.to_string(),
                    StoredProviderConfig {
                        api_key: None,
                        endpoint: Some("http://127.0.0.1:8081".to_string()),
                    },
                )]),
                ..StoredConfig::default()
            };
            assert_eq!(
                resolve_local_decision_endpoint(&ConfigureOptions {
                    stored_config: Some(stored.clone()),
                    ..ConfigureOptions::default()
                }),
                "http://127.0.0.1:8081"
            );
            let mut env = BTreeMap::new();
            env.insert(
                ENV_LOCAL_DECISION_ENDPOINT.to_string(),
                "http://127.0.0.1:8082".to_string(),
            );
            assert_eq!(
                resolve_local_decision_endpoint(&ConfigureOptions {
                    env: env.clone(),
                    stored_config: Some(stored.clone()),
                    ..ConfigureOptions::default()
                }),
                "http://127.0.0.1:8082"
            );
            env.insert(
                ENV_CANON_CLERK_LOCAL_DECISION_ENDPOINT.to_string(),
                "http://127.0.0.1:8083".to_string(),
            );
            assert_eq!(
                resolve_local_decision_endpoint(&ConfigureOptions {
                    env: env.clone(),
                    stored_config: Some(stored.clone()),
                    ..ConfigureOptions::default()
                }),
                "http://127.0.0.1:8083"
            );
            assert_eq!(
                resolve_local_decision_endpoint(&ConfigureOptions {
                    local_decision_endpoint: Some("http://127.0.0.1:8084".to_string()),
                    env,
                    stored_config: Some(stored),
                    ..ConfigureOptions::default()
                }),
                "http://127.0.0.1:8084"
            );
        }
    }
}
