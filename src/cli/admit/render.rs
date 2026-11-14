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

//! Terminal Funnel Provenance receipt and `stylish`/`compact`/`json` output formatters for `canon-clerk admit`.

use super::{AdmitArgs, AdmitFormat};
use crate::caseload::{Caseload, CaseloadDiscovery, CaseloadDocket, CaseloadEvidence};
use crate::cli::CliError;
use crate::cli::provenance::{
    StageDisposition, write_admit_configure_provenance, write_admit_provenance,
    write_discover_provenance, write_docket_provenance, write_filing_intake_provenance,
    write_validate_provenance,
};
use std::io::Write;

/// Complete provenance watermarks across the `[intake, discover, validate, configure, docket, admit]` closure.
#[derive(Debug, Clone, Copy)]
pub(super) struct AdmitWatermarks {
    pub intake: StageDisposition,
    pub discover: StageDisposition,
    pub validate: StageDisposition,
    pub configure: StageDisposition,
    pub docket: StageDisposition,
    pub admit: StageDisposition,
    pub total_workspace_canons: Option<usize>,
    pub docket_threshold: f64,
    pub admit_threshold: f64,
}

pub(super) fn render_admit_output(
    args: &AdmitArgs,
    caseload: &Caseload,
    watermarks: &AdmitWatermarks,
    stdout: &mut dyn Write,
) -> Result<(), CliError> {
    let effective_format = if args.io.json {
        AdmitFormat::Json
    } else {
        args.format
    };

    match effective_format {
        AdmitFormat::Json => {
            let serialized = serde_json::to_string_pretty(caseload).map_err(|err| {
                CliError::runtime(format!("Failed to serialize Caseload JSON: {err}"))
            })?;
            writeln!(stdout, "{serialized}")
                .map_err(|err| CliError::runtime(format!("Failed to write to stdout: {err}")))?;
        }
        AdmitFormat::Compact => {
            if let Some(evidence) = &caseload.evidence {
                render_compact_output(evidence, stdout).map_err(|err| {
                    CliError::runtime(format!("Failed to write compact output: {err}"))
                })?;
            }
        }
        AdmitFormat::Stylish => {
            render_stylish_output(caseload, watermarks, stdout).map_err(|err| {
                CliError::runtime(format!("Failed to write stylish output: {err}"))
            })?;
        }
    }

    Ok(())
}

fn render_compact_output(
    evidence: &CaseloadEvidence,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    for (canon_ref, items) in &evidence.exhibits {
        for item in items {
            let single_line_summary = item
                .admissibility_summary
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            writeln!(
                stdout,
                "{canon_ref}\t{}\t{}\t{:.2}\t{single_line_summary}",
                item.file_path,
                item.admissibility_status.as_str(),
                item.admissibility_score
            )?;
        }
    }
    Ok(())
}

fn render_stylish_output(
    caseload: &Caseload,
    wm: &AdmitWatermarks,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(stdout, "Pipeline Provenance:")?;
    write_filing_intake_provenance(caseload.intake.as_ref(), wm.intake, stdout)?;
    write_discover_provenance(
        caseload.discovery.as_ref(),
        wm.discover,
        wm.total_workspace_canons,
        stdout,
    )?;
    write_validate_provenance(caseload.validation.as_ref(), wm.validate, stdout)?;
    write_admit_configure_provenance(caseload, wm.configure, stdout)?;
    write_docket_provenance(
        caseload.docket.as_ref(),
        wm.docket,
        wm.docket_threshold,
        stdout,
    )?;
    write_admit_provenance(
        caseload.evidence.as_ref(),
        wm.admit,
        wm.admit_threshold,
        stdout,
    )?;
    writeln!(stdout)?;

    if let Some(evidence) = &caseload.evidence {
        write_evidence_manifest_summary(
            evidence,
            caseload.discovery.as_ref(),
            caseload.docket.as_ref(),
            wm.admit_threshold,
            stdout,
        )?;
    }

    Ok(())
}

fn write_evidence_manifest_summary(
    evidence: &CaseloadEvidence,
    discovery: Option<&CaseloadDiscovery>,
    docket: Option<&CaseloadDocket>,
    threshold: f64,
    stdout: &mut dyn Write,
) -> std::io::Result<()> {
    let admitted = evidence.admitted_exhibit_count();
    let excluded = evidence.excluded_exhibit_count();
    let active_cases = evidence.active_cases.len();
    writeln!(
        stdout,
        "Evidence Manifest ({admitted} admitted, {excluded} excluded across {active_cases} active case(s)):"
    )?;

    if discovery.is_some_and(|d| d.candidate_canons.is_empty()) && evidence.exhibits.is_empty() {
        return writeln!(
            stdout,
            "  0 candidate canons matched filing; 0 exhibits screened."
        );
    }

    if docket.is_some_and(|d| d.active_docket.is_empty()) && evidence.exhibits.is_empty() {
        return writeln!(
            stdout,
            "  0 active cases on docket; 0 exhibits screened (downstream audit pruned)."
        );
    }

    if evidence.active_cases.is_empty() {
        return writeln!(
            stdout,
            "  0 admissible exhibits at threshold {threshold:.2}; all active cases dismissed without trial (downstream audit pruned)."
        );
    }

    for canon_ref in &evidence.active_cases {
        let admitted_items = evidence.admitted_exhibits_for(canon_ref);
        writeln!(
            stdout,
            "  Case: {canon_ref} ({} admitted):",
            admitted_items.len()
        )?;
        for item in admitted_items {
            writeln!(
                stdout,
                "    - {} [score: {:.2}]",
                item.file_path, item.admissibility_score
            )?;
            writeln!(stdout, "        {}", item.admissibility_summary)?;
        }
    }
    Ok(())
}
