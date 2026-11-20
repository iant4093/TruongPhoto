use crate::error::CoreError;
use crate::models::AiTier;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

/// Default HTTP/WebSocket listen port for TruongPhoto.
pub const DEFAULT_PORT: u16 = 38420;
/// Default mDNS local hostname.
pub const DEFAULT_MDNS_HOSTNAME: &str = "truongphoto.local";

/// Map basemap rendering mode (`offline_pmtiles` by default, zero external network calls).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MapTileMode {
    #[default]
    OfflinePmtiles,
    OnlineOpenfreemap,
}

impl MapTileMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OfflinePmtiles => "offline_pmtiles",
            Self::OnlineOpenfreemap => "online_openfreemap",
        }
    }
}

impl fmt::Display for MapTileMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MapTileMode {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "offline_pmtiles" => Ok(Self::OfflinePmtiles),
            "online_openfreemap" => Ok(Self::OnlineOpenfreemap),
            other => Err(CoreError::InvalidEnum(format!("map_tile_mode: {other}"))),
        }
    }
}

/// Resolved filesystem layout under a TruongPhoto data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoragePaths {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub managed_uploads_dir: PathBuf,
    pub thumbnails_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub map_pmtiles_dir: PathBuf,
}

/// JSON DTO of default storage paths returned by `GET /api/setup/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultPathsDto {
    pub managed_uploads_dir: String,
    pub thumbnails_dir: String,
    pub cache_dir: String,
    pub backup_dir: String,
}

impl StoragePaths {
    /// Returns the OS-appropriate default application data directory.
    pub fn default_data_dir() -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("TruongPhoto");
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
                return PathBuf::from(local_app_data).join("TruongPhoto");
            }
            if let Some(user_profile) = std::env::var_os("USERPROFILE") {
                return PathBuf::from(user_profile)
                    .join("AppData")
                    .join("Local")
                    .join("TruongPhoto");
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
                return PathBuf::from(xdg).join("truongphoto");
            }
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("truongphoto");
            }
        }

        PathBuf::from(".truongphoto")
    }

    /// Derives all standard subdirectories and SQLite database path from `data_dir`.
    pub fn from_data_dir(data_dir: PathBuf) -> Self {
        let db_path = data_dir.join("library.db");
        let managed_uploads_dir = data_dir.join("uploads");
        let thumbnails_dir = data_dir.join("thumbnails");
        let cache_dir = data_dir.join("cache");
        let backup_dir = data_dir.join("backups");
        let map_pmtiles_dir = data_dir.join("maps");
        Self {
            data_dir,
            db_path,
            managed_uploads_dir,
            thumbnails_dir,
            cache_dir,
            backup_dir,
            map_pmtiles_dir,
        }
    }

    /// Converts directory paths into the `GET /api/setup/status` JSON DTO.
    pub fn to_default_paths_dto(&self) -> DefaultPathsDto {
        DefaultPathsDto {
            managed_uploads_dir: self.managed_uploads_dir.to_string_lossy().into_owned(),
            thumbnails_dir: self.thumbnails_dir.to_string_lossy().into_owned(),
            cache_dir: self.cache_dir.to_string_lossy().into_owned(),
            backup_dir: self.backup_dir.to_string_lossy().into_owned(),
        }
    }

    /// Creates the data directory and core subdirectories if they do not exist.
    pub fn ensure_dirs(&self) -> Result<(), CoreError> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.managed_uploads_dir)?;
        std::fs::create_dir_all(&self.thumbnails_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.backup_dir)?;
        std::fs::create_dir_all(&self.map_pmtiles_dir)?;
        Ok(())
    }
}

impl Default for StoragePaths {
    fn default() -> Self {
        Self::from_data_dir(Self::default_data_dir())
    }
}

/// Runtime startup configuration passed to `truongphoto-server`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub mdns_enabled: bool,
    pub mdns_hostname: String,
    pub desktop_bootstrap_token: Option<String>,
}

