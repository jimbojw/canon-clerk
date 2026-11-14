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

//! Aggregate prompt assembly and reason-first JSON Schema builder for `apprise` (`docs/architecture/nodes/apprise.md` §4).

use crate::canon::Canon;
use crate::caseload::{CaseloadDiscovery, CaseloadIntake};
use crate::discover::WorkspaceSnapshot;
use serde_json::{Value, json};
use std::fmt::Write as _;

pub(super) const APPRISE_SYSTEM_INSTRUCTION: &str = "\
You are the Clerk of the Court for Canon Clerk exercising Statutory Apprisal Authority (Jurisdictio Notificatoria).
Given a prospective architectural design intent, target scope context, and a set of candidate engineering canons, determine whether each candidate canon has a colorable claim of prospective subject-matter jurisdiction over the planned work before implementation begins.

Evaluation Rules:
1. Exact Domain Indirection: For each entry in `assessments`, set `canon_path` to the exact candidate canon identifier string presented under `### Canon:` (and listed under `## Required canon_path Identifiers`) without modification.
2. Reason First: Write a concise, concrete `apprisal_summary` explaining why the canon does or does not govern the planned design intent and target scope BEFORE assigning `apprisal_score`.
3. Positive Polarity Jurisdiction Scoring & Rubric Bands (`[0.0, 1.0]`):
   - Score `0.75 - 0.95` (Applicable, `>= 0.5`) when the planned work touches, introduces, or modifies the domain construct, layer, workflow, or documentation requirement governed by the canon's invariant or exceptions (such as domain entity structs, default option constants, pipeline subroutines, explicit dependency wiring, standard library vs. bespoke utility choices, pure validators, CLI flags/arguments/subcommands/help/streams, downstream adapter vs. upstream contract isolation, PR scope/workflow hygiene, or feature documentation requirements).
   - NEVER Dismiss Because of Anticipated Compliance: Prospective jurisdiction depends on whether the planned work touches the governed construct, NEVER on whether the design already satisfies the rule. If a canon states that an anti-pattern is forbidden in domain entities, defaults, pipeline subroutines, dependency wiring, CLI commands/flags/arguments, or feature refactors, you MUST score `0.75 - 0.95` whenever the planned work touches those constructs—even if the planned design is 100% compliant. Likewise, if a canon requires feature work to update a reference file (e.g., `README.md`), any planned feature work is under its prospective jurisdiction (`0.75 - 0.95`) even when that file is not yet listed in the target scope.
   - Score `0.05 - 0.25` (Dismissed, `< 0.5`) ONLY when the canon's specific domain construct is absent from and orthogonal to the planned design intent (e.g., `fix`-only commit rules on planned feature work; third-party GitHub Action rules when no `.github/` workflows are touched; POSIX `SIGINT`/`SIGTERM` signal-trap rules when no signal handlers are planned; live external service test skip/remediation rules when no live service tests are planned; custom path-ignore exclusion rules when no ignore sets are modified; API key or secret-storage rules when no credentials or auth configs are touched; asynchronous streaming generator rules on synchronous CLI/domain code; or document-grammar lexer/AST/frontmatter parser rules when no document syntax parser is modified).
   - Avoid ambiguous boundary scores in `0.35 - 0.65`: commit decisively to `>= 0.75` when the governed construct is present or `<= 0.25` when absent.
4. Completeness & Uniqueness: Return an `assessments` array containing one entry per candidate canon identifier in the exact order listed—no omissions, no duplicates, and no extra identifiers.";

/// Builds a constant-size domain-indirected, reason-first JSON Schema enforcing
/// `canon_path`, `apprisal_summary`, then `apprisal_score` (`docs/architecture/nodes/apprise.md` §4.3).
pub(super) fn build_apprise_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "assessments": {
                "type": "array",
                "description": "Statutory apprisal assessments, containing one entry per candidate canon identifier in the prompt.",
                "items": single_canon_assessment_schema()
            }
        },
        "propertyOrdering": ["assessments"],
        "required": ["assessments"]
    })
}

fn single_canon_assessment_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "canon_path": {
                "type": "string",
                "description": "Exact candidate canon identifier from the prompt (`### Canon: ...` / `## Required canon_path Identifiers`)."
            },
            "apprisal_summary": {
                "type": "string",
                "description": "Concise rationale explaining why this canon does or does not claim prospective jurisdiction over the planned design intent and target scope."
            },
            "apprisal_score": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0,
                "description": "Continuous prospective applicability score in [0.0, 1.0], where >= 0.5 indicates the canon governs the planned work."
            }
        },
        "propertyOrdering": ["canon_path", "apprisal_summary", "apprisal_score"],
        "required": ["canon_path", "apprisal_summary", "apprisal_score"]
    })
}

