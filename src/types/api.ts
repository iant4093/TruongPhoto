// Strict TypeScript interfaces matching truongphoto-core Rust DTOs & Phase 0 API contracts.

export type Role = 'admin' | 'contributor' | 'viewer';

export type ClientKind = 'web' | 'tauri_desktop' | 'tailscale';

export type AssetKind = 'photo' | 'video' | 'live_photo' | 'motion_photo' | 'raw_jpg_stack';

export type FileRole = 'primary' | 'raw' | 'jpeg_companion' | 'live_video' | 'motion_embedded';

export type JobStatus = 'queued' | 'running' | 'completed' | 'failed' | 'cancelled';

export type AiTier = 'tier1' | 'tier2' | 'tier3';

export type ChatTier = 'tier0' | 'tier1' | 'tier2' | 'tier3';

export type MapTileMode = 'offline_pmtiles' | 'online_openfreemap';

export type NavTab =
  | 'timeline'
  | 'chat'
  | 'people'
  | 'albums'
  | 'favorites'
  | 'map'
  | 'trash'
  | 'settings';

export type ApiErrorCode =
  | 'UNAUTHORIZED'
  | 'FORBIDDEN'
  | 'DESKTOP_PIN_LOCKED'
  | 'NOT_FOUND'
  | 'ALREADY_INITIALIZED'
  | 'NOT_INITIALIZED'
  | 'CONFLICT'
  | 'VALIDATION_ERROR'
  | 'RATE_LIMITED'
  | 'INTERNAL_ERROR';

export interface ApiErrorBody {
  code: ApiErrorCode;
  message: string;
  details: Record<string, unknown> | null;
}

export interface ApiErrorEnvelope {
  error: ApiErrorBody;
}

export interface HardwareInfo {
  total_ram_bytes: number;
  cpu_cores: number;
  recommended_vision_tier: AiTier;
  recommended_chat_tier: AiTier;
}

export interface PowerState {
  sleeping: boolean;
  display_asleep: boolean;
  session_locked: boolean;
  on_battery: boolean;
}

export interface LibraryStats {
  asset_count: number;
  folder_count: number;
  db_size_bytes: number;
}

export interface DefaultPathsDto {
  managed_uploads_dir: string;
  thumbnails_dir: string;
  cache_dir: string;
  backup_dir: string;
}

export interface UserPublic {
  id: string;
  username: string;
  display_name: string;
  role: Role;
  avatar_color: string;
}

export interface ProfileSummary {
  id: string;
  display_name: string;
  role: Role;
  avatar_color: string;
}

export interface AdminUserView {
  id: string;
  username: string;
  display_name: string;
  role: Role;
  avatar_color: string;
  disabled_at_ms: number | null;
  allowed_folder_ids: string[];
  created_at_ms: number;
}

export interface FolderView {
  id: string;
  name: string;
  root_path: string;
  volume_uuid: string | null;
  volume_label: string | null;
  is_managed_uploads: boolean;
  allow_disk_delete: boolean;
  is_offline: boolean;
  last_scanned_at_ms: number | null;
  asset_count: number;
  created_at_ms: number;
}

export interface AssetSummary {
  id: string;
  folder_id: string;
  kind: AssetKind;
  capture_at_ms: number;
  width: number | null;
  height: number | null;
  duration_ms: number | null;
  aspect_ratio: number | null;
  is_favorite: boolean;
  deleted_at_ms: number | null;
  thumb_rel_path: string | null;
  thumb_ready: boolean;
  title: string | null;
  rating: number | null;
  gps_lat: number | null;
  gps_lon: number | null;
  geo_city: string | null;
  geo_country: string | null;
  camera_model: string | null;
}

export interface TimelineBucket {
  year: number;
  month: number;
  count: number;
}

