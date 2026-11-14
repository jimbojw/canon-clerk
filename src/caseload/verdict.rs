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

//! Data plane models for the `.verdict` namespace (`docs/architecture/nodes/audit.md` §3).

use super::validation::CodeAnnotation;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::Ordering;

/// Deterministically derived per-case and overall gate compliance status (`docs/architecture/nodes/audit.md` §3).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum VerdictStatus {
    /// Evaluated evidence complies with canon statute (or matched a declared `Exception`), or zero trials were required (`"pass"`).
    #[default]
    Pass,
    /// Unexcused statute violation detected (`compliance_score < threshold`, `"fail"`).
    Fail,
}

impl VerdictStatus {
    /// Returns the canonical wire representation (`"pass"` or `"fail"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }

    /// Derives [`VerdictStatus`] deterministically from `score` against `threshold`.
    pub fn from_score(score: f64, threshold: f64) -> Self {
        if score >= threshold {
            Self::Pass
        } else {
            Self::Fail
        }
    }
}

/// Validates that a compliance score or threshold is finite and within `[0.0, 1.0]`, normalizing `-0.0` to `0.0`.
pub fn normalize_compliance_score(score: f64) -> Result<f64, String> {
    if !score.is_finite() {
        return Err("compliance_score must be a finite number in [0.0, 1.0]".to_string());
    }
    if !(0.0..=1.0).contains(&score) {
        return Err(format!(
            "compliance_score must be within [0.0, 1.0], got {score}"
        ));
    }
    Ok(if score == 0.0 { 0.0 } else { score })
}

fn serialize_compliance_score<S>(score: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let normalized = normalize_compliance_score(*score).map_err(serde::ser::Error::custom)?;
    serializer.serialize_f64(normalized)
}

fn deserialize_compliance_score<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = f64::deserialize(deserializer)?;
    normalize_compliance_score(raw).map_err(serde::de::Error::custom)
}

fn compare_annotations(a: &CodeAnnotation, b: &CodeAnnotation) -> Ordering {
    a.path
        .cmp(&b.path)
        .then_with(|| a.start_line.cmp(&b.start_line))
        .then_with(|| a.end_line.cmp(&b.end_line))
        .then_with(|| a.start_column.cmp(&b.start_column))
        .then_with(|| a.end_column.cmp(&b.end_column))
        .then_with(|| a.annotation_level.cmp(&b.annotation_level))
        .then_with(|| a.message.cmp(&b.message))
        .then_with(|| a.title.cmp(&b.title))
}

/// Sorts and deduplicates [`CodeAnnotation`] items in canonical source-coordinate order.
pub fn sort_annotations(list: &mut Vec<CodeAnnotation>) {
    list.sort_by(compare_annotations);
    list.dedup();
}

fn deserialize_sorted_annotations<'de, D>(deserializer: D) -> Result<Vec<CodeAnnotation>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut items = Vec::<CodeAnnotation>::deserialize(deserializer)?;
    sort_annotations(&mut items);
    Ok(items)
}

fn sort_and_dedup_adjudications(list: &mut Vec<CanonAdjudication>) {
    for item in list.iter_mut() {
        sort_annotations(&mut item.annotations);
    }
    list.sort_by(|a, b| a.canon_path.cmp(&b.canon_path));
    list.dedup_by(|a, b| a.canon_path == b.canon_path);
}

fn deserialize_sorted_adjudications<'de, D>(
    deserializer: D,
) -> Result<Vec<CanonAdjudication>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut items = Vec::<CanonAdjudication>::deserialize(deserializer)?;
    sort_and_dedup_adjudications(&mut items);
    Ok(items)
}

fn canonical_score_bits(score: f64) -> u64 {
    if score == 0.0 {
        0.0_f64.to_bits()
    } else {
        score.to_bits()
    }
}

