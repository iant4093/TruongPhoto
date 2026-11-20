use crate::error::AppError;
use crate::ingest::cache_mgr::safe_join_relative;
use crate::state::AppState;
use chrono::Utc;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use truongphoto_ai::{
    chinese_whispers_cluster, decode_centroid_le, encode_centroid_le, l2_normalize,
    match_face_to_centroids, sample_video_keyframes, update_running_centroid, AiError,
    DetectedFace, OcrResult, SampledKeyframe, DEFAULT_CW_SIM_THRESHOLD,
    DEFAULT_FACE_MATCH_THRESHOLD, MAX_AUTO_TAGS,
};
use truongphoto_core::{
    AiTier, AssetFileView, AssetKind, AssetSummary, ClusterFacesResponse, FileRole, FolderRecord,
    JobStatus, RbacScope, WsServerEvent,
};
use truongphoto_db::{repo, DbError};
use truongphoto_media::decode::decode_image_to_srgb;
use truongphoto_media::video::video_capabilities;
use uuid::Uuid;

const CW_SEED: u64 = 0x5452_554f_4e47_3230;
const STAGE2_FALLBACK_POLL: Duration = Duration::from_secs(15);
const ORT_EVICT_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

type PersonCentroidCache = Arc<Mutex<Vec<(String, u64, Vec<f32>)>>>;

fn select_ai_source_file<'a>(
    summary: &AssetSummary,
    files: &'a [AssetFileView],
) -> Option<&'a AssetFileView> {
    if summary.kind == AssetKind::RawJpgStack {
        files
            .iter()
            .find(|f| f.file_role == FileRole::JpegCompanion && !f.is_missing)
            .or_else(|| {
                files
                    .iter()
                    .find(|f| f.file_role == FileRole::Raw && !f.is_missing)
            })
            .or_else(|| files.iter().find(|f| !f.is_missing))
    } else {
        files
            .iter()
            .find(|f| f.file_role == FileRole::Primary && !f.is_missing)
            .or_else(|| files.iter().find(|f| !f.is_missing))
    }
}

fn resolve_source_path(folder: &FolderRecord, file: &AssetFileView) -> Option<PathBuf> {
    if folder.is_offline || file.is_missing {
        return None;
    }
    let full = safe_join_relative(Path::new(&folder.root_path), &file.rel_path)?;
    if full.is_file() {
        Some(full)
    } else {
        None
    }
}

fn is_unavailable_engine_err(err: &AiError) -> bool {
    matches!(err, AiError::ModelNotInstalled(_) | AiError::Ort(_))
}

fn parse_showinfo_pts_ms(stderr: &str) -> Vec<i64> {
    let mut timestamps = Vec::new();
    for line in stderr.lines() {
        if !line.contains("showinfo") {
            continue;
        }
        if let Some(idx) = line.find("pts_time:") {
            let rest = &line[idx + "pts_time:".len()..];
            let token = rest
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
            if let Ok(secs) = token.parse::<f64>() {
                if secs.is_finite() && secs >= 0.0 {
                    timestamps.push((secs * 1000.0).round() as i64);
                }
            }
        }
    }
    timestamps
}

fn sample_video_keyframes_fps_mode(
    video_path: &Path,
    max_frames: u32,
) -> Result<Vec<SampledKeyframe>, String> {
    if max_frames == 0 {
        return Ok(Vec::new());
    }
    let Some(ffmpeg) = video_capabilities().ffmpeg_path.clone() else {
        return Err("ffmpeg binary not found".to_string());
    };
    let tmp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let pattern = tmp_dir.path().join("kf_%03d.jpg");

    let output = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "info", "-i"])
        .arg(video_path)
        .args([
            "-vf",
            "select='if(isnan(prev_selected_t),1,gt(scene,0.3)*gte(t-prev_selected_t,5)+gte(t-prev_selected_t,30))',showinfo",
            "-fps_mode",
            "vfr",
            "-frames:v",
        ])
        .arg(max_frames.to_string())
        .arg(&pattern)
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let mut frame_paths: Vec<PathBuf> = std::fs::read_dir(tmp_dir.path())
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jpg"))
        .collect();
    frame_paths.sort();

    if frame_paths.is_empty() {
        return Ok(Vec::new());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut pts_ms = parse_showinfo_pts_ms(&stderr);
    if pts_ms.len() != frame_paths.len() {
        let n = frame_paths.len();
        pts_ms = (0..n).map(|i| (i as i64) * 5_000).collect();
    }

    let mut keyframes = Vec::with_capacity(frame_paths.len());
    for (path, timestamp_ms) in frame_paths.into_iter().zip(pts_ms.into_iter()) {
        let img = decode_image_to_srgb(&path).map_err(|e| e.to_string())?;
        keyframes.push(SampledKeyframe {
            timestamp_ms,
            image: img,
        });
    }
    Ok(keyframes)
}