export interface AlbumSummary {
  id: string;
  owner_user_id: string;
  title: string;
  description: string | null;
  cover_asset_id: string | null;
  sort_order: string;
  asset_count: number;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface PersonSummary {
  id: string;
  name: string | null;
  is_hidden: boolean;
  is_pinned: boolean;
  cover_face_id: string | null;
  face_count: number;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface AppSettings {
  port: number;
  mdns_enabled: boolean;
  mdns_hostname: string;
  desktop_pin_lock: boolean;
  managed_uploads_dir: string;
  thumbnails_dir: string;
  cache_dir: string;
  cache_limit_gb: number;
  backup_dir: string;
  backup_keep_count: number;
  backup_interval_hours?: number;
  video_max_frames: number;
  ai_tier_vision: AiTier;
  ai_tier_chat: AiTier;
  map_tile_mode?: MapTileMode;
  map_pmtiles_dir?: string;
  pause_on_battery?: boolean;
  cpu_throttle_percent?: number;
}

export type PartialAppSettings = Partial<AppSettings>;

export interface BackupResult {
  path: string;
  size_bytes: number;
  created_at_ms: number;
  pruned_files: number;
}

export interface HealthResponse {
  status: string;
  version: string;
  uptime_secs: number;
  initialized: boolean;
}

export interface SystemInfoResponse {
  version: string;
  platform: string;
  arch: string;
  hostname: string;
  port: number;
  lan_urls: string[];
  mdns_enabled: boolean;
  mdns_active: boolean;
  mdns_url: string | null;
  desktop_pin_lock: boolean;
  hardware: HardwareInfo;
  stats: LibraryStats;
  power: PowerState;
  last_backup?: BackupSnapshotView | null;
  throttle?: ThrottleStatus;
  tailscale_url?: string | null;
}

export interface SetupStatusResponse {
  initialized: boolean;
  is_direct_loopback: boolean;
  hardware: HardwareInfo;
  default_paths: DefaultPathsDto;
}

export interface SetupInitializeRequest {
  admin_username: string;
  admin_display_name: string;
  admin_profile_pin: string;
  server_pin: string;
  managed_uploads_dir: string;
  thumbnails_dir: string;
  cache_dir: string;
  backup_dir: string;
  initial_folder_path: string | null;
  initial_folder_name: string | null;
}

export interface AuthStatusResponse {
  initialized: boolean;
  authenticated: boolean;
  server_gate_passed: boolean;
  desktop_pin_lock: boolean;
  user: UserPublic | null;
  profiles: ProfileSummary[] | null;
}

export interface AuthLoginResponse {
  user: UserPublic;
  session_expires_at_ms: number;
}

export interface ServerPinResponse {
  profiles: ProfileSummary[];
}

export interface MeResponse {
  user: UserPublic;
  allowed_folder_ids: string[] | null;
}

export interface DesktopBootstrapInfo {
  api_base_url: string;
  desktop_bootstrap_token: string;
  port: number;
  mdns_url: string | null;
  version: string;
  platform: string;
}

export interface UpdateFeedStatus {
  available: boolean;
  version: string | null;
  notes: string | null;
}

// Request & list wrapper DTOs for REST endpoints
export interface LoginRequest {
  user_id: string;
  profile_pin: string;
  server_pin?: string | null;
}

export interface CreateFolderRequest {
  name: string;
  root_path: string;
  allow_disk_delete?: boolean;
  is_managed_uploads?: boolean;
}

export interface UpdateFolderRequest {
  name?: string;
  allow_disk_delete?: boolean;
  is_managed_uploads?: boolean;
}

export interface CreateAdminUserRequest {
  username: string;
  display_name: string;
  role: Role;
  profile_pin: string;
  avatar_color?: string;
  allowed_folder_ids?: string[];
}

export interface UpdateAdminUserRequest {
  display_name?: string;
  role?: Role;
  profile_pin?: string;
  avatar_color?: string;
  disabled?: boolean;
  allowed_folder_ids?: string[];
}

export interface UpdateServerPinRequest {
  current_server_pin: string;
  new_server_pin: string;
}

export interface ListAssetsParams {
  folder_id?: string;
  favorite?: boolean;
  before_ms?: number;
  cursor?: string;
  limit?: number;
}

export interface AssetListResponse {
  items: AssetSummary[];
  next_cursor: string | null;
  total_count: number;
}

export interface AssetBucketsResponse {
  buckets: TimelineBucket[];
}

export interface AlbumListResponse {
  items: AlbumSummary[];
}

export interface PersonListResponse {
  items: PersonSummary[];
}

export interface TrashListResponse {
  items: AssetSummary[];
  total_count: number;
}

// WebSocket discriminated unions matching crates/truongphoto-core/src/ws.rs
export type WsServerEvent =
  | {
      type: 'connected';
      server_version: string;
      user_id: string;
      role: Role;
    }
  | {
      type: 'heartbeat';
      uptime_secs: number;
      process_ram_bytes: number;
      db_size_bytes: number;
      active_jobs: number;
    }
  | {
      type: 'job_progress';
      job_id: number;
      kind: string;
      status: JobStatus;
      processed: number;
      total: number | null;
      message: string | null;
    }
  | {
      type: 'folder_changed';
      folder_id: string;
      action: string;
    }
  | {
      type: 'power_state_changed';
      power: PowerState;
    }
  | {
      type: 'throttle_state_changed';
      throttle: ThrottleStatus;
    }
  | {
      type: 'tailscale_status_changed';
      status: TailscaleStatus;
    }
  | {
      type: 'settings_changed';
      settings: AppSettings;
    }
  | {
      type: 'ai_state_changed';
      active_vision_tier: AiTier;
      staging_vision_tier: AiTier | null;
      paused: boolean;
    }
  | {
      type: 'pong';
    };

export type WsClientMessage = {
  type: 'ping';
};

// Phase 1 DTOs matching crates/truongphoto-core/src/models.rs & REST API contracts

export interface AssetFileView {
  id: string;
  asset_id: string;
  folder_id: string;
  rel_path: string;
  file_role: FileRole;
  mime_type: string;
  extension: string;
  file_size: number;
  mtime_ms: number;
  partial_blake3: string;
  content_identifier: string | null;
  embedded_video_offset: number | null;
  embedded_video_length: number | null;
  is_missing: boolean;
}

export interface PhotoMetaView {
  camera_make: string | null;
  camera_model: string | null;
  lens_model: string | null;
  focal_length_mm: number | null;
  focal_length_35mm: number | null;
  aperture_f: number | null;
  shutter_speed_sec: number | null;
  iso: number | null;
  orientation: number | null;
  color_space: string | null;
  gps_lat: number | null;
  gps_lon: number | null;
  gps_alt_m: number | null;
  geo_city: string | null;
  geo_admin1: string | null;
  geo_country: string | null;
  geo_country_code: string | null;
  taken_at_ms: number | null;
}

export interface VideoMetaView {
  duration_ms: number;
  video_codec: string | null;
  audio_codec: string | null;
  fps: number | null;
  bitrate_bps: number | null;
  bit_depth: number | null;
  color_transfer: string | null;
  color_primaries: string | null;
  is_hdr: boolean;
  rotation: number;
  gps_lat: number | null;
  gps_lon: number | null;
  geo_city: string | null;
  geo_admin1: string | null;
  geo_country: string | null;
  geo_country_code: string | null;
  taken_at_ms: number | null;
}

export interface UserOverridesView {
  title: string | null;
  description: string | null;
  capture_at_ms: number | null;
  gps_lat: number | null;
  gps_lon: number | null;
  geo_city: string | null;
  geo_admin1: string | null;
  geo_country: string | null;
  geo_country_code: string | null;
  rating: number | null;
  updated_by: string | null;
  updated_at_ms: number;
}

export interface UpdateOverridesRequest {
  title?: string | null;
  description?: string | null;
  capture_at_ms?: number | null;
  gps_lat?: number | null;
  gps_lon?: number | null;
  geo_city?: string | null;
  geo_admin1?: string | null;
  geo_country?: string | null;
  geo_country_code?: string | null;
  rating?: number | null;
}

export type TagSource = 'ai' | 'user';

export type TagState = 'active' | 'rejected';

export interface MediaTagView {
  tag: string;
  source: TagSource;
  state: TagState;
  confidence: number | null;
}

export interface Histogram256 {
  r: number[];
  g: number[];
  b: number[];
  luma: number[];
}

export type PlaybackMode = 'image' | 'direct' | 'hls';

export interface PlaybackInfo {
  mode: PlaybackMode;
  direct_url: string | null;
  hls_url: string | null;
  live_video_url: string | null;
  has_raw: boolean;
  preview_url: string;
  thumb_url: string;
  original_url: string;
}

export interface AssetDetail {
  summary: AssetSummary;
  files: AssetFileView[];
  photo_meta: PhotoMetaView | null;
  video_meta: VideoMetaView | null;
  overrides: UserOverridesView | null;
  tags: MediaTagView[];
  histogram: Histogram256 | null;
  album_ids: string[];
  playback: PlaybackInfo;
}

export interface PurgeReport {
  purged_count: number;
  disk_deleted_files: number;
  db_only_assets: number;
  partial_errors: string[];
}

export interface UploadItemError {
  filename: string;
  message: string;
}

export interface UploadResponse {
  uploaded: AssetSummary[];
  errors: UploadItemError[];
}

export type AlbumSortOrder = 'capture_desc' | 'capture_asc' | 'added_desc' | 'custom';

export type AlbumSharePermission = 'view' | 'contribute';

export type AlbumPermission = 'owner' | 'admin' | 'contribute' | 'view';

export interface AlbumShareView {
  user_id: string;
  display_name: string;
  avatar_color: string;
  permission: AlbumSharePermission;
}

export interface AlbumDetail {
  album: AlbumSummary;
  permission: AlbumPermission;
  assets: AssetSummary[];
  shares: AlbumShareView[];
}

export interface ShareableUser {
  id: string;
  display_name: string;
  avatar_color: string;
}

export interface ScanFolderResponse {
  job_id: number;
  folder_id: string;
}

export interface ScanAllResponse {
  job_ids: number[];
}

export interface ActiveJobInfo {
  job_id: number;
  kind: string;
  status: JobStatus;
  processed: number;
  total: number | null;
  message: string | null;
  updated_at_ms: number;
}

export interface CreateAlbumRequest {
  title: string;
  description?: string | null;
  asset_ids?: string[];
}

export interface UpdateAlbumRequest {
  title?: string;
  description?: string | null;
  cover_asset_id?: string | null;
  sort_order?: AlbumSortOrder | string;
}

export interface AlbumShareInput {
  user_id: string;
  permission: AlbumSharePermission;
}

// Phase 2 DTOs matching crates/truongphoto-core/src/models.rs & REST API contracts

export type SearchMode = 'hybrid' | 'semantic' | 'ocr' | 'filters';

export type PlannerMode = 'exact_simd' | 'hnsw' | 'sql_only';

export interface SearchRequest {
  query?: string | null;
  mode?: SearchMode;
  folder_id?: string | null;
  person_ids?: string[] | null;
  tags?: string[] | null;
  camera_model?: string | null;
  lens_model?: string | null;
  focal_length_min_mm?: number | null;
  focal_length_max_mm?: number | null;
  aperture_min_f?: number | null;
  aperture_max_f?: number | null;
  iso_min?: number | null;
  iso_max?: number | null;
  shutter_speed_min_sec?: number | null;
  shutter_speed_max_sec?: number | null;
  orientation?: 'portrait' | 'landscape' | 'square' | string | null;
  geo_query?: string | null;
  kind?: AssetKind | null;
  favorite?: boolean | null;
  date_from_ms?: number | null;
  date_to_ms?: number | null;
  limit?: number | null;
  offset?: number | null;
}

export interface SearchHit {
  asset: AssetSummary;
  score: number;
  timestamp_sec: number | null;
  match_reasons: string[];
  ocr_snippet: string | null;
}

export interface SearchResponse {
  items: SearchHit[];
  total_count: number;
  planner_mode: PlannerMode;
  elapsed_ms: number;
}

export interface FaceView {
  id: string;
  asset_id: string;
  person_id: string | null;
  person_name: string | null;
  bbox_x: number;
  bbox_y: number;
  bbox_w: number;
  bbox_h: number;
  det_score: number;
  assigned_by: string;
}

export interface UpdatePersonRequest {
  name?: string | null;
  is_hidden?: boolean | null;
  is_pinned?: boolean | null;
  cover_face_id?: string | null;
}

export interface MergePeopleRequest {
  source_person_ids: string[];
  target_person_id: string;
}

export interface MergePeopleResponse {
  person: PersonSummary;
  merged_faces: number;
}

export interface ReassignFaceRequest {
  target_person_id?: string | null;
  new_person_name?: string | null;
}

export interface ReassignFaceResponse {
  face_id: string;
  person_id: string | null;
}

export interface ClusterFacesResponse {
  clusters_created: number;
  faces_assigned: number;
}

export type AiModelStatus = 'not_installed' | 'downloading' | 'installed' | 'error';

export interface AiModelView {
  id: string;
  name: string;
  role: string;
  license: string;
  hf_repo: string;
  tier: AiTier | null;
  approx_bytes: number;
  status: AiModelStatus;
  bytes_total: number;
  bytes_downloaded: number;
  error_message: string | null;
  updated_at_ms: number;
}

export interface AiIndexStateView {
  active_vision_tier: AiTier;
  staging_vision_tier: AiTier | null;
  paused: boolean;
  idle_unload_secs: number;
  updated_at_ms: number;
  active_chat_tier?: ChatTier;
  vlm_rerank_enabled?: boolean;
}

export interface AiQueueStats {
  pending_assets: number;
  indexed_assets: number;
  staging_remaining: number;
}

export interface AiModelsDashboardResponse {
  ort_available: boolean;
  ort_ep: string;
  hardware: HardwareInfo;
  state: AiIndexStateView;
  models: AiModelView[];
  queue: AiQueueStats;
}

export interface SetAiTierRequest {
  tier: AiTier;
}

export type ReindexScope = 'missing' | 'all';

export interface ReindexAiRequest {
  scope?: ReindexScope;
}

export interface UpdateAiSettingsRequest {
  idle_unload_secs?: number | null;
  video_max_frames?: number | null;
}

// Phase 3 DTOs matching crates/truongphoto-core/src/models.rs & REST API contracts

export interface SetChatTierRequest {
  tier: ChatTier;
  vlm_rerank_enabled?: boolean | null;
}

export interface RouterPlan {
  semantic_query: string | null;
  ocr_terms: string | null;
  person_names: string[];
  location_query: string | null;
  camera_model: string | null;
  lens_model?: string | null;
  focal_length_min_mm?: number | null;
  focal_length_max_mm?: number | null;
  aperture_min_f?: number | null;
  aperture_max_f?: number | null;
  iso_min?: number | null;
  iso_max?: number | null;
  shutter_speed_min_sec?: number | null;
  shutter_speed_max_sec?: number | null;
  orientation?: 'portrait' | 'landscape' | 'square' | string | null;
  tags: string[];
  kind: AssetKind | null;
  favorite: boolean | null;
  date_from_ms: number | null;
  date_to_ms: number | null;
  clear_prior_filters: boolean;
  assistant_reply: string;
  follow_up_suggestions: string[];
}

export type ChatRole = 'user' | 'assistant';

export interface ChatHitRef {
  asset_id: string;
  score: number;
  timestamp_sec: number | null;
}

export interface ChatSessionSummary {
  id: string;
  title: string;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface ChatSessionDetail {
  id: string;
  title: string;
  active_filters: RouterPlan;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface ChatMessageView {
  id: string;
  session_id: string;
  role: ChatRole;
  content: string;
  plan: RouterPlan | null;
  hits: SearchHit[];
  reranked: boolean;
  planner_mode: PlannerMode | null;
  elapsed_ms: number | null;
  created_at_ms: number;
}

export interface ChatSessionWithMessagesResponse {
  session: ChatSessionDetail;
  messages: ChatMessageView[];
}

export interface CreateChatSessionRequest {
  title?: string | null;
}

export interface SendChatMessageRequest {
  content: string;
  limit?: number | null;
}

export interface SendChatMessageResponse {
  user_message: ChatMessageView;
  assistant_message: ChatMessageView;
  active_filters: RouterPlan;
  hits: SearchHit[];
  total_count: number;
  planner_mode: PlannerMode;
  reranked: boolean;
  elapsed_ms: number;
}

export interface MapConfigResponse {
  tile_mode: MapTileMode;
  available_pmtiles: string[];
  pmtiles_url: string | null;
  online_style_url: string;
  total_geotagged: number;
}

export interface MapCluster {
  id: string;
  lat: number;
  lon: number;
  count: number;
  cover_asset_id: string;
  min_lat: number;
  max_lat: number;
  min_lon: number;
  max_lon: number;
  label: string | null;
}

export interface MapClustersResponse {
  clusters: MapCluster[];
  total_in_view: number;
}

export interface MapPointsResponse {
  items: AssetSummary[];
  total_count: number;
}

// Phase 4 Tailscale, Rolling Backups & Throttling DTOs matching crates/truongphoto-core/src/models.rs

export type ThrottleReason = 'sleep' | 'battery' | 'high_cpu';

export interface ThrottleStatus {
  active: boolean;
  reason: ThrottleReason | string | null;
  cpu_usage_percent: number;
  on_battery: boolean;
}

export interface TailscaleStatus {
  installed: boolean;
  running: boolean;
  logged_in: boolean;
  cli_path: string | null;
  magic_dns_name: string | null;
  https_certs_enabled: boolean;
  serve_active: boolean;
  https_url: string | null;
  remediation: string | null;
  last_error: string | null;
}

export interface BackupSnapshotView {
  filename: string;
  path: string;
  size_bytes: number;
  created_at_ms: number;
}

export interface BackupListResponse {
  items: BackupSnapshotView[];
  backup_dir: string;
  interval_hours: number;
  keep_count: number;
}

export interface ChatWarmupResponse {
  active_chat_tier: ChatTier;
  loaded: boolean;
  warming: boolean;
}

export interface UpdateCheckResponse {
  status: 'up_to_date' | 'update_available' | 'no_releases' | 'offline';
  current_version: string;
  latest_version: string | null;
  update_available: boolean;
  release_name: string | null;
  release_notes: string | null;
  published_at: string | null;
  release_url: string;
  download_url: string | null;
  asset_name: string | null;
  repo: string;
  message: string | null;
}
