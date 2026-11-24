use crate::error::{ApiJson, AppError};
use crate::extractors::{AuthSession, RbacContext};
use crate::ingest::cache_mgr::safe_join_relative;
use crate::routes::search::execute_hybrid_search;
use crate::state::AppState;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use truongphoto_ai::{ChatTurnInput, LibraryChatContext, RerankCandidate};
use truongphoto_core::{
    ChatHitRef, ChatMessageView, ChatRole, ChatSessionDetail, ChatSessionSummary, ChatTier,
    CreateChatSessionRequest, PersonSummary, RouterPlan, SearchHit, SearchMode, SearchRequest,
    SendChatMessageRequest, SendChatMessageResponse,
};
use truongphoto_db::{repo, DbError};

const DEFAULT_CHAT_LIMIT: usize = 60;
const MAX_CHAT_LIMIT: usize = 500;
const MAX_VLM_RERANK_CANDIDATES: usize = 10;
const RECENT_TURNS_WINDOW: usize = 6;
const UNMATCHED_PERSON_SENTINEL: &str = "__unmatched_person__";

fn levenshtein_ascii_ci(a: &str, b: &str) -> usize {
    let a_bytes: Vec<u8> = a.bytes().map(|c| c.to_ascii_lowercase()).collect();
    let b_bytes: Vec<u8> = b.bytes().map(|c| c.to_ascii_lowercase()).collect();
    if a_bytes.is_empty() {
        return b_bytes.len();
    }
    if b_bytes.is_empty() {
        return a_bytes.len();
    }
    let mut prev: Vec<usize> = (0..=b_bytes.len()).collect();
    let mut curr = vec![0usize; b_bytes.len() + 1];
    for (i, &ac) in a_bytes.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &bc) in b_bytes.iter().enumerate() {
            let cost = usize::from(ac != bc);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_bytes.len()]
}

#[derive(Debug, Serialize)]
pub struct ChatSessionListResponse {
    pub items: Vec<ChatSessionSummary>,
}

#[derive(Debug, Serialize)]
pub struct CreateChatSessionResponse {
    pub session: ChatSessionSummary,
}

#[derive(Debug, Serialize)]
pub struct ChatSessionDetailResponse {
    pub session: ChatSessionDetail,
    pub messages: Vec<ChatMessageView>,
}

fn auto_title_from_prompt(prompt: &str) -> String {
    let collapsed: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let prefix: String = chars.by_ref().take(48).collect();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else if prefix.is_empty() {
        "New Chat".to_string()
    } else {
        prefix
    }
}

struct ResolvedPeopleOutcome {
    person_ids: Option<Vec<String>>,
    unmatched: Vec<String>,
    ambiguous: Vec<(String, Vec<String>)>,
}

fn resolve_rbac_people(
    requested_names: &[String],
    visible_people: &[PersonSummary],
) -> ResolvedPeopleOutcome {
    if requested_names.is_empty() {
        return ResolvedPeopleOutcome {
            person_ids: None,
            unmatched: Vec::new(),
            ambiguous: Vec::new(),
        };
    }

    let mut resolved = Vec::with_capacity(requested_names.len());
    let mut seen = HashSet::new();
    let mut unmatched = Vec::new();
    let mut ambiguous = Vec::new();

    for raw_name in requested_names {
        let trimmed = raw_name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let exact = visible_people.iter().find(|p| {
            p.name
                .as_deref()
                .is_some_and(|n| n.trim().eq_ignore_ascii_case(trimmed))
        });

        let matched_id = if let Some(person) = exact {
            Some(person.id.clone())
        } else {
            // Fall back to first-name / word-token match (e.g. "Nate" -> "Nate Higgers").
            let token_matches: Vec<&PersonSummary> = visible_people
                .iter()
                .filter(|p| {
                    p.name.as_deref().is_some_and(|n| {
                        n.split_whitespace()
                            .any(|word| word.eq_ignore_ascii_case(trimmed))
                    })
                })
                .collect();
            if token_matches.len() == 1 {
                Some(token_matches[0].id.clone())
            } else if token_matches.len() > 1 {
                let candidates: Vec<String> = token_matches
                    .iter()
                    .filter_map(|p| p.name.as_deref().map(str::trim).map(str::to_string))
                    .collect();
                ambiguous.push((trimmed.to_string(), candidates));
                None
            } else {
                // Bounded prefix + Levenshtein typo fallback (e.g. "Sebastain" -> "Sebastian",
                // "Seb" -> "Sebastian") with strict ambiguity guard.
                let q_low = trimmed.to_ascii_lowercase();
                let q_len = q_low.chars().count();
                let max_dist = if q_len >= 6 {
                    2
                } else if q_len >= 4 {
                    1
                } else {
                    0
                };
                let mut fuzzy_matches: Vec<(&PersonSummary, usize)> = Vec::new();
                if q_len >= 3 {
                    for p in visible_people {
                        let Some(name) = p.name.as_deref().map(str::trim) else {
                            continue;
                        };
                        let n_low = name.to_ascii_lowercase();
                        let prefix_hit = n_low
                            .split_whitespace()
                            .any(|w| w.len() >= q_len && w.starts_with(&q_low));
                        if prefix_hit {
                            fuzzy_matches.push((p, 0));
                            continue;
                        }
                        if max_dist > 0 {
                            let best_d = std::iter::once(n_low.as_str())
                                .chain(n_low.split_whitespace())
                                .map(|w| levenshtein_ascii_ci(&q_low, w))
                                .min()
                                .unwrap_or(usize::MAX);
                            if best_d <= max_dist {
                                fuzzy_matches.push((p, best_d));
                            }
                        }
                    }
                }
                if let Some(&min_d) = fuzzy_matches.iter().map(|(_, d)| d).min() {
                    let best: Vec<&PersonSummary> = fuzzy_matches
                        .into_iter()
                        .filter(|(_, d)| *d == min_d)
                        .map(|(p, _)| p)
                        .collect();
                    if best.len() == 1 {
                        Some(best[0].id.clone())
                    } else {
                        let candidates: Vec<String> = best
                            .iter()
                            .filter_map(|p| p.name.as_deref().map(str::trim).map(str::to_string))
                            .collect();
                        ambiguous.push((trimmed.to_string(), candidates));
                        None
                    }
                } else {
                    unmatched.push(trimmed.to_string());
                    None
                }
            }
        };

        let pid = matched_id.unwrap_or_else(|| UNMATCHED_PERSON_SENTINEL.to_string());
        if seen.insert(pid.clone()) {
            resolved.push(pid);
        }
    }

    ResolvedPeopleOutcome {
        person_ids: if resolved.is_empty() {
            None
        } else {
            Some(resolved)
        },
        unmatched,
        ambiguous,
    }
}

