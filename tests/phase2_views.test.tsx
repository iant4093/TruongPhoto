// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from '../src/App';
import { AiModelsPanel } from '../src/components/AiModelsPanel';
import { FeedbackHost } from '../src/components/Feedback';
import { Lightbox } from '../src/components/Lightbox';
import { SearchResultsView } from '../src/components/SearchResultsView';
import { PeopleView } from '../src/components/views/PeopleView';
import { SettingsView } from '../src/components/views/SettingsView';
import { GB, MB } from '../src/lib/format';
import { useFeedback } from '../src/store/feedback';
import { useAppStore } from '../src/store/useAppStore';
import type {
  AiIndexStateView,
  AiModelsDashboardResponse,
  AssetDetail,
  AssetSummary,
  AuthStatusResponse,
  FolderView,
  PersonSummary,
  SearchResponse,
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
    total_ram_bytes: 36 * GB,
    cpu_cores: 12,
    recommended_vision_tier: 'tier3',
    recommended_chat_tier: 'tier3',
  },
  stats: { asset_count: 2, folder_count: 1, db_size_bytes: 65536 },
  power: { sleeping: false, display_asleep: false, session_locked: false, on_battery: false },
};

const videoAsset: AssetSummary = {
  id: 'asset-vid-24',
  folder_id: 'f-1',
  kind: 'video',
  capture_at_ms: 1763500000000,
  width: 1920,
  height: 1080,
  duration_ms: 62000,
  aspect_ratio: 16 / 9,
  is_favorite: true,
  deleted_at_ms: null,
  thumb_rel_path: null,
  thumb_ready: true,
  title: 'Beach Sunset Clip',
  rating: 5,
  gps_lat: null,
  gps_lon: null,
  geo_city: 'Malibu',
  geo_country: 'USA',
  camera_model: 'iPhone 16 Pro',
};

const ocrPhotoAsset: AssetSummary = {
  id: 'asset-ocr-1',
  folder_id: 'f-1',
  kind: 'photo',
  capture_at_ms: 1763400000000,
  width: 3000,
  height: 4000,
  duration_ms: null,
  aspect_ratio: 3 / 4,
  is_favorite: false,
  deleted_at_ms: null,
  thumb_rel_path: null,
  thumb_ready: true,
  title: 'Coffee Receipt',
  rating: null,
  gps_lat: null,
  gps_lon: null,
  geo_city: 'Seattle',
  geo_country: 'USA',
  camera_model: 'X-T5',
};

const sampleSearchResponse: SearchResponse = {
  items: [
    {
      asset: videoAsset,
      score: 0.89,
      timestamp_sec: 24,
      match_reasons: ['Semantic 89%', 'Tag: sunset'],
      ocr_snippet: null,
    },
    {
      asset: ocrPhotoAsset,
      score: 0.82,
      timestamp_sec: null,
      match_reasons: ['OCR: RECEIPT'],
      ocr_snippet: 'BLUE BOTTLE COFFEE TOTAL $6.50',
    },
  ],
  total_count: 2,
  planner_mode: 'exact_simd',
  elapsed_ms: 4,
};

const samplePeople: PersonSummary[] = [
  {
    id: 'p-alice',
    name: 'Alice',
    is_hidden: false,
    is_pinned: true,
    cover_face_id: 'face-1',
    face_count: 14,
    created_at_ms: 1700000000000,
    updated_at_ms: 1700000000000,
  },
  {
    id: 'p-unnamed',
    name: null,
    is_hidden: false,
    is_pinned: false,
    cover_face_id: null,
    face_count: 3,
    created_at_ms: 1700000000000,
    updated_at_ms: 1700000000000,
  },
];

