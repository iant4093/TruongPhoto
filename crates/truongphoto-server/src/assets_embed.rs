use crate::error::AppError;
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../dist"]
struct SpaAssets;

/// JSON 404 fallback for unmatched `/api/*` routes.
pub async fn api_not_found() -> AppError {
    AppError::not_found("API endpoint not found")
}

/// Embedded static asset and SPA history fallback handler for non-`/api` routes.
pub async fn spa_fallback(req: Request<Body>) -> Response {
    let method = req.method().clone();
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let raw_path = req.uri().path().trim_start_matches('/');
    if raw_path == "api" || raw_path.starts_with("api/") {
        return api_not_found().await.into_response();
    }

    let asset_path = if raw_path.is_empty() {
        "index.html"
    } else {
        raw_path
    };

    if let Some(content) = SpaAssets::get(asset_path) {
        return build_asset_response(
            asset_path,
            content.data.into_owned(),
            method == Method::HEAD,
        );
    }

    // SPA client-side routes (extensionless paths like `/timeline`, `/settings`) serve `index.html`.
    let last_segment = asset_path.rsplit('/').next().unwrap_or(asset_path);
    if !last_segment.contains('.') {
        if let Some(index) = SpaAssets::get("index.html") {
            return build_asset_response(
                "index.html",
                index.data.into_owned(),
                method == Method::HEAD,
            );
        }
    }

    StatusCode::NOT_FOUND.into_response()
}

fn build_asset_response(path: &str, bytes: Vec<u8>, is_head: bool) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let content_type = if mime == mime_guess::mime::TEXT_HTML {
        "text/html; charset=utf-8".to_string()
    } else {
        mime.as_ref().to_string()
    };

    let cache_control = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else if path.ends_with(".html") {
        "no-cache"
    } else {
        "public, max-age=3600"
    };

    let body = if is_head {
        Body::empty()
    } else {
        Body::from(bytes)
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control)
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
