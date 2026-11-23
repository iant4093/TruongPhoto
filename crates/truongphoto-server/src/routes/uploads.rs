use crate::error::AppError;
use crate::extractors::RequireContributor;
use crate::ingest::scanner::ingest_single_file;
use crate::state::AppState;
use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    Json,
};
use chrono::{Datelike, Utc};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use truongphoto_core::{FolderRecord, RbacScope, UploadItemError, UploadResponse};
use truongphoto_db::{repo, rusqlite::params};
use truongphoto_media::{
    classify::{classify_extension, is_ignored_entry, normalized_extension},
    volume::resolve_volume_identity,
};
use uuid::Uuid;

/// `POST /api/uploads` (RequireContributor; streams multipart chunks directly to disk in O(1) RAM).
pub async fn upload_files(
    RequireContributor(auth): RequireContributor,
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), AppError> {
    let default_paths = state.default_paths.clone();
    let settings = state
        .db
        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
        .await?;

    let mut requested_folder_id: Option<String> = None;
    let mut cached_target_folder: Option<FolderRecord> = None;
    let mut uploaded = Vec::new();
    let mut errors = Vec::new();

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::validation(format!("Invalid multipart upload payload: {e}")))?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name == "folder_id" && field.file_name().is_none() {
            let val = field.text().await.map_err(|e| {
                AppError::validation(format!("Failed to read folder_id field: {e}"))
            })?;
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                requested_folder_id = Some(trimmed);
                cached_target_folder = None;
            }
            continue;
        }

        let Some(raw_filename) = field.file_name().map(str::to_string) else {
            continue;
        };

        let sanitized = sanitize_upload_filename(&raw_filename);
        let ext_opt = normalized_extension(Path::new(&sanitized));
        let supported = ext_opt.as_deref().and_then(classify_extension).is_some();

        if is_ignored_entry(&sanitized) || !supported {
            // Drain remaining bytes cleanly before reporting per-item error.
            while let Ok(Some(_)) = field.chunk().await {}
            errors.push(UploadItemError {
                filename: raw_filename,
                message: "Unsupported or ignored media file type.".to_string(),
            });
            continue;
        }

        let target_folder = if let Some(ref folder) = cached_target_folder {
            folder.clone()
        } else {
            let folder = resolve_upload_target_folder(
                &state,
                &auth.user.id,
                auth.user.role,
                &settings.managed_uploads_dir,
                requested_folder_id.as_deref(),
            )
            .await?;
            cached_target_folder = Some(folder.clone());
            folder
        };

        let root_dir = PathBuf::from(&target_folder.root_path);
        let staging_dir = root_dir.join(".staging");
        tokio::fs::create_dir_all(&staging_dir)
            .await
            .map_err(|e| AppError::internal(format!("Failed to create upload staging dir: {e}")))?;

        let staging_path = staging_dir.join(format!("{}.part", Uuid::new_v4()));
        let mut staging_guard = StagingFileGuard::new(staging_path.clone());
        let mut out_file = tokio::fs::File::create(&staging_path)
            .await
            .map_err(|e| AppError::internal(format!("Failed to create staging file: {e}")))?;

        let mut total_bytes: u64 = 0;
        let mut stream_err: Option<String> = None;

        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    total_bytes = total_bytes.saturating_add(chunk.len() as u64);
                    if let Err(e) = out_file.write_all(&chunk).await {
                        stream_err = Some(format!("Failed writing upload chunk: {e}"));
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    stream_err = Some(format!("Failed reading upload stream: {e}"));
                    break;
                }
            }
        }

        if let Some(msg) = stream_err {
            drop(out_file);
            drop(staging_guard);
            errors.push(UploadItemError {
                filename: raw_filename,
                message: msg,
            });
            continue;
        }

        if let Err(e) = out_file.flush().await {
            drop(out_file);
            drop(staging_guard);
            errors.push(UploadItemError {
                filename: raw_filename,
                message: format!("Failed flushing upload file: {e}"),
            });
            continue;
        }
        drop(out_file);

        if total_bytes == 0 {
            drop(staging_guard);
            errors.push(UploadItemError {
                filename: raw_filename,
                message: "Uploaded file is empty (0 bytes).".to_string(),
            });
            continue;
        }

        let now = Utc::now();
        let month_dir = root_dir
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", now.month()));
        tokio::fs::create_dir_all(&month_dir)
            .await
            .map_err(|e| AppError::internal(format!("Failed to create upload month dir: {e}")))?;

        let staging_for_move = staging_path.clone();
        let month_for_move = month_dir.clone();
        let sanitized_for_move = sanitized.clone();
        let dest_path = match tokio::task::spawn_blocking(move || {
            finalize_staging_file_no_overwrite(
                &staging_for_move,
                &month_for_move,
                &sanitized_for_move,
            )
        })
        .await
        {
            Ok(Ok(dest)) => {
                staging_guard.disarm();
                dest
            }
            Ok(Err(e)) => {
                drop(staging_guard);
                errors.push(UploadItemError {
                    filename: raw_filename,
                    message: format!("Failed to finalize upload file: {e}"),
                });
                continue;
            }
            Err(e) => {
                drop(staging_guard);
                errors.push(UploadItemError {
                    filename: raw_filename,
                    message: format!("Failed to finalize upload file: {e}"),
                });
                continue;
            }
        };

        match ingest_single_file(&state, &target_folder, &dest_path).await {
            Ok(Some(asset_id)) => {
                let aid = asset_id.clone();
                let summary_opt = state
                    .db
                    .read(move |conn| {
                        repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &aid)
                    })
                    .await?
                    .map(|d| d.summary);
                if let Some(summary) = summary_opt {
                    uploaded.push(summary);
                }
            }
            Ok(None) => {
                let _ = tokio::fs::remove_file(&dest_path).await;
                errors.push(UploadItemError {
                    filename: raw_filename,
                    message: "Uploaded file was skipped by media classifier.".to_string(),
                });
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&dest_path).await;
                errors.push(UploadItemError {
                    filename: raw_filename,
                    message: format!("Failed to ingest uploaded file: {e}"),
                });
            }
        }
    }

    Ok((
        StatusCode::CREATED,
        Json(UploadResponse { uploaded, errors }),
    ))
}

