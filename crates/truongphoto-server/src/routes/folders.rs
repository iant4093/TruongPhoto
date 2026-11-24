use crate::error::{ApiJson, AppError};
use crate::extractors::{RbacContext, RequireAdmin};
use crate::ingest::cache_mgr::safe_join_relative;
use crate::state::AppState;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use truongphoto_core::{AiTier, FolderView, RbacScope, WsServerEvent};
use truongphoto_db::{repo, DbError};
use truongphoto_media::volume::resolve_volume_identity;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct FolderListResponse {
    pub items: Vec<FolderView>,
}

#[derive(Debug, Serialize)]
pub struct FolderResponse {
    pub folder: FolderView,
}

#[derive(Debug, Deserialize)]
pub struct CreateFolderRequest {
    pub name: String,
    pub root_path: String,
    #[serde(default)]
    pub allow_disk_delete: bool,
    #[serde(default)]
    pub is_managed_uploads: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFolderRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub allow_disk_delete: Option<bool>,
    #[serde(default)]
    pub is_managed_uploads: Option<bool>,
}

/// `GET /api/folders` (Authenticated + RbacContext).
pub async fn list_folders(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
) -> Result<Json<FolderListResponse>, AppError> {
    let scope = rbac.scope;
    let items = state
        .db
        .read(move |conn| repo::folders::list_folders(conn, &scope))
        .await?;
    Ok(Json(FolderListResponse { items }))
}

/// `POST /api/folders` (RequireAdmin).
pub async fn create_folder(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<CreateFolderRequest>,
) -> Result<(StatusCode, Json<FolderResponse>), AppError> {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.len() > 128 {
        return Err(AppError::validation(
            "Folder name must be 1..=128 characters.",
        ));
    }

    let root_path = normalize_folder_path(&req.root_path);
    if root_path.is_empty() {
        return Err(AppError::validation("root_path cannot be empty."));
    }
    let path_obj = Path::new(&root_path);
    if !path_obj.exists() || !path_obj.is_dir() {
        return Err(AppError::validation(
            "root_path must refer to an existing directory.",
        ));
    }

    let vol = resolve_volume_identity(path_obj);
    let folder_id = Uuid::new_v4().to_string();
    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());

    let folder_id_for_db = folder_id.clone();
    let root_path_for_watch = PathBuf::from(&root_path);
    let view = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            repo::folders::create_folder(
                &tx,
                &repo::folders::CreateFolder {
                    id: folder_id_for_db.clone(),
                    name,
                    root_path,
                    volume_uuid: vol.volume_uuid,
                    volume_label: vol.volume_label,
                    is_managed_uploads: req.is_managed_uploads,
                    allow_disk_delete: req.allow_disk_delete,
                },
            )?;
            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "folder.create",
                    target_type: Some("folder"),
                    target_id: Some(&folder_id_for_db),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            let view = repo::folders::get_folder_view_by_id(&tx, &folder_id_for_db)?
                .ok_or_else(|| DbError::NotFound("created folder not found".to_string()))?;
            tx.commit()?;
            Ok(view)
        })
        .await?;

    state
        .watcher
        .watch_folder(&state, folder_id.clone(), root_path_for_watch);

    let _ = state.ws_tx.send(WsServerEvent::FolderChanged {
        folder_id,
        action: "created".to_string(),
    });

    Ok((StatusCode::CREATED, Json(FolderResponse { folder: view })))
}

/// `PATCH /api/folders/{id}` (RequireAdmin).
pub async fn update_folder(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(folder_id): AxumPath<String>,
    ApiJson(req): ApiJson<UpdateFolderRequest>,
) -> Result<Json<FolderResponse>, AppError> {
    let name = match req.name {
        Some(raw) => {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() || trimmed.len() > 128 {
                return Err(AppError::validation(
                    "Folder name must be 1..=128 characters.",
                ));
            }
            Some(trimmed)
        }
        None => None,
    };

    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());
    let fid_for_db = folder_id.clone();

    let view = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let updated = repo::folders::update_folder(
                &tx,
                &fid_for_db,
                &repo::folders::UpdateFolder {
                    name,
                    allow_disk_delete: req.allow_disk_delete,
                    is_managed_uploads: req.is_managed_uploads,
                    ..Default::default()
                },
            )?;
            if updated.is_none() {
                return Err(DbError::NotFound("Watched folder not found.".to_string()));
            }
            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "folder.update",
                    target_type: Some("folder"),
                    target_id: Some(&fid_for_db),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            let view = repo::folders::get_folder_view_by_id(&tx, &fid_for_db)?
                .ok_or_else(|| DbError::NotFound("Watched folder not found.".to_string()))?;
            tx.commit()?;
            Ok(view)
        })
        .await?;

    let _ = state.ws_tx.send(WsServerEvent::FolderChanged {
        folder_id,
        action: "updated".to_string(),
    });

    Ok(Json(FolderResponse { folder: view }))
}

