import type {
  AdminUserView,
  AiIndexStateView,
  AiModelsDashboardResponse,
  AiTier,
  AlbumDetail,
  AlbumListResponse,
  AlbumShareInput,
  AlbumShareView,
  AlbumSummary,
  AppSettings,
  AssetBucketsResponse,
  AssetDetail,
  AssetListResponse,
  AuthLoginResponse,
  AuthStatusResponse,
  BackupListResponse,
  BackupResult,
  ChatSessionSummary,
  ChatSessionWithMessagesResponse,
  ChatWarmupResponse,
  ClusterFacesResponse,
  CreateAdminUserRequest,
  CreateAlbumRequest,
  CreateFolderRequest,
  FolderView,
  HealthResponse,
  ListAssetsParams,
  LoginRequest,
  MapClustersResponse,
  MapConfigResponse,
  MapPointsResponse,
  MeResponse,
  MediaTagView,
  MergePeopleRequest,
  MergePeopleResponse,
  PartialAppSettings,
  PersonListResponse,
  PersonSummary,
  PurgeReport,
  ReassignFaceRequest,
  ReassignFaceResponse,
  ReindexScope,
  ScanAllResponse,
  ScanFolderResponse,
  SearchRequest,
  SearchResponse,
  SendChatMessageRequest,
  SendChatMessageResponse,
  ServerPinResponse,
  SetChatTierRequest,
  SetupInitializeRequest,
  SetupStatusResponse,
  ShareableUser,
  SystemInfoResponse,
  TailscaleStatus,
  TrashListResponse,
  UpdateAdminUserRequest,
  UpdateAiSettingsRequest,
  UpdateAlbumRequest,
  UpdateCheckResponse,
  UpdateFolderRequest,
  UpdateOverridesRequest,
  UpdatePersonRequest,
  UpdateServerPinRequest,
  UploadResponse,
  WsClientMessage,
  WsServerEvent,
} from '../types/api';

export class ApiClientError extends Error {
  readonly status: number;
  readonly code: string;
  readonly details: unknown;
  readonly retryAfterSecs: number | null;

  constructor(
    status: number,
    code: string,
    message: string,
    details: unknown = null,
    retryAfterSecs: number | null = null,
  ) {
    super(message);
    this.name = 'ApiClientError';
    this.status = status;
    this.code = code;
    this.details = details;
    this.retryAfterSecs = retryAfterSecs;
  }
}

type UnauthorizedHandler = () => void;
let unauthorizedHandler: UnauthorizedHandler | null = null;

export function setUnauthorizedHandler(cb: UnauthorizedHandler | null): void {
  unauthorizedHandler = cb;
}

function parseRetryAfter(headerValue: string | null, details: unknown): number | null {
  if (headerValue) {
    const trimmed = headerValue.trim();
    if (/^\d+$/.test(trimmed)) {
      return Number.parseInt(trimmed, 10);
    }
    const parsedDate = Date.parse(trimmed);
    if (!Number.isNaN(parsedDate)) {
      return Math.max(0, Math.ceil((parsedDate - Date.now()) / 1000));
    }
  }
  if (
    details &&
    typeof details === 'object' &&
    'retry_after_secs' in details &&
    typeof (details as { retry_after_secs?: unknown }).retry_after_secs === 'number'
  ) {
    return (details as { retry_after_secs: number }).retry_after_secs;
  }
  return null;
}

interface RequestOptions extends RequestInit {
  skipUnauthorizedHandler?: boolean;
}

