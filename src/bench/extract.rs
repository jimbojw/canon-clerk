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

//! Stage-polymorphic assessment extraction across `.apprisal`, `.docket`, `.evidence`, and `.verdict`.

use super::{BenchmarkStage, ConsistencyBenchError};
use crate::caseload::{
    AdmissibilityStatus, ApprisalStatus, Caseload, ColorabilityStatus, VerdictStatus,
    normalize_unit_score,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Composite subject identifier supporting both 1D per-canon stages (`apprise`, `docket`, `audit`)
/// and 2D `(canon_path, file_path)` exhibit assessments (`admit`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubjectKey {
    /// Repository-relative canon path or identifier.
    pub canon_path: String,
    /// Optional exhibit path or plane identifier for micro evidentiary stages (`admit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhibit_path: Option<String>,
}

impl SubjectKey {
    /// Constructs a 1D canon-scoped subject key (`exhibit_path: None`).
    pub fn canon(canon_path: impl Into<String>) -> Self {
        Self {
            canon_path: canon_path.into(),
            exhibit_path: None,
        }
    }

    /// Constructs a 2D `(canon_path, exhibit_path)` subject key for `admit` exhibit assessments.
    pub fn exhibit(canon_path: impl Into<String>, exhibit_path: impl Into<String>) -> Self {
        Self {
            canon_path: canon_path.into(),
            exhibit_path: Some(exhibit_path.into()),
        }
    }

    /// Formats a human-readable identifier (`"<canon_path>"` or `"<canon_path> :: <exhibit_path>"`).
    pub fn display_id(&self) -> String {
        match &self.exhibit_path {
            Some(exhibit) => format!("{} :: {exhibit}", self.canon_path),
            None => self.canon_path.clone(),
        }
    }
}

impl fmt::Display for SubjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_id())
    }
}

/// Normalized single-trial assessment extracted from a completed `Caseload` stage namespace.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ExtractedAssessment {
    /// Stage-specific status label (`"applicable"`, `"docketed"`, `"admitted"`, `"pass"`, etc.).
    pub status: String,
    /// Whether the subject crossed the affirmative stage gate (`score >= threshold`).
    pub is_positive: bool,
    /// Normalized continuous heuristic score in `[0.0, 1.0]`.
    pub score: f64,
    /// Model rationale summary justifying the assessment.
    pub summary: String,
}

/// Extracts deterministic per-subject assessments from `caseload` for `stage`.
pub fn extract_stage_assessments(
    stage: BenchmarkStage,
    caseload: &Caseload,
    threshold: f64,
) -> Result<BTreeMap<SubjectKey, ExtractedAssessment>, ConsistencyBenchError> {
    let _normalized_threshold = normalize_unit_score(threshold)
        .map_err(|detail| ConsistencyBenchError::InvalidThreshold { detail })?;

    match stage {
        BenchmarkStage::Apprise => extract_apprisal(caseload),
        BenchmarkStage::Docket => extract_docket(caseload),
        BenchmarkStage::Admit => extract_admit(caseload),
        BenchmarkStage::Audit => extract_audit(caseload),
    }
}

fn extract_apprisal(
    caseload: &Caseload,
) -> Result<BTreeMap<SubjectKey, ExtractedAssessment>, ConsistencyBenchError> {
    let apprisal =
        caseload
            .apprisal
            .as_ref()
            .ok_or(ConsistencyBenchError::MissingStagePayload {
                stage: BenchmarkStage::Apprise,
                namespace: ".apprisal",
            })?;

    let mut out = BTreeMap::new();
    for (canon_path, item) in &apprisal.assessments {
        out.insert(
            SubjectKey::canon(canon_path.clone()),
            ExtractedAssessment {
                status: item.apprisal_status.as_str().to_string(),
                is_positive: item.apprisal_status == ApprisalStatus::Applicable,
                score: item.apprisal_score,
                summary: item.apprisal_summary.clone(),
            },
        );
    }
    Ok(out)
}

