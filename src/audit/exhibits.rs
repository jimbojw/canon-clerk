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

//! Admitted exhibit line-annotated rendering and statutory `references:` resolution for `audit` (`docs/architecture/nodes/audit.md` §4).

use crate::canon::Canon;
use crate::caseload::{CaseloadIntake, ExhibitAssessment, FileArtifact, LinkedIssueContext};
use crate::discover::{GlobPattern, WorkspaceSnapshot, normalize_workspace_path};
use std::collections::BTreeSet;
use std::fmt::Write as _;

const MAX_AUDIT_EXHIBIT_CHARS: usize = 24_576;
const MAX_AUDIT_REFERENCE_CHARS: usize = 12_288;

/// Returns `true` if `exhibit_id` names a reserved filing metadata plane rather than a workspace file path.
pub(crate) fn is_metadata_plane_id(exhibit_id: &str) -> bool {
    matches!(
        exhibit_id,
        "pr_title" | "pr_body" | "linked_issues" | "commit_messages"
    )
}

/// Resolves workspace-relative POSIX file paths matching a canon's frontmatter `references:` globs
/// in deterministic lexicographical order.
pub fn resolve_canon_reference_paths(canon: &Canon, workspace: &WorkspaceSnapshot) -> Vec<String> {
    if canon.references.is_empty() {
        return Vec::new();
    }

    let mut matchers = Vec::new();
    for raw_pattern in &canon.references {
        let trimmed = raw_pattern.trim();
        if trimmed.is_empty() {
            continue;
        }
        let candidate = normalize_workspace_path(trimmed).unwrap_or_else(|_| trimmed.to_string());
        if let Ok(glob) = GlobPattern::parse(&candidate) {
            matchers.push(glob);
        }
    }
    if matchers.is_empty() {
        return Vec::new();
    }

    let mut matched = BTreeSet::new();
    for path in workspace
        .existing_files
        .iter()
        .chain(workspace.file_contents.keys())
    {
        if matchers.iter().any(|m| m.is_match(path)) {
            matched.insert(path.clone());
        }
    }
    matched.into_iter().collect()
}

/// Renders a single admitted exhibit (`ExhibitAssessment`) into `out` with 1-based line numbers for file exhibits.
pub(super) fn append_admitted_exhibit_section(
    out: &mut String,
    exhibit: &ExhibitAssessment,
    intake: &CaseloadIntake,
    workspace: &WorkspaceSnapshot,
) {
    match exhibit.file_path.as_str() {
        "pr_title" => append_pr_title_exhibit(out, exhibit, intake),
        "pr_body" => append_pr_body_exhibit(out, exhibit, intake),
        "linked_issues" => append_linked_issues_exhibit(out, exhibit, intake),
        file_path => append_admitted_file_exhibit(out, file_path, exhibit, intake, workspace),
    }
}

fn append_pr_title_exhibit(out: &mut String, exhibit: &ExhibitAssessment, intake: &CaseloadIntake) {
    let _ = writeln!(
        out,
        "### Admitted Metadata Exhibit: `pr_title` (score: {:.2})",
        exhibit.admissibility_score
    );
    if let Some(title) = non_blank(intake.pr_title.as_deref()) {
        let _ = writeln!(out, "{title}");
    }
    out.push('\n');
}

fn append_pr_body_exhibit(out: &mut String, exhibit: &ExhibitAssessment, intake: &CaseloadIntake) {
    let _ = writeln!(
        out,
        "### Admitted Metadata Exhibit: `pr_body` (score: {:.2})",
        exhibit.admissibility_score
    );
    if let Some(body) = non_blank(intake.pr_body.as_deref()) {
        let bounded = truncate_chars(body, MAX_AUDIT_EXHIBIT_CHARS, "PR description");
        let _ = writeln!(out, "{bounded}");
    }
    out.push('\n');
}

