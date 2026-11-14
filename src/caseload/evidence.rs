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

//! Data plane models for the `.evidence` namespace (`docs/architecture/nodes/admit.md` §3).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

/// Deterministically derived micro evidentiary admissibility status (`docs/architecture/nodes/admit.md` §3).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdmissibilityStatus {
    /// Candidate exhibit is admitted into evidence for the active case (`admissibility_score >= threshold`).
    Admitted,
    /// Candidate exhibit is excluded from downstream `audit` trial prompts (`admissibility_score < threshold`).
    Excluded,
}

impl AdmissibilityStatus {
    /// Returns the canonical wire representation (`"admitted"` or `"excluded"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Excluded => "excluded",
        }
    }

    /// Derives [`AdmissibilityStatus`] deterministically from `score` against `threshold`.
    pub fn from_score(score: f64, threshold: f64) -> Self {
        if score >= threshold {
            Self::Admitted
        } else {
            Self::Excluded
        }
    }
}

/// Validates that an admissibility score or threshold is finite and within `[0.0, 1.0]`, normalizing `-0.0` to `0.0`.
pub fn normalize_admissibility_score(score: f64) -> Result<f64, String> {
    if !score.is_finite() {
        return Err("admissibility_score must be a finite number in [0.0, 1.0]".to_string());
    }
    if !(0.0..=1.0).contains(&score) {
        return Err(format!(
            "admissibility_score must be within [0.0, 1.0], got {score}"
        ));
    }
    Ok(if score == 0.0 { 0.0 } else { score })
}

fn serialize_admissibility_score<S>(score: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let normalized = normalize_admissibility_score(*score).map_err(serde::ser::Error::custom)?;
    serializer.serialize_f64(normalized)
}

fn deserialize_admissibility_score<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = f64::deserialize(deserializer)?;
    normalize_admissibility_score(raw).map_err(serde::de::Error::custom)
}

fn deserialize_sorted_active_cases<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut items = Vec::<String>::deserialize(deserializer)?;
    items.sort();
    items.dedup();
    Ok(items)
}

fn deserialize_sorted_exhibits_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, Vec<ExhibitAssessment>>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut raw_map = BTreeMap::<String, Vec<ExhibitAssessment>>::deserialize(deserializer)?;
    for list in raw_map.values_mut() {
        sort_and_dedup_exhibits(list);
    }
    Ok(raw_map)
}

fn sort_and_dedup_exhibits(list: &mut Vec<ExhibitAssessment>) {
    list.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    list.dedup_by(|a, b| a.file_path == b.file_path);
}

fn canonical_score_bits(score: f64) -> u64 {
    if score == 0.0 {
        0.0_f64.to_bits()
    } else {
        score.to_bits()
    }
}

/// Micro evidentiary relevance assessment for a single candidate exhibit under an active case (`docs/architecture/nodes/admit.md` §3).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExhibitAssessment {
    /// Repository-relative POSIX file path or `inspect:` metadata plane identifier (`"pr_title"`, `"pr_body"`, `"linked_issues"`).
    pub file_path: String,
    /// Rationale explaining why the candidate exhibit is or is not relevant evidence for the governing canon.
    pub admissibility_summary: String,
    /// Continuous evidentiary relevance score in `[0.0, 1.0]`.
    #[serde(
        serialize_with = "serialize_admissibility_score",
        deserialize_with = "deserialize_admissibility_score"
    )]
    pub admissibility_score: f64,
    /// Deterministically derived admissibility status (`"admitted"` or `"excluded"`).
    pub admissibility_status: AdmissibilityStatus,
}

impl PartialEq for ExhibitAssessment {
    fn eq(&self, other: &Self) -> bool {
        self.file_path == other.file_path
            && self.admissibility_summary == other.admissibility_summary
            && canonical_score_bits(self.admissibility_score)
                == canonical_score_bits(other.admissibility_score)
            && self.admissibility_status == other.admissibility_status
    }
}

impl Eq for ExhibitAssessment {}

/// Micro evidentiary relevance assessments and trial-ready active cases captured during `admit` (`docs/architecture/nodes/admit.md` §3).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct CaseloadEvidence {
    /// Deterministic per-case exhibit assessments keyed by active canon identifier (`canon_path`),
    /// with each case's exhibit list stably sorted by `file_path`.
    #[serde(default, deserialize_with = "deserialize_sorted_exhibits_map")]
    pub exhibits: BTreeMap<String, Vec<ExhibitAssessment>>,
    /// Stably sorted lexicographical list of canon identifiers retaining at least one `"admitted"` exhibit.
    #[serde(default, deserialize_with = "deserialize_sorted_active_cases")]
    pub active_cases: Vec<String>,
    /// Optional diagnostic manifest recording non-fatal screening anomalies encountered during micro triage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anomalies: Vec<String>,
}

