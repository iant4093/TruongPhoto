// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from '../src/App';
import { AssetInspector } from '../src/components/AssetInspector';
import { FeedbackHost } from '../src/components/Feedback';
import { JustifiedTimeline } from '../src/components/JustifiedTimeline';
import { Lightbox } from '../src/components/Lightbox';
import { TimelineScrubber } from '../src/components/TimelineScrubber';
import { AlbumsView } from '../src/components/views/AlbumsView';
import { TrashView } from '../src/components/views/TrashView';
import { GB } from '../src/lib/format';
import { useFeedback } from '../src/store/feedback';
import { useAppStore } from '../src/store/useAppStore';
import type {
  AlbumDetail,
  AlbumSummary,
  AssetDetail,
  AssetSummary,
  AuthStatusResponse,
  FolderView,
  ShareableUser,
  SystemInfoResponse,
  UserPublic,
} from '../src/types/api';

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

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

const sampleAdmin: UserPublic = {
  id: 'u-admin',
  username: 'ian',
  display_name: 'Ian',
  role: 'admin',
  avatar_color: '#4a4a4a',
};

const sampleContributor: UserPublic = {
  id: 'u-contrib',
  username: 'mom',
  display_name: 'Mom',
  role: 'contributor',
  avatar_color: '#5d556b',
};

const sampleViewer: UserPublic = {
  id: 'u-viewer',
  username: 'guest',
  display_name: 'Guest',
  role: 'viewer',
  avatar_color: '#556b5d',
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
  asset_count: 2,
  created_at_ms: 1700000000000,
};

const sampleSystemInfo: SystemInfoResponse = {
  version: '0.1.0',
  platform: 'macos',
  arch: 'aarch64',
  hostname: 'MacBook.local',
  port: 38420,
  lan_urls: ['http://192.168.1.42:38420'],
  mdns_enabled: false,
  mdns_active: false,
  mdns_url: null,
  desktop_pin_lock: false,
  hardware: {
    total_ram_bytes: 32 * GB,
    cpu_cores: 12,
    recommended_vision_tier: 'tier2',
    recommended_chat_tier: 'tier3',
  },
  stats: { asset_count: 2, folder_count: 1, db_size_bytes: 65536 },
  power: { sleeping: false, display_asleep: false, session_locked: false, on_battery: false },
};

const sampleHistogram = {
  r: Array.from({ length: 256 }, (_, i) => (i * 250) % 65535),
  g: Array.from({ length: 256 }, (_, i) => (i * 180) % 65535),
  b: Array.from({ length: 256 }, (_, i) => (i * 140) % 65535),
  luma: Array.from({ length: 256 }, (_, i) => (i * 200) % 65535),
};

const rawLiveAssetSummary: AssetSummary = {
  id: 'asset-raw-live',
  folder_id: 'f-1',
  kind: 'raw_jpg_stack',
  capture_at_ms: 1763500000000,
  width: 4000,
  height: 3000,
  duration_ms: null,
  aspect_ratio: 4 / 3,
  is_favorite: false,
  deleted_at_ms: null,
  thumb_rel_path: '/api/media/asset-raw-live/thumb',
  thumb_ready: true,
  title: 'Tokyo Dusk',
  rating: 4,
  gps_lat: 35.6762,
  gps_lon: 139.6503,
  geo_city: 'Tokyo',
  geo_country: 'Japan',
  camera_model: 'X-T5',
};