impl ServerConfig {
    /// Builds a `ServerConfig` anchored at the specified `data_dir`.
    pub fn from_data_dir(data_dir: PathBuf) -> Self {
        let paths = StoragePaths::from_data_dir(data_dir);
        Self {
            host: "0.0.0.0".to_string(),
            port: DEFAULT_PORT,
            data_dir: paths.data_dir,
            db_path: paths.db_path,
            mdns_enabled: true,
            mdns_hostname: DEFAULT_MDNS_HOSTNAME.to_string(),
            desktop_bootstrap_token: None,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self::from_data_dir(StoragePaths::default_data_dir())
    }
}

fn default_map_pmtiles_dir() -> String {
    StoragePaths::default()
        .map_pmtiles_dir
        .to_string_lossy()
        .into_owned()
}

const fn default_backup_interval_hours() -> u32 {
    24
}

const fn default_pause_on_battery() -> bool {
    true
}

const fn default_cpu_throttle_percent() -> u8 {
    80
}

/// Persisted server/library settings exposed via `GET /api/admin/settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub port: u16,
    pub mdns_enabled: bool,
    pub mdns_hostname: String,
    pub desktop_pin_lock: bool,
    pub managed_uploads_dir: String,
    pub thumbnails_dir: String,
    pub cache_dir: String,
    pub cache_limit_gb: u32,
    pub backup_dir: String,
    pub backup_keep_count: u32,
    #[serde(default = "default_backup_interval_hours")]
    pub backup_interval_hours: u32,
    pub video_max_frames: u32,
    pub ai_tier_vision: AiTier,
    pub ai_tier_chat: AiTier,
    #[serde(default)]
    pub map_tile_mode: MapTileMode,
    #[serde(default = "default_map_pmtiles_dir")]
    pub map_pmtiles_dir: String,
    #[serde(default = "default_pause_on_battery")]
    pub pause_on_battery: bool,
    #[serde(default = "default_cpu_throttle_percent")]
    pub cpu_throttle_percent: u8,
}

impl AppSettings {
    /// Constructs default settings initialized with paths from `StoragePaths`.
    pub fn from_paths(paths: &StoragePaths) -> Self {
        Self {
            port: DEFAULT_PORT,
            mdns_enabled: true,
            mdns_hostname: DEFAULT_MDNS_HOSTNAME.to_string(),
            desktop_pin_lock: false,
            managed_uploads_dir: paths.managed_uploads_dir.to_string_lossy().into_owned(),
            thumbnails_dir: paths.thumbnails_dir.to_string_lossy().into_owned(),
            cache_dir: paths.cache_dir.to_string_lossy().into_owned(),
            cache_limit_gb: 10,
            backup_dir: paths.backup_dir.to_string_lossy().into_owned(),
            backup_keep_count: 7,
            backup_interval_hours: default_backup_interval_hours(),
            video_max_frames: 30,
            ai_tier_vision: AiTier::Tier2,
            ai_tier_chat: AiTier::Tier2,
            map_tile_mode: MapTileMode::OfflinePmtiles,
            map_pmtiles_dir: paths.map_pmtiles_dir.to_string_lossy().into_owned(),
            pause_on_battery: default_pause_on_battery(),
            cpu_throttle_percent: default_cpu_throttle_percent(),
        }
    }

