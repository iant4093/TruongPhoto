use crate::error::{ApiJson, AppError};
use crate::extractors::RequireAdmin;
use crate::state::AppState;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use truongphoto_ai::{
    find_model_spec, is_model_installed, model_dir, AiError, ModelDownloader, MODEL_MANIFEST,
};
use truongphoto_core::{
    AiIndexStateView, AiModelStatus, AiModelView, AiModelsDashboardResponse, JobStatus,
    ReindexAiRequest, ReindexScope, SetAiTierRequest, SetChatTierRequest, UpdateAiSettingsRequest,
    WsServerEvent,
};
use truongphoto_db::repo;

/// `GET /api/admin/ai/models` (RequireAdmin).
pub async fn list_ai_models(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AiModelsDashboardResponse>, AppError> {
    let (ai_state, db_rows, queue) = state
        .db
        .read(move |conn| {
            let st = repo::ai::get_ai_index_state(conn)?;
            let rows = repo::ai::list_ai_model_rows(conn)?;
            let queue = repo::ai::count_ai_queue_stats(
                conn,
                st.active_vision_tier,
                st.staging_vision_tier,
            )?;
            Ok((st, rows, queue))
        })
        .await?;

    let row_map: HashMap<String, repo::ai::AiModelRow> = db_rows
        .into_iter()
        .map(|r| (r.model_id.clone(), r))
        .collect();

    let models_dir = state.ai.models_dir();
    let models: Vec<AiModelView> = MODEL_MANIFEST
        .iter()
        .map(|spec| {
            let disk_installed = is_model_installed(models_dir, spec);
            let active_dl = state.ai.is_downloading(spec.id);

            let (status, bytes_total, bytes_downloaded, error_message, updated_at_ms) =
                if let Some(row) = row_map.get(spec.id) {
                    let effective_status = if active_dl {
                        AiModelStatus::Downloading
                    } else if row.status == AiModelStatus::Downloading {
                        if disk_installed {
                            AiModelStatus::Installed
                        } else {
                            AiModelStatus::NotInstalled
                        }
                    } else if row.status == AiModelStatus::Installed && !disk_installed {
                        AiModelStatus::NotInstalled
                    } else {
                        row.status
                    };
                    (
                        effective_status,
                        if row.bytes_total > 0 {
                            row.bytes_total
                        } else {
                            spec.approx_bytes
                        },
                        row.bytes_downloaded,
                        row.error_message.clone(),
                        row.updated_at_ms,
                    )
                } else if disk_installed {
                    (
                        AiModelStatus::Installed,
                        spec.approx_bytes,
                        spec.approx_bytes,
                        None,
                        0,
                    )
                } else {
                    (AiModelStatus::NotInstalled, spec.approx_bytes, 0, None, 0)
                };

            AiModelView {
                id: spec.id.to_string(),
                name: spec.name.to_string(),
                role: spec.role.to_string(),
                license: spec.license.to_string(),
                hf_repo: spec.hf_repo.to_string(),
                tier: spec.tier,
                approx_bytes: spec.approx_bytes,
                status,
                bytes_total,
                bytes_downloaded,
                error_message,
                updated_at_ms,
            }
        })
        .collect();

    let ort_available = state.ai.pipeline().ort.ort_available();
    let ort_ep = state.ai.pipeline().ort.ep_name().to_string();

    Ok(Json(AiModelsDashboardResponse {
        ort_available,
        ort_ep,
        hardware: state.hardware.clone(),
        state: ai_state,
        models,
        queue,
    }))
}

/// `POST /api/admin/ai/models/{id}/download` (RequireAdmin -> `202 Accepted`).
pub async fn download_ai_model(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(model_id): AxumPath<String>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let spec = find_model_spec(&model_id)
        .ok_or_else(|| AppError::not_found(format!("Unknown AI model '{model_id}'.")))?;

    let cancel_token = state.shutdown_token.child_token();
    if !state
        .ai
        .try_register_download(spec.id, cancel_token.clone())
    {
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "model_id": spec.id, "status": "downloading" })),
        ));
    }

    let local_dir_str = model_dir(state.ai.models_dir(), spec.id)
        .to_string_lossy()
        .into_owned();
    let now_ms = Utc::now().timestamp_millis();
    let row = repo::ai::AiModelRow {
        model_id: spec.id.to_string(),
        status: AiModelStatus::Downloading,
        local_dir: Some(local_dir_str.clone()),
        bytes_total: spec.approx_bytes,
        bytes_downloaded: 0,
        sha256_manifest: None,
        error_message: None,
        updated_at_ms: now_ms,
    };
    if let Err(e) = state
        .db
        .write(move |conn| repo::ai::upsert_ai_model_row(conn, &row))
        .await
    {
        state.ai.finish_download(spec.id);
        return Err(e.into());
    }

    let bg_state = Arc::clone(&state);
    tokio::spawn(async move {
        run_model_download(bg_state, spec, local_dir_str, cancel_token).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "model_id": spec.id, "status": "downloading" })),
    ))
}

