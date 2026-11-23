use crate::error::AppError;
use crate::ingest::cache_mgr::{safe_join_no_symlinks, safe_join_relative, CacheManager};
use crate::state::AppState;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use truongphoto_core::PurgeReport;
use truongphoto_db::{repo, DbHandle};
use truongphoto_media::volume::resolve_volume_identity;

const TRASH_SWEEP_INTERVAL: Duration = Duration::from_secs(3600);
const DEFAULT_RETENTION_DAYS: i64 = 30;

/// Permanently purges soft-deleted assets (all trashed assets if `asset_ids` is `None`).
///
/// Honors each folder's `allow_disk_delete` policy, verifies volume UUID & symlink safety,
/// re-checks trash status right before physical deletion, and aborts DB removal if disk unlink fails.
pub async fn purge_assets_inner(
    db: &DbHandle,
    thumbs_dir: &Path,
    cache_dir: &Path,
    cache_mgr: &CacheManager,
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
            let purged = db
                .write(move |conn| repo::assets::delete_purged_asset_records(conn, &aid, true))
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
        let purged = db
            .write(move |conn| repo::assets::delete_purged_asset_records(conn, &aid, false))
            .await?;

        if purged {
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

    purge_assets_inner(db, thumbs_dir, cache_dir, cache_mgr, Some(&expired_ids)).await
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
                        DEFAULT_RETENTION_DAYS,
                    )
                    .await
                    {
                        tracing::warn!(error = ?err, "trash retention sweep failed");
                    }
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
