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

//! CLI driving adapter for the `canon-clerk configure` subcommand.

use crate::caseload::{Caseload, CaseloadConfig, DEFAULT_AUDITOR_THINKING_LEVEL};
use crate::cli::CliError;
use crate::cli::args::{CaseloadIoArgs, ProviderConfigArgs};
use crate::cli::runners::{resolve_branch_b_options, run_branch_b_configure};
use crate::configure::verified_credential_env_vars;
use crate::telemetry::TelemetrySession;
use clap::{Args, ValueEnum};
use std::collections::BTreeMap;
use std::env;
use std::io::{Read, Write};

const CONFIGURE_EXAMPLES: &str = "\
Examples:
  # Auto-select optimal screener and auditor tiers from available credentials:
  canon-clerk configure

  # Explicitly pin screener and auditor models via provider-omitted upstream IDs:
  canon-clerk configure --screener-model gemini-4-hydrogen --auditor-model gemini-4-carbon

  # Explicitly pair TypeSafe Jev fast screener with Gemini 4 Carbon auditor:
  canon-clerk configure --screener-model jev-latest --auditor-model gemini-4-carbon

  # Configure custom models via fully-qualified specifiers:
  canon-clerk configure --screener-model google:gemini-4-hydrogen --auditor-model google:gemini-4-carbon

  # Configure maximum reasoning budget tokens:
  canon-clerk configure --reasoning-budget 4096

  # Emit tab-separated configuration key-value lines:
  canon-clerk configure --format compact

  # Ingest upstream Caseload JSON and emit enriched envelope to stdout:
  canon-clerk configure --caseload caseload-3.json --json";

/// Output presentation format for `canon-clerk configure`.
#[derive(ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfigureFormat {
    /// Human-readable configuration summary with redacted credential status.
    #[default]
    Stylish,
    /// Full cumulative `Caseload` JSON envelope.
    Json,
    /// Tab-separated (`<key>\t<value>`) one-line-per-setting output.
    Compact,
}

/// Command-line arguments for `canon-clerk configure`.
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
#[command(
    about = "Resolve and normalize provider credentials, model specifiers, and reasoning budgets",
    long_about = "Operational Environment Normalizer of the Caseload DAG (Branch B). Verifies active runtime provider credentials (GEMINI_API_KEY, TYPESAFE_API_KEY), normalizes screener and auditor model specifiers into canonical provider:model identifiers, and populates `.config` on the cumulative Caseload without persisting secret material.",
    after_help = CONFIGURE_EXAMPLES
)]
pub struct ConfigureArgs {
    #[command(flatten)]
    pub provider: ProviderConfigArgs,

    /// Output format (`stylish`, `json`, or `compact`).
    #[arg(
        long = "format",
        value_enum,
        default_value_t = ConfigureFormat::Stylish,
        help_heading = "Caseload & Output",
        help = "Format resolved configuration output"
    )]
    pub format: ConfigureFormat,

    #[command(flatten)]
    pub io: CaseloadIoArgs,
}

/// Executes the `configure` CLI subcommand against an explicit environment map and [`TelemetrySession`].
pub fn run_configure_with_telemetry(
    args: &ConfigureArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    env: &BTreeMap<String, String>,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    let incoming_caseload = args.io.read_caseload(stdin)?;
    let options = resolve_branch_b_options(&args.provider, env)?;
    let caseload = run_branch_b_configure(&options, incoming_caseload, telemetry)?;
    telemetry.finish_pipeline();

    args.io.write_output_caseload(&caseload)?;

    let config = caseload.config.clone().unwrap_or_default();
    render_configure_output(args, &caseload, &config, stdout)?;

    Ok(caseload)
}

/// Executes the `configure` CLI subcommand against an explicit environment variable map.
pub fn run_configure_with_env(
    args: &ConfigureArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    env: &BTreeMap<String, String>,
) -> Result<Caseload, CliError> {
    run_configure_with_telemetry(args, stdin, stdout, env, &TelemetrySession::noop())
}

