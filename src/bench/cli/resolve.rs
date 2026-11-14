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

//! Flag and environment fallback resolution for [`super::ConsistencyBenchCli`].

use super::{ConsistencyBenchCli, ConsistencyBenchFormat};
use crate::admit::ENV_ADMIT_THRESHOLD;
use crate::apprise::ENV_APPRISE_THRESHOLD;
use crate::audit::ENV_AUDIT_THRESHOLD;
use crate::bench::{
    BenchmarkStage, ConsistencyBenchError, DEFAULT_BENCHMARK_TRIALS, ENV_BENCH_FORMAT,
    ENV_BENCH_INCLUDE_STABLE, ENV_BENCH_OUTPUT_JSON, ENV_BENCH_STAGE, ENV_BENCH_THRESHOLD,
    ENV_BENCH_TRIALS,
};
use crate::caseload::normalize_unit_score;
use crate::cli::CliError;
use crate::docket::ENV_DOCKET_THRESHOLD;
use clap::ValueEnum;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) struct ResolvedCliParams {
    pub(super) stage: BenchmarkStage,
    pub(super) trials: usize,
    pub(super) threshold: f64,
    pub(super) format: ConsistencyBenchFormat,
    pub(super) include_stable: bool,
    pub(super) output_json: Option<PathBuf>,
}

pub(super) fn resolve_cli_parameters(
    cli: &ConsistencyBenchCli,
    env: &BTreeMap<String, String>,
) -> Result<ResolvedCliParams, CliError> {
    let stage = resolve_stage(cli.stage, env)?;
    let trials = resolve_trials(cli.trials, env)?;
    let threshold = resolve_threshold(stage, cli.threshold, env)?;
    let format = resolve_format(cli.format, cli.json, env)?;
    let include_stable = resolve_include_stable(cli.include_stable, env)?;
    let output_json = cli.output_json.clone().or_else(|| {
        env.get(ENV_BENCH_OUTPUT_JSON)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    });

    Ok(ResolvedCliParams {
        stage,
        trials,
        threshold,
        format,
        include_stable,
        output_json,
    })
}

pub(super) fn map_bench_error(err: ConsistencyBenchError) -> CliError {
    if err.exit_code() == 1 {
        CliError::runtime(err.to_string())
    } else {
        CliError::usage(err.to_string())
    }
}

fn resolve_stage(
    flag_stage: Option<BenchmarkStage>,
    env: &BTreeMap<String, String>,
) -> Result<BenchmarkStage, CliError> {
    if let Some(s) = flag_stage {
        return Ok(s);
    }
    if let Some(raw) = env
        .get(ENV_BENCH_STAGE)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return BenchmarkStage::from_str(raw, true).map_err(|_| {
            CliError::usage(format!(
                "Invalid stage `{raw}` in `{ENV_BENCH_STAGE}`.\n  \
                 Hint: Pass `--stage <apprise|docket|admit|audit>` or set `{ENV_BENCH_STAGE}=apprise`."
            ))
        });
    }
    Err(CliError::usage(format!(
        "Missing required benchmark `--stage <apprise|docket|admit|audit>`.\n  \
         Hint: Pass `--stage apprise`, `--stage docket`, `--stage admit`, or `--stage audit` (or set `{ENV_BENCH_STAGE}`)."
    )))
}

fn resolve_trials(
    flag_trials: Option<usize>,
    env: &BTreeMap<String, String>,
) -> Result<usize, CliError> {
    if let Some(n) = flag_trials {
        if n == 0 {
            return Err(CliError::usage(format!(
                "Invalid `--trials` value `0`: trial count must be at least 1.\n  \
                 Hint: Pass a positive integer (`>= 1`) via `--trials <COUNT>` (default: `{DEFAULT_BENCHMARK_TRIALS}`)."
            )));
        }
        return Ok(n);
    }
    if let Some(raw) = env
        .get(ENV_BENCH_TRIALS)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return raw.parse::<usize>().ok().filter(|v| *v >= 1).ok_or_else(|| {
            CliError::usage(format!(
                "Invalid trial count `{raw}` in `{ENV_BENCH_TRIALS}`: expected a positive integer (`>= 1`).\n  \
                 Hint: Pass `--trials <COUNT>` or set `{ENV_BENCH_TRIALS}={DEFAULT_BENCHMARK_TRIALS}`."
            ))
        });
    }
    Ok(DEFAULT_BENCHMARK_TRIALS)
}

