use crate::error::CoreError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub use crate::config::MapTileMode;
pub use crate::rbac::Role;

/// Client transport kind recorded on authenticated sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Web,
    TauriDesktop,
    Tailscale,
}

impl ClientKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::TauriDesktop => "tauri_desktop",
            Self::Tailscale => "tailscale",
        }
    }
}

impl fmt::Display for ClientKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ClientKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "web" => Ok(Self::Web),
            "tauri_desktop" => Ok(Self::TauriDesktop),
            "tailscale" => Ok(Self::Tailscale),
            other => Err(CoreError::InvalidEnum(format!("client_kind: {other}"))),
        }
    }
}

/// Full user row stored in SQLite (`users` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRecord {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub pin_hash: String,
    pub avatar_color: String,
    pub disabled_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Public user profile returned by auth/setup endpoints (never includes `pin_hash`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPublic {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub avatar_color: String,
}

impl From<&UserRecord> for UserPublic {
    fn from(user: &UserRecord) -> Self {
        Self {
            id: user.id.clone(),
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            role: user.role,
            avatar_color: user.avatar_color.clone(),
        }
    }
}

impl From<UserRecord> for UserPublic {
    fn from(user: UserRecord) -> Self {
        Self {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
            role: user.role,
            avatar_color: user.avatar_color,
        }
    }
}

/// Compact profile card returned after passing the 6-digit Server PIN gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub id: String,
    pub display_name: String,
    pub role: Role,
    pub avatar_color: String,
}

impl From<&UserRecord> for ProfileSummary {
    fn from(user: &UserRecord) -> Self {
        Self {
            id: user.id.clone(),
            display_name: user.display_name.clone(),
            role: user.role,
            avatar_color: user.avatar_color.clone(),
        }
    }
}

/// Admin user management projection including folder ACL grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUserView {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub avatar_color: String,
    pub disabled_at_ms: Option<i64>,
    pub allowed_folder_ids: Vec<String>,
    pub created_at_ms: i64,
}

/// Authenticated session row stored in SQLite (`sessions` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub token_hash: String,
    pub user_id: String,
    pub client_kind: ClientKind,
    pub user_agent: Option<String>,
    pub ip_addr: Option<String>,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub expires_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

/// Watched folder row stored in SQLite (`folders` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRecord {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub volume_uuid: Option<String>,
    pub volume_label: Option<String>,
    pub is_managed_uploads: bool,
    pub allow_disk_delete: bool,
    pub is_offline: bool,
    pub last_scanned_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// API view of a watched folder including asset count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderView {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub volume_uuid: Option<String>,
    pub volume_label: Option<String>,
    pub is_managed_uploads: bool,
    pub allow_disk_delete: bool,
    pub is_offline: bool,
    pub last_scanned_at_ms: Option<i64>,
    pub asset_count: u64,
    pub created_at_ms: i64,
}

/// Logical media asset classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Photo,
    Video,
    LivePhoto,
    MotionPhoto,
    RawJpgStack,
}

impl AssetKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Video => "video",
            Self::LivePhoto => "live_photo",
            Self::MotionPhoto => "motion_photo",
            Self::RawJpgStack => "raw_jpg_stack",
        }
    }
}

impl fmt::Display for AssetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AssetKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "photo" => Ok(Self::Photo),
            "video" => Ok(Self::Video),
            "live_photo" => Ok(Self::LivePhoto),
            "motion_photo" => Ok(Self::MotionPhoto),
            "raw_jpg_stack" => Ok(Self::RawJpgStack),
            other => Err(CoreError::InvalidEnum(format!("asset_kind: {other}"))),
        }
    }
}

/// Physical file role inside an asset stack (`asset_files.file_role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    Primary,
    Raw,
    JpegCompanion,
    LiveVideo,
    MotionEmbedded,
}

impl FileRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Raw => "raw",
            Self::JpegCompanion => "jpeg_companion",
            Self::LiveVideo => "live_video",
            Self::MotionEmbedded => "motion_embedded",
        }
    }
}

impl fmt::Display for FileRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FileRole {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "primary" => Ok(Self::Primary),
            "raw" => Ok(Self::Raw),
            "jpeg_companion" => Ok(Self::JpegCompanion),
            "live_video" => Ok(Self::LiveVideo),
            "motion_embedded" => Ok(Self::MotionEmbedded),
            other => Err(CoreError::InvalidEnum(format!("file_role: {other}"))),
        }
    }
}

/// Timeline / library grid summary DTO backed by `v_assets_effective`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetSummary {
    pub id: String,
    pub folder_id: String,
    pub kind: AssetKind,
    pub capture_at_ms: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    pub aspect_ratio: Option<f64>,
    pub is_favorite: bool,
    pub deleted_at_ms: Option<i64>,
    pub thumb_rel_path: Option<String>,
    pub thumb_ready: bool,
    pub title: Option<String>,
    pub rating: Option<u8>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub geo_city: Option<String>,
    pub geo_country: Option<String>,
    pub camera_model: Option<String>,
}

/// Year/month asset count bucket for the timeline scrubber.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineBucket {
    pub year: i32,
    pub month: u32,
    pub count: u64,
}

