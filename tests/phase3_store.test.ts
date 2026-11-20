// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  getMapClusters,
  getMapConfig,
  getMapPoints,
  mapTileUrl,
  setAiChatTier,
} from '../src/lib/api';
import { WORLD_GRATICULE_GEOJSON, WORLD_LAND_GEOJSON } from '../src/lib/worldGeoJson';
import { useAppStore } from '../src/store/useAppStore';
import type {
  AiIndexStateView,
  AssetSummary,
  ChatMessageView,
  ChatSessionDetail,
  ChatSessionSummary,
  MapClustersResponse,
  MapConfigResponse,
  MapPointsResponse,
  RouterPlan,
  SearchHit,
  SendChatMessageResponse,
} from '../src/types/api';

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const sampleAsset: AssetSummary = {
  id: 'asset-tokyo-1',
  folder_id: 'f-1',
  kind: 'video',
  capture_at_ms: 1_700_000_000_000,
  width: 1920,
  height: 1080,
  duration_ms: 15_000,
  aspect_ratio: 16 / 9,
  is_favorite: true,
  deleted_at_ms: null,
  thumb_rel_path: 'as/to/asset-tokyo-1.webp',
  thumb_ready: true,
  title: 'Shibuya Crossing',
  rating: 5,
  gps_lat: 35.6595,
  gps_lon: 139.7004,
  geo_city: 'Tokyo',
  geo_country: 'Japan',
  camera_model: 'Fujifilm X100VI',
};

const sampleHit: SearchHit = {
  asset: sampleAsset,
  score: 0.94,
  timestamp_sec: 4.2,
  match_reasons: ['Location: Tokyo', 'Kind: video'],
  ocr_snippet: null,
};

function makePlan(overrides: Partial<RouterPlan> = {}): RouterPlan {
  return {
    semantic_query: 'beach',
    ocr_terms: null,
    person_names: [],
    location_query: 'Tokyo',
    camera_model: null,
    tags: [],
    kind: null,
    favorite: null,
    date_from_ms: null,
    date_to_ms: null,
    clear_prior_filters: false,
    assistant_reply: 'Found 1 match in Tokyo.',
    follow_up_suggestions: ['Only videos', 'Favorites only'],
    ...overrides,
  };
}

beforeEach(() => {
  useAppStore.getState().resetForTest();
});

afterEach(() => {
  useAppStore.getState().resetForTest();
  vi.unstubAllGlobals();
});

