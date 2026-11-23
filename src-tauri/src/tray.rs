//! System tray icon, context menu, and lazy `360x440` popover window.

use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, PhysicalPosition, PhysicalSize, Position, Rect, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use truongphoto_core::config::DEFAULT_PORT;

use crate::commands::DesktopAppState;

const POPOVER_LABEL: &str = "tray-popover";
/// Popover size in logical points.
const POPOVER_WIDTH: f64 = 360.0;
const POPOVER_HEIGHT: f64 = 440.0;

/// A hidden popover is closed after this duration so its WebView reclaims memory when unused.
const POPOVER_IDLE_CLOSE: std::time::Duration = std::time::Duration::from_secs(60);
/// Bumped every time the popover is shown; a pending idle close only runs if unchanged.
static POPOVER_SHOWN: AtomicU64 = AtomicU64::new(0);

/// When the popover last hid due to losing focus (guards Windows tray click re-open).
static LAST_BLUR_HIDE: parking_lot::Mutex<Option<std::time::Instant>> =
    parking_lot::Mutex::new(None);

/// Returns `true` when `url` is allowed to load inside the desktop main or tray WebViews.
pub fn is_allowed_navigation_url(url: &tauri::Url, bound_port: u16) -> bool {
    if url.scheme() != "http" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let port = url.port_or_known_default();
    if bound_port != 0 && host == "127.0.0.1" && port == Some(bound_port) {
        return true;
    }
    #[cfg(debug_assertions)]
    if host == "localhost" && port == Some(1420) {
        return true;
    }
    false
}

/// Unminimizes, shows, and focuses the `"main"` desktop window.
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn bound_port_for_app(app: &AppHandle) -> u16 {
    app.try_state::<DesktopAppState>()
        .map(|s| s.bound_port)
        .filter(|&p| p != 0)
        .unwrap_or(DEFAULT_PORT)
}

fn local_url_for_app(app: &AppHandle) -> String {
    if let Some(state) = app.try_state::<DesktopAppState>() {
        return state.bootstrap.api_base_url.clone();
    }
    format!("http://127.0.0.1:{}", bound_port_for_app(app))
}

fn popover_url(bound_port: u16) -> WebviewUrl {
    #[cfg(debug_assertions)]
    let raw = {
        let _ = bound_port;
        "http://localhost:1420/tray.html".to_string()
    };
    #[cfg(not(debug_assertions))]
    let raw = format!("http://127.0.0.1:{bound_port}/tray.html");

    WebviewUrl::External(raw.parse().expect("valid tray popover URL"))
}

fn copy_text_to_clipboard(text: String) {
    let _ = std::thread::Builder::new()
        .name("tp-clipboard".into())
        .spawn(move || {
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::ffi::OsStrExt;
                use windows::Win32::Foundation::{GlobalFree, HANDLE};
                use windows::Win32::System::DataExchange::{
                    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
                };
                use windows::Win32::System::Memory::{
                    GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
                };
                const CF_UNICODETEXT: u32 = 13;

                let wide: Vec<u16> = std::ffi::OsStr::new(&text)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                let byte_len = wide.len() * std::mem::size_of::<u16>();

                for attempt in 0..3 {
                    // SAFETY: Standard Win32 clipboard sequence with owned HGLOBAL memory.
                    let opened = unsafe { OpenClipboard(None) }.is_ok();
                    if !opened {
                        if attempt < 2 {
                            std::thread::sleep(std::time::Duration::from_millis(30));
                        }
                        continue;
                    }
                    unsafe {
                        let _ = EmptyClipboard();
                        if let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, byte_len) {
                            let ptr = GlobalLock(hmem) as *mut u16;
                            if !ptr.is_null() {
                                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                                let _ = GlobalUnlock(hmem);
                                if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hmem.0))).is_err() {
                                    let _ = GlobalFree(Some(hmem));
                                }
                            } else {
                                let _ = GlobalFree(Some(hmem));
                            }
                        }
                        let _ = CloseClipboard();
                    }
                    break;
                }
            }

            #[cfg(not(target_os = "windows"))]
            {
                use std::io::Write;
                use std::process::{Command, Stdio};

                #[cfg(target_os = "macos")]
                let mut cmd = Command::new("pbcopy");

                #[cfg(not(target_os = "macos"))]
                let mut cmd = {
                    let mut c = Command::new("xclip");
                    c.args(["-selection", "clipboard"]);
                    c
                };

                if let Ok(mut child) = cmd
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(text.as_bytes());
                    }
                    let _ = child.wait();
                }
            }
        });
}

