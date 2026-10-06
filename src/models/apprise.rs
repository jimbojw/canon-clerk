use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprisalAssessment {
    #[serde(rename = "apprisalSummary")]
    pub apprisal_summary: String,
    #[serde(rename = "apprisalScore")]
    pub apprisal_score: f32,
    pub status: String,
}

impl ApprisalAssessment {
    pub fn new(apprisal_summary: impl Into<String>, apprisal_score: f32) -> Self {
        let status = if apprisal_score >= 0.5 {
            "applicable".to_string()
        } else {
            "dismissed".to_string()
        };
        Self {
            apprisal_summary: apprisal_summary.into(),
            apprisal_score,
            status,
        }
    }

    pub fn with_threshold(
        apprisal_summary: impl Into<String>,
        apprisal_score: f32,
        threshold: f32,
    ) -> Self {
        let status = if apprisal_score >= threshold {
            "applicable".to_string()
        } else {
            "dismissed".to_string()
        };
        Self {
            apprisal_summary: apprisal_summary.into(),
            apprisal_score,
            status,
        }
    }

    pub fn is_applicable(&self) -> bool {
        self.status == "applicable"
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CaseloadApprisal {
    pub assessments: HashMap<String, ApprisalAssessment>,
}

impl CaseloadApprisal {
    pub fn new() -> Self {
        Self {
            assessments: HashMap::new(),
        }
    }

    pub fn insert(&mut self, canon_path: impl Into<String>, assessment: ApprisalAssessment) {
        self.assessments.insert(canon_path.into(), assessment);
    }

    pub fn applicable_count(&self) -> usize {
        self.assessments.values().filter(|a| a.is_applicable()).count()
    }

    pub fn dismissed_count(&self) -> usize {
        self.assessments.len() - self.applicable_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apprisal_assessment_thresholds() {
        let applicable = ApprisalAssessment::new("Applies to CLI flags", 0.85);
        assert!(applicable.is_applicable());
        assert_eq!(applicable.status, "applicable");
        assert_eq!(applicable.apprisal_score, 0.85);

        let dismissed = ApprisalAssessment::new("No database interaction", 0.15);
        assert!(!dismissed.is_applicable());
        assert_eq!(dismissed.status, "dismissed");

        let custom_gate = ApprisalAssessment::with_threshold("Borderline rule", 0.6, 0.7);
        assert!(!custom_gate.is_applicable());
        assert_eq!(custom_gate.status, "dismissed");
    }

    #[test]
    fn test_caseload_apprisal_aggregation() {
        let mut apprisal = CaseloadApprisal::new();
        assert_eq!(apprisal.applicable_count(), 0);

        apprisal.insert("canon-a.md", ApprisalAssessment::new("Governs CLI", 0.9));
        apprisal.insert("canon-b.md", ApprisalAssessment::new("Governs DB", 0.1));

        assert_eq!(apprisal.assessments.len(), 2);
        assert_eq!(apprisal.applicable_count(), 1);
        assert_eq!(apprisal.dismissed_count(), 1);
    }
}