fn append_linked_issues_exhibit(
    out: &mut String,
    exhibit: &ExhibitAssessment,
    intake: &CaseloadIntake,
) {
    let _ = writeln!(
        out,
        "### Admitted Metadata Exhibit: `linked_issues` (score: {:.2})",
        exhibit.admissibility_score
    );
    if let Some(issues) = intake.linked_issues.as_deref().filter(|v| !v.is_empty()) {
        append_linked_issues(out, issues);
    }
    out.push('\n');
}

fn append_linked_issues(out: &mut String, issues: &[LinkedIssueContext]) {
    for issue in issues {
        let title = non_blank(issue.title.as_deref()).unwrap_or("(untitled)");
        let _ = writeln!(out, "- Issue #{}: {title}", issue.number);
        if let Some(body) = non_blank(issue.body.as_deref()) {
            let bounded = truncate_chars(body, MAX_AUDIT_EXHIBIT_CHARS / 2, "issue body");
            let _ = writeln!(out, "  {bounded}");
        }
    }
}

fn append_admitted_file_exhibit(
    out: &mut String,
    file_path: &str,
    exhibit: &ExhibitAssessment,
    intake: &CaseloadIntake,
    workspace: &WorkspaceSnapshot,
) {
    if let Some(artifact) = intake.diffs.get(file_path) {
        append_diff_file_exhibit(out, artifact, exhibit, workspace);
    } else {
        let _ = writeln!(
            out,
            "### Admitted File Exhibit: `{file_path}` (Target Workspace File, score: {:.2})",
            exhibit.admissibility_score
        );
        if let Some(content) = workspace
            .file_contents
            .get(file_path)
            .and_then(|s| non_blank(Some(s.as_str())))
        {
            let numbered = annotate_snapshot_lines(content);
            let bounded = truncate_chars(&numbered, MAX_AUDIT_EXHIBIT_CHARS, "file snapshot");
            let _ = writeln!(out, "```text\n{bounded}\n```");
        } else {
            let _ = writeln!(
                out,
                "(Target file materialized in workspace scope; anchor file-level annotations at line 1.)"
            );
        }
    }
    out.push('\n');
}

fn append_diff_file_exhibit(
    out: &mut String,
    artifact: &FileArtifact,
    exhibit: &ExhibitAssessment,
    workspace: &WorkspaceSnapshot,
) {
    match &artifact.previous_path {
        Some(prev) => {
            let _ = writeln!(
                out,
                "### Admitted File Exhibit: `{}` ({} from `{}`, +{} / -{}, score: {:.2})",
                artifact.path,
                artifact.status.as_str(),
                prev,
                artifact.lines_added,
                artifact.lines_deleted,
                exhibit.admissibility_score
            );
        }
        None => {
            let _ = writeln!(
                out,
                "### Admitted File Exhibit: `{}` ({}, +{} / -{}, score: {:.2})",
                artifact.path,
                artifact.status.as_str(),
                artifact.lines_added,
                artifact.lines_deleted,
                exhibit.admissibility_score
            );
        }
    }

    if let Some(patch) = non_blank(artifact.patch.as_deref()) {
        let numbered = annotate_unified_patch_lines(patch);
        let bounded = truncate_chars(&numbered, MAX_AUDIT_EXHIBIT_CHARS, "diff patch");
        let _ = writeln!(out, "```diff\n{bounded}\n```");
    } else if let Some(content) = non_blank(artifact.content.as_deref()).or_else(|| {
        workspace
            .file_contents
            .get(&artifact.path)
            .and_then(|s| non_blank(Some(s.as_str())))
    }) {
        let numbered = annotate_snapshot_lines(content);
        let bounded = truncate_chars(&numbered, MAX_AUDIT_EXHIBIT_CHARS, "file snapshot");
        let _ = writeln!(out, "```text\n{bounded}\n```");
    } else if let Some(reason) = artifact.patch_omission_reason {
        let _ = writeln!(
            out,
            "(Diff patch omitted: {:?}; anchor file-level annotations at line 1.)",
            reason
        );
    }
}