/// Registers the TruongPhoto system tray icon, context menu, and left-click popover toggle.
pub fn setup_system_tray(app: &AppHandle) -> tauri::Result<()> {
    let open_main = MenuItem::with_id(app, "open_main", "Open TruongPhoto", true, None::<&str>)?;
    let copy_local_url =
        MenuItem::with_id(app, "copy_local_url", "Copy Local URL", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit TruongPhoto", true, None::<&str>)?;

    let context_menu = Menu::with_items(app, &[&open_main, &copy_local_url, &sep, &quit])?;

    let icon = if cfg!(target_os = "macos") {
        // Template images use a transparent monochrome mark, not an opaque app tile.
        tauri::include_image!("icons/tray-icon.png").clone()
    } else {
        app.default_window_icon()
            .cloned()
            .unwrap_or_else(|| Image::new(&[], 0, 0))
    };

    TrayIconBuilder::with_id("truongphoto-tray")
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("TruongPhoto")
        .menu(&context_menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open_main" => show_main_window(app),
            "copy_local_url" => copy_text_to_clipboard(local_url_for_app(app)),
            "quit" => {
                if let Some(state) = app.try_state::<DesktopAppState>() {
                    state.trigger_shutdown();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_tray_popover(tray.app_handle(), rect);
            }
        })
        .build(app)?;

    Ok(())
}

/// Lazily creates `"tray-popover"` on first left-click.
fn popover_window(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(window) = app.get_webview_window(POPOVER_LABEL) {
        return Some(window);
    }
    let bound_port = bound_port_for_app(app);
    let window = WebviewWindowBuilder::new(app, POPOVER_LABEL, popover_url(bound_port))
        .title("TruongPhoto")
        .inner_size(POPOVER_WIDTH, POPOVER_HEIGHT)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .theme(Some(tauri::Theme::Dark))
        .on_navigation(move |url| is_allowed_navigation_url(url, bound_port))
        .build()
        .map_err(|err| tracing::warn!("tray popover build failed: {err}"))
        .ok()?;
    let handle = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Focused(false) = event {
            if handle.is_visible().unwrap_or(false) {
                *LAST_BLUR_HIDE.lock() = Some(std::time::Instant::now());
            }
            hide_popover(&handle);
        }
    });
    Some(window)
}

/// Hides the popover and closes it once it remains hidden for [`POPOVER_IDLE_CLOSE`].
fn hide_popover(popover: &WebviewWindow) {
    let _ = popover.hide();
    let shown = POPOVER_SHOWN.load(Ordering::Acquire);
    let popover = popover.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(POPOVER_IDLE_CLOSE).await;
        if POPOVER_SHOWN.load(Ordering::Acquire) == shown && !popover.is_visible().unwrap_or(true) {
            let _ = popover.close();
        }
    });
}