fn cached_preview_path(cache_dir: &Path, asset_id: &str) -> Option<PathBuf> {
    if asset_id.len() < 2 {
        return None;
    }
    let p0 = &asset_id[..2];
    let candidate = cache_dir
        .join("previews")
        .join(p0)
        .join(format!("{asset_id}.webp"));
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

fn decode_frames_for_asset(
    kind: AssetKind,
    src_path: &Path,
    preview_path: Option<&Path>,
    video_max_frames: u32,
) -> Result<Vec<SampledKeyframe>, String> {
    if kind == AssetKind::Video {
        if let Ok(kfs) = sample_video_keyframes(src_path, video_max_frames) {
            if !kfs.is_empty() {
                return Ok(kfs);
            }
        }
        if let Ok(kfs) = sample_video_keyframes_fps_mode(src_path, video_max_frames) {
            if !kfs.is_empty() {
                return Ok(kfs);
            }
        }
        let poster = decode_image_to_srgb(src_path).map_err(|e| e.to_string())?;
        return Ok(vec![SampledKeyframe {
            timestamp_ms: 0,
            image: poster,
        }]);
    }

    // Prefer already-cached 2048px preview when available to skip decoding giant 20-50MB originals.
    let mut img = match preview_path.and_then(|p| decode_image_to_srgb(p).ok()) {
        Some(cached) => cached,
        None => decode_image_to_srgb(src_path).map_err(|e| e.to_string())?,
    };
    let max_side = img.width().max(img.height());
    if max_side > 1600 {
        let scale = 1600.0f32 / (max_side as f32);
        let new_w = ((img.width() as f32 * scale).round() as u32).max(1);
        let new_h = ((img.height() as f32 * scale).round() as u32).max(1);
        img = image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Triangle);
    }
    Ok(vec![SampledKeyframe {
        timestamp_ms: 0,
        image: img,
    }])
}

fn merge_zero_shot_tags(candidates: Vec<(String, f32)>) -> Vec<(String, f64)> {
    let mut best: HashMap<String, f32> = HashMap::new();
    for (tag, score) in candidates {
        best.entry(tag)
            .and_modify(|cur| {
                if score > *cur {
                    *cur = score;
                }
            })
            .or_insert(score);
    }
    let mut sorted: Vec<(String, f64)> = best.into_iter().map(|(t, s)| (t, f64::from(s))).collect();
    sorted.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted.truncate(MAX_AUTO_TAGS);
    sorted
}

struct InferredAssetData {
    vision_vecs: Vec<(i64, Vec<f32>)>,
    vision_ok: bool,
    tags: Vec<(String, f64)>,
    faces: Vec<DetectedFace>,
    ocr: Option<OcrResult>,
}

fn is_paused_by_power_or_throttle(state: &AppState) -> bool {
    if state.power_rx.borrow().sleeping {
        return true;
    }
    let throttle = state.throttle_rx.borrow();
    throttle.active && matches!(throttle.reason.as_deref(), Some("sleep") | Some("battery"))
}

