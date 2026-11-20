use crate::error::AppError;
use crate::ingest::cache_mgr::{safe_join_relative, CacheManager};
use crate::state::AppState;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use truongphoto_core::PurgeReport;
use truongphoto_db::{repo, DbHandle};

const TRASH_SWEEP_INTERVAL: Duration = Duration::from_secs(3600);
const DEFAULT_RETENTION_DAYS: i64 = 30;

/// Permanently purges soft-deleted assets (all trashed assets if `asset_ids` is `None`).
///
/// Honors each folder's `allow_disk_delete` policy and skips offline folders.
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

        if !candidate.allow_disk_delete {
            let aid = candidate.asset_id.clone();
            db.write(move |conn| repo::assets::delete_purged_asset_records(conn, &aid, true))
                .await?;
            report.purged_count += 1;
            report.db_only_assets += 1;
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

        let files_to_delete: Vec<PathBuf> = candidate
            .files
            .iter()
            .filter_map(|f| safe_join_relative(&root_path, &f.rel_path))
            .collect();

        let thumb_to_delete = candidate
            .thumb_rel_path
            .as_deref()
            .and_then(|rel| safe_join_relative(thumbs_dir, rel));

        let (deleted_count, file_errors) = tokio::task::spawn_blocking(move || {
            let mut deleted = 0u64;
            let mut errs = Vec::new();
            for path in files_to_delete {
                if !path.exists() {
                    continue;
                }
                match delete_physical_file(&path) {
                    Ok(true) => deleted += 1,
                    Ok(false) => {}
                    Err(e) => errs.push(format!("Failed to delete {}: {e}", path.display())),
                }
            }
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
            (deleted, errs)
        })
        .await
        .map_err(|e| AppError::internal(format!("Trash purge worker failed: {e}")))?;

        report.partial_errors.extend(file_errors);

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
        db.write(move |conn| repo::assets::delete_purged_asset_records(conn, &aid, false))
            .await?;

        report.purged_count += 1;
        report.disk_deleted_files += deleted_count;
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
    if trash::delete(path).is_ok() {
        return Ok(true);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
