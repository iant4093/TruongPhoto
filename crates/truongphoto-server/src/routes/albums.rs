use crate::error::{ApiJson, AppError};
use crate::extractors::{AuthSession, RbacContext};
use crate::state::AppState;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use truongphoto_core::{
    AlbumDetail, AlbumSharePermission, AlbumShareView, AlbumSortOrder, AlbumSummary,
};
use truongphoto_db::{repo, DbError};

fn ensure_contributor(rbac: &RbacContext) -> Result<(), AppError> {
    if !rbac.auth.user.role.can_contribute() {
        return Err(AppError::forbidden(
            "Contributor or Administrator privileges are required.",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct AlbumListResponse {
    pub items: Vec<AlbumSummary>,
}

#[derive(Debug, Serialize)]
pub struct AlbumResponse {
    pub album: AlbumSummary,
}

#[derive(Debug, Deserialize)]
pub struct CreateAlbumRequest {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub asset_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAlbumRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub cover_asset_id: Option<String>,
    #[serde(default)]
    pub sort_order: Option<AlbumSortOrder>,
}

#[derive(Debug, Deserialize)]
pub struct AlbumAssetsRequest {
    pub asset_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AddedCountResponse {
    pub added_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RemovedCountResponse {
    pub removed_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct ShareInput {
    pub user_id: String,
    pub permission: AlbumSharePermission,
}

#[derive(Debug, Deserialize)]
pub struct ReplaceSharesRequest {
    pub shares: Vec<ShareInput>,
}

#[derive(Debug, Serialize)]
pub struct SharesResponse {
    pub shares: Vec<AlbumShareView>,
}

/// `GET /api/albums` (Authenticated + RbacContext).
pub async fn list_albums(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AlbumListResponse>, AppError> {
    let scope = rbac.scope;
    let user_id = rbac.auth.user.id;
    let items = state
        .db
        .read(move |conn| repo::albums::list_albums(conn, &user_id, &scope))
        .await?;
    Ok(Json(AlbumListResponse { items }))
}

/// `POST /api/albums` (RequireContributor).
pub async fn create_album(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<CreateAlbumRequest>,
) -> Result<(StatusCode, Json<AlbumResponse>), AppError> {
    ensure_contributor(&rbac)?;
    let title = req.title.trim().to_string();
    if title.is_empty() || title.len() > 200 {
        return Err(AppError::validation(
            "Album title must be 1..=200 characters.",
        ));
    }

    let owner_id = rbac.auth.user.id;
    let scope = rbac.scope;
    let album = state
        .db
        .write(move |conn| {
            repo::albums::create_album(
                conn,
                &owner_id,
                &title,
                req.description.as_deref(),
                &req.asset_ids,
                &scope,
            )
        })
        .await?;

    Ok((StatusCode::CREATED, Json(AlbumResponse { album })))
}

/// `GET /api/albums/{id}` (Authenticated + album access + RbacContext).
pub async fn get_album(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(album_id): AxumPath<String>,
) -> Result<Json<AlbumDetail>, AppError> {
    let user_id = rbac.auth.user.id;
    let role = rbac.auth.user.role;
    let scope = rbac.scope;
    let detail = state
        .db
        .read(move |conn| repo::albums::get_album_detail(conn, &album_id, &user_id, role, &scope))
        .await?
        .ok_or_else(|| AppError::not_found("Album not found."))?;
    Ok(Json(detail))
}

/// `PATCH /api/albums/{id}` (Album Owner or Admin).
pub async fn update_album(
    auth: AuthSession,
    State(state): State<Arc<AppState>>,
    AxumPath(album_id): AxumPath<String>,
    ApiJson(req): ApiJson<UpdateAlbumRequest>,
) -> Result<Json<AlbumResponse>, AppError> {
    let user_id = auth.user.id;
    let role = auth.user.role;
    let album = state
        .db
        .write(move |conn| {
            let perm = repo::albums::get_user_album_permission(conn, &album_id, &user_id, role)?
                .ok_or_else(|| DbError::NotFound("Album not found.".to_string()))?;
            if !perm.can_manage() {
                return Err(DbError::InvalidArgument("__FORBIDDEN__".to_string()));
            }
            let updated = repo::albums::update_album(
                conn,
                &album_id,
                req.title.as_deref(),
                req.description.as_deref(),
                req.cover_asset_id.as_deref(),
                req.sort_order,
            )?
            .ok_or_else(|| DbError::NotFound("Album not found.".to_string()))?;
            Ok(updated)
        })
        .await
        .map_err(|e| match e {
            DbError::InvalidArgument(ref m) if m == "__FORBIDDEN__" => {
                AppError::forbidden("Only the album owner or an administrator may edit this album.")
            }
            other => AppError::from(other),
        })?;

    Ok(Json(AlbumResponse { album }))
}

/// `DELETE /api/albums/{id}` (Album Owner or Admin).
pub async fn delete_album(
    auth: AuthSession,
    State(state): State<Arc<AppState>>,
    AxumPath(album_id): AxumPath<String>,
) -> Result<StatusCode, AppError> {
    let user_id = auth.user.id;
    let role = auth.user.role;
    state
        .db
        .write(move |conn| {
            let perm = repo::albums::get_user_album_permission(conn, &album_id, &user_id, role)?
                .ok_or_else(|| DbError::NotFound("Album not found.".to_string()))?;
            if !perm.can_manage() {
                return Err(DbError::InvalidArgument("__FORBIDDEN__".to_string()));
            }
            let deleted = repo::albums::delete_album(conn, &album_id)?;
            if !deleted {
                return Err(DbError::NotFound("Album not found.".to_string()));
            }
            Ok(())
        })
        .await
        .map_err(|e| match e {
            DbError::InvalidArgument(ref m) if m == "__FORBIDDEN__" => AppError::forbidden(
                "Only the album owner or an administrator may delete this album.",
            ),
            other => AppError::from(other),
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/albums/{id}/assets` (Owner, Admin, or `contribute` share + folder ACL + Contributor role).
pub async fn add_album_assets(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(album_id): AxumPath<String>,
    ApiJson(req): ApiJson<AlbumAssetsRequest>,
) -> Result<Json<AddedCountResponse>, AppError> {
    ensure_contributor(&rbac)?;
    let user_id = rbac.auth.user.id;
    let role = rbac.auth.user.role;
    let scope = rbac.scope;
    let added_count = state
        .db
        .write(move |conn| {
            let perm = repo::albums::get_user_album_permission(conn, &album_id, &user_id, role)?
                .ok_or_else(|| DbError::NotFound("Album not found.".to_string()))?;
            if !perm.can_contribute() {
                return Err(DbError::InvalidArgument("__FORBIDDEN__".to_string()));
            }
            repo::albums::add_album_assets(conn, &album_id, &user_id, &req.asset_ids, &scope)
        })
        .await
        .map_err(|e| match e {
            DbError::InvalidArgument(ref m) if m == "__FORBIDDEN__" => {
                AppError::forbidden("You do not have permission to add assets to this album.")
            }
            other => AppError::from(other),
        })?;

    Ok(Json(AddedCountResponse { added_count }))
}

/// `DELETE /api/albums/{id}/assets` (Owner, Admin, or `contribute` share + Contributor role).
pub async fn remove_album_assets(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(album_id): AxumPath<String>,
    ApiJson(req): ApiJson<AlbumAssetsRequest>,
) -> Result<Json<RemovedCountResponse>, AppError> {
    ensure_contributor(&rbac)?;
    let user_id = rbac.auth.user.id;
    let role = rbac.auth.user.role;
    let removed_count = state
        .db
        .write(move |conn| {
            let perm = repo::albums::get_user_album_permission(conn, &album_id, &user_id, role)?
                .ok_or_else(|| DbError::NotFound("Album not found.".to_string()))?;
            if !perm.can_contribute() {
                return Err(DbError::InvalidArgument("__FORBIDDEN__".to_string()));
            }
            repo::albums::remove_album_assets(conn, &album_id, &req.asset_ids)
        })
        .await
        .map_err(|e| match e {
            DbError::InvalidArgument(ref m) if m == "__FORBIDDEN__" => {
                AppError::forbidden("You do not have permission to remove assets from this album.")
            }
            other => AppError::from(other),
        })?;

    Ok(Json(RemovedCountResponse { removed_count }))
}

/// `PUT /api/albums/{id}/shares` (Album Owner or Admin).
pub async fn replace_album_shares(
    auth: AuthSession,
    State(state): State<Arc<AppState>>,
    AxumPath(album_id): AxumPath<String>,
    ApiJson(req): ApiJson<ReplaceSharesRequest>,
) -> Result<Json<SharesResponse>, AppError> {
    let user_id = auth.user.id;
    let role = auth.user.role;
    let pairs: Vec<(String, AlbumSharePermission)> = req
        .shares
        .into_iter()
        .map(|s| (s.user_id, s.permission))
        .collect();

    let shares = state
        .db
        .write(move |conn| {
            let perm = repo::albums::get_user_album_permission(conn, &album_id, &user_id, role)?
                .ok_or_else(|| DbError::NotFound("Album not found.".to_string()))?;
            if !perm.can_manage() {
                return Err(DbError::InvalidArgument("__FORBIDDEN__".to_string()));
            }
            repo::albums::replace_album_shares(conn, &album_id, &user_id, &pairs)
        })
        .await
        .map_err(|e| match e {
            DbError::InvalidArgument(ref m) if m == "__FORBIDDEN__" => {
                AppError::forbidden("Only the album owner or an administrator may manage shares.")
            }
            other => AppError::from(other),
        })?;

    Ok(Json(SharesResponse { shares }))
}
