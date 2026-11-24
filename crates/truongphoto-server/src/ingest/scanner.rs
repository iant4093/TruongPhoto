use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use parking_lot::Mutex;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::task::JoinSet;
use truongphoto_core::{partial_blake3_path, FolderRecord, JobStatus, PowerState, WsServerEvent};
use truongphoto_db::{repo, rusqlite::params};
use truongphoto_media::{
    classify::{
        classify_path, is_ignored_entry, mime_for_extension, normalized_extension, MediaCategory,
    },
    decode::generate_thumb_and_histogram,
    metadata::extract_metadata,
    volume::resolve_volume_identity,
};
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
struct CandidateFile {
    abs_path: PathBuf,
    rel_path: String,
    file_size: u64,
    mtime_ms: i64,
}

/// Coordinates incremental folder scans with per-folder concurrency guards and power-aware throttling.
pub struct ScanCoordinator {
    folder_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Default for ScanCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanCoordinator {
    pub fn new() -> Self {
        Self {
            folder_locks: Mutex::new(HashMap::new()),
        }
    }

    fn folder_lock(&self, folder_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.folder_locks.lock();
        map.entry(folder_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Creates a `folder_scan` job row immediately and spawns a background scan task.
    pub async fn enqueue_folder_scan(
        &self,
        state: &Arc<AppState>,
        folder_id: String,
    ) -> Result<i64, AppError> {
        let fid_check = folder_id.clone();
        let exists = state
            .db
            .read(move |conn| repo::folders::get_folder_by_id(conn, &fid_check))
            .await?;
        if exists.is_none() {
            return Err(AppError::not_found("Watched folder not found."));
        }

        let payload_json = json!({ "folder_id": folder_id }).to_string();
        let now_ms = Utc::now().timestamp_millis();
        let job = state
            .db
            .write(move |conn| {
                repo::jobs::enqueue_job(
                    conn,
                    &repo::jobs::EnqueueJob {
                        kind: "folder_scan".to_string(),
                        payload_json,
                        priority: 10,
                        max_attempts: 1,
                        run_after_ms: now_ms,
                    },
                )
            })
            .await?;

        let job_id = job.id;
        let lock = self.folder_lock(&folder_id);
        let state_cloned = state.clone();

        tokio::spawn(async move {
            let _guard = lock.lock().await;
            if state_cloned.shutdown_token.is_cancelled() {
                let _ = mark_job_cancelled(&state_cloned, job_id).await;
                return;
            }

            if let Err(err) = mark_job_running(&state_cloned, job_id).await {
                tracing::error!(job_id, error = %err, "failed to mark folder_scan job running");
                return;
            }

            match run_folder_scan(&state_cloned, &folder_id, job_id).await {
                Ok(()) => {}
                Err(err) => {
                    tracing::warn!(job_id, folder_id = %folder_id, error = %err, "folder scan aborted/failed");
                    let now_ms = Utc::now().timestamp_millis();
                    let msg = err.to_string();
                    let _ = state_cloned
                        .db
                        .write(move |conn| repo::jobs::fail_job(conn, job_id, &msg, None, now_ms))
                        .await;
                    let _ = state_cloned.ws_tx.send(WsServerEvent::JobProgress {
                        job_id,
                        kind: "folder_scan".to_string(),
                        status: JobStatus::Failed,
                        processed: 0,
                        total: None,
                        message: Some(err.to_string()),
                    });
                }
            }
        });

        Ok(job_id)
    }
}

async fn mark_job_running(state: &AppState, job_id: i64) -> anyhow::Result<()> {
    let now_ms = Utc::now().timestamp_millis();
    state
        .db
        .write(move |conn| {
            conn.execute(
                "UPDATE jobs
                 SET status = 'running',
                     attempts = attempts + 1,
                     started_at_ms = COALESCE(started_at_ms, ?2),
                     updated_at_ms = ?2
                 WHERE id = ?1",
                params![job_id, now_ms],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn mark_job_cancelled(state: &AppState, job_id: i64) -> anyhow::Result<()> {
    let now_ms = Utc::now().timestamp_millis();
    state
        .db
        .write(move |conn| {
            conn.execute(
                "UPDATE jobs
                 SET status = 'cancelled',
                     finished_at_ms = ?2,
                     updated_at_ms = ?2
                 WHERE id = ?1",
                params![job_id, now_ms],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Checks if a watched folder directory and volume UUID are online; updates DB and broadcasts events on transitions.
async fn verify_volume_online(
    state: &Arc<AppState>,
    folder: &FolderRecord,
) -> anyhow::Result<bool> {
    let root_path = PathBuf::from(&folder.root_path);
    let expected_uuid = folder.volume_uuid.clone();

    let (is_dir, current_vol) = tokio::task::spawn_blocking(move || {
        let dir_ok = root_path.is_dir();
        let vol = if dir_ok {
            resolve_volume_identity(&root_path)
        } else {
            Default::default()
        };
        (dir_ok, vol)
    })
    .await?;

    let uuid_mismatch = match expected_uuid.as_deref() {
        Some(expected) => current_vol.volume_uuid.as_deref() != Some(expected),
        None => false,
    };

    if !is_dir || uuid_mismatch {
        let fid = folder.id.clone();
        let now_ms = Utc::now().timestamp_millis();
        state.watcher.unwatch_folder(&folder.id);
        state
            .db
            .write(move |conn| repo::folders::set_folder_offline(conn, &fid, true, now_ms))
            .await?;
        let _ = state.ws_tx.send(WsServerEvent::FolderChanged {
            folder_id: folder.id.clone(),
            action: "offline".to_string(),
        });
        return Ok(false);
    }

    if folder.is_offline {
        let fid = folder.id.clone();
        let now_ms = Utc::now().timestamp_millis();
        state
            .db
            .write(move |conn| repo::folders::set_folder_offline(conn, &fid, false, now_ms))
            .await?;
        state
            .watcher
            .ensure_watched(state, folder.id.clone(), PathBuf::from(&folder.root_path));
        let _ = state.ws_tx.send(WsServerEvent::FolderChanged {
            folder_id: folder.id.clone(),
            action: "online".to_string(),
        });
    }

    Ok(true)
}

async fn wait_while_sleeping_or_cancelled(state: &AppState) -> bool {
    let mut power_rx = state.power_rx.clone();
    let mut throttle_rx = state.throttle_rx.clone();
    loop {
        if state.shutdown_token.is_cancelled() {
            return false;
        }
        let sleeping = power_rx.borrow().sleeping;
        let throttle = throttle_rx.borrow().clone();
        let paused_by_throttle = throttle.active
            && matches!(throttle.reason.as_deref(), Some("sleep") | Some("battery"));

        if sleeping || paused_by_throttle {
            tokio::select! {
                _ = state.shutdown_token.cancelled() => return false,
                res = power_rx.changed() => {
                    if res.is_err() {
                        return !state.shutdown_token.is_cancelled();
                    }
                }
                res = throttle_rx.changed() => {
                    if res.is_err() {
                        return !state.shutdown_token.is_cancelled();
                    }
                }
            }
            continue;
        }

        if throttle.active && throttle.reason.as_deref() == Some("high_cpu") {
            tokio::select! {
                _ = state.shutdown_token.cancelled() => return false,
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }

        return !state.shutdown_token.is_cancelled();
    }
}

fn target_worker_concurrency(state: &AppState, power: PowerState) -> usize {
    let throttle = state.throttle_rx.borrow().clone();
    if power.on_battery || (throttle.active && throttle.reason.as_deref() == Some("high_cpu")) {
        1
    } else {
        state.hardware.cpu_cores.clamp(1, 4)
    }
}

async fn run_folder_scan(
    state: &Arc<AppState>,
    folder_id: &str,
    job_id: i64,
) -> anyhow::Result<()> {
    let fid = folder_id.to_string();
    let default_paths = state.default_paths.clone();
    let (folder_opt, settings) = state
        .db
        .read(move |conn| {
            let f = repo::folders::get_folder_by_id(conn, &fid)?;
            let s = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((f, s))
        })
        .await?;

    let Some(folder) = folder_opt else {
        anyhow::bail!("Watched folder {folder_id} no longer exists");
    };

    if !verify_volume_online(state, &folder).await? {
        anyhow::bail!("Watched folder volume is offline or mismatched");
    }

    let fid = folder.id.clone();
    let existing_index = state
        .db
        .read(move |conn| repo::ingest::list_folder_file_index(conn, &fid))
        .await?;

    let root_path = PathBuf::from(&folder.root_path);
    let existing_for_walk = existing_index.clone();
    let (candidates, seen_rel_paths, walk_had_errors) =
        tokio::task::spawn_blocking(move || walk_folder_candidates(&root_path, &existing_for_walk))
            .await?;

    // Pass 1: mark disappeared paths missing ONLY if root walk had zero I/O errors.
    if !walk_had_errors {
        let missing_ids: Vec<String> = existing_index
            .values()
            .filter(|e| !e.is_missing && !seen_rel_paths.contains(&e.rel_path))
            .map(|e| e.file_id.clone())
            .collect();
        if !missing_ids.is_empty() {
            let now_ms = Utc::now().timestamp_millis();
            state
                .db
                .write(move |conn| repo::ingest::mark_files_missing(conn, &missing_ids, now_ms))
                .await?;
        }
    }

    let total = candidates.len() as u64;
    let _ = state.ws_tx.send(WsServerEvent::JobProgress {
        job_id,
        kind: "folder_scan".to_string(),
        status: JobStatus::Running,
        processed: 0,
        total: Some(total),
        message: None,
    });

    // Pass 2: bounded concurrency ingest with battery/sleep/shutdown awareness.
    let thumbs_dir = PathBuf::from(&settings.thumbnails_dir);
    let mut processed = 0u64;
    let mut last_progress_at = Instant::now();
    let mut iter = candidates.into_iter().peekable();
    let mut active: JoinSet<anyhow::Result<()>> = JoinSet::new();

    loop {
        if state.shutdown_token.is_cancelled() {
            active.abort_all();
            let _ = mark_job_cancelled(state, job_id).await;
            return Ok(());
        }

        if !wait_while_sleeping_or_cancelled(state).await {
            active.abort_all();
            let _ = mark_job_cancelled(state, job_id).await;
            return Ok(());
        }

        let limit = target_worker_concurrency(state, *state.power_rx.borrow());
        while active.len() < limit && iter.peek().is_some() {
            if let Some(cand) = iter.next() {
                let st = state.clone();
                let fld = folder.clone();
                let tdir = thumbs_dir.clone();
                active.spawn(async move {
                    let _ = process_candidate_file(&st, &fld, &tdir, cand, true).await?;
                    Ok(())
                });
            }
        }

        if active.is_empty() {
            break;
        }

        tokio::select! {
            _ = state.shutdown_token.cancelled() => {
                active.abort_all();
                let _ = mark_job_cancelled(state, job_id).await;
                return Ok(());
            }
            res = active.join_next() => {
                if let Some(join_res) = res {
                    match join_res {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => {
                            tracing::warn!(folder_id = %folder.id, error = %err, "failed to ingest media file during scan");
                        }
                        Err(join_err) => {
                            tracing::warn!(folder_id = %folder.id, error = %join_err, "ingest worker panicked");
                        }
                    }
                    processed += 1;
                    if processed.is_multiple_of(25) || last_progress_at.elapsed() >= Duration::from_millis(500) {
                        last_progress_at = Instant::now();
                        let _ = state.ws_tx.send(WsServerEvent::JobProgress {
                            job_id,
                            kind: "folder_scan".to_string(),
                            status: JobStatus::Running,
                            processed,
                            total: Some(total),
                            message: None,
                        });
                    }
                }
            }
        }
    }

    // Pass 3: re-verify volume online before cleaning up missing files.
    if !verify_volume_online(state, &folder).await? {
        anyhow::bail!("Watched folder volume went offline during scan");
    }

    let fid_cleanup = folder.id.clone();
    let now_ms = Utc::now().timestamp_millis();
    state
        .db
        .write(move |conn| {
            repo::ingest::cleanup_missing_files(conn, &fid_cleanup, now_ms)?;
            repo::folders::set_folder_last_scanned(conn, &fid_cleanup, now_ms)?;
            repo::jobs::complete_job(conn, job_id, now_ms)?;
            Ok(())
        })
        .await?;

    let _ = state.ws_tx.send(WsServerEvent::JobProgress {
        job_id,
        kind: "folder_scan".to_string(),
        status: JobStatus::Completed,
        processed,
        total: Some(total),
        message: None,
    });
    let _ = state.ws_tx.send(WsServerEvent::FolderChanged {
        folder_id: folder.id,
        action: "scanned".to_string(),
    });
    state.ai.notify_work();

    Ok(())
}

fn walk_folder_candidates(
    root_path: &Path,
    existing_index: &HashMap<String, repo::ingest::ExistingFileEntry>,
) -> (Vec<CandidateFile>, HashSet<String>, bool) {
    let mut candidates = Vec::new();
    let mut seen_rel_paths = HashSet::new();
    let mut walk_had_errors = false;

    let walker = WalkDir::new(root_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let Some(name) = entry.file_name().to_str() else {
                return false;
            };
            !is_ignored_entry(name)
        });

    for item in walker {
        let entry = match item {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(path = %root_path.display(), error = %err, "walkdir error");
                walk_had_errors = true;
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        if classify_path(entry.path()).is_none() {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(path = %entry.path().display(), error = %err, "metadata error");
                walk_had_errors = true;
                continue;
            }
        };

        let file_size = meta.len();
        if file_size == 0 {
            continue;
        }

        let mtime_ms = system_time_to_ms(meta.modified().ok());
        let Some(rel_path) = compute_rel_path(root_path, entry.path()) else {
            continue;
        };

        seen_rel_paths.insert(rel_path.clone());

        if let Some(ex) = existing_index.get(&rel_path) {
            if !ex.is_missing && ex.file_size == file_size && ex.mtime_ms == mtime_ms {
                continue;
            }
        }

        candidates.push(CandidateFile {
            abs_path: entry.path().to_path_buf(),
            rel_path,
            file_size,
            mtime_ms,
        });
    }

    (candidates, seen_rel_paths, walk_had_errors)
}

/// Reusable single-file ingest entry point used by folder scanner, FS watcher, and streaming uploads.
pub async fn ingest_single_file(
    state: &Arc<AppState>,
    folder: &FolderRecord,
    abs_path: &Path,
) -> Result<Option<String>, anyhow::Error> {
    let sym_meta = match tokio::fs::symlink_metadata(abs_path).await {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if sym_meta.file_type().is_symlink() || !sym_meta.is_file() {
        return Ok(None);
    }
    if classify_path(abs_path).is_none() {
        return Ok(None);
    }

    let root_path = Path::new(&folder.root_path);
    if let (Ok(canon_root), Ok(canon_file)) = (
        std::fs::canonicalize(root_path),
        std::fs::canonicalize(abs_path),
    ) {
        if !canon_file.starts_with(&canon_root) {
            return Ok(None);
        }
    }

    let file_size = sym_meta.len();
    if file_size == 0 {
        return Ok(None);
    }
    let mtime_ms = system_time_to_ms(sym_meta.modified().ok());

    let Some(rel_path) = compute_rel_path(root_path, abs_path) else {
        anyhow::bail!(
            "file {} is outside folder root {}",
            abs_path.display(),
            root_path.display()
        );
    };

    let fid = folder.id.clone();
    let rpath = rel_path.clone();
    let existing = state
        .db
        .read(move |conn| repo::ingest::get_folder_file_by_rel_path(conn, &fid, &rpath))
        .await?;

    if let Some(ex) = existing {
        if !ex.is_missing && ex.file_size == file_size && ex.mtime_ms == mtime_ms {
            return Ok(Some(ex.asset_id));
        }
    }

    let default_paths = state.default_paths.clone();
    let settings = state
        .db
        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
        .await?;
    let thumbs_dir = PathBuf::from(&settings.thumbnails_dir);

    let cand = CandidateFile {
        abs_path: abs_path.to_path_buf(),
        rel_path,
        file_size,
        mtime_ms,
    };

    let final_id = process_candidate_file(state, folder, &thumbs_dir, cand, false).await?;
    if final_id.is_some() {
        state.ai.notify_work();
    }
    Ok(final_id)
}

async fn process_candidate_file(
    state: &Arc<AppState>,
    folder: &FolderRecord,
    thumbs_dir: &Path,
    cand: CandidateFile,
    use_batched_write: bool,
) -> Result<Option<String>, anyhow::Error> {
    let abs_for_hash = cand.abs_path.clone();
    let partial_blake3 =
        tokio::task::spawn_blocking(move || partial_blake3_path(&abs_for_hash)).await??;

    // Move/rename re-linking check against missing asset_files within the same folder.
    let blake_lookup = partial_blake3.clone();
    let folder_lookup = folder.id.clone();
    let fsize = cand.file_size;
    let missing_match = state
        .db
        .read(move |conn| {
            repo::ingest::find_missing_by_fingerprint(conn, &folder_lookup, &blake_lookup, fsize)
        })
        .await?;

    if let Some(missing) = missing_match {
        let file_id = missing.file_id.clone();
        let new_folder_id = folder.id.clone();
        let new_rel = cand.rel_path.clone();
        let new_mtime = cand.mtime_ms;
        let now_ms = Utc::now().timestamp_millis();
        let relinked_asset_id = state
            .db
            .write(move |conn| {
                repo::ingest::relink_moved_file(
                    conn,
                    &file_id,
                    &new_folder_id,
                    &new_rel,
                    new_mtime,
                    now_ms,
                )
            })
            .await?;
        return Ok(Some(relinked_asset_id));
    }

    let Some(category) = classify_path(&cand.abs_path) else {
        return Ok(None);
    };
    let ext = normalized_extension(&cand.abs_path).unwrap_or_default();
    let mime_type = mime_for_extension(&ext).to_string();

    let cand_id = Uuid::new_v4().to_string();
    let p0 = &cand_id[0..2];
    let p1 = &cand_id[2..4];
    let thumb_rel = format!("{p0}/{p1}/{cand_id}.webp");
    let thumb_abs = thumbs_dir.join(p0).join(p1).join(format!("{cand_id}.webp"));

    let abs_for_media = cand.abs_path.clone();
    let thumb_out = thumb_abs.clone();
    let (meta, thumb_res) = tokio::task::spawn_blocking(move || {
        let meta = extract_metadata(&abs_for_media);
        let thumb = generate_thumb_and_histogram(&abs_for_media, &thumb_out);
        (meta, thumb)
    })
    .await?;

    let meta = match meta {
        Ok(m) => m,
        Err(err) => {
            tracing::debug!(path = %cand.abs_path.display(), error = %err, "metadata extraction fallback");
            Default::default()
        }
    };

    let (thumb_rel_path, thumb_ready, histogram, thumb_w, thumb_h) = match thumb_res {
        Ok(gen_thumb) => (
            Some(thumb_rel),
            true,
            Some(gen_thumb.histogram),
            Some(gen_thumb.width),
            Some(gen_thumb.height),
        ),
        Err(err) => {
            tracing::debug!(path = %cand.abs_path.display(), error = %err, "thumbnail generation skipped");
            (None, false, None, None, None)
        }
    };

    let rel_path_obj = Path::new(&cand.rel_path);
    let parent_dir = rel_path_obj
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let lower_stem = rel_path_obj
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let ingest_data = repo::ingest::IngestedFileData {
        asset_id_hint: Some(cand_id),
        folder_id: folder.id.clone(),
        rel_path: cand.rel_path,
        mime_type,
        extension: ext,
        file_size: cand.file_size,
        mtime_ms: cand.mtime_ms,
        partial_blake3,
        capture_at_ms: meta.capture_at_ms.unwrap_or(cand.mtime_ms),
        capture_tz_offset_m: meta.capture_tz_offset_m,
        width: meta.width.or(thumb_w),
        height: meta.height.or(thumb_h),
        duration_ms: meta.duration_ms,
        thumb_rel_path,
        thumb_ready,
        photo_meta: meta.photo_meta,
        raw_exif_json: None,
        video_meta: meta.video_meta,
        raw_probe_json: None,
        histogram,
        content_identifier: meta.content_identifier,
        embedded_video_offset: meta.embedded_video_offset,
        embedded_video_length: meta.embedded_video_length,
        is_raw: category == MediaCategory::Raw,
        is_video: category == MediaCategory::Video,
        is_livp: category == MediaCategory::Livp,
        parent_dir,
        lower_stem,
    };

    let out_id = Arc::new(Mutex::new(None));
    let out_id_clone = out_id.clone();

    if use_batched_write {
        state
            .db
            .write_batched(move |tx| {
                let id = repo::ingest::reconcile_ingested_file(tx, &ingest_data)?;
                *out_id_clone.lock() = Some(id);
                Ok(())
            })
            .await?;
    } else {
        let id = state
            .db
            .write(move |conn| repo::ingest::reconcile_ingested_file(conn, &ingest_data))
            .await?;
        *out_id.lock() = Some(id);
    }

    let final_id = out_id.lock().clone();
    Ok(final_id)
}

fn system_time_to_ms(time: Option<std::time::SystemTime>) -> i64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or_else(|| Utc::now().timestamp_millis())
}

fn compute_rel_path(root_path: &Path, abs_path: &Path) -> Option<String> {
    let rel_buf = if let Ok(r) = abs_path.strip_prefix(root_path) {
        r.to_path_buf()
    } else {
        let canon_root = std::fs::canonicalize(root_path).ok()?;
        let canon_abs = std::fs::canonicalize(abs_path).ok()?;
        canon_abs.strip_prefix(&canon_root).ok()?.to_path_buf()
    };

    let parts: Vec<String> = rel_buf
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}
