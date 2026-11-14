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

//! Candidate exhibit resolution and structured rendering for `admit` (`docs/architecture/nodes/admit.md` §4).

use crate::canon::{Canon, CanonScope, InspectToken};
use crate::caseload::{
    CaseloadDiscovery, CaseloadIntake, DiscoveryMode, FileArtifact, LinkedIssueContext,
};
use crate::discover::{WorkspaceSnapshot, scope_relative_path};
use std::collections::BTreeSet;
use std::fmt::Write as _;

const MAX_MICRO_EXHIBIT_CHARS: usize = 8_192;

/// Returns `true` if `exhibit_id` names a reserved filing metadata plane rather than a file path.
pub(super) fn is_metadata_plane_id(exhibit_id: &str) -> bool {
    matches!(
        exhibit_id,
        "pr_title" | "pr_body" | "linked_issues" | "commit_messages"
    )
}

fn is_metadata_plane_present(plane: InspectToken, intake: &CaseloadIntake) -> bool {
    match plane {
        InspectToken::PrTitle => non_blank(intake.pr_title.as_deref()).is_some(),
        InspectToken::PrBody => non_blank(intake.pr_body.as_deref()).is_some(),
        InspectToken::LinkedIssues => intake.linked_issues.as_ref().is_some_and(|v| !v.is_empty()),
        InspectToken::Diff | InspectToken::CommitMessages => false,
    }
}

fn is_metadata_id_present(exhibit_id: &str, intake: &CaseloadIntake) -> bool {
    match exhibit_id {
        "pr_title" => non_blank(intake.pr_title.as_deref()).is_some(),
        "pr_body" => non_blank(intake.pr_body.as_deref()).is_some(),
        "linked_issues" => intake.linked_issues.as_ref().is_some_and(|v| !v.is_empty()),
        _ => false,
    }
}

/// Resolves the stably sorted candidate exhibit identifiers (repository-relative POSIX file paths
/// and present `inspect:` metadata plane tokens) for a single active case (`canon_ref`).
pub(super) fn resolve_case_candidate_exhibits(
    canon_ref: &str,
    canon: &Canon,
    intake: &CaseloadIntake,
    discovery: &CaseloadDiscovery,
) -> Vec<String> {
    if discovery.mode == DiscoveryMode::Triggered
        && let Some(joined) = discovery.triggers_join.get(canon_ref)
        && !joined.is_empty()
    {
        let mut unique = BTreeSet::new();
        for item in joined {
            if is_metadata_plane_id(item) {
                if is_metadata_id_present(item, intake) {
                    unique.insert(item.clone());
                }
            } else {
                unique.insert(item.clone());
            }
        }
        return unique.into_iter().collect();
    }

    let mut unique = BTreeSet::new();
    let canon_scope = canon.scope.as_deref().map(|s| CanonScope(s.to_string()));
    let inspects_diff = canon.inspect.iter().any(|p| p.token == InspectToken::Diff);

    if inspects_diff {
        if !discovery.target_files.is_empty() {
            for file in &discovery.target_files {
                if scope_relative_path(file, canon_scope.as_ref()).is_some() {
                    unique.insert(file.clone());
                }
            }
        } else {
            for file in intake.diffs.keys() {
                if scope_relative_path(file, canon_scope.as_ref()).is_some() {
                    unique.insert(file.clone());
                }
            }
        }
    }

    for plane in &canon.inspect {
        if plane.token != InspectToken::Diff && is_metadata_plane_present(plane.token, intake) {
            unique.insert(plane.token.as_str().to_string());
        }
    }

    unique.into_iter().collect()
}

/// Appends the formatted representation of a single candidate exhibit (`file_path` or metadata plane).
pub(super) fn append_candidate_exhibit_section(
    out: &mut String,
    exhibit_id: &str,
    intake: &CaseloadIntake,
    workspace: &WorkspaceSnapshot,
) {
    match exhibit_id {
        "pr_title" => append_pr_title_exhibit(out, intake),
        "pr_body" => append_pr_body_exhibit(out, intake),
        "linked_issues" => append_linked_issues_exhibit(out, intake),
        file_path => append_file_exhibit(out, file_path, intake, workspace),
    }
}

fn append_pr_title_exhibit(out: &mut String, intake: &CaseloadIntake) {
    let _ = writeln!(
        out,
        "### Candidate Exhibit: `pr_title` (Pull Request Title Metadata)"
    );
    if let Some(title) = non_blank(intake.pr_title.as_deref()) {
        let _ = writeln!(out, "{title}");
    }
    let _ = writeln!(out, "- Exact `file_path`: `pr_title`\n");
}