const rawLiveDetail: AssetDetail = {
  summary: rawLiveAssetSummary,
  files: [
    {
      id: 'file-1',
      asset_id: 'asset-raw-live',
      folder_id: 'f-1',
      rel_path: '2026/IMG_001.RAF',
      file_role: 'raw',
      mime_type: 'image/x-fujifilm-raf',
      extension: 'raf',
      file_size: 32_000_000,
      mtime_ms: 1763500000000,
      partial_blake3: 'abc123',
      content_identifier: null,
      embedded_video_offset: null,
      embedded_video_length: null,
      is_missing: false,
    },
    {
      id: 'file-2',
      asset_id: 'asset-raw-live',
      folder_id: 'f-1',
      rel_path: '2026/IMG_001.JPG',
      file_role: 'jpeg_companion',
      mime_type: 'image/jpeg',
      extension: 'jpg',
      file_size: 4_500_000,
      mtime_ms: 1763500000000,
      partial_blake3: 'def456',
      content_identifier: null,
      embedded_video_offset: null,
      embedded_video_length: null,
      is_missing: false,
    },
  ],
  photo_meta: {
    camera_make: 'FUJIFILM',
    camera_model: 'X-T5',
    lens_model: 'XF23mmF1.4 R LM WR',
    focal_length_mm: 23,
    focal_length_35mm: 35,
    aperture_f: 2.0,
    shutter_speed_sec: 1 / 250,
    iso: 160,
    orientation: 1,
    color_space: 'sRGB',
    gps_lat: 35.6762,
    gps_lon: 139.6503,
    gps_alt_m: 40,
    geo_city: 'Tokyo',
    geo_admin1: 'Tokyo',
    geo_country: 'Japan',
    geo_country_code: 'JP',
    taken_at_ms: 1763500000000,
  },
  video_meta: null,
  overrides: {
    title: 'Tokyo Dusk',
    description: 'Sunset over Shibuya',
    capture_at_ms: 1763500000000,
    gps_lat: null,
    gps_lon: null,
    geo_city: 'Tokyo',
    geo_admin1: null,
    geo_country: 'Japan',
    geo_country_code: null,
    rating: 4,
    updated_by: 'u-admin',
    updated_at_ms: 1763500100000,
  },
  tags: [
    { tag: 'tokyo', source: 'user', state: 'active', confidence: null },
    { tag: 'cityscape', source: 'ai', state: 'active', confidence: 0.94 },
  ],
  histogram: sampleHistogram,
  album_ids: [],
  playback: {
    mode: 'image',
    direct_url: null,
    hls_url: null,
    live_video_url: '/api/media/asset-raw-live/live-video',
    has_raw: true,
    preview_url: '/api/media/asset-raw-live/preview',
    thumb_url: '/api/media/asset-raw-live/thumb',
    original_url: '/api/media/asset-raw-live/original',
  },
};

beforeEach(() => {
  vi.stubGlobal('WebSocket', NoopWebSocket as unknown as typeof WebSocket);
  useAppStore.getState().resetForTest();
  useFeedback.setState({ toasts: [], confirm: null });
});

afterEach(() => {
  useAppStore.getState().resetForTest();
  useFeedback.setState({ toasts: [], confirm: null });
  vi.unstubAllGlobals();
});

describe('Lightbox', () => {
  it('renders 3-tier RAW toggle, Live Photo pill, Histogram SVG in Info drawer, and closes on Escape', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === '/api/assets/asset-raw-live') return jsonResponse(rawLiveDetail);
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleAdmin,
      lightboxAssetId: 'asset-raw-live',
      lightboxAssetIds: ['asset-raw-live'],
      selectedAssetId: 'asset-raw-live',
    });

    render(<Lightbox />);

    // Wait for asset detail to load and verify 3-tier RAW toggle + Live Photo pill
    const previewBtn = await screen.findByRole('button', { name: 'Preview' });
    const rawBtn = screen.getByRole('button', { name: 'RAW 100%' });
    const livePill = screen.getByRole('button', { name: /LIVE/i });
    expect(previewBtn).toBeTruthy();
    expect(rawBtn).toBeTruthy();
    expect(livePill).toBeTruthy();

    // Verify switching to RAW 100% keeps base preview mounted (zero flash) while loading mode=raw_demosaic overlay
    const img = screen.getByAltText('Tokyo Dusk') as HTMLImageElement;
    expect(img.getAttribute('src')).toContain('/api/media/asset-raw-live/preview');
    fireEvent.click(rawBtn);
    const rawOverlay = await screen.findByTestId('lightbox-hires-layer');
    expect(rawOverlay.getAttribute('src')).toContain('/api/media/asset-raw-live/original?mode=raw_demosaic');
    expect(img.getAttribute('src')).toContain('/api/media/asset-raw-live/preview');

    // Toggle Info drawer open and verify Histogram SVG mounts
    fireEvent.click(screen.getByRole('button', { name: 'Toggle info' }));
    const histogramSvg = await screen.findByRole('img', { name: 'RGB histogram' });
    expect(histogramSvg.tagName.toLowerCase()).toBe('svg');

    // First Escape closes Info drawer; second Escape closes Lightbox
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('img', { name: 'RGB histogram' })).toBeNull();
    expect(useAppStore.getState().lightboxAssetId).toBe('asset-raw-live');

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(useAppStore.getState().lightboxAssetId).toBeNull();
  });
});

