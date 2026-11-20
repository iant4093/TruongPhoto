// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  ApiClientError,
  WsClient,
  deleteFolder,
  getMe,
  loginWithProfilePin,
  logout,
  setUnauthorizedHandler,
  verifyServerPin,
} from '../src/lib/api';
import {
  GB,
  KB,
  MB,
  TB,
  formatBytes,
  formatDateTime,
  formatRelativeTime,
  formatUptime,
} from '../src/lib/format';
import { useAppStore } from '../src/store/useAppStore';
import type {
  AdminUserView,
  AppSettings,
  AuthStatusResponse,
  FolderView,
  SetupStatusResponse,
  SystemInfoResponse,
  UserPublic,
} from '../src/types/api';

const tauriMocks = vi.hoisted(() => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: tauriMocks.invoke,
}));

function jsonResponse(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      'Content-Type': 'application/json',
      ...headers,
    },
  });
}

const sampleAdmin: UserPublic = {
  id: 'u-admin',
  username: 'ian',
  display_name: 'Ian',
  role: 'admin',
  avatar_color: '#4a4a4a',
};

const sampleViewer: UserPublic = {
  id: 'u-viewer',
  username: 'guest',
  display_name: 'Guest',
  role: 'viewer',
  avatar_color: '#556b5d',
};

const sampleSystemInfo: SystemInfoResponse = {
  version: '0.1.0',
  platform: 'macos',
  arch: 'aarch64',
  hostname: 'MacBook.local',
  port: 38420,
  lan_urls: ['http://192.168.1.10:38420'],
  mdns_enabled: true,
  mdns_active: true,
  mdns_url: 'http://truongphoto.local:38420',
  desktop_pin_lock: false,
  hardware: {
    total_ram_bytes: 32 * GB,
    cpu_cores: 12,
    recommended_vision_tier: 'tier2',
    recommended_chat_tier: 'tier3',
  },
  stats: {
    asset_count: 10,
    folder_count: 1,
    db_size_bytes: 163840,
  },
  power: {
    sleeping: false,
    display_asleep: false,
    session_locked: false,
    on_battery: false,
  },
};

const sampleFolders: FolderView[] = [
  {
    id: 'f-1',
    name: 'Pictures',
    root_path: '/Users/ian/Pictures',
    volume_uuid: 'VOL-123',
    volume_label: 'Macintosh HD',
    is_managed_uploads: false,
    allow_disk_delete: false,
    is_offline: false,
    last_scanned_at_ms: 1700000000000,
    asset_count: 10,
    created_at_ms: 1700000000000,
  },
];

const sampleAdminUsers: AdminUserView[] = [
  {
    id: 'u-admin',
    username: 'ian',
    display_name: 'Ian',
    role: 'admin',
    avatar_color: '#4a4a4a',
    disabled_at_ms: null,
    allowed_folder_ids: [],
    created_at_ms: 1700000000000,
  },
];

const sampleSettings: AppSettings = {
  port: 38420,
  mdns_enabled: true,
  mdns_hostname: 'truongphoto.local',
  desktop_pin_lock: false,
  managed_uploads_dir: '/data/uploads',
  thumbnails_dir: '/data/thumbnails',
  cache_dir: '/data/cache',
  cache_limit_gb: 10,
  backup_dir: '/data/backups',
  backup_keep_count: 7,
  video_max_frames: 30,
  ai_tier_vision: 'tier2',
  ai_tier_chat: 'tier2',
};

class MockWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static instances: MockWebSocket[] = [];

  readonly url: string;
  readyState = MockWebSocket.CONNECTING;
  sent: string[] = [];
  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    MockWebSocket.instances.push(this);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.({} as CloseEvent);
  }

  simulateOpen(): void {
    this.readyState = MockWebSocket.OPEN;
    this.onopen?.({} as Event);
  }

  simulateMessage(payload: unknown): void {
    this.onmessage?.({ data: JSON.stringify(payload) } as MessageEvent);
  }

  simulateDrop(): void {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.({} as CloseEvent);
  }
}

beforeEach(() => {
  MockWebSocket.instances = [];
  vi.stubGlobal('WebSocket', MockWebSocket as unknown as typeof WebSocket);
  delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  tauriMocks.invoke.mockReset();
  useAppStore.getState().resetForTest();
});

