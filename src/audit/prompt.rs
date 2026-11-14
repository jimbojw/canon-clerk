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

//! Per-case judicial trial prompt assembly and reason-first JSON Schema builder for `audit` (`docs/architecture/nodes/audit.md` §4).

use super::exhibits::{
    append_admitted_exhibit_section, append_grounding_references_section, is_metadata_plane_id,
    non_blank, resolve_canon_reference_paths,
};
use crate::canon::Canon;
use crate::caseload::{CaseloadDiscovery, CaseloadIntake, ExhibitAssessment};
use crate::discover::WorkspaceSnapshot;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fmt::Write as _;

pub(super) const AUDIT_SYSTEM_INSTRUCTION: &str = "\
You are the Judge of the Court for Canon Clerk performing Substantive Judicial Adjudication (`audit`).
Given ONE active Case's Governing Canon Statute, any Statutory Grounding References, and strictly the Admitted Exhibits from the filing, render a substantive compliance decree (`compliance_summary`, `compliance_score`, and `annotations`).

Evaluation Rules (The Four-Step Judicial Decision Tree):
1. Step 1 — Normative Invariant Evaluation (`SPEC.md` §5 Tetrad Discipline):
   - Evaluate the Admitted Exhibits strictly against the Governing Canon's **Normative Invariant (`What`)** within its declared scope and stratum.
   - `Remediation` (`How`) and `Rationale` (`Why`) are **non-normative**: `Rationale` explains intent, and `Remediation` illustrates how a contributor might repair a violation *after* failure. NEVER fail code that satisfies the Normative Invariant (`What`) via a valid alternative mechanism (for example, resolving environment-variable fallbacks via an injected environment map rather than a CLI parser macro attribute, or rendering pre-canonicalized/pre-sorted domain collections without redundant re-sorting at the print loop, or emitting requested human-readable report summaries on `stdout` in `--format stylish` while `--json`/`compact` machine modes remain pure and operational telemetry/logs go to `stderr`).
   - Do NOT invent extra requirements beyond the written Normative Invariant, and do NOT speculate that un-admitted files violate invariants.
   - If the Admitted Exhibits comply with the Normative Invariant (or do not violate it), set `compliance_score` in `[0.80, 1.0]`, explain why the evidence complies in `compliance_summary`, and set `annotations` to `[]`.
2. Step 2 — Affirmative Defense (`Exception`) Screening:
   - If a prima facie violation of the invariant is present, check every declared `Exception` clause in the Governing Canon against the Admitted Exhibits and Statutory Grounding References.
   - If an `Exception`'s verifiable conditions are satisfied, short-circuit to a conditional pass: set `compliance_score` in `[0.80, 0.95]`, explicitly cite the satisfied `Exception` in `compliance_summary`, and set `annotations` to `[]`.
3. Step 3 — Unexcused Violation & Actionable Remediation:
   - If the Governing Canon is violated and no declared `Exception` applies, set `compliance_score` in `[0.0, 0.20]`.
   - In `compliance_summary`, state the concrete violation observed in the Admitted Exhibits followed by concise, actionable remediation guidance (`How`).
4. Step 4 — Deterministic `CodeAnnotation` Anchoring Guardrails:
   - When `compliance_score >= 0.5`: `annotations` MUST be `[]`.
   - When `compliance_score < 0.5` and `## Admitted File Exhibit Paths` lists `(none — metadata-only case)`: `annotations` MUST be `[]` (never pass metadata tokens like `pr_title` or `pr_body` as file paths).
   - When `compliance_score < 0.5` and `## Admitted File Exhibit Paths` is non-empty:
     * Every `path` MUST be copied verbatim from `## Admitted File Exhibit Paths` (NEVER cite un-admitted files or missing companion files such as an un-touched `README.md`).
     * Use exact 1-based target-file line numbers shown in the left margin (`+  42 |` or `  42 |`) with `start_line >= 1` and `end_line >= start_line`.
     * For file-level violations or missing companion artifacts (e.g., a `feat` PR omitting a required `README.md` update), anchor `start_line: 1` and `end_line: 1` on the primary triggering file from `## Admitted File Exhibit Paths`.
     * Set `annotation_level` to `\"failure\"` for blocking statute violations (`\"warning\"` or `\"notice\"` only for supplementary context).
5. Polarized Score Calibration (Zero Boundary Ambiguity):
   - Reason first in `compliance_summary` before emitting `compliance_score`.
   - Commit decisively to `[0.80, 1.0]` (Pass) or `[0.0, 0.20]` (Fail); avoid ambiguous boundary scores in `0.35 - 0.65`.";

/// Builds a constant-size (`O(1)`) reason-first JSON Schema enforcing
/// `compliance_summary`, `compliance_score`, then `annotations` (`docs/architecture/nodes/audit.md` §4.5).
pub(super) fn build_audit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "compliance_summary": {
                "type": "string",
                "description": "Substantive judicial decree written BEFORE scoring: states whether the admitted exhibits comply with the canon invariant, cites any matched Exception, or diagnoses the violation with actionable remediation."
            },
            "compliance_score": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0,
                "description": "Polarized compliance score in [0.0, 1.0]: >= 0.80 when compliant or excused by a declared Exception; <= 0.20 when an unexcused statute violation is present."
            },
            "annotations": {
                "type": "array",
                "description": "Source line annotations pinpointing violations on admitted file exhibits (empty [] when compliant, excused, or when only PR metadata planes were admitted).",
                "items": single_annotation_schema()
            }
        },
        "propertyOrdering": ["compliance_summary", "compliance_score", "annotations"],
        "required": ["compliance_summary", "compliance_score", "annotations"]
    })
}

