// @vitest-environment jsdom
import React from 'react';
import { render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { VideoPlayer } from '../src/components/VideoPlayer';
import {
  cancelAiModelDownload,
  deleteAiModel,
  downloadAiModel,
  faceThumbUrl,
  getPerson,
  listPeople,
  listPersonAssets,
  mergePeople,
  pauseAiIndexing,
  reassignFace,
  resumeAiIndexing,
  setAiVisionTier,
  triggerAiReindex,
  triggerFaceClustering,
  updateAiSettings,
  updatePerson,
} from '../src/lib/api';
import { GB } from '../src/lib/format';
import { useAppStore } from '../src/store/useAppStore';
import type {
  AiIndexStateView,
  AiModelsDashboardResponse,
  AssetListResponse,
  ClusterFacesResponse,
  MergePeopleResponse,
  PersonListResponse,
  PersonSummary,
  PlaybackInfo,
  ReassignFaceResponse,
  SearchResponse,
} from '../src/types/api';

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const sampleDashboard: AiModelsDashboardResponse = {
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
    updated_at_ms: 1_700_000_000_000,
  },
  models: [
    {
      id: 'siglip2-large',
      name: 'SigLIP2 Large',
      role: 'vision_tier2',
      license: 'Apache-2.0',
      hf_repo: 'onnx-community/siglip2-large-patch16-256-ONNX',
      tier: 'tier2',
      approx_bytes: 1_200_000_000,
      status: 'installed',
      bytes_total: 1_200_000_000,
      bytes_downloaded: 1_200_000_000,
      error_message: null,
      updated_at_ms: 1_700_000_000_000,
    },
  ],
  queue: {
    pending_assets: 4,
    indexed_assets: 96,
    staging_remaining: 0,
  },
};

const sampleSearchResponse: SearchResponse = {
  items: [
    {
      asset: {
        id: 'asset-vid-1',
        folder_id: 'f-1',
        kind: 'video',
        capture_at_ms: 1_700_000_000_000,
        width: 1920,
        height: 1080,
        duration_ms: 45_000,
        aspect_ratio: 16 / 9,
        is_favorite: true,
        deleted_at_ms: null,
        thumb_rel_path: 'as/se/asset-vid-1.webp',
        thumb_ready: true,
        title: 'Sunset Drive',
        rating: 5,
        gps_lat: null,
        gps_lon: null,
        geo_city: 'Kyoto',
        geo_country: 'Japan',
        camera_model: 'iPhone 16 Pro',
      },
      score: 0.91,
      timestamp_sec: 24.5,
      match_reasons: ['Semantic 91%', 'Tag: sunset'],
      ocr_snippet: null,
    },
  ],
  total_count: 1,
  planner_mode: 'exact_simd',
  elapsed_ms: 2,
};

beforeEach(() => {
  useAppStore.getState().resetForTest();
});

afterEach(() => {
  useAppStore.getState().resetForTest();
  vi.unstubAllGlobals();
});