async fn backoff_if_high_cpu(state: &AppState) -> bool {
    let high_cpu = {
        let throttle = state.throttle_rx.borrow();
        throttle.active && throttle.reason.as_deref() == Some("high_cpu")
    };
    if high_cpu {
        tokio::select! {
            _ = state.shutdown_token.cancelled() => return false,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
    !state.shutdown_token.is_cancelled()
}

/// Processes up to `max_assets` pending Stage 2 AI indexing tasks (or staging tier upgrade tasks).
///
/// Returns the number of assets processed in this batch.
pub async fn process_stage2_batch(
    state: &Arc<AppState>,
    max_assets: usize,
) -> Result<usize, anyhow::Error> {
    if max_assets == 0 {
        return Ok(0);
    }

    let _worker_guard = state.ai.worker_lock.lock().await;

    let default_paths = state.default_paths.clone();
    let (ai_state, settings) = state
        .db
        .read(move |conn| {
            let st = repo::ai::get_ai_index_state(conn)?;
            let cfg = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((st, cfg))
        })
        .await?;

    if ai_state.paused || is_paused_by_power_or_throttle(state) {
        return Ok(0);
    }

    let cache_dir = PathBuf::from(&settings.cache_dir);

    if let Some(staging_tier) = ai_state.staging_vision_tier {
        return process_staging_tier_batch(
            state,
            ai_state.active_vision_tier,
            staging_tier,
            settings.video_max_frames,
            &cache_dir,
            max_assets,
        )
        .await;
    }

    process_active_stage2_batch(
        state,
        ai_state.active_vision_tier,
        settings.video_max_frames,
        &cache_dir,
        max_assets,
    )
    .await
}

async fn process_staging_tier_batch(
    state: &Arc<AppState>,
    active_tier: AiTier,
    staging_tier: AiTier,
    video_max_frames: u32,
    cache_dir: &Path,
    max_assets: usize,
) -> Result<usize, anyhow::Error> {
    let (asset_ids, queue_stats) = state
        .db
        .read(move |conn| {
            let ids = repo::ai::list_assets_needing_staging_tier(conn, staging_tier, max_assets)?;
            let stats = repo::ai::count_ai_queue_stats(conn, active_tier, Some(staging_tier))?;
            Ok((ids, stats))
        })
        .await?;

    let mut processed = 0usize;
    let total_staging = queue_stats.staging_remaining;

    if !asset_ids.is_empty() {
        let staging_idx = state.ai.vision_index(staging_tier)?;
        let pipeline = Arc::clone(state.ai.pipeline());

        for asset_id in asset_ids {
            if state.shutdown_token.is_cancelled() || is_paused_by_power_or_throttle(state) {
                break;
            }
            if !backoff_if_high_cpu(state).await {
                break;
            }
            let paused_now = state
                .db
                .read(|conn| Ok(repo::ai::get_ai_index_state(conn)?.paused))
                .await
                .unwrap_or(false);
            if paused_now {
                break;
            }

            let aid_lookup = asset_id.clone();
            let asset_opt = state
                .db
                .read(move |conn| {
                    repo::assets::get_asset_with_files(conn, &RbacScope::AdminAll, &aid_lookup)
                })
                .await?;

            let Some((summary, files, folder, _)) = asset_opt else {
                continue;
            };
            let Some(src_file) = select_ai_source_file(&summary, &files) else {
                continue;
            };
            let Some(src_path) = resolve_source_path(&folder, src_file) else {
                continue;
            };

            let pipe = Arc::clone(&pipeline);
            let kind = summary.kind;
            let preview_path = cached_preview_path(cache_dir, &asset_id);
            let inferred = tokio::task::spawn_blocking(move || {
                let frames = match decode_frames_for_asset(
                    kind,
                    &src_path,
                    preview_path.as_deref(),
                    video_max_frames,
                ) {
                    Ok(f) => f,
                    Err(err) => {
                        tracing::debug!(path = %src_path.display(), error = %err, "stage2 staging decode skipped");
                        return Ok::<_, AiError>(None);
                    }
                };
                let mut vecs = Vec::with_capacity(frames.len());
                for kf in frames {
                    match pipe.vision.embed_image(staging_tier, &kf.image) {
                        Ok(v) => vecs.push((kf.timestamp_ms, v)),
                        Err(e) if is_unavailable_engine_err(&e) => return Ok(None),
                        Err(e) => return Err(e),
                    }
                }
                Ok(Some(vecs))
            })
            .await??;

            let Some(vision_vecs) = inferred else {
                // Vision model not installed or ORT unavailable; do not cut over prematurely.
                break;
            };

            let idx_clone = Arc::clone(&staging_idx);
            let aid_write = asset_id.clone();
            let now_ms = Utc::now().timestamp_millis();
            state
                .db
                .write_batched(move |tx| {
                    let old_ids =
                        repo::ai::list_vector_ids_for_asset(tx, staging_tier, &aid_write)?;
                    for old_id in old_ids {
                        let _ = idx_clone.mark_deleted(old_id);
                    }
                    let mut next_vid = repo::ai::allocate_next_vision_vector_id(tx, staging_tier)?;
                    for (ts_ms, vec) in vision_vecs {
                        idx_clone
                            .add(next_vid, &vec)
                            .map_err(|e| DbError::InvalidArgument(e.to_string()))?;
                        repo::ai::upsert_vision_embedding(
                            tx,
                            staging_tier,
                            next_vid,
                            &aid_write,
                            ts_ms,
                            now_ms,
                        )?;
                        next_vid = next_vid.saturating_add(1);
                    }
                    Ok(())
                })
                .await?;

            processed += 1;
            let _ = state.ws_tx.send(WsServerEvent::JobProgress {
                job_id: 0,
                kind: format!("stage2_tier_upgrade:{}", staging_tier.as_str()),
                status: JobStatus::Running,
                processed: processed as u64,
                total: Some(total_staging),
                message: None,
            });
        }
    }

    let remaining = state
        .db
        .read(move |conn| {
            Ok(
                repo::ai::count_ai_queue_stats(conn, active_tier, Some(staging_tier))?
                    .staging_remaining,
            )
        })
        .await?;

    if remaining == 0 {
        let now_ms = Utc::now().timestamp_millis();
        let updated = state
            .db
            .write(move |conn| repo::ai::commit_vision_tier_cutover(conn, staging_tier, now_ms))
            .await?;

        let _ = state.ws_tx.send(WsServerEvent::AiStateChanged {
            active_vision_tier: updated.active_vision_tier,
            staging_vision_tier: updated.staging_vision_tier,
            paused: updated.paused,
        });
        let _ = state.ws_tx.send(WsServerEvent::JobProgress {
            job_id: 0,
            kind: format!("stage2_tier_upgrade:{}", staging_tier.as_str()),
            status: JobStatus::Completed,
            processed: processed as u64,
            total: Some(total_staging),
            message: Some(format!("Cut over to {}", staging_tier.as_str())),
        });
    }

    Ok(processed)
}

async fn process_active_stage2_batch(
    state: &Arc<AppState>,
    active_tier: AiTier,
    video_max_frames: u32,
    cache_dir: &Path,
    max_assets: usize,
) -> Result<usize, anyhow::Error> {
    let (asset_ids, raw_centroids, queue_stats) = state
        .db
        .read(move |conn| {
            let ids = repo::ai::list_assets_needing_stage2(conn, max_assets)?;
            let cents = repo::ai::list_person_centroids(conn)?;
            let stats = repo::ai::count_ai_queue_stats(conn, active_tier, None)?;
            Ok((ids, cents, stats))
        })
        .await?;

    if asset_ids.is_empty() {
        return Ok(0);
    }

    let centroids: PersonCentroidCache = Arc::new(Mutex::new(
        raw_centroids
            .into_iter()
            .filter_map(|(pid, count, blob)| decode_centroid_le(&blob).map(|vec| (pid, count, vec)))
            .collect(),
    ));

    let vision_idx = state.ai.vision_index(active_tier)?;
    let faces_idx = Arc::clone(state.ai.faces_index());
    let pipeline = Arc::clone(state.ai.pipeline());
    let total_pending = queue_stats.pending_assets;
    let mut processed = 0usize;

    for asset_id in asset_ids {
        if state.shutdown_token.is_cancelled() || is_paused_by_power_or_throttle(state) {
            break;
        }
        if !backoff_if_high_cpu(state).await {
            break;
        }
        let paused_now = state
            .db
            .read(|conn| Ok(repo::ai::get_ai_index_state(conn)?.paused))
            .await
            .unwrap_or(false);
        if paused_now {
            break;
        }

        let aid_lookup = asset_id.clone();
        let (asset_opt, has_existing_faces) = state
            .db
            .read(move |conn| {
                let asset =
                    repo::assets::get_asset_with_files(conn, &RbacScope::AdminAll, &aid_lookup)?;
                let face_count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM faces WHERE asset_id = ?1",
                    truongphoto_db::rusqlite::params![aid_lookup],
                    |row| row.get(0),
                )?;
                Ok((asset, face_count > 0))
            })
            .await?;

        let Some((summary, files, folder, _)) = asset_opt else {
            continue;
        };
        let Some(src_file) = select_ai_source_file(&summary, &files) else {
            continue;
        };
        let Some(src_path) = resolve_source_path(&folder, src_file) else {
            continue;
        };

        let pipe = Arc::clone(&pipeline);
        let kind = summary.kind;
        let preview_path = cached_preview_path(cache_dir, &asset_id);
        let inferred = tokio::task::spawn_blocking(move || {
            let frames = match decode_frames_for_asset(
                kind,
                &src_path,
                preview_path.as_deref(),
                video_max_frames,
            ) {
                Ok(f) => f,
                Err(err) => {
                    tracing::debug!(path = %src_path.display(), error = %err, "stage2 decode skipped");
                    return Ok::<_, AiError>(None);
                }
            };
            if frames.is_empty() {
                return Ok(None);
            }

            // Overlap CPU vision inference with hardware-accelerated (CoreML/DirectML) face + OCR inference.
            let (vision_res, (faces, ocr)) = std::thread::scope(|s| {
                let vision_handle = s.spawn(|| {
                    let mut vision_vecs = Vec::with_capacity(frames.len());
                    let mut raw_tags = Vec::new();
                    let mut vision_ok = true;

                    for kf in &frames {
                        match pipe.vision.embed_image(active_tier, &kf.image) {
                            Ok(vec) => {
                                if let Ok(scored) =
                                    pipe.vision.score_zero_shot_tags(active_tier, &vec)
                                {
                                    raw_tags.extend(scored);
                                }
                                vision_vecs.push((kf.timestamp_ms, vec));
                            }
                            Err(e) if is_unavailable_engine_err(&e) => {
                                vision_ok = false;
                                break;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    Ok((vision_vecs, raw_tags, vision_ok))
                });

                let primary_img = &frames[0].image;
                let faces = if has_existing_faces {
                    Vec::new()
                } else {
                    match pipe.faces.detect_and_embed(primary_img) {
                        Ok(detected) => detected,
                        Err(e) if is_unavailable_engine_err(&e) => Vec::new(),
                        Err(e) => {
                            tracing::debug!(error = %e, "face detection skipped for asset");
                            Vec::new()
                        }
                    }
                };

                let ocr = match pipe.ocr.recognize(primary_img) {
                    Ok(res) if res.full_text.trim().chars().count() >= 2 => Some(res),
                    Ok(_) => None,
                    Err(e) if is_unavailable_engine_err(&e) => None,
                    Err(e) => {
                        tracing::debug!(error = %e, "OCR skipped for asset");
                        None
                    }
                };

                let vision_out = vision_handle
                    .join()
                    .map_err(|_| AiError::Ort("vision inference worker thread panicked".into()))?;
                Ok::<_, AiError>((vision_out?, (faces, ocr)))
            })?;

            let (vision_vecs, raw_tags, vision_ok) = vision_res;

            Ok(Some(InferredAssetData {
                vision_vecs,
                vision_ok,
                tags: merge_zero_shot_tags(raw_tags),
                faces,
                ocr,
            }))
        })
        .await??;

        let Some(data) = inferred else {
            continue;
        };

        if !data.vision_ok && data.faces.is_empty() && data.ocr.is_none() {
            // No AI models available yet; avoid spinning through the remaining queue.
            break;
        }

        let vision_ok = data.vision_ok;
        let vision_idx_clone = Arc::clone(&vision_idx);
        let faces_idx_clone = Arc::clone(&faces_idx);
        let centroids_clone = Arc::clone(&centroids);
        let aid_write = asset_id.clone();
        let now_ms = Utc::now().timestamp_millis();

        state
            .db
            .write_batched(move |tx| {
                if data.vision_ok {
                    let old_ids = repo::ai::list_vector_ids_for_asset(tx, active_tier, &aid_write)?;
                    for old_id in old_ids {
                        let _ = vision_idx_clone.mark_deleted(old_id);
                    }
                    let mut next_vid = repo::ai::allocate_next_vision_vector_id(tx, active_tier)?;
                    for (ts_ms, vec) in data.vision_vecs {
                        vision_idx_clone
                            .add(next_vid, &vec)
                            .map_err(|e| DbError::InvalidArgument(e.to_string()))?;
                        repo::ai::upsert_vision_embedding(
                            tx,
                            active_tier,
                            next_vid,
                            &aid_write,
                            ts_ms,
                            now_ms,
                        )?;
                        next_vid = next_vid.saturating_add(1);
                    }
                    repo::ai::insert_ai_tags_ignoring_rejected(
                        tx,
                        &aid_write,
                        &data.tags,
                        active_tier.vision_model_id(),
                        now_ms,
                    )?;
                }

                if !data.faces.is_empty() {
                    let mut cent_guard = centroids_clone.lock();
                    for face in data.faces {
                        let pairs: Vec<(String, Vec<f32>)> = cent_guard
                            .iter()
                            .map(|(pid, _, vec)| (pid.clone(), vec.clone()))
                            .collect();
                        let matched = match_face_to_centroids(
                            &face.embedding,
                            &pairs,
                            DEFAULT_FACE_MATCH_THRESHOLD,
                        );

                        let (person_id, assigned_by) = if let Some((pid, _sim)) = matched {
                            if let Some((_, count, vec)) =
                                cent_guard.iter_mut().find(|(id, _, _)| id == &pid)
                            {
                                let updated = update_running_centroid(vec, *count, &face.embedding);
                                *count += 1;
                                *vec = updated.clone();
                                let blob = encode_centroid_le(&updated);
                                repo::ai::upsert_person_centroid(tx, &pid, *count, &blob, now_ms)?;
                            }
                            (Some(pid), "auto")
                        } else {
                            (None, "unassigned")
                        };

                        let fvid = repo::ai::allocate_next_face_vector_id(tx)?;
                        faces_idx_clone
                            .add(fvid, &face.embedding)
                            .map_err(|e| DbError::InvalidArgument(e.to_string()))?;
                        let face_id = Uuid::new_v4().to_string();
                        repo::ai::insert_face_with_embedding(
                            tx,
                            &face_id,
                            &aid_write,
                            person_id.as_deref(),
                            (face.bbox_x, face.bbox_y, face.bbox_w, face.bbox_h),
                            face.det_score,
                            fvid,
                            assigned_by,
                            now_ms,
                        )?;
                    }
                }

                if let Some(ocr) = data.ocr {
                    let trimmed = ocr.full_text.trim();
                    if trimmed.chars().count() >= 2 {
                        repo::ai::upsert_ocr_document(
                            tx,
                            &aid_write,
                            trimmed,
                            ocr.lang_hint.as_deref(),
                            &ocr.model_id,
                            now_ms,
                        )?;
                    }
                }

                if data.vision_ok {
                    repo::ai::mark_asset_ai_stage(tx, &aid_write, 2, now_ms)?;
                }

                Ok(())
            })
            .await?;

        if vision_ok {
            processed += 1;
            let status = if (processed as u64) >= total_pending {
                JobStatus::Completed
            } else {
                JobStatus::Running
            };
            let _ = state.ws_tx.send(WsServerEvent::JobProgress {
                job_id: 0,
                kind: "stage2_ai".to_string(),
                status,
                processed: processed as u64,
                total: Some(total_pending),
                message: None,
            });
        }
    }

    Ok(processed)
}

/// Runs deterministic Chinese Whispers clustering over all unassigned faces in `faces_index`.
pub async fn run_face_clustering(state: &Arc<AppState>) -> Result<ClusterFacesResponse, AppError> {
    let unassigned = state
        .db
        .read(repo::ai::list_unassigned_faces_with_vectors)
        .await?;

    if unassigned.len() < 2 {
        return Ok(ClusterFacesResponse::default());
    }

    let faces_idx = Arc::clone(state.ai.faces_index());
    let (cluster_inputs, vid_to_face_id, vid_to_vec) = tokio::task::spawn_blocking(move || {
        let mut nodes = Vec::with_capacity(unassigned.len());
        let mut vid_to_face_id = HashMap::with_capacity(unassigned.len());
        let mut vid_to_vec = HashMap::with_capacity(unassigned.len());

        for (face_id, vid) in unassigned {
            if let Some(vec) = faces_idx.flat().get_f32(vid) {
                nodes.push((vid, vec.clone()));
                vid_to_face_id.insert(vid, face_id);
                vid_to_vec.insert(vid, vec);
            }
        }

        let clusters = chinese_whispers_cluster(&nodes, DEFAULT_CW_SIM_THRESHOLD, 2, CW_SEED);
        (clusters, vid_to_face_id, vid_to_vec)
    })
    .await
    .map_err(|e| AppError::internal(format!("Face clustering worker panicked: {e}")))?;

    if cluster_inputs.is_empty() {
        return Ok(ClusterFacesResponse::default());
    }

    let mut prepared_clusters: Vec<(Vec<String>, Vec<u8>)> =
        Vec::with_capacity(cluster_inputs.len());
    let mut faces_assigned = 0u64;

    for cluster_vids in cluster_inputs {
        let mut face_ids = Vec::with_capacity(cluster_vids.len());
        let mut sum: Vec<f32> = Vec::new();
        for vid in cluster_vids {
            if let (Some(fid), Some(vec)) = (vid_to_face_id.get(&vid), vid_to_vec.get(&vid)) {
                if sum.is_empty() {
                    sum = vec![0.0; vec.len()];
                }
                for (acc, &v) in sum.iter_mut().zip(vec.iter()) {
                    *acc += v;
                }
                face_ids.push(fid.clone());
            }
        }
        if face_ids.len() < 2 || sum.is_empty() {
            continue;
        }
        faces_assigned += face_ids.len() as u64;
        let centroid = l2_normalize(&sum);
        let blob = encode_centroid_le(&centroid);
        prepared_clusters.push((face_ids, blob));
    }

    if prepared_clusters.is_empty() {
        return Ok(ClusterFacesResponse::default());
    }

    let clusters_created = prepared_clusters.len() as u64;
    let now_ms = Utc::now().timestamp_millis();

    state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            for (face_ids, blob) in &prepared_clusters {
                repo::people::create_person_for_cluster(&tx, face_ids, blob, now_ms)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;

    Ok(ClusterFacesResponse {
        clusters_created,
        faces_assigned,
    })
}

/// Spawns the cancellable Stage 2 AI background worker and 60s idle ORT session eviction sweep.
pub fn spawn_ai_background_tasks(state: Arc<AppState>) -> Vec<JoinHandle<()>> {
    let worker_state = Arc::clone(&state);
    let worker_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(STAGE2_FALLBACK_POLL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut throttle_rx = worker_state.throttle_rx.clone();

        loop {
            tokio::select! {
                _ = worker_state.shutdown_token.cancelled() => break,
                _ = worker_state.ai.wake.notified() => {}
                _ = interval.tick() => {}
                res = throttle_rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                    if throttle_rx.borrow_and_update().active {
                        continue;
                    }
                }
            }

            if worker_state.shutdown_token.is_cancelled() {
                break;
            }

            let mut processed_since_cluster = 0usize;
            loop {
                if worker_state.shutdown_token.is_cancelled() {
                    break;
                }
                let power = *worker_state.power_rx.borrow();
                if is_paused_by_power_or_throttle(&worker_state) {
                    break;
                }
                let throttle = worker_state.throttle_rx.borrow().clone();
                let batch_size = if power.on_battery
                    || (throttle.active && throttle.reason.as_deref() == Some("high_cpu"))
                {
                    2
                } else {
                    16
                };
                match process_stage2_batch(&worker_state, batch_size).await {
                    Ok(0) => break,
                    Ok(n) => {
                        processed_since_cluster += n;
                        if processed_since_cluster >= 48 {
                            let _ = run_face_clustering(&worker_state).await;
                            processed_since_cluster = 0;
                        }
                        if throttle.active && throttle.reason.as_deref() == Some("high_cpu") {
                            tokio::select! {
                                _ = worker_state.shutdown_token.cancelled() => break,
                                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                            }
                        } else if power.on_battery {
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        } else {
                            tokio::task::yield_now().await;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "stage2 AI background batch failed");
                        break;
                    }
                }
            }
            if processed_since_cluster > 0 && !worker_state.shutdown_token.is_cancelled() {
                let _ = run_face_clustering(&worker_state).await;
            }
        }
    });

    let evict_state = Arc::clone(&state);
    let eviction_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(ORT_EVICT_SWEEP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip immediate t=0 tick
        interval.tick().await;

        loop {
            tokio::select! {
                _ = evict_state.shutdown_token.cancelled() => break,
                _ = interval.tick() => {
                    let idle_unload_secs = evict_state
                        .db
                        .read(|conn| Ok(repo::ai::get_ai_index_state(conn)?.idle_unload_secs))
                        .await
                        .unwrap_or(300);
                    if idle_unload_secs > 0 {
                        let max_idle = Duration::from_secs(idle_unload_secs);
                        let evicted = evict_state
                            .ai
                            .pipeline()
                            .ort
                            .evict_idle_sessions(max_idle);
                        let chat_evicted = evict_state
                            .ai
                            .pipeline()
                            .chat
                            .evict_idle(max_idle);
                        if evicted > 0 || chat_evicted > 0 {
                            tracing::debug!(
                                evicted,
                                chat_evicted,
                                idle_unload_secs,
                                "evicted idle AI sessions"
                            );
                        }
                    }
                }
            }
        }
    });

    vec![worker_task, eviction_task]
}
