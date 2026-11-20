use crate::state::AppState;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::System;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use truongphoto_core::{AppSettings, PowerState, ThrottleStatus, WsServerEvent};
use truongphoto_db::repo;

const LOAD_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Computes unified sleep, battery, and high-CPU throttle state with priority:
/// `sleep` > `battery` > `high_cpu` > inactive.
pub fn compute_throttle_status(
    power: &PowerState,
    cpu_usage_percent: f32,
    pause_on_battery: bool,
    cpu_throttle_percent: u8,
) -> ThrottleStatus {
    let clamped_cpu = if cpu_usage_percent.is_finite() {
        cpu_usage_percent.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let threshold = f32::from(cpu_throttle_percent.clamp(30, 100));

    let reason = if power.sleeping {
        Some("sleep".to_string())
    } else if power.on_battery && pause_on_battery {
        Some("battery".to_string())
    } else if clamped_cpu >= threshold {
        Some("high_cpu".to_string())
    } else {
        None
    };

    ThrottleStatus {
        active: reason.is_some(),
        reason,
        cpu_usage_percent: clamped_cpu,
        on_battery: power.on_battery,
    }
}

/// Spawns the 5-second CPU & power load monitor publishing `ThrottleStatus`
/// and broadcasting `WsServerEvent::ThrottleStateChanged` on state/reason transitions.
pub fn spawn_load_monitor(
    state: Arc<AppState>,
    shutdown_token: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut sys = System::new();
        sys.refresh_cpu_usage();

        let mut interval = tokio::time::interval(LOAD_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip immediate t=0 tick so first CPU sample has a baseline window.
        interval.tick().await;

        let mut power_rx = state.power_rx.clone();
        let mut last_cpu: f32 = 0.0;

        loop {
            let refresh_cpu = tokio::select! {
                _ = shutdown_token.cancelled() => break,
                _ = interval.tick() => true,
                changed = power_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    false
                }
            };

            if refresh_cpu {
                sys.refresh_cpu_usage();
                last_cpu = sys.global_cpu_usage();
            }

            let power = *power_rx.borrow_and_update();
            let default_paths = state.default_paths.clone();
            let settings = state
                .db
                .read(move |conn| repo::settings::load_app_settings(conn, &default_paths))
                .await
                .unwrap_or_else(|_| AppSettings::from_paths(&state.default_paths));

            let next = compute_throttle_status(
                &power,
                last_cpu,
                settings.pause_on_battery,
                settings.cpu_throttle_percent,
            );

            let prev = state.throttle_rx.borrow().clone();
            let transition = prev.active != next.active || prev.reason != next.reason;
            let _ = state.throttle_tx.send(next.clone());

            if transition {
                let _ = state
                    .ws_tx
                    .send(WsServerEvent::ThrottleStateChanged { throttle: next });
            }
        }
    })
}
