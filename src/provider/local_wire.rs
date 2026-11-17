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

//! OpenAI-compatible `/v1/chat/completions` wire envelopes and calibrated two-class `YES`/`NO` logprob softmax decoding.

use super::local_decision::LOCAL_DISCRIMINATOR_SYSTEM_PROMPT;
use crate::provider::{ProviderError, TokenUsage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const DEFAULT_TOP_LOGPROBS: u8 = 5;

/// Chat message item in an OpenAI-compatible `/v1/chat/completions` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LocalChatMessage {
    pub(crate) role: String,
    pub(crate) content: String,
}

/// Request payload for `POST /v1/chat/completions` against a local logprob discriminator (`llama-server`, `vLLM`, `SGLang`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LocalChatCompletionRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<LocalChatMessage>,
    pub(crate) max_tokens: u32,
    pub(crate) temperature: f32,
    pub(crate) logprobs: bool,
    pub(crate) top_logprobs: u8,
    pub(crate) cache_prompt: bool,
}

impl LocalChatCompletionRequest {
    pub(crate) fn new(model: impl Into<String>, state: &str, instructions: &str) -> Self {
        let user_content = format!("State (`state`):\n{state}\n\n---\n\n{instructions}");
        Self {
            model: model.into(),
            messages: vec![
                LocalChatMessage {
                    role: "system".to_string(),
                    content: LOCAL_DISCRIMINATOR_SYSTEM_PROMPT.to_string(),
                },
                LocalChatMessage {
                    role: "user".to_string(),
                    content: user_content,
                },
            ],
            max_tokens: 1,
            temperature: 0.0,
            logprobs: true,
            top_logprobs: DEFAULT_TOP_LOGPROBS,
            cache_prompt: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct LocalTopLogprob {
    pub(crate) token: String,
    pub(crate) logprob: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct LocalTokenLogprob {
    pub(crate) token: String,
    pub(crate) logprob: f64,
    #[serde(default)]
    pub(crate) top_logprobs: Vec<LocalTopLogprob>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct LocalChoiceLogprobs {
    #[serde(default)]
    pub(crate) content: Vec<LocalTokenLogprob>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct LocalAssistantMessage {
    #[serde(default)]
    pub(crate) content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct LocalChatChoice {
    #[serde(default)]
    pub(crate) message: LocalAssistantMessage,
    #[serde(default)]
    pub(crate) logprobs: Option<LocalChoiceLogprobs>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct LocalChatUsage {
    #[serde(default)]
    pub(crate) prompt_tokens: u64,
    #[serde(default)]
    pub(crate) completion_tokens: u64,
    #[serde(default)]
    pub(crate) total_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct LocalChatCompletionResponse {
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) choices: Vec<LocalChatChoice>,
    #[serde(default)]
    pub(crate) usage: Option<LocalChatUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LocalErrorField {
    Object { message: Option<String> },
    Text(String),
}

#[derive(Debug, Deserialize)]
struct LocalErrorEnvelope {
    error: Option<LocalErrorField>,
    message: Option<String>,
}

pub(crate) fn extract_local_error_detail(body: &str) -> String {
    if let Ok(env) = serde_json::from_str::<LocalErrorEnvelope>(body) {
        let msg = match env.error {
            Some(LocalErrorField::Object { message }) => message,
            Some(LocalErrorField::Text(text)) => Some(text),
            None => None,
        }
        .or(env.message)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

        if let Some(found) = msg {
            return found;
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "empty error response".to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenPolarity {
    Affirmative,
    Negative,
}

fn classify_polarity(raw_token: &str) -> Option<TokenPolarity> {
    let cleaned = raw_token
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation());
    if cleaned.eq_ignore_ascii_case("yes") {
        Some(TokenPolarity::Affirmative)
    } else if cleaned.eq_ignore_ascii_case("no") {
        Some(TokenPolarity::Negative)
    } else {
        None
    }
}

fn decode_choice_yes_probability(choice: &LocalChatChoice) -> Result<f64, String> {
    if let Some(first_tok) = choice.logprobs.as_ref().and_then(|lp| lp.content.first())
        && let Some(prob) = compute_logprob_softmax(first_tok)
    {
        return Ok(prob);
    }

    if let Some(text) = choice.message.content.as_deref() {
        match classify_polarity(text) {
            Some(TokenPolarity::Affirmative) => return Ok(1.0),
            Some(TokenPolarity::Negative) => return Ok(0.0),
            None => {}
        }
    }

    Err(
        "response lacked calibratable `YES`/`NO` token logprobs or polar completion content"
            .to_string(),
    )
}

fn compute_logprob_softmax(first_tok: &LocalTokenLogprob) -> Option<f64> {
    let mut unique_tokens: BTreeMap<&str, f64> = BTreeMap::new();
    if first_tok.logprob.is_finite() {
        unique_tokens.insert(first_tok.token.as_str(), first_tok.logprob);
    }
    for entry in &first_tok.top_logprobs {
        if entry.logprob.is_finite() {
            unique_tokens
                .entry(entry.token.as_str())
                .and_modify(|prev| *prev = prev.max(entry.logprob))
                .or_insert(entry.logprob);
        }
    }

    let mut aff_mass = 0.0_f64;
    let mut neg_mass = 0.0_f64;
    let mut total_top_mass = 0.0_f64;
    let mut min_top_lp = f64::INFINITY;

    for (tok, lp) in unique_tokens {
        let p = lp.exp();
        total_top_mass += p;
        min_top_lp = min_top_lp.min(lp);
        match classify_polarity(tok) {
            Some(TokenPolarity::Affirmative) => aff_mass += p,
            Some(TokenPolarity::Negative) => neg_mass += p,
            None => {}
        }
    }

    if aff_mass > 0.0 && neg_mass > 0.0 {
        return Some((aff_mass / (aff_mass + neg_mass)).clamp(0.0, 1.0));
    }
    let tail_bound = if min_top_lp.is_finite() {
        (1.0 - total_top_mass).clamp(0.0, min_top_lp.exp())
    } else {
        0.0
    };
    if aff_mass > 0.0 {
        return Some((aff_mass / (aff_mass + tail_bound)).clamp(0.0, 1.0));
    }
    if neg_mass > 0.0 {
        return Some((tail_bound / (neg_mass + tail_bound)).clamp(0.0, 1.0));
    }
    None
}

pub(crate) fn decode_single_completion(
    body: &str,
    model_specifier: &str,
    domain_key: &str,
) -> Result<(f64, Option<String>, TokenUsage), ProviderError> {
    let parsed: LocalChatCompletionResponse =
        serde_json::from_str(body).map_err(|err| ProviderError::MalformedResponse {
            model_specifier: model_specifier.to_string(),
            reason: format!("invalid local `/v1/chat/completions` JSON envelope ({err})"),
        })?;

    let choice = parsed
        .choices
        .first()
        .ok_or_else(|| ProviderError::MalformedResponse {
            model_specifier: model_specifier.to_string(),
            reason: "empty `choices` array in local `/v1/chat/completions` response".to_string(),
        })?;

    let score =
        decode_choice_yes_probability(choice).map_err(|reason| ProviderError::SchemaDecode {
            model_specifier: model_specifier.to_string(),
            message: format!("failed to calibrate `noul` score for `{domain_key}`: {reason}"),
        })?;

    let resolved = parsed
        .model
        .as_deref()
        .map(normalize_local_model_label)
        .filter(|m| !m.is_empty());

    let usage = match parsed.usage {
        Some(u) => TokenUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            reasoning_tokens: 0,
            total_tokens: u
                .total_tokens
                .unwrap_or_else(|| u.prompt_tokens.saturating_add(u.completion_tokens)),
        },
        None => TokenUsage::default(),
    };

    Ok((score, resolved, usage))
}

pub(crate) fn normalize_local_model_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if (trimmed.contains('/') || trimmed.contains('\\'))
        && trimmed.to_ascii_lowercase().ends_with(".gguf")
    {
        return trimmed
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(trimmed)
            .to_string();
    }
    trimmed.to_string()
}