    /// Applies any populated fields from a `PartialAppSettings` patch.
    pub fn apply_partial(&mut self, patch: PartialAppSettings) {
        if let Some(port) = patch.port {
            self.port = port;
        }
        if let Some(mdns_enabled) = patch.mdns_enabled {
            self.mdns_enabled = mdns_enabled;
        }
        if let Some(mdns_hostname) = patch.mdns_hostname {
            self.mdns_hostname = mdns_hostname;
        }
        if let Some(desktop_pin_lock) = patch.desktop_pin_lock {
            self.desktop_pin_lock = desktop_pin_lock;
        }
        if let Some(managed_uploads_dir) = patch.managed_uploads_dir {
            self.managed_uploads_dir = managed_uploads_dir;
        }
        if let Some(thumbnails_dir) = patch.thumbnails_dir {
            self.thumbnails_dir = thumbnails_dir;
        }
        if let Some(cache_dir) = patch.cache_dir {
            self.cache_dir = cache_dir;
        }
        if let Some(cache_limit_gb) = patch.cache_limit_gb {
            self.cache_limit_gb = cache_limit_gb;
        }
        if let Some(backup_dir) = patch.backup_dir {
            self.backup_dir = backup_dir;
        }
        if let Some(backup_keep_count) = patch.backup_keep_count {
            self.backup_keep_count = backup_keep_count;
        }
        if let Some(backup_interval_hours) = patch.backup_interval_hours {
            self.backup_interval_hours = backup_interval_hours;
        }
        if let Some(video_max_frames) = patch.video_max_frames {
            self.video_max_frames = video_max_frames;
        }
        if let Some(ai_tier_vision) = patch.ai_tier_vision {
            self.ai_tier_vision = ai_tier_vision;
        }
        if let Some(ai_tier_chat) = patch.ai_tier_chat {
            self.ai_tier_chat = ai_tier_chat;
        }
        if let Some(map_tile_mode) = patch.map_tile_mode {
            self.map_tile_mode = map_tile_mode;
        }
        if let Some(map_pmtiles_dir) = patch.map_pmtiles_dir {
            self.map_pmtiles_dir = map_pmtiles_dir;
        }
        if let Some(pause_on_battery) = patch.pause_on_battery {
            self.pause_on_battery = pause_on_battery;
        }
        if let Some(cpu_throttle_percent) = patch.cpu_throttle_percent {
            self.cpu_throttle_percent = cpu_throttle_percent.clamp(30, 100);
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::from_paths(&StoragePaths::default())
    }
}

/// Partial settings update payload accepted by `PATCH /api/admin/settings`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialAppSettings {
    pub port: Option<u16>,
    pub mdns_enabled: Option<bool>,
    pub mdns_hostname: Option<String>,
    pub desktop_pin_lock: Option<bool>,
    pub managed_uploads_dir: Option<String>,
    pub thumbnails_dir: Option<String>,
    pub cache_dir: Option<String>,
    pub cache_limit_gb: Option<u32>,
    pub backup_dir: Option<String>,
    pub backup_keep_count: Option<u32>,
    pub backup_interval_hours: Option<u32>,
    pub video_max_frames: Option<u32>,
    pub ai_tier_vision: Option<AiTier>,
    pub ai_tier_chat: Option<AiTier>,
    pub map_tile_mode: Option<MapTileMode>,
    pub map_pmtiles_dir: Option<String>,
    pub pause_on_battery: Option<bool>,
    pub cpu_throttle_percent: Option<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_paths_from_data_dir_layout() {
        let base = PathBuf::from("/tmp/tp-test-data");
        let paths = StoragePaths::from_data_dir(base.clone());
        assert_eq!(paths.data_dir, base);
        assert_eq!(paths.db_path, base.join("library.db"));
        assert_eq!(paths.managed_uploads_dir, base.join("uploads"));
        assert_eq!(paths.thumbnails_dir, base.join("thumbnails"));
        assert_eq!(paths.cache_dir, base.join("cache"));
        assert_eq!(paths.backup_dir, base.join("backups"));
        assert_eq!(paths.map_pmtiles_dir, base.join("maps"));
    }

