use crate::AiError;
use serde_json::Value;
use std::collections::HashSet;
use truongphoto_core::RouterPlan;

/// Strict JSON Schema for `RouterPlan` structured output (`additionalProperties: false`).
pub const ROUTER_PLAN_JSON_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "RouterPlan",
  "type": "object",
  "additionalProperties": false,
  "required": [
    "semantic_query",
    "ocr_terms",
    "person_names",
    "location_query",
    "camera_model",
    "tags",
    "kind",
    "favorite",
    "date_from_ms",
    "date_to_ms",
    "clear_prior_filters",
    "assistant_reply",
    "follow_up_suggestions"
  ],
  "properties": {
    "semantic_query": { "type": ["string", "null"] },
    "ocr_terms": { "type": ["string", "null"] },
    "person_names": {
      "type": "array",
      "items": { "type": "string" }
    },
    "location_query": { "type": ["string", "null"] },
    "camera_model": { "type": ["string", "null"] },
    "tags": {
      "type": "array",
      "items": { "type": "string" }
    },
    "kind": {
      "type": ["string", "null"],
      "enum": ["photo", "video", "live_photo", "motion_photo", "raw_jpg_stack", null]
    },
    "favorite": { "type": ["boolean", "null"] },
    "date_from_ms": { "type": ["integer", "null"] },
    "date_to_ms": { "type": ["integer", "null"] },
    "clear_prior_filters": { "type": "boolean" },
    "assistant_reply": { "type": "string" },
    "follow_up_suggestions": {
      "type": "array",
      "items": { "type": "string" }
    }
  }
}"#;

const ALLOWED_ROUTER_PLAN_KEYS: &[&str] = &[
    "semantic_query",
    "ocr_terms",
    "person_names",
    "location_query",
    "camera_model",
    "tags",
    "kind",
    "favorite",
    "date_from_ms",
    "date_to_ms",
    "clear_prior_filters",
    "assistant_reply",
    "follow_up_suggestions",
];

fn normalize_opt_string(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_string_vec(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Parses and strictly validates a raw JSON string into [`RouterPlan`].
///
/// Rejects malformed JSON, non-object JSON values, unknown/hallucinated keys (e.g. `file_paths`),
/// invalid enum/type values, and inverted date ranges (`date_from_ms > date_to_ms`).
pub fn parse_and_validate_router_plan(raw_json: &str) -> Result<RouterPlan, AiError> {
    let trimmed = raw_json.trim();
    if trimmed.is_empty() {
        return Err(AiError::InvalidInput("empty RouterPlan JSON".to_string()));
    }

    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| AiError::InvalidInput(format!("malformed RouterPlan JSON: {e}")))?;

    let obj = value
        .as_object()
        .ok_or_else(|| AiError::InvalidInput("RouterPlan JSON must be an object".to_string()))?;

    for key in obj.keys() {
        if !ALLOWED_ROUTER_PLAN_KEYS.contains(&key.as_str()) {
            return Err(AiError::InvalidInput(format!(
                "unknown or hallucinated RouterPlan field `{key}`"
            )));
        }
    }

    let mut plan: RouterPlan = serde_json::from_value(value)
        .map_err(|e| AiError::InvalidInput(format!("invalid RouterPlan field types: {e}")))?;

    if let (Some(from_ms), Some(to_ms)) = (plan.date_from_ms, plan.date_to_ms) {
        if from_ms > to_ms {
            return Err(AiError::InvalidInput(format!(
                "invalid date range: date_from_ms ({from_ms}) > date_to_ms ({to_ms})"
            )));
        }
    }

    plan.semantic_query = normalize_opt_string(plan.semantic_query);
    plan.ocr_terms = normalize_opt_string(plan.ocr_terms);
    plan.location_query = normalize_opt_string(plan.location_query);
    plan.camera_model = normalize_opt_string(plan.camera_model);
    plan.person_names = normalize_string_vec(plan.person_names);
    plan.tags = normalize_string_vec(plan.tags);
    plan.assistant_reply = plan.assistant_reply.trim().to_string();
    plan.follow_up_suggestions = normalize_string_vec(plan.follow_up_suggestions);

    Ok(plan)
}

fn merge_string_lists(prior: &[String], next: &[String]) -> Vec<String> {
    if next.is_empty() {
        return prior.to_vec();
    }
    let mut merged = Vec::with_capacity(prior.len() + next.len());
    let mut seen = HashSet::new();
    for item in prior.iter().chain(next.iter()) {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            merged.push(trimmed.to_string());
        }
    }
    merged
}

/// Merges a turn's `RouterPlan` over `prior` active filters.
///
/// When `next.clear_prior_filters` is `true`, prior filters are discarded and `next` is returned
/// with `clear_prior_filters = true`. Otherwise, unchanged constraints from `prior` are carried
/// forward and any non-empty/`Some` fields from `next` are overlaid.
pub fn merge_router_plan(prior: &RouterPlan, next: &RouterPlan) -> RouterPlan {
    if next.clear_prior_filters {
        return RouterPlan {
            semantic_query: normalize_opt_string(next.semantic_query.clone()),
            ocr_terms: normalize_opt_string(next.ocr_terms.clone()),
            person_names: normalize_string_vec(next.person_names.clone()),
            location_query: normalize_opt_string(next.location_query.clone()),
            camera_model: normalize_opt_string(next.camera_model.clone()),
            tags: normalize_string_vec(next.tags.clone()),
            kind: next.kind,
            favorite: next.favorite,
            date_from_ms: next.date_from_ms,
            date_to_ms: next.date_to_ms,
            clear_prior_filters: true,
            assistant_reply: next.assistant_reply.trim().to_string(),
            follow_up_suggestions: normalize_string_vec(next.follow_up_suggestions.clone()),
        };
    }

    let (date_from_ms, date_to_ms) = if next.date_from_ms.is_some() || next.date_to_ms.is_some() {
        (next.date_from_ms, next.date_to_ms)
    } else {
        (prior.date_from_ms, prior.date_to_ms)
    };

    RouterPlan {
        semantic_query: normalize_opt_string(next.semantic_query.clone())
            .or_else(|| normalize_opt_string(prior.semantic_query.clone())),
        ocr_terms: normalize_opt_string(next.ocr_terms.clone())
            .or_else(|| normalize_opt_string(prior.ocr_terms.clone())),
        person_names: merge_string_lists(&prior.person_names, &next.person_names),
        location_query: normalize_opt_string(next.location_query.clone())
            .or_else(|| normalize_opt_string(prior.location_query.clone())),
        camera_model: normalize_opt_string(next.camera_model.clone())
            .or_else(|| normalize_opt_string(prior.camera_model.clone())),
        tags: merge_string_lists(&prior.tags, &next.tags),
        kind: next.kind.or(prior.kind),
        favorite: next.favorite.or(prior.favorite),
        date_from_ms,
        date_to_ms,
        clear_prior_filters: false,
        assistant_reply: next.assistant_reply.trim().to_string(),
        follow_up_suggestions: normalize_string_vec(next.follow_up_suggestions.clone()),
    }
}