async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { skipUnauthorizedHandler = false, headers: customHeaders, ...init } = options;
  const headers = new Headers(customHeaders);
  if (
    init.body !== undefined &&
    !(typeof FormData !== 'undefined' && init.body instanceof FormData) &&
    !headers.has('Content-Type')
  ) {
    headers.set('Content-Type', 'application/json');
  }

  const response = await fetch(path, {
    credentials: 'include',
    ...init,
    headers,
  });

  if (!response.ok) {
    let code = response.status === 401 ? 'UNAUTHORIZED' : 'INTERNAL_ERROR';
    let message = response.statusText || `Request failed with status ${response.status}`;
    let details: unknown = null;

    const text = await response.text().catch(() => '');
    if (text) {
      try {
        const parsed = JSON.parse(text) as {
          error?: { code?: string; message?: string; details?: unknown };
        };
        if (parsed?.error) {
          if (typeof parsed.error.code === 'string') code = parsed.error.code;
          if (typeof parsed.error.message === 'string') message = parsed.error.message;
          if ('details' in parsed.error) details = parsed.error.details ?? null;
        }
      } catch {
        message = text;
      }
    }

    const retryAfterSecs = parseRetryAfter(response.headers.get('Retry-After'), details);
    if (response.status === 401 && !skipUnauthorizedHandler) {
      unauthorizedHandler?.();
    }
    throw new ApiClientError(response.status, code, message, details, retryAfterSecs);
  }

  if (response.status === 204) {
    return undefined as T;
  }

  const contentType = response.headers.get('Content-Type') ?? '';
  if (!contentType.includes('application/json')) {
    const text = await response.text();
    if (!text) return undefined as T;
    return JSON.parse(text) as T;
  }
  return (await response.json()) as T;
}

// Health & System
export const getHealth = (): Promise<HealthResponse> =>
  request<HealthResponse>('/api/health', { method: 'GET' });

export const getSystemInfo = (): Promise<SystemInfoResponse> =>
  request<SystemInfoResponse>('/api/system/info', { method: 'GET' });

// First-Run Setup
export const getSetupStatus = (): Promise<SetupStatusResponse> =>
  request<SetupStatusResponse>('/api/setup/status', { method: 'GET' });

export const initializeSetup = (req: SetupInitializeRequest): Promise<AuthLoginResponse> =>
  request<AuthLoginResponse>('/api/setup/initialize', {
    method: 'POST',
    body: JSON.stringify(req),
  });

// Auth
export const getAuthStatus = (): Promise<AuthStatusResponse> =>
  request<AuthStatusResponse>('/api/auth/status', {
    method: 'GET',
    skipUnauthorizedHandler: true,
  });

export const verifyServerPin = (serverPin: string): Promise<ServerPinResponse> =>
  request<ServerPinResponse>('/api/auth/server-pin', {
    method: 'POST',
    body: JSON.stringify({ server_pin: serverPin }),
    skipUnauthorizedHandler: true,
  });

export const loginWithProfilePin = (req: LoginRequest): Promise<AuthLoginResponse> =>
  request<AuthLoginResponse>('/api/auth/login', {
    method: 'POST',
    body: JSON.stringify(req),
    skipUnauthorizedHandler: true,
  });

export const desktopBootstrap = (token: string): Promise<AuthLoginResponse> =>
  request<AuthLoginResponse>('/api/auth/desktop-bootstrap', {
    method: 'POST',
    body: JSON.stringify({ token }),
    skipUnauthorizedHandler: true,
  });

export const logout = (): Promise<void> =>
  request<void>('/api/auth/logout', {
    method: 'POST',
    skipUnauthorizedHandler: true,
  });

export const getMe = (): Promise<MeResponse> =>
  request<MeResponse>('/api/auth/me', { method: 'GET' });

// Watched Folders
export const listFolders = async (): Promise<FolderView[]> => {
  const data = await request<{ items: FolderView[] }>('/api/folders', { method: 'GET' });
  return data.items;
};

export const createFolder = async (req: CreateFolderRequest): Promise<FolderView> => {
  const data = await request<{ folder: FolderView }>('/api/folders', {
    method: 'POST',
    body: JSON.stringify(req),
  });
  return data.folder;
};

export const updateFolder = async (id: string, req: UpdateFolderRequest): Promise<FolderView> => {
  const data = await request<{ folder: FolderView }>(
    `/api/folders/${encodeURIComponent(id)}`,
    {
      method: 'PATCH',
      body: JSON.stringify(req),
    },
  );
  return data.folder;
};

export const deleteFolder = (id: string): Promise<void> =>
  request<void>(`/api/folders/${encodeURIComponent(id)}`, { method: 'DELETE' });

