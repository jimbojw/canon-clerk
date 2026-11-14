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

//! Case-or-Controversy guard, stage-prefixed flag scoping, and fast-forward predicates for `canon-clerk audit`.

use super::AuditArgs;
use crate::audit::{
    DEFAULT_AUDIT_MAX_CONCURRENT_CALLS, DEFAULT_AUDIT_THRESHOLD, ENV_AUDIT_CONCURRENCY,
    ENV_AUDIT_THRESHOLD,
};
use crate::caseload::{Caseload, normalize_compliance_score};
use crate::cli::CliError;
use crate::cli::args::CanonSelectionArgs;
use crate::cli::closure::has_provider_overrides;
use crate::cli::docket::guards::has_concrete_cli_filing;
use crate::configure::StoredConfig;
use crate::docket::ENV_MAX_CONCURRENT_CALLS;
use crate::intake::IntakeOptions;
use std::collections::BTreeMap;

pub(super) fn ensure_stage_prefixed_threshold_flags(args: &AuditArgs) -> Result<(), CliError> {
    if args.rejected_threshold.is_some() {
        return Err(CliError::usage(
            "Ambiguous unqualified flag `--threshold` is disallowed.\n  \
             Hint: Pass stage-prefixed `--audit-threshold <FLOAT>`, `--admit-threshold <FLOAT>`, \
             or `--docket-threshold <FLOAT>` on `canon-clerk audit`.",
        ));
    }
    if args.rejected_apprise_threshold.is_some() {
        return Err(CliError::usage(
            "Out-of-closure flag `--apprise-threshold` is not valid for `canon-clerk audit` (`apprise` is pruned from the audit closure).\n  \
             Hint: Pass `--audit-threshold <FLOAT>`, `--admit-threshold <FLOAT>`, or `--docket-threshold <FLOAT>` on `canon-clerk audit`.",
        ));
    }
    if args.rejected_apprise_batch_size.is_some() {
        return Err(CliError::usage(
            "Out-of-closure flag `--apprise-batch-size` is not valid for `canon-clerk audit` (`apprise` is pruned from the audit closure).\n  \
             Hint: Pass `--docket-batch-size <COUNT>` for upstream macro triage batching.",
        ));
    }
    if args.rejected_apprise_concurrency.is_some() {
        return Err(CliError::usage(
            "Out-of-closure flag `--apprise-concurrency` is not valid for `canon-clerk audit` (`apprise` is pruned from the audit closure).\n  \
             Hint: Pass `--audit-concurrency <COUNT>`, `--admit-concurrency <COUNT>`, or `--docket-concurrency <COUNT>` on `canon-clerk audit`.",
        ));
    }
    Ok(())
}

pub(crate) fn resolve_audit_threshold(
    flag_threshold: Option<f64>,
    env: &BTreeMap<String, String>,
) -> Result<f64, CliError> {
    if let Some(val) = flag_threshold {
        return normalize_compliance_score(val).map_err(|detail| {
            CliError::usage(format!(
                "Invalid `--audit-threshold` value `{val}`: {detail}.\n  \
                 Hint: Pass a finite float in `[0.0, 1.0]` via `--audit-threshold <FLOAT>` (default: `{DEFAULT_AUDIT_THRESHOLD}`)."
            ))
        });
    }

    if let Some(raw_env) = env
        .get(ENV_AUDIT_THRESHOLD)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let parsed = raw_env.parse::<f64>().map_err(|_| {
            CliError::usage(format!(
                "Invalid threshold `{raw_env}` in `{ENV_AUDIT_THRESHOLD}`: expected a float in `[0.0, 1.0]`.\n  \
                 Hint: Pass `--audit-threshold <FLOAT>` or set `{ENV_AUDIT_THRESHOLD}=0.5`."
            ))
        })?;
        return normalize_compliance_score(parsed).map_err(|detail| {
            CliError::usage(format!(
                "Invalid threshold `{raw_env}` in `{ENV_AUDIT_THRESHOLD}`: {detail}.\n  \
                 Hint: Set `{ENV_AUDIT_THRESHOLD}` to a finite value in `[0.0, 1.0]` (default: `{DEFAULT_AUDIT_THRESHOLD}`)."
            ))
        });
    }

    Ok(DEFAULT_AUDIT_THRESHOLD)
}

