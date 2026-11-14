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

//! Shared Pipeline Funnel Provenance watermarks and receipt line writers (`docs/architecture/scheduling.md` §6).

use crate::caseload::{
    Caseload, CaseloadDiscovery, CaseloadDocket, CaseloadEvidence, CaseloadIntake,
    CaseloadValidation, CaseloadVerdict, IntakeScope,
};
use std::io::Write;

/// Per-stage execution watermark recorded for the Pipeline Funnel Provenance receipt (`docs/architecture/scheduling.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageDisposition {
    /// Stage executed during the current CLI invocation.
    Executed,
    /// Stage was already populated on `--caseload` and fast-forwarded without re-execution.
    PrePopulated,
    /// `discover` executed and matched zero candidate canons, triggering an early clean pass.
    ZeroCandidateShortCircuit,
    /// Downstream stage skipped because upstream `discover` matched zero candidate canons.
    SkippedZeroCandidates,
    /// `validate` executed and found one or more candidate canon lint errors (`exit 1`).
    ValidationFailed,
    /// Downstream stage skipped because upstream `validate` failed (`exit 1`).
    SkippedValidationFailed,
    /// Downstream stage skipped because upstream `docket` opened zero active cases (`active_docket` empty).
    SkippedEmptyDocket,
    /// Downstream `audit` skipped because upstream `admit` admitted zero exhibits (`active_cases` empty).
    SkippedZeroEvidence,
}

pub(crate) fn glyph_for(disp: StageDisposition) -> &'static str {
    match disp {
        StageDisposition::Executed => "✓",
        StageDisposition::PrePopulated => "↷",
        StageDisposition::ZeroCandidateShortCircuit
        | StageDisposition::SkippedZeroCandidates
        | StageDisposition::SkippedValidationFailed
        | StageDisposition::SkippedEmptyDocket
        | StageDisposition::SkippedZeroEvidence => "⊘",
        StageDisposition::ValidationFailed => "✗",
    }
}

pub(crate) fn fast_forward_suffix(disp: StageDisposition) -> &'static str {
    match disp {
        StageDisposition::PrePopulated => " (fast-forward: pre-populated)",
        _ => "",
    }
}

/// Writes the `intake` provenance line for prospective statutory apprisal (`apprise`).
pub(crate) fn write_intent_intake_provenance(
    intake: Option<&CaseloadIntake>,
    disp: StageDisposition,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    let suffix = fast_forward_suffix(disp);
    let glyph = glyph_for(disp);
    let detail = match intake {
        Some(inc) if inc.scope == IntakeScope::AllTargets => {
            "design intent + all repository targets".to_string()
        }
        Some(inc) if !inc.diffs.is_empty() => {
            format!("design intent + {} modified file(s)", inc.diffs.len())
        }
        Some(inc) if !inc.target_paths.is_empty() => format!(
            "design intent + {} target directive(s) ({})",
            inc.target_paths.len(),
            inc.target_paths.join(", ")
        ),
        Some(_) => "prospective design intent ingested".to_string(),
        None => "(not populated)".to_string(),
    };
    writeln!(stdout, "  {glyph} intake    : {detail}{suffix}")
}

/// Writes the `intake` provenance line for contentious filing closures (`docket`, `admit`, `audit`).
pub(crate) fn write_filing_intake_provenance(
    intake: Option<&CaseloadIntake>,
    disp: StageDisposition,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    let suffix = fast_forward_suffix(disp);
    let glyph = glyph_for(disp);
    let detail = match intake {
        Some(inc) if inc.scope == IntakeScope::AllTargets => {
            "all repository targets ingested".to_string()
        }
        Some(inc) if !inc.diffs.is_empty() => {
            let added: usize = inc.diffs.values().map(|a| a.lines_added).sum();
            let deleted: usize = inc.diffs.values().map(|a| a.lines_deleted).sum();
            format!(
                "{} modified file(s) ingested (+{added} / -{deleted} lines)",
                inc.diffs.len()
            )
        }
        Some(inc) if !inc.target_paths.is_empty() => format!(
            "{} target directive(s) ingested ({})",
            inc.target_paths.len(),
            inc.target_paths.join(", ")
        ),
        Some(inc)
            if inc.pr_title.is_some()
                || inc.pr_body.is_some()
                || inc.linked_issues.as_ref().is_some_and(|v| !v.is_empty()) =>
        {
            "pull request metadata ingested".to_string()
        }
        Some(_) => "0 modified files ingested".to_string(),
        None => "(not populated)".to_string(),
    };
    writeln!(stdout, "  {glyph} intake    : {detail}{suffix}")
}

