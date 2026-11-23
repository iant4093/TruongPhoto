use crate::error::{ApiJson, AppError};
use crate::extractors::{RbacContext, RequireAdmin, RequireContributor};
use crate::ingest::trash_gc::purge_assets_inner;
use crate::state::AppState;
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use truongphoto_core::{
    AssetDetail, AssetSummary, MediaTagView, PurgeReport, ShareableUser, TimelineBucket,
    UpdateOverridesRequest,
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

#[derive(Debug, Deserialize)]
pub struct AssetsQueryParams {
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub favorite: Option<bool>,
    #[serde(default)]
    pub before_ms: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct AssetListResponse {
    pub items: Vec<AssetSummary>,
    pub next_cursor: Option<String>,
    pub total_count: i64,
}

#[derive(Debug, Serialize)]
pub struct AssetBucketsResponse {
    pub buckets: Vec<TimelineBucket>,
}

#[derive(Debug, Serialize)]
pub struct TrashListResponse {
    pub items: Vec<AssetSummary>,
    pub total_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct FavoriteRequest {
    pub is_favorite: bool,
}

#[derive(Debug, Serialize)]
pub struct FavoriteResponse {
    pub id: String,
    pub is_favorite: bool,
}

#[derive(Debug, Deserialize)]
pub struct AddTagRequest {
    pub tag: String,
}

#[derive(Debug, Serialize)]
pub struct TagsResponse {
    pub tags: Vec<MediaTagView>,
}

#[derive(Debug, Deserialize)]
pub struct AssetIdsRequest {
    pub asset_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TrashedCountResponse {
    pub trashed_count: usize,
}

#[derive(Debug, Serialize)]
pub struct RestoredCountResponse {
    pub restored_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct PurgeTrashRequest {
    pub asset_ids: Option<Vec<String>>,
    pub all: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ShareableUsersResponse {
    pub items: Vec<ShareableUser>,
}

/// `GET /api/assets` (Authenticated + RbacContext).
pub async fn list_assets(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    Query(params): Query<AssetsQueryParams>,
) -> Result<Json<AssetListResponse>, AppError> {
    let scope = rbac.scope;
    let query = repo::assets::AssetListQuery {
        folder_id: params.folder_id,
        favorite: params.favorite,
        before_ms: params.before_ms,
        cursor: params.cursor,
        limit: params.limit.unwrap_or(100),
    };

    let (items, next_cursor, total_count) = state
        .db
        .read(move |conn| repo::assets::list_assets(conn, &scope, &query))
        .await?;

    Ok(Json(AssetListResponse {
        items,
        next_cursor,
        total_count,
    }))
}

/// `GET /api/assets/buckets` (Authenticated + RbacContext).
pub async fn list_asset_buckets(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AssetBucketsResponse>, AppError> {
    let scope = rbac.scope;
    let buckets = state
        .db
        .read(move |conn| repo::assets::list_timeline_buckets(conn, &scope))
        .await?;
    Ok(Json(AssetBucketsResponse { buckets }))
}

/// `GET /api/assets/{id}` (Authenticated + RbacContext).
pub async fn get_asset_detail(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
) -> Result<Json<AssetDetail>, AppError> {
    let scope = rbac.scope;
    let user_id = rbac.auth.user.id;
    let role = rbac.auth.user.role;
    let detail = state
        .db
        .read(move |conn| {
            repo::assets::get_asset_detail_for_user(conn, &scope, &asset_id, Some((&user_id, role)))
        })
        .await?
        .ok_or_else(|| AppError::not_found("Asset not found."))?;
    Ok(Json(detail))
}

/// `PATCH /api/assets/{id}/favorite` (RequireContributor + folder ACL).
pub async fn set_favorite(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
    ApiJson(req): ApiJson<FavoriteRequest>,
) -> Result<Json<FavoriteResponse>, AppError> {
    ensure_contributor(&rbac)?;
    let scope = rbac.scope;
    let aid = asset_id.clone();
    let is_favorite = state
        .db
        .write(move |conn| repo::assets::set_asset_favorite(conn, &scope, &aid, req.is_favorite))
        .await?
        .ok_or_else(|| AppError::not_found("Asset not found."))?;

    Ok(Json(FavoriteResponse {
        id: asset_id,
        is_favorite,
    }))
}

/// `PATCH /api/assets/{id}/overrides` (RequireContributor + folder ACL; DB-only, never writes XMP).
pub async fn update_overrides(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
    ApiJson(req): ApiJson<UpdateOverridesRequest>,
) -> Result<Json<AssetDetail>, AppError> {
    ensure_contributor(&rbac)?;
    if let Some(rating) = req.rating {
        if rating > 5 {
            return Err(AppError::validation("rating must be between 0 and 5."));
        }
    }
    if req.gps_lat.is_some() != req.gps_lon.is_some() {
        return Err(AppError::validation(
            "gps_lat and gps_lon must both be provided together.",
        ));
    }
    if let Some(lat) = req.gps_lat {
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return Err(AppError::validation(
                "gps_lat must be a finite number between -90 and 90.",
            ));
        }
    }
    if let Some(lon) = req.gps_lon {
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            return Err(AppError::validation(
                "gps_lon must be a finite number between -180 and 180.",
            ));
        }
    }

    let scope = rbac.scope;
    let actor_id = rbac.auth.user.id;
    let role = rbac.auth.user.role;
    let detail = state
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let updated =
                repo::assets::upsert_user_overrides(&tx, &scope, &asset_id, &actor_id, &req)?;
            if updated.is_none() {
                return Err(DbError::NotFound("Asset not found.".to_string()));
            }
            let detail = repo::assets::get_asset_detail_for_user(
                &tx,
                &scope,
                &asset_id,
                Some((&actor_id, role)),
            )?
            .ok_or_else(|| DbError::NotFound("Asset not found.".to_string()))?;
            tx.commit()?;
            Ok(detail)
        })
        .await?;

    Ok(Json(detail))
}

/// `POST /api/assets/{id}/tags` (RequireContributor + folder ACL).
pub async fn add_tag(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath(asset_id): AxumPath<String>,
    ApiJson(req): ApiJson<AddTagRequest>,
) -> Result<(StatusCode, Json<TagsResponse>), AppError> {
    ensure_contributor(&rbac)?;
    let tag = req.tag.trim().to_string();
    if tag.is_empty() || tag.len() > 128 {
        return Err(AppError::validation("Tag must be 1..=128 characters."));
    }

    let scope = rbac.scope;
    let actor_id = rbac.auth.user.id;
    let tags = state
        .db
        .write(move |conn| repo::tags::add_user_tag(conn, &scope, &asset_id, &actor_id, &tag))
        .await?
        .ok_or_else(|| AppError::not_found("Asset not found."))?;

    Ok((StatusCode::CREATED, Json(TagsResponse { tags })))
}

/// `DELETE /api/assets/{id}/tags/{tag}` (RequireContributor + folder ACL).
pub async fn remove_tag(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    AxumPath((asset_id, tag)): AxumPath<(String, String)>,
) -> Result<Json<TagsResponse>, AppError> {
    ensure_contributor(&rbac)?;
    let scope = rbac.scope;
    let tags = state
        .db
        .write(move |conn| repo::tags::remove_or_reject_tag(conn, &scope, &asset_id, &tag))
        .await?
        .ok_or_else(|| AppError::not_found("Asset not found."))?;

    Ok(Json(TagsResponse { tags }))
}

/// `POST /api/assets/trash` (RequireContributor + folder ACL).
pub async fn trash_assets(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<AssetIdsRequest>,
) -> Result<Json<TrashedCountResponse>, AppError> {
    ensure_contributor(&rbac)?;
    let scope = rbac.scope;
    let actor_id = rbac.auth.user.id;
    let now_ms = Utc::now().timestamp_millis();
    let trashed_count = state
        .db
        .write(move |conn| {
            repo::assets::soft_delete_assets(conn, &scope, &actor_id, &req.asset_ids, now_ms)
        })
        .await?;

    Ok(Json(TrashedCountResponse { trashed_count }))
}

/// `POST /api/trash/restore` (RequireContributor + folder ACL).
pub async fn restore_trash(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<AssetIdsRequest>,
) -> Result<Json<RestoredCountResponse>, AppError> {
    ensure_contributor(&rbac)?;
    let scope = rbac.scope;
    let now_ms = Utc::now().timestamp_millis();
    let restored_count = state
        .db
        .write(move |conn| repo::assets::restore_assets(conn, &scope, &req.asset_ids, now_ms))
        .await?;

    Ok(Json(RestoredCountResponse { restored_count }))
}

/// `POST /api/trash/purge` (RequireAdmin ONLY).
pub async fn purge_trash(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<PurgeTrashRequest>,
) -> Result<Json<PurgeReport>, AppError> {
    let target_ids: Option<Vec<String>> = if req.all == Some(true) {
        None
    } else if let Some(ids) = req.asset_ids {
        if ids.is_empty() {
            return Ok(Json(PurgeReport::default()));
        }
        Some(ids)
    } else {
        return Err(AppError::validation(
            "Provide non-empty asset_ids or set all=true.",
        ));
    };

    let default_paths = state.default_paths.clone();
    let settings = state
        .db
        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
        .await?;

    let report = purge_assets_inner(
        &state.db,
        Path::new(&settings.thumbnails_dir),
        Path::new(&settings.cache_dir),
        &state.cache_mgr,
        target_ids.as_deref(),
    )
    .await?;

    let actor_id = auth.user.id;
    let ip_addr = Some(auth.peer.ip_string());
    let detail_json = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());
    let _ = state
        .db
        .write(move |conn| {
            repo::settings::insert_audit_log(
                conn,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "trash.purge",
                    target_type: Some("trash"),
                    target_id: None,
                    detail_json: &detail_json,
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            Ok(())
        })
        .await;

    Ok(Json(report))
}

/// `GET /api/trash` (Contributor or Admin + RbacContext).
pub async fn list_trash(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
) -> Result<Json<TrashListResponse>, AppError> {
    ensure_contributor(&rbac)?;
    let scope = rbac.scope;
    let (items, total_count) = state
        .db
        .read(move |conn| repo::assets::list_trash(conn, &scope))
        .await?;
    Ok(Json(TrashListResponse { items, total_count }))
}

/// `GET /api/users/shareable` (RequireContributor).
pub async fn list_shareable_users(
    RequireContributor(_auth): RequireContributor,
    State(state): State<Arc<AppState>>,
) -> Result<Json<ShareableUsersResponse>, AppError> {
    let items = state.db.read(repo::albums::list_shareable_users).await?;
    Ok(Json(ShareableUsersResponse { items }))
}