fn build_search_request_from_plan(
    plan: &RouterPlan,
    person_ids: Option<Vec<String>>,
    exclude_person_ids: Option<Vec<String>>,
    limit: u32,
) -> SearchRequest {
    let semantic = plan
        .semantic_query
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let ocr = plan
        .ocr_terms
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mode = match (&semantic, &ocr) {
        (Some(_), _) => SearchMode::Hybrid,
        (None, Some(_)) => SearchMode::Ocr,
        (None, None) => SearchMode::Filters,
    };

    let tags = if plan.tags.is_empty() {
        None
    } else {
        Some(plan.tags.clone())
    };

    SearchRequest {
        query: semantic.clone().or_else(|| ocr.clone()),
        ocr_query: if semantic.is_some() { ocr } else { None },
        mode,
        folder_id: None,
        person_ids,
        exclude_person_ids,
        has_people: plan.has_people,
        min_faces: plan.min_faces,
        max_faces: plan.max_faces,
        tags,
        camera_model: plan.camera_model.clone(),
        lens_model: plan.lens_model.clone(),
        focal_length_min_mm: plan.focal_length_min_mm,
        focal_length_max_mm: plan.focal_length_max_mm,
        aperture_min_f: plan.aperture_min_f,
        aperture_max_f: plan.aperture_max_f,
        iso_min: plan.iso_min,
        iso_max: plan.iso_max,
        shutter_speed_min_sec: plan.shutter_speed_min_sec,
        shutter_speed_max_sec: plan.shutter_speed_max_sec,
        orientation: plan.orientation.clone(),
        sort_by: plan.sort_by.clone(),
        geo_query: plan.location_query.clone(),
        exclude_geo_query: plan.exclude_location_query.clone(),
        kind: plan.kind,
        exclude_kind: plan.exclude_kind,
        favorite: plan.favorite,
        date_from_ms: plan.date_from_ms,
        date_to_ms: plan.date_to_ms,
        limit: Some(limit),
        offset: None,
    }
}

fn build_rerank_caption_hint(hit: &SearchHit) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(title) = hit
        .asset
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(title.to_string());
    }
    if let Some(city) = hit
        .asset
        .geo_city
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(city.to_string());
    }
    if let Some(country) = hit
        .asset
        .geo_country
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(country.to_string());
    }
    if let Some(cam) = hit
        .asset
        .camera_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(cam.to_string());
    }
    if let Some(ocr) = hit
        .ocr_snippet
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(ocr.to_string());
    }
    if !hit.match_reasons.is_empty() {
        parts.extend(hit.match_reasons.iter().cloned());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn apply_vlm_rerank_order(hits: &mut Vec<SearchHit>, ordered_ids: &[(String, f32)]) {
    let top_count = hits.len().min(MAX_VLM_RERANK_CANDIDATES);
    if top_count == 0 {
        return;
    }

    let tail = hits.split_off(top_count);
    let original_order: Vec<String> = hits.iter().map(|h| h.asset.id.clone()).collect();
    let mut by_id: HashMap<String, SearchHit> =
        hits.drain(..).map(|h| (h.asset.id.clone(), h)).collect();

    let has_confident_hit = ordered_ids.iter().any(|(_, score)| *score >= 0.15);
    let mut strong = Vec::with_capacity(top_count);
    let mut demoted = Vec::new();
    for (asset_id, rerank_score) in ordered_ids {
        if let Some(mut hit) = by_id.remove(asset_id) {
            hit.score = *rerank_score;
            hit.match_reasons.push("VLM Reranked".to_string());
            if has_confident_hit && *rerank_score < 0.15 {
                demoted.push(hit);
            } else {
                strong.push(hit);
            }
        }
    }
    // Preserve original hybrid order for any remaining candidates not returned by the reranker.
    for asset_id in original_order {
        if let Some(hit) = by_id.remove(&asset_id) {
            strong.push(hit);
        }
    }
    strong.extend(tail);
    strong.extend(demoted);
    *hits = strong;
}

fn should_trigger_vlm_rerank(prompt: &str, plan: &RouterPlan) -> bool {
    // Never overwrite explicit deterministic sorts ("newest photos of dogs", "highest ISO", etc.).
    if let Some(sort_key) = plan.sort_by.as_deref() {
        if sort_key != "best" {
            return false;
        }
    }
    if plan.semantic_query.is_some() {
        return true;
    }
    // Pure "show me my favorites" has no visual prompt to rerank.
    if plan.favorite == Some(true) {
        return false;
    }
    let lower = prompt.to_ascii_lowercase();
    [
        "best",
        "cool",
        "standout",
        "highlight",
        "funniest",
        "funny",
        "smiling",
        "laughing",
        "happy",
        "candid",
        "dramatic",
        "sharpest",
        "wallpaper",
        "mood",
        "vibe",
        "aesthetic",
        "cute",
        "pretty",
        "looking at camera",
        "eye contact",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

fn strip_third_party_lens_brand(lens: &str) -> Option<String> {
    const BRANDS: &[&str] = &[
        "sigma",
        "tamron",
        "samyang",
        "tokina",
        "viltrox",
        "rokinon",
        "zeiss",
        "laowa",
        "ttartisan",
        "7artisans",
    ];
    let trimmed = lens.trim();
    let (first, rest) = trimmed.split_once(char::is_whitespace)?;
    let rest_trimmed = rest.trim();
    if !rest_trimmed.is_empty() && BRANDS.contains(&first.to_ascii_lowercase().as_str()) {
        Some(rest_trimmed.to_string())
    } else {
        None
    }
}

/// `GET /api/chat/sessions` (AuthSession; strictly scoped to `auth.user.id`).
pub async fn list_sessions(
    auth: AuthSession,
    State(state): State<Arc<AppState>>,
) -> Result<Json<ChatSessionListResponse>, AppError> {
    let user_id = auth.user.id;
    let items = state
        .db
        .read(move |conn| repo::chat::list_chat_sessions(conn, &user_id))
        .await?;
    Ok(Json(ChatSessionListResponse { items }))
}

/// `POST /api/chat/sessions` (AuthSession -> `201 Created`).
pub async fn create_session(
    auth: AuthSession,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<CreateChatSessionRequest>,
) -> Result<(StatusCode, Json<CreateChatSessionResponse>), AppError> {
    let user_id = auth.user.id;
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .map(|s| s.chars().take(120).collect::<String>())
        .unwrap_or_default();
    let now_ms = Utc::now().timestamp_millis();

    let session = state
        .db
        .write(move |conn| repo::chat::create_chat_session(conn, &user_id, &title, now_ms))
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateChatSessionResponse { session }),
    ))
}

