//! Tauri v2 desktop wrapper bootstrap, window lifecycle, navigation guard,
//! clean server/DB shutdown, and capability/ACL parity verification tests.

pub mod commands;
pub mod power;
pub mod tray;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use truongphoto_core::{
    config::{ServerConfig, StoragePaths},
    models::{DesktopBootstrapInfo, PowerState},
};
use truongphoto_db::DbHandle;

/// Argument passed when launched at login via `tauri-plugin-autostart`.
const AUTOSTART_ARG: &str = "--autostart";

struct RuntimeHandles {
    power_monitor: Option<power::PowerMonitorHandle>,
    server_handle: Option<truongphoto_server::ServerHandle>,
    db_handle: Option<DbHandle>,
}

/// Idempotent graceful shutdown coordinator executed on `RunEvent::Exit`.
#[derive(Clone, Default)]
struct ShutdownCoordinator(Arc<Mutex<Option<RuntimeHandles>>>);

impl ShutdownCoordinator {
    fn new(
        power_monitor: power::PowerMonitorHandle,
        server_handle: truongphoto_server::ServerHandle,
        db_handle: DbHandle,
    ) -> Self {
        Self(Arc::new(Mutex::new(Some(RuntimeHandles {
            power_monitor: Some(power_monitor),
            server_handle: Some(server_handle),
            db_handle: Some(db_handle),
        }))))
    }

    pub(crate) async fn shutdown(&self) {
        let Some(mut handles) = self.0.lock().take() else {
            return;
        };

        if let Some(power_monitor) = handles.power_monitor.take() {
            power_monitor.stop();
        }

        if let Some(server_handle) = handles.server_handle.take() {
            server_handle.shutdown().await;
        }
        if let Some(db_handle) = handles.db_handle.take() {
            if let Err(err) = db_handle.shutdown().await {
                tracing::warn!(error = %err, "database shutdown failed");
            }
        }
    }

    pub(crate) fn shutdown_blocking(&self) {
        let Some(mut handles) = self.0.lock().take() else {
            return;
        };

        if let Some(power_monitor) = handles.power_monitor.take() {
            power_monitor.stop();
        }

        let fut = async move {
            if let Some(server_handle) = handles.server_handle.take() {
                server_handle.shutdown().await;
            }
            if let Some(db_handle) = handles.db_handle.take() {
                if let Err(err) = db_handle.shutdown().await {
                    tracing::warn!(error = %err, "database shutdown failed");
                }
            }
        };

        // Never call `block_on` directly from within an active Tokio runtime thread
        // (e.g. `tauri-plugin-updater`'s `on_before_exit` callback during `install_native_update`),
        // which would panic and abort the process before NSIS launches.
        if tokio::runtime::Handle::try_current().is_ok() {
            let _ = std::thread::Builder::new()
                .name("tp-shutdown".into())
                .spawn(move || {
                    tauri::async_runtime::block_on(fut);
                })
                .map(|h| h.join());
        } else {
            tauri::async_runtime::block_on(fut);
        }
    }
}

fn generate_desktop_bootstrap_token() -> anyhow::Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("CSPRNG failed: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

fn is_initial_bootstrap_url(url: &tauri::Url) -> bool {
    if url.as_str() == "about:blank" || url.scheme() == "tauri" {
        return true;
    }
    if url.scheme() == "http" && url.host_str() == Some("tauri.localhost") {
        return true;
    }
    #[cfg(debug_assertions)]
    if url.scheme() == "http"
        && url.host_str() == Some("localhost")
        && url.port_or_known_default() == Some(1420)
    {
        return true;
    }
    false
}

fn navigation_guard_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            if let Some(state) = webview
                .app_handle()
                .try_state::<commands::DesktopAppState>()
            {
                return tray::is_allowed_navigation_url(url, state.bound_port);
            }
            is_initial_bootstrap_url(url)
        })
        .build()
}

