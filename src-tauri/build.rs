// src-tauri/build.rs

/// Every app command registered in `tauri::generate_handler!`. Declaring them in an app
/// ACL manifest makes Tauri ACL-check custom commands so only windows granted `allow-<command>`
/// in `capabilities/` (`main` and `tray-popover`) can invoke them.
const APP_COMMANDS: &[&str] = &[
    "get_desktop_bootstrap",
    "get_power_state",
    "show_main_window",
    "quit_app",
    "check_update_feed",
    "install_native_update",
];

fn attributes() -> tauri_build::Attributes {
    tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(APP_COMMANDS))
}

fn main() {
    tauri_build::try_build(attributes()).expect("failed to run tauri-build");
}