fn append_pr_body_exhibit(out: &mut String, intake: &CaseloadIntake) {
    let _ = writeln!(
        out,
        "### Candidate Exhibit: `pr_body` (Pull Request Description Metadata)"
    );
    if let Some(body) = non_blank(intake.pr_body.as_deref()) {
        let bounded = truncate_chars(body, MAX_MICRO_EXHIBIT_CHARS, "metadata");
        let _ = writeln!(out, "{bounded}");
    }
    let _ = writeln!(out, "- Exact `file_path`: `pr_body`\n");
}

fn append_linked_issues_exhibit(out: &mut String, intake: &CaseloadIntake) {
    let _ = writeln!(
        out,
        "### Candidate Exhibit: `linked_issues` (Linked Issue Metadata)"
    );
    if let Some(issues) = intake.linked_issues.as_deref().filter(|v| !v.is_empty()) {
        append_linked_issue_items(out, issues);
    }
    let _ = writeln!(out, "- Exact `file_path`: `linked_issues`\n");
}

fn append_linked_issue_items(out: &mut String, issues: &[LinkedIssueContext]) {
    for issue in issues {
        let title = non_blank(issue.title.as_deref()).unwrap_or("(untitled)");
        let _ = writeln!(out, "- Issue #{}: {title}", issue.number);
        if let Some(body) = non_blank(issue.body.as_deref()) {
            let bounded = truncate_chars(body, MAX_MICRO_EXHIBIT_CHARS / 2, "issue body");
            let _ = writeln!(out, "  {bounded}");
        }
    }
}

fn append_file_exhibit(
    out: &mut String,
    file_path: &str,
    intake: &CaseloadIntake,
    workspace: &WorkspaceSnapshot,
) {
    if let Some(artifact) = intake.diffs.get(file_path) {
        append_diff_artifact_exhibit(out, artifact, workspace);
    } else {
        let _ = writeln!(
            out,
            "### Candidate Exhibit: `{file_path}` (Target Workspace File)"
        );
        if let Some(content) = workspace
            .file_contents
            .get(file_path)
            .and_then(|s| non_blank(Some(s.as_str())))
        {
            let bounded = truncate_chars(content, MAX_MICRO_EXHIBIT_CHARS, "file snapshot");
            let _ = writeln!(out, "```text\n{bounded}\n```");
        } else {
            let _ = writeln!(out, "(Target file path materialized in workspace scope.)");
        }
    }
    let _ = writeln!(out, "- Exact `file_path`: `{file_path}`\n");
}

fn append_diff_artifact_exhibit(
    out: &mut String,
    artifact: &FileArtifact,
    workspace: &WorkspaceSnapshot,
) {
    match &artifact.previous_path {
        Some(prev) => {
            let _ = writeln!(
                out,
                "### Candidate Exhibit: `{}` ({} from `{}`, +{} / -{})",
                artifact.path,
                artifact.status.as_str(),
                prev,
                artifact.lines_added,
                artifact.lines_deleted
            );
        }
        None => {
            let _ = writeln!(
                out,
                "### Candidate Exhibit: `{}` ({}, +{} / -{})",
                artifact.path,
                artifact.status.as_str(),
                artifact.lines_added,
                artifact.lines_deleted
            );
        }
    }

    if let Some(patch) = non_blank(artifact.patch.as_deref()) {
        let bounded = truncate_chars(patch, MAX_MICRO_EXHIBIT_CHARS, "patch");
        let _ = writeln!(out, "```diff\n{bounded}\n```");
    } else if let Some(content) = non_blank(artifact.content.as_deref()).or_else(|| {
        workspace
            .file_contents
            .get(&artifact.path)
            .and_then(|s| non_blank(Some(s.as_str())))
    }) {
        let bounded = truncate_chars(content, MAX_MICRO_EXHIBIT_CHARS, "file snapshot");
        let _ = writeln!(out, "```text\n{bounded}\n```");
    } else if let Some(reason) = artifact.patch_omission_reason {
        let _ = writeln!(out, "(Diff patch omitted: {:?})", reason);
    }
}

pub(super) fn non_blank(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|s| !s.is_empty())
}

fn truncate_chars(text: &str, max_chars: usize, label: &str) -> String {
    let mut char_indices = text.char_indices();
    match char_indices.nth(max_chars) {
        Some((byte_idx, _)) => format!(
            "{}\n... [{label} truncated for micro screening]",
            &text[..byte_idx]
        ),
        None => text.to_string(),
    }
}
