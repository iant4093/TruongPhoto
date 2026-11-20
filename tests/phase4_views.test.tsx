// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FeedbackHost } from '../src/components/Feedback';
import { Sidebar } from '../src/components/Sidebar';
import { SettingsView } from '../src/components/views/SettingsView';
import { GB, MB, formatBytes } from '../src/lib/format';
import { useFeedback } from '../src/store/feedback';
import { useAppStore } from '../src/store/useAppStore';
import { TrayApp } from '../src/tray/TrayApp';
import type {
  AiIndexStateView,
  AiModelsDashboardResponse,
  AppSettings,
  BackupListResponse,
  FolderView,
  SystemInfoResponse,
  TailscaleStatus,
  ThrottleStatus,
  UserPublic,
} from '../src/types/api';

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

class MockWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static instances: MockWebSocket[] = [];

  readyState = MockWebSocket.CONNECTING;
  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;

  constructor(_url: string) {
    MockWebSocket.instances.push(this);
  }

  send(): void {}
  close(): void {
    this.readyState = MockWebSocket.CLOSED;
  }

  simulateOpen(): void {
    this.readyState = MockWebSocket.OPEN;
    this.onopen?.({} as Event);
  }

  simulateMessage(payload: unknown): void {
    this.onmessage?.({ data: JSON.stringify(payload) } as MessageEvent);
  }
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

const sampleContributor: UserPublic = {
  id: 'u-contrib',
  username: 'mom',
  display_name: 'Mom',
  role: 'contributor',
  avatar_color: '#5d556b',
};

const sampleFolder: FolderView = {
  id: 'f-1',
  name: 'Family Photos',
  root_path: '/Users/ian/Pictures',
  volume_uuid: 'VOL-1',
  volume_label: 'Macintosh HD',
  is_managed_uploads: false,
  allow_disk_delete: false,
  is_offline: false,
  last_scanned_at_ms: 1700000000000,
  asset_count: 42,
  created_at_ms: 1700000000000,
};

const batteryThrottle: ThrottleStatus = {
  active: true,
  reason: 'battery',
  cpu_usage_percent: 24.5,
  on_battery: true,
};

const highCpuThrottle: ThrottleStatus = {
  active: true,
  reason: 'high_cpu',
  cpu_usage_percent: 91.2,
  on_battery: false,
};

const sampleSystemInfo: SystemInfoResponse = {
  version: '0.1.0',
  platform: 'macos',
  arch: 'aarch64',
  hostname: 'MacBook.local',
  port: 38420,
  lan_urls: ['http://192.168.1.42:38420'],
  mdns_enabled: true,
  mdns_active: true,
  mdns_url: 'http://truongphoto.local:38420',
  desktop_pin_lock: false,
  hardware: {
    total_ram_bytes: 36 * GB,
    cpu_cores: 12,
    recommended_vision_tier: 'tier3',
    recommended_chat_tier: 'tier3',
  },
  stats: { asset_count: 42, folder_count: 1, db_size_bytes: 2 * MB },
  power: { sleeping: false, display_asleep: false, session_locked: false, on_battery: true },
  throttle: batteryThrottle,
  tailscale_url: 'https://truongphoto.tailnet.ts.net',
};

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
  backup_interval_hours: 24,
  video_max_frames: 30,
  ai_tier_vision: 'tier2',
  ai_tier_chat: 'tier2',
  map_tile_mode: 'offline_pmtiles',
  map_pmtiles_dir: '/data/maps',
  pause_on_battery: true,
  cpu_throttle_percent: 80,
};

const sampleAiDashboard: AiModelsDashboardResponse = {
  ort_available: true,
  ort_ep: 'CoreML ANE',
  hardware: sampleSystemInfo.hardware,
  state: {
    active_vision_tier: 'tier2',
    staging_vision_tier: null,
    paused: false,
    idle_unload_secs: 300,
    updated_at_ms: 1700000000000,
    active_chat_tier: 'tier2',
    vlm_rerank_enabled: false,
  },
  models: [],
  queue: {
    pending_assets: 5,
    indexed_assets: 95,
    staging_remaining: 0,
  },
};

