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
use std::fmt;
use std::str::FromStr;

/// Canonical default provider namespace for MVP model specifiers (`docs/architecture/nodes/configure.md` §3).
pub const DEFAULT_PROVIDER: &str = "google";

/// Canonical provider namespace for TypeSafe AI decision models (`typesafe:jev-*`).
pub const TYPESAFE_PROVIDER: &str = "typesafe";

/// Canonical provider namespace for localhost discriminative decision daemons (`local:<model>`).
pub const LOCAL_PROVIDER: &str = "local";

/// Default canonical screener model specifier (`docs/architecture/nodes/configure.md` §3).
pub const DEFAULT_SCREENER_MODEL: &str = "google:gemini-decision-1";

/// Default canonical generative Gemini screener model specifier when explicitly overriding discriminative screening.
pub const DEFAULT_GENERATIVE_SCREENER_MODEL: &str = "google:gemini-4-hydrogen";

/// Explicit minimal/zero reasoning budget signal for heuristic screener stages (`apprise`, `docket`, `admit`, `vet`).
pub const DEFAULT_SCREENER_REASONING_BUDGET: Option<u32> = Some(0);

/// Default canonical auditor model specifier (`docs/architecture/nodes/configure.md` §3).
pub const DEFAULT_AUDITOR_MODEL: &str = "google:gemini-4-carbon";

/// Default qualitative thinking level applied to `audit` trials when neither `thinking_level` nor `reasoning_budget` is configured.
pub const DEFAULT_AUDITOR_THINKING_LEVEL: ThinkingLevel = ThinkingLevel::Low;

/// Default qualitative thinking level applied to `probe` health checks on Gemini 4 models when unspecified.
pub const DEFAULT_PROBE_THINKING_LEVEL: ThinkingLevel = ThinkingLevel::Low;

/// Provider-neutral qualitative reasoning effort tier (`docs/architecture/nodes/configure.md` §3).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// Minimal reasoning effort (maps to `"LOW"` on Gemini 4 models that lack a `"MINIMAL"` tier).
    Minimal,
    /// Low reasoning effort prioritizing fast turnaround and token economy (default for `audit` and `probe`).
    Low,
    /// Balanced reasoning depth and latency.
    Medium,
    /// Deep extended reasoning effort for intricate statutory analysis.
    High,
}

impl ThinkingLevel {
    /// Returns the canonical lowercase identifier (`"minimal"`, `"low"`, `"medium"`, `"high"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// Returns the uppercase Proto3 enum wire string (`"MINIMAL"`, `"LOW"`, `"MEDIUM"`, `"HIGH"`).
    pub const fn gemini_wire_str(self) -> &'static str {
        match self {
            Self::Minimal => "MINIMAL",
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

impl fmt::Display for ThinkingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ThinkingLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            _ => Err(s.to_string()),
        }
    }
}

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
    /// Optional qualitative reasoning effort tier governing extended thinking during `audit` trials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
    /// Optional token ceiling governing extended thinking during `audit` trials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_budget: Option<u32>,
}

impl Default for CaseloadConfig {
    fn default() -> Self {
        Self {
            screener_model: DEFAULT_SCREENER_MODEL.to_string(),
            auditor_model: DEFAULT_AUDITOR_MODEL.to_string(),
            thinking_level: None,
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
            r#"{"screener_model":"google:gemini-decision-1","auditor_model":"google:gemini-4-carbon"}"#
        );
        assert!(!json.contains("reasoning_budget"));
        assert!(!json.contains("thinking_level"));

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

        let with_level = CaseloadConfig {
            thinking_level: Some(ThinkingLevel::Low),
            ..CaseloadConfig::default()
        };
        let level_json = serde_json::to_string(&with_level).expect("serialize thinking_level");
        assert!(level_json.contains(r#""thinking_level":"low""#));
        let decoded_level: CaseloadConfig =
            serde_json::from_str(&level_json).expect("deserialize thinking_level");
        assert_eq!(decoded_level.thinking_level, Some(ThinkingLevel::Low));
    }

    #[test]
    fn test_thinking_level_parsing_and_wire_strings() {
        for (raw, expected, wire) in [
            ("minimal", ThinkingLevel::Minimal, "MINIMAL"),
            ("  LOW ", ThinkingLevel::Low, "LOW"),
            ("Medium", ThinkingLevel::Medium, "MEDIUM"),
            ("HIGH", ThinkingLevel::High, "HIGH"),
        ] {
            let parsed: ThinkingLevel = raw.parse().expect("valid thinking level");
            assert_eq!(parsed, expected);
            assert_eq!(parsed.gemini_wire_str(), wire);
        }
        assert!("extreme".parse::<ThinkingLevel>().is_err());
    }
}