/// Writes the `discover` provenance line across multi-stage closures.
pub(crate) fn write_discover_provenance(
    discovery: Option<&CaseloadDiscovery>,
    disp: StageDisposition,
    total_canons: Option<usize>,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    let glyph = glyph_for(disp);
    let suffix = fast_forward_suffix(disp);
    let matched = discovery.map_or(0, |d| d.candidate_canons.len());
    let detail = match (matched, total_canons) {
        (0, Some(total)) => format!("0 candidate canons matched ({total} canons evaluated)"),
        (0, None) => "0 candidate canons matched".to_string(),
        (n, Some(total)) if total >= n => format!(
            "{n} candidate canon(s) matched ({} un-triggered)",
            total - n
        ),
        (n, _) => format!("{n} candidate canon(s) matched"),
    };
    writeln!(stdout, "  {glyph} discover  : {detail}{suffix}")
}

/// Writes the `validate` provenance line across multi-stage closures.
pub(crate) fn write_validate_provenance(
    validation: Option<&CaseloadValidation>,
    disp: StageDisposition,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    if disp == StageDisposition::SkippedZeroCandidates {
        return writeln!(
            stdout,
            "  ⊘ validate  : (fast-forward: zero candidate canons)"
        );
    }
    let glyph = glyph_for(disp);
    let suffix = fast_forward_suffix(disp);
    let (total, errors, warnings) = match validation {
        Some(val) => {
            let errs: usize = val.results.values().map(|m| m.error_count).sum();
            let warns: usize = val.results.values().map(|m| m.warning_count).sum();
            (val.results.len(), errs, warns)
        }
        None => (0, 0, 0),
    };
    let status_word = if disp == StageDisposition::ValidationFailed {
        "failed"
    } else {
        "valid"
    };
    writeln!(
        stdout,
        "  {glyph} validate  : {total} candidate canon(s) {status_word} ({errors} errors, {warnings} warnings){suffix}"
    )
}

/// Writes the `configure` provenance line across converged closures.
pub(crate) fn write_configure_provenance(
    caseload: &Caseload,
    disp: StageDisposition,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    match disp {
        StageDisposition::SkippedZeroCandidates => {
            writeln!(
                stdout,
                "  ⊘ configure : (fast-forward: zero candidate canons)"
            )
        }
        StageDisposition::SkippedValidationFailed => {
            writeln!(
                stdout,
                "  ⊘ configure : (fast-forward: canon validation failed)"
            )
        }
        _ => {
            let glyph = glyph_for(disp);
            let suffix = fast_forward_suffix(disp);
            let model = caseload
                .config
                .as_ref()
                .map_or("<unresolved>", |c| c.screener_model.as_str());
            writeln!(
                stdout,
                "  {glyph} configure : screener model '{model}'{suffix}"
            )
        }
    }
}

/// Writes the `configure` provenance line when `configure` was not needed because `.docket` was pre-populated empty.
pub(crate) fn write_admit_configure_provenance(
    caseload: &Caseload,
    disp: StageDisposition,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    if disp == StageDisposition::SkippedEmptyDocket {
        return writeln!(
            stdout,
            "  ⊘ configure : (fast-forward: empty active docket)"
        );
    }
    write_configure_provenance(caseload, disp, stdout)
}

/// Writes the `docket` provenance line across contentious cascade closures (`docket`, `admit`, `audit`).
pub(crate) fn write_docket_provenance(
    docket: Option<&CaseloadDocket>,
    disp: StageDisposition,
    threshold: f64,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    match disp {
        StageDisposition::SkippedZeroCandidates => {
            writeln!(
                stdout,
                "  ⊘ docket    : (fast-forward: zero candidate canons)"
            )
        }
        StageDisposition::SkippedValidationFailed => {
            writeln!(
                stdout,
                "  ⊘ docket    : (fast-forward: canon validation failed)"
            )
        }
        _ => {
            let suffix = fast_forward_suffix(disp);
            let (docketed, dismissed) =
                docket.map_or((0, 0), |d| (d.docketed_count(), d.dismissed_count()));
            let glyph = if docketed == 0 && disp == StageDisposition::Executed {
                "⊘"
            } else {
                glyph_for(disp)
            };
            writeln!(
                stdout,
                "  {glyph} docket    : {docketed} case(s) colorable ({dismissed} dismissed, threshold {threshold:.2}){suffix}"
            )
        }
    }
}

