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
const MAX_VLM_RERANK_CANDIDATES: usize = 12;
const RECENT_TURNS_WINDOW: usize = 6;
const UNMATCHED_PERSON_SENTINEL: &str = "__unmatched_person__";

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

fn resolve_rbac_person_ids(
    requested_names: &[String],
    visible_people: &[PersonSummary],
) -> Option<Vec<String>> {
    if requested_names.is_empty() {
        return None;
    }

    let mut resolved = Vec::with_capacity(requested_names.len());
    let mut seen = HashSet::new();

    for raw_name in requested_names {
        let trimmed = raw_name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let matched_id = visible_people
            .iter()
            .find(|p| {
                p.name
                    .as_deref()
                    .is_some_and(|n| n.trim().eq_ignore_ascii_case(trimmed))
            })
            .or_else(|| {
                // Fall back to unique first-name / word-token match (e.g. "Nate" -> "Nate Higgers").
                let mut token_matches = visible_people.iter().filter(|p| {
                    p.name.as_deref().is_some_and(|n| {
                        n.split_whitespace()
                            .any(|word| word.eq_ignore_ascii_case(trimmed))
                    })
                });
                let first = token_matches.next();
                if token_matches.next().is_none() {
                    first
                } else {
                    None
                }
            })
            .map(|p| p.id.clone());

        let pid = matched_id.unwrap_or_else(|| UNMATCHED_PERSON_SENTINEL.to_string());
        if seen.insert(pid.clone()) {
            resolved.push(pid);
        }
    }

    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

fn build_search_request_from_plan(
    plan: &RouterPlan,
    person_ids: Option<Vec<String>>,
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
        has_people: plan.has_people,
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
        kind: plan.kind,
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

    let mut reordered = Vec::with_capacity(top_count + tail.len());
    for (asset_id, rerank_score) in ordered_ids {
        if let Some(mut hit) = by_id.remove(asset_id) {
            hit.score = *rerank_score;
            hit.match_reasons.push("VLM Reranked".to_string());
            reordered.push(hit);
        }
    }
    // Preserve original hybrid order for any remaining candidates not returned by the reranker.
    for asset_id in original_order {
        if let Some(hit) = by_id.remove(&asset_id) {
            reordered.push(hit);
        }
    }
    reordered.extend(tail);
    *hits = reordered;
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
        let cam_str = if cameras.is_empty() {
            "none detected".to_string()
        } else {
            cameras.join(", ")
        };
        let lens_str = if lenses.is_empty() {
            "none detected".to_string()
        } else {
            lenses.join(", ")
        };
        return format!("Your library uses camera(s): {cam_str}, with lens(es): {lens_str}.");
    }

    if lower.contains("folder")
        || lower.contains("trip")
        || lower.contains("event")
        || lower.contains("where")
        || lower.contains("place")
    {
        if folders_list.is_empty() {
            return format!("Your library contains {} items.", summary.total_assets);
        }
        return format!(
            "Your library has {} items across these collections: {}.",
            summary.total_assets,
            folders_list.join(", ")
        );
    }

    let mut parts = vec![format!("Your library has {} items", summary.total_assets)];
    if let Some((lo, hi)) = &summary.date_range {
        parts.push(format!("spanning {lo} to {hi}"));
    }
    let mut sentence = format!("{}.", parts.join(" "));
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

fn describe_zero_hit_filters(plan: &RouterPlan) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(q) = plan.semantic_query.as_deref() {
        parts.push(format!("\"{q}\""));
    }
    if let Some(loc) = plan.location_query.as_deref() {
        parts.push(format!("location \"{loc}\""));
    }
    if !plan.person_names.is_empty() {
        parts.push(format!("person {}", plan.person_names.join(" + ")));
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
    if parts.is_empty() {
        "I didn't find any matching photos or videos for those filters. Try broadening your request or asking what's in your library.".to_string()
    } else {
        format!(
            "I didn't find any photos or videos matching {}. Try a broader description or another filter.",
            parts.join(", ")
        )
    }
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
        date_range: library_summary.date_range.clone(),
        known_folders: library_summary
            .folders
            .iter()
            .map(|(name, cnt)| format!("{name} ({cnt})"))
            .collect(),
        top_tags: library_summary.top_tags.clone(),
        person_counts: library_summary.person_counts.clone(),
    };

    let turn_input = ChatTurnInput {
        user_message: content.clone(),
        prior_filters: session.active_filters.clone(),
        recent_turns,
        known_person_names,
        known_cameras: known_cameras.clone(),
        known_lenses: known_lenses.clone(),
        library: library_context,
        current_date_utc: Utc::now(),
    };

    let active_chat_tier = ai_state.active_chat_tier;
    let pipeline = Arc::clone(state.ai.pipeline());
    let mut plan = tokio::task::spawn_blocking(move || {
        pipeline.chat.route_turn(active_chat_tier, &turn_input)
    })
    .await
    .map_err(|e| AppError::internal(format!("Chat router worker panicked: {e}")))?
    .map_err(|e| AppError::internal(format!("Chat query routing failed: {e}")))?;

    let is_answer_only = plan.intent.as_deref() == Some("answer");
    let person_ids = if is_answer_only {
        None
    } else {
        resolve_rbac_person_ids(&plan.person_names, &visible_people)
    };
    let search_limit = req
        .limit
        .unwrap_or(DEFAULT_CHAT_LIMIT)
        .clamp(1, MAX_CHAT_LIMIT) as u32;
    let search_req = build_search_request_from_plan(&plan, person_ids, search_limit);

    let has_any_search_criteria = !is_answer_only
        && (search_req.query.is_some()
            || search_req.ocr_query.is_some()
            || search_req.person_ids.is_some()
            || search_req.has_people.is_some()
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
            || search_req.kind.is_some()
            || search_req.favorite.is_some()
            || search_req.date_from_ms.is_some()
            || search_req.date_to_ms.is_some());

    let mut search_resp = if has_any_search_criteria {
        execute_hybrid_search(&state, &rbac, search_req).await?
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
        && !search_resp.items.is_empty()
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
            describe_zero_hit_filters(&plan)
        } else {
            format!("Found {} matching item(s).", search_resp.total_count)
        };
    } else if has_any_search_criteria && search_resp.items.is_empty() {
        let lower = plan.assistant_reply.to_ascii_lowercase();
        if !lower.contains("no match")
            && !lower.contains("couldn't find")
            && !lower.contains("didn't find")
            && !lower.contains("none found")
        {
            plan.assistant_reply = describe_zero_hit_filters(&plan);
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
        "new search",
    ]
    .iter()
    .any(|kw| lower_prompt.contains(kw));
    let persisted_filters = if !has_any_search_criteria && !explicit_reset {
        session.active_filters.clone()
    } else {
        plan.clone()
    };

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
        active_filters: plan,
        hits: search_resp.items,
        total_count: search_resp.total_count,
        planner_mode,
        reranked,
        elapsed_ms,
    }))
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
    let already_loaded = pipeline.chat.is_loaded();

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
