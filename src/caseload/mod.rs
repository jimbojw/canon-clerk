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

//! Core data plane models for the cumulative `Caseload` state container (`.intake`, `.discovery`, `.validation`, `.config`, `.probe`, `.apprisal`, `.docket`, `.evidence`).

mod apprisal;
mod config;
mod discovery;
mod docket;
mod evidence;
mod intake;
mod probe;
mod validation;

pub use apprisal::{
    ApprisalStatus, CanonApprisalAssessment, CaseloadApprisal, normalize_unit_score,
};
pub use config::{CaseloadConfig, DEFAULT_AUDITOR_MODEL, DEFAULT_PROVIDER, DEFAULT_SCREENER_MODEL};
pub use discovery::{ActiveExhibits, CaseloadDiscovery, DiscoveryMode};
pub use docket::{
    CanonDocketAssessment, CaseloadDocket, ColorabilityStatus, normalize_colorability_score,
};
pub use evidence::{
    AdmissibilityStatus, CaseloadEvidence, ExhibitAssessment, normalize_admissibility_score,
};
pub use intake::{
    CaseloadIntake, ContentOmissionReason, FileArtifact, FileChangeStatus, IntakeScope,
    LinkedIssueContext, PatchOmissionReason,
};
pub use probe::{CaseloadProbe, ProbeEndpointResult, ProbeStatus};
pub use validation::{
    AnnotationLevel, CaseloadValidation, CodeAnnotation, ValidatedCanonMetadata, ValidationOutcome,
};

use serde::{Deserialize, Serialize};

/// Current canonical schema version for serialized `Caseload` envelopes.
pub const CASELOAD_SCHEMA_VERSION: &str = "1.0";

fn default_caseload_version() -> String {
    CASELOAD_SCHEMA_VERSION.to_string()
}

/// The central, cumulative state container flowing through the Canon Clerk Caseload DAG.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Caseload {
    /// Schema specification version (always `"1.0"`).
    #[serde(default = "default_caseload_version")]
    pub version: String,
    /// Intake: Tendered change diffs, target path directives, design intent, and PR metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intake: Option<CaseloadIntake>,
    /// Discovery: Matched target files, candidate canons, active exhibits, and trigger intersections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<CaseloadDiscovery>,
    /// Validation: Candidate canon AST and frontmatter validation results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<CaseloadValidation>,
    /// Configuration: Resolved provider credentials, model specifiers, and runtime options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<CaseloadConfig>,
    /// Probe: Live provider connectivity and endpoint latency diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<CaseloadProbe>,
    /// Apprisal: Prospective statutory applicability assessments against design intent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apprisal: Option<CaseloadApprisal>,
    /// Docket: Macro triage colorability assessments and active docket cases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docket: Option<CaseloadDocket>,
    /// Evidence: Micro triage admitted and excluded exhibits per active case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CaseloadEvidence>,
    /// Verdict: Final compliance decrees and line annotations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<serde_json::Value>,
}

impl Default for Caseload {
    fn default() -> Self {
        Self::new()
    }
}

impl Caseload {
    /// Constructs a new empty `Caseload` envelope stamped with schema version `"1.0"`.
    pub fn new() -> Self {
        Self {
            version: CASELOAD_SCHEMA_VERSION.to_string(),
            intake: None,
            discovery: None,
            validation: None,
            config: None,
            probe: None,
            apprisal: None,
            docket: None,
            evidence: None,
            verdict: None,
        }
    }

    /// Returns a new `Caseload` with the `.intake` namespace populated, preserving all other stage fields.
    pub fn with_intake(mut self, intake: CaseloadIntake) -> Self {
        self.intake = Some(intake);
        self
    }

    /// Returns a new `Caseload` with the `.discovery` namespace populated, preserving all other stage fields.
    pub fn with_discovery(mut self, discovery: CaseloadDiscovery) -> Self {
        self.discovery = Some(discovery);
        self
    }

    /// Returns a new `Caseload` with the `.validation` namespace populated, preserving all other stage fields.
    pub fn with_validation(mut self, validation: CaseloadValidation) -> Self {
        self.validation = Some(validation);
        self
    }

