use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use truongphoto_core::BackupSnapshotView;
use truongphoto_db::repo;

const BACKUP_CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Scans `backup_dir` for `library-*.db` snapshots and returns them sorted newest-first.
pub fn list_backup_snapshots(backup_dir: &Path) -> Vec<BackupSnapshotView> {
    let Ok(entries) = std::fs::read_dir(backup_dir) else {
        return Vec::new();
    };

    let mut snapshots = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(filename) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if !filename.starts_with("library-") || !filename.ends_with(".db") {
            continue;
        }

        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let created_at_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0);

        snapshots.push(BackupSnapshotView {
            filename,
            path: path.to_string_lossy().into_owned(),
            size_bytes: meta.len(),
            created_at_ms,
        });
    }

    snapshots.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| b.filename.cmp(&a.filename))
    });
    snapshots
}

/// Evaluates whether a rolling backup is due according to `backup_interval_hours`
/// and runs `db.backup_to_dir` when elapsed.
pub async fn run_scheduled_backup_check(
    state: &Arc<AppState>,
) -> Result<Option<BackupSnapshotView>, AppError> {
    let default_paths = state.default_paths.clone();
    let settings = state
        .db
        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
        .await?;

    if settings.backup_interval_hours == 0 {
        return Ok(None);
    }

    let backup_dir = PathBuf::from(&settings.backup_dir);
    let scan_dir = backup_dir.clone();
    let existing = tokio::task::spawn_blocking(move || list_backup_snapshots(&scan_dir))
        .await
        .map_err(|e| AppError::internal(format!("Backup directory scan failed: {e}")))?;

    let interval_ms = i64::from(settings.backup_interval_hours)
        .saturating_mul(3_600)
        .saturating_mul(1_000);
    let now_ms = Utc::now().timestamp_millis();

    if let Some(latest) = existing.first() {
        if now_ms.saturating_sub(latest.created_at_ms) < interval_ms {
            return Ok(None);
        }
    }

    let keep_count = usize::try_from(settings.backup_keep_count.max(1)).unwrap_or(7);
    let result = state.db.backup_to_dir(&backup_dir, keep_count).await?;

    let filename = result
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("library.db")
        .to_string();

    Ok(Some(BackupSnapshotView {
        filename,
        path: result.path.to_string_lossy().into_owned(),
        size_bytes: result.size_bytes,
        created_at_ms: result.created_at_ms,
    }))
}

/// Spawns the 15-minute cancellable rolling backup scheduler background task.
pub fn spawn_backup_scheduler(
    state: Arc<AppState>,
    shutdown_token: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(BACKUP_CHECK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip immediate t=0 tick so startup remains instantaneous.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = shutdown_token.cancelled() => break,
                _ = interval.tick() => {
                    let initialized = state
                        .db
                        .read(repo::settings::is_setup_completed)
                        .await
                        .unwrap_or(false);
                    if !initialized {
                        continue;
                    }
                    match run_scheduled_backup_check(&state).await {
                        Ok(Some(snapshot)) => {
                            tracing::info!(
                                filename = %snapshot.filename,
                                size_bytes = snapshot.size_bytes,
                                "scheduled rolling SQLite backup completed"
                            );
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::warn!(error = ?err, "scheduled rolling SQLite backup failed");
                        }
                    }
                }
            }
        }
    })
}
