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

//! Stage identifiers, environment constants, and typed errors for the consistency benchmark harness.

use crate::admit::{AdmitError, DEFAULT_ADMIT_THRESHOLD};
use crate::apprise::{AppriseError, DEFAULT_APPRISE_THRESHOLD};
use crate::audit::{AuditError, DEFAULT_AUDIT_THRESHOLD};
use crate::docket::{DEFAULT_DOCKET_THRESHOLD, DocketError};
use crate::telemetry::PipelineStage;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Default number of repeated evaluation trials per benchmark configuration (`N = 5`).
pub const DEFAULT_BENCHMARK_TRIALS: usize = 5;

/// Environment variable name overriding `--stage` on `consistency_bench`.
pub const ENV_BENCH_STAGE: &str = "CANON_CLERK_BENCH_STAGE";
/// Environment variable name overriding `--trials` on `consistency_bench`.
pub const ENV_BENCH_TRIALS: &str = "CANON_CLERK_BENCH_TRIALS";
/// Environment variable name overriding `--threshold` on `consistency_bench`.
pub const ENV_BENCH_THRESHOLD: &str = "CANON_CLERK_BENCH_THRESHOLD";
/// Environment variable name overriding `--batch-size` on `consistency_bench`.
pub const ENV_BENCH_BATCH_SIZE: &str = "CANON_CLERK_BENCH_BATCH_SIZE";
/// Environment variable name overriding `--concurrency` on `consistency_bench`.
pub const ENV_BENCH_CONCURRENCY: &str = "CANON_CLERK_BENCH_CONCURRENCY";
/// Environment variable name overriding `--format` on `consistency_bench`.
pub const ENV_BENCH_FORMAT: &str = "CANON_CLERK_BENCH_FORMAT";
/// Environment variable name overriding `--include-stable` on `consistency_bench`.
pub const ENV_BENCH_INCLUDE_STABLE: &str = "CANON_CLERK_BENCH_INCLUDE_STABLE";
/// Environment variable name overriding `--output-json` on `consistency_bench`.
pub const ENV_BENCH_OUTPUT_JSON: &str = "CANON_CLERK_BENCH_OUTPUT_JSON";

/// Heuristic Caseload stage targeted by the consistency benchmark harness.
#[derive(
    ValueEnum, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkStage {
    /// Apprisal Track: Prospective statutory applicability screening (`apprise`).
    Apprise,
    /// Cascade Spine: Macro subject-matter jurisdiction screening (`docket`).
    Docket,
    /// Cascade Spine: Micro evidentiary admissibility screening (`admit`).
    Admit,
    /// Cascade Spine: Substantive per-case compliance adjudication (`audit`).
    Audit,
}

impl BenchmarkStage {
    /// Returns the canonical `snake_case` stage identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apprise => "apprise",
            Self::Docket => "docket",
            Self::Admit => "admit",
            Self::Audit => "audit",
        }
    }

    /// Maps the benchmark target to its [`PipelineStage`] telemetry variant.
    pub const fn pipeline_stage(self) -> PipelineStage {
        match self {
            Self::Apprise => PipelineStage::Apprise,
            Self::Docket => PipelineStage::Docket,
            Self::Admit => PipelineStage::Admit,
            Self::Audit => PipelineStage::Audit,
        }
    }

    /// Returns the stage's affirmative status wire string (`"applicable"`, `"docketed"`, `"admitted"`, `"pass"`).
    pub const fn positive_status_label(self) -> &'static str {
        match self {
            Self::Apprise => "applicable",
            Self::Docket => "docketed",
            Self::Admit => "admitted",
            Self::Audit => "pass",
        }
    }

    /// Returns the stage's negative status wire string (`"dismissed"`, `"excluded"`, `"fail"`).
    pub const fn negative_status_label(self) -> &'static str {
        match self {
            Self::Apprise | Self::Docket => "dismissed",
            Self::Admit => "excluded",
            Self::Audit => "fail",
        }
    }

    /// Returns the human-readable column heading for affirmative classifications.
    pub const fn positive_heading(self) -> &'static str {
        match self {
            Self::Apprise => "Applicable",
            Self::Docket => "Docketed",
            Self::Admit => "Admitted",
            Self::Audit => "Passed",
        }
    }

    /// Returns the human-readable column heading for negative classifications.
    pub const fn negative_heading(self) -> &'static str {
        match self {
            Self::Apprise | Self::Docket => "Dismissed",
            Self::Admit => "Excluded",
            Self::Audit => "Failed",
        }
    }

    /// Returns the default gate score threshold (`0.5`) for the stage.
    pub const fn default_threshold(self) -> f64 {
        match self {
            Self::Apprise => DEFAULT_APPRISE_THRESHOLD,
            Self::Docket => DEFAULT_DOCKET_THRESHOLD,
            Self::Admit => DEFAULT_ADMIT_THRESHOLD,
            Self::Audit => DEFAULT_AUDIT_THRESHOLD,
        }
    }
}

