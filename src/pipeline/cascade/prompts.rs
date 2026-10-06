use crate::models::{CanonAst, FileArtifact};

pub struct CascadePrompts;

impl CascadePrompts {
    pub const DOCKET_SYSTEM_INSTRUCTION: &'static str = r#"You are Canon Clerk's Phase 2 Docket triage screener.
Your role is to perform Macro Triage: evaluate candidate canons against the change context and file diffs to determine whether each canon has threshold subject-matter jurisdiction to be heard.

CRITICAL DIRECTIVE: JURISDICTION, NOT COMPLIANCE
- You are evaluating subject-matter applicability and relevance, NOT whether the changes pass or fail the canon.
- Answer: "Given the totality of this change, its diffs, and this candidate canon's invariant, does this canon have a colorable claim to be heard?"
- Provide a single-sentence justification (colorabilitySummary) articulating subject-matter jurisdiction.
- Assign a normalized colorability score in [0.0, 1.0] (colorabilityScore), where >= 0.5 indicates active docket jurisdiction.
- If the change touches files or areas governed by the canon's domain, assign >= 0.5 even if the change appears compliant.
- If the changes are completely outside the canon's domain, assign < 0.5.
- Output JSON in this exact structure:
{
  "assessments": [
    {
      "canonPath": "<canon-path>",
      "colorabilitySummary": "<one sentence justification>",
      "colorabilityScore": <0.0 to 1.0>
    }
  ]
}"#;

    pub fn build_docket_prompt(
        canons: &[&CanonAst],
        diff_summary: &str,
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str("## CHANGE DIFF CONTEXT\n\n```diff\n");
        prompt.push_str(diff_summary);
        prompt.push_str("\n```\n\n## CANDIDATE CANONS TO EVALUATE\n\n");

        for canon in canons {
            prompt.push_str(&format!("### Canon: `{}`\n", canon.path));
            if let Some(fm) = &canon.frontmatter {
                if let Some(title) = &fm.title {
                    prompt.push_str(&format!("Title: {}\n", title));
                }
            }
            prompt.push_str("Excerpt:\n");
            let lines: Vec<&str> = canon.raw_markdown.lines().take(6).collect();
            prompt.push_str(&lines.join("\n"));
            prompt.push_str("\n\n---\n\n");
        }

        prompt.push_str("Evaluate every candidate canon listed above and output the structured JSON object.");
        prompt
    }

    pub const ADMIT_SYSTEM_INSTRUCTION: &'static str = r#"You are Canon Clerk's Phase 2 Exhibit admissibility screener.
Your role is to perform Micro Triage: evaluate modified files against an active docket canon to determine which files are relevant evidence (admitted exhibits).

DIRECTIVE: RELEVANCE OF EVIDENCE
- Determine whether each file is relevant to inspecting compliance with the governing canon.
- Assign an admissibility score in [0.0, 1.0] (admissibilityScore), where >= 0.5 admits the file into evidence.
- Provide a single-sentence rationale (admissibilitySummary).
- Output JSON in this exact structure:
{
  "exhibits": [
    {
      "filePath": "<file-path>",
      "admissibilitySummary": "<rationale>",
      "admissibilityScore": <0.0 to 1.0>
    }
  ]
}"#;

    pub fn build_admit_prompt(
        canon: &CanonAst,
        artifacts: &[&FileArtifact],
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str(&format!("## GOVERNING CANON: `{}`\n\n", canon.path));
        prompt.push_str(&canon.raw_markdown);
        prompt.push_str("\n\n## CANDIDATE FILE EXHIBITS\n\n");

        for art in artifacts {
            prompt.push_str(&format!("### File: `{}` (Change: {:?})\n", art.path, art.change_type));
            if let Some(diff) = &art.diff {
                prompt.push_str("```diff\n");
                let lines: Vec<&str> = diff.lines().take(20).collect();
                prompt.push_str(&lines.join("\n"));
                prompt.push_str("\n```\n");
            }
            prompt.push_str("\n---\n\n");
        }

        prompt.push_str("Evaluate admissibility for every file listed above and output the structured JSON object.");
        prompt
    }

    pub const AUDIT_SYSTEM_INSTRUCTION: &'static str = r#"You are Canon Clerk's Phase 3 Chief Justice Auditor.
Your role is to perform Single-Trial Adjudication: evaluate admitted exhibits against the governing canon statute to render a binding compliance verdict.

CRITICAL DIRECTIVES:
- Determine whether the change complies with the canon's rules and invariants.
- Assign a compliance score in [0.0, 1.0] (complianceScore):
  - >= 0.5: Compliant (PASS)
  - < 0.5: Violation detected (FAIL)
- Provide a substantive decree summary (complianceSummary).
- If violations exist, provide line-level code annotations pointing to specific violations.
- Output JSON in this exact structure:
{
  "complianceScore": <0.0 to 1.0>,
  "complianceSummary": "<substantive decree explaining compliance or violation>",
  "annotations": [
    {
      "filePath": "<file-path>",
      "line": <optional line number>,
      "message": "<violation explanation>",
      "severity": "error"
    }
  ]
}"#;

    pub fn build_audit_prompt(
        canon: &CanonAst,
        admitted_exhibits: &[&FileArtifact],
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str(&format!("## STATUTE: CANON `{}`\n\n", canon.path));
        prompt.push_str(&canon.raw_markdown);
        prompt.push_str("\n\n## ADMITTED EVIDENCE EXHIBITS\n\n");

        for art in admitted_exhibits {
            prompt.push_str(&format!("### Exhibit: `{}`\n", art.path));
            if let Some(diff) = &art.diff {
                prompt.push_str("Diff:\n```diff\n");
                prompt.push_str(diff);
                prompt.push_str("\n```\n");
            }
            if let Some(content) = &art.content {
                prompt.push_str("Content:\n```\n");
                prompt.push_str(content);
                prompt.push_str("\n```\n");
            }
            prompt.push_str("\n---\n\n");
        }

        prompt.push_str("Render your adjudication decree and output the structured JSON object.");
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ChangeType, FileArtifact};

    #[test]
    fn test_docket_prompt_generation() {
        let canon = CanonAst::new(".canons/cli-rule.md", "# CLI Rule\nMust use positional.");
        let prompt = CascadePrompts::build_docket_prompt(&[&canon], "diff --git a/main.rs");
        assert!(prompt.contains(".canons/cli-rule.md"));
        assert!(prompt.contains("Must use positional."));
        assert!(prompt.contains("diff --git a/main.rs"));
    }

    #[test]
    fn test_admit_prompt_generation() {
        let canon = CanonAst::new(".canons/rule.md", "# Rule");
        let art = FileArtifact::new("src/main.rs", ChangeType::Modified).with_diff("@@ -1 +1 @@");
        let prompt = CascadePrompts::build_admit_prompt(&canon, &[&art]);
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("@@ -1 +1 @@"));
    }

    #[test]
    fn test_audit_prompt_generation() {
        let canon = CanonAst::new(".canons/rule.md", "# Invariant Rule");
        let art = FileArtifact::new("src/lib.rs", ChangeType::Added).with_content("pub fn test() {}");
        let prompt = CascadePrompts::build_audit_prompt(&canon, &[&art]);
        assert!(prompt.contains("Invariant Rule"));
        assert!(prompt.contains("pub fn test() {}"));
    }
}
