use crate::error::{ApiJson, AppError};
use crate::extractors::RbacContext;
use crate::state::AppState;
use axum::{extract::State, Json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use truongphoto_ai::{collapse_video_keyframes, overfetch_k, AiError, KeyframeHit};
use truongphoto_core::{
    AssetSummary, PlannerMode, RbacScope, SearchHit, SearchMode, SearchRequest, SearchResponse,
};
use truongphoto_db::repo;

const DEFAULT_LIMIT: usize = 60;
const MAX_LIMIT: usize = 500;
const MIN_SEMANTIC_SCORE: f32 = 0.08;

struct PendingHit {
    asset_id: String,
    score: f32,
    timestamp_sec: Option<f64>,
    match_reasons: Vec<String>,
    ocr_snippet: Option<String>,
}

fn compare_sort_keys(
    sort_by: &str,
    a_id: &str,
    b_id: &str,
    a_score: f32,
    b_score: f32,
    keys: &HashMap<String, repo::search::AssetSortKey>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ka = keys.get(a_id);
    let kb = keys.get(b_id);
    let cmp_opt_f64 = |oa: Option<f64>, ob: Option<f64>, asc: bool| -> Ordering {
        match (oa, ob) {
            (Some(x), Some(y)) => {
                if asc {
                    x.total_cmp(&y)
                } else {
                    y.total_cmp(&x)
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    };
    let cmp_opt_i64 = |oa: Option<i64>, ob: Option<i64>, asc: bool| -> Ordering {
        match (oa, ob) {
            (Some(x), Some(y)) => {
                if asc {
                    x.cmp(&y)
                } else {
                    y.cmp(&x)
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    };

    let primary = match sort_by {
        "capture_desc" => match (ka, kb) {
            (Some(a), Some(b)) => b.capture_at_ms.cmp(&a.capture_at_ms),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        "capture_asc" => match (ka, kb) {
            (Some(a), Some(b)) => a.capture_at_ms.cmp(&b.capture_at_ms),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        "focal_asc" => cmp_opt_f64(
            ka.and_then(|k| k.focal_length_mm),
            kb.and_then(|k| k.focal_length_mm),
            true,
        ),
        "focal_desc" => cmp_opt_f64(
            ka.and_then(|k| k.focal_length_mm),
            kb.and_then(|k| k.focal_length_mm),
            false,
        ),
        "iso_desc" => cmp_opt_i64(ka.and_then(|k| k.iso), kb.and_then(|k| k.iso), false),
        "aperture_asc" => cmp_opt_f64(
            ka.and_then(|k| k.aperture_f),
            kb.and_then(|k| k.aperture_f),
            true,
        ),
        "shutter_asc" => cmp_opt_f64(
            ka.and_then(|k| k.shutter_speed_sec),
            kb.and_then(|k| k.shutter_speed_sec),
            true,
        ),
        "shutter_desc" => cmp_opt_f64(
            ka.and_then(|k| k.shutter_speed_sec),
            kb.and_then(|k| k.shutter_speed_sec),
            false,
        ),
        _ => Ordering::Equal,
    };

    primary
        .then_with(|| b_score.total_cmp(&a_score))
        .then_with(|| a_id.cmp(b_id))
}

async fn materialize_ocr_hits(
    state: &AppState,
    scope: RbacScope,
    mut ocr_hits: Vec<(String, f32, String)>,
    sort_by: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<(Vec<SearchHit>, i64), AppError> {
    if let Some(sort_key) = sort_by.map(str::trim).filter(|s| !s.is_empty()) {
        let sort_norm = sort_key.to_ascii_lowercase();
        let all_ids: Vec<String> = ocr_hits.iter().map(|(id, _, _)| id.clone()).collect();
        let scope_s = scope.clone();
        let sort_keys = state
            .db
            .read(move |conn| repo::search::load_asset_sort_keys(conn, &scope_s, &all_ids))
            .await?;
        ocr_hits.sort_unstable_by(|a, b| {
            compare_sort_keys(&sort_norm, &a.0, &b.0, a.1, b.1, &sort_keys)
        });
    }

    let total_count = ocr_hits.len() as i64;
    if offset >= ocr_hits.len() {
        return Ok((Vec::new(), total_count));
    }

    let end = offset.saturating_add(limit).min(ocr_hits.len());
    let page = &ocr_hits[offset..end];
    let page_ids: Vec<String> = page.iter().map(|(id, _, _)| id.clone()).collect();

    let summaries = state
        .db
        .read(move |conn| repo::search::load_asset_summaries_by_ids(conn, &scope, &page_ids))
        .await?;

    let mut items = Vec::with_capacity(page.len());
    for (asset_id, score, snippet) in page {
        if let Some(asset) = summaries.get(asset_id).cloned() {
            items.push(SearchHit {
                asset,
                score: *score,
                timestamp_sec: None,
                match_reasons: vec!["OCR".to_string()],
                ocr_snippet: Some(snippet.clone()),
            });
        }
    }
    Ok((items, total_count))
}

fn sql_assets_to_hits(assets: Vec<AssetSummary>, has_query: bool) -> Vec<SearchHit> {
    let reasons = if has_query {
        vec!["Filter".to_string()]
    } else {
        Vec::new()
    };
    assets
        .into_iter()
        .map(|asset| SearchHit {
            asset,
            score: 1.0,
            timestamp_sec: None,
            match_reasons: reasons.clone(),
            ocr_snippet: None,
        })
        .collect()
}

/// `POST /api/search` (AuthSession + RbacContext; strict RBAC across SQL filters, RoaringBitmap vectors, and FTS5 OCR).
pub async fn search_library(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<SearchRequest>,
) -> Result<Json<SearchResponse>, AppError> {
    let resp = execute_hybrid_search(&state, &rbac, req).await?;
    Ok(Json(resp))
}

const MAX_OFFSET: usize = 5_000;

fn validate_search_request(req: &SearchRequest) -> Result<(), AppError> {
    for (name, val) in [
        ("focal_length_min_mm", req.focal_length_min_mm),
        ("focal_length_max_mm", req.focal_length_max_mm),
        ("aperture_min_f", req.aperture_min_f),
        ("aperture_max_f", req.aperture_max_f),
        ("shutter_speed_min_sec", req.shutter_speed_min_sec),
        ("shutter_speed_max_sec", req.shutter_speed_max_sec),
    ] {
        if let Some(v) = val {
            if !v.is_finite() || v <= 0.0 {
                return Err(AppError::validation(format!(
                    "{name} must be a finite positive number."
                )));
            }
        }
    }
    if let Some(orient) = req
        .orientation
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !matches!(
            orient.to_ascii_lowercase().as_str(),
            "portrait" | "vertical" | "landscape" | "horizontal" | "square"
        ) {
            return Err(AppError::validation(
                "orientation must be portrait, landscape, or square.",
            ));
        }
    }
    if let Some(sort) = req
        .sort_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !matches!(
            sort.to_ascii_lowercase().as_str(),
            "capture_desc"
                | "capture_asc"
                | "focal_asc"
                | "focal_desc"
                | "iso_desc"
                | "aperture_asc"
                | "shutter_asc"
                | "shutter_desc"
        ) {
            return Err(AppError::validation("unsupported sort_by value."));
        }
    }
    Ok(())
}

/// Shared RBAC-enforced Hybrid Search engine used by both `POST /api/search` and
/// `POST /api/chat/sessions/{id}/messages`.
pub(crate) async fn execute_hybrid_search(
    state: &Arc<AppState>,
    rbac: &RbacContext,
    mut req: SearchRequest,
) -> Result<SearchResponse, AppError> {
    validate_search_request(&req)?;
    let started = Instant::now();
    let limit = (req.limit.unwrap_or(DEFAULT_LIMIT as u32) as usize).clamp(1, MAX_LIMIT);
    let offset = (req.offset.unwrap_or(0) as usize).min(MAX_OFFSET);
    req.offset = Some(offset as u32);
    let trimmed_query = req
        .query
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let trimmed_ocr_query = req
        .ocr_query
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if req.mode == SearchMode::Filters || trimmed_query.is_none() {
        let scope = rbac.scope.clone();
        let has_query = trimmed_query.is_some();
        let req_clone = req.clone();
        let (assets, total_count) = state
            .db
            .read(move |conn| repo::search::list_sql_filtered_assets(conn, &scope, &req_clone))
            .await?;

        return Ok(SearchResponse {
            items: sql_assets_to_hits(assets, has_query),
            total_count,
            planner_mode: PlannerMode::SqlOnly,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    let query_str = trimmed_query.expect("checked Some above");

    if req.mode == SearchMode::Ocr {
        let scope = rbac.scope.clone();
        let req_clone = req.clone();
        let q_clone = query_str.clone();
        let ocr_hits = state
            .db
            .read(move |conn| {
                let ai_state = repo::ai::get_ai_index_state(conn)?;
                let candidates = repo::search::build_filtered_candidates(
                    conn,
                    &scope,
                    &req_clone,
                    ai_state.active_vision_tier,
                )?;
                repo::search::search_ocr_fts(
                    conn,
                    &scope,
                    &q_clone,
                    Some(&candidates.asset_ids),
                    MAX_LIMIT,
                )
            })
            .await?;

        let (items, total_count) = materialize_ocr_hits(
            state,
            rbac.scope.clone(),
            ocr_hits,
            req.sort_by.as_deref(),
            offset,
            limit,
        )
        .await?;
        return Ok(SearchResponse {
            items,
            total_count,
            planner_mode: PlannerMode::SqlOnly,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    // Semantic or Hybrid mode
    let scope = rbac.scope.clone();
    let req_clone = req.clone();
    let q_clone = query_str.clone();
    let ocr_q_clone = trimmed_ocr_query.unwrap_or_else(|| query_str.clone());
    let want_ocr = req.mode == SearchMode::Hybrid;

    let (active_tier, candidates, ocr_hits, metadata_hits) = state
        .db
        .read(move |conn| {
            let ai_state = repo::ai::get_ai_index_state(conn)?;
            let tier = ai_state.active_vision_tier;
            let cands = repo::search::build_filtered_candidates(conn, &scope, &req_clone, tier)?;
            let ocr = if want_ocr {
                repo::search::search_ocr_fts(
                    conn,
                    &scope,
                    &ocr_q_clone,
                    Some(&cands.asset_ids),
                    MAX_LIMIT,
                )?
            } else {
                Vec::new()
            };
            let meta = if want_ocr {
                repo::search::search_metadata_matches(
                    conn, &scope, &req_clone, &q_clone, MAX_LIMIT,
                )?
            } else {
                Vec::new()
            };
            Ok((tier, cands, ocr, meta))
        })
        .await?;

    if candidates.vector_ids.is_empty() {
        if want_ocr && !ocr_hits.is_empty() {
            let (items, total_count) = materialize_ocr_hits(
                state,
                rbac.scope.clone(),
                ocr_hits,
                req.sort_by.as_deref(),
                offset,
                limit,
            )
            .await?;
            return Ok(SearchResponse {
                items,
                total_count,
                planner_mode: PlannerMode::SqlOnly,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }
        if want_ocr {
            let scope = rbac.scope.clone();
            let req_sql = req.clone();
            let (assets, total_count) = state
                .db
                .read(move |conn| repo::search::list_sql_filtered_assets(conn, &scope, &req_sql))
                .await?;
            return Ok(SearchResponse {
                items: sql_assets_to_hits(assets, true),
                total_count,
                planner_mode: PlannerMode::SqlOnly,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }
        return Ok(SearchResponse {
            items: Vec::new(),
            total_count: 0,
            planner_mode: PlannerMode::SqlOnly,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    let pipeline = Arc::clone(state.ai.pipeline());
    let q_embed = query_str.clone();
    let embed_res =
        tokio::task::spawn_blocking(move || pipeline.vision.embed_text(active_tier, &q_embed))
            .await
            .map_err(|e| AppError::internal(format!("Search text embed worker panicked: {e}")))?;

    let q_vec = match embed_res {
        Ok(v) => v,
        Err(AiError::ModelNotInstalled(_)) | Err(AiError::Ort(_)) => {
            // Gracefully degrade to OCR / SQL filters when vision model or ORT is not installed.
            if want_ocr && !ocr_hits.is_empty() {
                let (items, total_count) = materialize_ocr_hits(
                    state,
                    rbac.scope.clone(),
                    ocr_hits,
                    req.sort_by.as_deref(),
                    offset,
                    limit,
                )
                .await?;
                return Ok(SearchResponse {
                    items,
                    total_count,
                    planner_mode: PlannerMode::SqlOnly,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
            if want_ocr {
                let scope = rbac.scope.clone();
                let req_sql = req.clone();
                let (assets, total_count) = state
                    .db
                    .read(move |conn| {
                        repo::search::list_sql_filtered_assets(conn, &scope, &req_sql)
                    })
                    .await?;
                return Ok(SearchResponse {
                    items: sql_assets_to_hits(assets, true),
                    total_count,
                    planner_mode: PlannerMode::SqlOnly,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
            return Ok(SearchResponse {
                items: Vec::new(),
                total_count: 0,
                planner_mode: PlannerMode::SqlOnly,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }
        Err(e) => {
            return Err(AppError::internal(format!(
                "Vision text embedding failed: {e}"
            )));
        }
    };

    let active_index = state
        .ai
        .vision_index(active_tier)
        .map_err(|e| AppError::internal(format!("Failed to open vision vector index: {e}")))?;
    let has_explicit_sort = req.sort_by.as_deref().is_some_and(|s| !s.trim().is_empty());
    let fetch_k = if has_explicit_sort {
        overfetch_k(limit.saturating_add(offset))
            .saturating_mul(4)
            .max(1000)
    } else {
        overfetch_k(limit.saturating_add(offset))
    };
    let allowed_vids = candidates.vector_ids;

    let (raw_vec_hits, planner_mode) = tokio::task::spawn_blocking(move || {
        active_index.search_hybrid(&q_vec, &allowed_vids, fetch_k)
    })
    .await
    .map_err(|e| AppError::internal(format!("Vector search worker panicked: {e}")))?
    .map_err(|e| AppError::internal(format!("Vector search failed: {e}")))?;

    let vids: Vec<u32> = raw_vec_hits.iter().map(|(vid, _)| *vid).collect();
    let vid_map = state
        .db
        .read(move |conn| repo::ai::resolve_vision_vector_ids(conn, active_tier, &vids))
        .await?;

    let keyframe_hits: Vec<KeyframeHit> = raw_vec_hits
        .into_iter()
        .filter_map(|(vector_id, score)| {
            vid_map
                .get(&vector_id)
                .map(|(asset_id, timestamp_ms)| KeyframeHit {
                    vector_id,
                    asset_id: asset_id.clone(),
                    timestamp_ms: *timestamp_ms,
                    score,
                })
        })
        .collect();

    let collapse_limit = keyframe_hits.len().max(1);
    let collapsed = collapse_video_keyframes(keyframe_hits, collapse_limit);

    let meta_id_set: std::collections::HashSet<&str> = if want_ocr {
        metadata_hits.iter().map(String::as_str).collect()
    } else {
        std::collections::HashSet::new()
    };
    let has_meta_hits = !meta_id_set.is_empty();
    // When deterministic metadata (camera, lens, focal length, aperture, ISO, filename, folder, or
    // named person) matched the query, avoid polluting results with low-confidence SigLIP2 noise
    // for proper nouns / EXIF codes; still keep semantic boost on metadata matches and very strong
    // visual matches (>= 0.165).
    const META_SEMANTIC_EXTRA_MIN_SCORE: f32 = 0.165;

    let mut by_asset: HashMap<String, PendingHit> = HashMap::new();
    for hit in collapsed {
        if hit.score <= MIN_SEMANTIC_SCORE {
            continue;
        }
        if has_meta_hits
            && !meta_id_set.contains(hit.asset_id.as_str())
            && hit.score < META_SEMANTIC_EXTRA_MIN_SCORE
        {
            continue;
        }
        let pct = (hit.score.clamp(0.0, 1.0) * 100.0).round() as i32;
        by_asset.insert(
            hit.asset_id.clone(),
            PendingHit {
                asset_id: hit.asset_id,
                score: hit.score,
                timestamp_sec: hit.timestamp_sec,
                match_reasons: vec![format!("Semantic {pct}%")],
                ocr_snippet: None,
            },
        );
    }

    if want_ocr {
        for (asset_id, ocr_score, snippet) in ocr_hits {
            if let Some(existing) = by_asset.get_mut(&asset_id) {
                existing.score =
                    existing.score.max(ocr_score) + 0.25 * existing.score.min(ocr_score);
                existing.match_reasons.push("OCR".to_string());
                existing.ocr_snippet = Some(snippet);
            } else {
                by_asset.insert(
                    asset_id.clone(),
                    PendingHit {
                        asset_id,
                        score: ocr_score,
                        timestamp_sec: None,
                        match_reasons: vec!["OCR".to_string()],
                        ocr_snippet: Some(snippet),
                    },
                );
            }
        }

        // Fuse deterministic EXIF/camera/lens/filename/tag/person matches in Hybrid mode so queries
        // like "canon r7", "12mm", "f/1.4", "iso 6400", or "Portland Zoo" always surface exact matches.
        for (rank_idx, asset_id) in metadata_hits.into_iter().enumerate() {
            let meta_score = (0.85 - (rank_idx as f32) * 0.0005).max(0.50);
            if let Some(existing) = by_asset.get_mut(&asset_id) {
                existing.score =
                    existing.score.max(meta_score) + 0.15 * existing.score.min(meta_score);
                existing.match_reasons.push("Metadata".to_string());
            } else {
                by_asset.insert(
                    asset_id.clone(),
                    PendingHit {
                        asset_id,
                        score: meta_score,
                        timestamp_sec: None,
                        match_reasons: vec!["Metadata".to_string()],
                        ocr_snippet: None,
                    },
                );
            }
        }
    }

    let mut combined: Vec<PendingHit> = by_asset.into_values().collect();
    if let Some(sort_key) = req
        .sort_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let sort_norm = sort_key.to_ascii_lowercase();
        let all_ids: Vec<String> = combined.iter().map(|h| h.asset_id.clone()).collect();
        let scope_s = rbac.scope.clone();
        let sort_keys = state
            .db
            .read(move |conn| repo::search::load_asset_sort_keys(conn, &scope_s, &all_ids))
            .await?;
        combined.sort_unstable_by(|a, b| {
            compare_sort_keys(
                &sort_norm,
                &a.asset_id,
                &b.asset_id,
                a.score,
                b.score,
                &sort_keys,
            )
        });
    } else {
        combined.sort_unstable_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.asset_id.cmp(&b.asset_id))
        });
    }

    let total_count = combined.len() as i64;
    if offset >= combined.len() {
        return Ok(SearchResponse {
            items: Vec::new(),
            total_count,
            planner_mode,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    let end = offset.saturating_add(limit).min(combined.len());
    let page = &combined[offset..end];
    let page_ids: Vec<String> = page.iter().map(|h| h.asset_id.clone()).collect();
    let scope = rbac.scope.clone();

    let summaries = state
        .db
        .read(move |conn| repo::search::load_asset_summaries_by_ids(conn, &scope, &page_ids))
        .await?;

    let mut items = Vec::with_capacity(page.len());
    for hit in page {
        if let Some(asset) = summaries.get(&hit.asset_id).cloned() {
            items.push(SearchHit {
                asset,
                score: hit.score,
                timestamp_sec: hit.timestamp_sec,
                match_reasons: hit.match_reasons.clone(),
                ocr_snippet: hit.ocr_snippet.clone(),
            });
        }
    }

    Ok(SearchResponse {
        items,
        total_count,
        planner_mode,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}