/// Typed errors emitted by the consistency benchmark harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsistencyBenchError {
    /// `trials` was set to `0`.
    InvalidTrials,
    /// `configs` list was empty.
    EmptyConfigs,
    /// Invalid score threshold outside `[0.0, 1.0]`.
    InvalidThreshold {
        /// Validation detail.
        detail: String,
    },
    /// Invalid `--batch-size` specification.
    InvalidBatchSpec {
        /// Validation detail.
        detail: String,
    },
    /// Invalid `--concurrency` value (`0` or non-integer).
    InvalidConcurrency {
        /// Validation detail.
        detail: String,
    },
    /// Candidate canons failed static `validate` linting prior to benchmarking.
    ValidationFailed,
    /// Upstream `Caseload` setup or prerequisite error.
    Setup {
        /// Underlying setup error message.
        detail: String,
    },
    /// Expected stage namespace was missing on a completed trial `Caseload`.
    MissingStagePayload {
        /// Stage whose payload was absent.
        stage: BenchmarkStage,
        /// Expected JSON field path (e.g. `".apprisal"`).
        namespace: &'static str,
    },
    /// Stage payload failed schema normalization.
    MalformedStagePayload {
        /// Stage whose payload failed decoding.
        stage: BenchmarkStage,
        /// Decoding error detail.
        detail: String,
    },
    /// Underlying `apprise` domain execution error.
    Apprise(AppriseError),
    /// Underlying `docket` domain execution error.
    Docket(DocketError),
    /// Underlying `admit` domain execution error.
    Admit(AdmitError),
    /// Underlying `audit` domain execution error.
    Audit(AuditError),
}

impl ConsistencyBenchError {
    /// Maps the benchmark error to standard CLI exit codes (`1` validation failure, `2` usage/provider).
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::ValidationFailed => 1,
            Self::InvalidTrials
            | Self::EmptyConfigs
            | Self::InvalidThreshold { .. }
            | Self::InvalidBatchSpec { .. }
            | Self::InvalidConcurrency { .. }
            | Self::Setup { .. }
            | Self::MissingStagePayload { .. }
            | Self::MalformedStagePayload { .. }
            | Self::Apprise(_)
            | Self::Docket(_)
            | Self::Admit(_)
            | Self::Audit(_) => 2,
        }
    }
}

impl fmt::Display for ConsistencyBenchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTrials => write!(
                f,
                "Invalid trial count `0`: benchmark requires at least 1 trial.\n  \
                 Hint: Pass `--trials <COUNT>` with an integer `>= 1` (default: `{DEFAULT_BENCHMARK_TRIALS}`)."
            ),
            Self::EmptyConfigs => write!(
                f,
                "Benchmark configuration list cannot be empty.\n  \
                 Hint: Provide at least one `ConsistencyConfigSpec` (e.g. `--batch-size unbounded`)."
            ),
            Self::InvalidThreshold { detail } => write!(
                f,
                "Invalid benchmark threshold: {detail}.\n  \
                 Hint: Pass a finite float in `[0.0, 1.0]` via `--threshold <FLOAT>`."
            ),
            Self::InvalidBatchSpec { detail } => write!(
                f,
                "Invalid benchmark batch size specification: {detail}.\n  \
                 Hint: Pass `--batch-size 15`, `--batch-size unbounded`, or a comma-separated sweep like `--batch-size unbounded,15,25`."
            ),
            Self::InvalidConcurrency { detail } => write!(
                f,
                "Invalid benchmark concurrency limit: {detail}.\n  \
                 Hint: Pass a positive integer (`>= 1`) via `--concurrency <COUNT>`."
            ),
            Self::ValidationFailed => write!(
                f,
                "Cannot execute consistency benchmark because one or more candidate canons failed static validation.\n  \
                 Hint: Run `canon-clerk validate` to inspect and repair malformed candidate canons first."
            ),
            Self::Setup { detail } => write!(f, "{detail}"),
            Self::MissingStagePayload { stage, namespace } => write!(
                f,
                "Completed `{}` trial Caseload is missing expected `{namespace}` payload.\n  \
                 Hint: Verify that the stage executed rather than skipping upstream.",
                stage.as_str()
            ),
            Self::MalformedStagePayload { stage, detail } => write!(
                f,
                "Malformed `{}` payload on trial Caseload: {detail}.\n  \
                 Hint: Ensure stage assessments conform to the documented Caseload JSON contract.",
                stage.as_str()
            ),
            Self::Apprise(err) => write!(f, "{err}"),
            Self::Docket(err) => write!(f, "{err}"),
            Self::Admit(err) => write!(f, "{err}"),
            Self::Audit(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ConsistencyBenchError {}