// Admin Users, Server PIN, Settings & Backup
export const listAdminUsers = async (): Promise<AdminUserView[]> => {
  const data = await request<{ items: AdminUserView[] }>('/api/admin/users', { method: 'GET' });
  return data.items;
};

export const createAdminUser = async (req: CreateAdminUserRequest): Promise<AdminUserView> => {
  const data = await request<{ user: AdminUserView }>('/api/admin/users', {
    method: 'POST',
    body: JSON.stringify(req),
  });
  return data.user;
};

export const updateAdminUser = async (
  id: string,
  req: UpdateAdminUserRequest,
): Promise<AdminUserView> => {
  const data = await request<{ user: AdminUserView }>(
    `/api/admin/users/${encodeURIComponent(id)}`,
    {
      method: 'PATCH',
      body: JSON.stringify(req),
    },
  );
  return data.user;
};

export const deleteAdminUser = (id: string): Promise<void> =>
  request<void>(`/api/admin/users/${encodeURIComponent(id)}`, { method: 'DELETE' });

export const updateServerPin = (req: UpdateServerPinRequest): Promise<void> =>
  request<void>('/api/admin/server-pin', {
    method: 'PUT',
    body: JSON.stringify(req),
  });

export const getAdminSettings = (): Promise<AppSettings> =>
  request<AppSettings>('/api/admin/settings', { method: 'GET' });

export const updateAdminSettings = (patch: PartialAppSettings): Promise<AppSettings> =>
  request<AppSettings>('/api/admin/settings', {
    method: 'PATCH',
    body: JSON.stringify(patch),
  });

export const triggerBackup = (): Promise<BackupResult> =>
  request<BackupResult>('/api/admin/backup', { method: 'POST' });

// Library Shell Endpoints
export const listAssets = (params: ListAssetsParams = {}): Promise<AssetListResponse> => {
  const query = new URLSearchParams();
  if (params.folder_id) query.set('folder_id', params.folder_id);
  if (params.favorite !== undefined) query.set('favorite', String(params.favorite));
  if (params.before_ms !== undefined) query.set('before_ms', String(params.before_ms));
  if (params.cursor) query.set('cursor', params.cursor);
  if (params.limit !== undefined) query.set('limit', String(params.limit));
  const qs = query.toString();
  return request<AssetListResponse>(qs ? `/api/assets?${qs}` : '/api/assets', { method: 'GET' });
};

export const listAssetBuckets = (): Promise<AssetBucketsResponse> =>
  request<AssetBucketsResponse>('/api/assets/buckets', { method: 'GET' });

export const listAlbums = (): Promise<AlbumListResponse> =>
  request<AlbumListResponse>('/api/albums', { method: 'GET' });

export const listPeople = (opts?: { include_hidden?: boolean }): Promise<PersonListResponse> => {
  const query = new URLSearchParams();
  if (opts?.include_hidden !== undefined) {
    query.set('include_hidden', String(opts.include_hidden));
  }
  const qs = query.toString();
  return request<PersonListResponse>(qs ? `/api/people?${qs}` : '/api/people', { method: 'GET' });
};

export const listTrash = (
  params: { cursor?: string; limit?: number } = {},
): Promise<TrashListResponse> => {
  const query = new URLSearchParams();
  if (params.cursor) query.set('cursor', params.cursor);
  if (params.limit !== undefined) query.set('limit', String(params.limit));
  const qs = query.toString();
  return request<TrashListResponse>(qs ? `/api/trash?${qs}` : '/api/trash', { method: 'GET' });
};

// Phase 1 Media URL Helpers
export const mediaThumbUrl = (id: string): string =>
  `/api/media/${encodeURIComponent(id)}/thumb`;

export const mediaPreviewUrl = (id: string): string =>
  `/api/media/${encodeURIComponent(id)}/preview`;

export const mediaOriginalUrl = (
  id: string,
  opts?: { fileRole?: string; mode?: string; download?: boolean },
): string => {
  const query = new URLSearchParams();
  if (opts?.fileRole) query.set('file_role', opts.fileRole);
  if (opts?.mode) query.set('mode', opts.mode);
  if (opts?.download) query.set('download', '1');
  const qs = query.toString();
  const base = `/api/media/${encodeURIComponent(id)}/original`;
  return qs ? `${base}?${qs}` : base;
};

