use crate::error::AppError;
use crate::extractors::{AuthSession, RbacContext};
use crate::routes::media::{serve_file_slice_with_range, ServeSliceOptions};
use crate::state::AppState;
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::HeaderMap,
    response::Response,
    Json,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use truongphoto_core::{AppSettings, MapClustersResponse, MapConfigResponse, MapPointsResponse};
use truongphoto_db::repo;

const ONLINE_OPENFREEMAP_STYLE_URL: &str = "https://tiles.openfreemap.org/styles/dark";
const PREFERRED_PMTILES_FILENAME: &str = "world-z0-z6.pmtiles";
const DEFAULT_MAP_POINTS_LIMIT: usize = 100;
const MAX_MAP_POINTS_LIMIT: usize = 500;

#[derive(Debug, Deserialize)]
pub struct MapClustersQuery {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lng: f64,
    pub max_lng: f64,
    #[serde(default = "default_zoom")]
    pub zoom: u8,
}

const fn default_zoom() -> u8 {
    2
}

#[derive(Debug, Deserialize)]
pub struct MapPointsQuery {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lng: f64,
    pub max_lng: f64,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn validate_bbox(min_lat: f64, max_lat: f64, min_lng: f64, max_lng: f64) -> Result<(), AppError> {
    if !min_lat.is_finite() || !max_lat.is_finite() || !min_lng.is_finite() || !max_lng.is_finite()
    {
        return Err(AppError::validation(
            "Bounding box coordinates must be finite numbers.",
        ));
    }
    if min_lat > max_lat {
        return Err(AppError::validation(
            "min_lat must be less than or equal to max_lat.",
        ));
    }
    Ok(())
}

fn resolve_pmtiles_dir(state: &AppState, settings: &AppSettings) -> PathBuf {
    let trimmed = settings.map_pmtiles_dir.trim();
    if trimmed.is_empty() {
        state.config.data_dir.join("maps")
    } else {
        PathBuf::from(trimmed)
    }
}

fn is_valid_pmtiles_filename(name: &str) -> bool {
    !name.is_empty()
        && name.ends_with(".pmtiles")
        && !name.contains("..")
        && !name.contains('/')
        && !name.contains('\\')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn scan_pmtiles_dir(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            if is_valid_pmtiles_filename(name) {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names
}

/// `GET /api/map/config` (AuthSession + RbacContext).
pub async fn get_map_config(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MapConfigResponse>, AppError> {
    let scope = rbac.scope;
    let default_paths = state.default_paths.clone();

    let (settings, total_geotagged) = state
        .db
        .read(move |conn| {
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            let total_geotagged = repo::map::count_geotagged_assets(conn, &scope)?;
            Ok((settings, total_geotagged))
        })
        .await?;

    let pmtiles_dir = resolve_pmtiles_dir(&state, &settings);
    let available_pmtiles = tokio::task::spawn_blocking(move || scan_pmtiles_dir(&pmtiles_dir))
        .await
        .map_err(|e| AppError::internal(format!("PMTiles scan worker panicked: {e}")))?;

    let pmtiles_url = available_pmtiles
        .iter()
        .find(|n| n.as_str() == PREFERRED_PMTILES_FILENAME)
        .or_else(|| available_pmtiles.first())
        .map(|name| format!("/api/map/tiles/{name}"));

    Ok(Json(MapConfigResponse {
        tile_mode: settings.map_tile_mode,
        available_pmtiles,
        pmtiles_url,
        online_style_url: ONLINE_OPENFREEMAP_STYLE_URL.to_string(),
        total_geotagged,
    }))
}

/// `GET /api/map/clusters` (AuthSession + RbacContext).
pub async fn list_map_clusters(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    Query(q): Query<MapClustersQuery>,
) -> Result<Json<MapClustersResponse>, AppError> {
    validate_bbox(q.min_lat, q.max_lat, q.min_lng, q.max_lng)?;

    let scope = rbac.scope;
    let resp = state
        .db
        .read(move |conn| {
            repo::map::list_map_clusters(
                conn, &scope, q.min_lat, q.max_lat, q.min_lng, q.max_lng, q.zoom,
            )
        })
        .await?;

    Ok(Json(resp))
}

/// `GET /api/map/points` (AuthSession + RbacContext).
pub async fn list_map_points(
    rbac: RbacContext,
    State(state): State<Arc<AppState>>,
    Query(q): Query<MapPointsQuery>,
) -> Result<Json<MapPointsResponse>, AppError> {
    validate_bbox(q.min_lat, q.max_lat, q.min_lng, q.max_lng)?;

    let limit = q
        .limit
        .unwrap_or(DEFAULT_MAP_POINTS_LIMIT)
        .clamp(1, MAX_MAP_POINTS_LIMIT);
    let scope = rbac.scope;
    let resp = state
        .db
        .read(move |conn| {
            repo::map::list_map_points(
                conn, &scope, q.min_lat, q.max_lat, q.min_lng, q.max_lng, limit,
            )
        })
        .await?;

    Ok(Json(resp))
}

/// `GET /api/map/tiles/{name}` (AuthSession; strict filename validation + RFC 7233 HTTP Range streaming).
pub async fn get_pmtiles_tile(
    _auth: AuthSession,
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
    req_headers: HeaderMap,
) -> Result<Response, AppError> {
    if !is_valid_pmtiles_filename(&name) {
        return Err(AppError::not_found("Invalid PMTiles archive filename."));
    }

    let default_paths = state.default_paths.clone();
    let settings = state
        .db
        .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
        .await?;

    let pmtiles_dir = resolve_pmtiles_dir(&state, &settings);
    let tile_path = pmtiles_dir.join(&name);

    let meta = tokio::fs::metadata(&tile_path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::not_found("PMTiles archive not found.")
        } else {
            AppError::internal(format!("Failed to stat PMTiles archive: {e}"))
        }
    })?;

    if !meta.is_file() {
        return Err(AppError::not_found("PMTiles archive not found."));
    }

    let etag = format!("\"pmtiles-{name}-{}\"", meta.len());
    serve_file_slice_with_range(
        ServeSliceOptions {
            path: &tile_path,
            slice_offset: 0,
            slice_len: meta.len(),
            content_type: "application/octet-stream",
            etag: &etag,
            cache_control: Some("private, max-age=86400"),
            download_filename: None,
            extra_header: None,
        },
        &req_headers,
    )
    .await
}
