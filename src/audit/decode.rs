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

//! Reason-first trial decree decoder and deterministic `CodeAnnotation` guardrails for `audit` (`docs/architecture/nodes/audit.md` §3–§4).

use crate::caseload::{
    AnnotationLevel, CanonAdjudication, CodeAnnotation, VerdictStatus, normalize_compliance_score,
    sort_annotations,
};
use crate::provider::ProviderError;
use serde_json::Value;
use std::collections::BTreeSet;

/// Decodes and validates a single case's structured trial response, enforcing admitted-path
/// isolation and 1-based line-coordinate normalization (`docs/architecture/nodes/audit.md` §3).
pub(super) fn decode_case_adjudication(
    payload: &Value,
    canon_ref: &str,
    canon_id: &str,
    admitted_file_paths: &BTreeSet<String>,
    threshold: f64,
    model_specifier: &str,
) -> Result<CanonAdjudication, ProviderError> {
    let obj = payload
        .as_object()
        .ok_or_else(|| schema_err(model_specifier, canon_ref, "expected top-level JSON object"))?;

    let compliance_summary = obj
        .get("compliance_summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            schema_err(
                model_specifier,
                canon_ref,
                "missing or empty `compliance_summary` string",
            )
        })?
        .to_string();

    let raw_score = obj
        .get("compliance_score")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            schema_err(
                model_specifier,
                canon_ref,
                "missing or non-numeric `compliance_score`",
            )
        })?;
    let compliance_score = normalize_compliance_score(raw_score)
        .map_err(|detail| schema_err(model_specifier, canon_ref, &detail))?;

    let raw_annotations = obj
        .get("annotations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            schema_err(
                model_specifier,
                canon_ref,
                "missing or non-array `annotations` field",
            )
        })?;

    let status = VerdictStatus::from_score(compliance_score, threshold);
    let mut annotations = Vec::new();

    if status == VerdictStatus::Fail
        && let Some(fallback_file_path) = admitted_file_paths.iter().next()
    {
        for (idx, item) in raw_annotations.iter().enumerate() {
            let normalized = decode_and_normalize_annotation(
                item,
                idx,
                canon_ref,
                canon_id,
                admitted_file_paths,
                fallback_file_path,
                model_specifier,
            )?;
            annotations.push(normalized);
        }
    } else if status == VerdictStatus::Fail {
        // Pure metadata-plane case (`pr_title`, `pr_body`, `linked_issues`): validate element
        // shapes if the model emitted any, then enforce `annotations: []`.
        for (idx, item) in raw_annotations.iter().enumerate() {
            validate_annotation_shape_only(item, idx, canon_ref, model_specifier)?;
        }
    }

    sort_annotations(&mut annotations);

    Ok(CanonAdjudication {
        canon_path: canon_ref.to_string(),
        compliance_summary,
        compliance_score,
        status,
        annotations,
    })
}

fn decode_and_normalize_annotation(
    item: &Value,
    idx: usize,
    canon_ref: &str,
    canon_id: &str,
    admitted_file_paths: &BTreeSet<String>,
    fallback_file_path: &str,
    model_specifier: &str,
) -> Result<CodeAnnotation, ProviderError> {
    let obj = item.as_object().ok_or_else(|| {
        schema_err(
            model_specifier,
            canon_ref,
            &format!("annotation[{idx}] must be a JSON object"),
        )
    })?;

    let raw_path = obj
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            schema_err(
                model_specifier,
                canon_ref,
                &format!("annotation[{idx}] missing non-empty `path`"),
            )
        })?;

    let raw_start_line = parse_required_line(obj, "start_line", idx, canon_ref, model_specifier)?;
    let raw_end_line = parse_required_line(obj, "end_line", idx, canon_ref, model_specifier)?;
    let raw_start_col =
        parse_optional_column(obj, "start_column", idx, canon_ref, model_specifier)?;
    let raw_end_col = parse_optional_column(obj, "end_column", idx, canon_ref, model_specifier)?;
    let annotation_level =
        parse_annotation_level(obj.get("annotation_level"), idx, canon_ref, model_specifier)?;

    let message = obj
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            schema_err(
                model_specifier,
                canon_ref,
                &format!("annotation[{idx}] missing non-empty `message`"),
            )
        })?
        .to_string();

    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| Some(canon_id.to_string()));

    if admitted_file_paths.contains(raw_path) {
        let start_line = raw_start_line.max(1);
        let end_line = raw_end_line.max(start_line);
        let (start_column, end_column) =
            normalize_columns(start_line, end_line, raw_start_col, raw_end_col);
        Ok(CodeAnnotation {
            path: raw_path.to_string(),
            start_line,
            end_line,
            start_column,
            end_column,
            annotation_level,
            message,
            title,
        })
    } else {
        // Re-anchor un-admitted / missing-companion file citations to line 1 of the primary
        // admitted file exhibit so omission findings never reference non-admitted paths.
        Ok(CodeAnnotation {
            path: fallback_file_path.to_string(),
            start_line: 1,
            end_line: 1,
            start_column: None,
            end_column: None,
            annotation_level,
            message,
            title,
        })
    }
}