fn extract_docket(
    caseload: &Caseload,
) -> Result<BTreeMap<SubjectKey, ExtractedAssessment>, ConsistencyBenchError> {
    let docket = caseload
        .docket
        .as_ref()
        .ok_or(ConsistencyBenchError::MissingStagePayload {
            stage: BenchmarkStage::Docket,
            namespace: ".docket",
        })?;

    let mut out = BTreeMap::new();
    for (canon_path, item) in &docket.cases {
        out.insert(
            SubjectKey::canon(canon_path.clone()),
            ExtractedAssessment {
                status: item.colorability_status.as_str().to_string(),
                is_positive: item.colorability_status == ColorabilityStatus::Docketed,
                score: item.colorability_score,
                summary: item.colorability_summary.clone(),
            },
        );
    }
    Ok(out)
}

fn extract_admit(
    caseload: &Caseload,
) -> Result<BTreeMap<SubjectKey, ExtractedAssessment>, ConsistencyBenchError> {
    let evidence =
        caseload
            .evidence
            .as_ref()
            .ok_or(ConsistencyBenchError::MissingStagePayload {
                stage: BenchmarkStage::Admit,
                namespace: ".evidence",
            })?;

    let mut out = BTreeMap::new();
    for (canon_path, entries) in &evidence.exhibits {
        for entry in entries {
            let score = normalize_unit_score(entry.admissibility_score).map_err(|detail| {
                ConsistencyBenchError::MalformedStagePayload {
                    stage: BenchmarkStage::Admit,
                    detail,
                }
            })?;
            out.insert(
                SubjectKey::exhibit(canon_path.clone(), entry.file_path.clone()),
                ExtractedAssessment {
                    status: entry.admissibility_status.as_str().to_string(),
                    is_positive: entry.admissibility_status == AdmissibilityStatus::Admitted,
                    score,
                    summary: entry.admissibility_summary.clone(),
                },
            );
        }
    }
    Ok(out)
}

