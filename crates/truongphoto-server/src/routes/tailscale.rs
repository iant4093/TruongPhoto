use crate::error::AppError;
use crate::extractors::{AuthSession, RequireAdmin};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::Deserialize;
use std::sync::Arc;
use truongphoto_core::{TailscaleStatus, WsServerEvent};
use truongphoto_db::repo;

#[derive(Debug, Deserialize)]
pub struct QrQuery {
    #[serde(default)]
    pub url: Option<String>,
}

/// `GET /api/admin/tailscale/status` (RequireAdmin).
pub async fn tailscale_status(
    RequireAdmin(_auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<TailscaleStatus>, AppError> {
    let status = state.tailscale.get_status(state.bound_port).await;
    Ok(Json(status))
}

/// `POST /api/admin/tailscale/enable` (RequireAdmin).
pub async fn enable_tailscale(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<TailscaleStatus>, AppError> {
    let status = state.tailscale.enable(state.bound_port).await?;

    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());
    let target_url = status.https_url.clone();
    let _ = state
        .db
        .write(move |conn| {
            repo::settings::insert_audit_log(
                conn,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "admin.tailscale.enable",
                    target_type: Some("tailscale"),
                    target_id: target_url.as_deref(),
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            Ok(())
        })
        .await;

    let _ = state.ws_tx.send(WsServerEvent::TailscaleStatusChanged {
        status: status.clone(),
    });
    Ok(Json(status))
}

/// `POST /api/admin/tailscale/disable` (RequireAdmin).
pub async fn disable_tailscale(
    RequireAdmin(auth): RequireAdmin,
    State(state): State<Arc<AppState>>,
) -> Result<Json<TailscaleStatus>, AppError> {
    let status = state.tailscale.disable(state.bound_port).await?;

    let actor_id = auth.user.id.clone();
    let ip_addr = Some(auth.peer.ip_string());
    let _ = state
        .db
        .write(move |conn| {
            repo::settings::insert_audit_log(
                conn,
                &repo::settings::NewAuditLog {
                    actor_user_id: Some(&actor_id),
                    action: "admin.tailscale.disable",
                    target_type: Some("tailscale"),
                    target_id: None,
                    detail_json: "{}",
                    ip_addr: ip_addr.as_deref(),
                },
            )?;
            Ok(())
        })
        .await;

    let _ = state.ws_tx.send(WsServerEvent::TailscaleStatusChanged {
        status: status.clone(),
    });
    Ok(Json(status))
}

/// `GET /api/system/qr?url=<encoded>` (AuthSession).
///
/// Validates URL scheme/length/control chars and renders a pure-Rust SVG QR code
/// (`#171717` on `#ededed`, >= 200×200px, quiet zone included).
pub async fn system_qr(
    _auth: AuthSession,
    Query(query): Query<QrQuery>,
) -> Result<Response, AppError> {
    let url = query.url.as_deref().unwrap_or("");
    if url.is_empty()
        || url.len() > 512
        || url.chars().any(|c| c.is_ascii_control())
        || !(url.starts_with("http://") || url.starts_with("https://"))
    {
        return Err(AppError::validation(
            "QR url must be 1..=512 characters, start with http:// or https://, and contain no control characters.",
        ));
    }

    let code = QrCode::new(url.as_bytes())
        .map_err(|e| AppError::validation(format!("Unable to encode QR code: {e}")))?;

    let svg_xml = code
        .render::<svg::Color<'_>>()
        .min_dimensions(200, 200)
        .dark_color(svg::Color("#171717"))
        .light_color(svg::Color("#ededed"))
        .quiet_zone(true)
        .build();

    let mut resp = (StatusCode::OK, svg_xml).into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/svg+xml; charset=utf-8"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'"),
    );

    Ok(resp)
}
