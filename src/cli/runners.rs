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

//! Shared upstream stage runners for Branch A (Filing) and Branch B (Environment) pipelines.

use crate::caseload::{Caseload, CaseloadConfig, DEFAULT_PROVIDER, TYPESAFE_PROVIDER};
use crate::cli::CliError;
use crate::cli::args::{CanonSelectionArgs, FilingSourceArgs, ProviderConfigArgs};
use crate::configure::{
    ConfigureOptions, execute_configure, load_host_config_file, model_provider,
    resolve_default_config_path,
};
use crate::discover::{DiscoverError, DiscoverOptions, execute_discover_on_disk};
use crate::intake::{IntakeError, IntakeOptions, execute_intake};
use crate::provider::MultiModelProvider;
use crate::telemetry::{PipelineStage, TelemetrySession};
use crate::validate::{ValidateError, ValidateOptions, execute_validate_on_disk};
use std::collections::BTreeMap;
use std::env;
use std::io::Read;
use std::path::Path;

/// Maps a domain [`DiscoverError`] to a typed [`CliError`] preserving exit code conventions (`1` IO/runtime, `2` usage).
pub fn map_discover_error(err: DiscoverError) -> CliError {
    match err {
        DiscoverError::MissingInputSource
        | DiscoverError::InvalidGlob(_)
        | DiscoverError::PathEscapesWorkspace { .. } => CliError::usage(err.to_string()),
        DiscoverError::Io { .. } => CliError::runtime(err.to_string()),
    }
}

fn map_intake_error(err: IntakeError) -> CliError {
    match err {
        IntakeError::MissingInputSource | IntakeError::MalformedDiff(_) => {
            CliError::usage(err.to_string())
        }
    }
}

fn execute_intake_step(
    intake_opts: &IntakeOptions,
    incoming: Option<Caseload>,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    telemetry.run_stage(PipelineStage::Intake, |_scope| {
        execute_intake(intake_opts, incoming).map_err(map_intake_error)
    })
}

/// Executes Branch A `intake` stage, honoring fast-forward skip semantics if `.intake` is already present.
pub fn run_branch_a_intake(
    filing: &FilingSourceArgs,
    incoming: Option<Caseload>,
    stdin: &mut dyn Read,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    let intake_opts = filing.resolve_intake_options(stdin)?;

    if intake_opts.has_designated_source() {
        execute_intake_step(&intake_opts, incoming, telemetry)
    } else if let Some(existing) = incoming {
        Ok(existing)
    } else {
        Err(CliError::usage(IntakeError::MissingInputSource.to_string()))
    }
}

/// Executes the Branch A `[intake, discover]` transitive closure with fast-forward skipping.
pub fn run_branch_a_discover(
    filing: &FilingSourceArgs,
    canons: &CanonSelectionArgs,
    incoming: Option<Caseload>,
    workspace_root: &Path,
    stdin: &mut dyn Read,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    let intake_opts = filing.resolve_intake_options(stdin)?;

    let caseload_after_intake = if intake_opts.has_designated_source() {
        execute_intake_step(&intake_opts, incoming, telemetry)?
    } else if let Some(existing) = incoming {
        existing
    } else if canons.all_canons {
        Caseload::new()
    } else {
        return Err(CliError::usage(
            DiscoverError::MissingInputSource.to_string(),
        ));
    };

    if caseload_after_intake.discovery.is_some() && canons.is_empty() {
        Ok(caseload_after_intake)
    } else {
        let discover_opts = DiscoverOptions {
            all_canons: canons.all_canons,
            canon_patterns: canons.canon.clone(),
        };
        telemetry.run_stage(PipelineStage::Discover, |_scope| {
            execute_discover_on_disk(&discover_opts, caseload_after_intake, workspace_root)
                .map_err(map_discover_error)
        })
    }
}

