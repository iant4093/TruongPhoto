use crate::config::AppSettings;
use crate::models::{AiTier, JobStatus, PowerState, TailscaleStatus, ThrottleStatus};
use crate::rbac::Role;
use serde::{Deserialize, Serialize};

/// Events pushed from the Axum server to authenticated `/api/ws` clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerEvent {
    Connected {
        server_version: String,
        user_id: String,
        role: Role,
    },
    Heartbeat {
        uptime_secs: u64,
        process_ram_bytes: u64,
        db_size_bytes: u64,
        active_jobs: u64,
    },
    JobProgress {
        job_id: i64,
        kind: String,
        status: JobStatus,
        processed: u64,
        total: Option<u64>,
        message: Option<String>,
    },
    FolderChanged {
        folder_id: String,
        action: String,
    },
    PowerStateChanged {
        power: PowerState,
    },
    ThrottleStateChanged {
        throttle: ThrottleStatus,
    },
    TailscaleStatusChanged {
        status: TailscaleStatus,
    },
    SettingsChanged {
        settings: AppSettings,
    },
    AiStateChanged {
        active_vision_tier: AiTier,
        staging_vision_tier: Option<AiTier>,
        paused: bool,
    },
    Pong,
}

/// Messages sent from `/api/ws` clients to the Axum server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientMessage {
    Ping,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_events_serialize_tagged_snake_case() {
        let connected = WsServerEvent::Connected {
            server_version: "0.1.0".to_string(),
            user_id: "u1".to_string(),
            role: Role::Admin,
        };
        let v = serde_json::to_value(&connected).unwrap();
        assert_eq!(v["type"], "connected");
        assert_eq!(v["role"], "admin");

        let pong = serde_json::to_string(&WsServerEvent::Pong).unwrap();
        assert_eq!(pong, r#"{"type":"pong"}"#);

        let ping: WsClientMessage = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert_eq!(ping, WsClientMessage::Ping);

        let power = WsServerEvent::PowerStateChanged {
            power: PowerState {
                sleeping: false,
                display_asleep: false,
                session_locked: false,
                on_battery: true,
            },
        };
        let pv = serde_json::to_value(&power).unwrap();
        assert_eq!(pv["type"], "power_state_changed");
        assert_eq!(pv["power"]["on_battery"], true);

        let ai_evt = WsServerEvent::AiStateChanged {
            active_vision_tier: AiTier::Tier2,
            staging_vision_tier: Some(AiTier::Tier3),
            paused: false,
        };
        let av = serde_json::to_value(&ai_evt).unwrap();
        assert_eq!(av["type"], "ai_state_changed");
        assert_eq!(av["active_vision_tier"], "tier2");
        assert_eq!(av["staging_vision_tier"], "tier3");
        assert_eq!(av["paused"], false);

        let throttle_evt = WsServerEvent::ThrottleStateChanged {
            throttle: ThrottleStatus {
                active: true,
                reason: Some("battery".to_string()),
                cpu_usage_percent: 42.5,
                on_battery: true,
            },
        };
        let throttle_json = serde_json::to_string(&throttle_evt).unwrap();
        let tv: serde_json::Value = serde_json::from_str(&throttle_json).unwrap();
        assert_eq!(tv["type"], "throttle_state_changed");
        assert_eq!(tv["throttle"]["active"], true);
        assert_eq!(tv["throttle"]["reason"], "battery");
        assert_eq!(
            serde_json::from_str::<WsServerEvent>(&throttle_json).unwrap(),
            throttle_evt
        );

        let ts_evt = WsServerEvent::TailscaleStatusChanged {
            status: TailscaleStatus {
                installed: true,
                running: true,
                logged_in: true,
                cli_path: Some("/usr/local/bin/tailscale".to_string()),
                magic_dns_name: Some("photo.ts.net".to_string()),
                https_certs_enabled: true,
                serve_active: true,
                https_url: Some("https://photo.ts.net".to_string()),
                remediation: None,
                last_error: None,
            },
        };
        let ts_json = serde_json::to_string(&ts_evt).unwrap();
        let tsv: serde_json::Value = serde_json::from_str(&ts_json).unwrap();
        assert_eq!(tsv["type"], "tailscale_status_changed");
        assert_eq!(tsv["status"]["serve_active"], true);
        assert_eq!(
            serde_json::from_str::<WsServerEvent>(&ts_json).unwrap(),
            ts_evt
        );
    }
}
