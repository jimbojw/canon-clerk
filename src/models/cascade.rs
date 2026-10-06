use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorabilityAssessment {
    pub canon_path: String,
    pub colorability_score: f32,
    pub colorability_summary: String,
    pub status: String,
}

impl ColorabilityAssessment {
    pub fn new(
        canon_path: impl Into<String>,
        colorability_score: f32,
        colorability_summary: impl Into<String>,
    ) -> Self {
        let status = if colorability_score >= 0.5 {
            "docketed".to_string()
        } else {
            "dismissed".to_string()
        };
        Self {
            canon_path: canon_path.into(),
            colorability_score,
            colorability_summary: colorability_summary.into(),
            status,
        }
    }

    pub fn is_docketed(&self) -> bool {
        self.colorability_score >= 0.5
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmittedExhibit {
    pub canon_path: String,
    pub file_path: String,
    pub admissibility_score: f32,
    pub admissibility_summary: String,
    pub status: String,
}

impl AdmittedExhibit {
    pub fn new(
        canon_path: impl Into<String>,
        file_path: impl Into<String>,
        admissibility_score: f32,
        admissibility_summary: impl Into<String>,
    ) -> Self {
        let status = if admissibility_score >= 0.5 {
            "admitted".to_string()
        } else {
            "excluded".to_string()
        };
        Self {
            canon_path: canon_path.into(),
            file_path: file_path.into(),
            admissibility_score,
            admissibility_summary: admissibility_summary.into(),
            status,
        }
    }

    pub fn is_admitted(&self) -> bool {
        self.admissibility_score >= 0.5
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeAnnotation {
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub message: String,
    pub severity: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonAdjudication {
    pub canon_path: String,
    pub compliance_score: f32,
    pub compliance_summary: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<CodeAnnotation>,
}

impl CanonAdjudication {
    pub fn new(
        canon_path: impl Into<String>,
        compliance_score: f32,
        compliance_summary: impl Into<String>,
        annotations: Vec<CodeAnnotation>,
    ) -> Self {
        let status = if compliance_score >= 0.5 {
            "pass".to_string()
        } else {
            "fail".to_string()
        };
        Self {
            canon_path: canon_path.into(),
            compliance_score,
            compliance_summary: compliance_summary.into(),
            status,
            annotations,
        }
    }

    pub fn is_passing(&self) -> bool {
        self.compliance_score >= 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_colorability_threshold() {
        let pass = ColorabilityAssessment::new("cli-rule.md", 0.8, "Touches CLI flags");
        assert!(pass.is_docketed());
        assert_eq!(pass.status, "docketed");

        let fail = ColorabilityAssessment::new("db-rule.md", 0.2, "No DB changes");
        assert!(!fail.is_docketed());
        assert_eq!(fail.status, "dismissed");
    }

    #[test]
    fn test_admissibility_threshold() {
        let exhibit = AdmittedExhibit::new("cli-rule.md", "src/main.rs", 0.9, "Defines flags");
        assert!(exhibit.is_admitted());
        assert_eq!(exhibit.status, "admitted");

        let excluded = AdmittedExhibit::new("cli-rule.md", "docs/readme.md", 0.1, "Documentation only");
        assert!(!excluded.is_admitted());
        assert_eq!(excluded.status, "excluded");
    }

    #[test]
    fn test_compliance_adjudication() {
        let adj = CanonAdjudication::new("cli-rule.md", 0.95, "Fully compliant with guidelines", Vec::new());
        assert!(adj.is_passing());
        assert_eq!(adj.status, "pass");

        let fail_adj = CanonAdjudication::new(
            "cli-rule.md",
            0.3,
            "Missing positional operand",
            vec![CodeAnnotation {
                file_path: "src/main.rs".to_string(),
                line: Some(45),
                message: "Option must be positional".to_string(),
                severity: "error".to_string(),
            }],
        );
        assert!(!fail_adj.is_passing());
        assert_eq!(fail_adj.status, "fail");
        assert_eq!(fail_adj.annotations.len(), 1);
    }
}