    /// Returns a new `Caseload` with the `.config` namespace populated, preserving all other stage fields.
    pub fn with_config(mut self, config: CaseloadConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Returns a new `Caseload` with the `.probe` namespace populated, preserving all other stage fields.
    pub fn with_probe(mut self, probe: CaseloadProbe) -> Self {
        self.probe = Some(probe);
        self
    }

    /// Returns a new `Caseload` with the `.apprisal` namespace populated, preserving all other stage fields.
    pub fn with_apprisal(mut self, apprisal: CaseloadApprisal) -> Self {
        self.apprisal = Some(apprisal);
        self
    }

    /// Returns a new `Caseload` with the `.docket` namespace populated, preserving all other stage fields.
    pub fn with_docket(mut self, docket: CaseloadDocket) -> Self {
        self.docket = Some(docket);
        self
    }

    /// Returns a new `Caseload` with the `.evidence` namespace populated, preserving all other stage fields.
    pub fn with_evidence(mut self, evidence: CaseloadEvidence) -> Self {
        self.evidence = Some(evidence);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod serialization {
        use super::*;

        #[test]
        fn test_empty_caseload_round_trip() {
            let caseload = Caseload::default();
            let json = serde_json::to_string(&caseload).expect("serialize empty caseload");
            assert_eq!(json, r#"{"version":"1.0"}"#);

            let decoded: Caseload =
                serde_json::from_str(&json).expect("deserialize empty caseload");
            assert_eq!(decoded, caseload);
        }
    }

    mod envelope_immutability {
        use super::*;

        #[test]
        fn test_with_intake_preserves_existing_stage_namespaces() {
            let raw_upstream = r#"{
                "version": "1.0",
                "config": {
                    "screener_model": "google:gemini-flash-lite-latest"
                }
            }"#;
            let upstream: Caseload =
                serde_json::from_str(raw_upstream).expect("parse upstream caseload");
            assert!(upstream.config.is_some());
            assert!(upstream.intake.is_none());

            let enriched = upstream.clone().with_intake(CaseloadIntake::default());
            assert!(enriched.intake.is_some());
            assert_eq!(enriched.config, upstream.config);

            let with_disc = enriched
                .clone()
                .with_discovery(CaseloadDiscovery::default());
            assert!(with_disc.discovery.is_some());
            assert_eq!(with_disc.intake, enriched.intake);
            assert_eq!(with_disc.config, enriched.config);

            let with_val = with_disc
                .clone()
                .with_validation(CaseloadValidation::default());
            assert!(with_val.validation.is_some());
            assert_eq!(with_val.discovery, with_disc.discovery);
            assert_eq!(with_val.intake, with_disc.intake);
            assert_eq!(with_val.config, with_disc.config);
        }

        #[test]
        fn test_with_config_and_probe_preserve_existing_namespaces() {
            let branch_a = Caseload::new()
                .with_intake(CaseloadIntake::default())
                .with_discovery(CaseloadDiscovery::default())
                .with_validation(CaseloadValidation::default());

            let with_cfg = branch_a.clone().with_config(CaseloadConfig::default());
            assert_eq!(with_cfg.config, Some(CaseloadConfig::default()));
            assert_eq!(with_cfg.validation, branch_a.validation);
            assert_eq!(with_cfg.discovery, branch_a.discovery);
            assert_eq!(with_cfg.intake, branch_a.intake);

            let healthy_probe = CaseloadProbe {
                screener: ProbeEndpointResult {
                    status: ProbeStatus::Ok,
                    latency_ms: 85,
                    model: DEFAULT_SCREENER_MODEL.to_string(),
                    error: None,
                },
                auditor: ProbeEndpointResult {
                    status: ProbeStatus::Ok,
                    latency_ms: 210,
                    model: DEFAULT_AUDITOR_MODEL.to_string(),
                    error: None,
                },
            };
            assert!(healthy_probe.is_healthy());
            let with_prb = with_cfg.clone().with_probe(healthy_probe.clone());
            assert_eq!(with_prb.probe, Some(healthy_probe));
            assert_eq!(with_prb.config, with_cfg.config);
            assert_eq!(with_prb.validation, with_cfg.validation);
            assert_eq!(with_prb.discovery, with_cfg.discovery);
            assert_eq!(with_prb.intake, with_cfg.intake);
        }

        #[test]
        fn test_with_apprisal_preserves_existing_namespaces() {
            let upstream = Caseload::new()
                .with_intake(CaseloadIntake::default())
                .with_discovery(CaseloadDiscovery::default())
                .with_validation(CaseloadValidation::default())
                .with_config(CaseloadConfig::default());

            let with_app = upstream.clone().with_apprisal(CaseloadApprisal::default());
            assert_eq!(with_app.apprisal, Some(CaseloadApprisal::default()));
            assert_eq!(with_app.config, upstream.config);
            assert_eq!(with_app.validation, upstream.validation);
            assert_eq!(with_app.discovery, upstream.discovery);
            assert_eq!(with_app.intake, upstream.intake);
        }

        #[test]
        fn test_with_docket_preserves_existing_namespaces() {
            let upstream = Caseload::new()
                .with_intake(CaseloadIntake::default())
                .with_discovery(CaseloadDiscovery::default())
                .with_validation(CaseloadValidation::default())
                .with_config(CaseloadConfig::default());

            let with_doc = upstream.clone().with_docket(CaseloadDocket::default());
            assert_eq!(with_doc.docket, Some(CaseloadDocket::default()));
            assert_eq!(with_doc.config, upstream.config);
            assert_eq!(with_doc.validation, upstream.validation);
            assert_eq!(with_doc.discovery, upstream.discovery);
            assert_eq!(with_doc.intake, upstream.intake);

            let with_ev = with_doc.clone().with_evidence(CaseloadEvidence::default());
            assert_eq!(with_ev.evidence, Some(CaseloadEvidence::default()));
            assert_eq!(with_ev.docket, with_doc.docket);
            assert_eq!(with_ev.config, with_doc.config);
            assert_eq!(with_ev.validation, with_doc.validation);
            assert_eq!(with_ev.discovery, with_doc.discovery);
            assert_eq!(with_ev.intake, with_doc.intake);
        }
    }
}
