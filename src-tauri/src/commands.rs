//! Native-only Tauri v2 IPC commands and managed desktop state.
//!
//! All library, auth, folder, and settings operations flow over the embedded Axum REST/WS API.
//! Tauri IPC exposes only the six native desktop commands listed in `build.rs`.

use parking_lot::Mutex;
use tokio::sync::{oneshot, watch};
use truongphoto_core::models::{DesktopBootstrapInfo, PowerState, UpdateFeedStatus};

/// Tauri-managed runtime state backing the native IPC commands and tray helpers.
pub struct DesktopAppState {
    pub bootstrap: DesktopBootstrapInfo,
    pub bound_port: u16,
    pub power_rx: watch::Receiver<PowerState>,
    pub shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl DesktopAppState {
    /// Creates a new [`DesktopAppState`] with an optional one-shot graceful shutdown trigger.
    pub fn new(
        bootstrap: DesktopBootstrapInfo,
        bound_port: u16,
        power_rx: watch::Receiver<PowerState>,
        shutdown_tx: Option<oneshot::Sender<()>>,
    ) -> Self {
        Self {
            bootstrap,
            bound_port,
            power_rx,
            shutdown_tx: Mutex::new(shutdown_tx),
        }
    }

    /// Fires the one-shot shutdown signal if it has not already been consumed.
    pub fn trigger_shutdown(&self) {
        if let Some(tx) = self.shutdown_tx.lock().take() {
            let _ = tx.send(());
        }
    }
}

/// Returns the ephemeral loopback URL, desktop bootstrap token, port, and platform metadata.
#[tauri::command]
pub fn get_desktop_bootstrap(state: tauri::State<'_, DesktopAppState>) -> DesktopBootstrapInfo {
    state.bootstrap.clone()
}

/// Returns the latest OS sleep, display, session lock, and battery state.
#[tauri::command]
pub fn get_power_state(state: tauri::State<'_, DesktopAppState>) -> PowerState {
    *state.power_rx.borrow()
}

/// Unminimizes, shows, and focuses the main desktop window.
#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle) {
    crate::tray::show_main_window(&app);
}

/// Triggers graceful server/DB shutdown and exits the desktop process via the single shutdown task.
#[tauri::command]
pub fn quit_app(_app: tauri::AppHandle, state: tauri::State<'_, DesktopAppState>) {
    state.trigger_shutdown();
}

/// Checks the configured Tauri updater feed and returns update availability metadata.
#[tauri::command]
pub async fn check_update_feed(app: tauri::AppHandle) -> Result<UpdateFeedStatus, String> {
    use tauri_plugin_updater::UpdaterExt;

    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(err) => {
            tracing::debug!("updater unavailable: {err}");
            return Ok(UpdateFeedStatus {
                available: false,
                version: None,
                notes: Some(format!("Updater unavailable: {err}")),
            });
        }
    };

    match updater.check().await {
        Ok(Some(update)) => Ok(UpdateFeedStatus {
            available: true,
            version: Some(update.version),
            notes: update.body,
        }),
        Ok(None) => Ok(UpdateFeedStatus {
            available: false,
            version: None,
            notes: None,
        }),
        Err(err) => {
            tracing::debug!("update feed check failed: {err}");
            Ok(UpdateFeedStatus {
                available: false,
                version: None,
                notes: Some(format!("Update feed unavailable: {err}")),
            })
        }
    }
}

/// Downloads and installs a signed desktop update while guaranteeing graceful embedded server
/// and SQLite WAL shutdown before Windows NSIS (or macOS/Linux relaunch) terminates the process.
#[tauri::command]
pub async fn install_native_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    use tauri_plugin_updater::UpdaterExt;

    let exit_handle = app.clone();
    let updater = app
        .updater_builder()
        .on_before_exit(move || {
            if let Some(coord) = exit_handle.try_state::<crate::ShutdownCoordinator>() {
                coord.shutdown_blocking();
            }
            exit_handle.cleanup_before_exit();
        })
        .build()
        .map_err(|e| format!("Updater unavailable: {e}"))?;

    let Some(update) = updater
        .check()
        .await
        .map_err(|e| format!("Failed to check for updates: {e}"))?
    else {
        return Err("No update available.".to_string());
    };

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tauri::Emitter;

    let downloaded = Arc::new(AtomicU64::new(0));
    let last_emitted = Arc::new(AtomicU64::new(0));
    let progress_app = app.clone();
    let finish_app = app.clone();
    let dl_counter = Arc::clone(&downloaded);
    let emit_counter = Arc::clone(&last_emitted);

    update
        .download_and_install(
            move |chunk_len, content_length| {
                let total_dl =
                    dl_counter.fetch_add(chunk_len as u64, Ordering::Relaxed) + chunk_len as u64;
                let prev = emit_counter.load(Ordering::Relaxed);
                let should_emit = total_dl.saturating_sub(prev) >= 256 * 1024
                    || content_length.is_some_and(|total| total_dl >= total);
                if should_emit {
                    emit_counter.store(total_dl, Ordering::Relaxed);
                    let _ = progress_app.emit(
                        "desktop-update-progress",
                        serde_json::json!({
                            "downloaded": total_dl,
                            "total": content_length,
                            "finished": false,
                        }),
                    );
                }
            },
            move || {
                let _ = finish_app.emit(
                    "desktop-update-progress",
                    serde_json::json!({
                        "downloaded": downloaded.load(Ordering::Relaxed),
                        "total": serde_json::Value::Null,
                        "finished": true,
                    }),
                );
            },
        )
        .await
        .map_err(|e| format!("Failed to install update: {e}"))?;

    if let Some(coord) = app.try_state::<crate::ShutdownCoordinator>() {
        coord.shutdown().await;
    }
    app.restart();
}
