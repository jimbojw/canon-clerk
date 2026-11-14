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

//! Batch-sweep configuration resolution and upstream Branch A / Branch B `Caseload` preparation.

use super::{
    BenchmarkStage, ConsistencyBenchError, ConsistencyConfigSpec, ENV_BENCH_BATCH_SIZE,
    ENV_BENCH_CONCURRENCY,
};
use crate::admit::{DEFAULT_ADMIT_MAX_CONCURRENT_CALLS, ENV_ADMIT_CONCURRENCY};
use crate::apprise::{
    DEFAULT_APPRISE_MAX_CONCURRENT_CALLS, ENV_APPRISE_BATCH_SIZE, ENV_APPRISE_CONCURRENCY,
};
use crate::audit::{DEFAULT_AUDIT_MAX_CONCURRENT_CALLS, ENV_AUDIT_CONCURRENCY};
use crate::caseload::{Caseload, CaseloadIntake, IntakeScope, normalize_unit_score};
use crate::cli::CanonSelectionArgs;
use crate::configure::{ConfigureOptions, StoredConfig, execute_configure};
use crate::discover::{DiscoverOptions, WorkspaceSnapshot, execute_discover};
use crate::docket::{
    DEFAULT_DOCKET_MAX_CONCURRENT_CALLS, ENV_DOCKET_BATCH_SIZE, ENV_DOCKET_CONCURRENCY,
    ENV_MAX_CANONS_PER_CALL, ENV_MAX_CONCURRENT_CALLS,
};
use crate::intake::{IntakeOptions, execute_intake};
use crate::validate::{ValidateOptions, execute_validate};
use std::collections::BTreeMap;

/// Input options for resolving one or more [`ConsistencyConfigSpec`] entries in a benchmark run.
#[derive(Debug, Clone)]
pub struct ConfigSweepInputs<'a> {
    /// Target Caseload pipeline stage.
    pub stage: BenchmarkStage,
    /// Validated score threshold in `[0.0, 1.0]`.
    pub threshold: f64,
    /// Explicit `--batch-size` tokens (each token may be comma-separated, e.g. `"unbounded,15,25"`).
    pub raw_batch_specs: &'a [String],
    /// Optional explicit `--concurrency` override (`>= 1`).
    pub flag_concurrency: Option<usize>,
    /// Ambient environment map for fallback resolution.
    pub env: &'a BTreeMap<String, String>,
    /// Optional user-level stored configuration (`config.json`).
    pub stored_config: Option<&'a StoredConfig>,
    /// Per-request generation timeout in milliseconds.
    pub timeout_ms: u64,
}

/// Resolves CLI, environment, and stored configuration inputs into a non-empty list of [`ConsistencyConfigSpec`]s.
pub fn resolve_config_specs(
    inputs: &ConfigSweepInputs<'_>,
) -> Result<Vec<ConsistencyConfigSpec>, ConsistencyBenchError> {
    let threshold = normalize_unit_score(inputs.threshold)
        .map_err(|detail| ConsistencyBenchError::InvalidThreshold { detail })?;
    let concurrency = resolve_effective_concurrency(
        inputs.stage,
        inputs.flag_concurrency,
        inputs.env,
        inputs.stored_config,
    )?;

    let batch_values = resolve_batch_sweep_values(
        inputs.stage,
        inputs.raw_batch_specs,
        inputs.env,
        inputs.stored_config,
    )?;

    let default_conc = default_concurrency_for_stage(inputs.stage);
    let mut specs = Vec::with_capacity(batch_values.len());
    for batch_opt in batch_values {
        let effective_conc = match batch_opt {
            Some(_) => Some(concurrency.unwrap_or(default_conc)),
            None => concurrency,
        };
        let label = match batch_opt {
            Some(b) => format!("batch={b},conc={}", effective_conc.unwrap_or(default_conc)),
            None => "unbounded".to_string(),
        };
        specs.push(ConsistencyConfigSpec {
            label,
            threshold,
            max_canons_per_call: batch_opt,
            max_concurrent_calls: effective_conc,
            timeout_ms: inputs.timeout_ms,
        });
    }
    Ok(specs)
}

