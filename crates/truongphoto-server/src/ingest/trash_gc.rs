use crate::ai::AiCoordinator;
use crate::error::AppError;
use crate::ingest::cache_mgr::{safe_join_no_symlinks, safe_join_relative, CacheManager};
use crate::state::AppState;
use chrono::Utc;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use truongphoto_core::{AiTier, PurgeReport};
use truongphoto_db::{repo, rusqlite::Connection, DbHandle};
use truongphoto_media::volume::resolve_volume_identity;

const TRASH_SWEEP_INTERVAL: Duration = Duration::from_secs(3600);
const DEFAULT_RETENTION_DAYS: i64 = 30;
const STALE_STAGING_PART_AGE: Duration = Duration::from_secs(6 * 3600);
const STALE_HLS_TMP_AGE: Duration = Duration::from_secs(3600);

/// Derived thumbnails, caches, face crops, and vector slots to clean up after asset rows are removed.
#[derive(Debug, Default)]
pub(crate) struct AssetCleanupArtifacts {
    pub thumb_rel_paths: Vec<String>,
    pub cache_rel_paths: Vec<String>,
    pub face_ids: Vec<String>,
    pub vision_vids: Vec<(AiTier, u32)>,
    pub face_vids: Vec<u32>,
    pub affected_people: Vec<String>,
}

impl AssetCleanupArtifacts {
    pub fn is_empty(&self) -> bool {
        self.thumb_rel_paths.is_empty()
            && self.cache_rel_paths.is_empty()
            && self.face_ids.is_empty()
            && self.vision_vids.is_empty()
            && self.face_vids.is_empty()
            && self.affected_people.is_empty()
    }
}