/**
 * Triggers a file download without opening a popup window (works in both browsers and Tauri WebViews).
 */
export function triggerAssetDownload(id: string): void {
  if (typeof document === 'undefined') return;
  const link = document.createElement('a');
  link.href = mediaOriginalUrl(id, { download: true });
  link.download = '';
  link.rel = 'noopener';
  link.style.display = 'none';
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
}

export const mediaLiveVideoUrl = (id: string): string =>
  `/api/media/${encodeURIComponent(id)}/live-video`;

export const mediaHlsUrl = (id: string): string =>
  `/api/media/${encodeURIComponent(id)}/hls/index.m3u8`;

// Phase 1 Asset Inspection, Overrides, Tags & Trash
export const getAssetDetail = (id: string): Promise<AssetDetail> =>
  request<AssetDetail>(`/api/assets/${encodeURIComponent(id)}`, { method: 'GET' });

export const setAssetFavorite = (
  id: string,
  isFavorite: boolean,
): Promise<{ id: string; is_favorite: boolean }> =>
  request<{ id: string; is_favorite: boolean }>(
    `/api/assets/${encodeURIComponent(id)}/favorite`,
    {
      method: 'PATCH',
      body: JSON.stringify({ is_favorite: isFavorite }),
    },
  );

export const updateAssetOverrides = (
  id: string,
  req: UpdateOverridesRequest,
): Promise<AssetDetail> =>
  request<AssetDetail>(`/api/assets/${encodeURIComponent(id)}/overrides`, {
    method: 'PATCH',
    body: JSON.stringify(req),
  });

export const addAssetTag = async (id: string, tag: string): Promise<MediaTagView[]> => {
  const data = await request<{ tags: MediaTagView[] }>(
    `/api/assets/${encodeURIComponent(id)}/tags`,
    {
      method: 'POST',
      body: JSON.stringify({ tag }),
    },
  );
  return data.tags;
};

export const removeAssetTag = async (id: string, tag: string): Promise<MediaTagView[]> => {
  const data = await request<{ tags: MediaTagView[] }>(
    `/api/assets/${encodeURIComponent(id)}/tags/${encodeURIComponent(tag)}`,
    { method: 'DELETE' },
  );
  return data.tags;
};

export const trashAssets = (assetIds: string[]): Promise<{ trashed_count: number }> =>
  request<{ trashed_count: number }>('/api/assets/trash', {
    method: 'POST',
    body: JSON.stringify({ asset_ids: assetIds }),
  });

export const restoreTrashAssets = (assetIds: string[]): Promise<{ restored_count: number }> =>
  request<{ restored_count: number }>('/api/trash/restore', {
    method: 'POST',
    body: JSON.stringify({ asset_ids: assetIds }),
  });

export const purgeTrashAssets = (
  opts: { asset_ids?: string[]; all?: boolean } = {},
): Promise<PurgeReport> =>
  request<PurgeReport>('/api/trash/purge', {
    method: 'POST',
    body: JSON.stringify(opts),
  });

function parseErrorBody(
  status: number,
  statusText: string,
  text: string,
  retryAfterHeader: string | null,
): ApiClientError {
  let code = status === 401 ? 'UNAUTHORIZED' : 'INTERNAL_ERROR';
  let message = statusText || `Request failed with status ${status}`;
  let details: unknown = null;

  if (text) {
    try {
      const parsed = JSON.parse(text) as {
        error?: { code?: string; message?: string; details?: unknown };
      };
      if (parsed?.error) {
        if (typeof parsed.error.code === 'string') code = parsed.error.code;
        if (typeof parsed.error.message === 'string') message = parsed.error.message;
        if ('details' in parsed.error) details = parsed.error.details ?? null;
      }
    } catch {
      message = text;
    }
  }

  const retryAfterSecs = parseRetryAfter(retryAfterHeader, details);
  return new ApiClientError(status, code, message, details, retryAfterSecs);
}