fn setup_app(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "truongphoto_desktop_lib=info,truongphoto_server=info,tower_http=info".into()
            }),
        )
        .try_init();

    let token = generate_desktop_bootstrap_token()?;

    let data_dir = std::env::var_os("TP_DATA_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(StoragePaths::default_data_dir);
    let paths = StoragePaths::from_data_dir(data_dir);
    paths.ensure_dirs()?;

    let db = DbHandle::open(&paths.db_path)?;

    let persisted_settings = tauri::async_runtime::block_on(db.read({
        let paths = paths.clone();
        move |conn| truongphoto_db::repo::settings::load_app_settings(conn, &paths)
    }))?;

    let mut server_config = ServerConfig::from_data_dir(paths.data_dir.clone());
    server_config.port = persisted_settings.port;
    server_config.mdns_enabled = persisted_settings.mdns_enabled;
    server_config.mdns_hostname = persisted_settings.mdns_hostname;
    server_config.desktop_bootstrap_token = Some(token.clone());

    if let Ok(host) = std::env::var("TP_BIND_HOST") {
        let trimmed = host.trim();
        if !trimmed.is_empty() {
            server_config.host = trimmed.to_string();
        }
    }
    let mut explicit_port = false;
    if let Ok(port_str) = std::env::var("TP_PORT") {
        if let Ok(port) = port_str.trim().parse::<u16>() {
            server_config.port = port;
            explicit_port = true;
        }
    }

    let (power_tx, power_rx) = tokio::sync::watch::channel(PowerState::default());
    let power_monitor = power::start_power_monitor(power_tx);

    let requested_port = server_config.port;
    let start_result = tauri::async_runtime::block_on(async {
        let mut last_err = None;
        for attempt in 0..5u32 {
            match truongphoto_server::start_server(
                server_config.clone(),
                db.clone(),
                power_rx.clone(),
            )
            .await
            {
                Ok(handle) => return Ok(handle),
                Err(err) => {
                    let is_addr_in_use = err
                        .chain()
                        .filter_map(|c| c.downcast_ref::<std::io::Error>())
                        .any(|io_err| io_err.kind() == std::io::ErrorKind::AddrInUse);
                    if is_addr_in_use && attempt < 4 {
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        last_err = Some((err, true));
                        continue;
                    }
                    last_err = Some((err, is_addr_in_use));
                    break;
                }
            }
        }
        let (err, is_addr_in_use) = last_err.expect("at least one start attempt");
        if is_addr_in_use && !explicit_port && server_config.port != 0 {
            tracing::warn!(
                "port {} is already in use; falling back to an available local port",
                requested_port
            );
            server_config.port = 0;
            return truongphoto_server::start_server(server_config, db.clone(), power_rx.clone())
                .await;
        }
        if is_addr_in_use {
            return Err(anyhow::anyhow!(
                "Port {requested_port} is already in use by another application. Quit the other process or set TP_PORT to a different port."
            ));
        }
        Err(err)
    });

    let server_handle = match start_result {
        Ok(handle) => handle,
        Err(err) => {
            power_monitor.stop();
            let _ = tauri::async_runtime::block_on(db.shutdown());
            return Err(err.into());
        }
    };

    let bound_port = server_handle.port();
    let mdns_url = server_handle.state().mdns.lock().mdns_url();
    let bootstrap = DesktopBootstrapInfo {
        api_base_url: format!("http://127.0.0.1:{bound_port}"),
        desktop_bootstrap_token: token,
        port: bound_port,
        mdns_url,
        version: truongphoto_core::APP_VERSION.to_string(),
        platform: std::env::consts::OS.to_string(),
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let app_state =
        commands::DesktopAppState::new(bootstrap, bound_port, power_rx, Some(shutdown_tx));
    let shutdown_coord = ShutdownCoordinator::new(power_monitor, server_handle, db);

    app.manage(app_state);
    app.manage(shutdown_coord);

    let exit_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        if shutdown_rx.await.is_ok() {
            exit_handle.exit(0);
        }
    });

    let autostart_launch = std::env::args().any(|arg| arg == AUTOSTART_ARG);
    let main_url = {
        #[cfg(debug_assertions)]
        {
            tauri::WebviewUrl::App("index.html".into())
        }
        #[cfg(not(debug_assertions))]
        {
            let target: tauri::Url = format!("http://127.0.0.1:{bound_port}/")
                .parse()
                .expect("valid local server URL");
            tauri::WebviewUrl::External(target)
        }
    };

    let downloads_root = app
        .path()
        .download_dir()
        .ok()
        .or_else(|| app.path().home_dir().ok().map(|h| h.join("Downloads")));

    let download_names: Arc<Mutex<std::collections::HashMap<String, String>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let dl_name_cb = Arc::clone(&download_names);

    let builder = tauri::WebviewWindowBuilder::new(app, "main", main_url)
        .title("TruongPhoto")
        .visible(false)
        .inner_size(1280.0, 820.0)
        .min_inner_size(980.0, 640.0)
        .decorations(cfg!(target_os = "macos"))
        // On Windows, frameless rounded corners require a transparent WebView2 + Win32 window
        // without an opaque background_color (which fills WM_ERASEBKGND/softbuffer with solid RGB).
        // On macOS, native AppKit decorations provide rounded corners and #202020 prevents startup flash.
        .transparent(!cfg!(target_os = "macos"))
        .shadow(true)
        .center()
        .theme(Some(tauri::Theme::Dark))
        .on_download(move |webview, event| match event {
            tauri::webview::DownloadEvent::Requested { url, destination } => {
                if !tray::is_allowed_navigation_url(&url, bound_port) {
                    return false;
                }
                if let Some(ref dl_dir) = downloads_root {
                    let _ = std::fs::create_dir_all(dl_dir);
                    let suggested = destination
                        .file_name()
                        .and_then(|n| n.to_str())
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or("download.bin")
                        .to_string();
                    let target = unique_download_path(dl_dir, &suggested);
                    let final_name = target
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(str::to_string)
                        .unwrap_or(suggested);
                    dl_name_cb
                        .lock()
                        .insert(url.as_str().to_string(), final_name);
                    *destination = target;
                }
                true
            }
            tauri::webview::DownloadEvent::Finished {
                url, path, success, ..
            } => {
                let mapped_name = dl_name_cb.lock().remove(url.as_str());
                let filename = path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
                    .or(mapped_name);
                let _ = webview.emit(
                    "desktop-download-finished",
                    serde_json::json!({
                        "success": success,
                        "filename": filename,
                        "path": path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    }),
                );
                true
            }
            _ => true,
        });

    #[cfg(target_os = "macos")]
    let builder = builder
        .background_color(tauri::window::Color(0x20, 0x20, 0x20, 0xFF))
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(20.0, 18.0));

    let window = builder.build()?;
    let main_win = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            #[cfg(target_os = "macos")]
            if main_win.is_fullscreen().unwrap_or(false) {
                let _ = main_win.set_fullscreen(false);
            }
            let _ = main_win.hide();
        }
    });

    if autostart_launch {
        let _ = window.hide();
    } else {
        tray::show_main_window(app.handle());
    }

    tray::setup_system_tray(app.handle())?;

    Ok(())
}

