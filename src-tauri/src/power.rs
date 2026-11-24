//! OS sleep, display off, session lock, and AC/battery observer.
//!
//! Publishes [`PowerState`] transitions into a `tokio::sync::watch::Sender<PowerState>`
//! so the embedded Axum server can broadcast `WsServerEvent::PowerStateChanged`
//! and background pipelines can throttle work while on battery or asleep.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::watch;

pub use truongphoto_core::models::PowerState;

/// Discrete OS power and session events mapped onto [`PowerState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    SystemSleep,
    SystemWake,
    DisplaySleep,
    DisplayWake,
    SessionLock,
    SessionUnlock,
    BatteryChanged(bool),
}

/// Applies a [`PowerEvent`] to `state`, returning `true` if any field changed.
pub fn apply_event(state: &mut PowerState, event: PowerEvent) -> bool {
    let prev = *state;
    match event {
        PowerEvent::SystemSleep => state.sleeping = true,
        PowerEvent::SystemWake => state.sleeping = false,
        PowerEvent::DisplaySleep => state.display_asleep = true,
        PowerEvent::DisplayWake => state.display_asleep = false,
        PowerEvent::SessionLock => state.session_locked = true,
        PowerEvent::SessionUnlock => state.session_locked = false,
        PowerEvent::BatteryChanged(on_battery) => state.on_battery = on_battery,
    }
    *state != prev
}

/// Handle returned by [`start_power_monitor`]. Dropping it stops background polling.
pub struct PowerMonitorHandle {
    stop: Arc<AtomicBool>,
    tx: watch::Sender<PowerState>,
}

impl PowerMonitorHandle {
    /// Returns the most recently observed [`PowerState`].
    pub fn current(&self) -> PowerState {
        *self.tx.borrow()
    }

    /// Signals background polling threads to exit cleanly.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        platform::stop();
    }
}

impl Drop for PowerMonitorHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) struct PowerSink {
    state: Mutex<PowerState>,
    tx: watch::Sender<PowerState>,
}

impl PowerSink {
    fn new(tx: watch::Sender<PowerState>) -> Self {
        let initial = *tx.borrow();
        Self {
            state: Mutex::new(initial),
            tx,
        }
    }

    pub(crate) fn emit(&self, event: PowerEvent) {
        let next = {
            let mut guard = self.state.lock();
            if !apply_event(&mut guard, event) {
                return;
            }
            *guard
        };
        let _ = self.tx.send(next);
    }
}

/// Starts OS-level sleep, display, lock, and battery observers publishing to `tx`.
pub fn start_power_monitor(tx: watch::Sender<PowerState>) -> PowerMonitorHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let sink = Arc::new(PowerSink::new(tx.clone()));
    platform::start(sink, Arc::clone(&stop));
    PowerMonitorHandle { stop, tx }
}