describe('AssetInspector RBAC', () => {
  it('disables and hides Override & Tag editing for viewer and enables it for contributor and admin', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === '/api/assets/asset-raw-live') return jsonResponse(rawLiveDetail);
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    // 1. Viewer: strictly read-only
    useAppStore.setState({ user: sampleViewer, initialized: true, authenticated: true });
    const viewerView = render(<AssetInspector assetId="asset-raw-live" />);
    const viewerTitleInput = (await screen.findByLabelText('Override title')) as HTMLInputElement;
    expect(viewerTitleInput.disabled).toBe(true);
    expect((screen.getByLabelText('Override description') as HTMLTextAreaElement).disabled).toBe(
      true,
    );
    expect(screen.queryByRole('button', { name: 'Save Overrides' })).toBeNull();
    expect(screen.queryByLabelText('Add tag')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Remove tag tokyo' })).toBeNull();
    viewerView.unmount();

    // 2. Contributor & Admin: fully interactive
    for (const editor of [sampleContributor, sampleAdmin]) {
      useAppStore.setState({ user: editor, initialized: true, authenticated: true });
      const view = render(<AssetInspector assetId="asset-raw-live" />);
      const titleInput = (await screen.findByLabelText('Override title')) as HTMLInputElement;
      expect(titleInput.disabled).toBe(false);
      expect(screen.getByRole('button', { name: 'Save Overrides' })).toBeTruthy();
      expect(screen.getByLabelText('Add tag')).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Remove tag tokyo' })).toBeTruthy();
      view.unmount();
    }
  });
});

describe('App Upload button RBAC', () => {
  it('hides Upload button for viewer and shows it for contributor and admin', async () => {
    const setupFetchForUser = (user: UserPublic) => {
      const authStatus: AuthStatusResponse = {
        initialized: true,
        authenticated: true,
        server_gate_passed: true,
        desktop_pin_lock: false,
        user,
        profiles: null,
      };
      vi.stubGlobal(
        'fetch',
        vi.fn(async (input: RequestInfo | URL) => {
          const url = String(input);
          if (url === '/api/auth/status') return jsonResponse(authStatus);
          if (url === '/api/auth/me') {
            return jsonResponse({
              user,
              allowed_folder_ids: user.role === 'admin' ? null : ['f-1'],
            });
          }
          if (url === '/api/system/info') return jsonResponse(sampleSystemInfo);
          if (url === '/api/folders') return jsonResponse({ items: [sampleFolder] });
          if (url === '/api/admin/users') return jsonResponse({ items: [] });
          if (url === '/api/admin/settings') {
            return jsonResponse({
              port: 38420,
              mdns_enabled: false,
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
            });
          }
          if (url.startsWith('/api/assets/buckets')) return jsonResponse({ buckets: [] });
          if (url.startsWith('/api/assets')) {
            return jsonResponse({ items: [], next_cursor: null, total_count: 0 });
          }
          throw new Error(`Unexpected fetch: ${url}`);
        }),
      );
    };

    // Viewer: Upload button MUST be hidden
    setupFetchForUser(sampleViewer);
    const viewerRender = render(<App />);
    await screen.findByText('Sign Out');
    expect(screen.queryByRole('button', { name: 'Upload' })).toBeNull();
    viewerRender.unmount();
    useAppStore.getState().resetForTest();

    // Contributor: Upload button MUST be visible
    setupFetchForUser(sampleContributor);
    const contribRender = render(<App />);
    expect(await screen.findByRole('button', { name: 'Upload' })).toBeTruthy();
    contribRender.unmount();
    useAppStore.getState().resetForTest();

    // Admin: Upload button MUST be visible
    setupFetchForUser(sampleAdmin);
    const adminRender = render(<App />);
    expect(await screen.findByRole('button', { name: 'Upload' })).toBeTruthy();
    adminRender.unmount();
  });
});

describe('TrashView RBAC', () => {
  it('hides Permanently Purge and Empty Trash for contributor and displays them for admin', async () => {
    const trashedAsset: AssetSummary = {
      ...rawLiveAssetSummary,
      id: 'asset-trashed-1',
      deleted_at_ms: Date.now() - 2 * 86_400_000,
    };

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.startsWith('/api/trash')) {
          return jsonResponse({ items: [trashedAsset], total_count: 1 });
        }
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    // Contributor: sees Restore + countdown, NEVER Permanently Purge / Empty Trash
    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleContributor,
      folders: [sampleFolder],
    });
    const contribView = render(<TrashView />);
    await screen.findByText(/days left/i);
    expect(screen.getByRole('button', { name: /Restore/i })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Permanently Purge/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /Empty Trash/i })).toBeNull();
    contribView.unmount();

    // Admin: sees Permanently Purge + Empty Trash
    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleAdmin,
      folders: [sampleFolder],
    });
    render(
      <>
        <FeedbackHost />
        <TrashView />
      </>,
    );
    await screen.findByText(/days left/i);
    expect(screen.getByRole('button', { name: /Permanently Purge/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /Empty Trash/i })).toBeTruthy();
  });
});

