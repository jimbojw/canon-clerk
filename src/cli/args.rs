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

//! Composable Clap argument groups for Caseload DAG stage subcommands.

use crate::caseload::Caseload;
use crate::cli::CliError;
use crate::cli::io::{
    read_caseload_operand, read_operand_source, resolve_target_operands, write_caseload_file,
};
use crate::intake::IntakeOptions;
use clap::Args;
use std::io::Read;
use std::path::PathBuf;

/// Composable CLI argument group for filing sources and pull request metadata (Branch A).
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct FilingSourceArgs {
    /// Target file paths, directories, globs, or `-` to read newline-delimited paths from stdin.
    #[arg(
        value_name = "TARGETS",
        help = "Target file paths, directories, globs, or '-' for newline-delimited stdin paths"
    )]
    pub targets: Vec<String>,

    /// Read unified diff patch from a file path or `-` for standard input.
    #[arg(
        long = "diff",
        value_name = "PATH|-",
        help_heading = "Filing Sources",
        help = "Ingest unified diff patch from file or '-' for stdin"
    )]
    pub diff: Option<String>,

    /// Prospective architectural design intent or plan description.
    #[arg(
        long = "intent",
        value_name = "TEXT",
        help_heading = "Filing Sources",
        help = "Ingest prospective architectural design intent or plan description"
    )]
    pub intent: Option<String>,

    /// Ingest all repository files as target artifacts (`scope: "all-targets"`).
    #[arg(
        long = "all-targets",
        help_heading = "Filing Sources",
        help = "Ingest all repository files as target artifacts"
    )]
    pub all_targets: bool,

    /// Pull request title or commit subject line.
    #[arg(
        long = "pr-title",
        value_name = "TEXT",
        help_heading = "Pull Request Metadata",
        help = "Ingest pull request title or commit subject text"
    )]
    pub pr_title: Option<String>,

    /// Pull request Markdown description or commit body.
    #[arg(
        long = "pr-body",
        value_name = "TEXT",
        conflicts_with = "pr_body_file",
        help_heading = "Pull Request Metadata",
        help = "Ingest pull request Markdown description text"
    )]
    pub pr_body: Option<String>,

    /// Read pull request Markdown description from a file path or `-` for stdin.
    #[arg(
        long = "pr-body-file",
        value_name = "PATH",
        conflicts_with = "pr_body",
        help_heading = "Pull Request Metadata",
        help = "Ingest pull request Markdown description from a file"
    )]
    pub pr_body_file: Option<String>,
}

impl FilingSourceArgs {
    /// Resolves flags, file paths, and stdin streams into a domain [`IntakeOptions`] descriptor.
    pub fn resolve_intake_options(&self, stdin: &mut dyn Read) -> Result<IntakeOptions, CliError> {
        let patch_content = match &self.diff {
            Some(source) => Some(read_operand_source(source, "--diff", stdin)?),
            None => None,
        };

        let pr_body = if let Some(body) = &self.pr_body {
            Some(body.clone())
        } else if let Some(path) = &self.pr_body_file {
            Some(read_operand_source(path, "--pr-body-file", stdin)?)
        } else {
            None
        };

        let target_paths = resolve_target_operands(&self.targets, stdin)?;

        Ok(IntakeOptions {
            pr_title: self.pr_title.clone(),
            pr_body,
            intent: self.intent.clone(),
            target_paths,
            patch_content,
            all_targets: self.all_targets,
            linked_issues: None,
        })
    }
}

/// Composable CLI argument group for canon selection and discovery filtering.
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct CanonSelectionArgs {
    /// Discover or validate all canons across the repository corpus, bypassing path trigger matching.
    #[arg(
        long = "all-canons",
        help_heading = "Canon Selection",
        help = "Evaluate all repository canons, bypassing trigger matching"
    )]
    pub all_canons: bool,

    /// Explicitly restrict candidate canons to a specific file path or glob subset.
    #[arg(
        long = "canon",
        value_name = "PATH|GLOB",
        help_heading = "Canon Selection",
        help = "Explicitly restrict candidate canons to a specific subset"
    )]
    pub canon: Vec<String>,
}

impl CanonSelectionArgs {
    /// Returns `true` if canon selection is unconstrained (neither `--all-canons` nor `--canon` provided).
    pub fn is_empty(&self) -> bool {
        !self.all_canons && self.canon.is_empty()
    }
}

