use crate::models::{AdmittedExhibit, CanonAst, FileArtifact};
use crate::pipeline::cascade::prompts::CascadePrompts;
use crate::pipeline::probe::{AnyProviderClient, ProbeError};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct AdmitResponse {
    #[serde(default)]
    exhibits: Vec<AdmitExhibitItem>,
}

#[derive(Debug, Deserialize)]
struct AdmitExhibitItem {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(rename = "admissibilitySummary")]
    admissibility_summary: String,
    #[serde(rename = "admissibilityScore")]
    admissibility_score: f32,
}

pub struct AdmitRunner;

impl AdmitRunner {
    pub async fn execute_admit(
        canon: &CanonAst,
        artifacts: &[&FileArtifact],
        client: &AnyProviderClient,
    ) -> Result<Vec<AdmittedExhibit>, ProbeError> {
        if artifacts.is_empty() {
            return Ok(Vec::new());
        }

        let prompt = CascadePrompts::build_admit_prompt(canon, artifacts);
        let raw_json = client
            .generate_structured_json(CascadePrompts::ADMIT_SYSTEM_INSTRUCTION, &prompt)
            .await?;

        let parsed: AdmitResponse = serde_json::from_str(&raw_json)
            .map_err(|e| ProbeError::Network(format!("Failed to parse admit JSON: {}", e)))?;

        let exhibits = parsed
            .exhibits
            .into_iter()
            .map(|item| {
                AdmittedExhibit::new(
                    canon.path.clone(),
                    item.file_path,
                    item.admissibility_score,
                    item.admissibility_summary,
                )
            })
            .collect();

        Ok(exhibits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ChangeType;
    use crate::pipeline::probe::MockProviderClient;

    #[tokio::test]
    async fn test_execute_admit_mock() {
        let client = AnyProviderClient::Mock(MockProviderClient::new_healthy("mock", "model"));
        let canon = CanonAst::new(".canons/cli/rule.md", "# Rule");
        let art = FileArtifact::new("src/main.rs", ChangeType::Modified);

        let exhibits = AdmitRunner::execute_admit(&canon, &[&art], &client)
            .await
            .unwrap();

        assert_eq!(exhibits.len(), 1);
        assert!(exhibits[0].is_admitted());
        assert_eq!(exhibits[0].admissibility_score, 0.9);
        assert_eq!(exhibits[0].status, "admitted");
    }
}
