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

//! Compact `(query, documents)` cross-encoder projection for local `/v1/rerank` discriminators.

use std::fmt::Write as _;

const MAX_COMPACT_QUERY_CHARS: usize = 1_800;
const MAX_COMPACT_ADMIT_QUERY_CHARS: usize = 3_200;
const TOTAL_DIFF_PREVIEW_BUDGET_LINES: usize = 15;
const MIN_DIFF_PREVIEW_LINES_PER_FILE: usize = 3;
const MAX_DIFF_PREVIEW_LINES_PER_FILE: usize = 15;
const MAX_DIFF_LINE_CHARS: usize = 110;

/// Projects a single-question `vet` compliance check (`state` = case header + statute + exhibits)
/// into `(query = generic compliance rubric, document = case state)` so retrieval-trained
/// rerankers do not false-vet topical violations via domain keyword overlap in `<Query>`.
pub(crate) fn project_vet_compliance(state: &str, fallback_rubric: &str) -> (String, String) {
    (fallback_rubric.to_string(), state.to_string())
}

/// Returns `true` when all questions follow the `docket` / `apprise` `### Canon:` prompt format.
pub(crate) fn is_canon_screening_batch<'a>(
    mut questions: impl Iterator<Item = &'a String>,
) -> bool {
    questions.all(|q| q.trim_start().starts_with("### Canon:"))
}

/// Returns `true` when all questions follow the `admit` `Governing Canon:` prompt format.
pub(crate) fn is_admit_screening_batch<'a>(
    mut questions: impl Iterator<Item = &'a String>,
) -> bool {
    questions.all(|q| q.trim_start().starts_with("Governing Canon:"))
}

/// Extracts `Canon (<ref>): <Normative Invariant>` from a `docket`/`apprise` question block,
/// omitting non-normative `Rationale`/`Remediation` prose and generative few-shot suffixes.
pub(crate) fn extract_canon_invariant_doc(question_text: &str) -> String {
    let mut canon_ref = "";
    let mut past_meta = false;
    let mut kept = Vec::new();

    for line in question_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("### Canon:") {
            canon_ref = rest.trim().trim_matches('`');
            continue;
        }
        if !past_meta {
            if trimmed.is_empty() || trimmed.starts_with("- ") || trimmed.starts_with('#') {
                continue;
            }
            past_meta = true;
        }
        if is_non_normative_boundary(trimmed) {
            break;
        }
        kept.push(line);
    }

    let invariant = kept.join("\n").trim().to_string();
    if invariant.is_empty() {
        question_text.trim().to_string()
    } else if canon_ref.is_empty() {
        invariant
    } else {
        format!("Canon ({canon_ref}): {invariant}")
    }
}

fn is_non_normative_boundary(trimmed: &str) -> bool {
    let lower = trimmed
        .trim_start_matches(['#', '*', ' '])
        .to_ascii_lowercase();
    lower.starts_with("rationale:")
        || lower.starts_with("rationale ")
        || lower == "rationale"
        || lower.starts_with("remediation:")
        || lower.starts_with("remediation ")
        || lower == "remediation"
        || trimmed.starts_with("Question:")
        || trimmed.starts_with("- Exact `canon_path`:")
}

/// Condenses a `docket`/`apprise` shared filing `state` into a concise cross-encoder `<Query>`,
/// dividing the diff-line preview budget fairly across modified files.
pub(crate) fn build_compact_jurisdiction_query(state: &str) -> String {
    let mut out = String::from(
        "Determine whether each engineering canon has subject-matter jurisdiction over this \
         change (match rules whose governed domain construct—such as CLI flags, error messages, \
         configuration precedence, credentials, or feat PR README documentation—is touched, even \
         when compliant):\n",
    );
    let file_count = state
        .lines()
        .filter(|l| l.starts_with("### File: `"))
        .count();
    let per_file_cap = (TOTAL_DIFF_PREVIEW_BUDGET_LINES / file_count.max(1)).clamp(
        MIN_DIFF_PREVIEW_LINES_PER_FILE,
        MAX_DIFF_PREVIEW_LINES_PER_FILE,
    );

    let mut files = Vec::new();
    let mut diff_previews = Vec::new();
    let mut per_file_lines = 0usize;

    for line in state.lines() {
        if is_filing_metadata_bullet(line) {
            let _ = writeln!(out, "{}", &line[2..]);
        } else if let Some(rest) = line.strip_prefix("### File: `")
            && let Some((path, _)) = rest.split_once('`')
        {
            files.push(path);
            per_file_lines = 0;
        } else if per_file_lines < per_file_cap && is_changed_code_line(line) {
            per_file_lines += 1;
            diff_previews.push(truncate_str(line, MAX_DIFF_LINE_CHARS));
        }
    }

    if !files.is_empty() {
        let _ = writeln!(out, "Modified files: {}", files.join(", "));
    }
    if !diff_previews.is_empty() {
        out.push_str("Sample diff lines:\n");
        for preview in diff_previews {
            if out.len() >= MAX_COMPACT_QUERY_CHARS {
                break;
            }
            let _ = writeln!(out, "{preview}");
        }
    }
    truncate_str(&out, MAX_COMPACT_QUERY_CHARS).to_string()
}

fn is_filing_metadata_bullet(line: &str) -> bool {
    [
        "- PR Title:",
        "- PR Description:",
        "- Design Intent:",
        "- Target Directives:",
        "- Materialized Target Files:",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

fn is_changed_code_line(line: &str) -> bool {
    if !(line.starts_with('+') || line.starts_with('-'))
        || line.starts_with("+++")
        || line.starts_with("---")
    {
        return false;
    }
    let body = line[1..].trim();
    !body.is_empty() && !body.starts_with("//")
}

/// Projects an `admit` batch into a concise exhibit query and compact `(Canon, Target Exhibit)` documents.
pub(crate) fn project_admit_question_doc(question_text: &str) -> String {
    match question_text.split_once("\n\nQuestion:") {
        Some((header, _)) => header.trim().to_string(),
        None => question_text.trim().to_string(),
    }
}

pub(crate) fn build_compact_admit_query(state: &str) -> String {
    let prefix = "Determine whether the Target Candidate Exhibit introduces, modifies, or \
                  evidences an instance of the Governing Canon's domain construct:\n";
    let cap = MAX_COMPACT_ADMIT_QUERY_CHARS.saturating_sub(prefix.len());
    format!("{prefix}{}", truncate_str(state, cap))
}

fn truncate_str(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &text[..byte_idx],
        None => text,
    }
}