const sampleAiDashboard: AiModelsDashboardResponse = {
  ort_available: true,
  ort_ep: 'CoreML ANE',
  hardware: {
    total_ram_bytes: 36 * GB,
    cpu_cores: 12,
    recommended_vision_tier: 'tier3',
    recommended_chat_tier: 'tier3',
  },
  state: {
    active_vision_tier: 'tier2',
    staging_vision_tier: null,
    paused: false,
    idle_unload_secs: 300,
    updated_at_ms: 1700000000000,
  },
  models: [
    {
      id: 'siglip2-base',
      name: 'SigLIP2 Base',
      role: 'vision_tier1',
      license: 'Apache-2.0',
      hf_repo: 'onnx-community/siglip2-base-patch16-224-ONNX',
      tier: 'tier1',
      approx_bytes: 400 * MB,
      status: 'not_installed',
      bytes_total: 400 * MB,
      bytes_downloaded: 0,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'siglip2-large',
      name: 'SigLIP2 Large',
      role: 'vision_tier2',
      license: 'Apache-2.0',
      hf_repo: 'onnx-community/siglip2-large-patch16-256-ONNX',
      tier: 'tier2',
      approx_bytes: 1200 * MB,
      status: 'installed',
      bytes_total: 1200 * MB,
      bytes_downloaded: 1200 * MB,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'siglip2-so400m',
      name: 'SigLIP2 SO400M',
      role: 'vision_tier3',
      license: 'Apache-2.0',
      hf_repo: 'onnx-community/siglip2-so400m-patch14-384-ONNX',
      tier: 'tier3',
      approx_bytes: 1800 * MB,
      status: 'not_installed',
      bytes_total: 1800 * MB,
      bytes_downloaded: 0,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'yunet',
      name: 'YuNet Face Detector',
      role: 'face_det',
      license: 'MIT',
      hf_repo: 'opencv/face_detection_yunet',
      tier: null,
      approx_bytes: 230_000,
      status: 'installed',
      bytes_total: 230_000,
      bytes_downloaded: 230_000,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'auraface-v1',
      name: 'AuraFace v1',
      role: 'face_rec',
      license: 'Apache-2.0',
      hf_repo: 'fal/AuraFace-v1',
      tier: null,
      approx_bytes: 260 * MB,
      status: 'downloading',
      bytes_total: 260 * MB,
      bytes_downloaded: 130 * MB,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'pp-ocrv6-det',
      name: 'PP-OCRv6 Detection',
      role: 'ocr_det',
      license: 'Apache-2.0',
      hf_repo: 'PaddlePaddle/PP-OCRv6_small_det_onnx',
      tier: null,
      approx_bytes: 5 * MB,
      status: 'not_installed',
      bytes_total: 5 * MB,
      bytes_downloaded: 0,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'pp-ocrv6-rec',
      name: 'PP-OCRv6 Recognition',
      role: 'ocr_rec',
      license: 'Apache-2.0',
      hf_repo: 'PaddlePaddle/PP-OCRv6_small_rec_onnx',
      tier: null,
      approx_bytes: 12 * MB,
      status: 'not_installed',
      bytes_total: 12 * MB,
      bytes_downloaded: 0,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
  ],
  queue: {
    pending_assets: 5,
    indexed_assets: 95,
    staging_remaining: 0,
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

describe('SearchResultsView & SearchFilterBar', () => {
  it('renders planner telemetry pill, semantic/OCR badges, video timestamp badge, and opens Lightbox at timestampSec = 24', async () => {
    const searchCalls: Array<Record<string, unknown>> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url === '/api/people') return jsonResponse({ items: samplePeople });
        if (url === '/api/search') {
          searchCalls.push(JSON.parse(String(init?.body ?? '{}')) as Record<string, unknown>);
          return jsonResponse(sampleSearchResponse);
        }
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleAdmin,
      searchActive: true,
      searchQuery: 'sunset',
      searchResponse: sampleSearchResponse,
    });

    render(<SearchResultsView />);

    expect(screen.getByTestId('search-telemetry-pill').textContent).toBe(
      '2 results · Exact SIMD · 4 ms',
    );
    expect(screen.getByText('Semantic 89%')).toBeTruthy();
    expect(screen.getByText('OCR: RECEIPT')).toBeTruthy();
    expect(screen.getByText('BLUE BOTTLE COFFEE TOTAL $6.50')).toBeTruthy();
    expect(screen.getByText('0:24')).toBeTruthy();

    // Clicking the video hit opens Lightbox with initialTimestampSec = 24
    fireEvent.click(screen.getByRole('button', { name: /Open Beach Sunset Clip/i }));
    expect(useAppStore.getState().lightboxAssetId).toBe('asset-vid-24');
    expect(useAppStore.getState().lightboxAssetIds).toEqual(['asset-vid-24', 'asset-ocr-1']);
    expect(useAppStore.getState().lightboxInitialTimestampSec).toBe(24);

    // Switching mode to Semantic and toggling Favorites only triggers runSearch
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Semantic' }));
    });
    expect(searchCalls[0]?.mode).toBe('semantic');

    await act(async () => {
      fireEvent.click(screen.getByLabelText('Favorites only'));
    });
    expect(searchCalls[1]?.favorite).toBe(true);

    fireEvent.click(screen.getByRole('button', { name: /Clear Search/i }));
    expect(useAppStore.getState().searchActive).toBe(false);
    expect(useAppStore.getState().searchResponse).toBeNull();
  });

  it('wires App Omnibar Enter/Clear and passes lightboxInitialTimestampSec to VideoPlayer', async () => {
    const videoDetail: AssetDetail = {
      summary: videoAsset,
      files: [],
      photo_meta: null,
      video_meta: {
        duration_ms: 62000,
        video_codec: 'h264',
        audio_codec: 'aac',
        fps: 30,
        bitrate_bps: 4_000_000,
        bit_depth: 8,
        color_transfer: null,
        color_primaries: null,
        is_hdr: false,
        rotation: 0,
        gps_lat: null,
        gps_lon: null,
        geo_city: null,
        geo_admin1: null,
        geo_country: null,
        geo_country_code: null,
        taken_at_ms: 1763500000000,
      },
      overrides: null,
      tags: [],
      histogram: null,
      album_ids: [],
      playback: {
        mode: 'direct',
        direct_url: '/api/media/asset-vid-24/original',
        hls_url: null,
        live_video_url: null,
        has_raw: false,
        preview_url: '/api/media/asset-vid-24/preview',
        thumb_url: '/api/media/asset-vid-24/thumb',
        original_url: '/api/media/asset-vid-24/original',
      },
    };

    const authStatus: AuthStatusResponse = {
      initialized: true,
      authenticated: true,
      server_gate_passed: true,
      desktop_pin_lock: false,
      user: sampleAdmin,
      profiles: null,
    };

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === '/api/auth/status') return jsonResponse(authStatus);
        if (url === '/api/auth/me') return jsonResponse({ user: sampleAdmin, allowed_folder_ids: null });
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
        if (url.startsWith('/api/assets/asset-vid-24')) return jsonResponse(videoDetail);
        if (url.startsWith('/api/assets')) {
          return jsonResponse({ items: [], next_cursor: null, total_count: 0 });
        }
        if (url === '/api/people') return jsonResponse({ items: samplePeople });
        if (url === '/api/search') return jsonResponse(sampleSearchResponse);
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    const { container, unmount } = render(<App />);
    const input = await screen.findByPlaceholderText('Search library…');
    fireEvent.change(input, { target: { value: 'sunset' } });
    await act(async () => {
      fireEvent.submit(input.closest('form')!);
    });

    expect(await screen.findByTestId('search-telemetry-pill')).toBeTruthy();

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Open Beach Sunset Clip/i }));
    });

    await waitFor(() => {
      expect(container.querySelector('video')).not.toBeNull();
    });
    const videoEl = container.querySelector('video')!;
    videoEl.dispatchEvent(new Event('loadedmetadata'));
    expect(videoEl.currentTime).toBe(24);

    // Close lightbox and exit search via Escape
    useAppStore.getState().closeLightbox();
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(useAppStore.getState().searchActive).toBe(false);
    unmount();

    // Lightbox standalone check: verify Lightbox passes initialTimestampSec
    useAppStore.getState().openLightbox('asset-vid-24', ['asset-vid-24'], 12);
    const lb = render(<Lightbox />);
    await waitFor(() => {
      expect(lb.container.querySelector('video')).not.toBeNull();
    });
    const standaloneVideo = lb.container.querySelector('video')!;
    standaloneVideo.dispatchEvent(new Event('loadedmetadata'));
    expect(standaloneVideo.currentTime).toBe(12);
    lb.unmount();
  });
});