fn toggle_tray_popover(app: &AppHandle, tray_rect: Rect) {
    if app
        .get_webview_window(POPOVER_LABEL)
        .is_some_and(|p| p.is_visible().unwrap_or(false))
    {
        if let Some(popover) = app.get_webview_window(POPOVER_LABEL) {
            hide_popover(&popover);
        }
        return;
    }
    // On Windows a click on the tray icon first blurs the popover (hiding it), then fires the
    // tray click event; debounce 300ms so clicking the icon closes it cleanly instead of reopening.
    if LAST_BLUR_HIDE
        .lock()
        .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(300))
    {
        return;
    }
    let Some(popover) = popover_window(app) else {
        return;
    };

    let fallback_scale = popover.scale_factor().unwrap_or(1.0);
    let (icon_x, icon_y) = match tray_rect.position {
        Position::Physical(p) => (p.x as f64, p.y as f64),
        Position::Logical(l) => (l.x * fallback_scale, l.y * fallback_scale),
    };
    let (icon_w, icon_h) = match tray_rect.size {
        tauri::Size::Physical(s) => (s.width as f64, s.height as f64),
        tauri::Size::Logical(l) => (l.width * fallback_scale, l.height * fallback_scale),
    };
    let center_x = icon_x + icon_w / 2.0;
    let center_y = icon_y + icon_h / 2.0;
    // Match against `available_monitors()` using physical coordinates directly so macOS Retina
    // displays (where `monitor_from_point` uses logical points) still find the exact monitor.
    let monitor = app
        .available_monitors()
        .ok()
        .and_then(|monitors| {
            monitors.into_iter().find(|m| {
                let mp = m.position();
                let ms = m.size();
                let mx = mp.x as f64;
                let my = mp.y as f64;
                center_x >= mx
                    && center_x < mx + ms.width as f64
                    && center_y >= my
                    && center_y < my + ms.height as f64
            })
        })
        .or_else(|| {
            app.monitor_from_point(center_x / fallback_scale, center_y / fallback_scale)
                .ok()
                .flatten()
        })
        .or_else(|| popover.current_monitor().ok().flatten());
    let scale = monitor
        .as_ref()
        .map(|m| m.scale_factor())
        .unwrap_or(fallback_scale);
    let popover_size = PhysicalSize::new(
        (POPOVER_WIDTH * scale).round() as u32,
        (POPOVER_HEIGHT * scale).round() as u32,
    );
    let (target_x, target_y) = match &monitor {
        Some(m) => {
            let (mp, ms) = (m.position(), m.size());
            let wa = m.work_area();
            place_popover(
                IRect::new(icon_x, icon_y, icon_w, icon_h),
                (popover_size.width as f64, popover_size.height as f64),
                IRect::new(mp.x as f64, mp.y as f64, ms.width as f64, ms.height as f64),
                IRect::new(
                    wa.position.x as f64,
                    wa.position.y as f64,
                    wa.size.width as f64,
                    wa.size.height as f64,
                ),
                6.0 * scale,
            )
        }
        None => (
            (center_x - popover_size.width as f64 / 2.0).round() as i32,
            (icon_y - popover_size.height as f64).round() as i32,
        ),
    };

    // Move to target monitor BEFORE resizing so Windows mixed-DPI `WM_DPICHANGED` does not
    // double-scale the popover, then re-apply the exact clamped position.
    let _ = popover.set_position(Position::Physical(PhysicalPosition::new(
        target_x, target_y,
    )));
    let _ = popover.set_size(tauri::Size::Physical(popover_size));
    let _ = popover.set_position(Position::Physical(PhysicalPosition::new(
        target_x, target_y,
    )));
    POPOVER_SHOWN.fetch_add(1, Ordering::AcqRel);
    let _ = popover.show();
    let _ = popover.set_focus();
}

/// Physical-pixel rectangle for monitor-aware popover placement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct IRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl IRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    fn right(&self) -> f64 {
        self.x + self.w
    }

    fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Computes the top-left coordinate for the tray popover next to the tray icon on the inner side