const sampleBackups: BackupListResponse = {
  items: [
    {
      filename: 'library-20261120T100000Z.db',
      path: '/data/backups/library-20261120T100000Z.db',
      size_bytes: 2 * MB,
      created_at_ms: 1700000000000,
    },
    {
      filename: 'library-20261119T100000Z.db',
      path: '/data/backups/library-20261119T100000Z.db',
      size_bytes: 1_500_000,
      created_at_ms: 1699913600000,
    },
  ],
  backup_dir: '/data/backups',
  interval_hours: 24,
  keep_count: 7,
};

beforeEach(() => {
  MockWebSocket.instances = [];
  vi.stubGlobal('WebSocket', MockWebSocket as unknown as typeof WebSocket);
  useAppStore.getState().resetForTest();
  useFeedback.setState({ toasts: [], confirm: null });
});

afterEach(() => {
  useAppStore.getState().resetForTest();
  useFeedback.setState({ toasts: [], confirm: null });
  vi.unstubAllGlobals();
});

describe('TrayApp Phase 4 polish', () => {
  it('renders Tailscale HTTPS URL, toggles inline QR img, displays Throttle badge, and toggles Pause/Resume AI', async () => {
    let aiPaused = false;
    const calls: Array<{ url: string; method: string }> = [];

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        calls.push({ url, method });

        if (url === '/api/system/info') return jsonResponse(sampleSystemInfo);
        if (url === '/api/admin/ai/models') {
          return jsonResponse({
            ...sampleAiDashboard,
            state: { ...sampleAiDashboard.state, paused: aiPaused },
          });
        }
        if (url === '/api/admin/ai/pause' && method === 'POST') {
          aiPaused = true;
          const next: AiIndexStateView = { ...sampleAiDashboard.state, paused: true };
          return jsonResponse(next);
        }
        if (url === '/api/admin/ai/resume' && method === 'POST') {
          aiPaused = false;
          const next: AiIndexStateView = { ...sampleAiDashboard.state, paused: false };
          return jsonResponse(next);
        }
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    const { container } = render(<TrayApp />);

    // Tailscale HTTPS URL prioritized first with HTTPS badge + AI queue readout
    expect(await screen.findByText('https://truongphoto.tailnet.ts.net')).toBeTruthy();
    expect(screen.getByText('HTTPS')).toBeTruthy();
    expect(screen.getByText('95 indexed · 5 pending')).toBeTruthy();

    // Throttle badge rendered from systemInfo.throttle
    expect(screen.getByText('Paused (Battery)')).toBeTruthy();

    // Toggle inline QR panel
    const qrBtn = screen.getByRole('button', { name: 'QR' });
    expect(container.querySelector('img[alt="QR code"]')).toBeNull();
    fireEvent.click(qrBtn);

    const qrImg = container.querySelector('img[alt="QR code"]') as HTMLImageElement | null;
    expect(qrImg).not.toBeNull();
    expect(qrImg?.getAttribute('src')).toBe(
      `/api/system/qr?url=${encodeURIComponent('https://truongphoto.tailnet.ts.net')}`,
    );

    // Pause -> Resume AI button
    const pauseBtn = screen.getByRole('button', { name: 'Pause AI' });
    await act(async () => {
      fireEvent.click(pauseBtn);
    });
    expect(calls.some((c) => c.url === '/api/admin/ai/pause' && c.method === 'POST')).toBe(true);

    const resumeBtn = await screen.findByRole('button', { name: 'Resume AI' });
    await act(async () => {
      fireEvent.click(resumeBtn);
    });
    expect(calls.some((c) => c.url === '/api/admin/ai/resume' && c.method === 'POST')).toBe(true);

    // Live WebSocket throttle_state_changed updates badge to Throttled (High CPU)
    const ws = MockWebSocket.instances[0];
    expect(ws).toBeTruthy();
    act(() => {
      ws.simulateOpen();
      ws.simulateMessage({ type: 'throttle_state_changed', throttle: highCpuThrottle });
    });
    expect(screen.getByText('Throttled (High CPU)')).toBeTruthy();
  });
});

