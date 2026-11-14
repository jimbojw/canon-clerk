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

//! Workspace file tree and Canon corpus snapshot materializer for `discover`.

use super::DiscoverError;
use super::glob_matcher::{
    DEFAULT_CANON_GLOB, GlobPattern, is_ignored_path, normalize_workspace_path,
};
use crate::canon::{
    Canon, DEFAULT_GLOBAL_TRIGGER_PATTERN, DEFAULT_INSPECT_PLANES, ParseCanonOptions, derive_id,
    derive_triggers, id_to_title_case, parse_canon, resolve_source_info,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// In-memory snapshot of workspace file paths, optional UTF-8 target file contents, and discovered [`Canon`] definitions.
///
/// Decouples filesystem traversal from pure domain evaluation (`discover`, `validate`, `apprise`, `docket`, `admit`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    /// Stably sorted workspace-relative POSIX file paths present in the workspace.
    pub existing_files: BTreeSet<String>,
    /// Discovered [`Canon`] entities keyed by workspace-relative POSIX path or source reference.
    pub canons: BTreeMap<String, Canon>,
    /// Optional materialized UTF-8 file contents keyed by workspace-relative POSIX path (populated JIT for target-path sweeps).
    pub file_contents: BTreeMap<String, String>,
}

impl WorkspaceSnapshot {
    /// Creates an empty workspace snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a workspace-relative file path into the snapshot's existing file tree.
    pub fn with_file(mut self, path: impl Into<String>) -> Self {
        if let Ok(normalized) = normalize_workspace_path(&path.into())
            && normalized != "."
        {
            self.existing_files.insert(normalized);
        }
        self
    }

    /// Inserts a workspace-relative file path alongside its UTF-8 text content into the snapshot.
    pub fn with_file_content(
        mut self,
        path: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        if let Ok(normalized) = normalize_workspace_path(&path.into())
            && normalized != "."
        {
            self.existing_files.insert(normalized.clone());
            self.file_contents.insert(normalized, content.into());
        }
        self
    }

    /// Populates UTF-8 `file_contents` on disk for the specified workspace-relative `paths`
    /// when not already cached, skipping missing, directory, or non-UTF-8 binary files.
    pub fn populate_file_contents_from_disk(&mut self, workspace_root: &Path, paths: &[String]) {
        for raw_path in paths {
            let Ok(normalized) = normalize_workspace_path(raw_path) else {
                continue;
            };
            if normalized == "." || self.file_contents.contains_key(&normalized) {
                continue;
            }
            let full_path = workspace_root.join(&normalized);
            if full_path.is_file()
                && let Ok(content) = fs::read_to_string(&full_path)
            {
                self.existing_files.insert(normalized.clone());
                self.file_contents.insert(normalized, content);
            }
        }
    }

    /// Inserts a pre-parsed [`Canon`] keyed by its workspace-relative path or source reference.
    pub fn with_canon(mut self, source_ref: impl Into<String>, canon: Canon) -> Self {
        let key = source_ref.into();
        self.existing_files.insert(key.clone());
        self.canons.insert(key, canon);
        self
    }

    /// Parses raw Canon Markdown and inserts it into the snapshot under `options.source_ref`.
    pub fn with_canon_markdown(mut self, raw_markdown: &str, options: &ParseCanonOptions) -> Self {
        let key = options
            .source_ref
            .clone()
            .unwrap_or_else(|| ".canons/unnamed.md".to_string());
        let canon = parse_canon_or_fallback(raw_markdown, options);
        self.existing_files.insert(key.clone());
        self.canons.insert(key, canon);
        self
    }

    /// Materializes a [`WorkspaceSnapshot`] by traversing `workspace_root` on disk.
    pub fn from_disk(
        workspace_root: &Path,
        canon_patterns: &[String],
    ) -> Result<Self, DiscoverError> {
        let default_glob = GlobPattern::parse(DEFAULT_CANON_GLOB)?;
        let explicit_matchers = compile_canon_filter_globs(canon_patterns)?;

        let mut existing_files = BTreeSet::new();
        walk_workspace_files(workspace_root, workspace_root, &mut existing_files)?;

        let mut canons = BTreeMap::new();
        for rel_path in &existing_files {
            if !rel_path.ends_with(".md") {
                continue;
            }
            let matches_canon_domain = if explicit_matchers.is_empty() {
                default_glob.is_match(rel_path)
            } else {
                explicit_matchers.iter().any(|m| m.is_match(rel_path))
            };

            if matches_canon_domain {
                let full_path = workspace_root.join(rel_path);
                let raw_content =
                    fs::read_to_string(&full_path).map_err(|err| DiscoverError::Io {
                        path: rel_path.clone(),
                        message: err.to_string(),
                    })?;
                let opts = ParseCanonOptions::with_source_ref(rel_path.clone());
                let canon = parse_canon_or_fallback(&raw_content, &opts);
                canons.insert(rel_path.clone(), canon);
            }
        }

        Ok(Self {
            existing_files,
            canons,
            file_contents: BTreeMap::new(),
        })
    }
}

