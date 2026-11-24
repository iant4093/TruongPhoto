use crate::AiError;
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use serde_json::Value;
use std::collections::HashSet;
use truongphoto_core::RouterPlan;

/// Strict JSON Schema for `RouterPlan` structured output (`additionalProperties: false`).
/// Uses human-readable ISO date strings (`date_from` / `date_to` as `YYYY-MM-DD`) so local LLMs
/// never have to compute 13-digit Unix millisecond timestamps in their head.
pub const ROUTER_PLAN_JSON_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "RouterPlan",
  "type": "object",
  "additionalProperties": false,
  "required": [
    "intent",
    "semantic_query",
    "person_names",
    "location_query",
    "clear_prior_filters",
    "assistant_reply",
    "follow_up_suggestions"
  ],
  "properties": {
    "intent": {
      "type": ["string", "null"],
      "enum": ["search", "answer", null]
    },
    "semantic_query": { "type": ["string", "null"] },
    "ocr_terms": { "type": ["string", "null"] },
    "person_names": {
      "type": "array",
      "items": { "type": "string" }
    },
    "exclude_person_names": {
      "type": "array",
      "items": { "type": "string" }
    },
    "location_query": { "type": ["string", "null"] },
    "exclude_location_query": { "type": ["string", "null"] },
    "camera_model": { "type": ["string", "null"] },
    "lens_model": { "type": ["string", "null"] },
    "focal_length_min_mm": { "type": ["number", "null"] },
    "focal_length_max_mm": { "type": ["number", "null"] },
    "aperture_min_f": { "type": ["number", "null"] },
    "aperture_max_f": { "type": ["number", "null"] },
    "iso_min": { "type": ["integer", "null"] },
    "iso_max": { "type": ["integer", "null"] },
    "shutter_speed_min_sec": { "type": ["number", "null"] },
    "shutter_speed_max_sec": { "type": ["number", "null"] },
    "orientation": {
      "type": ["string", "null"],
      "enum": ["portrait", "landscape", "square", "panorama", null]
    },
    "has_people": { "type": ["boolean", "null"] },
    "min_faces": { "type": ["integer", "null"] },
    "max_faces": { "type": ["integer", "null"] },
    "sort_by": {
      "type": ["string", "null"],
      "enum": [
        "best",
        "capture_desc",
        "capture_asc",
        "focal_asc",
        "focal_desc",
        "iso_desc",
        "iso_asc",
        "aperture_asc",
        "shutter_asc",
        "shutter_desc",
        "duration_desc",
        null
      ]
    },
    "tags": {
      "type": "array",
      "items": { "type": "string" }
    },
    "kind": {
      "type": ["string", "null"],
      "enum": ["photo", "video", "live_photo", "motion_photo", "raw_jpg_stack", null]
    },
    "exclude_kind": {
      "type": ["string", "null"],
      "enum": ["photo", "video", "live_photo", "motion_photo", "raw_jpg_stack", null]
    },
    "favorite": { "type": ["boolean", "null"] },
    "date_from": { "type": ["string", "null"] },
    "date_to": { "type": ["string", "null"] },
    "clear_prior_filters": { "type": "boolean" },
    "assistant_reply": { "type": "string" },
    "follow_up_suggestions": {
      "type": "array",
      "items": { "type": "string" }
    }
  }
}"#;

const ALLOWED_ROUTER_PLAN_KEYS: &[&str] = &[
    "intent",
    "semantic_query",
    "ocr_terms",
    "person_names",
    "exclude_person_names",
    "location_query",
    "exclude_location_query",
    "camera_model",
    "lens_model",
    "focal_length_min_mm",
    "focal_length_max_mm",
    "aperture_min_f",
    "aperture_max_f",
    "iso_min",
    "iso_max",
    "shutter_speed_min_sec",
    "shutter_speed_max_sec",
    "orientation",
    "has_people",
    "min_faces",
    "max_faces",
    "sort_by",
    "tags",
    "kind",
    "exclude_kind",
    "favorite",
    "date_from",
    "date_to",
    "date_from_ms",
    "date_to_ms",
    "clear_prior_filters",
    "assistant_reply",
    "follow_up_suggestions",
];

fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (next_y, next_m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_y, next_m, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
}

/// Parses an ISO date string (`YYYY-MM-DD`, `YYYY-MM`, or `YYYY`, or RFC3339 prefix)
/// into UTC epoch milliseconds (start of period when `is_end == false`, end of day when `is_end == true`).
fn parse_iso_date_to_ms(raw: &str, is_end: bool) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip time component if the LLM emitted YYYY-MM-DDTHH:MM:SSZ
    let date_part = trimmed.split(['T', ' ']).next().unwrap_or(trimmed);
    let parts: Vec<&str> = date_part.split('-').collect();
    let naive = match parts.as_slice() {
        [y, m, d] => {
            let year = y.parse::<i32>().ok()?;
            let month = m.parse::<u32>().ok()?;
            let day = d.parse::<u32>().ok()?;
            NaiveDate::from_ymd_opt(year, month, day)?
        }
        [y, m] => {
            let year = y.parse::<i32>().ok()?;
            let month = m.parse::<u32>().ok()?;
            let day = if is_end {
                last_day_of_month(year, month)
            } else {
                1
            };
            NaiveDate::from_ymd_opt(year, month, day)?
        }
        [y] if y.len() == 4 => {
            let year = y.parse::<i32>().ok()?;
            if is_end {
                NaiveDate::from_ymd_opt(year, 12, 31)?
            } else {
                NaiveDate::from_ymd_opt(year, 1, 1)?
            }
        }
        _ => return None,
    };

    let dt = if is_end {
        naive.and_hms_milli_opt(23, 59, 59, 999)?
    } else {
        naive.and_hms_milli_opt(0, 0, 0, 0)?
    };
    Some(Utc.from_utc_datetime(&dt).timestamp_millis())
}

fn strip_json_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let after_lang = rest.split_once('\n').map(|(_, body)| body).unwrap_or(rest);
        after_lang
            .trim()
            .strip_suffix("```")
            .map(str::trim)
            .unwrap_or_else(|| after_lang.trim())
    } else {
        trimmed
    }
}

fn strip_trailing_count_suffix(raw: &str) -> &str {
    if let Some((head, tail)) = raw.rsplit_once(" (") {
        if let Some(inner) = tail.strip_suffix(')') {
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                return head.trim();
            }
        }
    }
    raw.trim()
}

fn normalize_opt_string(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        // Small models (Tier 1) occasionally emit literal placeholder strings instead of JSON null.
        if matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "null" | "none" | "n/a" | "nil" | "undefined" | "any"
        ) {
            return None;
        }
        Some(trimmed.to_string())
    })
}