describe('Phase 2 store & API helpers', () => {
  it('runs search, updates filters, and clears search state', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      expect(String(input)).toBe('/api/search');
      expect(init?.method).toBe('POST');
      const body = JSON.parse(String(init?.body));
      expect(body).toEqual({
        query: 'sunset',
        mode: 'semantic',
        favorite: true,
        limit: 60,
        offset: 0,
      });
      return jsonResponse(sampleSearchResponse);
    });
    vi.stubGlobal('fetch', fetchMock);

    useAppStore.getState().setSearchFilters({ mode: 'semantic', favorite: true });
    expect(useAppStore.getState().searchRequest.mode).toBe('semantic');
    expect(useAppStore.getState().searchRequest.favorite).toBe(true);

    await useAppStore.getState().runSearch({ query: 'sunset' });

    const afterRun = useAppStore.getState();
    expect(afterRun.searchActive).toBe(true);
    expect(afterRun.searchLoading).toBe(false);
    expect(afterRun.searchQuery).toBe('sunset');
    expect(afterRun.searchResponse).toEqual(sampleSearchResponse);

    useAppStore.getState().clearSearch();
    const afterClear = useAppStore.getState();
    expect(afterClear.searchActive).toBe(false);
    expect(afterClear.searchQuery).toBe('');
    expect(afterClear.searchResponse).toBeNull();
    expect(afterClear.searchLoading).toBe(false);
    expect(afterClear.searchRequest).toEqual({
      mode: 'hybrid',
      limit: 60,
      offset: 0,
    });
  });

  it('tracks lightboxInitialTimestampSec across openLightbox, setLightboxAsset, and closeLightbox', () => {
    useAppStore.getState().openLightbox('asset-1', ['asset-1', 'asset-2']);
    expect(useAppStore.getState().lightboxAssetId).toBe('asset-1');
    expect(useAppStore.getState().lightboxInitialTimestampSec).toBeNull();

    useAppStore.getState().openLightbox('asset-vid-1', ['asset-vid-1', 'asset-2'], 24.5);
    expect(useAppStore.getState().lightboxAssetId).toBe('asset-vid-1');
    expect(useAppStore.getState().lightboxInitialTimestampSec).toBe(24.5);

    useAppStore.getState().setLightboxAsset('asset-2');
    expect(useAppStore.getState().lightboxAssetId).toBe('asset-2');
    expect(useAppStore.getState().lightboxInitialTimestampSec).toBeNull();

    useAppStore.getState().openLightbox('asset-vid-1', ['asset-vid-1'], 12);
    expect(useAppStore.getState().lightboxInitialTimestampSec).toBe(12);
    useAppStore.getState().closeLightbox();
    expect(useAppStore.getState().lightboxAssetId).toBeNull();
    expect(useAppStore.getState().lightboxInitialTimestampSec).toBeNull();
  });

  it('refreshes AI dashboard and updates state on ai_state_changed WebSocket event', async () => {
    // Before dashboard is loaded, ai_state_changed is a safe no-op
    useAppStore.getState().applyWsEvent({
      type: 'ai_state_changed',
      active_vision_tier: 'tier3',
      staging_vision_tier: null,
      paused: true,
    });
    expect(useAppStore.getState().aiDashboard).toBeNull();

    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      expect(String(input)).toBe('/api/admin/ai/models');
      return jsonResponse(sampleDashboard);
    });
    vi.stubGlobal('fetch', fetchMock);

    await useAppStore.getState().refreshAiDashboard();
    expect(useAppStore.getState().aiDashboard).toEqual(sampleDashboard);

    useAppStore.getState().applyWsEvent({
      type: 'ai_state_changed',
      active_vision_tier: 'tier2',
      staging_vision_tier: 'tier3',
      paused: true,
    });

    const updated = useAppStore.getState().aiDashboard;
    expect(updated?.state.active_vision_tier).toBe('tier2');
    expect(updated?.state.staging_vision_tier).toBe('tier3');
    expect(updated?.state.paused).toBe(true);
    expect(updated?.models).toEqual(sampleDashboard.models);
  });

  it('builds encoded faceThumbUrl and calls Phase 2 People and AI endpoints', async () => {
    expect(faceThumbUrl('face/123+abc')).toBe('/api/media/faces/face%2F123%2Babc/thumb');

    const person: PersonSummary = {
      id: 'p-1',
      name: 'Alice',
      is_hidden: false,
      is_pinned: true,
      cover_face_id: 'f-1',
      face_count: 12,
      created_at_ms: 1000,
      updated_at_ms: 2000,
    };
    const peopleList: PersonListResponse = { items: [person] };
    const personAssets: AssetListResponse = { items: [], next_cursor: null, total_count: 0 };
    const mergeResp: MergePeopleResponse = { person, merged_faces: 3 };
    const reassignResp: ReassignFaceResponse = { face_id: 'face-1', person_id: 'p-2' };
    const clusterResp: ClusterFacesResponse = { clusters_created: 2, faces_assigned: 7 };
    const aiState: AiIndexStateView = {
      active_vision_tier: 'tier2',
      staging_vision_tier: null,
      paused: true,
      idle_unload_secs: 600,
      updated_at_ms: 3000,
    };

    const calls: Array<{ url: string; method: string; body?: unknown }> = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        const body = init?.body ? JSON.parse(String(init.body)) : undefined;
        calls.push({ url, method, body });

        if (url === '/api/people' || url.startsWith('/api/people?')) return jsonResponse(peopleList);
        if (url === '/api/people/p-1' && method === 'GET') return jsonResponse(person);
        if (url.startsWith('/api/people/p-1/assets')) return jsonResponse(personAssets);
        if (url === '/api/people/p-1' && method === 'PATCH') return jsonResponse(person);
        if (url === '/api/people/merge') return jsonResponse(mergeResp);
        if (url === '/api/people/p-1/faces/face-1/reassign') return jsonResponse(reassignResp);
        if (url === '/api/people/cluster') return jsonResponse(clusterResp, 202);
        if (url === '/api/admin/ai/models/siglip2-large/download') return new Response(null, { status: 202 });
        if (url === '/api/admin/ai/models/siglip2-large/cancel') return new Response(null, { status: 200 });
        if (url === '/api/admin/ai/models/siglip2-large' && method === 'DELETE') {
          return new Response(null, { status: 204 });
        }
        if (url === '/api/admin/ai/tier') return new Response(null, { status: 202 });
        if (url === '/api/admin/ai/reindex') return new Response(null, { status: 202 });
        if (url === '/api/admin/ai/pause' || url === '/api/admin/ai/resume') return jsonResponse(aiState);
        if (url === '/api/admin/ai/settings') return jsonResponse(aiState);
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    await expect(listPeople()).resolves.toEqual(peopleList);
    await expect(listPeople({ include_hidden: true })).resolves.toEqual(peopleList);
    await expect(getPerson('p-1')).resolves.toEqual(person);
    await expect(listPersonAssets('p-1', { cursor: 'c1', limit: 30 })).resolves.toEqual(personAssets);
    await expect(updatePerson('p-1', { name: 'Alice', is_pinned: true })).resolves.toEqual(person);
    await expect(
      mergePeople({ source_person_ids: ['p-2'], target_person_id: 'p-1' }),
    ).resolves.toEqual(mergeResp);
    await expect(
      reassignFace('p-1', 'face-1', { target_person_id: 'p-2' }),
    ).resolves.toEqual(reassignResp);
    await expect(triggerFaceClustering()).resolves.toEqual(clusterResp);

    await expect(downloadAiModel('siglip2-large')).resolves.toBeUndefined();
    await expect(cancelAiModelDownload('siglip2-large')).resolves.toBeUndefined();
    await expect(deleteAiModel('siglip2-large')).resolves.toBeUndefined();
    await expect(setAiVisionTier('tier3')).resolves.toBeUndefined();
    await expect(triggerAiReindex('all')).resolves.toBeUndefined();
    await expect(pauseAiIndexing()).resolves.toEqual(aiState);
    await expect(resumeAiIndexing()).resolves.toEqual(aiState);
    await expect(updateAiSettings({ idle_unload_secs: 600, video_max_frames: 20 })).resolves.toEqual(
      aiState,
    );

    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      'GET /api/people',
      'GET /api/people?include_hidden=true',
      'GET /api/people/p-1',
      'GET /api/people/p-1/assets?cursor=c1&limit=30',
      'PATCH /api/people/p-1',
      'POST /api/people/merge',
      'POST /api/people/p-1/faces/face-1/reassign',
      'POST /api/people/cluster',
      'POST /api/admin/ai/models/siglip2-large/download',
      'POST /api/admin/ai/models/siglip2-large/cancel',
      'DELETE /api/admin/ai/models/siglip2-large',
      'POST /api/admin/ai/tier',
      'POST /api/admin/ai/reindex',
      'POST /api/admin/ai/pause',
      'POST /api/admin/ai/resume',
      'PATCH /api/admin/ai/settings',
    ]);
  });

  it('seeks VideoPlayer to initialTimestampSec once metadata loads', () => {
    const playback: PlaybackInfo = {
      mode: 'direct',
      direct_url: '/api/media/vid-1/original',
      hls_url: null,
      live_video_url: null,
      has_raw: false,
      preview_url: '/api/media/vid-1/preview',
      thumb_url: '/api/media/vid-1/thumb',
      original_url: '/api/media/vid-1/original',
    };

    const { container } = render(
      React.createElement(VideoPlayer, { playback, initialTimestampSec: 24.5 }),
    );
    const video = container.querySelector('video');
    expect(video).not.toBeNull();
    expect(video!.currentTime).toBe(0);

    video!.dispatchEvent(new Event('loadedmetadata'));
    expect(video!.currentTime).toBe(24.5);
  });
});