async fn run_model_download(
    state: Arc<AppState>,
    spec: &'static truongphoto_ai::ModelSpec,
    local_dir_str: String,
    cancel_token: CancellationToken,
) {
    let downloader = match ModelDownloader::new() {
        Ok(d) => d,
        Err(e) => {
            let now_ms = Utc::now().timestamp_millis();
            let err_row = repo::ai::AiModelRow {
                model_id: spec.id.to_string(),
                status: AiModelStatus::Error,
                local_dir: Some(local_dir_str),
                bytes_total: spec.approx_bytes,
                bytes_downloaded: 0,
                sha256_manifest: None,
                error_message: Some(e.to_string()),
                updated_at_ms: now_ms,
            };
            let _ = state
                .db
                .write(move |conn| repo::ai::upsert_ai_model_row(conn, &err_row))
                .await;
            state.ai.finish_download(spec.id);
            return;
        }
    };

    let kind = format!("model_download:{}", spec.id);
    let _ = state.ws_tx.send(WsServerEvent::JobProgress {
        job_id: 0,
        kind: kind.clone(),
        status: JobStatus::Running,
        processed: 0,
        total: Some(spec.approx_bytes),
        message: Some(format!("Downloading {}", spec.name)),
    });

    let mut last_emit = Instant::now();
    let models_dir = state.ai.models_dir().to_path_buf();
    let ws_tx = state.ws_tx.clone();
    let db = state.db.clone();
    let kind_for_cb = kind.clone();

    let result = downloader
        .download_model(&models_dir, spec, cancel_token, |downloaded, total| {
            if last_emit.elapsed() >= Duration::from_millis(250) {
                last_emit = Instant::now();
                let _ = ws_tx.send(WsServerEvent::JobProgress {
                    job_id: 0,
                    kind: kind_for_cb.clone(),
                    status: JobStatus::Running,
                    processed: downloaded,
                    total: Some(total),
                    message: None,
                });
                let db_clone = db.clone();
                let id_str = spec.id.to_string();
                let now_ms = Utc::now().timestamp_millis();
                tokio::spawn(async move {
                    let _ = db_clone
                        .write(move |conn| {
                            repo::ai::update_ai_model_download_progress(
                                conn, &id_str, downloaded, total, now_ms,
                            )
                        })
                        .await;
                });
            }
        })
        .await;

    let now_ms = Utc::now().timestamp_millis();
    match result {
        Ok(manifest) => {
            let manifest_json = serde_json::to_string(&manifest).ok();
            let row = repo::ai::AiModelRow {
                model_id: spec.id.to_string(),
                status: AiModelStatus::Installed,
                local_dir: Some(local_dir_str),
                bytes_total: spec.approx_bytes,
                bytes_downloaded: spec.approx_bytes,
                sha256_manifest: manifest_json,
                error_message: None,
                updated_at_ms: now_ms,
            };
            let should_reset_stage2 = matches!(
                spec.role,
                "face_detector" | "face_embedder" | "ocr_det" | "ocr_rec"
            );
            let _ = state
                .db
                .write(move |conn| {
                    repo::ai::upsert_ai_model_row(conn, &row)?;
                    if should_reset_stage2 {
                        let _ = repo::ai::reset_all_assets_ai_stage_to_1(conn, now_ms)?;
                    }
                    Ok(())
                })
                .await;
            let _ = state.ws_tx.send(WsServerEvent::JobProgress {
                job_id: 0,
                kind,
                status: JobStatus::Completed,
                processed: spec.approx_bytes,
                total: Some(spec.approx_bytes),
                message: Some(format!("Installed {}", spec.name)),
            });
            state.ai.clear_runtime_caches();
            state.ai.notify_work();
        }
        Err(AiError::Cancelled) => {
            // If `delete_ai_model` already removed the model directory while we were cancelling,
            // do not resurrect the `ai_models` row.
            let dir_still_exists = model_dir(state.ai.models_dir(), spec.id).exists();
            if dir_still_exists {
                let row = repo::ai::AiModelRow {
                    model_id: spec.id.to_string(),
                    status: AiModelStatus::NotInstalled,
                    local_dir: Some(local_dir_str),
                    bytes_total: spec.approx_bytes,
                    bytes_downloaded: 0,
                    sha256_manifest: None,
                    error_message: None,
                    updated_at_ms: now_ms,
                };
                let _ = state
                    .db
                    .write(move |conn| repo::ai::upsert_ai_model_row(conn, &row))
                    .await;
            }
            let _ = state.ws_tx.send(WsServerEvent::JobProgress {
                job_id: 0,
                kind,
                status: JobStatus::Cancelled,
                processed: 0,
                total: Some(spec.approx_bytes),
                message: None,
            });
        }
        Err(err) => {
            let msg = err.to_string();
            let row = repo::ai::AiModelRow {
                model_id: spec.id.to_string(),
                status: AiModelStatus::Error,
                local_dir: Some(local_dir_str),
                bytes_total: spec.approx_bytes,
                bytes_downloaded: 0,
                sha256_manifest: None,
                error_message: Some(msg.clone()),
                updated_at_ms: now_ms,
            };
            let _ = state
                .db
                .write(move |conn| repo::ai::upsert_ai_model_row(conn, &row))
                .await;
            let _ = state.ws_tx.send(WsServerEvent::JobProgress {
                job_id: 0,
                kind,
                status: JobStatus::Failed,
                processed: 0,
                total: Some(spec.approx_bytes),
                message: Some(msg),
            });
        }
    }

    state.ai.finish_download(spec.id);
}