const fn default_concurrency_for_stage(stage: BenchmarkStage) -> usize {
    match stage {
        BenchmarkStage::Apprise => DEFAULT_APPRISE_MAX_CONCURRENT_CALLS,
        BenchmarkStage::Admit => DEFAULT_ADMIT_MAX_CONCURRENT_CALLS,
        BenchmarkStage::Docket => DEFAULT_DOCKET_MAX_CONCURRENT_CALLS,
        BenchmarkStage::Audit => DEFAULT_AUDIT_MAX_CONCURRENT_CALLS,
    }
}

fn resolve_effective_concurrency(
    stage: BenchmarkStage,
    flag_concurrency: Option<usize>,
    env: &BTreeMap<String, String>,
    stored: Option<&StoredConfig>,
) -> Result<Option<usize>, ConsistencyBenchError> {
    if let Some(val) = flag_concurrency {
        if val == 0 {
            return Err(ConsistencyBenchError::InvalidConcurrency {
                detail: "concurrency must be at least 1, got 0".to_string(),
            });
        }
        return Ok(Some(val));
    }

    let stage_key = match stage {
        BenchmarkStage::Apprise => ENV_APPRISE_CONCURRENCY,
        BenchmarkStage::Admit => ENV_ADMIT_CONCURRENCY,
        BenchmarkStage::Docket => ENV_DOCKET_CONCURRENCY,
        BenchmarkStage::Audit => ENV_AUDIT_CONCURRENCY,
    };
    for key in [ENV_BENCH_CONCURRENCY, stage_key, ENV_MAX_CONCURRENT_CALLS] {
        if let Some(raw) = non_empty_env(env, key) {
            let parsed = raw
                .parse::<usize>()
                .ok()
                .filter(|v| *v >= 1)
                .ok_or_else(|| ConsistencyBenchError::InvalidConcurrency {
                    detail: format!("`{key}` must be a positive integer (`>= 1`), got `{raw}`"),
                })?;
            return Ok(Some(parsed));
        }
    }

    if let Some(cfg) = stored {
        let stored_val = match stage {
            BenchmarkStage::Apprise => cfg.apprise_concurrency,
            BenchmarkStage::Admit => cfg.admit_concurrency,
            BenchmarkStage::Docket => cfg.docket_concurrency,
            BenchmarkStage::Audit => cfg.audit_concurrency,
        };
        if let Some(val) = stored_val {
            if val == 0 {
                return Err(ConsistencyBenchError::InvalidConcurrency {
                    detail: "stored concurrency in `config.json` must be at least 1, got 0"
                        .to_string(),
                });
            }
            return Ok(Some(val));
        }
    }

    Ok(None)
}

fn resolve_batch_sweep_values(
    stage: BenchmarkStage,
    raw_specs: &[String],
    env: &BTreeMap<String, String>,
    stored: Option<&StoredConfig>,
) -> Result<Vec<Option<usize>>, ConsistencyBenchError> {
    if !raw_specs.is_empty() {
        return parse_batch_tokens(raw_specs);
    }

    if let Some(raw_env) = non_empty_env(env, ENV_BENCH_BATCH_SIZE) {
        return parse_batch_tokens(std::slice::from_ref(&raw_env));
    }

    let stage_key = match stage {
        BenchmarkStage::Apprise => ENV_APPRISE_BATCH_SIZE,
        BenchmarkStage::Docket | BenchmarkStage::Admit | BenchmarkStage::Audit => {
            ENV_DOCKET_BATCH_SIZE
        }
    };
    for key in [stage_key, ENV_MAX_CANONS_PER_CALL] {
        if let Some(raw) = non_empty_env(env, key) {
            let parsed = raw
                .parse::<usize>()
                .ok()
                .filter(|v| *v >= 1)
                .ok_or_else(|| ConsistencyBenchError::InvalidBatchSpec {
                    detail: format!("`{key}` must be a positive integer (`>= 1`), got `{raw}`"),
                })?;
            return Ok(vec![Some(parsed)]);
        }
    }

    if let Some(cfg) = stored {
        let stored_val = match stage {
            BenchmarkStage::Apprise => cfg.apprise_batch_size,
            BenchmarkStage::Docket | BenchmarkStage::Admit | BenchmarkStage::Audit => {
                cfg.docket_batch_size
            }
        };
        if let Some(val) = stored_val {
            if val == 0 {
                return Err(ConsistencyBenchError::InvalidBatchSpec {
                    detail: "stored batch size in `config.json` must be at least 1, got 0"
                        .to_string(),
                });
            }
            return Ok(vec![Some(val)]);
        }
    }

    Ok(vec![None])
}