/// Composable CLI argument group for model configuration, reasoning budgets, and provider credentials (Branch B).
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfigArgs {
    /// Custom screener model specifier or shorthand alias (`flash-lite`, `flash`, `pro`, `jev`; env: `CANON_CLERK_SCREENER_MODEL`).
    #[arg(
        long = "screener-model",
        value_name = "MODEL",
        help_heading = "Model & Reasoning Options",
        help = "Screener model specifier or alias (fallback env: CANON_CLERK_SCREENER_MODEL)"
    )]
    pub screener_model: Option<String>,

    /// Custom auditor model specifier or shorthand alias (`pro`, `flash`, `flash-lite`; env: `CANON_CLERK_AUDITOR_MODEL`).
    #[arg(
        long = "auditor-model",
        value_name = "MODEL",
        help_heading = "Model & Reasoning Options",
        help = "Auditor model specifier or alias (fallback env: CANON_CLERK_AUDITOR_MODEL)"
    )]
    pub auditor_model: Option<String>,

    /// Maximum reasoning budget in tokens for extended thinking during `audit` trials (env: `CANON_CLERK_REASONING_BUDGET`).
    #[arg(
        long = "reasoning-budget",
        value_name = "TOKENS",
        help_heading = "Model & Reasoning Options",
        help = "Maximum reasoning budget tokens (fallback env: CANON_CLERK_REASONING_BUDGET)"
    )]
    pub reasoning_budget: Option<u32>,

    /// Google Gemini API key override (fallback env: `GEMINI_API_KEY`; never persisted to Caseload JSON).
    #[arg(
        long = "gemini-api-key",
        value_name = "KEY",
        help_heading = "Provider Credentials",
        help = "Google Gemini API key (fallback env: GEMINI_API_KEY)"
    )]
    pub gemini_api_key: Option<String>,

    /// TypeSafe AI API key override for Jev models (fallback env: `TYPESAFE_API_KEY`; never persisted to Caseload JSON).
    #[arg(
        long = "typesafe-api-key",
        value_name = "KEY",
        help_heading = "Provider Credentials",
        help = "TypeSafe AI API key for Jev models (fallback env: TYPESAFE_API_KEY)"
    )]
    pub typesafe_api_key: Option<String>,
}

/// Composable CLI argument group for Caseload envelope I/O across all stage subcommands.
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct CaseloadIoArgs {
    /// Read an existing upstream `Caseload` JSON from a file path or `-` for stdin.
    #[arg(
        long = "caseload",
        value_name = "PATH|-",
        help_heading = "Caseload & Output",
        help = "Ingest existing upstream Caseload JSON from file or '-' for stdin"
    )]
    pub caseload: Option<String>,

    /// Write the cumulative `Caseload` JSON to a file path independently of stdout.
    #[arg(
        long = "output-caseload",
        value_name = "PATH",
        help_heading = "Caseload & Output",
        help = "Write cumulative Caseload JSON to disk independently of stdout"
    )]
    pub output_caseload: Option<PathBuf>,

    /// Emit the enriched `Caseload` as JSON to standard output.
    #[arg(
        long = "json",
        help_heading = "Caseload & Output",
        help = "Emit enriched Caseload JSON to stdout"
    )]
    pub json: bool,
}

impl CaseloadIoArgs {
    /// Reads incoming Caseload JSON from `--caseload` if specified.
    pub fn read_caseload(&self, stdin: &mut dyn Read) -> Result<Option<Caseload>, CliError> {
        read_caseload_operand(self.caseload.as_deref(), stdin)
    }

    /// Writes the cumulative Caseload to `--output-caseload` if specified.
    pub fn write_output_caseload(&self, caseload: &Caseload) -> Result<(), CliError> {
        if let Some(path) = &self.output_caseload {
            write_caseload_file(path, caseload)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_canon_selection_args_is_empty() {
        let default_args = CanonSelectionArgs::default();
        assert!(default_args.is_empty());

        let all_canons = CanonSelectionArgs {
            all_canons: true,
            canon: Vec::new(),
        };
        assert!(!all_canons.is_empty());

        let specific_canons = CanonSelectionArgs {
            all_canons: false,
            canon: vec!["foo.md".to_string()],
        };
        assert!(!specific_canons.is_empty());
    }

    #[test]
    fn test_filing_source_args_resolve_intake_options() {
        let args = FilingSourceArgs {
            targets: vec!["src/lib.rs".to_string()],
            pr_title: Some("feat: test".to_string()),
            pr_body: Some("body".to_string()),
            intent: Some("intent".to_string()),
            all_targets: true,
            ..FilingSourceArgs::default()
        };
        let mut stdin = Cursor::new(b"");
        let opts = args
            .resolve_intake_options(&mut stdin)
            .expect("resolve intake options");
        assert_eq!(opts.pr_title.as_deref(), Some("feat: test"));
        assert_eq!(opts.pr_body.as_deref(), Some("body"));
        assert_eq!(opts.intent.as_deref(), Some("intent"));
        assert!(opts.all_targets);
        assert_eq!(opts.target_paths, Some(vec!["src/lib.rs".to_string()]));
    }
}