/// Assembles the shared prospective design intent and target scope prefix shared identically across all batch prompts.
pub(super) fn build_apprise_intent_prefix(
    intake: &CaseloadIntake,
    discovery: &CaseloadDiscovery,
) -> String {
    let mut out = String::new();
    let intent = intake.intent.as_deref().unwrap_or_default().trim();

    let _ = writeln!(out, "## Prospective Design Intent\n\n{intent}\n");
    append_scope_section(&mut out, intake, discovery);
    out
}

/// Assembles a user prompt for a specific candidate canon batch slice prefixed by the shared `intent_prefix`.
pub(super) fn build_apprise_batch_user_prompt(
    intent_prefix: &str,
    batch_canons: &[String],
    discovery: &CaseloadDiscovery,
    workspace: &WorkspaceSnapshot,
) -> Result<String, String> {
    let mut out = String::with_capacity(intent_prefix.len() + batch_canons.len() * 512);
    out.push_str(intent_prefix);

    let _ = writeln!(out, "## Candidate Canons ({})\n", batch_canons.len());
    for canon_ref in batch_canons {
        let canon = workspace
            .canons
            .get(canon_ref)
            .ok_or_else(|| canon_ref.clone())?;
        let matched = discovery
            .triggers_join
            .get(canon_ref)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        append_canon_section(&mut out, canon_ref, canon, matched);
    }

    let _ = writeln!(
        out,
        "## Required `canon_path` Identifiers ({})",
        batch_canons.len()
    );
    for (idx, canon_ref) in batch_canons.iter().enumerate() {
        let _ = writeln!(out, "{}. `{canon_ref}`", idx + 1);
    }

    Ok(out)
}

fn append_scope_section(out: &mut String, intake: &CaseloadIntake, discovery: &CaseloadDiscovery) {
    let _ = writeln!(out, "## Target & Discovery Scope");
    let _ = writeln!(out, "- Intake Scope: {}", intake.scope.as_str());
    let _ = writeln!(out, "- Discovery Mode: {}", discovery.mode.as_str());
    if !intake.target_paths.is_empty() {
        let _ = writeln!(
            out,
            "- Target Directives: {}",
            intake.target_paths.join(", ")
        );
    }
    if !discovery.target_files.is_empty() {
        let _ = writeln!(
            out,
            "- Materialized Target Files: {}",
            discovery.target_files.join(", ")
        );
    }
    if let Some(title) = intake.pr_title.as_deref().filter(|s| !s.trim().is_empty()) {
        let _ = writeln!(out, "- PR Title: {}", title.trim());
    }
    if let Some(body) = intake.pr_body.as_deref().filter(|s| !s.trim().is_empty()) {
        let _ = writeln!(out, "- PR Description: {}", body.trim());
    }
    out.push('\n');
}

pub(super) fn append_canon_section(
    out: &mut String,
    canon_ref: &str,
    canon: &Canon,
    matched: &[String],
) {
    let _ = writeln!(out, "### Canon: `{canon_ref}`");
    let _ = writeln!(out, "- ID: `{}`", canon.id);
    let _ = writeln!(out, "- Title: {}", canon.title);
    if !canon.triggers.is_empty() {
        let _ = writeln!(out, "- Triggers: {}", canon.triggers.join(", "));
    }
    if !canon.exists.is_empty() {
        let _ = writeln!(out, "- Exists: {}", canon.exists.join(", "));
    }
    if !canon.inspect.is_empty() {
        let inspect_list: Vec<String> = canon
            .inspect
            .iter()
            .map(|p| {
                if p.optional {
                    format!("{}?", p.token.as_str())
                } else {
                    p.token.as_str().to_string()
                }
            })
            .collect();
        let _ = writeln!(out, "- Inspect: {}", inspect_list.join(", "));
    }
    if !canon.references.is_empty() {
        let _ = writeln!(out, "- References: {}", canon.references.join(", "));
    }
    if !canon.tags.is_empty() {
        let _ = writeln!(out, "- Tags: {}", canon.tags.join(", "));
    }
    if !matched.is_empty() {
        let _ = writeln!(out, "- Triggered Exhibits: {}", matched.join(", "));
    }
    let body = canon.raw_body.trim();
    let statute = if body.is_empty() {
        canon.invariant.trim()
    } else {
        body
    };
    let _ = writeln!(out, "\n{statute}");
    let _ = writeln!(out, "- Exact `canon_path`: `{canon_ref}`\n");
}
