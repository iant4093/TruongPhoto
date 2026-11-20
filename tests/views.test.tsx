// @vitest-environment jsdom
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FeedbackHost } from '../src/components/Feedback';
import { MobileTabBar } from '../src/components/MobileTabBar';
import { PinGateView } from '../src/components/views/PinGateView';
import { SettingsView } from '../src/components/views/SettingsView';
import { SetupWizardView } from '../src/components/views/SetupWizardView';
import { GB } from '../src/lib/format';
import { useFeedback } from '../src/store/feedback';
import { useAppStore } from '../src/store/useAppStore';
import type {
  AdminUserView,
  AppSettings,
  BackupResult,
  FolderView,
  ProfileSummary,
  SetupStatusResponse,
  SystemInfoResponse,
  UserPublic,
} from '../src/types/api';

function jsonResponse(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      'Content-Type': 'application/json',
      ...headers,
    },
  });
}

const sampleProfiles: ProfileSummary[] = [
  { id: 'u-admin', display_name: 'Ian', role: 'admin', avatar_color: '#4a4a4a' },
  { id: 'u-viewer', display_name: 'Guest', role: 'viewer', avatar_color: '#556b5d' },
];

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

const sampleSetupStatus: SetupStatusResponse = {
  initialized: false,
  is_direct_loopback: true,
  hardware: {
    total_ram_bytes: 32 * GB,
    cpu_cores: 12,
    recommended_vision_tier: 'tier2',
    recommended_chat_tier: 'tier3',
  },
  default_paths: {
    managed_uploads_dir: '/data/uploads',
    thumbnails_dir: '/data/thumbnails',
    cache_dir: '/data/cache',
    backup_dir: '/data/backups',
  },
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
  hardware: sampleSetupStatus.hardware,
  stats: {
    asset_count: 12,
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
    name: 'Family Photos',
    root_path: '/Users/ian/Pictures',
    volume_uuid: 'VOL-123',
    volume_label: 'Macintosh HD',
    is_managed_uploads: false,
    allow_disk_delete: false,
    is_offline: false,
    last_scanned_at_ms: 1700000000000,
    asset_count: 12,
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
  {
    id: 'u-viewer',
    username: 'guest',
    display_name: 'Guest',
    role: 'viewer',
    avatar_color: '#556b5d',
    disabled_at_ms: null,
    allowed_folder_ids: ['f-1'],
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

class NoopWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readyState = NoopWebSocket.CONNECTING;
  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;
  send(): void {}
  close(): void {
    this.readyState = NoopWebSocket.CLOSED;
  }
}

beforeEach(() => {
  vi.stubGlobal('WebSocket', NoopWebSocket as unknown as typeof WebSocket);
  useAppStore.getState().resetForTest();
  useFeedback.setState({ toasts: [], confirm: null });
});

afterEach(() => {
  useAppStore.getState().resetForTest();
  useFeedback.setState({ toasts: [], confirm: null });
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('PinGateView', () => {
  it('validates numeric PINs and completes the 2-step Server PIN -> Profile PIN flow', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === '/api/auth/server-pin') {
        expect(JSON.parse(String(init?.body))).toEqual({ server_pin: '123456' });
        return jsonResponse({ profiles: sampleProfiles });
      }
      if (url === '/api/auth/login') {
        expect(JSON.parse(String(init?.body))).toEqual({
          user_id: 'u-viewer',
          profile_pin: '4321',
          server_pin: '123456',
        });
        return jsonResponse({ user: sampleViewer, session_expires_at_ms: 9999999999999 });
      }
      if (url === '/api/auth/me') {
        return jsonResponse({ user: sampleViewer, allowed_folder_ids: ['f-1'] });
      }
      if (url === '/api/system/info') return jsonResponse(sampleSystemInfo);
      if (url === '/api/folders') return jsonResponse({ items: sampleFolders });
      throw new Error(`Unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<PinGateView />);

    const serverPinInput = screen.getByLabelText('Server PIN');
    // Invalid non-6-digit PIN should be rejected client-side without calling fetch
    fireEvent.change(serverPinInput, { target: { value: '12ab' } });
    fireEvent.click(screen.getByRole('button', { name: 'Unlock Server' }));
    expect(screen.getByRole('alert').textContent).toContain('6-digit Server PIN');
    expect(fetchMock).not.toHaveBeenCalled();

    // Valid 6-digit Server PIN unlocks Step 2
    fireEvent.change(serverPinInput, { target: { value: '123456' } });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Unlock Server' }));
    });

    expect(screen.getByRole('radio', { name: /Ian/i })).toBeTruthy();
    const guestRadio = screen.getByRole('radio', { name: /Guest/i });
    fireEvent.click(guestRadio);

    const profilePinInput = screen.getByLabelText('Profile PIN');
    // Invalid 3-digit Profile PIN rejected client-side
    fireEvent.change(profilePinInput, { target: { value: '123' } });
    fireEvent.click(screen.getByRole('button', { name: 'Sign In' }));
    expect(screen.getByRole('alert').textContent).toContain('4-digit Profile PIN');

    // Valid 4-digit Profile PIN signs in
    fireEvent.change(profilePinInput, { target: { value: '4321' } });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Sign In' }));
    });

    expect(useAppStore.getState().authenticated).toBe(true);
    expect(useAppStore.getState().user).toEqual(sampleViewer);
  });

  it('renders a live per-second countdown timer when rate-limited (429)', async () => {
    vi.useFakeTimers();

    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === '/api/auth/server-pin') {
        return jsonResponse(
          {
            error: {
              code: 'RATE_LIMITED',
              message: 'Too many failed attempts',
              details: { retry_after_secs: 3 },
            },
          },
          429,
          { 'Retry-After': '3' },
        );
      }
      throw new Error(`Unexpected fetch: ${String(input)}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<PinGateView />);

    fireEvent.change(screen.getByLabelText('Server PIN'), { target: { value: '999999' } });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Unlock Server' }));
    });

    expect(screen.getByTestId('rate-limit-countdown').textContent).toContain('3s');
    const lockedBtn = screen.getByRole('button', { name: /Locked \(3s\)/i }) as HTMLButtonElement;
    expect(lockedBtn.disabled).toBe(true);

    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getByTestId('rate-limit-countdown').textContent).toContain('2s');

    act(() => {
      vi.advanceTimersByTime(2000);
    });
    expect(screen.queryByTestId('rate-limit-countdown')).toBeNull();
    const unlockedBtn = screen.getByRole('button', { name: 'Unlock Server' }) as HTMLButtonElement;
    expect(unlockedBtn.disabled).toBe(false);
  });
});

