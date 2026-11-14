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

use crate::caseload::{Caseload, CaseloadConfig, DEFAULT_PROVIDER, TYPESAFE_PROVIDER};
use crate::cli::CliError;
use crate::cli::args::{CaseloadIoArgs, ProviderConfigArgs};
use crate::cli::runners::{resolve_branch_b_options, run_branch_b_configure};
use crate::configure::{ENV_GEMINI_API_KEY, ENV_TYPESAFE_API_KEY, model_provider};
use crate::telemetry::TelemetrySession;
use clap::{Args, ValueEnum};
use std::collections::BTreeMap;
use std::env;
use std::io::{Read, Write};

const CONFIGURE_EXAMPLES: &str = "\
Examples:
  # Verify local credentials and view normalized model specifiers:
  canon-clerk configure

  # Configure custom screener and auditor models via aliases:
  canon-clerk configure --screener-model flash-lite --auditor-model pro

  # Configure TypeSafe Jev as fast screener alongside Gemini Pro auditor:
  canon-clerk configure --screener-model jev --auditor-model pro

  # Configure custom models via fully-qualified specifiers:
  canon-clerk configure --screener-model google:gemini-2.5-flash-lite --auditor-model google:gemini-2.5-pro

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
    let mut keys = Vec::new();
    if model_provider(&config.screener_model) == DEFAULT_PROVIDER
        || model_provider(&config.auditor_model) == DEFAULT_PROVIDER
    {
        keys.push(ENV_GEMINI_API_KEY);
    }
    if model_provider(&config.screener_model) == TYPESAFE_PROVIDER
        || model_provider(&config.auditor_model) == TYPESAFE_PROVIDER
    {
        keys.push(ENV_TYPESAFE_API_KEY);
    }
    format!("{} (verified)", keys.join(", "))
}

fn render_compact_output(config: &CaseloadConfig, stdout: &mut dyn Write) -> std::io::Result<()> {
    writeln!(stdout, "screener_model\t{}", config.screener_model)?;
    writeln!(stdout, "auditor_model\t{}", config.auditor_model)?;
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
    match config.reasoning_budget {
        Some(budget) => writeln!(stdout, "  Reasoning Budget : {budget} tokens")?,
        None => writeln!(stdout, "  Reasoning Budget : default (unbounded)")?,
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
    use clap::Parser;
    use std::io::Cursor;

    fn test_env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            ENV_GEMINI_API_KEY.to_string(),
            "synthetic-test-api-key-value".to_string(),
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
                "flash-lite",
                "--auditor-model",
                "pro",
                "--reasoning-budget",
                "4096",
                "--json",
            ])
            .expect("parse configure subcommand");

            match cli.command {
                Some(Commands::Configure(args)) => {
                    assert_eq!(args.provider.screener_model.as_deref(), Some("flash-lite"));
                    assert_eq!(args.provider.auditor_model.as_deref(), Some("pro"));
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
                    screener_model: Some("flash-lite".to_string()),
                    auditor_model: Some("pro".to_string()),
                    reasoning_budget: Some(2048),
                    gemini_api_key: Some(secret.to_string()),
                    typesafe_api_key: None,
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
            assert!(compact_str.contains("screener_model\tgoogle:gemini-flash-lite-latest"));
            assert!(compact_str.contains("auditor_model\tgoogle:gemini-pro-latest"));
            assert!(compact_str.contains("reasoning_budget\t1024"));
            assert!(!compact_str.contains(secret));
        }

        #[test]
        fn test_hybrid_typesafe_jev_screener_verifies_both_keys_without_leakage() {
            let ts_secret = "synthetic-typesafe-cli-secret-never-echo";
            let args = ConfigureArgs {
                provider: ProviderConfigArgs {
                    screener_model: Some("jev".to_string()),
                    typesafe_api_key: Some(ts_secret.to_string()),
                    ..ProviderConfigArgs::default()
                },
                format: ConfigureFormat::Stylish,
                ..ConfigureArgs::default()
            };
            let mut stdin = Cursor::new(b"");
            let mut out = Vec::new();
            let caseload = run_configure_with_env(&args, &mut stdin, &mut out, &test_env())
                .expect("hybrid configure should succeed");
            let rendered = String::from_utf8(out).expect("utf8");
            assert!(rendered.contains("typesafe:jev-latest"));
            assert!(rendered.contains("GEMINI_API_KEY, TYPESAFE_API_KEY (verified)"));
            assert!(!rendered.contains(ts_secret));
            assert_eq!(
                caseload.config.expect("config").screener_model,
                "typesafe:jev-latest"
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
            let empty_env = BTreeMap::new();

            let err = run_configure_with_env(&args, &mut stdin, &mut stdout, &empty_env)
                .expect_err("missing key must fail");
            assert_eq!(err.exit_code(), 2);
            assert!(err.to_string().contains("Hint:"));
        }

        #[test]
        fn test_caseload_stdin_fast_forwards_when_config_already_present() {
            let upstream = Caseload::new().with_config(CaseloadConfig {
                screener_model: "google:gemini-2.5-flash-lite".to_string(),
                auditor_model: "google:gemini-2.5-pro".to_string(),
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
