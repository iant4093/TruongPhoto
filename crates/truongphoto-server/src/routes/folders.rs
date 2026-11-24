use crate::error::{ApiJson, AppError};
use crate::extractors::{RbacContext, RequireAdmin};
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
use truongphoto_core::{FolderSubfolderView, FolderView, RbacScope, WsServerEvent};
use truongphoto_db::{repo, DbError};
use truongphoto_media::volume::resolve_volume_identity;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct FolderListResponse {
    pub items: Vec<FolderView>,
}

#[derive(Debug, Serialize)]
pub struct FolderSubfolderListResponse {
    pub items: Vec<FolderSubfolderView>,
}

/// `GET /api/folders/{id}/subfolders` (Authenticated + RbacContext).
pub async fn list_folder_subfolders(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(folder_id): AxumPath<String>,
) -> Result<Json<FolderSubfolderListResponse>, AppError> {
    let scope = rbac.scope;
    let items = state
        .db
        .read(move |conn| repo::stacks::list_folder_subfolders(conn, &scope, &folder_id))
        .await?;
    Ok(Json(FolderSubfolderListResponse { items }))
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
    // Stop filesystem watching and wait for any in-flight scan of this folder to finish
    // BEFORE deleting DB records so a concurrent scan cannot re-insert rows mid-delete.
    state.watcher.unwatch_folder(&folder_id);
    let folder_lock = state.scanner.folder_lock(&folder_id);
    let _folder_guard = folder_lock.lock().await;

    let default_paths = state.default_paths.clone();
    let faces_idx = Arc::clone(state.ai.faces_index());
    let now_ms = Utc::now().timestamp_millis();

    let (cleanup, thumbnails_dir, cache_dir) = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;

            let folder_asset_ids: Vec<String> = {
                let mut stmt = tx.prepare("SELECT id FROM assets WHERE folder_id = ?1")?;
                let rows = stmt
                    .query_map([&fid_for_db], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };

            let artifacts =
                crate::ingest::trash_gc::collect_artifacts_for_assets(&tx, &folder_asset_ids)?;
            let settings = repo::settings::load_app_settings(&tx, &default_paths)?;

            let deleted = repo::folders::delete_folder(&tx, &fid_for_db)?;
            if !deleted {
                return Err(DbError::NotFound("Watched folder not found.".to_string()));
            }

            // Recompute or prune centroids for people whose faces belonged to the deleted folder.
            for pid in &artifacts.affected_people {
                let _ =
                    crate::routes::people::recompute_person_centroid(&tx, &faces_idx, pid, now_ms);
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
            Ok((
                artifacts,
                PathBuf::from(settings.thumbnails_dir),
                PathBuf::from(settings.cache_dir),
            ))
        })
        .await?;

    crate::ingest::trash_gc::reap_cleanup_artifacts(
        Arc::clone(&state.ai),
        thumbnails_dir,
        cache_dir,
        cleanup,
    )
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