fn single_annotation_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Exact repository-relative POSIX file path copied verbatim from `## Admitted File Exhibit Paths`."
            },
            "start_line": {
                "type": "integer",
                "minimum": 1,
                "description": "1-based starting line number in the target file (use 1 for file-level or missing-companion omission findings)."
            },
            "end_line": {
                "type": "integer",
                "minimum": 1,
                "description": "1-based ending line number in the target file (>= start_line)."
            },
            "start_column": {
                "type": "integer",
                "minimum": 1,
                "description": "Optional 1-based starting column number."
            },
            "end_column": {
                "type": "integer",
                "minimum": 1,
                "description": "Optional 1-based ending column number."
            },
            "annotation_level": {
                "type": "string",
                "enum": ["failure", "warning", "notice"],
                "description": "Diagnostic severity ('failure' for canon violations)."
            },
            "title": {
                "type": "string",
                "description": "Short violation title or canon identifier."
            },
            "message": {
                "type": "string",
                "description": "Concise violation explanation and concrete fix at this source location."
            }
        },
        "propertyOrdering": [
            "path",
            "start_line",
            "end_line",
            "start_column",
            "end_column",
            "annotation_level",
            "title",
            "message"
        ],
        "required": ["path", "start_line", "end_line", "annotation_level", "message"]
    })
}

/// Assembles the isolated judicial trial prompt for a single active case (`canon_ref`)
/// and returns `(user_prompt, admitted_file_paths)`.
pub(super) fn build_audit_case_user_prompt(
    canon_ref: &str,
    canon: &Canon,
    admitted_exhibits: &[&ExhibitAssessment],
    intake: &CaseloadIntake,
    discovery: &CaseloadDiscovery,
    workspace: &WorkspaceSnapshot,
) -> (String, BTreeSet<String>) {
    let admitted_file_paths: BTreeSet<String> = admitted_exhibits
        .iter()
        .map(|e| e.file_path.as_str())
        .filter(|id| !is_metadata_plane_id(id))
        .map(ToString::to_string)
        .collect();

    let reference_paths = resolve_canon_reference_paths(canon, workspace);
    let mut out = String::with_capacity(1536 + admitted_exhibits.len() * 768);

    append_case_overview(&mut out, canon_ref, intake, discovery);
    append_governing_canon_statute(&mut out, canon_ref, canon);
    append_grounding_references_section(
        &mut out,
        &reference_paths,
        &admitted_file_paths,
        workspace,
    );

    let _ = writeln!(out, "## Admitted Exhibits ({})\n", admitted_exhibits.len());
    for exhibit in admitted_exhibits {
        append_admitted_exhibit_section(&mut out, exhibit, intake, workspace);
    }

    let _ = writeln!(out, "## Admitted File Exhibit Paths");
    if admitted_file_paths.is_empty() {
        let _ = writeln!(out, "(none — metadata-only case; emit `annotations: []`)");
    } else {
        for (idx, path) in admitted_file_paths.iter().enumerate() {
            let _ = writeln!(out, "{}. `{path}`", idx + 1);
        }
    }

    (out, admitted_file_paths)
}

fn append_case_overview(
    out: &mut String,
    canon_ref: &str,
    intake: &CaseloadIntake,
    discovery: &CaseloadDiscovery,
) {
    let _ = writeln!(out, "## Active Case Trial Overview");
    let _ = writeln!(out, "- Case Docket Path: `{canon_ref}`");
    let _ = writeln!(out, "- Intake Scope: {}", intake.scope.as_str());
    let _ = writeln!(out, "- Discovery Mode: {}", discovery.mode.as_str());
    if let Some(title) = non_blank(intake.pr_title.as_deref()) {
        let _ = writeln!(out, "- PR Title: {title}");
    }
    if let Some(intent) = non_blank(intake.intent.as_deref()) {
        let _ = writeln!(out, "- Design Intent: {intent}");
    }
    out.push('\n');
}

fn append_governing_canon_statute(out: &mut String, canon_ref: &str, canon: &Canon) {
    let _ = writeln!(out, "## Governing Canon Statute: `{canon_ref}`");
    let _ = writeln!(out, "- ID: `{}`", canon.id);
    let _ = writeln!(out, "- Title: {}", canon.title);
    let _ = writeln!(out, "- Normative Invariant: {}", canon.invariant.trim());
    if !canon.triggers.is_empty() {
        let _ = writeln!(out, "- Triggers: {}", canon.triggers.join(", "));
    }
    if !canon.references.is_empty() {
        let _ = writeln!(out, "- References: {}", canon.references.join(", "));
    }
    if !canon.exceptions.is_empty() {
        let _ = writeln!(out, "- Declared Exceptions ({}):", canon.exceptions.len());
        for exc in &canon.exceptions {
            let _ = writeln!(out, "  * {}", exc.trim());
        }
    }
    if let Some(remediation) = non_blank(canon.remediation.as_deref()) {
        let _ = writeln!(
            out,
            "- Declared Remediation (Non-Normative Fix Guidance Upon Failure): {remediation}"
        );
    }
    let body = canon.raw_body.trim();
    let statute = if body.is_empty() {
        canon.invariant.trim()
    } else {
        body
    };
    let _ = writeln!(out, "\n{statute}\n");
}
