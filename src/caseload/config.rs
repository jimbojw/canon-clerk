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

//! Data plane models for the `.config` namespace (`docs/architecture/nodes/configure.md` §3).

use serde::{Deserialize, Serialize};

/// Canonical default provider namespace for MVP model specifiers (`docs/architecture/nodes/configure.md` §3).
pub const DEFAULT_PROVIDER: &str = "google";

/// Canonical provider namespace for TypeSafe AI decision models (`typesafe:jev-*`).
pub const TYPESAFE_PROVIDER: &str = "typesafe";

/// Default canonical screener model specifier (`docs/architecture/nodes/configure.md` §3).
pub const DEFAULT_SCREENER_MODEL: &str = "google:gemini-flash-lite-latest";

/// Default canonical auditor model specifier (`docs/architecture/nodes/configure.md` §3).
pub const DEFAULT_AUDITOR_MODEL: &str = "google:gemini-pro-latest";

fn default_screener_model() -> String {
    DEFAULT_SCREENER_MODEL.to_string()
}

fn default_auditor_model() -> String {
    DEFAULT_AUDITOR_MODEL.to_string()
}

/// Normalized runtime provider configuration captured during the `configure` stage (`docs/architecture/nodes/configure.md` §3).
/// Raw API keys are intentionally omitted to uphold the Credential Non-Persistence Invariant.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CaseloadConfig {
    /// Canonical provider-prefixed model identifier used by fast heuristic screening stages (`apprise`, `docket`, `admit`).
    #[serde(default = "default_screener_model")]
    pub screener_model: String,
    /// Canonical provider-prefixed model identifier used for substantive trial adjudication (`audit`).
    #[serde(default = "default_auditor_model")]
    pub auditor_model: String,
    /// Optional token ceiling governing extended thinking during `audit` trials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_budget: Option<u32>,
}

impl Default for CaseloadConfig {
    fn default() -> Self {
        Self {
            screener_model: DEFAULT_SCREENER_MODEL.to_string(),
            auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
            reasoning_budget: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caseload::Caseload;

    #[test]
    fn test_caseload_config_round_trip_and_optional_budget_omission() {
        let default_cfg = CaseloadConfig::default();
        let json = serde_json::to_string(&default_cfg).expect("serialize default config");
        assert_eq!(
            json,
            r#"{"screener_model":"google:gemini-flash-lite-latest","auditor_model":"google:gemini-pro-latest"}"#
        );
        assert!(!json.contains("reasoning_budget"));

        let with_budget = CaseloadConfig {
            reasoning_budget: Some(4096),
            ..CaseloadConfig::default()
        };
        let caseload = Caseload::new().with_config(with_budget);
        let encoded = serde_json::to_string(&caseload).expect("serialize caseload");
        let decoded: Caseload = serde_json::from_str(&encoded).expect("deserialize caseload");
        assert_eq!(decoded, caseload);
        assert_eq!(
            decoded.config.as_ref().and_then(|c| c.reasoning_budget),
            Some(4096)
        );
    }
}