fn normalize_entity_opt_string(value: Option<String>) -> Option<String> {
    normalize_opt_string(value).and_then(|s| {
        let stripped = strip_trailing_count_suffix(&s);
        if stripped.is_empty() {
            None
        } else {
            Some(stripped.to_string())
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
        if matches!(key.as_str(), "null" | "none" | "n/a") {
            continue;
        }
        if seen.insert(key) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn normalize_entity_vec(items: Vec<String>) -> Vec<String> {
    let cleaned = items
        .into_iter()
        .map(|s| strip_trailing_count_suffix(&s).to_string())
        .collect();
    normalize_string_vec(cleaned)
}

/// Parses and strictly validates a raw JSON string into [`RouterPlan`].
///
/// Rejects malformed JSON, non-object JSON values, unknown/hallucinated keys (e.g. `file_paths`),
/// invalid enum/type values, and inverted numeric date ranges (`date_from_ms > date_to_ms`).
pub fn parse_and_validate_router_plan(raw_json: &str) -> Result<RouterPlan, AiError> {
    let trimmed = strip_json_code_fence(raw_json);
    if trimmed.is_empty() {
        return Err(AiError::InvalidInput("empty RouterPlan JSON".to_string()));
    }

    let mut value: Value = serde_json::from_str(trimmed)
        .map_err(|e| AiError::InvalidInput(format!("malformed RouterPlan JSON: {e}")))?;

    let obj = value
        .as_object_mut()
        .ok_or_else(|| AiError::InvalidInput("RouterPlan JSON must be an object".to_string()))?;

    for key in obj.keys() {
        if !ALLOWED_ROUTER_PLAN_KEYS.contains(&key.as_str()) {
            return Err(AiError::InvalidInput(format!(
                "unknown or hallucinated RouterPlan field `{key}`"
            )));
        }
    }

    // Deterministically convert ISO date strings (`date_from` / `date_to`) into UTC epoch ms.
    // Auto-swap inverted ISO date string pairs (common on smaller models).
    let iso_from = obj
        .remove("date_from")
        .and_then(|v| v.as_str().map(str::to_string));
    let iso_to = obj
        .remove("date_to")
        .and_then(|v| v.as_str().map(str::to_string));

    let mut parsed_from = iso_from
        .as_deref()
        .and_then(|raw| parse_iso_date_to_ms(raw, false));
    let mut parsed_to = iso_to
        .as_deref()
        .and_then(|raw| parse_iso_date_to_ms(raw, true));
    if let (Some(a), Some(b)) = (parsed_from, parsed_to) {
        if a > b {
            // Re-parse swapped raw dates so start-of-day / end-of-day alignment stays exact.
            parsed_from = iso_to
                .as_deref()
                .and_then(|raw| parse_iso_date_to_ms(raw, false))
                .or(Some(b));
            parsed_to = iso_from
                .as_deref()
                .and_then(|raw| parse_iso_date_to_ms(raw, true))
                .or(Some(a));
        }
    }

    if let Some(ms) = parsed_from {
        let needs_insert = obj.get("date_from_ms").is_none_or(|v| v.is_null());
        if needs_insert {
            obj.insert("date_from_ms".to_string(), Value::from(ms));
        }
    }
    if let Some(ms) = parsed_to {
        let needs_insert = obj.get("date_to_ms").is_none_or(|v| v.is_null());
        if needs_insert {
            obj.insert("date_to_ms".to_string(), Value::from(ms));
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

    // Sanitize numeric EXIF bounds: discard non-finite / non-positive values and swap inverted pairs
    // (common when LLMs parse "1/1000 to 1/2000" as min=0.001, max=0.0005).
    fn sanitize_pos_f64_pair(lo: &mut Option<f64>, hi: &mut Option<f64>) {
        if lo.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            *lo = None;
        }
        if hi.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            *hi = None;
        }
        if let (Some(a), Some(b)) = (*lo, *hi) {
            if a > b {
                *lo = Some(b);
                *hi = Some(a);
            }
        }
    }
    sanitize_pos_f64_pair(&mut plan.focal_length_min_mm, &mut plan.focal_length_max_mm);
    sanitize_pos_f64_pair(&mut plan.aperture_min_f, &mut plan.aperture_max_f);
    sanitize_pos_f64_pair(
        &mut plan.shutter_speed_min_sec,
        &mut plan.shutter_speed_max_sec,
    );
    if plan.iso_min == Some(0) {
        plan.iso_min = None;
    }
    if plan.iso_max == Some(0) {
        plan.iso_max = None;
    }
    if let (Some(a), Some(b)) = (plan.iso_min, plan.iso_max) {
        if a > b {
            plan.iso_min = Some(b);
            plan.iso_max = Some(a);
        }
    }

    if plan.min_faces == Some(0) {
        plan.min_faces = None;
    }
    if plan.max_faces == Some(0) {
        plan.max_faces = None;
    }
    if let (Some(a), Some(b)) = (plan.min_faces, plan.max_faces) {
        if a > b {
            plan.min_faces = Some(b);
            plan.max_faces = Some(a);
        }
    }

    plan.intent =
        normalize_opt_string(plan.intent).and_then(|i| match i.to_ascii_lowercase().as_str() {
            "search" => Some("search".to_string()),
            "answer" => Some("answer".to_string()),
            _ => None,
        });
    plan.semantic_query = normalize_opt_string(plan.semantic_query);
    plan.ocr_terms = normalize_opt_string(plan.ocr_terms);
    plan.location_query = normalize_entity_opt_string(plan.location_query);
    plan.exclude_location_query = normalize_entity_opt_string(plan.exclude_location_query);
    plan.camera_model = normalize_entity_opt_string(plan.camera_model);
    plan.lens_model = normalize_entity_opt_string(plan.lens_model);
    plan.orientation = normalize_opt_string(plan.orientation).and_then(|o| {
        match o.to_ascii_lowercase().as_str() {
            "portrait" | "vertical" => Some("portrait".to_string()),
            "landscape" | "horizontal" => Some("landscape".to_string()),
            "square" => Some("square".to_string()),
            "panorama" | "panoramic" | "pano" => Some("panorama".to_string()),
            _ => None,
        }
    });
    plan.sort_by =
        normalize_opt_string(plan.sort_by).and_then(|s| match s.to_ascii_lowercase().as_str() {
            "best" | "capture_desc" | "capture_asc" | "focal_asc" | "focal_desc" | "iso_desc"
            | "iso_asc" | "aperture_asc" | "shutter_asc" | "shutter_desc" | "duration_desc" => {
                Some(s.to_ascii_lowercase())
            }
            _ => None,
        });
    plan.person_names = normalize_entity_vec(plan.person_names);
    plan.exclude_person_names = normalize_entity_vec(plan.exclude_person_names);
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

/// Shifts calendar-date UTC midnight bounds in `plan` by `-tz_offset_minutes` so they represent
/// the user's true local-day UTC interval (e.g. UTC-8 `2026-11-23` -> `08:00Z..07:59:59.999Z`).
pub fn shift_plan_dates_to_utc(plan: &mut RouterPlan, tz_offset_minutes: Option<i32>) {
    if let Some(offset_min) = tz_offset_minutes {
        let clamped = offset_min.clamp(-14 * 60, 14 * 60);
        if clamped != 0 {
            let offset_ms = i64::from(clamped) * 60_000;
            if let Some(ms) = plan.date_from_ms {
                plan.date_from_ms = Some(ms.saturating_sub(offset_ms));
            }
            if let Some(ms) = plan.date_to_ms {
                plan.date_to_ms = Some(ms.saturating_sub(offset_ms));
            }
        }
    }
}

/// Merges a turn's `RouterPlan` over `prior` active filters.
///
/// When `next.clear_prior_filters` is `true`, prior filters are discarded and `next` is returned
/// with `clear_prior_filters = true`. Otherwise, unchanged constraints from `prior` are carried
/// forward and any non-empty/`Some` fields from `next` are overlaid.
pub fn merge_router_plan(prior: &RouterPlan, next: &RouterPlan) -> RouterPlan {
    let next_intent = normalize_opt_string(next.intent.clone());
    if next_intent.as_deref() == Some("answer") {
        return RouterPlan {
            intent: Some("answer".to_string()),
            clear_prior_filters: next.clear_prior_filters,
            assistant_reply: next.assistant_reply.trim().to_string(),
            follow_up_suggestions: normalize_string_vec(next.follow_up_suggestions.clone()),
            ..Default::default()
        };
    }

    if next.clear_prior_filters {
        return RouterPlan {
            intent: next_intent,
            semantic_query: normalize_opt_string(next.semantic_query.clone()),
            ocr_terms: normalize_opt_string(next.ocr_terms.clone()),
            person_names: normalize_entity_vec(next.person_names.clone()),
            exclude_person_names: normalize_entity_vec(next.exclude_person_names.clone()),
            location_query: normalize_entity_opt_string(next.location_query.clone()),
            exclude_location_query: normalize_entity_opt_string(
                next.exclude_location_query.clone(),
            ),
            camera_model: normalize_entity_opt_string(next.camera_model.clone()),
            lens_model: normalize_entity_opt_string(next.lens_model.clone()),
            focal_length_min_mm: next.focal_length_min_mm,
            focal_length_max_mm: next.focal_length_max_mm,
            aperture_min_f: next.aperture_min_f,
            aperture_max_f: next.aperture_max_f,
            iso_min: next.iso_min,
            iso_max: next.iso_max,
            shutter_speed_min_sec: next.shutter_speed_min_sec,
            shutter_speed_max_sec: next.shutter_speed_max_sec,
            orientation: normalize_opt_string(next.orientation.clone()),
            has_people: next.has_people,
            min_faces: next.min_faces,
            max_faces: next.max_faces,
            sort_by: normalize_opt_string(next.sort_by.clone()),
            tags: normalize_string_vec(next.tags.clone()),
            kind: next.kind,
            exclude_kind: next.exclude_kind,
            favorite: next.favorite,
            date_from_ms: next.date_from_ms,
            date_to_ms: next.date_to_ms,
            clear_prior_filters: true,
            assistant_reply: next.assistant_reply.trim().to_string(),
            follow_up_suggestions: normalize_string_vec(next.follow_up_suggestions.clone()),
        };
    }

    fn merge_ord_range<T: Copy + PartialOrd>(
        p_lo: Option<T>,
        p_hi: Option<T>,
        n_lo: Option<T>,
        n_hi: Option<T>,
    ) -> (Option<T>, Option<T>) {
        match (n_lo, n_hi) {
            (None, None) => (p_lo, p_hi),
            (Some(a), Some(b)) => (Some(a), Some(b)),
            (Some(a), None) => {
                if p_hi.is_some_and(|b| a <= b) {
                    (Some(a), p_hi)
                } else {
                    (Some(a), None)
                }
            }
            (None, Some(b)) => {
                if p_lo.is_some_and(|a| a <= b) {
                    (p_lo, Some(b))
                } else {
                    (None, Some(b))
                }
            }
        }
    }

    let (date_from_ms, date_to_ms) = merge_ord_range(
        prior.date_from_ms,
        prior.date_to_ms,
        next.date_from_ms,
        next.date_to_ms,
    );
    let (focal_length_min_mm, focal_length_max_mm) = merge_ord_range(
        prior.focal_length_min_mm,
        prior.focal_length_max_mm,
        next.focal_length_min_mm,
        next.focal_length_max_mm,
    );
    let (aperture_min_f, aperture_max_f) = merge_ord_range(
        prior.aperture_min_f,
        prior.aperture_max_f,
        next.aperture_min_f,
        next.aperture_max_f,
    );
    let (iso_min, iso_max) =
        merge_ord_range(prior.iso_min, prior.iso_max, next.iso_min, next.iso_max);
    let (shutter_speed_min_sec, shutter_speed_max_sec) = merge_ord_range(
        prior.shutter_speed_min_sec,
        prior.shutter_speed_max_sec,
        next.shutter_speed_min_sec,
        next.shutter_speed_max_sec,
    );

    let (min_faces, max_faces) = merge_ord_range(
        prior.min_faces,
        prior.max_faces,
        next.min_faces,
        next.max_faces,
    );

    let mut merged_people = merge_string_lists(&prior.person_names, &next.person_names);
    let mut merged_exclude_people =
        merge_string_lists(&prior.exclude_person_names, &next.exclude_person_names);
    // If a person was explicitly included in this turn, remove them from exclusions and vice versa.
    if !next.person_names.is_empty() {
        let inc_low: HashSet<String> = next
            .person_names
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .collect();
        merged_exclude_people.retain(|s| !inc_low.contains(&s.to_ascii_lowercase()));
    }
    if !next.exclude_person_names.is_empty() {
        let exc_low: HashSet<String> = next
            .exclude_person_names
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .collect();
        merged_people.retain(|s| !exc_low.contains(&s.to_ascii_lowercase()));
    }

    let mut merged_has_people = next.has_people.or(prior.has_people);
    let mut merged_min_faces = min_faces;
    let mut merged_max_faces = max_faces;
    // Avoid contradictory SQL when switching between "photos of X" / "group shots" and "without people".
    if next.has_people == Some(false) {
        merged_people.clear();
        merged_min_faces = None;
        merged_max_faces = None;
        merged_has_people = Some(false);
    } else if (!next.person_names.is_empty()
        || next.has_people == Some(true)
        || next.min_faces.is_some())
        && merged_has_people == Some(false)
    {
        merged_has_people = if next.has_people == Some(true) {
            Some(true)
        } else {
            None
        };
    }

    // Reconcile `kind` vs `exclude_kind` two-way so "only videos" -> "no videos" never produces 0 hits.
    let mut merged_kind = next.kind.or(prior.kind);
    let merged_exclude_kind = if next.kind.is_some() && next.exclude_kind.is_none() {
        None
    } else {
        next.exclude_kind.or(prior.exclude_kind)
    };
    if next.kind.is_none() && merged_exclude_kind.is_some() && merged_kind == merged_exclude_kind {
        merged_kind = None;
    }

    // Reconcile `location_query` vs `exclude_location_query` two-way.
    let next_loc = normalize_entity_opt_string(next.location_query.clone());
    let prior_loc = normalize_entity_opt_string(prior.location_query.clone());
    let next_exc_loc = normalize_entity_opt_string(next.exclude_location_query.clone());
    let prior_exc_loc = normalize_entity_opt_string(prior.exclude_location_query.clone());
    let mut merged_loc = next_loc.clone().or(prior_loc);
    let mut merged_exc_loc = next_exc_loc.clone().or(prior_exc_loc);
    if let (Some(loc), Some(exc)) = (merged_loc.as_deref(), merged_exc_loc.as_deref()) {
        let loc_l = loc.to_ascii_lowercase();
        let exc_l = exc.to_ascii_lowercase();
        if loc_l == exc_l || loc_l.contains(&exc_l) || exc_l.contains(&loc_l) {
            if next_loc.is_some() && next_exc_loc.is_none() {
                merged_exc_loc = None;
            } else if next_exc_loc.is_some() && next_loc.is_none() {
                merged_loc = None;
            }
        }
    }

    RouterPlan {
        intent: next_intent,
        semantic_query: normalize_opt_string(next.semantic_query.clone())
            .or_else(|| normalize_opt_string(prior.semantic_query.clone())),
        ocr_terms: normalize_opt_string(next.ocr_terms.clone())
            .or_else(|| normalize_opt_string(prior.ocr_terms.clone())),
        person_names: merged_people,
        exclude_person_names: merged_exclude_people,
        location_query: merged_loc,
        exclude_location_query: merged_exc_loc,
        camera_model: normalize_entity_opt_string(next.camera_model.clone())
            .or_else(|| normalize_entity_opt_string(prior.camera_model.clone())),
        lens_model: normalize_entity_opt_string(next.lens_model.clone())
            .or_else(|| normalize_entity_opt_string(prior.lens_model.clone())),
        focal_length_min_mm,
        focal_length_max_mm,
        aperture_min_f,
        aperture_max_f,
        iso_min,
        iso_max,
        shutter_speed_min_sec,
        shutter_speed_max_sec,
        orientation: normalize_opt_string(next.orientation.clone())
            .or_else(|| normalize_opt_string(prior.orientation.clone())),
        has_people: merged_has_people,
        min_faces: merged_min_faces,
        max_faces: merged_max_faces,
        sort_by: normalize_opt_string(next.sort_by.clone())
            .or_else(|| normalize_opt_string(prior.sort_by.clone())),
        tags: merge_string_lists(&prior.tags, &next.tags),
        kind: merged_kind,
        exclude_kind: merged_exclude_kind,
        favorite: next.favorite.or(prior.favorite),
        date_from_ms,
        date_to_ms,
        clear_prior_filters: false,
        assistant_reply: next.assistant_reply.trim().to_string(),
        follow_up_suggestions: normalize_string_vec(next.follow_up_suggestions.clone()),
    }
}