// Phase 1 Streaming Multipart Upload (XHR for upload.onprogress, fetch fallback for Node tests)
export const uploadMediaFiles = async (
  files: File[],
  opts?: { folderId?: string; onProgress?: (pct: number) => void },
): Promise<UploadResponse> => {
  const formData = new FormData();
  if (opts?.folderId) {
    formData.append('folder_id', opts.folderId);
  }
  for (const file of files) {
    formData.append('files[]', file);
  }

  if (typeof XMLHttpRequest === 'undefined') {
    const res = await request<UploadResponse>('/api/uploads', {
      method: 'POST',
      body: formData,
    });
    opts?.onProgress?.(100);
    return res;
  }

  return new Promise<UploadResponse>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/api/uploads', true);
    xhr.withCredentials = true;

    xhr.upload.onprogress = (ev) => {
      if (ev.lengthComputable && ev.total > 0) {
        opts?.onProgress?.(Math.min(100, Math.round((ev.loaded / ev.total) * 100)));
      }
    };

    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        opts?.onProgress?.(100);
        try {
          resolve(JSON.parse(xhr.responseText || '{}') as UploadResponse);
        } catch (err) {
          reject(
            new ApiClientError(
              xhr.status,
              'INTERNAL_ERROR',
              err instanceof Error ? err.message : 'Invalid upload JSON response',
            ),
          );
        }
        return;
      }

      const apiErr = parseErrorBody(
        xhr.status,
        xhr.statusText,
        xhr.responseText,
        xhr.getResponseHeader('Retry-After'),
      );
      if (xhr.status === 401) {
        unauthorizedHandler?.();
      }
      reject(apiErr);
    };

    xhr.onerror = () => {
      reject(new ApiClientError(0, 'INTERNAL_ERROR', 'Network error while uploading media'));
    };

    xhr.onabort = () => {
      reject(new ApiClientError(0, 'INTERNAL_ERROR', 'Upload aborted'));
    };

    xhr.send(formData);
  });
};

// Phase 1 Folder Scans
export const scanFolder = (id: string): Promise<ScanFolderResponse> =>
  request<ScanFolderResponse>(`/api/folders/${encodeURIComponent(id)}/scan`, {
    method: 'POST',
  });

export const scanAllFolders = (): Promise<ScanAllResponse> =>
  request<ScanAllResponse>('/api/folders/scan-all', { method: 'POST' });

// Phase 1 Albums CRUD & Sharing
export const createAlbum = async (req: CreateAlbumRequest): Promise<AlbumSummary> => {
  const data = await request<{ album: AlbumSummary }>('/api/albums', {
    method: 'POST',
    body: JSON.stringify(req),
  });
  return data.album;
};

export const getAlbumDetail = (id: string): Promise<AlbumDetail> =>
  request<AlbumDetail>(`/api/albums/${encodeURIComponent(id)}`, { method: 'GET' });