/// `GET /api/chat/sessions/{id}` (AuthSession + RbacContext; `404` if missing or owned by another user).
pub async fn get_session(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<ChatSessionDetailResponse>, AppError> {
    let user_id = rbac.auth.user.id;
    let scope = rbac.scope;

    let (session, stored_messages, summaries) = state
        .db
        .read(move |conn| {
            let Some(session) = repo::chat::get_chat_session(conn, &user_id, &session_id)? else {
                return Err(DbError::NotFound("Chat session not found.".to_string()));
            };
            let stored = repo::chat::list_chat_messages(conn, &session_id)?;
            let mut asset_ids = Vec::new();
            for msg in &stored {
                for href in &msg.hit_refs {
                    asset_ids.push(href.asset_id.clone());
                }
            }
            let summaries = repo::search::load_asset_summaries_by_ids(conn, &scope, &asset_ids)?;
            Ok((session, stored, summaries))
        })
        .await?;

    let messages = stored_messages
        .into_iter()
        .map(|msg| {
            let hits = msg
                .hit_refs
                .into_iter()
                .filter_map(|href| {
                    summaries
                        .get(&href.asset_id)
                        .cloned()
                        .map(|asset| SearchHit {
                            asset,
                            score: href.score,
                            timestamp_sec: href.timestamp_sec,
                            match_reasons: Vec::new(),
                            ocr_snippet: None,
                        })
                })
                .collect();

            ChatMessageView {
                id: msg.id,
                session_id: msg.session_id,
                role: msg.role,
                content: msg.content,
                plan: msg.plan,
                hits,
                reranked: msg.reranked,
                planner_mode: msg.planner_mode,
                elapsed_ms: msg.elapsed_ms,
                created_at_ms: msg.created_at_ms,
            }
        })
        .collect();

    Ok(Json(ChatSessionDetailResponse { session, messages }))
}

/// `DELETE /api/chat/sessions/{id}` (AuthSession; `204 No Content`, `404` if missing or owned by another user).
pub async fn delete_session(
    auth: AuthSession,
    State(state): State<Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<StatusCode, AppError> {
    let user_id = auth.user.id;
    let deleted = state
        .db
        .write(move |conn| repo::chat::delete_chat_session(conn, &user_id, &session_id))
        .await?;

    if !deleted {
        return Err(AppError::not_found("Chat session not found."));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn synthesize_grounded_answer(
    prompt: &str,
    summary: &repo::search::LibraryChatSummary,
    cameras: &[String],
    lenses: &[String],
) -> String {
    let lower = prompt.to_ascii_lowercase();

    let people_list: Vec<String> = summary
        .person_counts
        .iter()
        .take(12)
        .map(|(name, cnt)| format!("{name} ({cnt})"))
        .collect();
    let folders_list: Vec<String> = summary
        .folders
        .iter()
        .take(8)
        .map(|(name, cnt)| format!("{name} ({cnt})"))
        .collect();

    if lower.contains("who") || lower.contains("people") {
        if people_list.is_empty() {
            return format!(
                "You have {} items in your library, and no named people are tagged yet. You can name detected faces in the People view.",
                summary.total_assets
            );
        }
        return format!(
            "Here are the named people in your library: {}. Ask me to show photos of any of them!",
            people_list.join(", ")
        );
    }

    if lower.contains("camera")
        || lower.contains("lens")
        || lower.contains("gear")
        || lower.contains("equipment")
    {
        let cam_str = if !summary.camera_counts.is_empty() {
            summary
                .camera_counts
                .iter()
                .take(8)
                .map(|(name, cnt)| format!("{name} ({cnt})"))
                .collect::<Vec<_>>()
                .join(", ")
        } else if cameras.is_empty() {
            "none detected".to_string()
        } else {
            cameras.join(", ")
        };
        let lens_str = if !summary.lens_counts.is_empty() {
            summary
                .lens_counts
                .iter()
                .take(8)
                .map(|(name, cnt)| format!("{name} ({cnt})"))
                .collect::<Vec<_>>()
                .join(", ")
        } else if lenses.is_empty() {
            "none detected".to_string()
        } else {
            lenses.join(", ")
        };
        return format!("Your library uses camera(s): {cam_str}, with lens(es): {lens_str}.");
    }

    if lower.contains("month")
        || lower.contains("year")
        || lower.contains("when")
        || lower.contains("date")
    {
        let mut parts = Vec::new();
        if let Some((lo, hi)) = &summary.date_range {
            parts.push(format!("Your library spans {lo} to {hi}."));
        }
        if !summary.top_months.is_empty() {
            let months: Vec<String> = summary
                .top_months
                .iter()
                .take(6)
                .map(|(m, c)| format!("{m} ({c})"))
                .collect();
            parts.push(format!("Busiest months: {}.", months.join(", ")));
        }
        if !summary.year_counts.is_empty() {
            let years: Vec<String> = summary
                .year_counts
                .iter()
                .take(6)
                .map(|(y, c)| format!("{y} ({c})"))
                .collect();
            parts.push(format!("By year: {}.", years.join(", ")));
        }
        if !parts.is_empty() {
            return parts.join(" ");
        }
    }

    if lower.contains("folder")
        || lower.contains("trip")
        || lower.contains("event")
        || lower.contains("where")
        || lower.contains("place")
    {
        let places_list: Vec<String> = summary
            .top_places
            .iter()
            .take(8)
            .map(|(name, cnt)| format!("{name} ({cnt})"))
            .collect();
        let mut parts = Vec::new();
        if !folders_list.is_empty() {
            parts.push(format!("Main collections: {}.", folders_list.join(", ")));
        }
        if !places_list.is_empty() {
            parts.push(format!("Top GPS places: {}.", places_list.join(", ")));
        }
        if parts.is_empty() {
            return format!("Your library contains {} items.", summary.total_assets);
        }
        return format!(
            "Your library has {} items. {}",
            summary.total_assets,
            parts.join(" ")
        );
    }

    let mut parts = vec![format!("Your library has {} items", summary.total_assets)];
    if summary.favorite_count > 0 {
        parts.push(format!("({} favorites)", summary.favorite_count));
    }
    if let Some((lo, hi)) = &summary.date_range {
        parts.push(format!("spanning {lo} to {hi}"));
    }
    let mut sentence = format!("{}.", parts.join(" "));
    if !summary.kind_counts.is_empty() {
        let kinds: Vec<String> = summary
            .kind_counts
            .iter()
            .map(|(k, c)| format!("{k}: {c}"))
            .collect();
        sentence.push_str(&format!(" Breakdown: {}.", kinds.join(", ")));
    }
    if !folders_list.is_empty() {
        sentence.push_str(&format!(" Main collections: {}.", folders_list.join(", ")));
    }
    if !people_list.is_empty() {
        sentence.push_str(&format!(" Recognized people: {}.", people_list.join(", ")));
    }
    if !cameras.is_empty() {
        sentence.push_str(&format!(" Shot on {}.", cameras.join(", ")));
    }
    sentence
}

fn describe_zero_hit_filters(
    plan: &RouterPlan,
    unmatched_people: &[String],
    ambiguous_people: &[(String, Vec<String>)],
    known_people: &[String],
) -> String {
    if let Some((query_name, candidates)) = ambiguous_people.first() {
        let preview: Vec<&str> = candidates.iter().take(6).map(String::as_str).collect();
        return format!(
            "Multiple people match \"{query_name}\": {}. Which one did you mean?",
            preview.join(", ")
        );
    }
    if !unmatched_people.is_empty() {
        let missing = unmatched_people.join(", ");
        return if known_people.is_empty() {
            format!(
                "I couldn't find anyone named {missing} in your library — no people have been named yet."
            )
        } else {
            let preview: Vec<&str> = known_people.iter().take(8).map(String::as_str).collect();
            format!(
                "I don't have anyone named {missing} in your library. Known people are: {}.",
                preview.join(", ")
            )
        };
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(q) = plan.semantic_query.as_deref() {
        parts.push(format!("\"{q}\""));
    }
    if let Some(loc) = plan.location_query.as_deref() {
        parts.push(format!("location \"{loc}\""));
    }
    if let Some(ex_loc) = plan.exclude_location_query.as_deref() {
        parts.push(format!("excluding location \"{ex_loc}\""));
    }
    if !plan.person_names.is_empty() {
        parts.push(format!("person {}", plan.person_names.join(" + ")));
    }
    if !plan.exclude_person_names.is_empty() {
        parts.push(format!(
            "excluding {}",
            plan.exclude_person_names.join(", ")
        ));
    }
    if let Some(hp) = plan.has_people {
        parts.push(if hp {
            "with people".to_string()
        } else {
            "without people".to_string()
        });
    }
    if let (Some(lo), Some(hi)) = (plan.min_faces, plan.max_faces) {
        parts.push(if lo == hi {
            format!("{lo} face(s)")
        } else {
            format!("{lo}–{hi} faces")
        });
    } else if plan.max_faces == Some(1) {
        parts.push("solo".to_string());
    } else if let Some(lo) = plan.min_faces {
        parts.push(format!("{lo}+ faces"));
    }
    if let Some(ocr) = plan.ocr_terms.as_deref() {
        parts.push(format!("text \"{ocr}\""));
    }
    if let Some(cam) = plan.camera_model.as_deref() {
        parts.push(format!("camera {cam}"));
    }
    if let Some(lens) = plan.lens_model.as_deref() {
        parts.push(format!("lens {lens}"));
    }
    if let (Some(lo), Some(hi)) = (plan.focal_length_min_mm, plan.focal_length_max_mm) {
        if (lo - hi).abs() < 0.05 {
            parts.push(format!("{lo:.0}mm"));
        } else {
            parts.push(format!("{lo:.0}–{hi:.0}mm"));
        }
    } else if let Some(lo) = plan.focal_length_min_mm {
        parts.push(format!("≥ {lo:.0}mm"));
    } else if let Some(hi) = plan.focal_length_max_mm {
        parts.push(format!("≤ {hi:.0}mm"));
    }
    if let (Some(lo), Some(hi)) = (plan.aperture_min_f, plan.aperture_max_f) {
        if (lo - hi).abs() < 0.05 {
            parts.push(format!("f/{lo:.1}"));
        } else {
            parts.push(format!("f/{lo:.1}–f/{hi:.1}"));
        }
    } else if let Some(lo) = plan.aperture_min_f {
        parts.push(format!("≥ f/{lo:.1}"));
    } else if let Some(hi) = plan.aperture_max_f {
        parts.push(format!("≤ f/{hi:.1}"));
    }
    if let (Some(lo), Some(hi)) = (plan.iso_min, plan.iso_max) {
        if lo == hi {
            parts.push(format!("ISO {lo}"));
        } else {
            parts.push(format!("ISO {lo}–{hi}"));
        }
    } else if let Some(lo) = plan.iso_min {
        parts.push(format!("ISO ≥ {lo}"));
    } else if let Some(hi) = plan.iso_max {
        parts.push(format!("ISO ≤ {hi}"));
    }
    if plan.shutter_speed_min_sec.is_some() || plan.shutter_speed_max_sec.is_some() {
        parts.push("that shutter speed range".to_string());
    }
    if let Some(orient) = plan.orientation.as_deref() {
        parts.push(format!("{orient} orientation"));
    }
    if !plan.tags.is_empty() {
        parts.push(format!("tag {}", plan.tags.join(", ")));
    }
    if let Some(kind) = plan.kind {
        parts.push(format!("kind {kind:?}").to_ascii_lowercase());
    }
    if plan.favorite == Some(true) {
        parts.push("favorites".to_string());
    }
    if plan.date_from_ms.is_some() || plan.date_to_ms.is_some() {
        parts.push("that date range".to_string());
    }
    if parts.is_empty() {
        "I didn't find any matching photos or videos for those filters. Try broadening your request or asking what's in your library.".to_string()
    } else {
        format!(
            "I didn't find any photos or videos matching {}. Try removing a filter or broadening your search.",
            parts.join(", ")
        )
    }
}

fn build_zero_hit_relaxation_suggestions(
    plan: &RouterPlan,
    summary: &repo::search::LibraryChatSummary,
) -> Vec<String> {
    let mut out = Vec::new();
    if plan.location_query.is_some() {
        out.push("Remove the location filter".to_string());
    }
    if plan.date_from_ms.is_some() || plan.date_to_ms.is_some() {
        out.push("Remove the date filter".to_string());
    }
    if !plan.person_names.is_empty() {
        out.push("Remove the person filter".to_string());
    }
    if out.len() < 2 {
        if let Some((first_folder, _)) = summary.folders.first() {
            out.push(format!("Show me {first_folder}"));
        }
    }
    if out.len() < 3 {
        out.push("What's in my library?".to_string());
    }
    out.truncate(3);
    out
}

/// `POST /api/chat/sessions/{id}/messages` (AuthSession + RbacContext; multi-turn RBAC-enforced hybrid search + optional Tier 3 VLM reranker).
pub async fn send_message(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
    ApiJson(req): ApiJson<SendChatMessageRequest>,
) -> Result<Json<SendChatMessageResponse>, AppError> {
    let content = req.content.trim().to_string();
    if content.is_empty() {
        return Err(AppError::validation(
            "Chat message content cannot be empty.",
        ));
    }

    let started = Instant::now();
    let user_ts_ms = Utc::now().timestamp_millis();
    let user_id = rbac.auth.user.id.clone();
    let scope = rbac.scope.clone();
    let sid_read = session_id.clone();
    let default_paths = state.default_paths.clone();

    let (
        session,
        recent_turns,
        visible_people,
        known_cameras,
        known_lenses,
        library_summary,
        ai_state,
        settings,
    ) = state
        .db
        .read(move |conn| {
            let Some(session) = repo::chat::get_chat_session(conn, &user_id, &sid_read)? else {
                return Err(DbError::NotFound("Chat session not found.".to_string()));
            };
            let stored = repo::chat::list_chat_messages(conn, &sid_read)?;
            let recent: Vec<(ChatRole, String)> = stored
                .iter()
                .rev()
                .take(RECENT_TURNS_WINDOW)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|m| (m.role, m.content.clone()))
                .collect();
            let people = repo::people::list_people(conn, &scope)?;
            let (cameras, lenses) = repo::search::list_distinct_gear(conn, &scope)?;
            let library_summary = repo::search::load_library_chat_summary(conn, &scope)?;
            let ai_state = repo::ai::get_ai_index_state(conn)?;
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((
                session,
                recent,
                people,
                cameras,
                lenses,
                library_summary,
                ai_state,
                settings,
            ))
        })
        .await?;

    let known_person_names: Vec<String> = visible_people
        .iter()
        .filter_map(|p| p.name.as_deref().map(str::trim))
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect();

    let library_context = LibraryChatContext {
        total_assets: library_summary.total_assets,
        favorite_count: library_summary.favorite_count,
        kind_counts: library_summary.kind_counts.clone(),
        date_range: library_summary.date_range.clone(),
        known_folders: library_summary
            .folders
            .iter()
            .map(|(name, cnt)| format!("{name} ({cnt})"))
            .collect(),
        top_tags: library_summary.top_tags.clone(),
        person_counts: library_summary.person_counts.clone(),
        camera_counts: library_summary.camera_counts.clone(),
        lens_counts: library_summary.lens_counts.clone(),
        top_places: library_summary.top_places.clone(),
        year_counts: library_summary.year_counts.clone(),
        top_months: library_summary.top_months.clone(),
    };

    // Apply client's timezone offset (minutes east of UTC, clamped to valid ±14h) so relative date
    // cues like "today", "yesterday", and "last weekend" match the user's local calendar date.
    let clamped_tz_offset_min = req.tz_offset_minutes.map(|m| m.clamp(-14 * 60, 14 * 60));
    let local_now = match clamped_tz_offset_min {
        Some(offset_min) => Utc::now() + chrono::Duration::minutes(i64::from(offset_min)),
        None => Utc::now(),
    };

    let turn_input = ChatTurnInput {
        user_message: content.clone(),
        prior_filters: session.active_filters.clone(),
        recent_turns,
        known_person_names,
        known_cameras: known_cameras.clone(),
        known_lenses: known_lenses.clone(),
        library: library_context,
        current_date_utc: local_now,
    };

    let active_chat_tier = ai_state.active_chat_tier;
    let pipeline = Arc::clone(state.ai.pipeline());
    let mut plan = tokio::task::spawn_blocking(move || {
        pipeline.chat.route_turn(active_chat_tier, &turn_input)
    })
    .await
    .map_err(|e| AppError::internal(format!("Chat router worker panicked: {e}")))?
    .map_err(|e| AppError::internal(format!("Chat query routing failed: {e}")))?;

    // Convert newly-produced calendar-date UTC midnight bounds into the user's actual local
    // midnight UTC timestamps (e.g. UTC-8 "today" covers 08:00Z..07:59:59.999Z next day).
    if let Some(offset_min) = clamped_tz_offset_min {
        if offset_min != 0 {
            let offset_ms = i64::from(offset_min) * 60_000;
            if plan.date_from_ms != session.active_filters.date_from_ms {
                if let Some(ms) = plan.date_from_ms {
                    plan.date_from_ms = Some(ms.saturating_sub(offset_ms));
                }
            }
            if plan.date_to_ms != session.active_filters.date_to_ms {
                if let Some(ms) = plan.date_to_ms {
                    plan.date_to_ms = Some(ms.saturating_sub(offset_ms));
                }
            }
        }
    }

    let is_answer_only = plan.intent.as_deref() == Some("answer");
    let resolved_people = if is_answer_only {
        ResolvedPeopleOutcome {
            person_ids: None,
            unmatched: Vec::new(),
            ambiguous: Vec::new(),
        }
    } else {
        resolve_rbac_people(&plan.person_names, &visible_people)
    };
    let unmatched_people = resolved_people.unmatched;
    let ambiguous_people = resolved_people.ambiguous;
    let person_ids = resolved_people.person_ids;
    let exclude_person_ids = if is_answer_only || plan.exclude_person_names.is_empty() {
        None
    } else {
        let excl = resolve_rbac_people(&plan.exclude_person_names, &visible_people);
        let valid: Vec<String> = excl
            .person_ids
            .unwrap_or_default()
            .into_iter()
            .filter(|id| id != UNMATCHED_PERSON_SENTINEL)
            .collect();
        if valid.is_empty() {
            None
        } else {
            Some(valid)
        }
    };
    let search_limit = req
        .limit
        .unwrap_or(DEFAULT_CHAT_LIMIT)
        .clamp(1, MAX_CHAT_LIMIT) as u32;
    let search_req =
        build_search_request_from_plan(&plan, person_ids, exclude_person_ids, search_limit);

    let has_any_search_criteria = !is_answer_only
        && (search_req.query.is_some()
            || search_req.ocr_query.is_some()
            || search_req.person_ids.is_some()
            || search_req.exclude_person_ids.is_some()
            || search_req.has_people.is_some()
            || search_req.min_faces.is_some()
            || search_req.max_faces.is_some()
            || search_req.tags.is_some()
            || search_req.camera_model.is_some()
            || search_req.lens_model.is_some()
            || search_req.focal_length_min_mm.is_some()
            || search_req.focal_length_max_mm.is_some()
            || search_req.aperture_min_f.is_some()
            || search_req.aperture_max_f.is_some()
            || search_req.iso_min.is_some()
            || search_req.iso_max.is_some()
            || search_req.shutter_speed_min_sec.is_some()
            || search_req.shutter_speed_max_sec.is_some()
            || search_req.orientation.is_some()
            || search_req.sort_by.is_some()
            || search_req.geo_query.is_some()
            || search_req.exclude_geo_query.is_some()
            || search_req.kind.is_some()
            || search_req.exclude_kind.is_some()
            || search_req.favorite.is_some()
            || search_req.date_from_ms.is_some()
            || search_req.date_to_ms.is_some());

    let mut search_resp = if has_any_search_criteria {
        let primary = execute_hybrid_search(&state, &rbac, search_req.clone()).await?;
        // Zero-hit third-party lens brand-prefix fallback (e.g. user asks "Sigma 18-50mm" while
        // EXIF lens_model stores "18-50mm F2.8 DC DN | Contemporary 021").
        if primary.total_count == 0 {
            if let Some(stripped_lens) = search_req
                .lens_model
                .as_deref()
                .and_then(strip_third_party_lens_brand)
            {
                let mut retry_req = search_req;
                retry_req.lens_model = Some(stripped_lens);
                execute_hybrid_search(&state, &rbac, retry_req).await?
            } else {
                primary
            }
        } else {
            primary
        }
    } else {
        truongphoto_core::SearchResponse {
            items: Vec::new(),
            total_count: 0,
            planner_mode: truongphoto_core::PlannerMode::SqlOnly,
            elapsed_ms: 0,
        }
    };
    let mut reranked = false;

    if active_chat_tier == ChatTier::Tier3
        && ai_state.vlm_rerank_enabled
        && search_resp.items.len() >= 2
        && should_trigger_vlm_rerank(&content, &plan)
    {
        let thumbs_dir = PathBuf::from(&settings.thumbnails_dir);
        let candidate_meta: Vec<(String, f32, Option<String>, Option<String>)> = search_resp
            .items
            .iter()
            .take(MAX_VLM_RERANK_CANDIDATES)
            .map(|hit| {
                (
                    hit.asset.id.clone(),
                    hit.score,
                    hit.asset.thumb_rel_path.clone(),
                    build_rerank_caption_hint(hit),
                )
            })
            .collect();

        let pipeline = Arc::clone(state.ai.pipeline());
        let query_for_vlm = content.clone();

        let rerank_outcome = tokio::task::spawn_blocking(move || {
            let candidates: Vec<RerankCandidate> = candidate_meta
                .into_iter()
                .map(|(asset_id, score, thumb_rel, caption_hint)| {
                    let thumb_bytes = thumb_rel
                        .as_deref()
                        .and_then(|rel| safe_join_relative(&thumbs_dir, rel))
                        .and_then(|p| std::fs::read(p).ok())
                        .and_then(|raw| transcode_thumb_to_jpeg_for_vlm(&raw))
                        .unwrap_or_default();
                    RerankCandidate {
                        asset_id,
                        score,
                        thumb_bytes,
                        caption_hint,
                    }
                })
                .collect();
            pipeline
                .chat
                .rerank_thumbnails(ChatTier::Tier3, &query_for_vlm, &candidates)
        })
        .await
        .map_err(|e| AppError::internal(format!("VLM reranker worker panicked: {e}")))?;

        match rerank_outcome {
            Ok(Some(rerank_res)) => {
                apply_vlm_rerank_order(&mut search_resp.items, &rerank_res.ordered_asset_ids);
                reranked = true;
                if let Some(obs) = rerank_res
                    .observation
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if plan.assistant_reply.trim().is_empty() {
                        plan.assistant_reply = obs.to_string();
                    } else {
                        plan.assistant_reply = format!("{} {}", plan.assistant_reply.trim(), obs);
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "Tier 3 VLM thumbnail reranking failed; keeping hybrid order");
            }
        }
    }

    let known_people_for_msg: Vec<String> = visible_people
        .iter()
        .filter_map(|p| p.name.as_deref().map(str::trim))
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect();

    if is_answer_only {
        let lower_reply = plan.assistant_reply.trim().to_ascii_lowercase();
        let needs_fallback = lower_reply.is_empty()
            || lower_reply.contains("didn't find")
            || lower_reply.contains("no match")
            || lower_reply.starts_with("here are your photos")
            || lower_reply.starts_with("showing photos");
        if needs_fallback {
            plan.assistant_reply = synthesize_grounded_answer(
                &content,
                &library_summary,
                &known_cameras,
                &known_lenses,
            );
        }
        if plan.follow_up_suggestions.is_empty() {
            if let Some((first_person, _)) = library_summary.person_counts.first() {
                plan.follow_up_suggestions
                    .push(format!("Show me photos of {first_person}"));
            }
            if let Some((first_folder, _)) = library_summary.folders.first() {
                plan.follow_up_suggestions
                    .push(format!("Show me {first_folder}"));
            }
            plan.follow_up_suggestions
                .push("Show me my best photos".to_string());
        }
    } else if plan.assistant_reply.trim().is_empty() {
        plan.assistant_reply = if search_resp.items.is_empty() {
            describe_zero_hit_filters(
                &plan,
                &unmatched_people,
                &ambiguous_people,
                &known_people_for_msg,
            )
        } else {
            format!("Found {} matching item(s).", search_resp.total_count)
        };
    } else if has_any_search_criteria && search_resp.items.is_empty() {
        let lower = plan.assistant_reply.to_ascii_lowercase();
        if !ambiguous_people.is_empty()
            || !unmatched_people.is_empty()
            || (!lower.contains("no match")
                && !lower.contains("couldn't find")
                && !lower.contains("didn't find")
                && !lower.contains("none found"))
        {
            plan.assistant_reply = describe_zero_hit_filters(
                &plan,
                &unmatched_people,
                &ambiguous_people,
                &known_people_for_msg,
            );
        }
        if let Some((_, candidates)) = ambiguous_people.first() {
            plan.follow_up_suggestions = candidates
                .iter()
                .take(3)
                .map(|full_name| format!("Show me photos of {full_name}"))
                .collect();
        } else {
            plan.follow_up_suggestions =
                build_zero_hit_relaxation_suggestions(&plan, &library_summary);
        }
    } else if has_any_search_criteria && !search_resp.items.is_empty() {
        // Ground exact count / existence questions ("how many photos of Sebastian", "do I have any ...")
        // with the real SQL/hybrid total_count instead of leaving a generic or guessed reply.
        let lp = content.to_ascii_lowercase();
        if lp.contains("how many") || lp.starts_with("do i have") || lp.starts_with("are there any")
        {
            let count_str = search_resp.total_count.to_string();
            let has_exact_count_token = plan
                .assistant_reply
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|tok| tok == count_str);
            if !has_exact_count_token {
                let prefix = format!(
                    "Found {} matching {}.",
                    search_resp.total_count,
                    if search_resp.total_count == 1 {
                        "item"
                    } else {
                        "items"
                    }
                );
                if plan.assistant_reply.trim().is_empty() {
                    plan.assistant_reply = prefix;
                } else {
                    plan.assistant_reply = format!("{prefix} {}", plan.assistant_reply.trim());
                }
            }
        }
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
    let asst_ts_ms = Utc::now().timestamp_millis().max(user_ts_ms + 1);
    let hit_refs: Vec<ChatHitRef> = search_resp
        .items
        .iter()
        .map(|hit| ChatHitRef {
            asset_id: hit.asset.id.clone(),
            score: hit.score,
            timestamp_sec: hit.timestamp_sec,
        })
        .collect();

    let updated_title = if session.title == "New Chat" {
        Some(auto_title_from_prompt(&content))
    } else {
        None
    };

    let lower_prompt = content.to_ascii_lowercase();
    let explicit_reset = [
        "start over",
        "reset",
        "clear filter",
        "clear all",
        "clear the",
        "clear search",
        "new search",
        "nevermind",
        "never mind",
        "forget that",
        "scratch that",
        "remove the",
        "remove filter",
        "drop the",
    ]
    .iter()
    .any(|kw| lower_prompt.contains(kw))
        || lower_prompt.trim() == "clear";
    let mut persisted_filters = if is_answer_only
        || (!has_any_search_criteria && !explicit_reset && !plan.clear_prior_filters)
    {
        session.active_filters.clone()
    } else {
        plan.clone()
    };
    // Never persist unmatched or ambiguous person names into session active_filters so a typo
    // or ambiguous name cannot silently poison subsequent conversational turns.
    if !unmatched_people.is_empty() || !ambiguous_people.is_empty() {
        let bad_names: std::collections::HashSet<String> = unmatched_people
            .iter()
            .chain(ambiguous_people.iter().map(|(raw, _)| raw))
            .map(|s| s.trim().to_ascii_lowercase())
            .collect();
        persisted_filters
            .person_names
            .retain(|name| !bad_names.contains(&name.trim().to_ascii_lowercase()));
    }

    let user_id_write = rbac.auth.user.id.clone();
    let sid_write = session_id.clone();
    let user_content_write = content.clone();
    let asst_content_write = plan.assistant_reply.clone();
    let plan_write = plan.clone();
    let persisted_filters_write = persisted_filters.clone();
    let planner_mode = search_resp.planner_mode;

    let (stored_user, stored_asst) = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let user_msg = repo::chat::append_chat_message(
                &tx,
                &sid_write,
                ChatRole::User,
                &user_content_write,
                None,
                None,
                false,
                None,
                None,
                user_ts_ms,
            )?;
            let asst_msg = repo::chat::append_chat_message(
                &tx,
                &sid_write,
                ChatRole::Assistant,
                &asst_content_write,
                Some(&plan_write),
                Some(&hit_refs),
                reranked,
                Some(planner_mode),
                Some(elapsed_ms),
                asst_ts_ms,
            )?;
            repo::chat::update_chat_session_filters(
                &tx,
                &user_id_write,
                &sid_write,
                &persisted_filters_write,
                updated_title.as_deref(),
                asst_ts_ms,
            )?;
            tx.commit()?;
            Ok((user_msg, asst_msg))
        })
        .await?;

    let user_message = ChatMessageView {
        id: stored_user.id,
        session_id: stored_user.session_id,
        role: stored_user.role,
        content: stored_user.content,
        plan: None,
        hits: Vec::new(),
        reranked: false,
        planner_mode: None,
        elapsed_ms: None,
        created_at_ms: stored_user.created_at_ms,
    };

    let assistant_message = ChatMessageView {
        id: stored_asst.id,
        session_id: stored_asst.session_id,
        role: stored_asst.role,
        content: stored_asst.content,
        plan: stored_asst.plan,
        hits: search_resp.items.clone(),
        reranked,
        planner_mode: Some(planner_mode),
        elapsed_ms: Some(elapsed_ms),
        created_at_ms: stored_asst.created_at_ms,
    };

    Ok(Json(SendChatMessageResponse {
        user_message,
        assistant_message,
        active_filters: persisted_filters,
        hits: search_resp.items,
        total_count: search_resp.total_count,
        planner_mode,
        reranked,
        elapsed_ms,
    }))
}

/// Converts WebP/PNG thumbnails into JPEG bytes so `llama-server`'s `stb_image` multimodal decoder
/// can reliably parse every Tier 3 VLM rerank candidate.
fn transcode_thumb_to_jpeg_for_vlm(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(raw.to_vec());
    }
    let img = image::load_from_memory(raw).ok()?;
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    enc.encode_image(&rgb).ok()?;
    Some(out)
}

#[derive(Debug, Serialize)]
pub struct ChatWarmupResponse {
    pub active_chat_tier: ChatTier,
    pub loaded: bool,
    pub warming: bool,
}

/// `POST /api/chat/warmup` — triggers non-blocking background warmup of the active local chat LLM.
pub async fn warmup_chat(
    _auth: AuthSession,
    State(state): State<Arc<AppState>>,
) -> Result<Json<ChatWarmupResponse>, AppError> {
    let ai_state = state.db.read(repo::ai::get_ai_index_state).await?;
    let active_tier = ai_state.active_chat_tier;
    let pipeline = Arc::clone(state.ai.pipeline());
    let already_loaded = pipeline.chat.is_loaded_for_tier(active_tier);

    if active_tier != ChatTier::Tier0 && !already_loaded {
        let pipe_bg = Arc::clone(&pipeline);
        tokio::task::spawn_blocking(move || {
            let _ = pipe_bg.chat.warmup(active_tier);
        });
    }

    Ok(Json(ChatWarmupResponse {
        active_chat_tier: active_tier,
        loaded: already_loaded,
        warming: active_tier != ChatTier::Tier0 && !already_loaded,
    }))
}
