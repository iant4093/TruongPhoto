use crate::ai::worker::run_face_clustering;
use crate::error::{ApiJson, AppError};
use crate::extractors::{RbacContext, RequireAdmin};
use crate::routes::library::AssetListResponse;
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    response::Response,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use truongphoto_ai::{
    crop_face_thumbnail_webp, encode_centroid_le, l2_normalize, HybridVectorIndex,
};
use truongphoto_core::{
    ClusterFacesResponse, MergePeopleRequest, MergePeopleResponse, PersonSummary,
    ReassignFaceRequest, ReassignFaceResponse, UpdatePersonRequest,
};
use truongphoto_db::{repo, rusqlite::Connection, DbError};
use truongphoto_media::decode::decode_image_to_srgb;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ListPeopleQuery {
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct PersonListResponse {
    pub items: Vec<PersonSummary>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PersonAssetsQuery {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

fn atomic_write_bytes_sync(dest: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    truongphoto_core::process::atomic_write_bytes(dest, bytes)
}

pub(crate) fn recompute_person_centroid(
    conn: &Connection,
    faces_index: &HybridVectorIndex,
    person_id: &str,
    now_ms: i64,
) -> Result<(), DbError> {
    let vids = repo::people::list_face_vector_ids_for_person(conn, person_id)?;
    let mut count = 0u64;
    let mut sum: Vec<f32> = Vec::new();
    for vid in vids {
        if let Some(vec) = faces_index.flat().get_f32(vid) {
            if sum.is_empty() {
                sum = vec![0.0; vec.len()];
            }
            for (acc, &v) in sum.iter_mut().zip(vec.iter()) {
                *acc += v;
            }
            count += 1;
        }
    }

    if count == 0 || sum.is_empty() {
        repo::ai::delete_person_centroid(conn, person_id)?;
    } else {
        let centroid = l2_normalize(&sum);
        let blob = encode_centroid_le(&centroid);
        repo::ai::upsert_person_centroid(conn, person_id, count, &blob, now_ms)?;
    }
    Ok(())
}

/// `GET /api/people` (AuthSession + RbacContext; `?include_hidden=true|false`, Admin-only for hidden).
pub async fn list_people(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListPeopleQuery>,
) -> Result<Json<PersonListResponse>, AppError> {
    let scope = rbac.scope;
    let include_hidden = query.include_hidden.unwrap_or(false) && rbac.auth.user.role.is_admin();
    let items = state
        .db
        .read(move |conn| repo::people::list_people_filtered(conn, &scope, include_hidden))
        .await?;
    Ok(Json(PersonListResponse { items }))
}

/// `GET /api/people/{id}` (AuthSession + RbacContext; `404` if 0 active faces in allowed folders).
pub async fn get_person(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(person_id): AxumPath<String>,
) -> Result<Json<PersonSummary>, AppError> {
    let scope = rbac.scope;
    let is_admin = rbac.auth.user.role.is_admin();
    let person = state
        .db
        .read(move |conn| repo::people::get_person_by_id(conn, &scope, &person_id))
        .await?
        .filter(|p| is_admin || !p.is_hidden)
        .ok_or_else(|| AppError::not_found("Person not found."))?;

    Ok(Json(person))
}

/// `GET /api/people/{id}/assets` (AuthSession + RbacContext; `?cursor&limit`).
pub async fn list_person_assets(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(person_id): AxumPath<String>,
    Query(query): Query<PersonAssetsQuery>,
) -> Result<Json<AssetListResponse>, AppError> {
    let scope = rbac.scope;
    let is_admin = rbac.auth.user.role.is_admin();
    let cursor = query.cursor;
    let limit = query.limit.unwrap_or(100);

    let (items, next_cursor, total_count) = state
        .db
        .read(move |conn| {
            let person = repo::people::get_person_by_id(conn, &scope, &person_id)?;
            let visible = person.filter(|p| is_admin || !p.is_hidden);
            if visible.is_none() {
                return Err(DbError::NotFound("Person not found.".to_string()));
            }
            repo::people::list_person_assets(conn, &scope, &person_id, cursor.as_deref(), limit)
        })
        .await?;

    Ok(Json(AssetListResponse {
        items,
        next_cursor,
        total_count,
    }))
}

/// `GET /api/media/faces/{face_id}/thumb` (AuthSession + RbacContext; `404` outside caller's `RbacScope`).
pub async fn get_face_thumb(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(face_id): AxumPath<String>,
) -> Result<Response, AppError> {
    let scope = rbac.scope;
    let fid = face_id.clone();
    let default_paths = state.default_paths.clone();

    let (src_opt, settings) = state
        .db
        .read(move |conn| {
            let src = repo::ai::get_face_with_asset_for_thumb(conn, &scope, &fid)?;
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            Ok((src, settings))
        })
        .await?;

    let src = src_opt.ok_or_else(|| AppError::not_found("Face not found."))?;
    let p0 = &face_id[..face_id.len().min(2)];
    let cache_path = PathBuf::from(&settings.cache_dir)
        .join("faces")
        .join(p0)
        .join(format!("{face_id}.webp"));

    let bytes = if cache_path.is_file() {
        tokio::fs::read(&cache_path)
            .await
            .map_err(|e| AppError::internal(format!("Failed to read cached face thumb: {e}")))?
    } else {
        let lock_key = format!("face_thumb:{face_id}");
        let _guard = state.cache_mgr.singleflight_lock(&lock_key).await;

        if cache_path.is_file() {
            tokio::fs::read(&cache_path)
                .await
                .map_err(|e| AppError::internal(format!("Failed to read cached face thumb: {e}")))?
        } else {
            let img_path = crate::ingest::cache_mgr::safe_join_no_symlinks(
                Path::new(&src.folder_root_path),
                &src.primary_rel_path,
            )
            .ok_or_else(|| AppError::not_found("Invalid media file path."))?;
            if !img_path.is_file() {
                return Err(AppError::not_found("Media file does not exist on disk."));
            }

            let bbox = (src.bbox_x, src.bbox_y, src.bbox_w, src.bbox_h);
            let dest_clone = cache_path.clone();
            let _decode_permit = state.cache_mgr.acquire_image_decode_permit().await;
            tokio::task::spawn_blocking(move || {
                let rgb = decode_image_to_srgb(&img_path).map_err(|e| {
                    AppError::internal(format!("Failed to decode face source: {e}"))
                })?;
                let webp = crop_face_thumbnail_webp(&rgb, bbox)
                    .map_err(|e| AppError::internal(format!("Failed to crop face thumb: {e}")))?;
                atomic_write_bytes_sync(&dest_clone, &webp)
                    .map_err(|e| AppError::internal(format!("Failed to cache face thumb: {e}")))?;
                Ok::<Vec<u8>, AppError>(webp)
            })
            .await
            .map_err(|e| AppError::internal(format!("Face thumbnail worker panicked: {e}")))??
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "image/webp")
        .header(CACHE_CONTROL, "private, max-age=86400")
        .body(Body::from(bytes))
        .map_err(|e| AppError::internal(format!("Failed to build face thumb response: {e}")))
}

/// `PATCH /api/people/{id}` (RequireAdmin).
pub async fn update_person(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath(person_id): AxumPath<String>,
    ApiJson(req): ApiJson<UpdatePersonRequest>,
) -> Result<Json<PersonSummary>, AppError> {
    let now_ms = Utc::now().timestamp_millis();
    let updated = state
        .db
        .write(move |conn| repo::people::update_person(conn, &person_id, &req, now_ms))
        .await?
        .ok_or_else(|| AppError::not_found("Person not found."))?;
    Ok(Json(updated))
}

/// `POST /api/people/merge` (RequireAdmin; merges faces and recomputes target centroid).
pub async fn merge_people(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<MergePeopleRequest>,
) -> Result<Json<MergePeopleResponse>, AppError> {
    if req.target_person_id.trim().is_empty() {
        return Err(AppError::validation("target_person_id must not be empty."));
    }

    let now_ms = Utc::now().timestamp_millis();
    let faces_idx = Arc::clone(state.ai.faces_index());
    let target_id = req.target_person_id.clone();
    let source_ids = req.source_person_ids;

    let (person, merged_faces) = state
        .db
        .write(move |conn| {
            let Some((person, merged_faces)) =
                repo::people::merge_people(conn, &source_ids, &target_id, now_ms)?
            else {
                return Err(DbError::NotFound("Target person not found.".to_string()));
            };
            recompute_person_centroid(conn, &faces_idx, &target_id, now_ms)?;
            Ok((person, merged_faces))
        })
        .await?;

    Ok(Json(MergePeopleResponse {
        person,
        merged_faces,
    }))
}

/// `POST /api/people/{id}/faces/{face_id}/reassign` (RequireAdmin).
pub async fn reassign_face(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    AxumPath((person_id, face_id)): AxumPath<(String, String)>,
    ApiJson(req): ApiJson<ReassignFaceRequest>,
) -> Result<Json<ReassignFaceResponse>, AppError> {
    let now_ms = Utc::now().timestamp_millis();
    let faces_idx = Arc::clone(state.ai.faces_index());

    let res = state
        .db
        .write(move |conn| {
            let (res, old_person_id) = repo::people::reassign_face_returning_old(
                conn,
                &face_id,
                req.target_person_id.as_deref(),
                req.new_person_name.as_deref(),
                now_ms,
            )?
            .ok_or_else(|| DbError::NotFound("Face not found.".to_string()))?;

            if let Some(ref old_pid) = old_person_id {
                let _ = recompute_person_centroid(conn, &faces_idx, old_pid, now_ms);
            } else if !person_id.is_empty() && person_id != "unassigned" {
                let _ = recompute_person_centroid(conn, &faces_idx, &person_id, now_ms);
            }
            if let Some(ref new_pid) = res.person_id {
                if old_person_id.as_deref() != Some(new_pid.as_str()) {
                    let _ = recompute_person_centroid(conn, &faces_idx, new_pid, now_ms);
                }
            }
            Ok(res)
        })
        .await?;

    Ok(Json(res))
}

/// `POST /api/people/cluster` (RequireAdmin -> `202 Accepted` `ClusterFacesResponse`).
pub async fn cluster_people(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<ClusterFacesResponse>), AppError> {
    let _worker_guard = state.ai.worker_lock.lock().await;
    let res = run_face_clustering(&state).await?;
    Ok((StatusCode::ACCEPTED, Json(res)))
}