/// Album summary DTO returned by `GET /api/albums`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumSummary {
    pub id: String,
    pub owner_user_id: String,
    pub title: String,
    pub description: Option<String>,
    pub cover_asset_id: Option<String>,
    pub sort_order: String,
    pub asset_count: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// RBAC-filtered Person cluster summary returned by `GET /api/people`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonSummary {
    pub id: String,
    pub name: Option<String>,
    pub is_hidden: bool,
    pub is_pinned: bool,
    pub cover_face_id: Option<String>,
    pub face_count: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Background job lifecycle status (`jobs.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JobStatus {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(CoreError::InvalidEnum(format!("job_status: {other}"))),
        }
    }
}

/// Persistent background job row (`jobs` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: i64,
    pub kind: String,
    pub payload_json: String,
    pub status: JobStatus,
    pub priority: i32,
    pub attempts: i32,
    pub max_attempts: i32,
    pub error_message: Option<String>,
    pub run_after_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Recommended or configured AI compute tier based on host RAM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiTier {
    Tier1,
    #[default]
    Tier2,
    Tier3,
}

impl AiTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tier1 => "tier1",
            Self::Tier2 => "tier2",
            Self::Tier3 => "tier3",
        }
    }

    /// Embedding vector dimension of the SigLIP2 vision model for this tier.
    pub const fn dim(&self) -> usize {
        match self {
            Self::Tier1 => 768,
            Self::Tier2 => 1024,
            Self::Tier3 => 1152,
        }
    }

    /// Canonical model ID in the Phase 2 AI model manifest for this vision tier.
    pub const fn vision_model_id(&self) -> &'static str {
        match self {
            Self::Tier1 => "siglip2-base",
            Self::Tier2 => "siglip2-large",
            Self::Tier3 => "siglip2-so400m",
        }
    }
}

impl fmt::Display for AiTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AiTier {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tier1" => Ok(Self::Tier1),
            "tier2" => Ok(Self::Tier2),
            "tier3" => Ok(Self::Tier3),
            other => Err(CoreError::InvalidEnum(format!("ai_tier: {other}"))),
        }
    }
}

/// Detected host hardware capabilities and tier recommendations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub total_ram_bytes: u64,
    pub cpu_cores: usize,
    pub recommended_vision_tier: AiTier,
    pub recommended_chat_tier: AiTier,
}

/// OS sleep, lock, and battery state reported by `src-tauri/src/power.rs`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerState {
    pub sleeping: bool,
    pub display_asleep: bool,
    pub session_locked: bool,
    pub on_battery: bool,
}

/// Aggregate library metrics included in `/api/system/info`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryStats {
    pub asset_count: u64,
    pub folder_count: u64,
    pub db_size_bytes: u64,
}

/// Payload returned by the `get_desktop_bootstrap` Tauri IPC command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopBootstrapInfo {
    pub api_base_url: String,
    pub desktop_bootstrap_token: String,
    pub port: u16,
    pub mdns_url: Option<String>,
    pub version: String,
    pub platform: String,
}

/// Payload returned by the `check_update_feed` Tauri IPC command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateFeedStatus {
    pub available: bool,
    pub version: Option<String>,
    pub notes: Option<String>,
}

/// Physical file entry attached to a logical asset (`asset_files` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetFileView {
    pub id: String,
    pub asset_id: String,
    pub folder_id: String,
    pub rel_path: String,
    pub file_role: FileRole,
    pub mime_type: String,
    pub extension: String,
    pub file_size: u64,
    pub mtime_ms: i64,
    pub partial_blake3: String,
    pub content_identifier: Option<String>,
    pub embedded_video_offset: Option<u64>,
    pub embedded_video_length: Option<u64>,
    pub is_missing: bool,
}

/// Deterministic EXIF and offline reverse-geocoded photo metadata (`deterministic_photo_meta`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotoMetaView {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub focal_length_35mm: Option<u32>,
    pub aperture_f: Option<f64>,
    pub shutter_speed_sec: Option<f64>,
    pub iso: Option<u32>,
    pub orientation: Option<u16>,
    pub color_space: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub gps_alt_m: Option<f64>,
    pub geo_city: Option<String>,
    pub geo_admin1: Option<String>,
    pub geo_country: Option<String>,
    pub geo_country_code: Option<String>,
    pub taken_at_ms: Option<i64>,
}

/// Deterministic video stream and offline reverse-geocoded metadata (`deterministic_video_meta`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoMetaView {
    pub duration_ms: i64,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub fps: Option<f64>,
    pub bitrate_bps: Option<u64>,
    pub bit_depth: Option<u8>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub is_hdr: bool,
    pub rotation: i32,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub geo_city: Option<String>,
    pub geo_admin1: Option<String>,
    pub geo_country: Option<String>,
    pub geo_country_code: Option<String>,
    pub taken_at_ms: Option<i64>,
}

/// Non-destructive user metadata overrides (`user_overrides` table).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserOverridesView {
    pub title: Option<String>,
    pub description: Option<String>,
    pub capture_at_ms: Option<i64>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub geo_city: Option<String>,
    pub geo_admin1: Option<String>,
    pub geo_country: Option<String>,
    pub geo_country_code: Option<String>,
    pub rating: Option<u8>,
    pub updated_by: Option<String>,
    pub updated_at_ms: i64,
}

/// Partial update payload accepted by `PATCH /api/assets/{id}/overrides`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateOverridesRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub capture_at_ms: Option<i64>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub geo_city: Option<String>,
    pub geo_admin1: Option<String>,
    pub geo_country: Option<String>,
    pub geo_country_code: Option<String>,
    pub rating: Option<u8>,
}

/// Origin of a media tag (`media_tags.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagSource {
    Ai,
    User,
}

impl TagSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::User => "user",
        }
    }
}

impl fmt::Display for TagSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TagSource {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ai" => Ok(Self::Ai),
            "user" => Ok(Self::User),
            other => Err(CoreError::InvalidEnum(format!("tag_source: {other}"))),
        }
    }
}

/// Lifecycle state of a media tag (`media_tags.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagState {
    Active,
    Rejected,
}

impl TagState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Rejected => "rejected",
        }
    }
}

impl fmt::Display for TagState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TagState {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "rejected" => Ok(Self::Rejected),
            other => Err(CoreError::InvalidEnum(format!("tag_state: {other}"))),
        }
    }
}

/// Tag projection returned in asset details and tag mutation endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaTagView {
    pub tag: String,
    pub source: TagSource,
    pub state: TagState,
    pub confidence: Option<f64>,
}

/// Number of bins per channel in `Histogram256`.
pub const HISTOGRAM_BINS: usize = 256;

/// Compact 256-bin RGB + Rec.709 Luma histogram normalized to `u16` (`0..=65535`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Histogram256 {
    pub r: Vec<u16>,
    pub g: Vec<u16>,
    pub b: Vec<u16>,
    pub luma: Vec<u16>,
}

impl Histogram256 {
    /// Encodes histogram bin values as packed little-endian `u16` bytes.
    pub fn encode_bins_le(bins: &[u16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bins.len() * 2);
        for &bin in bins {
            out.extend_from_slice(&bin.to_le_bytes());
        }
        out
    }

    /// Decodes a 512-byte little-endian 256-bin histogram slice, returning `None` if malformed.
    pub fn decode_bins_le(bytes: &[u8]) -> Option<Vec<u16>> {
        if bytes.len() != HISTOGRAM_BINS * 2 {
            return None;
        }
        Some(
            bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect(),
        )
    }
}

/// Client media playback strategy for an asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    Image,
    Direct,
    Hls,
}

impl PlaybackMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Direct => "direct",
            Self::Hls => "hls",
        }
    }
}

impl fmt::Display for PlaybackMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PlaybackMode {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "image" => Ok(Self::Image),
            "direct" => Ok(Self::Direct),
            "hls" => Ok(Self::Hls),
            other => Err(CoreError::InvalidEnum(format!("playback_mode: {other}"))),
        }
    }
}

/// Resolved playback and rendition URLs returned inside `AssetDetail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackInfo {
    pub mode: PlaybackMode,
    pub direct_url: Option<String>,
    pub hls_url: Option<String>,
    pub live_video_url: Option<String>,
    pub has_raw: bool,
    pub preview_url: String,
    pub thumb_url: String,
    pub original_url: String,
}