/// Appends resolved static `references:` files as authoritative statutory grounding context.
pub(super) fn append_grounding_references_section(
    out: &mut String,
    reference_paths: &[String],
    admitted_file_paths: &BTreeSet<String>,
    workspace: &WorkspaceSnapshot,
) {
    if reference_paths.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "## Statutory Grounding References ({})\n\n\
         *(Authoritative workspace reference files declared in canon `references:`. \
         Do NOT emit `annotations` against reference paths unless they also appear in `## Admitted File Exhibit Paths`.)*\n",
        reference_paths.len()
    );
    for ref_path in reference_paths {
        if admitted_file_paths.contains(ref_path) {
            let _ = writeln!(
                out,
                "### Reference: `{ref_path}` *(also admitted above as a filing exhibit)*\n"
            );
            continue;
        }
        let _ = writeln!(out, "### Reference: `{ref_path}`");
        if let Some(content) = workspace
            .file_contents
            .get(ref_path)
            .and_then(|s| non_blank(Some(s.as_str())))
        {
            let bounded = truncate_chars(content, MAX_AUDIT_REFERENCE_CHARS, "reference file");
            let _ = writeln!(out, "```text\n{bounded}\n```\n");
        } else {
            let _ = writeln!(out, "(Present in workspace at `{ref_path}`.)\n");
        }
    }
}

/// Annotates unified diff hunks with explicit 1-based target-file line numbers (`+  42 | ...`).
pub(crate) fn annotate_unified_patch_lines(patch: &str) -> String {
    let mut out = String::with_capacity(patch.len() + 256);
    let mut current_new_line: Option<usize> = None;

    for raw_line in patch.lines() {
        if let Some(start_new) = parse_hunk_new_start(raw_line) {
            current_new_line = Some(start_new);
            let _ = writeln!(out, "{raw_line}");
            continue;
        }

        match current_new_line {
            Some(line_no) if raw_line.starts_with('+') && !raw_line.starts_with("+++") => {
                let _ = writeln!(out, "+{line_no:>4} | {}", &raw_line[1..]);
                current_new_line = Some(line_no + 1);
            }
            Some(line_no) if raw_line.starts_with(' ') => {
                let _ = writeln!(out, " {line_no:>4} | {}", &raw_line[1..]);
                current_new_line = Some(line_no + 1);
            }
            Some(_) if raw_line.starts_with('-') && !raw_line.starts_with("---") => {
                let _ = writeln!(out, "    - | {}", &raw_line[1..]);
            }
            _ => {
                let _ = writeln!(out, "{raw_line}");
            }
        }
    }

    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn parse_hunk_new_start(line: &str) -> Option<usize> {
    let after_prefix = line.strip_prefix("@@ -")?;
    let (header_body, _) = after_prefix.split_once(" @@")?;
    let (_, new_range) = header_body.split_once(" +")?;
    let start_str = new_range
        .split_once(',')
        .map_or(new_range, |(start, _)| start);
    let parsed = start_str.trim().parse::<usize>().ok()?;
    Some(parsed.max(1))
}

/// Annotates full-file UTF-8 snapshots with 1-based line numbers (`   1 | ...`).
pub(crate) fn annotate_snapshot_lines(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + 128);
    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let _ = writeln!(out, "{line_no:>4} | {line}");
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

pub(super) fn non_blank(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|s| !s.is_empty())
}

fn truncate_chars(text: &str, max_chars: usize, label: &str) -> String {
    let mut char_indices = text.char_indices();
    match char_indices.nth(max_chars) {
        Some((byte_idx, _)) => format!(
            "{}\n... [{label} truncated for trial prompt]",
            &text[..byte_idx]
        ),
        None => text.to_string(),
    }
}