afterEach(() => {
  useAppStore.getState().resetForTest();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('format.ts', () => {
  it('formats byte sizes across units', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(-5)).toBe('0 B');
    expect(formatBytes(Number.NaN)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2 * KB)).toBe('2.0 KB');
    expect(formatBytes(5.5 * MB)).toBe('5.5 MB');
    expect(formatBytes(1.25 * GB)).toBe('1.25 GB');
    expect(formatBytes(2.5 * TB)).toBe('2.50 TB');
  });

  it('formats uptime durations cleanly', () => {
    expect(formatUptime(-1)).toBe('0s');
    expect(formatUptime(42)).toBe('42s');
    expect(formatUptime(125)).toBe('2m 5s');
    expect(formatUptime(120)).toBe('2m');
    expect(formatUptime(3660)).toBe('1h 1m');
    expect(formatUptime(86400 + 7200)).toBe('1d 2h');
  });

  it('formats relative and date-time timestamps', () => {
    const now = 1_700_000_000_000;
    expect(formatRelativeTime(now - 3_000, now)).toBe('Just now');
    expect(formatRelativeTime(now - 25_000, now)).toBe('25s ago');
    expect(formatRelativeTime(now - 5 * 60_000, now)).toBe('5m ago');
    expect(formatRelativeTime(now - 3 * 3600_000, now)).toBe('3h ago');
    expect(formatRelativeTime(now - 4 * 86400_000, now)).toBe('4d ago');
    expect(formatRelativeTime(now + 120_000, now)).toBe('in 2m');
    expect(formatRelativeTime(0, now)).toBe('Unknown');
    expect(formatDateTime(0)).toBe('Unknown');
    expect(formatDateTime(now)).not.toBe('Unknown');
  });
});

describe('api.ts', () => {
  it('parses unified error envelope and Retry-After header', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(
          {
            error: {
              code: 'RATE_LIMITED',
              message: 'Too many failed attempts',
              details: { retry_after_secs: 16 },
            },
          },
          429,
          { 'Retry-After': '32' },
        ),
      ),
    );

    let caught: unknown;
    try {
      await verifyServerPin('000000');
    } catch (err) {
      caught = err;
    }

    expect(caught).toBeInstanceOf(ApiClientError);
    const apiErr = caught as ApiClientError;
    expect(apiErr.status).toBe(429);
    expect(apiErr.code).toBe('RATE_LIMITED');
    expect(apiErr.message).toBe('Too many failed attempts');
    expect(apiErr.retryAfterSecs).toBe(32);
    expect(apiErr.details).toEqual({ retry_after_secs: 16 });
  });

  it('falls back to details.retry_after_secs when Retry-After header is absent', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(
          {
            error: {
              code: 'RATE_LIMITED',
              message: 'Slow down',
              details: { retry_after_secs: 8 },
            },
          },
          429,
        ),
      ),
    );

    await expect(verifyServerPin('111111')).rejects.toMatchObject({
      status: 429,
      code: 'RATE_LIMITED',
      retryAfterSecs: 8,
    });
  });

  it('invokes unauthorized handler on 401 for protected endpoints but not on PIN login', async () => {
    const onUnauthorized = vi.fn();
    setUnauthorizedHandler(onUnauthorized);

    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(
          { error: { code: 'UNAUTHORIZED', message: 'Invalid PIN', details: null } },
          401,
        ),
      ),
    );

    await expect(
      loginWithProfilePin({ user_id: 'u1', profile_pin: '9999' }),
    ).rejects.toBeInstanceOf(ApiClientError);
    expect(onUnauthorized).not.toHaveBeenCalled();

    await expect(getMe()).rejects.toBeInstanceOf(ApiClientError);
    expect(onUnauthorized).toHaveBeenCalledTimes(1);
  });

  it('handles 204 No Content responses without JSON parsing errors', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(new Response(null, { status: 204 })),
    );

    await expect(logout()).resolves.toBeUndefined();
    await expect(deleteFolder('f-1')).resolves.toBeUndefined();
  });

  it('reconnects WsClient with exponential backoff and sends periodic ping', () => {
    vi.useFakeTimers();
    const events: string[] = [];
    const states: boolean[] = [];

    const client = new WsClient({
      url: 'ws://127.0.0.1:38420/api/ws',
      onEvent: (ev) => events.push(ev.type),
      onConnectionChange: (c) => states.push(c),
    });

    client.connect();
    expect(MockWebSocket.instances).toHaveLength(1);

    const ws1 = MockWebSocket.instances[0];
    ws1.simulateOpen();
    expect(states.at(-1)).toBe(true);

    ws1.simulateMessage({ type: 'pong' });
    expect(events).toEqual(['pong']);

    vi.advanceTimersByTime(25_000);
    expect(ws1.sent).toEqual(['{"type":"ping"}']);

    // Drop connection -> first backoff is 1000ms
    ws1.simulateDrop();
    expect(states.at(-1)).toBe(false);

    vi.advanceTimersByTime(999);
    expect(MockWebSocket.instances).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(MockWebSocket.instances).toHaveLength(2);

    // Drop second attempt before opening -> second backoff is 2000ms
    MockWebSocket.instances[1].simulateDrop();
    vi.advanceTimersByTime(1999);
    expect(MockWebSocket.instances).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(MockWebSocket.instances).toHaveLength(3);

    client.disconnect();
    vi.advanceTimersByTime(60_000);
    expect(MockWebSocket.instances).toHaveLength(3);
  });
});