/// Full asset inspection payload returned by `GET /api/assets/{id}` and `PATCH /api/assets/{id}/overrides`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetDetail {
    pub summary: AssetSummary,
    pub files: Vec<AssetFileView>,
    pub photo_meta: Option<PhotoMetaView>,
    pub video_meta: Option<VideoMetaView>,
    pub overrides: Option<UserOverridesView>,
    pub tags: Vec<MediaTagView>,
    pub histogram: Option<Histogram256>,
    pub album_ids: Vec<String>,
    pub playback: PlaybackInfo,
}

/// Result summary returned by `POST /api/trash/purge` and background trash retention sweeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeReport {
    pub purged_count: u64,
    pub disk_deleted_files: u64,
    pub db_only_assets: u64,
    pub partial_errors: Vec<String>,
}

/// Per-file failure entry inside `UploadResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadItemError {
    pub filename: String,
    pub message: String,
}

/// Response returned by `POST /api/uploads`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UploadResponse {
    pub uploaded: Vec<AssetSummary>,
    pub errors: Vec<UploadItemError>,
}

/// Album sorting mode (`albums.sort_order`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlbumSortOrder {
    #[default]
    CaptureDesc,
    CaptureAsc,
    AddedDesc,
    Custom,
}

impl AlbumSortOrder {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CaptureDesc => "capture_desc",
            Self::CaptureAsc => "capture_asc",
            Self::AddedDesc => "added_desc",
            Self::Custom => "custom",
        }
    }
}

impl fmt::Display for AlbumSortOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AlbumSortOrder {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "capture_desc" => Ok(Self::CaptureDesc),
            "capture_asc" => Ok(Self::CaptureAsc),
            "added_desc" => Ok(Self::AddedDesc),
            "custom" => Ok(Self::Custom),
            other => Err(CoreError::InvalidEnum(format!("album_sort_order: {other}"))),
        }
    }
}

/// Share permission level stored in `album_shares.permission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlbumSharePermission {
    View,
    Contribute,
}

impl AlbumSharePermission {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Contribute => "contribute",
        }
    }
}

impl fmt::Display for AlbumSharePermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AlbumSharePermission {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "view" => Ok(Self::View),
            "contribute" => Ok(Self::Contribute),
            other => Err(CoreError::InvalidEnum(format!(
                "album_share_permission: {other}"
            ))),
        }
    }
}

/// Effective permission of the current caller on an album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlbumPermission {
    Owner,
    Admin,
    Contribute,
    View,
}

impl AlbumPermission {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Contribute => "contribute",
            Self::View => "view",
        }
    }

    /// Returns true if the caller may rename/edit/delete the album or manage shares.
    pub const fn can_manage(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }

    /// Returns true if the caller may add or remove assets in the album.
    pub const fn can_contribute(self) -> bool {
        matches!(self, Self::Owner | Self::Admin | Self::Contribute)
    }
}

impl fmt::Display for AlbumPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AlbumPermission {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "contribute" => Ok(Self::Contribute),
            "view" => Ok(Self::View),
            other => Err(CoreError::InvalidEnum(format!("album_permission: {other}"))),
        }
    }
}

/// Share entry returned inside `AlbumDetail` and `PUT /api/albums/{id}/shares`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumShareView {
    pub user_id: String,
    pub display_name: String,
    pub avatar_color: String,
    pub permission: AlbumSharePermission,
}

/// Full album detail payload returned by `GET /api/albums/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumDetail {
    pub album: AlbumSummary,
    pub permission: AlbumPermission,
    pub assets: Vec<AssetSummary>,
    pub shares: Vec<AlbumShareView>,
}

/// Minimal non-disabled user projection returned by `GET /api/users/shareable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareableUser {
    pub id: String,
    pub display_name: String,
    pub avatar_color: String,
}

/// Evictable cache artifact kind (`lru_cache_entries.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheEntryKind {
    #[serde(rename = "preview_2048")]
    Preview2048,
    RawFull,
    HlsProxy,
    LiveProxy,
    MotionVideo,
}

impl CacheEntryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preview2048 => "preview_2048",
            Self::RawFull => "raw_full",
            Self::HlsProxy => "hls_proxy",
            Self::LiveProxy => "live_proxy",
            Self::MotionVideo => "motion_video",
        }
    }
}

impl fmt::Display for CacheEntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CacheEntryKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "preview_2048" => Ok(Self::Preview2048),
            "raw_full" => Ok(Self::RawFull),
            "hls_proxy" => Ok(Self::HlsProxy),
            "live_proxy" => Ok(Self::LiveProxy),
            "motion_video" => Ok(Self::MotionVideo),
            other => Err(CoreError::InvalidEnum(format!("cache_entry_kind: {other}"))),
        }
    }
}

/// Row tracked in `lru_cache_entries` for bounded disk cache eviction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LruCacheEntry {
    pub cache_key: String,
    pub asset_id: Option<String>,
    pub kind: CacheEntryKind,
    pub rel_path: String,
    pub size_bytes: u64,
    pub last_accessed_ms: i64,
    pub created_at_ms: i64,
}

/// Query execution strategy requested by `POST /api/search`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Hybrid,
    Semantic,
    Ocr,
    Filters,
}

impl SearchMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Semantic => "semantic",
            Self::Ocr => "ocr",
            Self::Filters => "filters",
        }
    }
}

impl fmt::Display for SearchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SearchMode {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "hybrid" => Ok(Self::Hybrid),
            "semantic" => Ok(Self::Semantic),
            "ocr" => Ok(Self::Ocr),
            "filters" => Ok(Self::Filters),
            other => Err(CoreError::InvalidEnum(format!("search_mode: {other}"))),
        }
    }
}

/// Vector/filter query plan selected by the Phase 2 hybrid query planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerMode {
    ExactSimd,
    Hnsw,
    SqlOnly,
}

impl PlannerMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactSimd => "exact_simd",
            Self::Hnsw => "hnsw",
            Self::SqlOnly => "sql_only",
        }
    }
}

impl fmt::Display for PlannerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PlannerMode {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "exact_simd" => Ok(Self::ExactSimd),
            "hnsw" => Ok(Self::Hnsw),
            "sql_only" => Ok(Self::SqlOnly),
            other => Err(CoreError::InvalidEnum(format!("planner_mode: {other}"))),
        }
    }
}