fn unique_download_path(dir: &std::path::Path, filename: &str) -> std::path::PathBuf {
    let base_path = std::path::Path::new(filename);
    let stem = base_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("download");
    let ext = base_path.extension().and_then(|s| s.to_str());
    let first = dir.join(filename);
    if !first.exists() {
        return first;
    }
    for n in 1..10_000u32 {
        let candidate_name = match ext {
            Some(e) if !e.is_empty() => format!("{stem} ({n}).{e}"),
            _ => format!("{stem} ({n})"),
        };
        let candidate = dir.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// Boots the TruongPhoto Tauri v2 desktop application and embedded Axum server.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let context = tauri::generate_context!();

    let app = tauri::Builder::default()
        // Register single-instance first so a second launch hands over before other plugins start.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().any(|a| a == AUTOSTART_ARG) {
                tray::show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(navigation_guard_plugin())
        .setup(setup_app)
        .invoke_handler(tauri::generate_handler![
            commands::get_desktop_bootstrap,
            commands::get_power_state,
            commands::show_main_window,
            commands::quit_app,
            commands::check_update_feed,
            commands::install_native_update,
        ])
        .build(context);

    let app = match app {
        Ok(app) => app,
        Err(err) => {
            report_fatal_startup_error(&err.to_string());
            std::process::exit(1);
        }
    };

    app.run(|handle, event| match event {
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => tray::show_main_window(handle),
        tauri::RunEvent::Exit => {
            if let Some(coord) = handle.try_state::<ShutdownCoordinator>() {
                coord.shutdown_blocking();
            }
        }
        _ => {}
    });
}

fn report_fatal_startup_error(message: &str) {
    tracing::error!("TruongPhoto fatal startup error: {message}");
    eprintln!("TruongPhoto failed to start: {message}");
    let data_dir = std::env::var_os("TP_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| truongphoto_core::StoragePaths::default().data_dir);
    let _ = std::fs::create_dir_all(&data_dir);
    let log_path = data_dir.join("startup-error.log");
    let unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = format!("[{unix_secs}] {message}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()));
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

        let body_text = format!(
            "TruongPhoto failed to start:\n\n{message}\n\nCheck startup-error.log in your TruongPhoto data folder for details."
        );
        let body: Vec<u16> = std::ffi::OsStr::new(&body_text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let title: Vec<u16> = std::ffi::OsStr::new("TruongPhoto Failed to Start")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: Both strings are valid NUL-terminated UTF-16 buffers.
        unsafe {
            let _ = MessageBoxW(
                None,
                PCWSTR(body.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        let full_msg = format!(
            "{message} (Check startup-error.log in your TruongPhoto data folder for details.)"
        );
        let escaped = full_msg
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\r', '\n'], " ");
        let script = format!(
            "display alert \"TruongPhoto Failed to Start\" message \"{escaped}\" as critical"
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    /// Command names registered in `tauri::generate_handler!` above.
    fn registered_commands() -> BTreeSet<String> {
        let src = include_str!("lib.rs");
        let start = src.find("generate_handler![").expect("handler list") + 18;
        let end = start + src[start..].find(']').expect("handler list end");
        src[start..end]
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .map(|l| {
                l.trim_end_matches(',')
                    .rsplit("::")
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    /// Command names declared in `src-tauri/build.rs` `APP_COMMANDS`.
    fn manifest_commands() -> BTreeSet<String> {
        let src = include_str!("../build.rs");
        let start = src
            .find("APP_COMMANDS: &[&str] = &[")
            .expect("APP_COMMANDS");
        let end = start + src[start..].find("];").expect("APP_COMMANDS end");
        src[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    fn capability(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("valid capability JSON")
    }

    fn all_permissions(text: &str) -> BTreeSet<String> {
        capability(text)["permissions"]
            .as_array()
            .expect("permissions array")
            .iter()
            .filter_map(|p| p.as_str())
            .map(str::to_string)
            .collect()
    }

    /// `allow-<command>` app command permissions granted by a capability file.
    fn granted_app_commands(text: &str) -> BTreeSet<String> {
        all_permissions(text)
            .into_iter()
            .filter(|p| p.starts_with("allow-"))
            .collect()
    }

    #[test]
    fn app_acl_manifest_and_main_capability_match_registered_handlers() {
        let registered = registered_commands();
        assert_eq!(registered.len(), 6);
        assert_eq!(manifest_commands(), registered);

        let granted = granted_app_commands(include_str!("../capabilities/main.json"));
        let expected: BTreeSet<String> = registered
            .iter()
            .map(|c| format!("allow-{}", c.replace('_', "-")))
            .collect();
        assert_eq!(granted, expected);
    }

    #[test]
    fn tray_capability_is_strict_subset_of_main_with_no_privileged_plugins() {
        let main_text = include_str!("../capabilities/main.json");
        let tray_text = include_str!("../capabilities/tray.json");

        let main_perms = all_permissions(main_text);
        let tray_perms = all_permissions(tray_text);
        assert!(tray_perms.is_subset(&main_perms));
        assert!(tray_perms.len() < main_perms.len());

        let tray_app_cmds = granted_app_commands(tray_text);
        let expected_tray_cmds: BTreeSet<String> = [
            "get_desktop_bootstrap",
            "get_power_state",
            "show_main_window",
            "quit_app",
        ]
        .iter()
        .map(|c| format!("allow-{}", c.replace('_', "-")))
        .collect();
        assert_eq!(tray_app_cmds, expected_tray_cmds);

        let all_tray_json = capability(tray_text)["permissions"].to_string();
        for forbidden in [
            "dialog:",
            "autostart:",
            "updater:",
            "process:",
            "allow-close",
            "allow-minimize",
            "shell:default",
        ] {
            assert!(
                !all_tray_json.contains(forbidden),
                "tray capability must not grant {forbidden}"
            );
        }
    }

    #[test]
    fn tauri_config_and_capabilities_lock_windows_and_csp() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let csp = conf["app"]["security"]["csp"].as_str().unwrap();
        assert!(csp.contains("http://127.0.0.1:*"));
        assert!(csp.contains("ws://127.0.0.1:*"));

        // `"main"` is created programmatically in `setup_app` so `.on_download(...)` can be attached.
        let windows = conf["app"]["windows"].as_array().unwrap();
        assert!(windows.is_empty());
        assert!(include_str!("lib.rs").contains(".on_download("));

        assert_eq!(
            capability(include_str!("../capabilities/main.json"))["windows"],
            serde_json::json!(["main"])
        );
        assert_eq!(
            capability(include_str!("../capabilities/tray.json"))["windows"],
            serde_json::json!(["tray-popover"])
        );
    }
}