    #[test]
    fn app_settings_partial_update_and_serde() {
        let paths = StoragePaths::from_data_dir(PathBuf::from("/tmp/tp-settings"));
        let mut settings = AppSettings::from_paths(&paths);
        assert_eq!(settings.port, 38420);
        assert!(settings.mdns_enabled);
        assert_eq!(settings.backup_keep_count, 7);
        assert_eq!(settings.map_tile_mode, MapTileMode::OfflinePmtiles);
        assert_eq!(
            settings.map_pmtiles_dir,
            PathBuf::from("/tmp/tp-settings")
                .join("maps")
                .to_string_lossy()
        );

        let patch: PartialAppSettings = serde_json::from_str(
            r#"{"mdns_enabled":false,"cache_limit_gb":25,"ai_tier_chat":"tier3","map_tile_mode":"online_openfreemap","map_pmtiles_dir":"/custom/maps"}"#,
        )
        .unwrap();
        settings.apply_partial(patch);

        assert!(!settings.mdns_enabled);
        assert_eq!(settings.cache_limit_gb, 25);
        assert_eq!(settings.ai_tier_chat, AiTier::Tier3);
        assert_eq!(settings.port, 38420);
        assert_eq!(settings.map_tile_mode, MapTileMode::OnlineOpenfreemap);
        assert_eq!(settings.map_pmtiles_dir, "/custom/maps");
    }

    #[test]
    fn map_tile_mode_roundtrip_and_backward_compat_settings() {
        assert_eq!(MapTileMode::default(), MapTileMode::OfflinePmtiles);
        assert_eq!(
            serde_json::to_string(&MapTileMode::OfflinePmtiles).unwrap(),
            "\"offline_pmtiles\""
        );
        assert_eq!(
            serde_json::to_string(&MapTileMode::OnlineOpenfreemap).unwrap(),
            "\"online_openfreemap\""
        );
        assert_eq!(
            "offline_pmtiles".parse::<MapTileMode>().unwrap(),
            MapTileMode::OfflinePmtiles
        );
        assert_eq!(
            "online_openfreemap".parse::<MapTileMode>().unwrap(),
            MapTileMode::OnlineOpenfreemap
        );
        assert!("unknown_mode".parse::<MapTileMode>().is_err());

        let old_json = r#"{"port":38420,"mdns_enabled":true}"#;
        let parsed: AppSettings = serde_json::from_str(old_json).unwrap();
        assert_eq!(parsed.map_tile_mode, MapTileMode::OfflinePmtiles);
        assert!(!parsed.map_pmtiles_dir.is_empty());
        assert!(parsed.map_pmtiles_dir.ends_with("maps"));
        assert_eq!(parsed.backup_interval_hours, 24);
        assert!(parsed.pause_on_battery);
        assert_eq!(parsed.cpu_throttle_percent, 80);
    }

    #[test]
    fn phase4_settings_defaults_and_clamping() {
        let paths = StoragePaths::from_data_dir(PathBuf::from("/tmp/tp-p4"));
        let mut settings = AppSettings::from_paths(&paths);
        assert_eq!(settings.backup_interval_hours, 24);
        assert!(settings.pause_on_battery);
        assert_eq!(settings.cpu_throttle_percent, 80);

        // Clamp below lower bound (30)
        let low_patch: PartialAppSettings = serde_json::from_str(
            r#"{"backup_interval_hours":0,"pause_on_battery":false,"cpu_throttle_percent":10}"#,
        )
        .unwrap();
        settings.apply_partial(low_patch);
        assert_eq!(settings.backup_interval_hours, 0);
        assert!(!settings.pause_on_battery);
        assert_eq!(settings.cpu_throttle_percent, 30);

        // Clamp above upper bound (100)
        let high_patch: PartialAppSettings =
            serde_json::from_str(r#"{"cpu_throttle_percent":255}"#).unwrap();
        settings.apply_partial(high_patch);
        assert_eq!(settings.cpu_throttle_percent, 100);

        // In-range value preserved
        let mid_patch: PartialAppSettings =
            serde_json::from_str(r#"{"backup_interval_hours":6,"cpu_throttle_percent":65}"#)
                .unwrap();
        settings.apply_partial(mid_patch);
        assert_eq!(settings.backup_interval_hours, 6);
        assert_eq!(settings.cpu_throttle_percent, 65);
    }
}