describe('PeopleView RBAC & Interactions', () => {
  it('hides Admin controls (Rename, Merge People, Run Face Clustering) for viewer and contributor', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === '/api/people') return jsonResponse({ items: samplePeople });
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    for (const nonAdmin of [sampleViewer, sampleContributor]) {
      useAppStore.setState({ initialized: true, authenticated: true, user: nonAdmin });
      const view = render(<PeopleView />);
      await screen.findByText('Alice');
      expect(screen.getByText('Unnamed')).toBeTruthy();
      expect(screen.getByText('14 faces')).toBeTruthy();
      expect(screen.queryByRole('button', { name: /Rename/i })).toBeNull();
      expect(screen.queryByRole('button', { name: /Merge People/i })).toBeNull();
      expect(screen.queryByRole('button', { name: /Run Face Clustering/i })).toBeNull();
      expect(screen.queryByLabelText('Show Hidden')).toBeNull();
      view.unmount();
    }
  });

  it('renders Admin controls, opens Person Detail timeline, and runs rename/merge/cluster for admin', async () => {
    const calls: Array<{ url: string; method: string; body?: unknown }> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        const body = init?.body ? JSON.parse(String(init.body)) : undefined;
        calls.push({ url, method, body });

        if (url.startsWith('/api/people?include_hidden=true') || url === '/api/people') {
          return jsonResponse({ items: samplePeople });
        }
        if (url === '/api/people/p-alice' && method === 'GET') {
          return jsonResponse(samplePeople[0]);
        }
        if (url.startsWith('/api/people/p-alice/assets')) {
          return jsonResponse({ items: [ocrPhotoAsset], next_cursor: null, total_count: 1 });
        }
        if (url === '/api/people/p-alice' && method === 'PATCH') {
          return jsonResponse({ ...samplePeople[0], name: 'Alice T.' });
        }
        if (url === '/api/people/merge') {
          return jsonResponse({ person: { ...samplePeople[0], face_count: 17 }, merged_faces: 3 });
        }
        if (url === '/api/people/cluster') {
          return jsonResponse({ clusters_created: 1, faces_assigned: 4 }, 202);
        }
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    useAppStore.setState({ initialized: true, authenticated: true, user: sampleAdmin });
    render(
      <>
        <FeedbackHost />
        <PeopleView />
      </>,
    );

    await screen.findByText('Alice');
    expect(screen.getByRole('button', { name: /Merge People/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /Run Face Clustering/i })).toBeTruthy();
    expect(screen.getByLabelText('Show Hidden')).toBeTruthy();
    expect(screen.getAllByRole('button', { name: /Rename/i }).length).toBeGreaterThan(0);

    // Run Face Clustering
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Run Face Clustering/i }));
    });
    expect(calls.some((c) => c.url === '/api/people/cluster' && c.method === 'POST')).toBe(true);

    // Merge People multi-select
    fireEvent.click(screen.getByRole('button', { name: /Merge People/i }));
    fireEvent.click(screen.getByRole('button', { name: 'Alice' }));
    fireEvent.click(screen.getByRole('button', { name: 'Unnamed' }));
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Merge Selected/i }));
    });
    expect(calls.some((c) => c.url === '/api/people/merge' && c.method === 'POST')).toBe(true);

    // Open Person Detail and inline rename
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Alice' }));
    });
    await screen.findByRole('button', { name: /Back to People/i });
    const nameInput = screen.getByLabelText('Person name');
    fireEvent.change(nameInput, { target: { value: 'Alice T.' } });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    });
    expect(calls.some((c) => c.url === '/api/people/p-alice' && c.method === 'PATCH')).toBe(true);
    expect(screen.getByText('Alice T.')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: /Back to People/i }));
    await screen.findByRole('button', { name: /Merge People/i });
  });
});