/// Hybrid semantic, OCR, and deterministic metadata filter search request (`POST /api/search`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchRequest {
    pub query: Option<String>,
    #[serde(default)]
    pub ocr_query: Option<String>,
    pub mode: SearchMode,
    pub folder_id: Option<String>,
    pub person_ids: Option<Vec<String>>,
    #[serde(default)]
    pub exclude_person_ids: Option<Vec<String>>,
    #[serde(default)]
    pub has_people: Option<bool>,
    #[serde(default)]
    pub min_faces: Option<u32>,
    #[serde(default)]
    pub max_faces: Option<u32>,
    pub tags: Option<Vec<String>>,
    pub camera_model: Option<String>,
    #[serde(default)]
    pub lens_model: Option<String>,
    #[serde(default)]
    pub focal_length_min_mm: Option<f64>,
    #[serde(default)]
    pub focal_length_max_mm: Option<f64>,
    #[serde(default)]
    pub aperture_min_f: Option<f64>,
    #[serde(default)]
    pub aperture_max_f: Option<f64>,
    #[serde(default)]
    pub iso_min: Option<u32>,
    #[serde(default)]
    pub iso_max: Option<u32>,
    #[serde(default)]
    pub shutter_speed_min_sec: Option<f64>,
    #[serde(default)]
    pub shutter_speed_max_sec: Option<f64>,
    #[serde(default)]
    pub orientation: Option<String>,
    #[serde(default)]
    pub sort_by: Option<String>,
    pub geo_query: Option<String>,
    #[serde(default)]
    pub exclude_geo_query: Option<String>,
    pub kind: Option<AssetKind>,
    #[serde(default)]
    pub exclude_kind: Option<AssetKind>,
    pub favorite: Option<bool>,
    pub date_from_ms: Option<i64>,
    pub date_to_ms: Option<i64>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Ranked asset hit returned inside `SearchResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub asset: AssetSummary,
    pub score: f32,
    pub timestamp_sec: Option<f64>,
    pub match_reasons: Vec<String>,
    pub ocr_snippet: Option<String>,
}

/// Search result envelope returned by `POST /api/search`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub items: Vec<SearchHit>,
    pub total_count: i64,
    pub planner_mode: PlannerMode,
    pub elapsed_ms: u64,
}

/// Detected face bounding box and optional person assignment (`faces` table).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceView {
    pub id: String,
    pub asset_id: String,
    pub person_id: Option<String>,
    pub person_name: Option<String>,
    pub bbox_x: f64,
    pub bbox_y: f64,
    pub bbox_w: f64,
    pub bbox_h: f64,
    pub det_score: f64,
    pub assigned_by: String,
}

/// Partial person update payload accepted by `PATCH /api/people/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatePersonRequest {
    pub name: Option<String>,
    pub is_hidden: Option<bool>,
    pub is_pinned: Option<bool>,
    pub cover_face_id: Option<String>,
}

/// Request payload accepted by `POST /api/people/merge`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePeopleRequest {
    pub source_person_ids: Vec<String>,
    pub target_person_id: String,
}

/// Response returned by `POST /api/people/merge`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePeopleResponse {
    pub person: PersonSummary,
    pub merged_faces: u64,
}

/// Request payload accepted by `POST /api/people/{id}/faces/{face_id}/reassign`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReassignFaceRequest {
    pub target_person_id: Option<String>,
    pub new_person_name: Option<String>,
}

/// Response returned by `POST /api/people/{id}/faces/{face_id}/reassign`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReassignFaceResponse {
    pub face_id: String,
    pub person_id: Option<String>,
}

/// Response returned by `POST /api/people/cluster`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFacesResponse {
    pub clusters_created: u64,
    pub faces_assigned: u64,
}

/// Installation lifecycle status of a local AI model (`ai_models.status`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiModelStatus {
    #[default]
    NotInstalled,
    Downloading,
    Installed,
    Error,
}

impl AiModelStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::Downloading => "downloading",
            Self::Installed => "installed",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for AiModelStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AiModelStatus {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "not_installed" => Ok(Self::NotInstalled),
            "downloading" => Ok(Self::Downloading),
            "installed" => Ok(Self::Installed),
            "error" => Ok(Self::Error),
            other => Err(CoreError::InvalidEnum(format!("ai_model_status: {other}"))),
        }
    }
}

/// Model card projection returned in `GET /api/admin/ai/models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiModelView {
    pub id: String,
    pub name: String,
    pub role: String,
    pub license: String,
    pub hf_repo: String,
    pub tier: Option<AiTier>,
    pub approx_bytes: u64,
    pub status: AiModelStatus,
    pub bytes_total: u64,
    pub bytes_downloaded: u64,
    pub error_message: Option<String>,
    pub updated_at_ms: i64,
}

/// Zero-downtime vision tier cutover, chat tier, and pause state (`ai_index_state` table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIndexStateView {
    pub active_vision_tier: AiTier,
    pub staging_vision_tier: Option<AiTier>,
    pub paused: bool,
    pub idle_unload_secs: u64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub active_chat_tier: ChatTier,
    #[serde(default)]
    pub vlm_rerank_enabled: bool,
}

/// Stage 2 background AI indexing queue counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiQueueStats {
    pub pending_assets: u64,
    pub indexed_assets: u64,
    pub staging_remaining: u64,
}

/// Full AI runtime, hardware, model catalog, and queue status returned by `GET /api/admin/ai/models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiModelsDashboardResponse {
    pub ort_available: bool,
    pub ort_ep: String,
    pub hardware: HardwareInfo,
    pub state: AiIndexStateView,
    pub models: Vec<AiModelView>,
    pub queue: AiQueueStats,
}

/// Request payload accepted by `POST /api/admin/ai/tier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetAiTierRequest {
    pub tier: AiTier,
}

/// Scope selector for `POST /api/admin/ai/reindex`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReindexScope {
    #[default]
    Missing,
    All,
}

impl ReindexScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::All => "all",
        }
    }
}

impl fmt::Display for ReindexScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReindexScope {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "missing" => Ok(Self::Missing),
            "all" => Ok(Self::All),
            other => Err(CoreError::InvalidEnum(format!("reindex_scope: {other}"))),
        }
    }
}

/// Request payload accepted by `POST /api/admin/ai/reindex`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReindexAiRequest {
    pub scope: ReindexScope,
}

/// Partial AI settings update accepted by `PATCH /api/admin/ai/settings`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateAiSettingsRequest {
    pub idle_unload_secs: Option<u64>,
    pub video_max_frames: Option<u32>,
}

/// Conversational query router tier (`ai_index_state.active_chat_tier`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatTier {
    #[default]
    Tier0,
    Tier1,
    Tier2,
    Tier3,
}

impl ChatTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tier0 => "tier0",
            Self::Tier1 => "tier1",
            Self::Tier2 => "tier2",
            Self::Tier3 => "tier3",
        }
    }

    /// Canonical GGUF model ID in the Phase 3 AI model manifest, or `None` for Classic `Tier0`.
    pub const fn chat_model_id(&self) -> Option<&'static str> {
        match self {
            Self::Tier0 => None,
            Self::Tier1 => Some("gemma4-e2b"),
            Self::Tier2 => Some("gemma4-e4b"),
            Self::Tier3 => Some("gemma4-26b-a4b"),
        }
    }
}

impl fmt::Display for ChatTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ChatTier {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tier0" => Ok(Self::Tier0),
            "tier1" => Ok(Self::Tier1),
            "tier2" => Ok(Self::Tier2),
            "tier3" => Ok(Self::Tier3),
            other => Err(CoreError::InvalidEnum(format!("chat_tier: {other}"))),
        }
    }
}