describe('useAppStore.ts', () => {
  it('bootstraps uninitialized server into SetupWizard state', async () => {
    const authStatus: AuthStatusResponse = {
      initialized: false,
      authenticated: false,
      server_gate_passed: false,
      desktop_pin_lock: false,
      user: null,
      profiles: null,
    };
    const setupStatus: SetupStatusResponse = {
      initialized: false,
      is_direct_loopback: true,
      hardware: sampleSystemInfo.hardware,
      default_paths: {
        managed_uploads_dir: '/data/uploads',
        thumbnails_dir: '/data/thumbnails',
        cache_dir: '/data/cache',
        backup_dir: '/data/backups',
      },
    };

    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === '/api/auth/status') return jsonResponse(authStatus);
      if (url === '/api/setup/status') return jsonResponse(setupStatus);
      throw new Error(`Unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    await useAppStore.getState().bootstrap();

    const state = useAppStore.getState();
    expect(state.bootstrapping).toBe(false);
    expect(state.initialized).toBe(false);
    expect(state.authenticated).toBe(false);
    expect(state.setupStatus).toEqual(setupStatus);
  });

  it('auto-authenticates in Tauri desktop when desktop_pin_lock is false', async () => {
    (window as unknown as { __TAURI_INTERNALS__: Record<string, unknown> }).__TAURI_INTERNALS__ = {};
    tauriMocks.invoke.mockResolvedValue({
      api_base_url: 'http://127.0.0.1:38420',
      desktop_bootstrap_token: 'secret-token',
      port: 38420,
      mdns_url: 'http://truongphoto.local:38420',
      version: '0.1.0',
      platform: 'macos',
    });

    const authStatus: AuthStatusResponse = {
      initialized: true,
      authenticated: false,
      server_gate_passed: false,
      desktop_pin_lock: false,
      user: null,
      profiles: null,
    };

    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === '/api/auth/status') return jsonResponse(authStatus);
      if (url === '/api/auth/desktop-bootstrap') {
        expect(JSON.parse(String(init?.body))).toEqual({ token: 'secret-token' });
        return jsonResponse({ user: sampleAdmin, session_expires_at_ms: 9999999999999 });
      }
      if (url === '/api/auth/me') return jsonResponse({ user: sampleAdmin, allowed_folder_ids: null });
      if (url === '/api/system/info') return jsonResponse(sampleSystemInfo);
      if (url === '/api/folders') return jsonResponse({ items: sampleFolders });
      if (url === '/api/admin/users') return jsonResponse({ items: sampleAdminUsers });
      if (url === '/api/admin/settings') return jsonResponse(sampleSettings);
      throw new Error(`Unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    await useAppStore.getState().bootstrap();

    const state = useAppStore.getState();
    expect(tauriMocks.invoke).toHaveBeenCalledWith('get_desktop_bootstrap', undefined);
    expect(state.bootstrapping).toBe(false);
    expect(state.initialized).toBe(true);
    expect(state.authenticated).toBe(true);
    expect(state.user).toEqual(sampleAdmin);
    expect(state.folders).toEqual(sampleFolders);
    expect(state.adminUsers).toEqual(sampleAdminUsers);
    expect(state.adminSettings).toEqual(sampleSettings);
    expect(MockWebSocket.instances).toHaveLength(1);
  });

  it('skips auto-auth in Tauri when desktop_pin_lock is true', async () => {
    (window as unknown as { __TAURI_INTERNALS__: Record<string, unknown> }).__TAURI_INTERNALS__ = {};

    const authStatus: AuthStatusResponse = {
      initialized: true,
      authenticated: false,
      server_gate_passed: false,
      desktop_pin_lock: true,
      user: null,
      profiles: null,
    };

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/api/auth/status') return jsonResponse(authStatus);
        throw new Error(`Unexpected fetch: ${String(input)}`);
      }),
    );

    await useAppStore.getState().bootstrap();

    const state = useAppStore.getState();
    expect(tauriMocks.invoke).not.toHaveBeenCalled();
    expect(state.authenticated).toBe(false);
    expect(state.desktopPinLock).toBe(true);
  });

  it('loads viewer session without fetching admin endpoints', async () => {
    const authStatus: AuthStatusResponse = {
      initialized: true,
      authenticated: true,
      server_gate_passed: true,
      desktop_pin_lock: false,
      user: sampleViewer,
      profiles: null,
    };

    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === '/api/auth/status') return jsonResponse(authStatus);
      if (url === '/api/auth/me') {
        return jsonResponse({ user: sampleViewer, allowed_folder_ids: ['f-1'] });
      }
      if (url === '/api/system/info') return jsonResponse(sampleSystemInfo);
      if (url === '/api/folders') return jsonResponse({ items: sampleFolders });
      throw new Error(`Unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    await useAppStore.getState().bootstrap();

    const state = useAppStore.getState();
    expect(state.authenticated).toBe(true);
    expect(state.user).toEqual(sampleViewer);
    expect(state.allowedFolderIds).toEqual(['f-1']);
    expect(state.adminUsers).toEqual([]);
    expect(state.adminSettings).toBeNull();
  });

  it('updates liveMetrics, powerState, and settings from WebSocket events and handles 401 session expiration', async () => {
    const authStatus: AuthStatusResponse = {
      initialized: true,
      authenticated: true,
      server_gate_passed: true,
      desktop_pin_lock: false,
      user: sampleAdmin,
      profiles: null,
    };

    let expireSession = false;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (expireSession) {
        return jsonResponse(
          { error: { code: 'UNAUTHORIZED', message: 'Session expired', details: null } },
          401,
        );
      }
      if (url === '/api/auth/status') return jsonResponse(authStatus);
      if (url === '/api/auth/me') return jsonResponse({ user: sampleAdmin, allowed_folder_ids: null });
      if (url === '/api/system/info') return jsonResponse(sampleSystemInfo);
      if (url === '/api/folders') return jsonResponse({ items: sampleFolders });
      if (url === '/api/admin/users') return jsonResponse({ items: sampleAdminUsers });
      if (url === '/api/admin/settings') return jsonResponse(sampleSettings);
      throw new Error(`Unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    await useAppStore.getState().bootstrap();
    const ws = MockWebSocket.instances[0];
    ws.simulateOpen();

    ws.simulateMessage({
      type: 'heartbeat',
      uptime_secs: 120,
      process_ram_bytes: 50 * MB,
      db_size_bytes: 204800,
      active_jobs: 2,
    });

    expect(useAppStore.getState().liveMetrics).toMatchObject({
      uptime_secs: 120,
      process_ram_bytes: 50 * MB,
      db_size_bytes: 204800,
      active_jobs: 2,
    });
    expect(useAppStore.getState().systemInfo?.stats.db_size_bytes).toBe(204800);

    ws.simulateMessage({
      type: 'power_state_changed',
      power: { sleeping: false, display_asleep: true, session_locked: false, on_battery: true },
    });
    expect(useAppStore.getState().powerState).toEqual({
      sleeping: false,
      display_asleep: true,
      session_locked: false,
      on_battery: true,
    });
    expect(useAppStore.getState().systemInfo?.power.on_battery).toBe(true);

    ws.simulateMessage({
      type: 'settings_changed',
      settings: { ...sampleSettings, desktop_pin_lock: true, mdns_enabled: false },
    });
    expect(useAppStore.getState().desktopPinLock).toBe(true);
    expect(useAppStore.getState().systemInfo?.mdns_enabled).toBe(false);

    // Simulate session expiration on an authenticated refresh
    expireSession = true;
    await expect(useAppStore.getState().refreshFolders()).rejects.toBeInstanceOf(ApiClientError);

    const afterExpire = useAppStore.getState();
    expect(afterExpire.authenticated).toBe(false);
    expect(afterExpire.user).toBeNull();
    expect(afterExpire.folders).toEqual([]);
    expect(afterExpire.wsConnected).toBe(false);
  });
});