pub(crate) fn resolve_audit_concurrency(
    flag_concurrency: Option<usize>,
    env: &BTreeMap<String, String>,
    stored: Option<&StoredConfig>,
) -> Result<Option<usize>, CliError> {
    if let Some(val) = flag_concurrency {
        if val == 0 {
            return Err(CliError::usage(format!(
                "Invalid `--audit-concurrency` value `0`: concurrency limit must be at least 1.\n  \
                 Hint: Pass a positive integer (`>= 1`) via `--audit-concurrency <COUNT>` or `{ENV_AUDIT_CONCURRENCY}` (default: `{DEFAULT_AUDIT_MAX_CONCURRENT_CALLS}`)."
            )));
        }
        return Ok(Some(val));
    }

    for env_key in [ENV_AUDIT_CONCURRENCY, ENV_MAX_CONCURRENT_CALLS] {
        if let Some(raw_env) = env.get(env_key).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let parsed = raw_env.parse::<usize>().ok().filter(|v| *v >= 1).ok_or_else(|| {
                CliError::usage(format!(
                    "Invalid concurrency limit `{raw_env}` in `{env_key}`: expected a positive integer (`>= 1`).\n  \
                     Hint: Pass `--audit-concurrency <COUNT>` or set `{ENV_AUDIT_CONCURRENCY}={DEFAULT_AUDIT_MAX_CONCURRENT_CALLS}`."
                ))
            })?;
            return Ok(Some(parsed));
        }
    }

    if let Some(stored_val) = stored.and_then(|s| s.audit_concurrency) {
        if stored_val == 0 {
            return Err(CliError::usage(format!(
                "Invalid `auditConcurrency` value `0` in user configuration: concurrency limit must be at least 1.\n  \
                 Hint: Configure a positive integer (`>= 1`) for `auditConcurrency` or override with `--audit-concurrency <COUNT>` / `{ENV_AUDIT_CONCURRENCY}`."
            )));
        }
        return Ok(Some(stored_val));
    }

    Ok(None)
}

pub(super) fn has_docket_cli_overrides(args: &AuditArgs) -> bool {
    args.docket_threshold.is_some()
        || args.docket_batch_size.is_some()
        || args.docket_concurrency.is_some()
}

pub(super) fn has_admit_cli_overrides(args: &AuditArgs) -> bool {
    args.admit_threshold.is_some() || args.admit_concurrency.is_some()
}

pub(super) fn can_fast_forward_completed_audit(
    args: &AuditArgs,
    intake_opts: &IntakeOptions,
    incoming: Option<&Caseload>,
) -> bool {
    incoming.is_some_and(|c| c.intake.is_some() && c.verdict.is_some())
        && !intake_opts.has_designated_source()
        && args.canons.is_empty()
        && args.audit_threshold.is_none()
        && args.audit_concurrency.is_none()
        && !has_admit_cli_overrides(args)
        && !has_docket_cli_overrides(args)
        && !has_provider_overrides(&args.provider)
}

pub(super) fn enforce_filing_and_case_guard(
    intake_opts: &IntakeOptions,
    canons: &CanonSelectionArgs,
    incoming: Option<&Caseload>,
) -> Result<(), CliError> {
    let cli_has_filing = has_concrete_cli_filing(intake_opts);
    let upstream_has_intake = incoming.is_some_and(|c| c.intake.is_some());

    if cli_has_filing || upstream_has_intake {
        return Ok(());
    }

    if !canons.is_empty() || incoming.is_some() {
        return Err(CliError::usage(
            "Case-or-Controversy violation: `canon-clerk audit` cannot adjudicate cases without an underlying filing in `.intake`.\n  \
             Hint: Provide target paths (`src/...`, `-`, `--all-targets`), pipe a diff (`--diff -`), \
             or pass `--caseload <PATH|->` produced from an upstream filing.",
        ));
    }

    Err(CliError::usage(
        "No filing source provided for `audit`.\n  \
         Hint: Pipe a diff via standard input (`--diff -`), specify target paths (`src/...`, `-`, `--all-targets`), \
         or pass an upstream caseload (`--caseload <PATH|->`) with `.intake` populated.",
    ))
}