/// Collects derived thumbs, cache paths, face IDs, vector IDs, and affected people for `asset_ids`
/// BEFORE the `assets` rows are deleted or hidden.
pub(crate) fn collect_artifacts_for_assets(
    conn: &Connection,
    asset_ids: &[String],
) -> truongphoto_db::rusqlite::Result<AssetCleanupArtifacts> {
    let mut out = AssetCleanupArtifacts::default();
    if asset_ids.is_empty() {
        return Ok(out);
    }
    let mut people_set: HashSet<String> = HashSet::new();
    let mut face_vid_set: HashSet<u32> = HashSet::new();

    for aid in asset_ids {
        if let Ok(Some(rel)) = conn.query_row(
            "SELECT thumb_rel_path FROM assets WHERE id = ?1",
            [aid],
            |row| row.get::<_, Option<String>>(0),
        ) {
            out.thumb_rel_paths.push(rel);
        }

        {
            let mut stmt =
                conn.prepare("SELECT rel_path FROM lru_cache_entries WHERE asset_id = ?1")?;
            let rows = stmt.query_map([aid], |row| row.get::<_, String>(0))?;
            for rel in rows.flatten() {
                out.cache_rel_paths.push(rel);
            }
        }

        {
            let mut stmt = conn.prepare(
                "SELECT model_tier, vector_id FROM vision_embeddings WHERE asset_id = ?1",
            )?;
            let rows = stmt.query_map([aid], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for r in rows.flatten() {
                if let (Ok(tier), Ok(vid)) = (r.0.parse::<AiTier>(), u32::try_from(r.1)) {
                    out.vision_vids.push((tier, vid));
                }
            }
        }

        {
            let mut stmt = conn.prepare(
                "SELECT f.id, COALESCE(fe.vector_id, f.vector_id) AS vid, f.person_id
                 FROM faces f
                 LEFT JOIN face_embeddings fe ON fe.face_id = f.id
                 WHERE f.asset_id = ?1",
            )?;
            let rows = stmt.query_map([aid], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;
            for (face_id, vid_opt, person_opt) in rows.flatten() {
                out.face_ids.push(face_id);
                if let Some(vid) = vid_opt.and_then(|v| u32::try_from(v).ok()) {
                    if face_vid_set.insert(vid) {
                        out.face_vids.push(vid);
                    }
                }
                if let Some(pid) = person_opt {
                    if people_set.insert(pid.clone()) {
                        out.affected_people.push(pid);
                    }
                }
            }
        }
    }

    Ok(out)
}

/// Tombstones HNSW vectors and deletes derived thumbnails, cache entries, and face crops off the async runtime.
pub(crate) async fn reap_cleanup_artifacts(
    ai: Arc<AiCoordinator>,
    thumbnails_dir: PathBuf,
    cache_dir: PathBuf,
    artifacts: AssetCleanupArtifacts,
) {
    if artifacts.is_empty() {
        return;
    }
    let _ = tokio::task::spawn_blocking(move || {
        for (tier, vid) in artifacts.vision_vids {
            if let Ok(idx) = ai.vision_index(tier) {
                let _ = idx.mark_deleted(vid);
            }
        }
        let faces = ai.faces_index();
        for fvid in artifacts.face_vids {
            let _ = faces.mark_deleted(fvid);
        }
        for rel in artifacts.thumb_rel_paths {
            if let Some(p) = safe_join_relative(&thumbnails_dir, &rel) {
                let _ = std::fs::remove_file(p);
            }
        }
        for rel in artifacts.cache_rel_paths {
            if let Some(p) = safe_join_relative(&cache_dir, &rel) {
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        for face_id in artifacts.face_ids {
            if face_id.len() >= 2 {
                let p0 = &face_id[..2];
                let rel = format!("faces/{p0}/{face_id}.webp");
                if let Some(p) = safe_join_relative(&cache_dir, &rel) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    })
    .await;
}

/// Reaps orphaned upload `.staging/*.part` files (>6h) and orphaned HLS temp dirs (>1h).
pub(crate) fn reap_stale_temp_files(uploads_dir: &Path, cache_dir: &Path) {
    let now = SystemTime::now();
    let staging_dir = uploads_dir.join(".staging");
    if let Ok(entries) = std::fs::read_dir(&staging_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("part") {
                if let Ok(meta) = entry.metadata() {
                    let age = meta
                        .modified()
                        .ok()
                        .and_then(|m| now.duration_since(m).ok())
                        .unwrap_or_default();
                    if age >= STALE_STAGING_PART_AGE {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }

    let hls_root = cache_dir.join("hls");
    if let Ok(shards) = std::fs::read_dir(&hls_root) {
        for shard in shards.flatten() {
            let shard_path = shard.path();
            if !shard_path.is_dir() {
                continue;
            }
            if let Ok(items) = std::fs::read_dir(&shard_path) {
                for item in items.flatten() {
                    let name = item.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.contains(".tmp-") || name_str.starts_with(".tmp-") {
                        if let Ok(meta) = item.metadata() {
                            let age = meta
                                .modified()
                                .ok()
                                .and_then(|m| now.duration_since(m).ok())
                                .unwrap_or_default();
                            if age >= STALE_HLS_TMP_AGE {
                                let p = item.path();
                                if p.is_dir() {
                                    let _ = std::fs::remove_dir_all(&p);
                                } else {
                                    let _ = std::fs::remove_file(&p);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Permanently purges soft-deleted assets (all trashed assets if `asset_ids` is `None`).
///
/// Honors each folder's `allow_disk_delete` policy, verifies volume UUID & symlink safety,
/// re-checks trash status right before physical deletion, and aborts DB removal if disk unlink fails.
pub async fn purge_assets_inner(
    db: &DbHandle,
    thumbs_dir: &Path,
    cache_dir: &Path,
    cache_mgr: &CacheManager,
    ai: Option<&Arc<AiCoordinator>>,
    asset_ids: Option<&[String]>,
) -> Result<PurgeReport, AppError> {
    let ids_owned = asset_ids.map(|s| s.to_vec());
    let candidates = db
        .read(move |conn| repo::assets::get_assets_for_purge(conn, ids_owned.as_deref()))
        .await?;

    let mut report = PurgeReport::default();

    for candidate in candidates {
        if candidate.is_offline {
            report.partial_errors.push(format!(
                "Skipped asset {} because folder {} is offline",
                candidate.asset_id, candidate.folder_id
            ));
            continue;
        }

        // Re-check right before purging so a concurrent Restore cannot be destroyed.
        let aid_check = candidate.asset_id.clone();
        let still_trashed = db
            .read(move |conn| repo::assets::is_asset_trashed(conn, &aid_check))
            .await?;
        if !still_trashed {
            continue;
        }

        let thumb_to_delete = candidate
            .thumb_rel_path
            .as_deref()
            .and_then(|rel| safe_join_relative(thumbs_dir, rel));

        if !candidate.allow_disk_delete {
            let aid = candidate.asset_id.clone();
            let faces_idx = ai.map(|a| Arc::clone(a.faces_index()));
            let now_ms = Utc::now().timestamp_millis();
            let (purged, artifacts) = db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    let artifacts = collect_artifacts_for_assets(&tx, std::slice::from_ref(&aid))?;
                    let purged = repo::assets::delete_purged_asset_records(&tx, &aid, true)?;
                    if purged {
                        if let Some(ref faces) = faces_idx {
                            for pid in &artifacts.affected_people {
                                let _ = crate::routes::people::recompute_person_centroid(
                                    &tx, faces, pid, now_ms,
                                );
                            }
                        }
                    }
                    tx.commit()?;
                    Ok((purged, artifacts))
                })
                .await?;
            if purged {
                if let Some(thumb) = thumb_to_delete {
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = std::fs::remove_file(&thumb);
                    })
                    .await;
                }
                let _ = cache_mgr
                    .purge_asset_cache(&candidate.asset_id, cache_dir)
                    .await;
                if let Some(ai_ref) = ai {
                    reap_cleanup_artifacts(
                        Arc::clone(ai_ref),
                        thumbs_dir.to_path_buf(),
                        cache_dir.to_path_buf(),
                        artifacts,
                    )
                    .await;
                }
                report.purged_count += 1;
                report.db_only_assets += 1;
            }
            continue;
        }

        let root_path = PathBuf::from(&candidate.folder_root_path);
        if !root_path.is_dir() {
            report.partial_errors.push(format!(
                "Skipped asset {} because folder path {} is unavailable",
                candidate.asset_id, candidate.folder_root_path
            ));
            continue;
        }

        if let Some(expected_vol) = candidate.folder_volume_uuid.as_deref() {
            let root_for_vol = root_path.clone();
            let actual_vol =
                tokio::task::spawn_blocking(move || resolve_volume_identity(&root_for_vol))
                    .await
                    .ok()
                    .and_then(|v| v.volume_uuid);
            if actual_vol.as_deref() != Some(expected_vol) {
                report.partial_errors.push(format!(
                    "Skipped asset {} because volume UUID did not match folder {}",
                    candidate.asset_id, candidate.folder_id
                ));
                continue;
            }
        }

        let mut files_to_delete: Vec<PathBuf> = Vec::with_capacity(candidate.files.len());
        let mut symlink_blocked = false;
        for f in &candidate.files {
            if let Some(safe) = safe_join_no_symlinks(&root_path, &f.rel_path) {
                files_to_delete.push(safe);
            } else {
                symlink_blocked = true;
                report.partial_errors.push(format!(
                    "Skipped unsafe or symlinked path '{}' for asset {}",
                    f.rel_path, candidate.asset_id
                ));
            }
        }
        if symlink_blocked {
            continue;
        }

        let (deleted_count, file_errors, physical_failed) =
            tokio::task::spawn_blocking(move || {
                let mut deleted = 0u64;
                let mut errs = Vec::new();
                let mut disk_fail = false;
                for path in files_to_delete {
                    if !path.exists() {
                        continue;
                    }
                    match delete_physical_file(&path) {
                        Ok(true) => deleted += 1,
                        Ok(false) => {}
                        Err(e) => {
                            disk_fail = true;
                            errs.push(format!("Failed to delete {}: {e}", path.display()));
                        }
                    }
                }
                if !disk_fail {
                    if let Some(thumb) = thumb_to_delete {
                        if thumb.exists() {
                            if let Err(e) = std::fs::remove_file(&thumb) {
                                if e.kind() != std::io::ErrorKind::NotFound {
                                    errs.push(format!(
                                        "Failed to remove thumbnail {}: {e}",
                                        thumb.display()
                                    ));
                                }
                            }
                        }
                    }
                }
                (deleted, errs, disk_fail)
            })
            .await
            .map_err(|e| AppError::internal(format!("Trash purge worker failed: {e}")))?;

        report.partial_errors.extend(file_errors);
        report.disk_deleted_files += deleted_count;

        // If any physical file failed to delete (e.g. Windows sharing violation), keep the asset
        // in Trash so it does not silently resurrect on the next folder scan.
        if physical_failed {
            continue;
        }

        if let Err(e) = cache_mgr
            .purge_asset_cache(&candidate.asset_id, cache_dir)
            .await
        {
            report.partial_errors.push(format!(
                "Failed to purge cache for {}: {e}",
                candidate.asset_id
            ));
        }

        let aid = candidate.asset_id.clone();
        let faces_idx = ai.map(|a| Arc::clone(a.faces_index()));
        let now_ms = Utc::now().timestamp_millis();
        let (purged, artifacts) = db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let artifacts = collect_artifacts_for_assets(&tx, std::slice::from_ref(&aid))?;
                let purged = repo::assets::delete_purged_asset_records(&tx, &aid, false)?;
                if purged {
                    if let Some(ref faces) = faces_idx {
                        for pid in &artifacts.affected_people {
                            let _ = crate::routes::people::recompute_person_centroid(
                                &tx, faces, pid, now_ms,
                            );
                        }
                    }
                }
                tx.commit()?;
                Ok((purged, artifacts))
            })
            .await?;

        if purged {
            if let Some(ai_ref) = ai {
                reap_cleanup_artifacts(
                    Arc::clone(ai_ref),
                    thumbs_dir.to_path_buf(),
                    cache_dir.to_path_buf(),
                    artifacts,
                )
                .await;
            }
            report.purged_count += 1;
        }
    }

    Ok(report)
}

/// Purges all trashed assets older than `retention_days` days using `purge_assets_inner`.
pub async fn run_trash_retention_sweep(
    db: &DbHandle,
    thumbs_dir: &Path,
    cache_dir: &Path,
    cache_mgr: &CacheManager,
    ai: Option<&Arc<AiCoordinator>>,
    retention_days: i64,
) -> Result<PurgeReport, AppError> {
    let cutoff_ms =
        Utc::now().timestamp_millis() - retention_days.max(0).saturating_mul(86_400_000);
    let expired_ids = db
        .read(move |conn| repo::assets::list_expired_trash(conn, cutoff_ms))
        .await?;

    if expired_ids.is_empty() {
        return Ok(PurgeReport::default());
    }

    purge_assets_inner(db, thumbs_dir, cache_dir, cache_mgr, ai, Some(&expired_ids)).await
}

/// Spawns the hourly cancellable 30-day automatic trash retention sweep background task.
pub fn spawn_trash_gc_task(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TRASH_SWEEP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip immediate t=0 tick so startup remains instantaneous.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = state.shutdown_token.cancelled() => break,
                _ = interval.tick() => {
                    let default_paths = state.default_paths.clone();
                    let settings = match state
                        .db
                        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
                        .await
                    {
                        Ok(s) => s,
                        Err(err) => {
                            tracing::warn!(error = %err, "failed to load settings for trash GC");
                            continue;
                        }
                    };

                    let now_ms = Utc::now().timestamp_millis();
                    let _ = state
                        .db
                        .write(move |conn| repo::sessions::prune_expired_sessions(conn, now_ms))
                        .await;

                    if let Err(err) = run_trash_retention_sweep(
                        &state.db,
                        Path::new(&settings.thumbnails_dir),
                        Path::new(&settings.cache_dir),
                        &state.cache_mgr,
                        Some(&state.ai),
                        DEFAULT_RETENTION_DAYS,
                    )
                    .await
                    {
                        tracing::warn!(error = ?err, "trash retention sweep failed");
                    }

                    let uploads_dir = PathBuf::from(&settings.managed_uploads_dir);
                    let cache_dir = PathBuf::from(&settings.cache_dir);
                    let _ = tokio::task::spawn_blocking(move || {
                        reap_stale_temp_files(&uploads_dir, &cache_dir);
                    })
                    .await;
                }
            }
        }
    })
}

fn delete_physical_file(path: &Path) -> std::io::Result<bool> {
    // On Windows, strip \\?\ prefix for short paths so shell Recycle Bin (trash::delete) works.
    #[cfg(windows)]
    let trash_target: PathBuf = {
        let s = path.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            if stripped.len() < 260 && !stripped.starts_with("UNC\\") {
                PathBuf::from(stripped)
            } else {
                path.to_path_buf()
            }
        } else {
            path.to_path_buf()
        }
    };
    #[cfg(not(windows))]
    let trash_target: PathBuf = path.to_path_buf();

    let attempts = if cfg!(windows) { 4 } else { 1 };
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..attempts {
        if trash::delete(&trash_target).is_ok() {
            return Ok(true);
        }
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < attempts {
                    std::thread::sleep(Duration::from_millis(80 * (attempt as u64 + 1)));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("failed to delete file")))
}
