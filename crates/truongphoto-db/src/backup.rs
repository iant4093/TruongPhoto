use crate::error::{DbError, DbResult};
use chrono::Utc;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Result metadata returned by a rolling `VACUUM INTO` snapshot backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupResult {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub created_at_ms: i64,
    pub pruned_files: usize,
}

/// Runs a consistent WAL snapshot backup via `VACUUM INTO` and prunes older snapshots.
pub async fn backup_to_dir(
    db_path: PathBuf,
    backup_dir: PathBuf,
    keep_count: usize,
) -> DbResult<BackupResult> {
    tokio::task::spawn_blocking(move || backup_blocking(&db_path, &backup_dir, keep_count))
        .await
        .map_err(|e| DbError::TaskJoin(e.to_string()))?
}

fn backup_blocking(db_path: &Path, backup_dir: &Path, keep_count: usize) -> DbResult<BackupResult> {
    std::fs::create_dir_all(backup_dir)?;

    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(db_path, flags)?;
    conn.execute_batch("PRAGMA busy_timeout = 5000;")?;

    let (now, final_path, tmp_path) = loop {
        let now = Utc::now();
        let stamp = now.format("%Y%m%d-%H%M%S-%3f").to_string();
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let final_name = format!("library-{stamp}.db");
        let tmp_name = format!(".library-{stamp}-{}.db.tmp", &unique[..8]);
        let final_path = backup_dir.join(final_name);
        let tmp_path = backup_dir.join(tmp_name);
        if !final_path.exists() && !tmp_path.exists() {
            break (now, final_path, tmp_path);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };

    let tmp_str = tmp_path.to_str().ok_or_else(|| {
        DbError::Backup(format!(
            "backup temp path is not valid UTF-8: {}",
            tmp_path.display()
        ))
    })?;

    if let Err(err) = conn.execute("VACUUM INTO ?1", [tmp_str]) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(DbError::Sqlite(err));
    }

    let meta = match std::fs::metadata(&tmp_path) {
        Ok(m) if m.len() > 0 => m,
        Ok(_) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(DbError::Backup(
                "VACUUM INTO produced an empty backup file".to_string(),
            ));
        }
        Err(err) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(DbError::Io(err));
        }
    };

    std::fs::rename(&tmp_path, &final_path)?;

    let pruned_files = prune_old_backups(backup_dir, keep_count.max(1))?;

    Ok(BackupResult {
        path: final_path,
        size_bytes: meta.len(),
        created_at_ms: now.timestamp_millis(),
        pruned_files,
    })
}

fn prune_old_backups(backup_dir: &Path, keep_count: usize) -> DbResult<usize> {
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(backup_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("library-") && name.ends_with(".db") {
                entries.push((name.to_string(), path));
            }
        }
    }

    // Filenames use UTC YYYYMMDD-HHMMSS-mmm, so descending lexicographical order is newest-first.
    entries.sort_by(|a, b| b.0.cmp(&a.0));

    let mut pruned = 0usize;
    for (_, path) in entries.into_iter().skip(keep_count) {
        match std::fs::remove_file(&path) {
            Ok(()) => pruned += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(DbError::Io(e)),
        }
    }

    Ok(pruned)
}