describe('Phase 3 store & API helpers', () => {
  it('auto-creates a chat session on sendChatPrompt and updates multi-turn filters and reranked state', async () => {
    const autoSession: ChatSessionSummary = {
      id: 'chat-auto-1',
      title: 'beach photos in Tokyo',
      created_at_ms: 1_000,
      updated_at_ms: 1_000,
    };

    const turn1Plan = makePlan();
    const turn1Response: SendChatMessageResponse = {
      user_message: {
        id: 'm-1',
        session_id: 'chat-auto-1',
        role: 'user',
        content: 'beach photos in Tokyo',
        plan: null,
        hits: [],
        reranked: false,
        planner_mode: null,
        elapsed_ms: null,
        created_at_ms: 1_100,
      },
      assistant_message: {
        id: 'm-2',
        session_id: 'chat-auto-1',
        role: 'assistant',
        content: 'Found 1 match in Tokyo.',
        plan: turn1Plan,
        hits: [sampleHit],
        reranked: false,
        planner_mode: 'exact_simd',
        elapsed_ms: 12,
        created_at_ms: 1_120,
      },
      active_filters: turn1Plan,
      hits: [sampleHit],
      total_count: 1,
      planner_mode: 'exact_simd',
      reranked: false,
      elapsed_ms: 12,
    };

    const turn2Plan = makePlan({
      kind: 'video',
      assistant_reply: 'Filtered to 1 video in Tokyo (VLM reranked).',
    });
    const turn2Response: SendChatMessageResponse = {
      user_message: {
        id: 'm-3',
        session_id: 'chat-auto-1',
        role: 'user',
        content: 'only videos',
        plan: null,
        hits: [],
        reranked: false,
        planner_mode: null,
        elapsed_ms: null,
        created_at_ms: 2_000,
      },
      assistant_message: {
        id: 'm-4',
        session_id: 'chat-auto-1',
        role: 'assistant',
        content: 'Filtered to 1 video in Tokyo (VLM reranked).',
        plan: turn2Plan,
        hits: [sampleHit],
        reranked: true,
        planner_mode: 'exact_simd',
        elapsed_ms: 48,
        created_at_ms: 2_050,
      },
      active_filters: turn2Plan,
      hits: [sampleHit],
      total_count: 1,
      planner_mode: 'exact_simd',
      reranked: true,
      elapsed_ms: 48,
    };

    const calls: string[] = [];
    let msgCallCount = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        calls.push(`${method} ${url}`);
        if (url === '/api/chat/sessions' && method === 'POST') {
          return jsonResponse({ session: autoSession }, 201);
        }
        if (url === '/api/chat/sessions/chat-auto-1/messages' && method === 'POST') {
          msgCallCount += 1;
          return jsonResponse(msgCallCount === 1 ? turn1Response : turn2Response);
        }
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    expect(useAppStore.getState().activeChatSessionId).toBeNull();

    await useAppStore.getState().sendChatPrompt('beach photos in Tokyo', 24);
    let state = useAppStore.getState();
    expect(state.activeChatSessionId).toBe('chat-auto-1');
    expect(state.chatSessions).toEqual([{ ...autoSession, updated_at_ms: 1120 }]);
    expect(state.chatMessages).toHaveLength(2);
    expect(state.chatActiveFilters).toEqual(turn1Plan);
    expect(state.chatLastReranked).toBe(false);
    expect(state.chatSending).toBe(false);

    // Second turn reuses the active session without creating a duplicate
    await useAppStore.getState().sendChatPrompt('only videos');
    state = useAppStore.getState();
    expect(state.activeChatSessionId).toBe('chat-auto-1');
    expect(state.chatMessages).toHaveLength(4);
    expect(state.chatMessages.map((m: ChatMessageView) => m.id)).toEqual(['m-1', 'm-2', 'm-3', 'm-4']);
    expect(state.chatActiveFilters).toEqual(turn2Plan);
    expect(state.chatLastReranked).toBe(true);
    expect(state.chatSessions[0].updated_at_ms).toBe(2_050);

    expect(calls).toEqual([
      'POST /api/chat/sessions',
      'POST /api/chat/sessions/chat-auto-1/messages',
      'POST /api/chat/sessions/chat-auto-1/messages',
    ]);
  });

  it('creates, lists, selects, removes chat sessions, and resets chat state on unauthorized/drawer toggle', async () => {
    const s1: ChatSessionSummary = {
      id: 's-1',
      title: 'Japan Trip',
      created_at_ms: 100,
      updated_at_ms: 200,
    };
    const s2: ChatSessionSummary = {
      id: 's-2',
      title: 'Family Videos',
      created_at_ms: 300,
      updated_at_ms: 400,
    };
    const s1Detail: ChatSessionDetail = {
      id: 's-1',
      title: 'Japan Trip',
      active_filters: makePlan(),
      created_at_ms: 100,
      updated_at_ms: 200,
    };
    const s1Messages: ChatMessageView[] = [
      {
        id: 'msg-1',
        session_id: 's-1',
        role: 'assistant',
        content: 'Found 1 match in Tokyo.',
        plan: makePlan(),
        hits: [sampleHit],
        reranked: true,
        planner_mode: 'hnsw',
        elapsed_ms: 15,
        created_at_ms: 200,
      },
    ];

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url === '/api/chat/sessions' && method === 'POST') {
          return jsonResponse({ session: s2 }, 201);
        }
        if (url === '/api/chat/sessions' && method === 'GET') {
          return jsonResponse({ items: [s2, s1] });
        }
        if (url === '/api/chat/sessions/s-1' && method === 'GET') {
          return jsonResponse({ session: s1Detail, messages: s1Messages });
        }
        if (url === '/api/chat/sessions/s-1' && method === 'DELETE') {
          return new Response(null, { status: 204 });
        }
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    useAppStore.getState().setChatDrawerOpen(true);
    expect(useAppStore.getState().chatDrawerOpen).toBe(true);
    useAppStore.getState().toggleChatDrawer();
    expect(useAppStore.getState().chatDrawerOpen).toBe(false);

    const created = await useAppStore.getState().createNewChatSession('Family Videos');
    expect(created).toEqual(s2);
    expect(useAppStore.getState().activeChatSessionId).toBe('s-2');
    expect(useAppStore.getState().chatSessions).toEqual([s2]);

    await useAppStore.getState().loadChatSessions();
    expect(useAppStore.getState().chatSessions).toEqual([s2, s1]);

    await useAppStore.getState().selectChatSession('s-1');
    expect(useAppStore.getState().activeChatSessionId).toBe('s-1');
    expect(useAppStore.getState().chatMessages).toEqual(s1Messages);
    expect(useAppStore.getState().chatActiveFilters).toEqual(s1Detail.active_filters);
    expect(useAppStore.getState().chatLastReranked).toBe(true);

    // Deleting active session s-1 clears messages and active filters
    await useAppStore.getState().removeChatSession('s-1');
    expect(useAppStore.getState().chatSessions).toEqual([s2]);
    expect(useAppStore.getState().activeChatSessionId).toBeNull();
    expect(useAppStore.getState().chatMessages).toEqual([]);
    expect(useAppStore.getState().chatActiveFilters).toBeNull();
    expect(useAppStore.getState().chatLastReranked).toBe(false);

    // handleUnauthorized resets drawer and sessions
    useAppStore.getState().setChatDrawerOpen(true);
    useAppStore.getState().handleUnauthorized();
    expect(useAppStore.getState().chatDrawerOpen).toBe(false);
    expect(useAppStore.getState().chatSessions).toEqual([]);
  });

  it('builds mapTileUrl and calls Phase 3 chat-tier and map REST endpoints', async () => {
    expect(mapTileUrl('world-z0-z6.pmtiles')).toBe('/api/map/tiles/world-z0-z6.pmtiles');
    expect(mapTileUrl('region test.pmtiles')).toBe('/api/map/tiles/region%20test.pmtiles');

    const aiState: AiIndexStateView = {
      active_vision_tier: 'tier2',
      staging_vision_tier: null,
      paused: false,
      idle_unload_secs: 300,
      updated_at_ms: 5000,
      active_chat_tier: 'tier3',
      vlm_rerank_enabled: true,
    };
    const mapConfig: MapConfigResponse = {
      tile_mode: 'offline_pmtiles',
      available_pmtiles: ['world-z0-z6.pmtiles'],
      pmtiles_url: '/api/map/tiles/world-z0-z6.pmtiles',
      online_style_url: 'https://tiles.openfreemap.org/styles/dark',
      total_geotagged: 120,
    };
    const clustersResp: MapClustersResponse = {
      clusters: [
        {
          id: 'c-tokyo',
          lat: 35.6762,
          lon: 139.6503,
          count: 14,
          cover_asset_id: 'asset-tokyo-1',
          min_lat: 35.6,
          max_lat: 35.7,
          min_lon: 139.6,
          max_lon: 139.7,
          label: 'Tokyo',
        },
      ],
      total_in_view: 14,
    };
    const pointsResp: MapPointsResponse = {
      items: [sampleAsset],
      total_count: 1,
    };

    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        calls.push(`${method} ${url}`);
        if (url === '/api/admin/ai/chat-tier' && method === 'POST') {
          expect(JSON.parse(String(init?.body))).toEqual({
            tier: 'tier3',
            vlm_rerank_enabled: true,
          });
          return jsonResponse(aiState);
        }
        if (url === '/api/map/config') return jsonResponse(mapConfig);
        if (url.startsWith('/api/map/clusters?')) return jsonResponse(clustersResp);
        if (url.startsWith('/api/map/points?')) return jsonResponse(pointsResp);
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    await expect(setAiChatTier({ tier: 'tier3', vlm_rerank_enabled: true })).resolves.toEqual(
      aiState,
    );
    await expect(getMapConfig()).resolves.toEqual(mapConfig);
    await expect(
      getMapClusters({ min_lat: -90, max_lat: 90, min_lng: -180, max_lng: 180, zoom: 2 }),
    ).resolves.toEqual(clustersResp);
    await expect(
      getMapPoints({ min_lat: 35.6, max_lat: 35.7, min_lng: 139.6, max_lng: 139.7, limit: 50 }),
    ).resolves.toEqual(pointsResp);

    expect(calls).toEqual([
      'POST /api/admin/ai/chat-tier',
      'GET /api/map/config',
      'GET /api/map/clusters?min_lat=-90&max_lat=90&min_lng=-180&max_lng=180&zoom=2',
      'GET /api/map/points?min_lat=35.6&max_lat=35.7&min_lng=139.6&max_lng=139.7&limit=50',
    ]);
  });

  it('exports valid compact WORLD_LAND_GEOJSON and WORLD_GRATICULE_GEOJSON', () => {
    expect(WORLD_LAND_GEOJSON.type).toBe('FeatureCollection');
    expect(WORLD_LAND_GEOJSON.features.length).toBeGreaterThanOrEqual(12);

    const names = new Set(WORLD_LAND_GEOJSON.features.map((f) => f.properties.name));
    for (const required of [
      'North America',
      'South America',
      'Greenland',
      'Europe',
      'Great Britain',
      'Ireland',
      'Africa',
      'Madagascar',
      'Asia',
      'Japan',
      'Australia',
      'Antarctica',
    ]) {
      expect(names.has(required)).toBe(true);
    }

    for (const feature of WORLD_LAND_GEOJSON.features) {
      expect(feature.type).toBe('Feature');
      expect(feature.geometry.type).toBe('Polygon');
      expect(feature.geometry.coordinates.length).toBeGreaterThanOrEqual(1);
      for (const ring of feature.geometry.coordinates) {
        expect(ring.length).toBeGreaterThanOrEqual(4);
        expect(ring[0]).toEqual(ring[ring.length - 1]);
        for (const [lon, lat] of ring) {
          expect(lon).toBeGreaterThanOrEqual(-180);
          expect(lon).toBeLessThanOrEqual(180);
          expect(lat).toBeGreaterThanOrEqual(-85);
          expect(lat).toBeLessThanOrEqual(85);
        }
      }
    }

    const landJsonSize = JSON.stringify(WORLD_LAND_GEOJSON).length;
    expect(landJsonSize).toBeGreaterThan(2000);
    expect(landJsonSize).toBeLessThan(25000);

    expect(WORLD_GRATICULE_GEOJSON.type).toBe('FeatureCollection');
    expect(WORLD_GRATICULE_GEOJSON.features.length).toBe(18);
    for (const feature of WORLD_GRATICULE_GEOJSON.features) {
      expect(feature.geometry.type).toBe('LineString');
      expect(feature.geometry.coordinates.length).toBeGreaterThanOrEqual(2);
    }
  });
});