/// Substantive trial adjudication rendered for a single active case during `audit` (`docs/architecture/nodes/audit.md` §3).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CanonAdjudication {
    /// Repository-relative canon path or identifier (`canon_path`).
    pub canon_path: String,
    /// Substantive decree explaining compliance, matched `Exception` criteria, or actionable remediation.
    pub compliance_summary: String,
    /// Continuous compliance score in `[0.0, 1.0]`.
    #[serde(
        serialize_with = "serialize_compliance_score",
        deserialize_with = "deserialize_compliance_score"
    )]
    pub compliance_score: f64,
    /// Deterministically derived case compliance status (`"pass"` or `"fail"`).
    pub status: VerdictStatus,
    /// Stably sorted source line annotations pinpointing violations on admitted file exhibits.
    #[serde(default, deserialize_with = "deserialize_sorted_annotations")]
    pub annotations: Vec<CodeAnnotation>,
}

impl PartialEq for CanonAdjudication {
    fn eq(&self, other: &Self) -> bool {
        self.canon_path == other.canon_path
            && self.compliance_summary == other.compliance_summary
            && canonical_score_bits(self.compliance_score)
                == canonical_score_bits(other.compliance_score)
            && self.status == other.status
            && self.annotations == other.annotations
    }
}

impl Eq for CanonAdjudication {}

fn default_verdict_summary() -> String {
    synthesize_verdict_summary(&[])
}

fn synthesize_verdict_summary(adjudications: &[CanonAdjudication]) -> String {
    if adjudications.is_empty() {
        return "0 active cases adjudicated; review gate passed.".to_string();
    }
    let total = adjudications.len();
    let failed = adjudications
        .iter()
        .filter(|a| a.status == VerdictStatus::Fail)
        .count();
    if failed == 0 {
        format!("{total} active case(s) adjudicated; all compliant.")
    } else {
        let annotations: usize = adjudications.iter().map(|a| a.annotations.len()).sum();
        format!(
            "{failed} of {total} adjudicated case(s) failed compliance ({annotations} annotation(s))."
        )
    }
}

/// Final review gate decree and per-case trial outcomes captured during `audit` (`docs/architecture/nodes/audit.md` §3).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CaseloadVerdict {
    /// Overall review gate outcome (`"pass"` if and only if zero cases have `status == "fail"`).
    #[serde(default)]
    pub status: VerdictStatus,
    /// Human-readable synthesis of trial outcomes.
    #[serde(default = "default_verdict_summary")]
    pub summary: String,
    /// Stably sorted per-case trial decrees ordered lexicographically by `canon_path`.
    #[serde(default, deserialize_with = "deserialize_sorted_adjudications")]
    pub adjudications: Vec<CanonAdjudication>,
}

impl Default for CaseloadVerdict {
    fn default() -> Self {
        Self::from_adjudications(Vec::new())
    }
}

impl CaseloadVerdict {
    /// Constructs a [`CaseloadVerdict`] from per-case trial adjudications, sorting cases by
    /// `canon_path`, sorting each case's `annotations`, and deterministically deriving `status` and `summary`.
    pub fn from_adjudications(mut adjudications: Vec<CanonAdjudication>) -> Self {
        sort_and_dedup_adjudications(&mut adjudications);
        let has_failure = adjudications
            .iter()
            .any(|item| item.status == VerdictStatus::Fail);
        let status = if has_failure {
            VerdictStatus::Fail
        } else {
            VerdictStatus::Pass
        };
        let summary = synthesize_verdict_summary(&adjudications);
        Self {
            status,
            summary,
            adjudications,
        }
    }

    /// Returns `true` when the overall review gate passed (`status == VerdictStatus::Pass`).
    pub const fn is_pass(&self) -> bool {
        matches!(self.status, VerdictStatus::Pass)
    }

    /// Returns the number of adjudicated cases with `status == VerdictStatus::Pass`.
    pub fn passed_count(&self) -> usize {
        self.adjudications
            .iter()
            .filter(|a| a.status == VerdictStatus::Pass)
            .count()
    }

    /// Returns the number of adjudicated cases with `status == VerdictStatus::Fail`.
    pub fn failed_count(&self) -> usize {
        self.adjudications
            .iter()
            .filter(|a| a.status == VerdictStatus::Fail)
            .count()
    }