/// Parses `pmset -g batt` stdout and returns `true` when drawing from battery power.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn parse_pmset_on_battery(output: &str) -> bool {
    output.to_ascii_lowercase().contains("'battery power'")
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{parse_pmset_on_battery, PowerEvent, PowerSink};
    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// # Safety
    /// `center` must be a valid `NSNotificationCenter` (or subclass) instance.
    unsafe fn observe(center: *mut AnyObject, name: &str, sink: Arc<PowerSink>, event: PowerEvent) {
        let name = NSString::from_str(name);
        let block = RcBlock::new(move |_note: *mut AnyObject| sink.emit(event));
        let queue: *mut AnyObject = msg_send![class!(NSOperationQueue), mainQueue];
        let nil: *mut AnyObject = std::ptr::null_mut();
        let token: *mut AnyObject = msg_send![
            center,
            addObserverForName: &*name,
            object: nil,
            queue: queue,
            usingBlock: &*block
        ];
        // Observers remain active for the lifetime of the desktop process.
        let _: *mut AnyObject = msg_send![token, retain];
    }

    fn query_on_battery() -> Option<bool> {
        let output = Command::new("pmset").args(["-g", "batt"]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Some(parse_pmset_on_battery(&text))
    }

    pub fn start(sink: Arc<PowerSink>, stop: Arc<AtomicBool>) {
        // Ensure AppKit is linked so `NSWorkspace` class lookup succeeds.
        let _ = std::mem::size_of::<objc2_app_kit::NSWorkspace>();

        // SAFETY: Standard AppKit/Foundation notification center calls on valid singleton
        // instances; notification centers retain/copy the block.
        unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let center: *mut AnyObject = msg_send![workspace, notificationCenter];
            observe(
                center,
                "NSWorkspaceWillSleepNotification",
                Arc::clone(&sink),
                PowerEvent::SystemSleep,
            );
            observe(
                center,
                "NSWorkspaceDidWakeNotification",
                Arc::clone(&sink),
                PowerEvent::SystemWake,
            );
            observe(
                center,
                "NSWorkspaceScreensDidSleepNotification",
                Arc::clone(&sink),
                PowerEvent::DisplaySleep,
            );
            observe(
                center,
                "NSWorkspaceScreensDidWakeNotification",
                Arc::clone(&sink),
                PowerEvent::DisplayWake,
            );

            let distributed: *mut AnyObject =
                msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
            observe(
                distributed,
                "com.apple.screenIsLocked",
                Arc::clone(&sink),
                PowerEvent::SessionLock,
            );
            observe(
                distributed,
                "com.apple.screenIsUnlocked",
                Arc::clone(&sink),
                PowerEvent::SessionUnlock,
            );
        }

        let _ = std::thread::Builder::new()
            .name("tp-battery-poll".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    if let Some(on_battery) = query_on_battery() {
                        sink.emit(PowerEvent::BatteryChanged(on_battery));
                    }
                    for _ in 0..30 {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            });
    }

    pub fn stop() {}
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{PowerEvent, PowerSink};
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::Arc;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Power::{
        GetSystemPowerStatus, RegisterPowerSettingNotification, RegisterSuspendResumeNotification,
        UnregisterPowerSettingNotification, UnregisterSuspendResumeNotification,
        POWERBROADCAST_SETTING, SYSTEM_POWER_STATUS,
    };
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
    use windows::Win32::UI::WindowsAndMessaging::*;

    static WINDOWS_SINK: Mutex<Option<Arc<PowerSink>>> = Mutex::new(None);
    static WATCHER_HWND: AtomicIsize = AtomicIsize::new(0);

    fn emit(event: PowerEvent) {
        if let Some(sink) = WINDOWS_SINK.lock().as_ref().cloned() {
            sink.emit(event);
        }
    }

    fn query_and_emit_battery() {
        let mut status = SYSTEM_POWER_STATUS::default();
        // SAFETY: `status` is a valid, stack-allocated `SYSTEM_POWER_STATUS`.
        if unsafe { GetSystemPowerStatus(&mut status) }.is_ok() && status.ACLineStatus != 255 {
            emit(PowerEvent::BatteryChanged(status.ACLineStatus == 0));
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CLOSE => {
                // Exit the GetMessageW loop first so WTS/power notifications unregister against a live HWND
                // before DestroyWindow(hwnd) runs after the loop.
                PostQuitMessage(0);
                return LRESULT(0);
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return LRESULT(0);
            }
            WM_WTSSESSION_CHANGE => match wparam.0 as u32 {
                WTS_SESSION_LOCK | WTS_CONSOLE_DISCONNECT => emit(PowerEvent::SessionLock),
                WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT => emit(PowerEvent::SessionUnlock),
                _ => {}
            },
            WM_POWERBROADCAST => match wparam.0 as u32 {
                PBT_APMSUSPEND => emit(PowerEvent::SystemSleep),
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                    emit(PowerEvent::SystemWake);
                    query_and_emit_battery();
                }
                PBT_APMPOWERSTATUSCHANGE => query_and_emit_battery(),
                PBT_POWERSETTINGCHANGE if lparam.0 != 0 => {
                    let setting = &*(lparam.0 as *const POWERBROADCAST_SETTING);
                    if setting.PowerSetting == GUID_CONSOLE_DISPLAY_STATE && setting.DataLength >= 1
                    {
                        match setting.Data[0] {
                            0 => emit(PowerEvent::DisplaySleep),
                            1 => emit(PowerEvent::DisplayWake),
                            _ => {}
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    pub fn stop() {
        *WINDOWS_SINK.lock() = None;
        let raw = WATCHER_HWND.swap(0, Ordering::AcqRel);
        if raw != 0 {
            let hwnd = HWND(raw as *mut core::ffi::c_void);
            // SAFETY: Posting WM_CLOSE to our hidden watcher window is thread-safe and triggers
            // DestroyWindow -> PostQuitMessage(0) on the watcher thread.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn start(sink: Arc<PowerSink>, stop: Arc<AtomicBool>) {
        *WINDOWS_SINK.lock() = Some(sink);
        query_and_emit_battery();

        let _ = std::thread::Builder::new()
            .name("tp-power-watcher".into())
            .spawn(move || unsafe {
                let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
                let class_name = w!("TruongPhotoPowerWatcher");
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: class_name,
                    ..Default::default()
                };
                RegisterClassExW(&wc);
                if let Ok(hwnd) = CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    class_name,
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(hinstance.into()),
                    None,
                ) {
                    WATCHER_HWND.store(hwnd.0 as isize, Ordering::Release);
                    let mut wts_registered = false;
                    let mut delay = std::time::Duration::from_millis(500);
                    let mut waited = std::time::Duration::ZERO;
                    while !stop.load(Ordering::Acquire) {
                        match WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
                            Ok(()) => {
                                wts_registered = true;
                                break;
                            }
                            Err(err) => {
                                if waited >= std::time::Duration::from_secs(60) {
                                    tracing::warn!("lock/unlock detection unavailable: {err}");
                                    break;
                                }
                                std::thread::sleep(delay);
                                waited += delay;
                                delay = (delay * 2).min(std::time::Duration::from_secs(8));
                            }
                        }
                    }
                    let power_notify = if !stop.load(Ordering::Acquire) {
                        RegisterPowerSettingNotification(
                            HANDLE(hwnd.0),
                            &GUID_CONSOLE_DISPLAY_STATE,
                            REGISTER_NOTIFICATION_FLAGS(0),
                        )
                        .ok()
                    } else {
                        None
                    };
                    let suspend_notify = if !stop.load(Ordering::Acquire) {
                        RegisterSuspendResumeNotification(
                            HANDLE(hwnd.0),
                            REGISTER_NOTIFICATION_FLAGS(0),
                        )
                        .ok()
                    } else {
                        None
                    };
                    if !stop.load(Ordering::Acquire) {
                        let mut msg = MSG::default();
                        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                            let _ = DispatchMessageW(&msg);
                        }
                    }
                    WATCHER_HWND.store(0, Ordering::Release);
                    if let Some(handle) = suspend_notify {
                        let _ = UnregisterSuspendResumeNotification(handle);
                    }
                    if let Some(handle) = power_notify {
                        let _ = UnregisterPowerSettingNotification(handle);
                    }
                    if wts_registered {
                        let _ = WTSUnRegisterSessionNotification(hwnd);
                    }
                    let _ = DestroyWindow(hwnd);
                }
            });
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::PowerSink;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    pub fn start(_sink: Arc<PowerSink>, _stop: Arc<AtomicBool>) {}
    pub fn stop() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_event_tracks_sleep_display_lock_and_battery() {
        let mut state = PowerState::default();
        assert_eq!(
            state,
            PowerState {
                sleeping: false,
                display_asleep: false,
                session_locked: false,
                on_battery: false,
            }
        );

        assert!(apply_event(&mut state, PowerEvent::SystemSleep));
        assert!(state.sleeping);
        assert!(!apply_event(&mut state, PowerEvent::SystemSleep));

        assert!(apply_event(&mut state, PowerEvent::DisplaySleep));
        assert!(state.display_asleep);

        assert!(apply_event(&mut state, PowerEvent::SessionLock));
        assert!(state.session_locked);

        assert!(apply_event(&mut state, PowerEvent::BatteryChanged(true)));
        assert!(state.on_battery);
        assert!(!apply_event(&mut state, PowerEvent::BatteryChanged(true)));

        assert!(apply_event(&mut state, PowerEvent::SystemWake));
        assert!(!state.sleeping);
        assert!(state.display_asleep);
        assert!(state.session_locked);
        assert!(state.on_battery);

        assert!(apply_event(&mut state, PowerEvent::DisplayWake));
        assert!(!state.display_asleep);

        assert!(apply_event(&mut state, PowerEvent::SessionUnlock));
        assert!(!state.session_locked);

        assert!(apply_event(&mut state, PowerEvent::BatteryChanged(false)));
        assert_eq!(state, PowerState::default());
    }

    #[test]
    fn power_sink_broadcasts_only_on_state_change() {
        let (tx, rx) = watch::channel(PowerState::default());
        let sink = PowerSink::new(tx);

        sink.emit(PowerEvent::DisplaySleep);
        assert!(rx.borrow().display_asleep);
        assert!(rx.has_changed().unwrap());

        // Consume change mark, then emit a no-op duplicate event.
        let _ = *rx.borrow();
        let mut rx_watch = rx;
        let _ = rx_watch.borrow_and_update();
        sink.emit(PowerEvent::DisplaySleep);
        assert!(!rx_watch.has_changed().unwrap());

        sink.emit(PowerEvent::BatteryChanged(true));
        assert!(rx_watch.has_changed().unwrap());
        assert!(rx_watch.borrow_and_update().on_battery);
    }

    #[test]
    fn parses_pmset_battery_and_ac_output() {
        let on_batt = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=123)\t82%; discharging; 3:40 remaining present: true\n";
        let on_ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=123)\t100%; charged; 0:00 remaining present: true\n";
        let desktop_ac = "Now drawing from 'AC Power'\n";

        assert!(parse_pmset_on_battery(on_batt));
        assert!(!parse_pmset_on_battery(on_ac));
        assert!(!parse_pmset_on_battery(desktop_ac));
    }
}