fn resolve_threshold(
    stage: BenchmarkStage,
    flag_threshold: Option<f64>,
    env: &BTreeMap<String, String>,
) -> Result<f64, CliError> {
    let default_val = stage.default_threshold();
    if let Some(val) = flag_threshold {
        return normalize_unit_score(val).map_err(|detail| {
            CliError::usage(format!(
                "Invalid `--threshold` value `{val}`: {detail}.\n  \
                 Hint: Pass a finite float in `[0.0, 1.0]` via `--threshold <FLOAT>` (default: `{default_val}`)."
            ))
        });
    }

    let stage_env = match stage {
        BenchmarkStage::Apprise => Some(ENV_APPRISE_THRESHOLD),
        BenchmarkStage::Docket => Some(ENV_DOCKET_THRESHOLD),
        BenchmarkStage::Admit => Some(ENV_ADMIT_THRESHOLD),
        BenchmarkStage::Audit => Some(ENV_AUDIT_THRESHOLD),
    };
    for key in [Some(ENV_BENCH_THRESHOLD), stage_env].into_iter().flatten() {
        if let Some(raw) = env.get(key).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let parsed = raw.parse::<f64>().map_err(|_| {
                CliError::usage(format!(
                    "Invalid threshold `{raw}` in `{key}`: expected a float in `[0.0, 1.0]`.\n  \
                     Hint: Pass `--threshold <FLOAT>` or set `{key}={default_val}`."
                ))
            })?;
            return normalize_unit_score(parsed).map_err(|detail| {
                CliError::usage(format!(
                    "Invalid threshold `{raw}` in `{key}`: {detail}.\n  \
                     Hint: Set `{key}` to a finite value in `[0.0, 1.0]` (default: `{default_val}`)."
                ))
            });
        }
    }
    Ok(default_val)
}

fn resolve_format(
    flag_format: Option<ConsistencyBenchFormat>,
    json_flag: bool,
    env: &BTreeMap<String, String>,
) -> Result<ConsistencyBenchFormat, CliError> {
    if json_flag {
        return Ok(ConsistencyBenchFormat::Json);
    }
    if let Some(fmt) = flag_format {
        return Ok(fmt);
    }
    if let Some(raw) = env
        .get(ENV_BENCH_FORMAT)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return ConsistencyBenchFormat::from_str(raw, true).map_err(|_| {
            CliError::usage(format!(
                "Invalid format `{raw}` in `{ENV_BENCH_FORMAT}`.\n  \
                 Hint: Pass `--format <markdown|json>` or `--json`."
            ))
        });
    }
    Ok(ConsistencyBenchFormat::Markdown)
}

fn resolve_include_stable(
    flag_include_stable: bool,
    env: &BTreeMap<String, String>,
) -> Result<bool, CliError> {
    if flag_include_stable {
        return Ok(true);
    }
    if let Some(raw) = env
        .get(ENV_BENCH_INCLUDE_STABLE)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return match raw.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(true),
            "0" | "false" | "no" => Ok(false),
            _ => Err(CliError::usage(format!(
                "Invalid boolean `{raw}` in `{ENV_BENCH_INCLUDE_STABLE}`.\n  \
                 Hint: Pass `--include-stable` or set `{ENV_BENCH_INCLUDE_STABLE}=true`."
            ))),
        };
    }
    Ok(false)
}