/// `DELETE /api/folders/{id}` (RequireAdmin; removes DB row and derived caches/vectors only, never deletes user originals).
pub async fn delete_folder(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(folder_id): AxumPath<String>,
) -> Result<StatusCode, AppError> {
    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());
    let fid_for_db = folder_id.clone();
    let default_paths = state.default_paths.clone();
    let faces_idx = Arc::clone(state.ai.faces_index());
    let now_ms = Utc::now().timestamp_millis();

    struct FolderCleanupArtifacts {
        thumb_rel_paths: Vec<String>,
        cache_rel_paths: Vec<String>,
        vision_vids: Vec<(AiTier, u32)>,
        face_vids: Vec<u32>,
        thumbnails_dir: PathBuf,
        cache_dir: PathBuf,
    }

    let cleanup = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;

            let thumb_rel_paths: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT thumb_rel_path FROM assets WHERE folder_id = ?1 AND thumb_rel_path IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map([&fid_for_db], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };

            let cache_rel_paths: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT l.rel_path FROM lru_cache_entries l JOIN assets a ON a.id = l.asset_id WHERE a.folder_id = ?1",
                )?;
                let rows = stmt
                    .query_map([&fid_for_db], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };

            let vision_vids: Vec<(AiTier, u32)> = {
                let mut stmt = tx.prepare(
                    "SELECT ve.model_tier, ve.vector_id
                     FROM vision_embeddings ve
                     JOIN assets a ON a.id = ve.asset_id
                     WHERE a.folder_id = ?1",
                )?;
                let rows = stmt
                    .query_map([&fid_for_db], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })?
                    .filter_map(|r| {
                        let (tier_str, vid) = r.ok()?;
                        let tier = tier_str.parse::<AiTier>().ok()?;
                        let vid_u32 = u32::try_from(vid).ok()?;
                        Some((tier, vid_u32))
                    })
                    .collect();
                rows
            };

            let face_vids: Vec<u32> = {
                let mut stmt = tx.prepare(
                    "SELECT DISTINCT COALESCE(fe.vector_id, f.vector_id) AS vid
                     FROM faces f
                     JOIN assets a ON a.id = f.asset_id
                     LEFT JOIN face_embeddings fe ON fe.face_id = f.id
                     WHERE a.folder_id = ?1
                       AND COALESCE(fe.vector_id, f.vector_id) IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map([&fid_for_db], |row| row.get::<_, i64>(0))?
                    .filter_map(|r| r.ok().and_then(|v| u32::try_from(v).ok()))
                    .collect();
                rows
            };

            let affected_people: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT DISTINCT f.person_id
                     FROM faces f
                     JOIN assets a ON a.id = f.asset_id
                     WHERE a.folder_id = ?1
                       AND f.person_id IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map([&fid_for_db], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };

            let settings = repo::settings::load_app_settings(&tx, &default_paths)?;

            let deleted = repo::folders::delete_folder(&tx, &fid_for_db)?;
            if !deleted {
                return Err(DbError::NotFound("Watched folder not found.".to_string()));
            }

            // Recompute or prune centroids for people whose faces belonged to the deleted folder.
            for pid in &affected_people {
                let _ = crate::routes::people::recompute_person_centroid(&tx, &faces_idx, pid, now_ms);
            }

            repo::settings::insert_audit_log(
                &tx,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "folder.delete",
                    target_type: Some("folder"),
                    target_id: Some(&fid_for_db),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            tx.commit()?;
            Ok(FolderCleanupArtifacts {
                thumb_rel_paths,
                cache_rel_paths,
                vision_vids,
                face_vids,
                thumbnails_dir: PathBuf::from(settings.thumbnails_dir),
                cache_dir: PathBuf::from(settings.cache_dir),
            })
        })
        .await?;

    state.watcher.unwatch_folder(&folder_id);

    // Tombstone deleted vector slots and remove derived thumbs/caches off the request thread.
    let ai_cleanup = Arc::clone(&state.ai);
    let _ = tokio::task::spawn_blocking(move || {
        for (tier, vid) in cleanup.vision_vids {
            if let Ok(idx) = ai_cleanup.vision_index(tier) {
                let _ = idx.mark_deleted(vid);
            }
        }
        let faces = ai_cleanup.faces_index();
        for fvid in cleanup.face_vids {
            let _ = faces.mark_deleted(fvid);
        }
        for rel in cleanup.thumb_rel_paths {
            if let Some(p) = safe_join_relative(&cleanup.thumbnails_dir, &rel) {
                let _ = std::fs::remove_file(p);
            }
        }
        for rel in cleanup.cache_rel_paths {
            if let Some(p) = safe_join_relative(&cleanup.cache_dir, &rel) {
                // Cache entries include directories (hls/, raw/, live/) as well as files.
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    })
    .await;

    let _ = state.ws_tx.send(WsServerEvent::FolderChanged {
        folder_id,
        action: "deleted".to_string(),
    });

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
pub struct ScanFolderResponse {
    pub job_id: i64,
    pub folder_id: String,
}

#[derive(Debug, Serialize)]
pub struct ScanAllResponse {
    pub job_ids: Vec<i64>,
}

/// `POST /api/folders/{id}/scan` (RequireAdmin; 202 Accepted).
pub async fn scan_folder(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(folder_id): AxumPath<String>,
) -> Result<(StatusCode, Json<ScanFolderResponse>), AppError> {
    let job_id = state
        .scanner
        .enqueue_folder_scan(&state, folder_id.clone())
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ScanFolderResponse { job_id, folder_id }),
    ))
}

/// `POST /api/folders/scan-all` (RequireAdmin; 202 Accepted).
pub async fn scan_all_folders(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<ScanAllResponse>), AppError> {
    let folders = state
        .db
        .read(|conn| repo::folders::list_folders(conn, &RbacScope::AdminAll))
        .await?;

    let mut job_ids = Vec::with_capacity(folders.len());
    for folder in folders {
        let job_id = state.scanner.enqueue_folder_scan(&state, folder.id).await?;
        job_ids.push(job_id);
    }

    Ok((StatusCode::ACCEPTED, Json(ScanAllResponse { job_ids })))
}

fn normalize_folder_path(raw: &str) -> String {
    truongphoto_core::process::normalize_folder_path(raw)
}