async fn resolve_upload_target_folder(
    state: &Arc<AppState>,
    user_id: &str,
    role: truongphoto_core::Role,
    managed_uploads_dir: &str,
    requested_folder_id: Option<&str>,
) -> Result<FolderRecord, AppError> {
    if let Some(fid) = requested_folder_id {
        let fid_owned = fid.to_string();
        let uid = user_id.to_string();
        let (folder_opt, allowed) = state
            .db
            .read(move |conn| {
                let folder = repo::folders::get_folder_by_id(conn, &fid_owned)?;
                let allowed = if role.is_admin() {
                    true
                } else {
                    repo::users::get_allowed_folder_ids(conn, &uid)?
                        .iter()
                        .any(|id| id == &fid_owned)
                };
                Ok((folder, allowed))
            })
            .await?;

        if !allowed {
            return Err(AppError::forbidden(
                "You do not have permission to upload into this folder.",
            ));
        }
        let folder = folder_opt.ok_or_else(|| AppError::not_found("Target folder not found."))?;
        if !Path::new(&folder.root_path).is_dir() {
            return Err(AppError::validation(
                "Target folder directory is not accessible.",
            ));
        }
        return Ok(folder);
    }

    let root_path = PathBuf::from(managed_uploads_dir);
    tokio::fs::create_dir_all(&root_path)
        .await
        .map_err(|e| AppError::internal(format!("Failed to create managed uploads dir: {e}")))?;

    let root_clone = root_path.clone();
    let vol = tokio::task::spawn_blocking(move || resolve_volume_identity(&root_clone))
        .await
        .map_err(|e| AppError::internal(format!("Volume lookup panicked: {e}")))?;

    let root_str = managed_uploads_dir.to_string();
    let uid = user_id.to_string();
    let now_ms = Utc::now().timestamp_millis();

    let folder = state
        .db
        .write(move |conn| {
            let (folder, newly_created) = repo::folders::find_or_create_managed_uploads_folder(
                conn,
                &root_str,
                vol.volume_uuid.as_deref(),
                vol.volume_label.as_deref(),
            )?;
            if !role.is_admin() {
                if newly_created {
                    conn.execute(
                        "INSERT OR IGNORE INTO user_folder_acls (user_id, folder_id, granted_at_ms)
                         VALUES (?1, ?2, ?3)",
                        params![uid, folder.id, now_ms],
                    )?;
                } else {
                    let allowed = repo::users::get_allowed_folder_ids(conn, &uid)?
                        .iter()
                        .any(|id| id == &folder.id);
                    if !allowed {
                        return Err(truongphoto_db::DbError::InvalidArgument(
                            "__FORBIDDEN__".to_string(),
                        ));
                    }
                }
            }
            Ok(folder)
        })
        .await
        .map_err(|e| match e {
            truongphoto_db::DbError::InvalidArgument(ref m) if m == "__FORBIDDEN__" => {
                AppError::forbidden(
                    "You do not have permission to upload into the managed uploads folder.",
                )
            }
            other => AppError::from(other),
        })?;

    state
        .watcher
        .watch_folder(state, folder.id.clone(), PathBuf::from(&folder.root_path));

    Ok(folder)
}

fn sanitize_upload_filename(raw: &str) -> String {
    let base = Path::new(raw)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.bin");
    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while cleaned.starts_with('.') {
        cleaned.remove(0);
    }
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "upload.bin".to_string()
    } else {
        trimmed.to_string()
    }
}

struct StagingFileGuard {
    path: Option<PathBuf>,
}

impl StagingFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for StagingFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn finalize_staging_file_no_overwrite(
    staging_path: &Path,
    dir: &Path,
    filename: &str,
) -> std::io::Result<PathBuf> {
    let p = Path::new(filename);
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("upload");
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");

    for attempt in 0..16 {
        let candidate = if attempt == 0 {
            dir.join(filename)
        } else {
            let short_id = &Uuid::new_v4().to_string()[..8];
            let unique_name = if ext.is_empty() {
                format!("{stem}_{short_id}")
            } else {
                format!("{stem}_{short_id}.{ext}")
            };
            dir.join(unique_name)
        };

        match std::fs::hard_link(staging_path, &candidate) {
            Ok(()) => {
                let _ = std::fs::remove_file(staging_path);
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // Fallback for filesystems where hard links are unsupported (e.g., exFAT/SMB).
                if !candidate.exists() {
                    std::fs::rename(staging_path, &candidate)?;
                    return Ok(candidate);
                }
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Could not allocate unique destination filename",
    ))
}
