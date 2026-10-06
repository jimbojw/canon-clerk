use super::artifact::FileArtifact;
use super::canon::{CanonAst, CanonVerdict};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseloadSummary {
    pub total_artifacts: usize,
    pub total_canons: usize,
    pub active_canons: usize,
    pub total_verdicts: usize,
    pub passing_verdicts: usize,
    pub failing_verdicts: usize,
    pub is_passing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caseload {
    pub id: String,
    pub artifacts: Vec<FileArtifact>,
    pub canons: Vec<CanonAst>,
    pub active_canon_ids: Vec<String>,
    pub verdicts: Vec<CanonVerdict>,
}

impl Caseload {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            artifacts: Vec::new(),
            canons: Vec::new(),
            active_canon_ids: Vec::new(),
            verdicts: Vec::new(),
        }
    }

    pub fn add_artifact(&mut self, artifact: FileArtifact) {
        self.artifacts.push(artifact);
    }

    pub fn add_canon(&mut self, canon: CanonAst) {
        self.canons.push(canon);
    }

    pub fn activate_canon(&mut self, canon_id: impl Into<String>) {
        let id = canon_id.into();
        if !self.active_canon_ids.contains(&id) {
            self.active_canon_ids.push(id);
        }
    }

    pub fn record_verdict(&mut self, verdict: CanonVerdict) {
        self.verdicts.push(verdict);
    }

    pub fn is_passing(&self) -> bool {
        self.verdicts.iter().all(|v| v.admitted)
    }

    pub fn summary(&self) -> CaseloadSummary {
        let passing = self.verdicts.iter().filter(|v| v.admitted).count();
        let failing = self.verdicts.len() - passing;
        CaseloadSummary {
            total_artifacts: self.artifacts.len(),
            total_canons: self.canons.len(),
            active_canons: self.active_canon_ids.len(),
            total_verdicts: self.verdicts.len(),
            passing_verdicts: passing,
            failing_verdicts: failing,
            is_passing: self.is_passing(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::artifact::ChangeType;

    #[test]
    fn test_caseload_lifecycle() {
        let mut caseload = Caseload::new("cl-test-01");
        assert_eq!(caseload.id, "cl-test-01");
        assert!(caseload.is_passing());

        caseload.add_artifact(FileArtifact::new("src/main.rs", ChangeType::Modified));
        caseload.add_canon(CanonAst::new(".canons/rule1.md", "# Rule"));
        caseload.activate_canon("rule1");

        assert_eq!(caseload.artifacts.len(), 1);
        assert_eq!(caseload.canons.len(), 1);
        assert_eq!(caseload.active_canon_ids, vec!["rule1"]);

        caseload.record_verdict(CanonVerdict::admit("rule1", 10));
        assert!(caseload.is_passing());

        let summary = caseload.summary();
        assert_eq!(summary.total_artifacts, 1);
        assert_eq!(summary.active_canons, 1);
        assert_eq!(summary.passing_verdicts, 1);
        assert_eq!(summary.failing_verdicts, 0);
        assert!(summary.is_passing);
    }
}