/// Request payload accepted by `POST /api/admin/ai/chat-tier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetChatTierRequest {
    pub tier: ChatTier,
    #[serde(default)]
    pub vlm_rerank_enabled: Option<bool>,
}

/// Structured conversational query router output & carried-forward filter state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterPlan {
    #[serde(default)]
    pub intent: Option<String>,
    pub semantic_query: Option<String>,
    pub ocr_terms: Option<String>,
    #[serde(default)]
    pub person_names: Vec<String>,
    #[serde(default)]
    pub exclude_person_names: Vec<String>,
    pub location_query: Option<String>,
    #[serde(default)]
    pub exclude_location_query: Option<String>,
    pub camera_model: Option<String>,
    #[serde(default)]
    pub lens_model: Option<String>,
    #[serde(default)]
    pub focal_length_min_mm: Option<f64>,
    #[serde(default)]
    pub focal_length_max_mm: Option<f64>,
    #[serde(default)]
    pub aperture_min_f: Option<f64>,
    #[serde(default)]
    pub aperture_max_f: Option<f64>,
    #[serde(default)]
    pub iso_min: Option<u32>,
    #[serde(default)]
    pub iso_max: Option<u32>,
    #[serde(default)]
    pub shutter_speed_min_sec: Option<f64>,
    #[serde(default)]
    pub shutter_speed_max_sec: Option<f64>,
    #[serde(default)]
    pub orientation: Option<String>,
    #[serde(default)]
    pub has_people: Option<bool>,
    #[serde(default)]
    pub min_faces: Option<u32>,
    #[serde(default)]
    pub max_faces: Option<u32>,
    #[serde(default)]
    pub sort_by: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub kind: Option<AssetKind>,
    #[serde(default)]
    pub exclude_kind: Option<AssetKind>,
    pub favorite: Option<bool>,
    pub date_from_ms: Option<i64>,
    pub date_to_ms: Option<i64>,
    #[serde(default)]
    pub clear_prior_filters: bool,
    pub assistant_reply: String,
    #[serde(default)]
    pub follow_up_suggestions: Vec<String>,
}

/// Author role of a persisted chat turn (`chat_messages.role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

impl ChatRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

impl fmt::Display for ChatRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ChatRole {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            other => Err(CoreError::InvalidEnum(format!("chat_role: {other}"))),
        }
    }
}

/// Compact persisted hit reference stored in `chat_messages.result_asset_ids`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatHitRef {
    pub asset_id: String,
    pub score: f32,
    pub timestamp_sec: Option<f64>,
}

/// Summary DTO for a conversational search session (`GET /api/chat/sessions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSessionSummary {
    pub id: String,
    pub title: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Detailed session DTO including carried-forward active filters (`GET /api/chat/sessions/{id}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatSessionDetail {
    pub id: String,
    pub title: String,
    pub active_filters: RouterPlan,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Hydrated chat message view returned in session details and message responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessageView {
    pub id: String,
    pub session_id: String,
    pub role: ChatRole,
    pub content: String,
    pub plan: Option<RouterPlan>,
    pub hits: Vec<SearchHit>,
    pub reranked: bool,
    pub planner_mode: Option<PlannerMode>,
    pub elapsed_ms: Option<u64>,
    pub created_at_ms: i64,
}

/// Request payload accepted by `POST /api/chat/sessions`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateChatSessionRequest {
    pub title: Option<String>,
}

/// Request payload accepted by `POST /api/chat/sessions/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendChatMessageRequest {
    pub content: String,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Client's local timezone offset from UTC in minutes (e.g. `-480` for PST, `+60` for CET)
    /// so relative date queries like "today" and "yesterday" align with local wall-clock time.
    #[serde(default)]
    pub tz_offset_minutes: Option<i32>,
}

/// Response returned by `POST /api/chat/sessions/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendChatMessageResponse {
    pub user_message: ChatMessageView,
    pub assistant_message: ChatMessageView,
    pub active_filters: RouterPlan,
    pub hits: Vec<SearchHit>,
    pub total_count: i64,
    pub planner_mode: PlannerMode,
    pub reranked: bool,
    pub elapsed_ms: u64,
}

/// Map basemap configuration and RBAC geotagged count returned by `GET /api/map/config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapConfigResponse {
    pub tile_mode: MapTileMode,
    pub available_pmtiles: Vec<String>,
    pub pmtiles_url: Option<String>,
    pub online_style_url: String,
    pub total_geotagged: u64,
}

/// Server-side spatial cluster cell returned by `GET /api/map/clusters`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapCluster {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    pub count: u64,
    pub cover_asset_id: String,
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
    pub label: Option<String>,
}

/// Response returned by `GET /api/map/clusters`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapClustersResponse {
    pub clusters: Vec<MapCluster>,
    pub total_in_view: u64,
}

/// Response returned by `GET /api/map/points`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapPointsResponse {
    pub items: Vec<AssetSummary>,
    pub total_count: u64,
}

/// Unified sleep, battery, and CPU thermal throttling state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThrottleStatus {
    pub active: bool,
    pub reason: Option<String>,
    pub cpu_usage_percent: f32,
    pub on_battery: bool,
}

/// Tailscale daemon, MagicDNS, HTTPS certificate, and `tailscale serve` status.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TailscaleStatus {
    pub installed: bool,
    pub running: bool,
    pub logged_in: bool,
    pub cli_path: Option<String>,
    pub magic_dns_name: Option<String>,
    pub https_certs_enabled: bool,
    pub serve_active: bool,
    pub https_url: Option<String>,
    pub remediation: Option<String>,
    pub last_error: Option<String>,
}

/// Persisted SQLite backup snapshot entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupSnapshotView {
    pub filename: String,
    pub path: String,
    pub size_bytes: u64,
    pub created_at_ms: i64,
}