describe('AiModelsPanel & SettingsView RBAC', () => {
  it('renders hardware recommendation, ONNX EP status, all model cards, tier switcher, and pause/resume toggle', async () => {
    let paused = false;
    const calls: Array<{ url: string; method: string; body?: unknown }> = [];

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        const body = init?.body ? JSON.parse(String(init.body)) : undefined;
        calls.push({ url, method, body });

        if (url === '/api/admin/ai/models' && method === 'GET') {
          return jsonResponse({
            ...sampleAiDashboard,
            state: { ...sampleAiDashboard.state, paused },
          });
        }
        if (url === '/api/admin/ai/tier' && method === 'POST') {
          return new Response(null, { status: 202 });
        }
        if (url === '/api/admin/ai/pause' && method === 'POST') {
          paused = true;
          const nextState: AiIndexStateView = { ...sampleAiDashboard.state, paused: true };
          return jsonResponse(nextState);
        }
        if (url === '/api/admin/ai/resume' && method === 'POST') {
          paused = false;
          const nextState: AiIndexStateView = { ...sampleAiDashboard.state, paused: false };
          return jsonResponse(nextState);
        }
        if (url === '/api/admin/ai/models/siglip2-base/download' && method === 'POST') {
          return new Response(null, { status: 202 });
        }
        if (url === '/api/admin/ai/models/auraface-v1/cancel' && method === 'POST') {
          return new Response(null, { status: 200 });
        }
        if (url === '/api/admin/ai/models/siglip2-large' && method === 'DELETE') {
          return new Response(null, { status: 204 });
        }
        if (url === '/api/admin/ai/settings' && method === 'PATCH') {
          return jsonResponse({ ...sampleAiDashboard.state, idle_unload_secs: 900 });
        }
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    useAppStore.setState({ initialized: true, authenticated: true, user: sampleAdmin });
    const { container } = render(
      <>
        <FeedbackHost />
        <AiModelsPanel />
      </>,
    );

    await screen.findByText(/36\.00 GB RAM/i);
    expect(screen.getByText(/Tier 3 recommended/i)).toBeTruthy();
    expect(screen.getByTestId('ai-ep-badge').textContent).toBe('CoreML ANE');

    // Verify model cards render
    for (const modelId of [
      'siglip2-base',
      'siglip2-large',
      'siglip2-so400m',
      'yunet',
      'auraface-v1',
      'pp-ocrv6-det',
      'pp-ocrv6-rec',
    ]) {
      expect(container.querySelector(`[data-model-id="${modelId}"]`)).not.toBeNull();
    }

    // Tier switcher
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Tier 3 · SO400M' }));
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/ai/tier' &&
          c.method === 'POST' &&
          (c.body as { tier?: string })?.tier === 'tier3',
      ),
    ).toBe(true);

    // Pause -> Resume toggle
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Pause Indexing/i }));
    });
    expect(calls.some((c) => c.url === '/api/admin/ai/pause' && c.method === 'POST')).toBe(true);

    const resumeBtn = await screen.findByRole('button', { name: /Resume Indexing/i });
    await act(async () => {
      fireEvent.click(resumeBtn);
    });
    expect(calls.some((c) => c.url === '/api/admin/ai/resume' && c.method === 'POST')).toBe(true);

    // Model actions: Download, Cancel, Delete
    const baseCard = container.querySelector('[data-model-id="siglip2-base"]') as HTMLElement;
    await act(async () => {
      fireEvent.click(within(baseCard).getByRole('button', { name: 'Download' }));
    });
    expect(
      calls.some((c) => c.url === '/api/admin/ai/models/siglip2-base/download'),
    ).toBe(true);

    const auraCard = container.querySelector('[data-model-id="auraface-v1"]') as HTMLElement;
    await act(async () => {
      fireEvent.click(within(auraCard).getByRole('button', { name: 'Cancel' }));
    });
    expect(
      calls.some((c) => c.url === '/api/admin/ai/models/auraface-v1/cancel'),
    ).toBe(true);

    const largeCard = container.querySelector('[data-model-id="siglip2-large"]') as HTMLElement;
    await act(async () => {
      fireEvent.click(within(largeCard).getByRole('button', { name: 'Delete' }));
    });
    const confirmBtn = await screen.findByRole('button', { name: 'Remove Model' });
    await act(async () => {
      fireEvent.click(confirmBtn);
    });
    expect(
      calls.some((c) => c.url === '/api/admin/ai/models/siglip2-large' && c.method === 'DELETE'),
    ).toBe(true);

    // Idle unload timeout selector
    await act(async () => {
      fireEvent.click(screen.getByRole('combobox', { name: 'Idle Memory Unload' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('option', { name: '15m' }));
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/ai/settings' &&
          c.method === 'PATCH' &&
          (c.body as { idle_unload_secs?: number })?.idle_unload_secs === 900,
      ),
    ).toBe(true);
  });

  it('hides AiModelsPanel in SettingsView for viewer and contributor', () => {
    for (const nonAdmin of [sampleViewer, sampleContributor]) {
      useAppStore.setState({
        initialized: true,
        authenticated: true,
        user: nonAdmin,
        allowedFolderIds: ['f-1'],
        folders: [sampleFolder],
        systemInfo: sampleSystemInfo,
      });

      const view = render(<SettingsView />);
      expect(screen.queryByText('Local AI & Models')).toBeNull();
      expect(screen.queryByTestId('ai-ep-badge')).toBeNull();
      view.unmount();
    }
  });
});
