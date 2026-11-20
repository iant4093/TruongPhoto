//! Native-only Tauri v2 IPC commands and managed desktop state.
//!
//! All library, auth, folder, and settings operations flow over the embedded Axum REST/WS API.
//! Tauri IPC exposes only the five native desktop commands listed in `build.rs`.

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

/// Triggers graceful server/DB shutdown and exits the desktop process.
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle, state: tauri::State<'_, DesktopAppState>) {
    state.trigger_shutdown();
    app.exit(0);
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
