use crate::models::{CanonAst, ColorabilityAssessment};
use crate::pipeline::cascade::prompts::CascadePrompts;
use crate::pipeline::probe::{AnyProviderClient, ProbeError};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct DocketResponse {
    #[serde(default)]
    assessments: Vec<DocketAssessmentItem>,
}

#[derive(Debug, Deserialize)]
struct DocketAssessmentItem {
    #[serde(rename = "canonPath")]
    canon_path: String,
    #[serde(rename = "colorabilitySummary")]
    colorability_summary: String,
    #[serde(rename = "colorabilityScore")]
    colorability_score: f32,
}

pub struct DocketRunner;

impl DocketRunner {
    pub async fn execute_docket(
        canons: &[&CanonAst],
        diff_summary: &str,
        client: &AnyProviderClient,
    ) -> Result<Vec<ColorabilityAssessment>, ProbeError> {
        if canons.is_empty() {
            return Ok(Vec::new());
        }

        let prompt = CascadePrompts::build_docket_prompt(canons, diff_summary);
        let raw_json = client
            .generate_structured_json(CascadePrompts::DOCKET_SYSTEM_INSTRUCTION, &prompt)
            .await?;

        let parsed: DocketResponse = serde_json::from_str(&raw_json)
            .map_err(|e| ProbeError::Network(format!("Failed to parse docket JSON: {}", e)))?;

        let assessments = parsed
            .assessments
            .into_iter()
            .map(|item| {
                ColorabilityAssessment::new(
                    item.canon_path,
                    item.colorability_score,
                    item.colorability_summary,
                )
            })
            .collect();

        Ok(assessments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::probe::MockProviderClient;

    #[tokio::test]
    async fn test_execute_docket_mock() {
        let client = AnyProviderClient::Mock(MockProviderClient::new_healthy("mock", "model"));
        let canon = CanonAst::new(".canons/cli/cli-arguments-must-represent-primary-operands.md", "# CLI Rule");
        let assessments = DocketRunner::execute_docket(&[&canon], "diff content", &client)
            .await
            .unwrap();

        assert_eq!(assessments.len(), 1);
        assert!(assessments[0].is_docketed());
        assert_eq!(assessments[0].colorability_score, 0.85);
        assert_eq!(assessments[0].status, "docketed");
    }
}