/// Parses one or more comma-delimited batch-size specifiers (`"unbounded"`, `"0"`, or positive integers).
pub fn parse_batch_tokens(
    raw_specs: &[String],
) -> Result<Vec<Option<usize>>, ConsistencyBenchError> {
    let mut result = Vec::new();
    for raw in raw_specs {
        for part in raw.split(',') {
            let token = part.trim();
            if token.is_empty() {
                continue;
            }
            let lower = token.to_ascii_lowercase();
            if matches!(lower.as_str(), "0" | "none" | "unbounded" | "single") {
                result.push(None);
            } else {
                let count = token.parse::<usize>().ok().filter(|v| *v >= 1).ok_or_else(|| {
                    ConsistencyBenchError::InvalidBatchSpec {
                        detail: format!(
                            "expected `unbounded` (or `0`) or a positive integer (`>= 1`), got `{token}`"
                        ),
                    }
                })?;
                result.push(Some(count));
            }
        }
    }

    if result.is_empty() {
        return Err(ConsistencyBenchError::InvalidBatchSpec {
            detail: "batch size specification cannot be empty".to_string(),
        });
    }
    Ok(result)
}

fn non_empty_env(env: &BTreeMap<String, String>, key: &str) -> Option<String> {
    env.get(key)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

/// Upstream Branch A + Branch B inputs used to materialize a benchmark-ready [`Caseload`].
pub struct UpstreamCaseloadInputs<'a> {
    /// Target Caseload pipeline stage.
    pub stage: BenchmarkStage,
    /// Filing options resolved from CLI flags / stdin.
    pub intake_opts: &'a IntakeOptions,
    /// Canon scope selection (`--all-canons`, `--canon`).
    pub canons: &'a CanonSelectionArgs,
    /// Provider and model configuration options.
    pub configure_opts: &'a ConfigureOptions,
    /// Optional pre-populated upstream `Caseload`.
    pub incoming: Option<Caseload>,
    /// Materialized workspace file tree and canon corpus snapshot.
    pub workspace: &'a WorkspaceSnapshot,
}

/// Executes guarded `intake` → `discover` → `validate` + `configure` once to prepare a clean
/// upstream [`Caseload`] ready for `N` repeated benchmark trials.
pub fn prepare_upstream_caseload(
    inputs: &UpstreamCaseloadInputs<'_>,
) -> Result<Caseload, ConsistencyBenchError> {
    let stripped = inputs
        .incoming
        .clone()
        .map(|c| clear_stage_and_downstream(inputs.stage, c));

    let (after_intake, targets_changed) =
        resolve_upstream_intake(inputs.stage, inputs.intake_opts, inputs.canons, stripped)?;
    let after_discover = resolve_upstream_discover(
        inputs.stage,
        inputs.canons,
        after_intake,
        inputs.workspace,
        targets_changed,
    )?;
    let after_validate = resolve_upstream_validate(after_discover, inputs.workspace)?;
    resolve_upstream_configure(after_validate, inputs.configure_opts)
}

pub(crate) fn clear_stage_and_downstream(
    stage: BenchmarkStage,
    mut caseload: Caseload,
) -> Caseload {
    match stage {
        BenchmarkStage::Apprise => {
            caseload.apprisal = None;
        }
        BenchmarkStage::Docket => {
            caseload.docket = None;
            caseload.evidence = None;
            caseload.verdict = None;
        }
        BenchmarkStage::Admit => {
            caseload.evidence = None;
            caseload.verdict = None;
        }
        BenchmarkStage::Audit => {
            caseload.verdict = None;
        }
    }
    caseload
}