impl CaseloadEvidence {
    /// Constructs a [`CaseloadEvidence`] from per-case exhibit assessments, sorting each case's
    /// exhibits lexicographically by `file_path` and deriving `active_cases` in deterministic order.
    pub fn from_exhibits(mut exhibits: BTreeMap<String, Vec<ExhibitAssessment>>) -> Self {
        for list in exhibits.values_mut() {
            sort_and_dedup_exhibits(list);
        }
        let active_cases = exhibits
            .iter()
            .filter(|(_, list)| {
                list.iter()
                    .any(|item| item.admissibility_status == AdmissibilityStatus::Admitted)
            })
            .map(|(canon_ref, _)| canon_ref.clone())
            .collect();
        Self {
            exhibits,
            active_cases,
            anomalies: Vec::new(),
        }
    }

    /// Returns strictly the [`AdmissibilityStatus::Admitted`] exhibits for `canon_path` in
    /// deterministic lexicographical order, structurally preventing excluded exhibits from
    /// leaking into downstream `audit` trial prompts.
    pub fn admitted_exhibits_for(&self, canon_path: &str) -> Vec<&ExhibitAssessment> {
        self.exhibits
            .get(canon_path)
            .map(|list| {
                list.iter()
                    .filter(|item| item.admissibility_status == AdmissibilityStatus::Admitted)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns the total count of exhibit assessments marked [`AdmissibilityStatus::Admitted`] across all cases.
    pub fn admitted_exhibit_count(&self) -> usize {
        self.exhibits
            .values()
            .flat_map(|list| list.iter())
            .filter(|item| item.admissibility_status == AdmissibilityStatus::Admitted)
            .count()
    }

    /// Returns the total count of exhibit assessments marked [`AdmissibilityStatus::Excluded`] across all cases.
    pub fn excluded_exhibit_count(&self) -> usize {
        self.exhibits
            .values()
            .flat_map(|list| list.iter())
            .filter(|item| item.admissibility_status == AdmissibilityStatus::Excluded)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caseload::Caseload;

    mod serialization_and_determinism {
        use super::*;

        #[test]
        fn test_caseload_evidence_round_trip_sorted_exhibits_and_active_cases() {
            let mut exhibits = BTreeMap::new();
            exhibits.insert(
                ".canons/z-rule.md".to_string(),
                vec![
                    ExhibitAssessment {
                        file_path: "src/z.rs".to_string(),
                        admissibility_summary: "Modifies z module.".to_string(),
                        admissibility_score: 0.91,
                        admissibility_status: AdmissibilityStatus::Admitted,
                    },
                    ExhibitAssessment {
                        file_path: "pr_title".to_string(),
                        admissibility_summary: "Generic PR title.".to_string(),
                        admissibility_score: 0.12,
                        admissibility_status: AdmissibilityStatus::Excluded,
                    },
                ],
            );
            exhibits.insert(
                ".canons/m-dismissed.md".to_string(),
                vec![ExhibitAssessment {
                    file_path: "src/other.rs".to_string(),
                    admissibility_summary: "Unrelated helper file.".to_string(),
                    admissibility_score: 0.08,
                    admissibility_status: AdmissibilityStatus::Excluded,
                }],
            );
            exhibits.insert(
                ".canons/a-rule.md".to_string(),
                vec![ExhibitAssessment {
                    file_path: "pr_body".to_string(),
                    admissibility_summary: "Contains required Closes #291 reference.".to_string(),
                    admissibility_score: 0.96,
                    admissibility_status: AdmissibilityStatus::Admitted,
                }],
            );

            let evidence = CaseloadEvidence::from_exhibits(exhibits);
            assert_eq!(evidence.admitted_exhibit_count(), 2);
            assert_eq!(evidence.excluded_exhibit_count(), 2);
            assert_eq!(
                evidence.active_cases,
                vec![
                    ".canons/a-rule.md".to_string(),
                    ".canons/z-rule.md".to_string()
                ]
            );
            assert_eq!(
                evidence.exhibits[".canons/z-rule.md"][0].file_path,
                "pr_title"
            );
            assert_eq!(
                evidence.exhibits[".canons/z-rule.md"][1].file_path,
                "src/z.rs"
            );

            let caseload = Caseload::new().with_evidence(evidence);
            let encoded = serde_json::to_string(&caseload).expect("serialize evidence caseload");
            assert!(
                !encoded.contains("\"anomalies\""),
                "empty anomalies vec must be omitted on wire"
            );

            let decoded: Caseload =
                serde_json::from_str(&encoded).expect("deserialize evidence caseload");
            assert_eq!(decoded, caseload);

            let reencoded = serde_json::to_string(&decoded).expect("re-serialize caseload");
            assert_eq!(encoded, reencoded);
        }

        #[test]
        fn test_unsorted_wire_payload_normalizes_and_preserves_anomalies() {
            let raw_json = r#"{
                "exhibits": {
                    ".canons/rule.md": [
                        {
                            "file_path": "src/z.rs",
                            "admissibility_summary": "Z file",
                            "admissibility_score": 0.8,
                            "admissibility_status": "admitted"
                        },
                        {
                            "file_path": "pr_title",
                            "admissibility_summary": "Title plane",
                            "admissibility_score": 0.2,
                            "admissibility_status": "excluded"
                        }
                    ]
                },
                "active_cases": [".canons/z.md", ".canons/a.md", ".canons/z.md"],
                "anomalies": ["Truncated oversized file snapshot"]
            }"#;
            let decoded: CaseloadEvidence =
                serde_json::from_str(raw_json).expect("deserialize evidence with anomalies");
            assert_eq!(decoded.exhibits[".canons/rule.md"][0].file_path, "pr_title");
            assert_eq!(decoded.exhibits[".canons/rule.md"][1].file_path, "src/z.rs");
            assert_eq!(
                decoded.active_cases,
                vec![".canons/a.md".to_string(), ".canons/z.md".to_string()]
            );
            assert_eq!(
                decoded.anomalies,
                vec!["Truncated oversized file snapshot".to_string()]
            );
        }
    }

    mod admitted_isolation_and_score_validation {
        use super::*;

        #[test]
        fn test_admitted_exhibits_for_excludes_below_threshold_items_and_normalizes_neg_zero() {
            let mut exhibits = BTreeMap::new();
            exhibits.insert(
                ".canons/cli/flags.md".to_string(),
                vec![
                    ExhibitAssessment {
                        file_path: "pr_title".to_string(),
                        admissibility_summary: "Irrelevant title.".to_string(),
                        admissibility_score: -0.0,
                        admissibility_status: AdmissibilityStatus::Excluded,
                    },
                    ExhibitAssessment {
                        file_path: "src/cli/admit/mod.rs".to_string(),
                        admissibility_summary: "Defines CLI flags.".to_string(),
                        admissibility_score: 0.94,
                        admissibility_status: AdmissibilityStatus::Admitted,
                    },
                ],
            );
            let evidence = CaseloadEvidence::from_exhibits(exhibits);
            let admitted = evidence.admitted_exhibits_for(".canons/cli/flags.md");
            assert_eq!(admitted.len(), 1);
            assert_eq!(admitted[0].file_path, "src/cli/admit/mod.rs");
            assert!(
                evidence
                    .admitted_exhibits_for(".canons/missing.md")
                    .is_empty()
            );

            let encoded = serde_json::to_string(&evidence).expect("serialize -0.0");
            assert!(!encoded.contains("-0.0"));
            assert!(encoded.contains("\"admissibility_score\":0.0"));
        }

        #[test]
        fn test_rejects_non_finite_and_out_of_bounds_scores() {
            assert!(normalize_admissibility_score(f64::NAN).is_err());
            assert!(normalize_admissibility_score(f64::INFINITY).is_err());
            assert!(normalize_admissibility_score(-0.01).is_err());
            assert!(normalize_admissibility_score(1.01).is_err());
            assert_eq!(
                AdmissibilityStatus::from_score(0.5, 0.5),
                AdmissibilityStatus::Admitted
            );
            assert_eq!(
                AdmissibilityStatus::from_score(0.49, 0.5),
                AdmissibilityStatus::Excluded
            );
            assert_eq!(AdmissibilityStatus::Admitted.as_str(), "admitted");
            assert_eq!(AdmissibilityStatus::Excluded.as_str(), "excluded");
        }
    }
}
