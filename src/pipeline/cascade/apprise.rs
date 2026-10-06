use crate::models::{ApprisalAssessment, CanonAst, CaseloadApprisal};
use crate::pipeline::cascade::prompts::CascadePrompts;
use crate::pipeline::probe::{AnyProviderClient, ProbeError};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct AppriseResponse {
    #[serde(default)]
    assessments: HashMap<String, AppriseItem>,
}

#[derive(Debug, Deserialize)]
struct AppriseItem {
    #[serde(rename = "apprisalSummary")]
    apprisal_summary: String,
    #[serde(rename = "apprisalScore")]
    apprisal_score: f32,
}

pub struct AppriseRunner;

impl AppriseRunner {
    pub async fn execute_apprise(
        candidate_canons: &[&CanonAst],
        intent: &str,
        threshold: f32,
        client: &AnyProviderClient,
    ) -> Result<CaseloadApprisal, ProbeError> {
        if candidate_canons.is_empty() {
            return Ok(CaseloadApprisal::new());
        }

        let mut cumulative_apprisal = CaseloadApprisal::new();

        // Screen in chunks of up to 15 canons to keep within model token bounds
        for chunk in candidate_canons.chunks(15) {
            let prompt = CascadePrompts::build_apprise_prompt(chunk, intent);
            let raw_json = client
                .generate_structured_json(CascadePrompts::APPRISE_SYSTEM_INSTRUCTION, &prompt)
                .await?;

            let parsed: AppriseResponse = serde_json::from_str(&raw_json)
                .map_err(|e| ProbeError::Network(format!("Failed to parse apprise JSON: {}", e)))?;

            for (path, item) in parsed.assessments {
                cumulative_apprisal.insert(
                    path,
                    ApprisalAssessment::with_threshold(
                        item.apprisal_summary,
                        item.apprisal_score,
                        threshold,
                    ),
                );
            }
        }

        Ok(cumulative_apprisal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::probe::MockProviderClient;

    #[tokio::test]
    async fn test_execute_apprise_empty_canons() {
        let client = AnyProviderClient::Mock(MockProviderClient::new_healthy("mock", "model"));
        let result = AppriseRunner::execute_apprise(&[], "some intent", 0.5, &client)
            .await
            .unwrap();

        assert_eq!(result.assessments.len(), 0);
        assert_eq!(result.applicable_count(), 0);
    }

    #[tokio::test]
    async fn test_execute_apprise_mock() {
        let client = AnyProviderClient::Mock(MockProviderClient::new_healthy("mock", "model"));
        let canon = CanonAst::new(".canons/cli/cli-arguments.md", "# Positional args required");

        let result = AppriseRunner::execute_apprise(&[&canon], "Add new CLI subcommand", 0.5, &client)
            .await
            .unwrap();

        assert_eq!(result.assessments.len(), 1);
        let assessment = result.assessments.get(".canons/cli/cli-arguments.md").unwrap();
        assert!(assessment.is_applicable());
        assert_eq!(assessment.status, "applicable");
        assert_eq!(assessment.apprisal_score, 0.88);
        assert!(assessment.apprisal_summary.contains("Prospective intent touches"));
    }
}
