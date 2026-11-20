// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from '../src/App';
import { AiModelsPanel } from '../src/components/AiModelsPanel';
import { ChatDrawer } from '../src/components/ChatDrawer';
import { FeedbackHost } from '../src/components/Feedback';
import { MapView, buildMapStyle, hasWebGLSupport } from '../src/components/views/MapView';
import { SettingsView } from '../src/components/views/SettingsView';
import { GB, MB } from '../src/lib/format';
import { useFeedback } from '../src/store/feedback';
import { useAppStore } from '../src/store/useAppStore';
import type {
  AiIndexStateView,
  AiModelsDashboardResponse,
  AppSettings,
  AssetSummary,
  AuthStatusResponse,
  ChatSessionSummary,
  FolderView,
  MapCluster,
  MapConfigResponse,
  RouterPlan,
  SearchHit,
  SendChatMessageResponse,
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
  asset_count: 14,
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
  stats: { asset_count: 14, folder_count: 1, db_size_bytes: 65536 },
  power: { sleeping: false, display_asleep: false, session_locked: false, on_battery: false },
};

const sampleSettings: AppSettings = {
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
  map_tile_mode: 'offline_pmtiles',
  map_pmtiles_dir: '/data/maps',
};

const tokyoVideoAsset: AssetSummary = {
  id: 'asset-tokyo-vid',
  folder_id: 'f-1',
  kind: 'video',
  capture_at_ms: 1763500000000,
  width: 1920,
  height: 1080,
  duration_ms: 60_000,
  aspect_ratio: 16 / 9,
  is_favorite: true,
  deleted_at_ms: null,
  thumb_rel_path: null,
  thumb_ready: true,
  title: 'Shibuya Rain Clip',
  rating: 5,
  gps_lat: 35.6595,
  gps_lon: 139.7004,
  geo_city: 'Tokyo',
  geo_country: 'Japan',
  camera_model: 'X100VI',
};

const tokyoPhotoAsset: AssetSummary = {
  id: 'asset-tokyo-photo',
  folder_id: 'f-1',
  kind: 'photo',
  capture_at_ms: 1763400000000,
  width: 4000,
  height: 3000,
  duration_ms: null,
  aspect_ratio: 4 / 3,
  is_favorite: false,
  deleted_at_ms: null,
  thumb_rel_path: null,
  thumb_ready: true,
  title: 'Tokyo Ramen Receipt',
  rating: null,
  gps_lat: 35.6612,
  gps_lon: 139.7041,
  geo_city: 'Tokyo',
  geo_country: 'Japan',
  camera_model: 'X100VI',
};

