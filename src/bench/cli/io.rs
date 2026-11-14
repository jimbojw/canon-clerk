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

//! Disk workspace materialization and report artifact output helpers for [`super::ConsistencyBenchCli`].

use super::ConsistencyBenchFormat;
use crate::audit::resolve_canon_reference_paths;
use crate::bench::render::render_consistency_markdown;
use crate::bench::{BenchmarkStage, ConsistencyBenchReport};
use crate::canon::ParseCanonOptions;
use crate::caseload::Caseload;
use crate::cli::{CliError, map_discover_error};
use crate::discover::WorkspaceSnapshot;
use std::fs;
use std::io::Write;
use std::path::Path;

pub(super) fn materialize_disk_snapshot(
    workspace_root: &Path,
    canon_patterns: &[String],
    incoming: Option<&Caseload>,
) -> Result<WorkspaceSnapshot, CliError> {
    let mut snapshot =
        WorkspaceSnapshot::from_disk(workspace_root, canon_patterns).map_err(map_discover_error)?;
    if let Some(disc) = incoming.and_then(|c| c.discovery.as_ref()) {
        for canon_ref in &disc.candidate_canons {
            if !snapshot.canons.contains_key(canon_ref) {
                let full_path = workspace_root.join(canon_ref);
                if full_path.is_file()
                    && let Ok(raw) = fs::read_to_string(&full_path)
                {
                    let opts = ParseCanonOptions::with_source_ref(canon_ref.clone());
                    snapshot = snapshot.with_canon_markdown(&raw, &opts);
                }
            }
        }
    }
    Ok(snapshot)
}

pub(super) fn populate_stage_disk_contents(
    stage: BenchmarkStage,
    workspace_root: &Path,
    caseload: &Caseload,
    snapshot: &mut WorkspaceSnapshot,
) {
    if !matches!(stage, BenchmarkStage::Admit | BenchmarkStage::Audit) {
        return;
    }
    let Some(disc) = &caseload.discovery else {
        return;
    };
    snapshot.populate_file_contents_from_disk(workspace_root, &disc.target_files);
    if stage == BenchmarkStage::Audit {
        let mut ref_paths = Vec::new();
        for canon_ref in &disc.candidate_canons {
            if let Some(canon) = snapshot.canons.get(canon_ref) {
                ref_paths.extend(resolve_canon_reference_paths(canon, snapshot));
            }
        }
        if !ref_paths.is_empty() {
            snapshot.populate_file_contents_from_disk(workspace_root, &ref_paths);
        }
    }
}

pub(super) fn write_report_json_file(
    path: &Path,
    report: &ConsistencyBenchReport,
) -> Result<(), CliError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::usage(format!(
                "Failed to create parent directory `{}` for `--output-json`: {err}\n  \
                 Hint: Verify directory write permissions for `{}`.",
                parent.display(),
                path.display()
            ))
        })?;
    }
    let payload = serde_json::to_string_pretty(report)
        .map_err(|err| CliError::runtime(format!("Failed to serialize benchmark JSON: {err}")))?;
    fs::write(path, format!("{payload}\n")).map_err(|err| {
        CliError::usage(format!(
            "Failed to write benchmark JSON to `{}`: {err}\n  \
             Hint: Verify that `{}` is writable.",
            path.display(),
            path.display()
        ))
    })
}

pub(super) fn emit_stdout_report(
    report: &ConsistencyBenchReport,
    format: ConsistencyBenchFormat,
    include_stable: bool,
    stdout: &mut dyn Write,
) -> Result<(), CliError> {
    match format {
        ConsistencyBenchFormat::Json => {
            let payload = serde_json::to_string_pretty(report).map_err(|err| {
                CliError::runtime(format!("Failed to serialize benchmark JSON: {err}"))
            })?;
            writeln!(stdout, "{payload}")
                .map_err(|err| CliError::runtime(format!("Failed to write stdout: {err}")))
        }
        ConsistencyBenchFormat::Markdown => {
            let rendered = render_consistency_markdown(report, include_stable);
            write!(stdout, "{rendered}")
                .map_err(|err| CliError::runtime(format!("Failed to write stdout: {err}")))
        }
    }
}