/// Response returned by `GET /api/admin/backups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupListResponse {
    pub items: Vec<BackupSnapshotView>,
    pub backup_dir: String,
    pub interval_hours: u32,
    pub keep_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_enums_roundtrip() {
        assert_eq!(
            serde_json::to_string(&AssetKind::RawJpgStack).unwrap(),
            "\"raw_jpg_stack\""
        );
        assert_eq!(
            serde_json::to_string(&FileRole::MotionEmbedded).unwrap(),
            "\"motion_embedded\""
        );
        assert_eq!(
            serde_json::to_string(&ClientKind::TauriDesktop).unwrap(),
            "\"tauri_desktop\""
        );
        assert_eq!(
            serde_json::to_string(&JobStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(serde_json::to_string(&AiTier::Tier3).unwrap(), "\"tier3\"");

        assert_eq!(
            "live_photo".parse::<AssetKind>().unwrap(),
            AssetKind::LivePhoto
        );
        assert_eq!("raw".parse::<FileRole>().unwrap(), FileRole::Raw);
        assert_eq!(
            "tailscale".parse::<ClientKind>().unwrap(),
            ClientKind::Tailscale
        );
        assert_eq!(
            "completed".parse::<JobStatus>().unwrap(),
            JobStatus::Completed
        );
        assert_eq!("tier1".parse::<AiTier>().unwrap(), AiTier::Tier1);

        assert_eq!(serde_json::to_string(&TagSource::Ai).unwrap(), "\"ai\"");
        assert_eq!(
            serde_json::to_string(&TagState::Rejected).unwrap(),
            "\"rejected\""
        );
        assert_eq!(
            serde_json::to_string(&PlaybackMode::Hls).unwrap(),
            "\"hls\""
        );
        assert_eq!(
            serde_json::to_string(&AlbumSortOrder::CaptureDesc).unwrap(),
            "\"capture_desc\""
        );
        assert_eq!(
            serde_json::to_string(&AlbumSharePermission::Contribute).unwrap(),
            "\"contribute\""
        );
        assert_eq!(
            serde_json::to_string(&AlbumPermission::Owner).unwrap(),
            "\"owner\""
        );
        assert_eq!(
            serde_json::to_string(&CacheEntryKind::Preview2048).unwrap(),
            "\"preview_2048\""
        );
        assert_eq!(
            "preview_2048".parse::<CacheEntryKind>().unwrap(),
            CacheEntryKind::Preview2048
        );
        assert_eq!(
            "motion_video".parse::<CacheEntryKind>().unwrap(),
            CacheEntryKind::MotionVideo
        );
        assert_eq!("user".parse::<TagSource>().unwrap(), TagSource::User);
        assert_eq!("active".parse::<TagState>().unwrap(), TagState::Active);
        assert_eq!(
            "direct".parse::<PlaybackMode>().unwrap(),
            PlaybackMode::Direct
        );
        assert_eq!(
            "added_desc".parse::<AlbumSortOrder>().unwrap(),
            AlbumSortOrder::AddedDesc
        );
        assert_eq!(
            "view".parse::<AlbumSharePermission>().unwrap(),
            AlbumSharePermission::View
        );
        assert_eq!(
            "admin".parse::<AlbumPermission>().unwrap(),
            AlbumPermission::Admin
        );
        assert!(AlbumPermission::Owner.can_manage());
        assert!(AlbumPermission::Admin.can_manage());
        assert!(!AlbumPermission::Contribute.can_manage());
        assert!(AlbumPermission::Contribute.can_contribute());
        assert!(!AlbumPermission::View.can_contribute());
    }

    #[test]
    fn histogram_encode_decode_roundtrips_and_rejects_malformed_slices() {
        let bins: Vec<u16> = (0..HISTOGRAM_BINS).map(|i| (i as u16) * 256).collect();
        let encoded = Histogram256::encode_bins_le(&bins);
        assert_eq!(encoded.len(), 512);
        let decoded = Histogram256::decode_bins_le(&encoded).expect("valid 512-byte blob");
        assert_eq!(decoded, bins);

        assert!(Histogram256::decode_bins_le(&[]).is_none());
        assert!(Histogram256::decode_bins_le(&encoded[..510]).is_none());
        assert!(Histogram256::decode_bins_le(&encoded[..511]).is_none());

        let mut too_long = encoded.clone();
        too_long.extend_from_slice(&[0, 0]);
        assert!(Histogram256::decode_bins_le(&too_long).is_none());
    }

    #[test]
    fn phase1_dtos_serde_roundtrip() {
        let patch: UpdateOverridesRequest =
            serde_json::from_str(r#"{"title":"Kyoto Trip","rating":5}"#).unwrap();
        assert_eq!(patch.title.as_deref(), Some("Kyoto Trip"));
        assert_eq!(patch.rating, Some(5));
        assert!(patch.description.is_none());
        assert!(patch.gps_lat.is_none());

        let detail = AssetDetail {
            summary: AssetSummary {
                id: "asset-1".to_string(),
                folder_id: "folder-1".to_string(),
                kind: AssetKind::LivePhoto,
                capture_at_ms: 1_700_000_000_000,
                width: Some(4032),
                height: Some(3024),
                duration_ms: Some(2500),
                aspect_ratio: Some(4.0 / 3.0),
                is_favorite: true,
                deleted_at_ms: None,
                thumb_rel_path: Some("as/se/asset-1.webp".to_string()),
                thumb_ready: true,
                title: Some("Kyoto Trip".to_string()),
                rating: Some(5),
                gps_lat: Some(35.0116),
                gps_lon: Some(135.7681),
                geo_city: Some("Kyoto".to_string()),
                geo_country: Some("Japan".to_string()),
                camera_model: Some("iPhone 16 Pro".to_string()),
            },
            files: vec![AssetFileView {
                id: "file-1".to_string(),
                asset_id: "asset-1".to_string(),
                folder_id: "folder-1".to_string(),
                rel_path: "IMG_1024.HEIC".to_string(),
                file_role: FileRole::Primary,
                mime_type: "image/heic".to_string(),
                extension: "heic".to_string(),
                file_size: 2_345_678,
                mtime_ms: 1_700_000_000_000,
                partial_blake3: "deadbeef".to_string(),
                content_identifier: Some("CID-123".to_string()),
                embedded_video_offset: None,
                embedded_video_length: None,
                is_missing: false,
            }],
            photo_meta: Some(PhotoMetaView {
                camera_make: Some("Apple".to_string()),
                camera_model: Some("iPhone 16 Pro".to_string()),
                iso: Some(100),
                ..Default::default()
            }),
            video_meta: None,
            overrides: Some(UserOverridesView {
                title: Some("Kyoto Trip".to_string()),
                rating: Some(5),
                updated_by: Some("user-1".to_string()),
                updated_at_ms: 1_700_000_100_000,
                ..Default::default()
            }),
            tags: vec![MediaTagView {
                tag: "temple".to_string(),
                source: TagSource::User,
                state: TagState::Active,
                confidence: None,
            }],
            histogram: Some(Histogram256 {
                r: vec![10; HISTOGRAM_BINS],
                g: vec![20; HISTOGRAM_BINS],
                b: vec![30; HISTOGRAM_BINS],
                luma: vec![25; HISTOGRAM_BINS],
            }),
            album_ids: vec!["album-1".to_string()],
            playback: PlaybackInfo {
                mode: PlaybackMode::Image,
                direct_url: None,
                hls_url: None,
                live_video_url: Some("/api/media/asset-1/live-video".to_string()),
                has_raw: false,
                preview_url: "/api/media/asset-1/preview".to_string(),
                thumb_url: "/api/media/asset-1/thumb".to_string(),
                original_url: "/api/media/asset-1/original".to_string(),
            },
        };

        let json = serde_json::to_string(&detail).unwrap();
        let parsed: AssetDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, detail);
    }

    #[test]
    fn phase2_ai_tier_and_dtos_roundtrip() {
        assert_eq!(AiTier::Tier1.dim(), 768);
        assert_eq!(AiTier::Tier2.dim(), 1024);
        assert_eq!(AiTier::Tier3.dim(), 1152);
        assert_eq!(AiTier::Tier1.vision_model_id(), "siglip2-base");
        assert_eq!(AiTier::Tier2.vision_model_id(), "siglip2-large");
        assert_eq!(AiTier::Tier3.vision_model_id(), "siglip2-so400m");

        assert_eq!(
            serde_json::to_string(&SearchMode::Hybrid).unwrap(),
            "\"hybrid\""
        );
        assert_eq!(
            serde_json::to_string(&PlannerMode::ExactSimd).unwrap(),
            "\"exact_simd\""
        );
        assert_eq!(
            serde_json::to_string(&AiModelStatus::NotInstalled).unwrap(),
            "\"not_installed\""
        );
        assert_eq!(
            serde_json::to_string(&ReindexScope::Missing).unwrap(),
            "\"missing\""
        );
        assert_eq!(
            "installed".parse::<AiModelStatus>().unwrap(),
            AiModelStatus::Installed
        );
        assert_eq!("all".parse::<ReindexScope>().unwrap(), ReindexScope::All);

        let req: SearchRequest = serde_json::from_str(r#"{"query":"sunset"}"#).unwrap();
        assert_eq!(req.mode, SearchMode::Hybrid);
        assert_eq!(req.query.as_deref(), Some("sunset"));

        let reindex: ReindexAiRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(reindex.scope, ReindexScope::Missing);

        let update_person: UpdatePersonRequest =
            serde_json::from_str(r#"{"name":"Alice","is_pinned":true}"#).unwrap();
        assert_eq!(update_person.name.as_deref(), Some("Alice"));
        assert_eq!(update_person.is_pinned, Some(true));
        assert!(update_person.is_hidden.is_none());

        let dashboard = AiModelsDashboardResponse {
            ort_available: true,
            ort_ep: "CoreML".to_string(),
            hardware: HardwareInfo {
                total_ram_bytes: 36 * 1024 * 1024 * 1024,
                cpu_cores: 12,
                recommended_vision_tier: AiTier::Tier3,
                recommended_chat_tier: AiTier::Tier3,
            },
            state: AiIndexStateView {
                active_vision_tier: AiTier::Tier2,
                staging_vision_tier: Some(AiTier::Tier3),
                paused: false,
                idle_unload_secs: 300,
                updated_at_ms: 1_700_000_000_000,
                active_chat_tier: ChatTier::Tier0,
                vlm_rerank_enabled: false,
            },
            models: vec![AiModelView {
                id: "siglip2-large".to_string(),
                name: "SigLIP2 Large".to_string(),
                role: "vision_tier2".to_string(),
                license: "Apache-2.0".to_string(),
                hf_repo: "onnx-community/siglip2-large-patch16-256-ONNX".to_string(),
                tier: Some(AiTier::Tier2),
                approx_bytes: 1_200_000_000,
                status: AiModelStatus::Installed,
                bytes_total: 1_200_000_000,
                bytes_downloaded: 1_200_000_000,
                error_message: None,
                updated_at_ms: 1_700_000_000_000,
            }],
            queue: AiQueueStats {
                pending_assets: 10,
                indexed_assets: 90,
                staging_remaining: 5,
            },
        };
        let encoded = serde_json::to_string(&dashboard).unwrap();
        let decoded: AiModelsDashboardResponse = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, dashboard);
    }

    #[test]
    fn phase3_chat_and_map_dtos_roundtrip() {
        assert_eq!(ChatTier::default(), ChatTier::Tier0);
        assert_eq!(ChatTier::Tier0.as_str(), "tier0");
        assert_eq!(ChatTier::Tier1.as_str(), "tier1");
        assert_eq!(ChatTier::Tier2.as_str(), "tier2");
        assert_eq!(ChatTier::Tier3.as_str(), "tier3");
        assert_eq!(ChatTier::Tier0.chat_model_id(), None);
        assert_eq!(ChatTier::Tier1.chat_model_id(), Some("gemma4-e2b"));
        assert_eq!(ChatTier::Tier2.chat_model_id(), Some("gemma4-e4b"));
        assert_eq!(ChatTier::Tier3.chat_model_id(), Some("gemma4-26b-a4b"));
        assert_eq!(
            serde_json::to_string(&ChatTier::Tier2).unwrap(),
            "\"tier2\""
        );
        assert_eq!("tier3".parse::<ChatTier>().unwrap(), ChatTier::Tier3);
        assert!("tier9".parse::<ChatTier>().is_err());

        assert_eq!(serde_json::to_string(&ChatRole::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&ChatRole::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!("user".parse::<ChatRole>().unwrap(), ChatRole::User);
        assert_eq!(
            "assistant".parse::<ChatRole>().unwrap(),
            ChatRole::Assistant
        );
        assert!("system".parse::<ChatRole>().is_err());

        // Empty JSON object deserializes into RouterPlan::default() for active_filters_json = '{}'
        let empty_plan: RouterPlan = serde_json::from_str("{}").unwrap();
        assert_eq!(empty_plan, RouterPlan::default());
        assert!(!empty_plan.clear_prior_filters);
        assert!(empty_plan.person_names.is_empty());

        let plan = RouterPlan {
            semantic_query: Some("beach sunset".to_string()),
            ocr_terms: Some("cafe".to_string()),
            person_names: vec!["Mom".to_string()],
            location_query: Some("Tokyo".to_string()),
            camera_model: Some("Fujifilm X100VI".to_string()),
            tags: vec!["travel".to_string()],
            kind: Some(AssetKind::Video),
            favorite: Some(true),
            date_from_ms: Some(1_700_000_000_000),
            date_to_ms: Some(1_705_000_000_000),
            clear_prior_filters: true,
            assistant_reply: "Found 2 videos from Tokyo.".to_string(),
            follow_up_suggestions: vec!["Favorites only".to_string()],
            ..Default::default()
        };
        let plan_json = serde_json::to_string(&plan).unwrap();
        let parsed_plan: RouterPlan = serde_json::from_str(&plan_json).unwrap();
        assert_eq!(parsed_plan, plan);

        let tier_req: SetChatTierRequest =
            serde_json::from_str(r#"{"tier":"tier3","vlm_rerank_enabled":true}"#).unwrap();
        assert_eq!(tier_req.tier, ChatTier::Tier3);
        assert_eq!(tier_req.vlm_rerank_enabled, Some(true));

        let chat_req: SendChatMessageRequest =
            serde_json::from_str(r#"{"content":"only videos"}"#).unwrap();
        assert_eq!(chat_req.content, "only videos");
        assert!(chat_req.limit.is_none());

        let sample_asset = AssetSummary {
            id: "asset-geo-1".to_string(),
            folder_id: "folder-1".to_string(),
            kind: AssetKind::Photo,
            capture_at_ms: 1_700_000_000_000,
            width: Some(4000),
            height: Some(3000),
            duration_ms: None,
            aspect_ratio: Some(4.0 / 3.0),
            is_favorite: true,
            deleted_at_ms: None,
            thumb_rel_path: Some("as/ge/asset-geo-1.webp".to_string()),
            thumb_ready: true,
            title: None,
            rating: None,
            gps_lat: Some(35.6762),
            gps_lon: Some(139.6503),
            geo_city: Some("Tokyo".to_string()),
            geo_country: Some("Japan".to_string()),
            camera_model: None,
        };

        let sample_hit = SearchHit {
            asset: sample_asset.clone(),
            score: 0.92,
            timestamp_sec: Some(12.5),
            match_reasons: vec!["semantic".to_string()],
            ocr_snippet: None,
        };

        let msg_resp = SendChatMessageResponse {
            user_message: ChatMessageView {
                id: "m1".to_string(),
                session_id: "s1".to_string(),
                role: ChatRole::User,
                content: "beach in Tokyo".to_string(),
                plan: None,
                hits: vec![],
                reranked: false,
                planner_mode: None,
                elapsed_ms: None,
                created_at_ms: 1_700_000_000_000,
            },
            assistant_message: ChatMessageView {
                id: "m2".to_string(),
                session_id: "s1".to_string(),
                role: ChatRole::Assistant,
                content: "Found 1 match.".to_string(),
                plan: Some(plan.clone()),
                hits: vec![sample_hit.clone()],
                reranked: true,
                planner_mode: Some(PlannerMode::ExactSimd),
                elapsed_ms: Some(42),
                created_at_ms: 1_700_000_000_050,
            },
            active_filters: plan,
            hits: vec![sample_hit],
            total_count: 1,
            planner_mode: PlannerMode::ExactSimd,
            reranked: true,
            elapsed_ms: 42,
        };
        let msg_json = serde_json::to_string(&msg_resp).unwrap();
        let parsed_msg: SendChatMessageResponse = serde_json::from_str(&msg_json).unwrap();
        assert_eq!(parsed_msg, msg_resp);

        let hit_ref = ChatHitRef {
            asset_id: "asset-geo-1".to_string(),
            score: 0.92,
            timestamp_sec: Some(12.5),
        };
        assert_eq!(
            serde_json::from_str::<ChatHitRef>(&serde_json::to_string(&hit_ref).unwrap()).unwrap(),
            hit_ref
        );

        let map_cfg = MapConfigResponse {
            tile_mode: MapTileMode::OfflinePmtiles,
            available_pmtiles: vec!["world-z0-z6.pmtiles".to_string()],
            pmtiles_url: Some("/api/map/tiles/world-z0-z6.pmtiles".to_string()),
            online_style_url: "https://tiles.openfreemap.org/styles/dark".to_string(),
            total_geotagged: 42,
        };
        assert_eq!(
            serde_json::from_str::<MapConfigResponse>(&serde_json::to_string(&map_cfg).unwrap())
                .unwrap(),
            map_cfg
        );

        let clusters = MapClustersResponse {
            clusters: vec![MapCluster {
                id: "c1".to_string(),
                lat: 35.6762,
                lon: 139.6503,
                count: 14,
                cover_asset_id: "asset-geo-1".to_string(),
                min_lat: 35.6,
                max_lat: 35.7,
                min_lon: 139.6,
                max_lon: 139.7,
                label: Some("Tokyo".to_string()),
            }],
            total_in_view: 14,
        };
        assert_eq!(
            serde_json::from_str::<MapClustersResponse>(&serde_json::to_string(&clusters).unwrap())
                .unwrap(),
            clusters
        );

        let points = MapPointsResponse {
            items: vec![sample_asset],
            total_count: 1,
        };
        assert_eq!(
            serde_json::from_str::<MapPointsResponse>(&serde_json::to_string(&points).unwrap())
                .unwrap(),
            points
        );
    }

    #[test]
    fn phase4_throttle_tailscale_backup_dtos_roundtrip() {
        let default_throttle = ThrottleStatus::default();
        assert!(!default_throttle.active);
        assert!(default_throttle.reason.is_none());
        assert_eq!(default_throttle.cpu_usage_percent, 0.0);
        assert!(!default_throttle.on_battery);

        let throttle = ThrottleStatus {
            active: true,
            reason: Some("high_cpu".to_string()),
            cpu_usage_percent: 87.5,
            on_battery: true,
        };
        assert_eq!(
            serde_json::from_str::<ThrottleStatus>(&serde_json::to_string(&throttle).unwrap())
                .unwrap(),
            throttle
        );

        let default_ts = TailscaleStatus::default();
        assert!(!default_ts.installed);
        assert!(!default_ts.running);
        assert!(!default_ts.logged_in);
        assert!(default_ts.cli_path.is_none());
        assert!(default_ts.magic_dns_name.is_none());
        assert!(!default_ts.https_certs_enabled);
        assert!(!default_ts.serve_active);
        assert!(default_ts.https_url.is_none());
        assert!(default_ts.remediation.is_none());
        assert!(default_ts.last_error.is_none());

        let ts = TailscaleStatus {
            installed: true,
            running: true,
            logged_in: true,
            cli_path: Some("/Applications/Tailscale.app/Contents/MacOS/Tailscale".to_string()),
            magic_dns_name: Some("truongphoto.tailnet.ts.net".to_string()),
            https_certs_enabled: true,
            serve_active: true,
            https_url: Some("https://truongphoto.tailnet.ts.net".to_string()),
            remediation: None,
            last_error: None,
        };
        assert_eq!(
            serde_json::from_str::<TailscaleStatus>(&serde_json::to_string(&ts).unwrap()).unwrap(),
            ts
        );

        let snapshot = BackupSnapshotView {
            filename: "library-20261120T100000Z.db".to_string(),
            path: "/data/backups/library-20261120T100000Z.db".to_string(),
            size_bytes: 1_048_576,
            created_at_ms: 1_700_000_000_000,
        };
        assert_eq!(
            serde_json::from_str::<BackupSnapshotView>(&serde_json::to_string(&snapshot).unwrap())
                .unwrap(),
            snapshot
        );

        let backup_list = BackupListResponse {
            items: vec![snapshot],
            backup_dir: "/data/backups".to_string(),
            interval_hours: 24,
            keep_count: 7,
        };
        assert_eq!(
            serde_json::from_str::<BackupListResponse>(
                &serde_json::to_string(&backup_list).unwrap()
            )
            .unwrap(),
            backup_list
        );
    }
}