fn resolve_upstream_intake(
    stage: BenchmarkStage,
    intake_opts: &IntakeOptions,
    canons: &CanonSelectionArgs,
    mut incoming: Option<Caseload>,
) -> Result<(Caseload, bool), ConsistencyBenchError> {
    if !intake_opts.has_designated_source() {
        if let Some(existing) = incoming {
            return Ok((existing, false));
        }
        return Err(ConsistencyBenchError::Setup {
            detail: format!(
                "Missing filing inputs for `{}` benchmark.\n  \
                 Hint: Provide target paths (`src/...`), `--diff <PATH|->`, `--intent <TEXT>` (for `apprise`), or `--caseload <PATH|->`.",
                stage.as_str()
            ),
        });
    }

    let cli_has_targets = intake_opts.all_targets
        || intake_opts.patch_content.is_some()
        || intake_opts.target_paths.is_some();

    if !cli_has_targets
        && let Some(existing) = incoming.as_mut()
        && let Some(prev_intake) = existing.intake.take()
    {
        let meta_changed =
            has_non_empty(&intake_opts.pr_title) || has_non_empty(&intake_opts.pr_body);
        existing.intake = Some(merge_intake_metadata(prev_intake, intake_opts));
        return Ok((
            existing.clone(),
            meta_changed && stage != BenchmarkStage::Apprise,
        ));
    }

    let mut effective_opts = intake_opts.clone();
    if effective_opts.intent.is_none() {
        effective_opts.intent = incoming
            .as_ref()
            .and_then(|c| c.intake.as_ref())
            .and_then(|i| i.intent.clone());
    }

    if stage == BenchmarkStage::Apprise
        && !cli_has_targets
        && canons.is_empty()
        && incoming.is_none()
    {
        return Err(ConsistencyBenchError::Setup {
            detail: "`apprise` benchmark requires both `--intent <TEXT>` and a target or canon scope.\n  \
                     Hint: Pass target paths (e.g. `src/cli`), `--all-targets`, `--all-canons`, or `--canon <PATH|GLOB>`."
                .to_string(),
        });
    }

    let updated =
        execute_intake(&effective_opts, incoming).map_err(|err| ConsistencyBenchError::Setup {
            detail: err.to_string(),
        })?;
    Ok((updated, true))
}

fn merge_intake_metadata(mut base: CaseloadIntake, opts: &IntakeOptions) -> CaseloadIntake {
    if let Some(intent) = opts
        .intent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        base.intent = Some(intent.to_string());
    }
    if let Some(title) = opts
        .pr_title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        base.pr_title = Some(title.to_string());
    }
    if let Some(body) = opts
        .pr_body
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        base.pr_body = Some(body.to_string());
    }
    base
}

fn has_non_empty(opt: &Option<String>) -> bool {
    opt.as_deref().is_some_and(|s| !s.trim().is_empty())
}

fn resolve_upstream_discover(
    stage: BenchmarkStage,
    canons: &CanonSelectionArgs,
    mut caseload: Caseload,
    workspace: &WorkspaceSnapshot,
    targets_changed: bool,
) -> Result<Caseload, ConsistencyBenchError> {
    if caseload.discovery.is_some() && !targets_changed && canons.is_empty() {
        return Ok(caseload);
    }

    caseload.discovery = None;
    caseload.validation = None;

    let intake_has_targets = caseload.intake.as_ref().is_some_and(|i| {
        i.scope == IntakeScope::AllTargets || !i.target_paths.is_empty() || !i.diffs.is_empty()
    });
    let all_canons = match stage {
        BenchmarkStage::Apprise => {
            canons.all_canons || (!intake_has_targets && !canons.canon.is_empty())
        }
        BenchmarkStage::Docket | BenchmarkStage::Admit | BenchmarkStage::Audit => canons.all_canons,
    };

    let discover_opts = DiscoverOptions {
        all_canons,
        canon_patterns: canons.canon.clone(),
    };
    execute_discover(&discover_opts, caseload, workspace).map_err(|err| {
        ConsistencyBenchError::Setup {
            detail: err.to_string(),
        }
    })
}

fn resolve_upstream_validate(
    caseload: Caseload,
    workspace: &WorkspaceSnapshot,
) -> Result<Caseload, ConsistencyBenchError> {
    let validated = if caseload.validation.is_some() {
        caseload
    } else {
        execute_validate(&ValidateOptions::default(), caseload, workspace).map_err(|err| {
            ConsistencyBenchError::Setup {
                detail: err.to_string(),
            }
        })?
    };

    if validated.validation.as_ref().is_some_and(|v| v.has_errors) {
        return Err(ConsistencyBenchError::ValidationFailed);
    }
    Ok(validated)
}