/// Compiles `--canon <path|glob>` patterns into [`GlobPattern`]s, expanding directory prefixes.
pub(crate) fn compile_canon_filter_globs(
    canon_patterns: &[String],
) -> Result<Vec<GlobPattern>, DiscoverError> {
    let mut compiled = Vec::new();
    for raw in canon_patterns {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = normalize_workspace_path(trimmed)
            .map_err(|_| DiscoverError::PathEscapesWorkspace { path: raw.clone() })?;
        compiled.push(GlobPattern::parse(&normalized)?);
        if !normalized.ends_with(".md") && !normalized.contains(['*', '?', '[', '{']) {
            let dir_glob = if normalized == "." {
                DEFAULT_CANON_GLOB.to_string()
            } else {
                format!("{normalized}/**/*.md")
            };
            compiled.push(GlobPattern::parse(&dir_glob)?);
        }
    }
    Ok(compiled)
}

fn walk_workspace_files(
    root: &Path,
    current_dir: &Path,
    out: &mut BTreeSet<String>,
) -> Result<(), DiscoverError> {
    let entries = match fs::read_dir(current_dir) {
        Ok(iter) => iter,
        Err(err) => {
            if current_dir == root {
                return Err(DiscoverError::Io {
                    path: current_dir.display().to_string(),
                    message: err.to_string(),
                });
            }
            return Ok(());
        }
    };

    let mut sorted_entries: Vec<_> = entries.filter_map(Result::ok).collect();
    sorted_entries.sort_by_key(|e| e.file_name());

    for entry in sorted_entries {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel_posix = rel.to_string_lossy().replace('\\', "/");
        if rel_posix.is_empty() || is_ignored_path(&rel_posix) {
            continue;
        }

        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            walk_workspace_files(root, &path, out)?;
        } else if metadata.is_file() {
            out.insert(rel_posix);
        }
    }

    Ok(())
}

/// Parses a Canon file or constructs a fallback [`Canon`] when YAML frontmatter is malformed,
/// allowing `discover` to forward broken canons to downstream `validate` linting.
pub fn parse_canon_or_fallback(raw_content: &str, options: &ParseCanonOptions) -> Canon {
    if let Ok(canon) = parse_canon(raw_content, options) {
        return canon;
    }

    let source_info = options.source_ref.as_deref().and_then(resolve_source_info);
    let scope = source_info.as_ref().and_then(|s| s.scope.clone());
    let id = derive_id(None, source_info.as_ref());
    let title = id_to_title_case(&id);
    let triggers = derive_triggers(None, scope.as_ref())
        .unwrap_or_else(|_| vec![DEFAULT_GLOBAL_TRIGGER_PATTERN.to_string()]);
    let inspect = DEFAULT_INSPECT_PLANES.to_vec();

    Canon {
        id,
        title: title.clone(),
        triggers,
        exists: Vec::new(),
        inspect,
        tags: Vec::new(),
        references: Vec::new(),
        invariant: title,
        invariant_span: None,
        exceptions: Vec::new(),
        rationale: None,
        remediation: None,
        source_ref: source_info.as_ref().map(|s| s.source_ref.clone()),
        source_kind: source_info.as_ref().map(|s| s.source_kind.clone()),
        file_path: source_info.as_ref().and_then(|s| s.file_path.clone()),
        diagnostic_path: source_info
            .as_ref()
            .map(|s| s.diagnostic_path.clone())
            .filter(|p| !p.is_empty()),
        scope: scope.map(|s| s.0),
        raw_content: raw_content.to_string(),
        raw_body: raw_content.to_string(),
        raw_frontmatter: None,
        frontmatter_span: None,
        frontmatter_key_spans: Vec::new(),
        directive_spans: Vec::new(),
    }
}