fn validate_annotation_shape_only(
    item: &Value,
    idx: usize,
    canon_ref: &str,
    model_specifier: &str,
) -> Result<(), ProviderError> {
    let obj = item.as_object().ok_or_else(|| {
        schema_err(
            model_specifier,
            canon_ref,
            &format!("annotation[{idx}] must be a JSON object"),
        )
    })?;
    let _ = parse_annotation_level(obj.get("annotation_level"), idx, canon_ref, model_specifier)?;
    if obj
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(schema_err(
            model_specifier,
            canon_ref,
            &format!("annotation[{idx}] missing non-empty `message`"),
        ));
    }
    Ok(())
}

fn parse_required_line(
    obj: &serde_json::Map<String, Value>,
    field: &str,
    idx: usize,
    canon_ref: &str,
    model_specifier: &str,
) -> Result<usize, ProviderError> {
    let raw = obj.get(field).and_then(Value::as_i64).ok_or_else(|| {
        schema_err(
            model_specifier,
            canon_ref,
            &format!("annotation[{idx}] missing integer `{field}`"),
        )
    })?;
    if raw < 0 {
        return Err(schema_err(
            model_specifier,
            canon_ref,
            &format!("annotation[{idx}] `{field}` cannot be negative, got {raw}"),
        ));
    }
    Ok((raw as usize).max(1))
}

fn parse_optional_column(
    obj: &serde_json::Map<String, Value>,
    field: &str,
    idx: usize,
    canon_ref: &str,
    model_specifier: &str,
) -> Result<Option<usize>, ProviderError> {
    let Some(val) = obj.get(field) else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let raw = val.as_i64().ok_or_else(|| {
        schema_err(
            model_specifier,
            canon_ref,
            &format!("annotation[{idx}] `{field}` must be an integer when present"),
        )
    })?;
    if raw < 0 {
        return Err(schema_err(
            model_specifier,
            canon_ref,
            &format!("annotation[{idx}] `{field}` cannot be negative, got {raw}"),
        ));
    }
    Ok(if raw == 0 { None } else { Some(raw as usize) })
}

fn normalize_columns(
    start_line: usize,
    end_line: usize,
    start_col: Option<usize>,
    end_col: Option<usize>,
) -> (Option<usize>, Option<usize>) {
    match (start_col, end_col) {
        (Some(sc), Some(ec)) if start_line == end_line && ec < sc => (Some(sc), Some(sc)),
        other => other,
    }
}

fn parse_annotation_level(
    val: Option<&Value>,
    idx: usize,
    canon_ref: &str,
    model_specifier: &str,
) -> Result<AnnotationLevel, ProviderError> {
    match val.and_then(Value::as_str).map(str::trim) {
        Some("failure") => Ok(AnnotationLevel::Failure),
        Some("warning") => Ok(AnnotationLevel::Warning),
        Some("notice") => Ok(AnnotationLevel::Notice),
        other => Err(schema_err(
            model_specifier,
            canon_ref,
            &format!(
                "annotation[{idx}] invalid `annotation_level` {:?} (expected `failure`, `warning`, or `notice`)",
                other
            ),
        )),
    }
}

fn schema_err(model_specifier: &str, canon_ref: &str, detail: &str) -> ProviderError {
    ProviderError::SchemaDecode {
        model_specifier: model_specifier.to_string(),
        message: format!("case `{canon_ref}`: {detail}"),
    }
}