/// Executes the Branch A `[intake, discover, validate]` transitive closure with fast-forward skipping.
pub fn run_branch_a_validate(
    filing: &FilingSourceArgs,
    canons: &CanonSelectionArgs,
    incoming: Option<Caseload>,
    workspace_root: &Path,
    stdin: &mut dyn Read,
    max_warnings: Option<usize>,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    let intake_opts = filing.resolve_intake_options(stdin)?;

    let caseload_after_intake = if intake_opts.has_designated_source() {
        execute_intake_step(&intake_opts, incoming, telemetry)?
    } else if let Some(existing) = incoming {
        existing
    } else if canons.all_canons || !canons.canon.is_empty() {
        Caseload::new()
    } else {
        return Err(CliError::usage(ValidateError::MissingDiscovery.to_string()));
    };

    let caseload_after_discover = if caseload_after_intake.discovery.is_some() && canons.is_empty()
    {
        caseload_after_intake
    } else {
        let discover_opts = DiscoverOptions {
            all_canons: canons.all_canons
                || (caseload_after_intake.intake.is_none() && !canons.canon.is_empty()),
            canon_patterns: canons.canon.clone(),
        };
        telemetry.run_stage(PipelineStage::Discover, |_scope| {
            execute_discover_on_disk(&discover_opts, caseload_after_intake, workspace_root)
                .map_err(map_discover_error)
        })?
    };

    if caseload_after_discover.validation.is_some() && canons.is_empty() && max_warnings.is_none() {
        Ok(caseload_after_discover)
    } else {
        let validate_opts = ValidateOptions { max_warnings };
        telemetry.run_stage(PipelineStage::Validate, |_scope| {
            execute_validate_on_disk(&validate_opts, caseload_after_discover, workspace_root)
                .map_err(|err| match err {
                    ValidateError::MissingDiscovery => CliError::usage(err.to_string()),
                    ValidateError::Discover(d) => map_discover_error(d),
                })
        })
    }
}

/// Resolves host config file, environment variables, and CLI overrides into Branch B [`ConfigureOptions`].
pub fn resolve_branch_b_options(
    provider: &ProviderConfigArgs,
    env: &BTreeMap<String, String>,
) -> Result<ConfigureOptions, CliError> {
    let cwd = env::current_dir().ok();
    let stored_config = match resolve_default_config_path(env) {
        Some(cfg_path) => load_host_config_file(&cfg_path, cwd.as_deref())
            .map_err(|err| CliError::usage(err.to_string()))?,
        None => None,
    };

    Ok(ConfigureOptions {
        screener_model: provider.screener_model.clone(),
        auditor_model: provider.auditor_model.clone(),
        reasoning_budget: provider.reasoning_budget,
        gemini_api_key: provider.gemini_api_key.clone(),
        typesafe_api_key: provider.typesafe_api_key.clone(),
        env: env.clone(),
        stored_config,
    })
}

/// Executes Branch B `configure` stage, fast-forwarding if `.config` is present and no overrides are specified.
pub fn run_branch_b_configure(
    options: &ConfigureOptions,
    incoming: Option<Caseload>,
    telemetry: &TelemetrySession<'_>,
) -> Result<Caseload, CliError> {
    if let Some(existing) = incoming.as_ref()
        && existing.config.is_some()
        && !options.has_explicit_overrides()
    {
        Ok(existing.clone())
    } else {
        telemetry.run_stage(PipelineStage::Configure, |_scope| {
            execute_configure(options, incoming).map_err(|err| CliError::usage(err.to_string()))
        })
    }
}

/// Extracts and validates the active Gemini API key from resolved [`ConfigureOptions`].
pub fn resolve_gemini_api_key(options: &ConfigureOptions) -> Result<String, CliError> {
    crate::configure::resolve_gemini_api_key(options)
        .map_err(|err| CliError::usage(err.to_string()))
}

/// Extracts and validates the active TypeSafe AI API key from resolved [`ConfigureOptions`].
pub(crate) fn resolve_typesafe_api_key(options: &ConfigureOptions) -> Result<String, CliError> {
    crate::configure::resolve_typesafe_api_key(options)
        .map_err(|err| CliError::usage(err.to_string()))
}

