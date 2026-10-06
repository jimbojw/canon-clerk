use crate::models::{CanonAdjudication, CanonAst, CodeAnnotation, FileArtifact};
use crate::pipeline::cascade::prompts::CascadePrompts;
use crate::pipeline::probe::{AnyProviderClient, ProbeError};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct AuditResponse {
    #[serde(rename = "complianceScore")]
    compliance_score: f32,
    #[serde(rename = "complianceSummary")]
    compliance_summary: String,
    #[serde(default)]
    annotations: Vec<AuditAnnotationItem>,
}

#[derive(Debug, Deserialize)]
struct AuditAnnotationItem {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(default)]
    line: Option<usize>,
    message: String,
    #[serde(default = "default_severity")]
    severity: String,
}

fn default_severity() -> String {
    "error".to_string()
}

pub struct AuditRunner;

impl AuditRunner {
    pub async fn execute_audit(
        canon: &CanonAst,
        admitted_exhibits: &[&FileArtifact],
        client: &AnyProviderClient,
    ) -> Result<CanonAdjudication, ProbeError> {
        let prompt = CascadePrompts::build_audit_prompt(canon, admitted_exhibits);
        let raw_json = client
            .generate_structured_json(CascadePrompts::AUDIT_SYSTEM_INSTRUCTION, &prompt)
            .await?;

        let parsed: AuditResponse = serde_json::from_str(&raw_json)
            .map_err(|e| ProbeError::Network(format!("Failed to parse audit JSON: {}", e)))?;

        let annotations = parsed
            .annotations
            .into_iter()
            .map(|a| CodeAnnotation {
                file_path: a.file_path,
                line: a.line,
                message: a.message,
                severity: a.severity,
            })
            .collect();

        Ok(CanonAdjudication::new(
            canon.path.clone(),
            parsed.compliance_score,
            parsed.compliance_summary,
            annotations,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ChangeType;
    use crate::pipeline::probe::MockProviderClient;

    #[tokio::test]
    async fn test_execute_audit_mock() {
        let client = AnyProviderClient::Mock(MockProviderClient::new_healthy("mock", "model"));
        let canon = CanonAst::new(".canons/cli/rule.md", "# Statute");
        let exhibit = FileArtifact::new("src/main.rs", ChangeType::Modified);

        let adj = AuditRunner::execute_audit(&canon, &[&exhibit], &client)
            .await
            .unwrap();

        assert!(adj.is_passing());
        assert_eq!(adj.compliance_score, 0.95);
        assert_eq!(adj.status, "pass");
        assert!(adj.compliance_summary.contains("correctly adhere"));
    }
}