export const updateAlbum = async (id: string, req: UpdateAlbumRequest): Promise<AlbumSummary> => {
  const data = await request<{ album: AlbumSummary }>(`/api/albums/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body: JSON.stringify(req),
  });
  return data.album;
};

export const deleteAlbum = (id: string): Promise<void> =>
  request<void>(`/api/albums/${encodeURIComponent(id)}`, { method: 'DELETE' });

export const addAlbumAssets = (
  id: string,
  assetIds: string[],
): Promise<{ added_count: number }> =>
  request<{ added_count: number }>(`/api/albums/${encodeURIComponent(id)}/assets`, {
    method: 'POST',
    body: JSON.stringify({ asset_ids: assetIds }),
  });

export const removeAlbumAssets = (
  id: string,
  assetIds: string[],
): Promise<{ removed_count: number }> =>
  request<{ removed_count: number }>(`/api/albums/${encodeURIComponent(id)}/assets`, {
    method: 'DELETE',
    body: JSON.stringify({ asset_ids: assetIds }),
  });

export const replaceAlbumShares = async (
  id: string,
  shares: AlbumShareInput[],
): Promise<AlbumShareView[]> => {
  const data = await request<{ shares: AlbumShareView[] }>(
    `/api/albums/${encodeURIComponent(id)}/shares`,
    {
      method: 'PUT',
      body: JSON.stringify({ shares }),
    },
  );
  return data.shares;
};

export const listShareableUsers = async (): Promise<ShareableUser[]> => {
  const data = await request<{ items: ShareableUser[] }>('/api/users/shareable', {
    method: 'GET',
  });
  return data.items;
};

// Phase 2 Search, People & AI Models
export const searchLibrary = (req: SearchRequest): Promise<SearchResponse> =>
  request<SearchResponse>('/api/search', {
    method: 'POST',
    body: JSON.stringify(req),
  });

export const getPerson = (id: string): Promise<PersonSummary> =>
  request<PersonSummary>(`/api/people/${encodeURIComponent(id)}`, { method: 'GET' });

export const listPersonAssets = (
  id: string,
  params: { cursor?: string; limit?: number } = {},
): Promise<AssetListResponse> => {
  const query = new URLSearchParams();
  if (params.cursor) query.set('cursor', params.cursor);
  if (params.limit !== undefined) query.set('limit', String(params.limit));
  const qs = query.toString();
  const base = `/api/people/${encodeURIComponent(id)}/assets`;
  return request<AssetListResponse>(qs ? `${base}?${qs}` : base, { method: 'GET' });
};

export const faceThumbUrl = (faceId: string): string =>
  `/api/media/faces/${encodeURIComponent(faceId)}/thumb`;

export const updatePerson = (id: string, req: UpdatePersonRequest): Promise<PersonSummary> =>
  request<PersonSummary>(`/api/people/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body: JSON.stringify(req),
  });

export const mergePeople = (req: MergePeopleRequest): Promise<MergePeopleResponse> =>
  request<MergePeopleResponse>('/api/people/merge', {
    method: 'POST',
    body: JSON.stringify(req),
  });

export const reassignFace = (
  personId: string,
  faceId: string,
  req: ReassignFaceRequest,
): Promise<ReassignFaceResponse> =>
  request<ReassignFaceResponse>(
    `/api/people/${encodeURIComponent(personId)}/faces/${encodeURIComponent(faceId)}/reassign`,
    {
      method: 'POST',
      body: JSON.stringify(req),
    },
  );

export const triggerFaceClustering = (): Promise<ClusterFacesResponse> =>
  request<ClusterFacesResponse>('/api/people/cluster', { method: 'POST' });

export const getAiModelsDashboard = (): Promise<AiModelsDashboardResponse> =>
  request<AiModelsDashboardResponse>('/api/admin/ai/models', { method: 'GET' });

export const downloadAiModel = (modelId: string): Promise<void> =>
  request<void>(`/api/admin/ai/models/${encodeURIComponent(modelId)}/download`, {
    method: 'POST',
  });

export const cancelAiModelDownload = (modelId: string): Promise<void> =>
  request<void>(`/api/admin/ai/models/${encodeURIComponent(modelId)}/cancel`, {
    method: 'POST',
  });

export const deleteAiModel = (modelId: string): Promise<void> =>
  request<void>(`/api/admin/ai/models/${encodeURIComponent(modelId)}`, {
    method: 'DELETE',
  });

export const setAiVisionTier = (tier: AiTier): Promise<void> =>
  request<void>('/api/admin/ai/tier', {
    method: 'POST',
    body: JSON.stringify({ tier }),
  });

export const triggerAiReindex = (scope: ReindexScope): Promise<void> =>
  request<void>('/api/admin/ai/reindex', {
    method: 'POST',
    body: JSON.stringify({ scope }),
  });

export const pauseAiIndexing = (): Promise<AiIndexStateView> =>
  request<AiIndexStateView>('/api/admin/ai/pause', { method: 'POST' });

export const resumeAiIndexing = (): Promise<AiIndexStateView> =>
  request<AiIndexStateView>('/api/admin/ai/resume', { method: 'POST' });

export const updateAiSettings = (req: UpdateAiSettingsRequest): Promise<AiIndexStateView> =>
  request<AiIndexStateView>('/api/admin/ai/settings', {
    method: 'PATCH',
    body: JSON.stringify(req),
  });