const phase3Dashboard: AiModelsDashboardResponse = {
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
    active_chat_tier: 'tier2',
    vlm_rerank_enabled: false,
  },
  models: [
    {
      id: 'siglip2-large',
      name: 'SigLIP2 Large (Tier 2)',
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
      id: 'gemma4-e2b',
      name: 'Gemma 4 E2B Chat (Tier 1)',
      role: 'chat_tier1',
      license: 'Apache-2.0',
      hf_repo: 'ggml-org/gemma-4-E2B-it-GGUF',
      tier: 'tier1',
      approx_bytes: 1600 * MB,
      status: 'not_installed',
      bytes_total: 1600 * MB,
      bytes_downloaded: 0,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'gemma4-e4b',
      name: 'Gemma 4 E4B Chat (Tier 2 Default)',
      role: 'chat_tier2',
      license: 'Apache-2.0',
      hf_repo: 'ggml-org/gemma-4-E4B-it-GGUF',
      tier: 'tier2',
      approx_bytes: 2800 * MB,
      status: 'installed',
      bytes_total: 2800 * MB,
      bytes_downloaded: 2800 * MB,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'qwen3.5-4b',
      name: 'Qwen 3.5 4B Chat (Tier 2 Alt)',
      role: 'chat_tier2_alt',
      license: 'Apache-2.0',
      hf_repo: 'ggml-org/Qwen3.5-4B-GGUF',
      tier: 'tier2',
      approx_bytes: 2700 * MB,
      status: 'not_installed',
      bytes_total: 2700 * MB,
      bytes_downloaded: 0,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'gemma4-26b-a4b',
      name: 'Gemma 4 26B-A4B Chat + VLM (Tier 3)',
      role: 'chat_tier3',
      license: 'Apache-2.0',
      hf_repo: 'ggml-org/gemma-4-26B-A4B-it-GGUF',
      tier: 'tier3',
      approx_bytes: 15100 * MB,
      status: 'not_installed',
      bytes_total: 15100 * MB,
      bytes_downloaded: 0,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
    {
      id: 'world-basemap-pmtiles',
      name: 'Offline World Basemap (PMTiles)',
      role: 'map_basemap',
      license: 'ODbL/BSD',
      hf_repo: 'Protomaps / OpenStreetMap',
      tier: null,
      approx_bytes: 35 * MB,
      status: 'not_installed',
      bytes_total: 35 * MB,
      bytes_downloaded: 0,
      error_message: null,
      updated_at_ms: 1700000000000,
    },
  ],
  queue: {
    pending_assets: 0,
    indexed_assets: 14,
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

describe('ChatDrawer & App Ask AI shell integration', () => {
  it('sends multi-turn messages, renders filter chips, VLM Reranked badge, inline hits with video timestamp, and follow-up suggestions', async () => {
    const sessionSummary: ChatSessionSummary = {
      id: 'chat-s1',
      title: 'Tokyo memories',
      created_at_ms: 1_000,
      updated_at_ms: 1_000,
    };

    const fullPlan1: RouterPlan = {
      semantic_query: 'ramen',
      ocr_terms: 'RAMEN',
      person_names: ['Mom'],
      location_query: 'Tokyo',
      camera_model: 'X100VI',
      tags: ['street'],
      kind: 'video',
      favorite: true,
      date_from_ms: Date.UTC(2025, 11, 1),
      date_to_ms: Date.UTC(2026, 1, 28),
      clear_prior_filters: false,
      assistant_reply: 'Found 1 video in Tokyo matching Mom and RAMEN.',
      follow_up_suggestions: ['Only photos', 'Reset filters'],
    };

    const videoHit: SearchHit = {
      asset: tokyoVideoAsset,
      score: 0.96,
      timestamp_sec: 24,
      match_reasons: ['Location: Tokyo', 'Person: Mom'],
      ocr_snippet: null,
    };

    const turn1Response: SendChatMessageResponse = {
      user_message: {
        id: 'u-1',
        session_id: 'chat-s1',
        role: 'user',
        content: 'Tokyo videos with Mom',
        plan: null,
        hits: [],
        reranked: false,
        planner_mode: null,
        elapsed_ms: null,
        created_at_ms: 1_100,
      },
      assistant_message: {
        id: 'a-1',
        session_id: 'chat-s1',
        role: 'assistant',
        content: 'Found 1 video in Tokyo matching Mom and RAMEN.',
        plan: fullPlan1,
        hits: [videoHit],
        reranked: true,
        planner_mode: 'exact_simd',
        elapsed_ms: 18,
        created_at_ms: 1_120,
      },
      active_filters: fullPlan1,
      hits: [videoHit],
      total_count: 1,
      planner_mode: 'exact_simd',
      reranked: true,
      elapsed_ms: 18,
    };

    const turn2Plan: RouterPlan = {
      ...fullPlan1,
      kind: 'photo',
      assistant_reply: 'Narrowed to photos in Tokyo.',
      follow_up_suggestions: [],
    };
    const photoHit: SearchHit = {
      asset: tokyoPhotoAsset,
      score: 0.88,
      timestamp_sec: null,
      match_reasons: ['OCR: RAMEN'],
      ocr_snippet: 'ICHIRAN RAMEN',
    };

    const turn2Response: SendChatMessageResponse = {
      user_message: {
        id: 'u-2',
        session_id: 'chat-s1',
        role: 'user',
        content: 'Only photos',
        plan: null,
        hits: [],
        reranked: false,
        planner_mode: null,
        elapsed_ms: null,
        created_at_ms: 2_000,
      },
      assistant_message: {
        id: 'a-2',
        session_id: 'chat-s1',
        role: 'assistant',
        content: 'Narrowed to photos in Tokyo.',
        plan: turn2Plan,
        hits: [photoHit],
        reranked: false,
        planner_mode: 'exact_simd',
        elapsed_ms: 9,
        created_at_ms: 2_020,
      },
      active_filters: turn2Plan,
      hits: [photoHit],
      total_count: 1,
      planner_mode: 'exact_simd',
      reranked: false,
      elapsed_ms: 9,
    };

    const sentPrompts: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url === '/api/chat/sessions' && method === 'GET') {
          return jsonResponse({ items: [] });
        }
        if (url === '/api/chat/sessions' && method === 'POST') {
          return jsonResponse({ session: sessionSummary }, 201);
        }
        if (url === '/api/chat/sessions/chat-s1/messages' && method === 'POST') {
          const parsed = JSON.parse(String(init?.body ?? '{}')) as { content: string };
          sentPrompts.push(parsed.content);
          return jsonResponse(sentPrompts.length === 1 ? turn1Response : turn2Response);
        }
        if (url === '/api/chat/sessions/chat-s1' && method === 'DELETE') {
          return new Response(null, { status: 204 });
        }
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleViewer,
      chatDrawerOpen: true,
      aiDashboard: {
        ...phase3Dashboard,
        state: {
          ...phase3Dashboard.state,
          active_chat_tier: 'tier3',
          vlm_rerank_enabled: true,
        },
      },
    });

    const { container } = render(
      <>
        <FeedbackHost />
        <ChatDrawer />
      </>,
    );

    expect(screen.getByText('Tier 3 + VLM')).toBeTruthy();

    // Send first prompt
    const input = screen.getByLabelText('Ask AI prompt');
    fireEvent.change(input, { target: { value: 'Tokyo videos with Mom' } });
    await act(async () => {
      fireEvent.submit(input.closest('form')!);
    });

    // Assistant reply, VLM Reranked badge, and all transparent filter chips
    expect(await screen.findByText('Found 1 video in Tokyo matching Mom and RAMEN.')).toBeTruthy();
    expect(screen.getByText('VLM Reranked')).toBeTruthy();
    expect(screen.getByText('Kind: Video')).toBeTruthy();
    expect(screen.getByText('Location: Tokyo')).toBeTruthy();
    expect(screen.getByText('Person: Mom')).toBeTruthy();
    expect(screen.getByText('Camera: X100VI')).toBeTruthy();
    expect(screen.getByText('Tag: street')).toBeTruthy();
    expect(screen.getByText('OCR: RAMEN')).toBeTruthy();
    expect(screen.getByText('Favorites')).toBeTruthy();
    expect(screen.getByText('Date range: 2025-12-01 → 2026-02-28')).toBeTruthy();

    // Inline video hit displays timestamp pill 0:24 and opens Lightbox at timestamp 24
    expect(screen.getByText('0:24')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: /Open Shibuya Rain Clip/i }));
    expect(useAppStore.getState().lightboxAssetId).toBe('asset-tokyo-vid');
    expect(useAppStore.getState().lightboxInitialTimestampSec).toBe(24);

    // Clicking follow-up suggestion sends the second turn
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Only photos' }));
    });
    expect(await screen.findByText('Narrowed to photos in Tokyo.')).toBeTruthy();
    expect(sentPrompts).toEqual(['Tokyo videos with Mom', 'Only photos']);
    expect(container.querySelectorAll('.chat-turn.is-user')).toHaveLength(2);
    expect(container.querySelectorAll('.chat-turn.is-assistant')).toHaveLength(2);

    // Escape closes the drawer via escapeStack
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(useAppStore.getState().chatDrawerOpen).toBe(false);
  });

  it('mounts Ask AI button in workspace header for Viewer and toggles ChatDrawer with Cmd/Ctrl+K', async () => {
    const authStatus: AuthStatusResponse = {
      initialized: true,
      authenticated: true,
      server_gate_passed: true,
      desktop_pin_lock: false,
      user: sampleViewer,
      profiles: null,
    };

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === '/api/auth/status') return jsonResponse(authStatus);
        if (url === '/api/auth/me') return jsonResponse({ user: sampleViewer, allowed_folder_ids: ['f-1'] });
        if (url === '/api/system/info') return jsonResponse(sampleSystemInfo);
        if (url === '/api/folders') return jsonResponse({ items: [sampleFolder] });
        if (url.startsWith('/api/assets/buckets')) return jsonResponse({ buckets: [] });
        if (url.startsWith('/api/assets')) return jsonResponse({ items: [], next_cursor: null, total_count: 0 });
        if (url === '/api/chat/sessions') return jsonResponse({ items: [] });
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    render(<App />);

    const askBtn = await screen.findByRole('button', { name: /Ask AI/i });
    expect(askBtn).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Upload' })).toBeNull();
    expect(useAppStore.getState().chatDrawerOpen).toBe(false);

    // Click Ask AI button opens ChatDrawer
    await act(async () => {
      fireEvent.click(askBtn);
    });
    expect(useAppStore.getState().chatDrawerOpen).toBe(true);
    expect(await screen.findByLabelText('Ask AI prompt')).toBeTruthy();

    // Cmd/Ctrl+K toggles ChatDrawer closed and back open
    await act(async () => {
      fireEvent.keyDown(document, { key: 'k', metaKey: true });
    });
    expect(useAppStore.getState().chatDrawerOpen).toBe(false);

    await act(async () => {
      fireEvent.keyDown(document, { key: 'k', ctrlKey: true });
    });
    expect(useAppStore.getState().chatDrawerOpen).toBe(true);
  });
});