describe('AlbumsView CRUD & Share', () => {
  it('creates an album, opens album detail, and opens Share modal cleanly', async () => {
    const albums: AlbumSummary[] = [];
    const shareableUsers: ShareableUser[] = [
      { id: 'u-viewer', display_name: 'Guest', avatar_color: '#556b5d' },
    ];

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url === '/api/albums' && (!init?.method || init.method === 'GET')) {
          return jsonResponse({ items: albums });
        }
        if (url === '/api/albums' && init?.method === 'POST') {
          const body = JSON.parse(String(init.body)) as { title: string; description?: string | null };
          const created: AlbumSummary = {
            id: 'alb-1',
            owner_user_id: sampleContributor.id,
            title: body.title,
            description: body.description ?? null,
            cover_asset_id: null,
            sort_order: 'capture_desc',
            asset_count: 1,
            created_at_ms: 1763500000000,
            updated_at_ms: 1763500000000,
          };
          albums.push(created);
          return jsonResponse({ album: created }, 201);
        }
        if (url === '/api/albums/alb-1' && (!init?.method || init.method === 'GET')) {
          const detail: AlbumDetail = {
            album: albums[0],
            permission: 'owner',
            assets: [rawLiveAssetSummary],
            shares: [],
          };
          return jsonResponse(detail);
        }
        if (url === '/api/users/shareable') {
          return jsonResponse({ items: shareableUsers });
        }
        if (url === '/api/albums/alb-1/shares' && init?.method === 'PUT') {
          return jsonResponse({
            shares: [
              {
                user_id: 'u-viewer',
                display_name: 'Guest',
                avatar_color: '#556b5d',
                permission: 'view',
              },
            ],
          });
        }
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleContributor,
    });

    render(
      <>
        <FeedbackHost />
        <AlbumsView />
      </>,
    );

    // 1. Create Album
    const newAlbumBtns = await screen.findAllByRole('button', { name: /New Album/i });
    fireEvent.click(newAlbumBtns[0]);
    fireEvent.change(screen.getByLabelText('New album title'), {
      target: { value: 'Kyoto Trip' },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Create Album' }));
    });

    // 2. Open Album Detail
    const albumCard = await screen.findByRole('button', { name: /Kyoto Trip/i });
    await act(async () => {
      fireEvent.click(albumCard);
    });

    const shareBtn = await screen.findByRole('button', { name: 'Share' });
    expect(screen.getByText('Back to Albums')).toBeTruthy();

    // 3. Open Share modal and verify shareable user appears
    await act(async () => {
      fireEvent.click(shareBtn);
    });
    expect(await screen.findByRole('dialog', { name: 'Share album' })).toBeTruthy();
    expect(await screen.findByLabelText('Share with Guest')).toBeTruthy();

    await waitFor(() => {
      fireEvent.click(screen.getByLabelText('Share with Guest'));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Save Shares' }));
    });
    expect(screen.queryByRole('dialog', { name: 'Share album' })).toBeNull();
  });
});

describe('JustifiedTimeline & TimelineScrubber', () => {
  it('renders justified tiles with fallback 960px width in jsdom and triggers scrubber jump', () => {
    const onJump = vi.fn();
    useAppStore.setState({ user: sampleAdmin, initialized: true, authenticated: true });

    render(
      <>
        <JustifiedTimeline assets={[rawLiveAssetSummary]} />
        <TimelineScrubber
          buckets={[{ year: 2026, month: 11, count: 142 }]}
          onJumpToMonth={onJump}
        />
      </>,
    );

    expect(screen.getByText('RAW+JPG')).toBeTruthy();
    const scrubber = screen.getByRole('slider', { name: /Timeline chronological scrubber/i });
    fireEvent.pointerMove(scrubber, { clientY: 100 });
    expect(screen.getByText('Nov 2026 · 142')).toBeTruthy();
    fireEvent.pointerUp(scrubber, { clientY: 100 });
    expect(onJump).toHaveBeenCalledWith(2026, 11);
  });
});