/// of the taskbar (bottom, top, left, right, or auto-hidden), clamped within `work`.
pub(crate) fn place_popover(
    icon: IRect,
    size: (f64, f64),
    monitor: IRect,
    work: IRect,
    margin: f64,
) -> (i32, i32) {
    let (w, h) = size;
    let (cx, cy) = (icon.x + icon.w / 2.0, icon.y + icon.h / 2.0);
    #[derive(PartialEq)]
    enum Edge {
        Top,
        Bottom,
        Left,
        Right,
    }
    let edge = if work.y > monitor.y {
        Edge::Top
    } else if work.bottom() < monitor.bottom() {
        Edge::Bottom
    } else if work.x > monitor.x {
        Edge::Left
    } else if work.right() < monitor.right() {
        Edge::Right
    } else {
        let d = [
            (cy - monitor.y, 0),
            (monitor.bottom() - cy, 1),
            (cx - monitor.x, 2),
            (monitor.right() - cx, 3),
        ];
        match d.iter().min_by(|a, b| a.0.total_cmp(&b.0)).map(|d| d.1) {
            Some(0) => Edge::Top,
            Some(2) => Edge::Left,
            Some(3) => Edge::Right,
            _ => Edge::Bottom,
        }
    };
    let (x, y) = match edge {
        Edge::Bottom => (cx - w / 2.0, work.bottom().min(icon.y) - h - margin),
        Edge::Top => (cx - w / 2.0, work.y.max(icon.bottom()) + margin),
        Edge::Left => (work.x.max(icon.right()) + margin, cy - h / 2.0),
        Edge::Right => (work.right().min(icon.x) - w - margin, cy - h / 2.0),
    };
    let clamp = |v: f64, lo: f64, hi: f64| v.max(lo).min(hi.max(lo));
    (
        clamp(x, work.x + margin, work.right() - w - margin).round() as i32,
        clamp(y, work.y + margin, work.bottom() - h - margin).round() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (f64, f64) = (POPOVER_WIDTH, POPOVER_HEIGHT);
    const MON: IRect = IRect {
        x: 0.0,
        y: 0.0,
        w: 2560.0,
        h: 1440.0,
    };

    #[test]
    fn bottom_taskbar_opens_above_the_icon() {
        let work = IRect::new(0.0, 0.0, 2560.0, 1392.0);
        let icon = IRect::new(2300.0, 1400.0, 24.0, 24.0);
        let (x, y) = place_popover(icon, SIZE, MON, work, 6.0);
        assert_eq!(y, 1392 - 440 - 6);
        assert_eq!(x, 2312 - 180);
    }

    #[test]
    fn top_taskbar_opens_below_the_icon() {
        let work = IRect::new(0.0, 48.0, 2560.0, 1392.0);
        let icon = IRect::new(2300.0, 12.0, 24.0, 24.0);
        assert_eq!(place_popover(icon, SIZE, MON, work, 6.0).1, 54);
    }

    #[test]
    fn side_taskbars_open_beside_the_icon() {
        let left = IRect::new(62.0, 0.0, 2498.0, 1440.0);
        let icon = IRect::new(20.0, 1300.0, 24.0, 24.0);
        let (x, y) = place_popover(icon, SIZE, MON, left, 6.0);
        assert_eq!(x, 68);
        assert_eq!(y, 1440 - 440 - 6);

        let right = IRect::new(0.0, 0.0, 2498.0, 1440.0);
        let icon = IRect::new(2516.0, 700.0, 24.0, 24.0);
        assert_eq!(place_popover(icon, SIZE, MON, right, 6.0).0, 2498 - 360 - 6);
    }

    #[test]
    fn auto_hidden_taskbar_uses_nearest_edge_and_stays_on_monitor() {
        let mon = IRect::new(2560.0, -820.0, 1440.0, 2560.0);
        let icon = IRect::new(3900.0, 1700.0, 24.0, 24.0);
        let (x, y) = place_popover(icon, SIZE, mon, mon, 6.0);
        assert!((-820..1700).contains(&y), "{y}");
        assert!((2560..=4000 - 360).contains(&x), "{x}");
    }

    #[test]
    fn navigation_guard_allows_only_bound_loopback_and_dev_origin() {
        let parse = |s: &str| tauri::Url::parse(s).unwrap();

        assert!(is_allowed_navigation_url(
            &parse("http://127.0.0.1:38420/"),
            38420
        ));
        assert!(is_allowed_navigation_url(
            &parse("http://127.0.0.1:38420/tray.html?tab=1"),
            38420
        ));

        assert!(!is_allowed_navigation_url(
            &parse("http://127.0.0.1:38421/"),
            38420
        ));
        assert!(!is_allowed_navigation_url(
            &parse("http://127.0.0.1:38420/"),
            0
        ));
        assert!(!is_allowed_navigation_url(
            &parse("https://127.0.0.1:38420/"),
            38420
        ));
        assert!(!is_allowed_navigation_url(
            &parse("http://192.168.1.42:38420/"),
            38420
        ));
        assert!(!is_allowed_navigation_url(
            &parse("https://evil.example.com/"),
            38420
        ));
        assert!(!is_allowed_navigation_url(
            &parse("tauri://localhost/index.html"),
            38420
        ));
        assert!(!is_allowed_navigation_url(
            &parse("file:///etc/passwd"),
            38420
        ));

        let dev_url = parse("http://localhost:1420/tray.html");
        assert_eq!(
            is_allowed_navigation_url(&dev_url, 38420),
            cfg!(debug_assertions)
        );
        assert!(!is_allowed_navigation_url(
            &parse("http://localhost:9999/tray.html"),
            38420
        ));
    }
}