/// `POST /api/admin/ai/models/{id}/cancel` (RequireAdmin -> `200 OK`).
pub async fn cancel_ai_model_download(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(model_id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    let spec = find_model_spec(&model_id)
        .ok_or_else(|| AppError::not_found(format!("Unknown AI model '{model_id}'.")))?;
    let cancelled = state.ai.cancel_download(spec.id);
    Ok(Json(json!({
        "model_id": spec.id,
        "cancelled": cancelled,
    })))
}

/// `DELETE /api/admin/ai/models/{id}` (RequireAdmin -> `204 No Content`).
pub async fn delete_ai_model(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(model_id): AxumPath<String>,
) -> Result<StatusCode, AppError> {
    let spec = find_model_spec(&model_id)
        .ok_or_else(|| AppError::not_found(format!("Unknown AI model '{model_id}'.")))?;

    state.ai.cancel_download(spec.id);
    // Stop any running llama-server sidecar holding GGUF file locks (required on Windows before delete)
    // and clear all in-memory ORT/Vision/OCR/Chat caches and CPU fallback latches.
    let _ = state
        .ai
        .pipeline()
        .chat
        .evict_idle(std::time::Duration::ZERO);
    state.ai.clear_runtime_caches();

    let dir = model_dir(state.ai.models_dir(), spec.id);
    if dir.exists() {
        let mut last_err: Option<std::io::Error> = None;
        let attempts = if cfg!(windows) { 6 } else { 2 };
        for attempt in 0..attempts {
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < attempts {
                        tokio::time::sleep(Duration::from_millis(80 * (attempt as u64 + 1))).await;
                    }
                }
            }
        }
        if let Some(e) = last_err {
            return Err(AppError::internal(format!(
                "Failed to delete model files: {e}"
            )));
        }
    }

    let id_str = spec.id.to_string();
    state
        .db
        .write(move |conn| repo::ai::delete_ai_model_row(conn, &id_str))
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/admin/ai/tier` (RequireAdmin -> `202 Accepted`).
pub async fn set_ai_tier(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<SetAiTierRequest>,
) -> Result<(StatusCode, Json<AiIndexStateView>), AppError> {
    let now_ms = Utc::now().timestamp_millis();
    let target_tier = req.tier;

    let current_state = state.db.read(repo::ai::get_ai_index_state).await?;
    let next_staging = if target_tier == current_state.active_vision_tier {
        None
    } else {
        Some(target_tier)
    };

    let ai_state = state
        .db
        .write(move |conn| repo::ai::set_staging_vision_tier(conn, next_staging, now_ms))
        .await?;

    state.ai.notify_work();
    let _ = state.ws_tx.send(WsServerEvent::AiStateChanged {
        active_vision_tier: ai_state.active_vision_tier,
        staging_vision_tier: ai_state.staging_vision_tier,
        paused: ai_state.paused,
    });

    Ok((StatusCode::ACCEPTED, Json(ai_state)))
}

/// `POST /api/admin/ai/chat-tier` (RequireAdmin -> `200 OK` `AiIndexStateView`; instant hot-swap).
pub async fn set_chat_tier(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<SetChatTierRequest>,
) -> Result<Json<AiIndexStateView>, AppError> {
    let now_ms = Utc::now().timestamp_millis();
    let tier = req.tier;
    let vlm_rerank_enabled = req.vlm_rerank_enabled;

    let prev_state = state.db.read(repo::ai::get_ai_index_state).await?;
    let ai_state = state
        .db
        .write(move |conn| repo::ai::set_chat_tier(conn, tier, vlm_rerank_enabled, now_ms))
        .await?;

    // Evict any previously running llama-server sidecar when switching chat tiers so
    // switching Tier 3 -> Tier 0 (or Tier 2) doesn't strand a 4–15 GB model in memory.
    if prev_state.active_chat_tier != tier {
        let pipeline = Arc::clone(state.ai.pipeline());
        let _ = tokio::task::spawn_blocking(move || {
            pipeline.chat.evict_idle(std::time::Duration::ZERO)
        })
        .await;
    }

    let _ = state.ws_tx.send(WsServerEvent::AiStateChanged {
        active_vision_tier: ai_state.active_vision_tier,
        staging_vision_tier: ai_state.staging_vision_tier,
        paused: ai_state.paused,
    });

    Ok(Json(ai_state))
}

/// `POST /api/admin/ai/reindex` (RequireAdmin -> `202 Accepted`).
pub async fn reindex_ai(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<ReindexAiRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let now_ms = Utc::now().timestamp_millis();
    let reset_count = if req.scope == ReindexScope::All {
        state
            .db
            .write(move |conn| repo::ai::reset_all_assets_ai_stage_to_1(conn, now_ms))
            .await?
    } else {
        0
    };

    state.ai.notify_work();

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "scope": req.scope,
            "reset_count": reset_count,
        })),
    ))
}

/// `POST /api/admin/ai/pause` (RequireAdmin -> `200 OK` `AiIndexStateView`).
pub async fn pause_ai(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AiIndexStateView>, AppError> {
    let now_ms = Utc::now().timestamp_millis();
    let ai_state = state
        .db
        .write(move |conn| repo::ai::set_ai_paused(conn, true, now_ms))
        .await?;

    let _ = state.ws_tx.send(WsServerEvent::AiStateChanged {
        active_vision_tier: ai_state.active_vision_tier,
        staging_vision_tier: ai_state.staging_vision_tier,
        paused: ai_state.paused,
    });

    Ok(Json(ai_state))
}

/// `POST /api/admin/ai/resume` (RequireAdmin -> `200 OK` `AiIndexStateView`).
pub async fn resume_ai(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AiIndexStateView>, AppError> {
    let now_ms = Utc::now().timestamp_millis();
    let ai_state = state
        .db
        .write(move |conn| repo::ai::set_ai_paused(conn, false, now_ms))
        .await?;

    state.ai.notify_work();
    let _ = state.ws_tx.send(WsServerEvent::AiStateChanged {
        active_vision_tier: ai_state.active_vision_tier,
        staging_vision_tier: ai_state.staging_vision_tier,
        paused: ai_state.paused,
    });

    Ok(Json(ai_state))
}

/// `PATCH /api/admin/ai/settings` (RequireAdmin -> `200 OK` `AiIndexStateView`).
pub async fn update_ai_settings(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<UpdateAiSettingsRequest>,
) -> Result<Json<AiIndexStateView>, AppError> {
    if let Some(frames) = req.video_max_frames {
        if !(1..=120).contains(&frames) {
            return Err(AppError::validation(
                "video_max_frames must be between 1 and 120.",
            ));
        }
    }

    let now_ms = Utc::now().timestamp_millis();
    let default_paths = state.default_paths.clone();

    let (ai_state, updated_settings) = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            if let Some(secs) = req.idle_unload_secs {
                repo::ai::update_ai_idle_unload_secs(&tx, secs, now_ms)?;
            }
            let updated_settings = if let Some(frames) = req.video_max_frames {
                let mut settings = repo::settings::load_app_settings(&tx, &default_paths)?;
                settings.video_max_frames = frames;
                repo::settings::save_app_settings(&tx, &settings)?;
                Some(settings)
            } else {
                None
            };
            let st = repo::ai::get_ai_index_state(&tx)?;
            tx.commit()?;
            Ok((st, updated_settings))
        })
        .await?;

    if let Some(settings) = updated_settings {
        let _ = state
            .ws_tx
            .send(WsServerEvent::SettingsChanged { settings });
    }

    Ok(Json(ai_state))
}