describe('Sidebar Phase 4 throttle pill', () => {
  it('displays .throttle-pill when throttleStatus.active is true and hides it when inactive', () => {
    useAppStore.setState({
      systemInfo: sampleSystemInfo,
      powerState: sampleSystemInfo.power,
      throttleStatus: batteryThrottle,
    });

    const { container } = render(<Sidebar />);
    const pill = container.querySelector('.throttle-pill');
    expect(pill).not.toBeNull();
    expect(pill?.textContent).toBe('Paused · Battery');

    act(() => {
      useAppStore.getState().applyWsEvent({
        type: 'throttle_state_changed',
        throttle: highCpuThrottle,
      });
    });
    expect(container.querySelector('.throttle-pill')?.textContent).toBe('Throttled · CPU');

    act(() => {
      useAppStore.getState().applyWsEvent({
        type: 'throttle_state_changed',
        throttle: { active: false, reason: null, cpu_usage_percent: 12, on_battery: false },
      });
    });
    expect(container.querySelector('.throttle-pill')).toBeNull();
  });
});

describe('SettingsView Phase 4 Tailscale, Backups & Throttling RBAC', () => {
  it('hides Tailscale Remote HTTPS, Backup Schedule/History, and Throttling controls for viewer and contributor', () => {
    for (const nonAdmin of [sampleViewer, sampleContributor]) {
      useAppStore.setState({
        initialized: true,
        authenticated: true,
        user: nonAdmin,
        allowedFolderIds: ['f-1'],
        folders: [sampleFolder],
        systemInfo: sampleSystemInfo,
        adminUsers: [],
        adminSettings: null,
      });

      const view = render(<SettingsView />);
      expect(screen.queryByText('Tailscale Remote HTTPS')).toBeNull();
      expect(screen.queryByText('Installed')).toBeNull();
      expect(screen.queryByLabelText('Backup Interval')).toBeNull();
      expect(screen.queryByText('Backup Snapshot History')).toBeNull();
      expect(screen.queryByText('Power & Thermal Throttling')).toBeNull();
      expect(screen.queryByLabelText('Pause Indexing on Battery')).toBeNull();
      expect(screen.queryByLabelText('CPU Throttle Threshold (%)')).toBeNull();
      expect(screen.queryByAltText('Remote access QR code')).toBeNull();
      view.unmount();
    }
  });

  it('renders Tailscale Remote HTTPS card, remediation, QR code, Backup Schedule + history table, and Throttling controls for admin', async () => {
    let tailscaleState: TailscaleStatus = {
      installed: true,
      running: true,
      logged_in: true,
      cli_path: '/Applications/Tailscale.app/Contents/MacOS/Tailscale',
      magic_dns_name: 'truongphoto.tailnet.ts.net',
      https_certs_enabled: true,
      serve_active: false,
      https_url: null,
      remediation: 'Click Enable Tailscale HTTPS to expose encrypted remote access.',
      last_error: null,
    };
    let currentSettings: AppSettings = { ...sampleSettings };
    const calls: Array<{ url: string; method: string; body?: unknown }> = [];

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        const body = init?.body ? JSON.parse(String(init.body)) : undefined;
        calls.push({ url, method, body });

        if (url === '/api/admin/ai/models') return jsonResponse(sampleAiDashboard);
        if (url === '/api/map/config') {
          return jsonResponse({
            tile_mode: 'offline_pmtiles',
            available_pmtiles: [],
            pmtiles_url: null,
            online_style_url: 'https://tiles.openfreemap.org/styles/dark',
            total_geotagged: 0,
          });
        }
        if (url === '/api/admin/tailscale/status' && method === 'GET') {
          return jsonResponse(tailscaleState);
        }
        if (url === '/api/admin/tailscale/enable' && method === 'POST') {
          tailscaleState = {
            ...tailscaleState,
            serve_active: true,
            https_url: 'https://truongphoto.tailnet.ts.net',
            remediation: null,
          };
          return jsonResponse(tailscaleState);
        }
        if (url === '/api/admin/tailscale/disable' && method === 'POST') {
          tailscaleState = {
            ...tailscaleState,
            serve_active: false,
            https_url: null,
            remediation: null,
          };
          return jsonResponse(tailscaleState);
        }
        if (url === '/api/admin/backups' && method === 'GET') {
          return jsonResponse(sampleBackups);
        }
        if (url === '/api/admin/settings' && method === 'PATCH') {
          currentSettings = { ...currentSettings, ...(body as Partial<AppSettings>) };
          return jsonResponse(currentSettings);
        }
        if (url === '/api/admin/settings' && method === 'GET') {
          return jsonResponse(currentSettings);
        }
        if (url === '/api/admin/users' && method === 'GET') {
          return jsonResponse({ items: [] });
        }
        if (url === '/api/system/info' && method === 'GET') {
          return jsonResponse({
            ...sampleSystemInfo,
            tailscale_url: tailscaleState.serve_active ? tailscaleState.https_url : null,
          });
        }
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleAdmin,
      allowedFolderIds: null,
      folders: [sampleFolder],
      systemInfo: { ...sampleSystemInfo, tailscale_url: null },
      adminUsers: [],
      adminSettings: currentSettings,
    });

    const { container } = render(
      <>
        <FeedbackHost />
        <SettingsView />
      </>,
    );

    // Tailscale status pills + remediation callout + QR image
    expect(await screen.findByText('Tailscale Remote HTTPS')).toBeTruthy();
    expect(await screen.findByText('Installed')).toBeTruthy();
    expect(screen.getByText('Logged In')).toBeTruthy();
    expect(screen.getByText('MagicDNS')).toBeTruthy();
    expect(screen.getByText('HTTPS Certs')).toBeTruthy();
    expect(
      screen.getByText('Click Enable Tailscale HTTPS to expose encrypted remote access.'),
    ).toBeTruthy();

    const qrImgBefore = screen.getByAltText('Remote access QR code') as HTMLImageElement;
    expect(qrImgBefore.getAttribute('src')).toBe(
      `/api/system/qr?url=${encodeURIComponent('http://truongphoto.local:38420')}`,
    );

    // Enable Tailscale HTTPS -> switches button to Disable and updates QR code URL
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Enable Tailscale HTTPS' }));
    });
    expect(
      calls.some((c) => c.url === '/api/admin/tailscale/enable' && c.method === 'POST'),
    ).toBe(true);
    expect(await screen.findByRole('button', { name: 'Disable Tailscale HTTPS' })).toBeTruthy();
    expect(
      (screen.getByAltText('Remote access QR code') as HTMLImageElement).getAttribute('src'),
    ).toBe(`/api/system/qr?url=${encodeURIComponent('https://truongphoto.tailnet.ts.net')}`);

    // Disable Tailscale HTTPS
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Disable Tailscale HTTPS' }));
    });
    expect(
      calls.some((c) => c.url === '/api/admin/tailscale/disable' && c.method === 'POST'),
    ).toBe(true);
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Enable Tailscale HTTPS' })).toBeTruthy();
    });

    // Backup snapshot history table & Backup Interval selector
    const table = container.querySelector('.backup-table') as HTMLElement;
    expect(table).not.toBeNull();
    expect(within(table).getByText('library-20261120T100000Z.db')).toBeTruthy();
    expect(within(table).getByText(formatBytes(2 * MB))).toBeTruthy();
    expect(within(table).getByText('library-20261119T100000Z.db')).toBeTruthy();

    await act(async () => {
      fireEvent.change(screen.getByLabelText('Backup Interval'), { target: { value: '168' } });
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/settings' &&
          c.method === 'PATCH' &&
          (c.body as { backup_interval_hours?: number })?.backup_interval_hours === 168,
      ),
    ).toBe(true);

    // Power & Thermal Throttling controls
    expect(screen.getByText('Power & Thermal Throttling')).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByLabelText(/Pause Indexing on Battery/i));
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/settings' &&
          c.method === 'PATCH' &&
          (c.body as { pause_on_battery?: boolean })?.pause_on_battery === false,
      ),
    ).toBe(true);

    const cpuInput = screen.getByLabelText('CPU Throttle Threshold (%)');
    fireEvent.change(cpuInput, { target: { value: '65' } });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Apply Threshold' }));
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/settings' &&
          c.method === 'PATCH' &&
          (c.body as { cpu_throttle_percent?: number })?.cpu_throttle_percent === 65,
      ),
    ).toBe(true);
  });
});