    /// Returns the total count of [`CodeAnnotation`] items across all adjudicated cases.
    pub fn annotation_count(&self) -> usize {
        self.adjudications.iter().map(|a| a.annotations.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caseload::{AnnotationLevel, Caseload};

    mod serialization_and_determinism {
        use super::*;

        #[test]
        fn test_caseload_verdict_sorts_adjudications_and_annotations_and_aggregates_status() {
            let verdict = CaseloadVerdict::from_adjudications(vec![
                CanonAdjudication {
                    canon_path: ".canons/z-rule.md".to_string(),
                    compliance_summary: "Flag `--fooBar` violates kebab-case.".to_string(),
                    compliance_score: 0.12,
                    status: VerdictStatus::Fail,
                    annotations: vec![
                        CodeAnnotation {
                            path: "src/cli/z.rs".to_string(),
                            start_line: 20,
                            end_line: 20,
                            start_column: None,
                            end_column: None,
                            annotation_level: AnnotationLevel::Failure,
                            message: "Second line violation.".to_string(),
                            title: Some("z-rule".to_string()),
                        },
                        CodeAnnotation {
                            path: "src/cli/a.rs".to_string(),
                            start_line: 5,
                            end_line: 5,
                            start_column: Some(9),
                            end_column: Some(16),
                            annotation_level: AnnotationLevel::Failure,
                            message: "Rename `--fooBar` to `--foo-bar`.".to_string(),
                            title: Some("z-rule".to_string()),
                        },
                    ],
                },
                CanonAdjudication {
                    canon_path: ".canons/a-rule.md".to_string(),
                    compliance_summary: "All error messages include Remediation hints.".to_string(),
                    compliance_score: 0.95,
                    status: VerdictStatus::Pass,
                    annotations: Vec::new(),
                },
            ]);

            assert!(!verdict.is_pass());
            assert_eq!(verdict.status, VerdictStatus::Fail);
            assert_eq!(verdict.passed_count(), 1);
            assert_eq!(verdict.failed_count(), 1);
            assert_eq!(verdict.annotation_count(), 2);
            assert_eq!(verdict.adjudications[0].canon_path, ".canons/a-rule.md");
            assert_eq!(verdict.adjudications[1].canon_path, ".canons/z-rule.md");
            assert_eq!(verdict.adjudications[1].annotations[0].path, "src/cli/a.rs");
            assert_eq!(verdict.adjudications[1].annotations[1].path, "src/cli/z.rs");

            let caseload = Caseload::new().with_verdict(verdict);
            let encoded = serde_json::to_string(&caseload).expect("serialize verdict caseload");
            let decoded: Caseload =
                serde_json::from_str(&encoded).expect("deserialize verdict caseload");
            assert_eq!(decoded, caseload);
            assert_eq!(
                serde_json::to_string(&decoded).expect("re-serialize verdict"),
                encoded
            );
        }

        #[test]
        fn test_empty_and_all_passing_verdicts_report_pass_and_normalize_neg_zero() {
            let empty = CaseloadVerdict::default();
            assert!(empty.is_pass());
            assert_eq!(empty.passed_count(), 0);
            assert_eq!(empty.failed_count(), 0);
            assert!(empty.summary.contains("0 active cases"));

            let neg_zero_fail = CaseloadVerdict::from_adjudications(vec![CanonAdjudication {
                canon_path: ".canons/rule.md".to_string(),
                compliance_summary: "Unexcused violation.".to_string(),
                compliance_score: -0.0,
                status: VerdictStatus::Fail,
                annotations: Vec::new(),
            }]);
            let encoded = serde_json::to_string(&neg_zero_fail).expect("serialize -0.0");
            assert!(!encoded.contains("-0.0"));
            assert!(encoded.contains("\"compliance_score\":0.0"));
        }
    }

    mod score_bounds_and_status {
        use super::*;

        #[test]
        fn test_rejects_non_finite_and_out_of_bounds_scores() {
            assert!(normalize_compliance_score(f64::NAN).is_err());
            assert!(normalize_compliance_score(f64::NEG_INFINITY).is_err());
            assert!(normalize_compliance_score(-0.01).is_err());
            assert!(normalize_compliance_score(1.01).is_err());
            assert_eq!(VerdictStatus::from_score(0.5, 0.5), VerdictStatus::Pass);
            assert_eq!(VerdictStatus::from_score(0.49, 0.5), VerdictStatus::Fail);
            assert_eq!(VerdictStatus::Pass.as_str(), "pass");
            assert_eq!(VerdictStatus::Fail.as_str(), "fail");
        }
    }
}