/// Builds a live [`MultiModelProvider`] resolving only the credentials required by `config`.
pub(crate) fn build_live_model_provider(
    options: &ConfigureOptions,
    config: &CaseloadConfig,
) -> Result<MultiModelProvider, CliError> {
    let needs_google = model_provider(&config.screener_model) == DEFAULT_PROVIDER
        || model_provider(&config.auditor_model) == DEFAULT_PROVIDER;
    let needs_typesafe = model_provider(&config.screener_model) == TYPESAFE_PROVIDER
        || model_provider(&config.auditor_model) == TYPESAFE_PROVIDER;

    let gemini_key = if needs_google {
        Some(resolve_gemini_api_key(options)?)
    } else {
        resolve_gemini_api_key(options).ok()
    };
    let typesafe_key = if needs_typesafe {
        Some(resolve_typesafe_api_key(options)?)
    } else {
        resolve_typesafe_api_key(options).ok()
    };

    Ok(MultiModelProvider::new(gemini_key, typesafe_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caseload::CaseloadConfig;
    use crate::telemetry::{RecordingTelemetrySink, SteppingTelemetryClock, TelemetryEvent};
    use std::io::Cursor;

    mod error_mapping {
        use super::*;

        #[test]
        fn test_map_discover_error_exit_codes() {
            let missing = DiscoverError::MissingInputSource;
            assert_eq!(map_discover_error(missing).exit_code(), 2);

            let glob_err = DiscoverError::InvalidGlob(
                crate::discover::GlobPattern::parse("[unclosed").unwrap_err(),
            );
            assert_eq!(map_discover_error(glob_err).exit_code(), 2);

            let io_err = DiscoverError::Io {
                path: "foo".to_string(),
                message: "not found".to_string(),
            };
            assert_eq!(map_discover_error(io_err).exit_code(), 1);
        }
    }

    mod fast_forward_telemetry_omission {
        use super::*;

        #[test]
        fn test_run_branch_a_intake_fast_forward_skips_stage_events() {
            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 5);
            let session = TelemetrySession::new(&sink, &clock);

            let existing = Caseload::new();
            let filing = FilingSourceArgs::default();
            let mut stdin = Cursor::new(b"");
            let result = run_branch_a_intake(&filing, Some(existing.clone()), &mut stdin, &session)
                .expect("fast-forward skip on intake");
            assert_eq!(result, existing);
            assert!(sink.events().is_empty());
        }

        #[test]
        fn test_run_branch_b_configure_fast_forward_vs_executed_events() {
            let sink = RecordingTelemetrySink::new();
            let clock = SteppingTelemetryClock::new(1_767_225_600_000, 5);
            let session = TelemetrySession::new(&sink, &clock);

            let existing = Caseload::new().with_config(CaseloadConfig {
                screener_model: "google:gemini-2.5-flash-lite".to_string(),
                auditor_model: "google:gemini-2.5-pro".to_string(),
                reasoning_budget: Some(1024),
            });
            let options = ConfigureOptions {
                screener_model: None,
                auditor_model: None,
                reasoning_budget: None,
                gemini_api_key: None,
                typesafe_api_key: None,
                env: BTreeMap::new(),
                stored_config: None,
            };
            let result = run_branch_b_configure(&options, Some(existing.clone()), &session)
                .expect("fast-forward skip on config");
            assert_eq!(result, existing);
            assert!(sink.events().is_empty());

            let executed_opts = ConfigureOptions {
                gemini_api_key: Some("test-key".to_string()),
                ..options
            };
            run_branch_b_configure(&executed_opts, None, &session)
                .expect("executed configure stage");
            assert_eq!(sink.events().len(), 2);
            assert!(matches!(
                sink.events()[0],
                TelemetryEvent::StageStart {
                    stage: PipelineStage::Configure,
                    ..
                }
            ));
            assert!(matches!(
                sink.events()[1],
                TelemetryEvent::StageFinish {
                    stage: PipelineStage::Configure,
                    usage: None,
                    ..
                }
            ));
        }
    }
}
