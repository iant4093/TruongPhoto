//! Shared domain contracts, configuration, RBAC primitives, errors,
//! partial BLAKE3 fingerprinting, and WebSocket event schemas for TruongPhoto.

/// Compile-time application version (honors `TP_BUILD_VERSION` set by release CI,
/// falling back to `CARGO_PKG_VERSION`).
pub const APP_VERSION: &str = match option_env!("TP_BUILD_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub mod config;
pub mod error;
pub mod fingerprint;
pub mod models;
pub mod process;
pub mod rbac;
pub mod ws;

pub use config::{
    AppSettings, DefaultPathsDto, MapTileMode, PartialAppSettings, ServerConfig, StoragePaths,
};
pub use error::{ApiErrorBody, ApiErrorCode, ApiErrorEnvelope, CoreError};
pub use fingerprint::{partial_blake3_path, partial_blake3_reader, PARTIAL_BLAKE3_CHUNK_BYTES};
pub use models::{
    AdminUserView, AiIndexStateView, AiModelStatus, AiModelView, AiModelsDashboardResponse,
    AiQueueStats, AiTier, AlbumDetail, AlbumPermission, AlbumSharePermission, AlbumShareView,
    AlbumSortOrder, AlbumSummary, AssetDetail, AssetFileView, AssetKind, AssetSummary,
    BackupListResponse, BackupSnapshotView, CacheEntryKind, ChatHitRef, ChatMessageView, ChatRole,
    ChatSessionDetail, ChatSessionSummary, ChatTier, ClientKind, ClusterFacesResponse,
    CreateChatSessionRequest, DesktopBootstrapInfo, FaceView, FileRole, FolderRecord, FolderView,
    HardwareInfo, Histogram256, JobRecord, JobStatus, LibraryStats, LruCacheEntry, MapCluster,
    MapClustersResponse, MapConfigResponse, MapPointsResponse, MediaTagView, MergePeopleRequest,
    MergePeopleResponse, PersonSummary, PhotoMetaView, PlannerMode, PlaybackInfo, PlaybackMode,
    PowerState, ProfileSummary, PurgeReport, ReassignFaceRequest, ReassignFaceResponse,
    ReindexAiRequest, ReindexScope, RouterPlan, SearchHit, SearchMode, SearchRequest,
    SearchResponse, SendChatMessageRequest, SendChatMessageResponse, SessionRecord,
    SetAiTierRequest, SetChatTierRequest, ShareableUser, TagSource, TagState, TailscaleStatus,
    ThrottleStatus, TimelineBucket, UpdateAiSettingsRequest, UpdateFeedStatus,
    UpdateOverridesRequest, UpdatePersonRequest, UploadItemError, UploadResponse,
    UserOverridesView, UserPublic, UserRecord, VideoMetaView,
};
pub use rbac::{RbacScope, Role};
pub use ws::{WsClientMessage, WsServerEvent};