fn extract_audit(
    caseload: &Caseload,
) -> Result<BTreeMap<SubjectKey, ExtractedAssessment>, ConsistencyBenchError> {
    let verdict = caseload
        .verdict
        .as_ref()
        .ok_or(ConsistencyBenchError::MissingStagePayload {
            stage: BenchmarkStage::Audit,
            namespace: ".verdict",
        })?;

    let mut out = BTreeMap::new();
    for entry in &verdict.adjudications {
        let score = normalize_unit_score(entry.compliance_score).map_err(|detail| {
            ConsistencyBenchError::MalformedStagePayload {
                stage: BenchmarkStage::Audit,
                detail,
            }
        })?;
        out.insert(
            SubjectKey::canon(entry.canon_path.clone()),
            ExtractedAssessment {
                status: entry.status.as_str().to_string(),
                is_positive: entry.status == VerdictStatus::Pass,
                score,
                summary: entry.compliance_summary.clone(),
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprise::DEFAULT_APPRISE_THRESHOLD;
    use crate::caseload::{
        CanonAdjudication, CanonApprisalAssessment, CanonDocketAssessment, CaseloadApprisal,
        CaseloadDocket, CaseloadEvidence, CaseloadVerdict, ExhibitAssessment,
    };

    mod apprise_and_docket_extraction {
        use super::*;

        #[test]
        fn test_extracts_apprisal_and_docket_canon_subjects() {
            let mut apprisal_map = BTreeMap::new();
            apprisal_map.insert(
                ".canons/cli/rule.md".to_string(),
                CanonApprisalAssessment {
                    apprisal_summary: "Governs CLI flags.".to_string(),
                    apprisal_score: 0.9,
                    apprisal_status: ApprisalStatus::Applicable,
                },
            );
            let apprise_caseload = Caseload::new().with_apprisal(CaseloadApprisal {
                assessments: apprisal_map,
            });
            let extracted_apprise = extract_stage_assessments(
                BenchmarkStage::Apprise,
                &apprise_caseload,
                DEFAULT_APPRISE_THRESHOLD,
            )
            .expect("extract apprisal");
            let item = extracted_apprise
                .get(&SubjectKey::canon(".canons/cli/rule.md"))
                .expect("canon key present");
            assert!(item.is_positive);
            assert_eq!(item.status, "applicable");
            assert_eq!(item.score, 0.9);

            let mut docket_map = BTreeMap::new();
            docket_map.insert(
                ".canons/core/rule.md".to_string(),
                CanonDocketAssessment {
                    colorability_summary: "Orthogonal to change.".to_string(),
                    colorability_score: 0.1,
                    colorability_status: ColorabilityStatus::Dismissed,
                },
            );
            let docket_caseload =
                Caseload::new().with_docket(CaseloadDocket::from_cases(docket_map));
            let extracted_docket = extract_stage_assessments(
                BenchmarkStage::Docket,
                &docket_caseload,
                DEFAULT_APPRISE_THRESHOLD,
            )
            .expect("extract docket");
            let docket_item = extracted_docket
                .get(&SubjectKey::canon(".canons/core/rule.md"))
                .expect("docket key present");
            assert!(!docket_item.is_positive);
            assert_eq!(docket_item.status, "dismissed");
        }
    }

    mod admit_and_audit_extraction {
        use super::*;

        #[test]
        fn test_extracts_admit_composite_canon_exhibit_keys_and_audit_verdicts() {
            let mut admit_map = BTreeMap::new();
            admit_map.insert(
                ".canons/cli/flags.md".to_string(),
                vec![ExhibitAssessment {
                    file_path: "src/cli/args.rs".to_string(),
                    admissibility_summary: "Defines CLI flag group.".to_string(),
                    admissibility_score: 0.92,
                    admissibility_status: AdmissibilityStatus::Admitted,
                }],
            );
            let admit_caseload =
                Caseload::new().with_evidence(CaseloadEvidence::from_exhibits(admit_map));

            let admit_items = extract_stage_assessments(
                BenchmarkStage::Admit,
                &admit_caseload,
                DEFAULT_APPRISE_THRESHOLD,
            )
            .expect("extract admit");
            let key = SubjectKey::exhibit(".canons/cli/flags.md", "src/cli/args.rs");
            assert_eq!(key.display_id(), ".canons/cli/flags.md :: src/cli/args.rs");
            let exhibit_item = admit_items.get(&key).expect("composite key present");
            assert!(exhibit_item.is_positive);
            assert_eq!(exhibit_item.status, "admitted");
            assert_eq!(exhibit_item.score, 0.92);

            let audit_caseload =
                Caseload::new().with_verdict(CaseloadVerdict::from_adjudications(vec![
                    CanonAdjudication {
                        canon_path: ".canons/cli/flags.md".to_string(),
                        compliance_summary: "Missing env fallback.".to_string(),
                        compliance_score: 0.2,
                        status: VerdictStatus::Fail,
                        annotations: Vec::new(),
                    },
                ]));

            let audit_items = extract_stage_assessments(
                BenchmarkStage::Audit,
                &audit_caseload,
                DEFAULT_APPRISE_THRESHOLD,
            )
            .expect("extract audit");
            let verdict_item = audit_items
                .get(&SubjectKey::canon(".canons/cli/flags.md"))
                .expect("audit canon present");
            assert!(!verdict_item.is_positive);
            assert_eq!(verdict_item.status, "fail");
            assert_eq!(verdict_item.score, 0.2);
        }
    }

    mod error_cases {
        use super::*;

        #[test]
        fn test_missing_and_malformed_stage_payloads_report_errors() {
            let empty = Caseload::new();
            let err = extract_stage_assessments(
                BenchmarkStage::Apprise,
                &empty,
                DEFAULT_APPRISE_THRESHOLD,
            )
            .expect_err("missing .apprisal must fail");
            assert!(err.to_string().contains(".apprisal"));

            let mut bad_map = BTreeMap::new();
            bad_map.insert(
                ".canons/a.md".to_string(),
                vec![ExhibitAssessment {
                    file_path: "src/lib.rs".to_string(),
                    admissibility_summary: "Out of range".to_string(),
                    admissibility_score: 1.5,
                    admissibility_status: AdmissibilityStatus::Admitted,
                }],
            );
            let bad_admit = Caseload::new().with_evidence(CaseloadEvidence::from_exhibits(bad_map));
            let score_err = extract_stage_assessments(
                BenchmarkStage::Admit,
                &bad_admit,
                DEFAULT_APPRISE_THRESHOLD,
            )
            .expect_err("out-of-bounds score must fail");
            assert!(score_err.to_string().contains("[0.0, 1.0]"));
        }
    }
}