describe('MapView offline-first jsdom fallback & cluster strip', () => {
  it('renders cleanly in jsdom without WebGL, shows Offline Map pill + geotagged count, loads cluster points, and opens Lightbox', async () => {
    expect(hasWebGLSupport()).toBe(false);

    const mapConfig: MapConfigResponse = {
      tile_mode: 'offline_pmtiles',
      available_pmtiles: [],
      pmtiles_url: null,
      online_style_url: 'https://tiles.openfreemap.org/styles/dark',
      total_geotagged: 14,
    };

    const tokyoCluster: MapCluster = {
      id: 'c-tokyo',
      lat: 35.6762,
      lon: 139.6503,
      count: 14,
      cover_asset_id: 'asset-tokyo-photo',
      min_lat: 35.6,
      max_lat: 35.75,
      min_lon: 139.6,
      max_lon: 139.8,
      label: 'Tokyo, Japan',
    };

    const requestedUrls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        requestedUrls.push(url);
        if (url === '/api/map/config') return jsonResponse(mapConfig);
        if (url.startsWith('/api/map/clusters?')) {
          return jsonResponse({ clusters: [tokyoCluster], total_in_view: 14 });
        }
        if (url.startsWith('/api/map/points?')) {
          return jsonResponse({ items: [tokyoPhotoAsset, tokyoVideoAsset], total_count: 2 });
        }
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );

    // Verify style builder produces zero-network GeoJSON style by default & PMTiles/Online styles when configured
    const defaultStyle = buildMapStyle(mapConfig);
    expect(typeof defaultStyle).toBe('object');
    if (typeof defaultStyle === 'object') {
      expect(defaultStyle.sources).toHaveProperty('world-land');
      expect(defaultStyle.sources).toHaveProperty('world-graticule');
    }
    const pmtilesStyle = buildMapStyle({
      ...mapConfig,
      pmtiles_url: '/api/map/tiles/world-z0-z6.pmtiles',
    });
    expect(typeof pmtilesStyle).toBe('object');
    expect(
      buildMapStyle({
        ...mapConfig,
        tile_mode: 'online_openfreemap',
      }),
    ).toBe('https://tiles.openfreemap.org/styles/dark');

    useAppStore.setState({
      initialized: true,
      authenticated: true,
      user: sampleViewer,
      folders: [sampleFolder],
    });

    const { container } = render(
      <>
        <FeedbackHost />
        <MapView />
      </>,
    );

    const statusPill = await screen.findByTestId('map-status-pill');
    expect(statusPill.textContent).toContain('Offline Map');
    expect(statusPill.textContent).toContain('14 geotagged');
    expect(screen.getByRole('button', { name: /Fit All/i })).toBeTruthy();

    // Cluster item rendered in fallback list
    const clusterBtn = await screen.findByRole('button', { name: /Tokyo, Japan/i });
    expect(clusterBtn.textContent).toContain('14 photos');

    // Clicking cluster loads /api/map/points into .map-photo-strip
    await act(async () => {
      fireEvent.click(clusterBtn);
    });

    const strip = await screen.findByRole('region', { name: /Photos in Tokyo, Japan/i });
    expect(strip.classList.contains('map-photo-strip')).toBe(true);
    expect(requestedUrls.some((u) => u.startsWith('/api/map/points?'))).toBe(true);

    // Clicking a photo in the strip opens Lightbox
    const thumbBtn = within(strip).getByRole('button', { name: /Open Tokyo Ramen Receipt/i });
    fireEvent.click(thumbBtn);
    expect(useAppStore.getState().lightboxAssetId).toBe('asset-tokyo-photo');
    expect(useAppStore.getState().lightboxAssetIds).toEqual([
      'asset-tokyo-photo',
      'asset-tokyo-vid',
    ]);
    expect(container.querySelector('[data-testid="map-fallback-list"]')).not.toBeNull();
  });
});