/// Writes the `admit` provenance line across contentious cascade closures (`admit`, `audit`).
pub(crate) fn write_admit_provenance(
    evidence: Option<&CaseloadEvidence>,
    disp: StageDisposition,
    threshold: f64,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    match disp {
        StageDisposition::SkippedZeroCandidates => {
            writeln!(
                stdout,
                "  ⊘ admit     : (fast-forward: zero candidate canons)"
            )
        }
        StageDisposition::SkippedValidationFailed => {
            writeln!(
                stdout,
                "  ⊘ admit     : (fast-forward: canon validation failed)"
            )
        }
        StageDisposition::SkippedEmptyDocket => {
            writeln!(
                stdout,
                "  ⊘ admit     : (fast-forward: empty active docket)"
            )
        }
        _ => {
            let suffix = fast_forward_suffix(disp);
            let (admitted, excluded, active_cases) = evidence.map_or((0, 0, 0), |ev| {
                (
                    ev.admitted_exhibit_count(),
                    ev.excluded_exhibit_count(),
                    ev.active_cases.len(),
                )
            });
            let glyph = if admitted == 0 && disp == StageDisposition::Executed {
                "⊘"
            } else {
                glyph_for(disp)
            };
            writeln!(
                stdout,
                "  {glyph} admit     : {admitted} exhibit(s) admitted across {active_cases} case(s) ({excluded} excluded, threshold {threshold:.2}){suffix}"
            )
        }
    }
}

/// Writes the `configure` provenance line across the 7-stage `audit` closure, displaying both screener and auditor models.
pub(crate) fn write_audit_configure_provenance(
    caseload: &Caseload,
    disp: StageDisposition,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    match disp {
        StageDisposition::SkippedZeroCandidates => {
            writeln!(
                stdout,
                "  ⊘ configure : (fast-forward: zero candidate canons)"
            )
        }
        StageDisposition::SkippedValidationFailed => {
            writeln!(
                stdout,
                "  ⊘ configure : (fast-forward: canon validation failed)"
            )
        }
        StageDisposition::SkippedEmptyDocket => {
            writeln!(
                stdout,
                "  ⊘ configure : (fast-forward: empty active docket)"
            )
        }
        StageDisposition::SkippedZeroEvidence => {
            writeln!(
                stdout,
                "  ⊘ configure : (fast-forward: no admissible exhibits)"
            )
        }
        _ => {
            let glyph = glyph_for(disp);
            let suffix = fast_forward_suffix(disp);
            let (screener, auditor) = caseload
                .config
                .as_ref()
                .map_or(("<unresolved>", "<unresolved>"), |c| {
                    (c.screener_model.as_str(), c.auditor_model.as_str())
                });
            writeln!(
                stdout,
                "  {glyph} configure : screener '{screener}', auditor '{auditor}'{suffix}"
            )
        }
    }
}

/// Writes the `audit` provenance line for the 7-stage judicial adjudication closure.
pub(crate) fn write_audit_provenance(
    verdict: Option<&CaseloadVerdict>,
    disp: StageDisposition,
    threshold: f64,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    match disp {
        StageDisposition::SkippedZeroCandidates => {
            writeln!(
                stdout,
                "  ⊘ audit     : (fast-forward: zero candidate canons)"
            )
        }
        StageDisposition::SkippedValidationFailed => {
            writeln!(
                stdout,
                "  ⊘ audit     : (fast-forward: canon validation failed)"
            )
        }
        StageDisposition::SkippedEmptyDocket => {
            writeln!(
                stdout,
                "  ⊘ audit     : 0 trials scheduled (fast-forward: empty active docket)"
            )
        }
        StageDisposition::SkippedZeroEvidence => {
            writeln!(
                stdout,
                "  ⊘ audit     : 0 trials scheduled (fast-forward: no admissible exhibits)"
            )
        }
        _ => {
            let suffix = fast_forward_suffix(disp);
            let (passed, failed, total) = verdict.map_or((0, 0, 0), |v| {
                (v.passed_count(), v.failed_count(), v.adjudications.len())
            });
            let glyph = if failed > 0 {
                "✗"
            } else if total == 0 && disp == StageDisposition::Executed {
                "⊘"
            } else {
                glyph_for(disp)
            };
            writeln!(
                stdout,
                "  {glyph} audit     : {passed} passed, {failed} failed across {total} trial(s) (threshold {threshold:.2}){suffix}"
            )
        }
    }
}