describe('SetupWizardView', () => {
  it('validates multi-step inputs and submits setup initialization', async () => {
    useAppStore.setState({ setupStatus: sampleSetupStatus, initialized: false });

    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === '/api/setup/initialize') {
        expect(JSON.parse(String(init?.body))).toEqual({
          admin_username: 'ian',
          admin_display_name: 'Ian',
          admin_profile_pin: '1234',
          server_pin: '123456',
          managed_uploads_dir: '/data/uploads',
          thumbnails_dir: '/data/thumbnails',
          cache_dir: '/data/cache',
          backup_dir: '/data/backups',
          initial_folder_path: '/Users/ian/Pictures',
          initial_folder_name: 'Pictures',
        });
        return jsonResponse({ user: sampleAdmin, session_expires_at_ms: 9999999999999 }, 201);
      }
      if (url === '/api/auth/me') return jsonResponse({ user: sampleAdmin, allowed_folder_ids: null });
      if (url === '/api/system/info') return jsonResponse(sampleSystemInfo);
      if (url === '/api/folders') return jsonResponse({ items: sampleFolders });
      if (url === '/api/admin/users') return jsonResponse({ items: sampleAdminUsers });
      if (url === '/api/admin/settings') return jsonResponse(sampleSettings);
      if (url.startsWith('/api/folders/') && url.endsWith('/scan')) {
        return jsonResponse({ job_id: 1, folder_id: 'f-1' }, 202);
      }
      if (url === '/api/admin/ai/tier') {
        return new Response(null, { status: 202 });
      }
      if (url.startsWith('/api/admin/ai/models/') && url.endsWith('/download')) {
        return new Response(null, { status: 202 });
      }
      if (url === '/api/admin/ai/models') {
        return jsonResponse({
          ort_available: true,
          ort_ep: 'CoreML ANE',
          hardware: sampleSetupStatus.hardware,
          state: {
            active_vision_tier: 'tier2',
            staging_vision_tier: null,
            paused: false,
            idle_unload_secs: 300,
            updated_at_ms: 1700000000000,
          },
          models: [],
          queue: { pending_assets: 0, indexed_assets: 0, staging_remaining: 0 },
        });
      }
      throw new Error(`Unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<SetupWizardView />);

    // Step 1: Hardware Tier Review
    expect(screen.getByText(/Hardware Tier Review/i)).toBeTruthy();
    expect(screen.getByText('32.00 GB')).toBeTruthy();
    expect(screen.getByText('tier2')).toBeTruthy();
    expect(screen.getByText('tier3')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));

    // Step 2: Validation check on mismatched PIN
    expect(screen.getByText(/Admin Profile & PINs/i)).toBeTruthy();
    fireEvent.change(screen.getByLabelText('Admin Username'), { target: { value: 'ian' } });
    fireEvent.change(screen.getByLabelText('Display Name'), { target: { value: 'Ian' } });
    fireEvent.change(screen.getByLabelText('4-Digit Profile PIN'), { target: { value: '1234' } });
    fireEvent.change(screen.getByLabelText('Confirm Profile PIN'), { target: { value: '9999' } });
    fireEvent.change(screen.getByLabelText('6-Digit Server PIN'), { target: { value: '123456' } });
    fireEvent.change(screen.getByLabelText('Confirm Server PIN'), { target: { value: '123456' } });
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    expect(screen.getByRole('alert').textContent).toContain('Profile PIN confirmation');

    // Fix PIN and advance to Step 3
    fireEvent.change(screen.getByLabelText('Confirm Profile PIN'), { target: { value: '1234' } });
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));

    // Step 3: Default paths pre-filled + optional watched folder
    expect(screen.getByText(/Storage Paths & Initial Folder/i)).toBeTruthy();
    expect((screen.getByLabelText('Managed Uploads Directory') as HTMLInputElement).value).toBe(
      '/data/uploads',
    );
    fireEvent.change(screen.getByPlaceholderText('/Users/ian/Pictures'), {
      target: { value: '/Users/ian/Pictures' },
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Complete Setup' }));
    });

    expect(useAppStore.getState().authenticated).toBe(true);
    expect(useAppStore.getState().initialized).toBe(true);
    expect(useAppStore.getState().user).toEqual(sampleAdmin);
  });
});

describe('SettingsView RBAC', () => {
  it('hides all Admin sections for viewer and contributor roles', () => {
    for (const nonAdmin of [sampleViewer, sampleContributor]) {
      useAppStore.setState({
        initialized: true,
        authenticated: true,
        user: nonAdmin,
        allowedFolderIds: ['f-1'],
        folders: sampleFolders,
        systemInfo: sampleSystemInfo,
        adminUsers: [],
        adminSettings: null,
      });

      const { unmount } = render(<SettingsView />);
      expect(screen.getByText('Account & Session')).toBeTruthy();
      expect(screen.getByText('Family Photos')).toBeTruthy();
      expect(screen.queryByText('Watched Folders')).toBeNull();
      expect(screen.queryByText('Users & Permissions')).toBeNull();
      expect(screen.queryByText('Network & Desktop')).toBeNull();
      expect(screen.queryByText('Storage & Backups')).toBeNull();
      expect(screen.queryByRole('button', { name: /Back Up Library Now/i })).toBeNull();
      unmount();
    }
  });

  it('renders Watched Folders, Users & Permissions, Network & Desktop, and Storage & Backups for admin', async () => {
    const backupResult: BackupResult = {
      path: '/data/backups/library-20261120.db',
      size_bytes: 204800,
      created_at_ms: 1700000100000,
      pruned_files: 1,
    };

    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === '/api/admin/backup' && init?.method === 'POST') {
        return jsonResponse(backupResult);
      }
      throw new Error(`Unexpected fetch: ${url}`);
    });
    vi.stubGlobal('fetch', fetchMock);

    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleAdmin,
      allowedFolderIds: null,
      folders: sampleFolders,
      systemInfo: sampleSystemInfo,
      adminUsers: sampleAdminUsers,
      adminSettings: sampleSettings,
    });

    render(
      <>
        <FeedbackHost />
        <SettingsView />
      </>,
    );

    expect(screen.getByText('Account & Session')).toBeTruthy();
    expect(screen.getByText('Watched Folders')).toBeTruthy();
    expect(screen.getByText('Users & Permissions')).toBeTruthy();
    expect(screen.getByText('Network & Desktop')).toBeTruthy();
    expect(screen.getByText('Storage & Backups')).toBeTruthy();
    expect(screen.getByText('All folders (Admin)')).toBeTruthy();

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Back Up Library Now/i }));
    });

    expect(screen.getByText(/library-20261120\.db/)).toBeTruthy();
  });
});

describe('MobileTabBar', () => {
  it('renders all 5 primary navigation targets and switches active tab on click', () => {
    useAppStore.setState({ activeNav: 'timeline' });
    render(<MobileTabBar />);

    const expectedTabs = ['Timeline', 'People', 'Albums', 'Map', 'Settings'];
    for (const label of expectedTabs) {
      expect(screen.getByRole('button', { name: label })).toBeTruthy();
    }

    expect(
      screen.getByRole('button', { name: 'Timeline' }).getAttribute('aria-current'),
    ).toBe('page');

    fireEvent.click(screen.getByRole('button', { name: 'Map' }));
    expect(useAppStore.getState().activeNav).toBe('map');
    expect(screen.getByRole('button', { name: 'Map' }).getAttribute('aria-current')).toBe(
      'page',
    );
    expect(
      screen.getByRole('button', { name: 'Timeline' }).getAttribute('aria-current'),
    ).toBeNull();
  });
});