describe('AiModelsPanel Phase 3 Chat Tier & SettingsView Map Basemap RBAC', () => {
  it('renders Chat Tier 0-3 selector, VLM rerank toggle, and GGUF + PMTiles model cards', async () => {
    let chatTier = 'tier2' as 'tier0' | 'tier1' | 'tier2' | 'tier3';
    let vlmEnabled = false;
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
            ...phase3Dashboard,
            state: {
              ...phase3Dashboard.state,
              active_chat_tier: chatTier,
              vlm_rerank_enabled: vlmEnabled,
            },
          });
        }
        if (url === '/api/admin/ai/chat-tier' && method === 'POST') {
          const req = body as {
            tier: 'tier0' | 'tier1' | 'tier2' | 'tier3';
            vlm_rerank_enabled?: boolean;
          };
          chatTier = req.tier;
          vlmEnabled = Boolean(req.vlm_rerank_enabled);
          const nextState: AiIndexStateView = {
            ...phase3Dashboard.state,
            active_chat_tier: chatTier,
            vlm_rerank_enabled: vlmEnabled,
          };
          return jsonResponse(nextState);
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

    await screen.findByText('Conversational Chat Tier');

    // All 4 chat tier options are rendered
    expect(screen.getByRole('button', { name: 'Tier 0 Classic' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Tier 1 Gemma-4-E2B' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Tier 2 Gemma-4-E4B' })).toBeTruthy();
    const tier3Btn = screen.getByRole('button', { name: 'Tier 3 Gemma-4-26B + VLM' });
    expect(tier3Btn).toBeTruthy();

    // GGUF & PMTiles model cards render
    for (const modelId of [
      'gemma4-e2b',
      'gemma4-e4b',
      'qwen3.5-4b',
      'gemma4-26b-a4b',
      'world-basemap-pmtiles',
    ]) {
      expect(container.querySelector(`[data-model-id="${modelId}"]`)).not.toBeNull();
    }

    // VLM toggle is disabled while on Tier 2
    const vlmToggle = screen.getByLabelText(/VLM Thumbnail Reranking/i) as HTMLInputElement;
    expect(vlmToggle.disabled).toBe(true);

    // Switch to Tier 3 -> enables VLM toggle
    await act(async () => {
      fireEvent.click(tier3Btn);
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/ai/chat-tier' &&
          c.method === 'POST' &&
          (c.body as { tier?: string })?.tier === 'tier3',
      ),
    ).toBe(true);

    await waitFor(() => {
      expect((screen.getByLabelText(/VLM Thumbnail Reranking/i) as HTMLInputElement).disabled).toBe(
        false,
      );
    });

    // Enable VLM reranking
    await act(async () => {
      fireEvent.click(screen.getByLabelText(/VLM Thumbnail Reranking/i));
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/ai/chat-tier' &&
          c.method === 'POST' &&
          (c.body as { tier?: string; vlm_rerank_enabled?: boolean })?.tier === 'tier3' &&
          (c.body as { vlm_rerank_enabled?: boolean })?.vlm_rerank_enabled === true,
      ),
    ).toBe(true);
  });

  it('renders Map Basemap tile mode toggle ONLY for admin and hides it for viewer and contributor', async () => {
    for (const nonAdmin of [sampleViewer, sampleContributor]) {
      useAppStore.setState({
        initialized: true,
        authenticated: true,
        user: nonAdmin,
        allowedFolderIds: ['f-1'],
        folders: [sampleFolder],
        systemInfo: sampleSystemInfo,
        adminSettings: null,
      });

      const view = render(<SettingsView />);
      expect(screen.queryByText('Map Basemap')).toBeNull();
      expect(screen.queryByRole('button', { name: 'Offline (PMTiles)' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Online (OpenFreeMap)' })).toBeNull();
      view.unmount();
    }

    let currentTileMode: 'offline_pmtiles' | 'online_openfreemap' = 'offline_pmtiles';
    const calls: Array<{ url: string; method: string; body?: unknown }> = [];

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        const body = init?.body ? JSON.parse(String(init.body)) : undefined;
        calls.push({ url, method, body });

        if (url === '/api/admin/ai/models') return jsonResponse(phase3Dashboard);
        if (url === '/api/map/config') {
          return jsonResponse({
            tile_mode: currentTileMode,
            available_pmtiles: ['world-z0-z6.pmtiles'],
            pmtiles_url: '/api/map/tiles/world-z0-z6.pmtiles',
            online_style_url: 'https://tiles.openfreemap.org/styles/dark',
            total_geotagged: 14,
          });
        }
        if (url === '/api/admin/settings' && method === 'PATCH') {
          const patch = body as { map_tile_mode?: 'offline_pmtiles' | 'online_openfreemap' };
          if (patch.map_tile_mode) currentTileMode = patch.map_tile_mode;
          return jsonResponse({ ...sampleSettings, map_tile_mode: currentTileMode });
        }
        if (url === '/api/admin/settings' && method === 'GET') {
          return jsonResponse({ ...sampleSettings, map_tile_mode: currentTileMode });
        }
        if (url === '/api/admin/users') return jsonResponse({ items: [] });
        if (url === '/api/admin/ai/models/world-basemap-pmtiles/download' && method === 'POST') {
          return new Response(null, { status: 202 });
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
      systemInfo: sampleSystemInfo,
      adminUsers: [],
      adminSettings: sampleSettings,
    });

    render(
      <>
        <FeedbackHost />
        <SettingsView />
      </>,
    );

    expect(await screen.findByText('Map Basemap')).toBeTruthy();
    expect(screen.getByText('/data/maps')).toBeTruthy();
    expect(await screen.findByText('world-z0-z6.pmtiles')).toBeTruthy();

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Online (OpenFreeMap)' }));
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/settings' &&
          c.method === 'PATCH' &&
          (c.body as { map_tile_mode?: string })?.map_tile_mode === 'online_openfreemap',
      ),
    ).toBe(true);

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Download Offline World Basemap/i }));
    });
    expect(
      calls.some(
        (c) =>
          c.url === '/api/admin/ai/models/world-basemap-pmtiles/download' &&
          c.method === 'POST',
      ),
    ).toBe(true);
  });
});
