use crate::backup_scheduler::list_backup_snapshots;
use crate::error::AppError;
use crate::extractors::AuthSession;
use crate::state::AppState;
use axum::{extract::State, Json};
use serde::Serialize;
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use sysinfo::System;
use truongphoto_core::{
    BackupSnapshotView, HardwareInfo, LibraryStats, PowerState, RbacScope, ThrottleStatus,
};
use truongphoto_db::repo;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub initialized: bool,
}

#[derive(Debug, Serialize)]
pub struct SystemInfoResponse {
    pub version: &'static str,
    pub platform: &'static str,
    pub arch: &'static str,
    pub hostname: String,
    pub port: u16,
    pub lan_urls: Vec<String>,
    pub mdns_enabled: bool,
    pub mdns_active: bool,
    pub mdns_url: Option<String>,
    pub desktop_pin_lock: bool,
    pub hardware: HardwareInfo,
    pub stats: LibraryStats,
    pub power: PowerState,
    pub last_backup: Option<BackupSnapshotView>,
    pub throttle: ThrottleStatus,
    pub tailscale_url: Option<String>,
}

/// `GET /api/health` (Public).
pub async fn health(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, AppError> {
    let initialized = state.db.read(repo::settings::is_setup_completed).await?;
    Ok(Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.started_at.elapsed().as_secs(),
        initialized,
    }))
}

/// `GET /api/system/info` (Authenticated).
pub async fn system_info(
    _auth: AuthSession,
    State(state): State<Arc<AppState>>,
) -> Result<Json<SystemInfoResponse>, AppError> {
    let default_paths = state.default_paths.clone();
    let (settings, asset_count, folder_count) = state
        .db
        .read(move |conn| {
            let settings = repo::settings::load_app_settings(conn, &default_paths)?;
            let asset_count = repo::assets::count_active_assets(conn, &RbacScope::AdminAll)?;
            let folder_count = repo::folders::count_folders(conn)?;
            Ok((settings, asset_count, folder_count))
        })
        .await?;

    let backup_dir = PathBuf::from(&settings.backup_dir);
    let last_backup =
        tokio::task::spawn_blocking(move || list_backup_snapshots(&backup_dir).into_iter().next())
            .await
            .map_err(|e| AppError::internal(format!("Failed to inspect backups: {e}")))?;

    let (mdns_enabled, mdns_active, mdns_url) = {
        let mdns = state.mdns.lock();
        (mdns.is_enabled(), mdns.is_active(), mdns.mdns_url())
    };

    let hostname = System::host_name().unwrap_or_else(|| "localhost".to_string());
    let lan_urls = collect_lan_urls(state.bound_port);
    let power = *state.power_rx.borrow();
    let throttle = state.throttle_rx.borrow().clone();
    let tailscale_url = state
        .tailscale
        .cached_or_refresh_status(state.bound_port)
        .await
        .https_url;

    Ok(Json(SystemInfoResponse {
        version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        hostname,
        port: state.bound_port,
        lan_urls,
        mdns_enabled,
        mdns_active,
        mdns_url,
        desktop_pin_lock: settings.desktop_pin_lock,
        hardware: state.hardware.clone(),
        stats: LibraryStats {
            asset_count,
            folder_count,
            db_size_bytes: state.db.db_size_bytes(),
        },
        power,
        last_backup,
        throttle,
        tailscale_url,
    }))
}

fn collect_lan_urls(port: u16) -> Vec<String> {
    let Ok(ifaces) = local_ip_address::list_afinet_netifas() else {
        return Vec::new();
    };

    let mut urls = BTreeSet::new();
    for (_name, ip) in ifaces {
        if let IpAddr::V4(v4) = ip {
            if !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified() {
                urls.insert(format!("http://{v4}:{port}"));
            }
        }
    }
    urls.into_iter().collect()
}