fn resolve_upstream_configure(
    caseload: Caseload,
    configure_opts: &ConfigureOptions,
) -> Result<Caseload, ConsistencyBenchError> {
    let has_candidates = caseload
        .discovery
        .as_ref()
        .is_some_and(|d| !d.candidate_canons.is_empty());

    if !has_candidates && caseload.config.is_none() && !configure_opts.has_explicit_overrides() {
        return Ok(caseload);
    }
    if caseload.config.is_some() && !configure_opts.has_explicit_overrides() {
        return Ok(caseload);
    }

    execute_configure(configure_opts, Some(caseload)).map_err(|err| ConsistencyBenchError::Setup {
        detail: err.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprise::DEFAULT_APPRISE_THRESHOLD;
    use crate::canon::ParseCanonOptions;
    use crate::provider::DEFAULT_GENERATION_TIMEOUT_MS;

    mod batch_sweep_parsing {
        use super::*;

        #[test]
        fn test_parses_comma_separated_batch_sweep_and_unbounded_aliases() {
            let env = BTreeMap::new();
            let raw = vec!["unbounded,15".to_string(), "25".to_string()];
            let specs = resolve_config_specs(&ConfigSweepInputs {
                stage: BenchmarkStage::Apprise,
                threshold: DEFAULT_APPRISE_THRESHOLD,
                raw_batch_specs: &raw,
                flag_concurrency: Some(3),
                env: &env,
                stored_config: None,
                timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
            })
            .expect("resolve sweep specs");

            assert_eq!(specs.len(), 3);
            assert_eq!(specs[0].label, "unbounded");
            assert_eq!(specs[0].max_canons_per_call, None);
            assert_eq!(specs[1].label, "batch=15,conc=3");
            assert_eq!(specs[1].max_canons_per_call, Some(15));
            assert_eq!(specs[1].max_concurrent_calls, Some(3));
            assert_eq!(specs[2].label, "batch=25,conc=3");
            assert_eq!(specs[2].max_canons_per_call, Some(25));
        }

        #[test]
        fn test_rejects_invalid_batch_spec_and_zero_concurrency() {
            let bad = parse_batch_tokens(&["abc".to_string()]).expect_err("invalid batch token");
            assert_eq!(bad.exit_code(), 2);
            assert!(bad.to_string().contains("unbounded"));

            let env = BTreeMap::new();
            let zero_conc = resolve_config_specs(&ConfigSweepInputs {
                stage: BenchmarkStage::Docket,
                threshold: DEFAULT_APPRISE_THRESHOLD,
                raw_batch_specs: &[],
                flag_concurrency: Some(0),
                env: &env,
                stored_config: None,
                timeout_ms: DEFAULT_GENERATION_TIMEOUT_MS,
            })
            .expect_err("zero concurrency must fail");
            assert_eq!(zero_conc.exit_code(), 2);
        }
    }

    mod upstream_caseload_preparation {
        use super::*;

        #[test]
        fn test_prepares_apprise_caseload_and_strips_existing_stage_payload() {
            let snapshot = WorkspaceSnapshot::new()
                .with_file("src/cli/mod.rs")
                .with_canon_markdown(
                    "---\ntriggers:\n  - \"src/cli/**\"\ntags:\n  - cli\n---\nCLI flags MUST have long names.\n\nRationale: Clarity across scripts.\n",
                    &ParseCanonOptions::with_source_ref(".canons/cli/long-flags.md"),
                );

            let intake_opts = IntakeOptions {
                intent: Some("Add a new CLI flag".to_string()),
                target_paths: Some(vec!["src/cli/mod.rs".to_string()]),
                ..IntakeOptions::default()
            };
            let configure_opts = ConfigureOptions {
                gemini_api_key: Some("test-api-key".to_string()),
                ..ConfigureOptions::default()
            };

            let prepared = prepare_upstream_caseload(&UpstreamCaseloadInputs {
                stage: BenchmarkStage::Apprise,
                intake_opts: &intake_opts,
                canons: &CanonSelectionArgs::default(),
                configure_opts: &configure_opts,
                incoming: None,
                workspace: &snapshot,
            })
            .expect("prepare upstream caseload");

            assert!(prepared.intake.is_some());
            assert_eq!(
                prepared
                    .discovery
                    .as_ref()
                    .expect("discovery")
                    .candidate_canons,
                vec![".canons/cli/long-flags.md".to_string()]
            );
            assert!(prepared.validation.is_some());
            assert!(prepared.config.is_some());
            assert!(prepared.apprisal.is_none());
        }
    }
}