// Phase 3 Conversational Search & Offline-First Map
export const listChatSessions = async (): Promise<ChatSessionSummary[]> => {
  const data = await request<{ items: ChatSessionSummary[] }>('/api/chat/sessions', {
    method: 'GET',
  });
  return data.items;
};

export const createChatSession = async (title?: string): Promise<ChatSessionSummary> => {
  const data = await request<{ session: ChatSessionSummary }>('/api/chat/sessions', {
    method: 'POST',
    body: JSON.stringify(title !== undefined ? { title } : {}),
  });
  return data.session;
};

export const getChatSession = (id: string): Promise<ChatSessionWithMessagesResponse> =>
  request<ChatSessionWithMessagesResponse>(`/api/chat/sessions/${encodeURIComponent(id)}`, {
    method: 'GET',
  });

export const deleteChatSession = (id: string): Promise<void> =>
  request<void>(`/api/chat/sessions/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });

export const sendChatMessage = (
  sessionId: string,
  content: string,
  limit?: number,
  signal?: AbortSignal,
): Promise<SendChatMessageResponse> => {
  const tzOffsetMinutes = -new Date().getTimezoneOffset();
  const body: SendChatMessageRequest =
    limit !== undefined
      ? { content, limit, tz_offset_minutes: tzOffsetMinutes }
      : { content, tz_offset_minutes: tzOffsetMinutes };
  return request<SendChatMessageResponse>(
    `/api/chat/sessions/${encodeURIComponent(sessionId)}/messages`,
    {
      method: 'POST',
      body: JSON.stringify(body),
      signal,
    },
  );
};

export const setAiChatTier = (req: SetChatTierRequest): Promise<AiIndexStateView> =>
  request<AiIndexStateView>('/api/admin/ai/chat-tier', {
    method: 'POST',
    body: JSON.stringify(req),
  });

export const warmupChat = (): Promise<ChatWarmupResponse> =>
  request<ChatWarmupResponse>('/api/chat/warmup', {
    method: 'POST',
  });

export const checkForUpdates = (): Promise<UpdateCheckResponse> =>
  request<UpdateCheckResponse>('/api/admin/updates/check', {
    method: 'GET',
  });

export const getMapConfig = (): Promise<MapConfigResponse> =>
  request<MapConfigResponse>('/api/map/config', { method: 'GET' });

export const getMapClusters = (bbox: {
  min_lat: number;
  max_lat: number;
  min_lng: number;
  max_lng: number;
  zoom: number;
}): Promise<MapClustersResponse> => {
  const query = new URLSearchParams({
    min_lat: String(bbox.min_lat),
    max_lat: String(bbox.max_lat),
    min_lng: String(bbox.min_lng),
    max_lng: String(bbox.max_lng),
    zoom: String(bbox.zoom),
  });
  return request<MapClustersResponse>(`/api/map/clusters?${query.toString()}`, {
    method: 'GET',
  });
};

export const getMapPoints = (bbox: {
  min_lat: number;
  max_lat: number;
  min_lng: number;
  max_lng: number;
  limit?: number;
}): Promise<MapPointsResponse> => {
  const query = new URLSearchParams({
    min_lat: String(bbox.min_lat),
    max_lat: String(bbox.max_lat),
    min_lng: String(bbox.min_lng),
    max_lng: String(bbox.max_lng),
  });
  if (bbox.limit !== undefined) {
    query.set('limit', String(bbox.limit));
  }
  return request<MapPointsResponse>(`/api/map/points?${query.toString()}`, {
    method: 'GET',
  });
};

export const mapTileUrl = (name: string): string =>
  `/api/map/tiles/${encodeURIComponent(name)}`;

// Phase 4 Tailscale Remote HTTPS, Rolling Backups & QR SVG
export const getTailscaleStatus = (): Promise<TailscaleStatus> =>
  request<TailscaleStatus>('/api/admin/tailscale/status', { method: 'GET' });

export const enableTailscaleServe = (): Promise<TailscaleStatus> =>
  request<TailscaleStatus>('/api/admin/tailscale/enable', { method: 'POST' });

export const disableTailscaleServe = (): Promise<TailscaleStatus> =>
  request<TailscaleStatus>('/api/admin/tailscale/disable', { method: 'POST' });

export const listBackups = (): Promise<BackupListResponse> =>
  request<BackupListResponse>('/api/admin/backups', { method: 'GET' });

export const systemQrUrl = (url: string): string =>
  `/api/system/qr?url=${encodeURIComponent(url)}`;

// WebSocket Client with exponential backoff (1s -> 2s -> 4s -> 8s -> 16s -> 30s) & periodic ping
const WS_BACKOFF_MS = [1000, 2000, 4000, 8000, 16000, 30000] as const;
const WS_PING_INTERVAL_MS = 25000;

export interface WsClientOptions {
  url?: string;
  onEvent: (event: WsServerEvent) => void;
  onConnectionChange?: (connected: boolean) => void;
}

export class WsClient {
  private readonly urlOverride?: string;
  private readonly onEvent: (event: WsServerEvent) => void;
  private readonly onConnectionChange?: (connected: boolean) => void;
  private socket: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private pingTimer: ReturnType<typeof setInterval> | null = null;
  private attempt = 0;
  private closedManually = false;

  constructor(options: WsClientOptions) {
    this.urlOverride = options.url;
    this.onEvent = options.onEvent;
    this.onConnectionChange = options.onConnectionChange;
  }

  private resolveUrl(): string {
    if (this.urlOverride) return this.urlOverride;
    if (typeof window === 'undefined' || !window.location?.host) {
      return 'ws://127.0.0.1:38420/api/ws';
    }
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    return `${protocol}//${window.location.host}/api/ws`;
  }

  connect(): void {
    this.closedManually = false;
    this.clearReconnectTimer();
    if (typeof WebSocket === 'undefined') return;
    if (
      this.socket &&
      (this.socket.readyState === WebSocket.OPEN ||
        this.socket.readyState === WebSocket.CONNECTING)
    ) {
      return;
    }

    const ws = new WebSocket(this.resolveUrl());
    this.socket = ws;

    ws.onopen = () => {
      if (this.socket !== ws) return;
      this.attempt = 0;
      this.onConnectionChange?.(true);
      this.startPing();
    };

    ws.onmessage = (event) => {
      if (this.socket !== ws || typeof event.data !== 'string') return;
      try {
        const parsed = JSON.parse(event.data) as WsServerEvent;
        if (parsed && typeof parsed.type === 'string') {
          this.onEvent(parsed);
        }
      } catch {
        // Ignore malformed payloads
      }
    };

    ws.onerror = () => {
      // onclose will handle cleanup and reconnect scheduling
    };

    ws.onclose = () => {
      if (this.socket !== ws) {
        return;
      }
      this.socket = null;
      this.stopPing();
      this.onConnectionChange?.(false);
      if (!this.closedManually) {
        this.scheduleReconnect();
      }
    };
  }

  disconnect(): void {
    this.closedManually = true;
    this.clearReconnectTimer();
    this.stopPing();
    const ws = this.socket;
    this.socket = null;
    if (ws) {
      ws.onopen = null;
      ws.onmessage = null;
      ws.onerror = null;
      ws.onclose = null;
      try {
        ws.close();
      } catch {
        // Ignore close errors
      }
    }
    this.onConnectionChange?.(false);
  }

  sendPing(): void {
    if (this.socket && this.socket.readyState === WebSocket.OPEN) {
      const msg: WsClientMessage = { type: 'ping' };
      this.socket.send(JSON.stringify(msg));
    }
  }

  private startPing(): void {
    this.stopPing();
    this.pingTimer = setInterval(() => {
      this.sendPing();
    }, WS_PING_INTERVAL_MS);
  }

  private stopPing(): void {
    if (this.pingTimer !== null) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private scheduleReconnect(): void {
    this.clearReconnectTimer();
    const idx = Math.min(this.attempt, WS_BACKOFF_MS.length - 1);
    const delay = WS_BACKOFF_MS[idx];
    this.attempt += 1;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      if (!this.closedManually) {
        this.connect();
      }
    }, delay);
  }
}