/// Executes the `configure` CLI subcommand using the process environment.
pub fn run_configure(
    args: &ConfigureArgs,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<Caseload, CliError> {
    let env: BTreeMap<String, String> = env::vars().collect();
    run_configure_with_env(args, stdin, stdout, &env)
}

fn render_configure_output(
    args: &ConfigureArgs,
    caseload: &Caseload,
    config: &CaseloadConfig,
    stdout: &mut dyn Write,
) -> Result<(), CliError> {
    let effective_format = if args.io.json {
        ConfigureFormat::Json
    } else {
        args.format
    };

    match effective_format {
        ConfigureFormat::Json => {
            let serialized = serde_json::to_string_pretty(caseload).map_err(|err| {
                CliError::runtime(format!("Failed to serialize Caseload JSON: {err}"))
            })?;
            writeln!(stdout, "{serialized}")
                .map_err(|err| CliError::runtime(format!("Failed to write to stdout: {err}")))?;
        }
        ConfigureFormat::Compact => {
            render_compact_output(config, stdout)
                .map_err(|err| CliError::runtime(format!("Failed to write to stdout: {err}")))?;
        }
        ConfigureFormat::Stylish => {
            render_stylish_output(config, stdout)
                .map_err(|err| CliError::runtime(format!("Failed to write to stdout: {err}")))?;
        }
    }

    Ok(())
}

fn verified_credentials_label(config: &CaseloadConfig) -> String {
    let keys = verified_credential_env_vars(config);
    if keys.is_empty() {
        "none required (local)".to_string()
    } else {
        format!("{} (verified)", keys.join(", "))
    }
}

fn render_compact_output(config: &CaseloadConfig, stdout: &mut dyn Write) -> std::io::Result<()> {
    writeln!(stdout, "screener_model\t{}", config.screener_model)?;
    writeln!(stdout, "auditor_model\t{}", config.auditor_model)?;
    if let Some(level) = config.thinking_level {
        writeln!(stdout, "thinking_level\t{level}")?;
    }
    if let Some(budget) = config.reasoning_budget {
        writeln!(stdout, "reasoning_budget\t{budget}")?;
    }
    writeln!(
        stdout,
        "credentials\t{}",
        verified_credentials_label(config)
    )?;
    Ok(())
}

fn render_stylish_output(config: &CaseloadConfig, stdout: &mut dyn Write) -> std::io::Result<()> {
    writeln!(stdout, "Configuration Summary (READY):")?;
    writeln!(stdout, "  Screener Model   : {}", config.screener_model)?;
    writeln!(stdout, "  Auditor Model    : {}", config.auditor_model)?;
    match (config.thinking_level, config.reasoning_budget) {
        (Some(level), _) => writeln!(stdout, "  Thinking Level   : {level}")?,
        (None, Some(budget)) => writeln!(stdout, "  Reasoning Budget : {budget} tokens")?,
        (None, None) => writeln!(
            stdout,
            "  Thinking Level   : {DEFAULT_AUDITOR_THINKING_LEVEL} (audit default)"
        )?,
    }
    writeln!(
        stdout,
        "  Credentials      : {}",
        verified_credentials_label(config)
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caseload::{DEFAULT_AUDITOR_MODEL, DEFAULT_SCREENER_MODEL};
    use crate::cli::{Cli, Commands};
    use crate::configure::{ENV_GEMINI_API_KEY, ENV_TYPESAFE_API_KEY};
    use clap::Parser;
    use std::io::Cursor;

    fn test_env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            ENV_GEMINI_API_KEY.to_string(),
            "synthetic-test-api-key-value".to_string(),
        );
        env.insert(
            "XDG_CONFIG_HOME".to_string(),
            "/nonexistent-canon-clerk-xdg-dir".to_string(),
        );
        env
    }

    mod cli_argument_parsing_and_help {
        use super::*;

        #[test]
        fn test_parse_configure_subcommand_flags() {
            let cli = Cli::try_parse_from([
                "canon-clerk",
                "configure",
                "--screener-model",
                "gemini-2.5-flash-lite",
                "--auditor-model",
                "gemini-2.5-pro",
                "--reasoning-budget",
                "4096",
                "--json",
            ])
            .expect("parse configure subcommand");

            match cli.command {
                Some(Commands::Configure(args)) => {
                    assert_eq!(
                        args.provider.screener_model.as_deref(),
                        Some("gemini-2.5-flash-lite")
                    );
                    assert_eq!(
                        args.provider.auditor_model.as_deref(),
                        Some("gemini-2.5-pro")
                    );
                    assert_eq!(args.provider.reasoning_budget, Some(4096));
                    assert!(args.io.json);
                }
                _ => panic!("expected Configure subcommand"),
            }
        }
    }

    mod execution_and_output_formats {
        use super::*;

        #[test]
        fn test_json_output_emits_normalized_caseload_without_key_leakage() {
            let secret = "synthetic-top-secret-never-echo-999";
            let args = ConfigureArgs {
                provider: ProviderConfigArgs {
                    screener_model: Some("gemini-decision-1".to_string()),
                    auditor_model: Some("gemini-4-carbon".to_string()),
                    thinking_level: None,
                    reasoning_budget: Some(2048),
                    gemini_api_key: Some(secret.to_string()),
                    typesafe_api_key: None,
                    local_decision_endpoint: None,
                },
                io: CaseloadIoArgs {
                    json: true,
                    ..CaseloadIoArgs::default()
                },
                ..ConfigureArgs::default()
            };
            let mut stdin = Cursor::new(b"");
            let mut stdout = Vec::new();
            let env = BTreeMap::new();

            let caseload = run_configure_with_env(&args, &mut stdin, &mut stdout, &env)
                .expect("run_configure_with_env");
            let out_str = String::from_utf8(stdout).expect("utf8 stdout");

            assert!(!out_str.contains(secret));
            let parsed: Caseload = serde_json::from_str(&out_str).expect("valid caseload json");
            assert_eq!(parsed, caseload);
            assert_eq!(
                parsed.config,
                Some(CaseloadConfig {
                    screener_model: DEFAULT_SCREENER_MODEL.to_string(),
                    auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
                    thinking_level: None,
                    reasoning_budget: Some(2048),
                })
            );
        }

        #[test]
        fn test_stylish_and_compact_formats_never_echo_api_key() {
            let secret = "synthetic-secret-key-material-should-stay-hidden";
            let mut env = BTreeMap::new();
            env.insert(ENV_GEMINI_API_KEY.to_string(), secret.to_string());

            let stylish_args = ConfigureArgs {
                format: ConfigureFormat::Stylish,
                ..ConfigureArgs::default()
            };
            let mut stdin = Cursor::new(b"");
            let mut stylish_out = Vec::new();
            run_configure_with_env(&stylish_args, &mut stdin, &mut stylish_out, &env)
                .expect("stylish configure");
            let stylish_str = String::from_utf8(stylish_out).expect("utf8");
            assert!(stylish_str.contains("Configuration Summary (READY):"));
            assert!(stylish_str.contains("Thinking Level   : low (audit default)"));
            assert!(stylish_str.contains("GEMINI_API_KEY (verified)"));
            assert!(!stylish_str.contains(secret));

            let compact_args = ConfigureArgs {
                provider: ProviderConfigArgs {
                    reasoning_budget: Some(1024),
                    ..ProviderConfigArgs::default()
                },
                format: ConfigureFormat::Compact,
                ..ConfigureArgs::default()
            };
            let mut compact_out = Vec::new();
            run_configure_with_env(&compact_args, &mut stdin, &mut compact_out, &env)
                .expect("compact configure");
            let compact_str = String::from_utf8(compact_out).expect("utf8");
            assert!(compact_str.contains("screener_model\tgoogle:gemini-decision-1"));
            assert!(compact_str.contains("auditor_model\tgoogle:gemini-4-carbon"));
            assert!(compact_str.contains("reasoning_budget\t1024"));
            assert!(!compact_str.contains(secret));
        }

        #[test]
        fn test_hybrid_credentials_auto_select_gemini_decision_and_allow_explicit_jev_override() {
            let ts_secret = "synthetic-typesafe-cli-secret-never-echo";
            // 1. Unpinned hybrid credentials select google:gemini-decision-1 + google:gemini-4-carbon
            let args = ConfigureArgs {
                provider: ProviderConfigArgs {
                    typesafe_api_key: Some(ts_secret.to_string()),
                    ..ProviderConfigArgs::default()
                },
                format: ConfigureFormat::Stylish,
                ..ConfigureArgs::default()
            };
            let mut stdin = Cursor::new(b"");
            let mut out = Vec::new();
            let caseload = run_configure_with_env(&args, &mut stdin, &mut out, &test_env())
                .expect("hybrid configure should auto-select gemini-decision-1 + carbon");
            let rendered = String::from_utf8(out).expect("utf8");
            assert!(rendered.contains(DEFAULT_SCREENER_MODEL));
            assert!(rendered.contains(DEFAULT_AUDITOR_MODEL));
            assert!(rendered.contains("Credentials      : GEMINI_API_KEY (verified)"));
            assert!(!rendered.contains(ts_secret));
            assert_eq!(
                caseload.config,
                Some(CaseloadConfig {
                    screener_model: DEFAULT_SCREENER_MODEL.to_string(),
                    auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
                    thinking_level: None,
                    reasoning_budget: None,
                })
            );

            // 2. Explicit --screener-model jev-latest pins TypeSafe Jev and verifies both keys without leakage
            let compact_args = ConfigureArgs {
                provider: ProviderConfigArgs {
                    screener_model: Some("jev-latest".to_string()),
                    typesafe_api_key: Some(ts_secret.to_string()),
                    ..ProviderConfigArgs::default()
                },
                format: ConfigureFormat::Compact,
                ..ConfigureArgs::default()
            };
            let mut compact_out = Vec::new();
            run_configure_with_env(&compact_args, &mut stdin, &mut compact_out, &test_env())
                .expect("compact explicit jev configure");
            let compact_rendered = String::from_utf8(compact_out).expect("utf8");
            assert!(compact_rendered.contains("screener_model\ttypesafe:jev-latest"));
            assert!(compact_rendered.contains("auditor_model\tgoogle:gemini-4-carbon"));
            assert!(
                compact_rendered
                    .contains("credentials\tGEMINI_API_KEY, TYPESAFE_API_KEY (verified)")
            );
            assert!(!compact_rendered.contains(ts_secret));

            // 3. Explicit --screener-model gemini-4-hydrogen overrides default gemini-decision-1
            let mut both_env = test_env();
            both_env.insert(ENV_TYPESAFE_API_KEY.to_string(), ts_secret.to_string());
            let override_args = ConfigureArgs {
                provider: ProviderConfigArgs {
                    screener_model: Some("gemini-4-hydrogen".to_string()),
                    ..ProviderConfigArgs::default()
                },
                format: ConfigureFormat::Stylish,
                ..ConfigureArgs::default()
            };
            let mut override_out = Vec::new();
            let overridden =
                run_configure_with_env(&override_args, &mut stdin, &mut override_out, &both_env)
                    .expect("explicit flag overrides default screener");
            let override_str = String::from_utf8(override_out).expect("utf8");
            assert!(override_str.contains("google:gemini-4-hydrogen"));
            assert!(override_str.contains("Credentials      : GEMINI_API_KEY (verified)"));
            assert_eq!(
                overridden.config.expect("config").screener_model,
                "google:gemini-4-hydrogen"
            );
        }
    }

    mod error_handling_and_fast_forwarding {
        use super::*;

        #[test]
        fn test_missing_api_key_exits_with_code_2_and_hint() {
            let args = ConfigureArgs::default();
            let mut stdin = Cursor::new(b"");
            let mut stdout = Vec::new();
            let mut empty_env = BTreeMap::new();
            empty_env.insert(
                "XDG_CONFIG_HOME".to_string(),
                "/nonexistent-canon-clerk-xdg-dir".to_string(),
            );

            let err = run_configure_with_env(&args, &mut stdin, &mut stdout, &empty_env)
                .expect_err("missing key must fail");
            assert_eq!(err.exit_code(), 2);
            assert!(err.to_string().contains(ENV_GEMINI_API_KEY));
            assert!(err.to_string().contains("Hint:"));

            // TypeSafe-only env on full-bench `configure` exits 2 requesting generative auditor key
            let mut ts_only_env = empty_env;
            ts_only_env.insert(
                ENV_TYPESAFE_API_KEY.to_string(),
                "ts-only-secret".to_string(),
            );
            let ts_err = run_configure_with_env(&args, &mut stdin, &mut stdout, &ts_only_env)
                .expect_err("typesafe-only configure must exit 2");
            assert_eq!(ts_err.exit_code(), 2);
            assert!(ts_err.to_string().contains("generative auditor"));
            assert!(ts_err.to_string().contains(ENV_GEMINI_API_KEY));
            assert!(ts_err.to_string().contains("Hint:"));
        }

        #[test]
        fn test_conflicting_or_invalid_thinking_flags_exit_with_code_2_and_hint() {
            let args = ConfigureArgs {
                provider: ProviderConfigArgs {
                    thinking_level: Some("low".to_string()),
                    reasoning_budget: Some(2048),
                    ..ProviderConfigArgs::default()
                },
                ..ConfigureArgs::default()
            };
            let mut stdin = Cursor::new(b"");
            let mut stdout = Vec::new();
            let err = run_configure_with_env(&args, &mut stdin, &mut stdout, &test_env())
                .expect_err("conflicting thinking flags must fail");
            assert_eq!(err.exit_code(), 2);
            assert!(err.to_string().contains("mutually exclusive"));
            assert!(err.to_string().contains("Hint:"));

            let invalid_args = ConfigureArgs {
                provider: ProviderConfigArgs {
                    thinking_level: Some("turbo".to_string()),
                    ..ProviderConfigArgs::default()
                },
                ..ConfigureArgs::default()
            };
            let invalid_err =
                run_configure_with_env(&invalid_args, &mut stdin, &mut stdout, &test_env())
                    .expect_err("invalid thinking level must fail");
            assert_eq!(invalid_err.exit_code(), 2);
            assert!(invalid_err.to_string().contains("Invalid thinking level"));
            assert!(invalid_err.to_string().contains("Hint:"));

            for retired_alias in [
                "hydrogen",
                "carbon",
                "jev",
                "google:hydrogen",
                "typesafe:jev",
            ] {
                let alias_args = ConfigureArgs {
                    provider: ProviderConfigArgs {
                        screener_model: Some(retired_alias.to_string()),
                        ..ProviderConfigArgs::default()
                    },
                    ..ConfigureArgs::default()
                };
                let alias_err =
                    run_configure_with_env(&alias_args, &mut stdin, &mut stdout, &test_env())
                        .expect_err(&format!("retired alias `{retired_alias}` must exit 2"));
                assert_eq!(alias_err.exit_code(), 2);
                assert!(alias_err.to_string().contains("Invalid model specifier"));
                assert!(alias_err.to_string().contains("Hint:"));
            }
        }

        #[test]
        fn test_discriminative_auditor_and_bare_gemini_decision_exit_with_code_2_and_hint() {
            let mut stdin = Cursor::new(b"");
            let mut stdout = Vec::new();
            let mut both_env = test_env();
            both_env.insert(
                ENV_TYPESAFE_API_KEY.to_string(),
                "synthetic-ts-key".to_string(),
            );

            for disc_auditor in [
                "jev-latest",
                "gemini-decision-1",
                "google:gemini-decision-1",
            ] {
                let args = ConfigureArgs {
                    provider: ProviderConfigArgs {
                        auditor_model: Some(disc_auditor.to_string()),
                        ..ProviderConfigArgs::default()
                    },
                    ..ConfigureArgs::default()
                };
                let err = run_configure_with_env(&args, &mut stdin, &mut stdout, &both_env)
                    .expect_err(&format!(
                        "discriminative auditor `{disc_auditor}` must exit 2"
                    ));
                assert_eq!(err.exit_code(), 2);
                assert!(
                    err.to_string()
                        .contains("cannot be configured as `auditor_model`")
                );
                assert!(err.to_string().contains("Hint:"));
            }

            for bare in ["gemini-decision", "google:gemini-decision"] {
                let args = ConfigureArgs {
                    provider: ProviderConfigArgs {
                        screener_model: Some(bare.to_string()),
                        ..ProviderConfigArgs::default()
                    },
                    ..ConfigureArgs::default()
                };
                let err = run_configure_with_env(&args, &mut stdin, &mut stdout, &both_env)
                    .expect_err(&format!("bare `{bare}` must exit 2"));
                assert_eq!(err.exit_code(), 2);
                assert!(err.to_string().contains("Invalid model specifier"));
                assert!(err.to_string().contains("Hint:"));
            }
        }

        #[test]
        fn test_caseload_stdin_fast_forwards_when_config_already_present() {
            let upstream = Caseload::new().with_config(CaseloadConfig {
                screener_model: "google:gemini-2.5-flash-lite".to_string(),
                auditor_model: "google:gemini-2.5-pro".to_string(),
                thinking_level: None,
                reasoning_budget: Some(4096),
            });
            let upstream_json = serde_json::to_string(&upstream).expect("serialize upstream");

            let args = ConfigureArgs {
                io: CaseloadIoArgs {
                    caseload: Some("-".to_string()),
                    json: true,
                    ..CaseloadIoArgs::default()
                },
                ..ConfigureArgs::default()
            };
            let mut stdin = Cursor::new(upstream_json.into_bytes());
            let mut stdout = Vec::new();
            let empty_env = BTreeMap::new();

            let result = run_configure_with_env(&args, &mut stdin, &mut stdout, &empty_env)
                .expect("fast-forward on pre-populated caseload.config");
            assert_eq!(result, upstream);
        }

        #[test]
        fn test_enriches_upstream_branch_a_caseload_from_stdin() {
            let raw = r#"{"version":"1.0","intake":{"scope":"targeted","target_paths":["src/lib.rs"],"diffs":{}}}"#;
            let args = ConfigureArgs {
                io: CaseloadIoArgs {
                    caseload: Some("-".to_string()),
                    json: true,
                    ..CaseloadIoArgs::default()
                },
                ..ConfigureArgs::default()
            };
            let mut stdin = Cursor::new(raw.as_bytes());
            let mut stdout = Vec::new();

            let result = run_configure_with_env(&args, &mut stdin, &mut stdout, &test_env())
                .expect("enrich branch A caseload");
            assert!(result.intake.is_some());
            assert_eq!(result.config, Some(CaseloadConfig::default()));
        }
    }
}
