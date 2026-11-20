// @vitest-environment jsdom
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ContextMenuHost } from '../src/components/ContextMenu';
import { Lightbox } from '../src/components/Lightbox';
import { ChatView } from '../src/components/views/ChatView';
import { useAppStore } from '../src/store/useAppStore';
import type { AssetDetail, ChatMessageView, SendChatMessageResponse } from '../src/types/api';

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

describe('Polished Native UX Features', () => {
  beforeEach(() => {
    useAppStore.getState().resetForTest();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('shows optimistic user message and animated thinking indicator immediately in ChatView', async () => {
    let resolveMessage!: (resp: Response) => void;
    const messagePromise = new Promise<Response>((resolve) => {
      resolveMessage = resolve;
    });

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        if (url === '/api/chat/sessions' && method === 'GET') return jsonResponse({ items: [] });
        if (url === '/api/chat/warmup') {
          return jsonResponse({ active_chat_tier: 'tier2', loaded: true, warming: false });
        }
        if (url === '/api/admin/ai/models') return jsonResponse({ error: { code: 'FORBIDDEN', message: 'nope' } }, 403);
        if (url === '/api/chat/sessions' && method === 'POST') {
          return jsonResponse({
            session: {
              id: 's-opt',
              title: 'show me sunsets',
              created_at_ms: 1000,
              updated_at_ms: 1000,
              message_count: 0,
            },
          });
        }
        if (url === '/api/chat/sessions/s-opt/messages' && method === 'POST') {
          return messagePromise;
        }
        throw new Error(`Unexpected fetch: ${method} ${url}`);
      }),
    );

    render(<ChatView />);

    const textarea = await screen.findByLabelText('Ask AI prompt');
    fireEvent.change(textarea, { target: { value: 'show me sunsets' } });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    });

    // User's prompt and thinking indicator appear IMMEDIATELY before backend responds
    const userBubble = document.querySelector('.chat-bubble.is-user');
    expect(userBubble?.textContent).toBe('show me sunsets');
    expect(screen.getByRole('status')).toBeTruthy();

    const userMsg: ChatMessageView = {
      id: 'm-user',
      session_id: 's-opt',
      role: 'user',
      content: 'show me sunsets',
      plan: null,
      hits: [],
      reranked: false,
      planner_mode: null,
      elapsed_ms: null,
      created_at_ms: 1001,
    };
    const assistantMsg: ChatMessageView = {
      id: 'm-asst',
      session_id: 's-opt',
      role: 'assistant',
      content: 'Here are your sunset photos!',
      plan: null,
      hits: [],
      reranked: false,
      planner_mode: 'exact_simd',
      elapsed_ms: 850,
      created_at_ms: 1002,
    };
    const payload: SendChatMessageResponse = {
      user_message: userMsg,
      assistant_message: assistantMsg,
      active_filters: {
        semantic_query: 'sunset',
        tags: [],
        person_names: [],
        location_query: null,
        camera_model: null,
        kind: null,
        favorite: null,
        date_from_ms: null,
        date_to_ms: null,
        ocr_terms: null,
        clear_prior_filters: false,
        assistant_reply: 'Here are your sunset photos!',
        follow_up_suggestions: [],
      },
      hits: [],
      total_count: 0,
      planner_mode: 'exact_simd',
      reranked: false,
      elapsed_ms: 850,
    };

    await act(async () => {
      resolveMessage(jsonResponse(payload));
    });

    expect(await screen.findByText('Here are your sunset photos!')).toBeTruthy();
    expect(useAppStore.getState().chatSending).toBe(false);
  });

  it('toggles Lightbox zoom on direct photo click', async () => {
    const detail: AssetDetail = {
      summary: {
        id: 'asset-zoom',
        folder_id: 'f-1',
        kind: 'photo',
        capture_at_ms: 1_700_000_000_000,
        width: 4000,
        height: 3000,
        duration_ms: null,
        aspect_ratio: 4 / 3,
        is_favorite: false,
        deleted_at_ms: null,
        thumb_rel_path: 'aa/bb.webp',
        thumb_ready: true,
        title: 'Zoomable Photo',
        rating: null,
        gps_lat: null,
        gps_lon: null,
        geo_city: null,
        geo_country: null,
        camera_model: 'Canon EOS R7',
      },
      files: [],
      photo_meta: null,
      video_meta: null,
      overrides: null,
      tags: [],
      histogram: null,
      album_ids: [],
      playback: {
        mode: 'image',
        direct_url: null,
        hls_url: null,
        live_video_url: null,
        has_raw: false,
        preview_url: '/api/media/asset-zoom/preview',
        thumb_url: '/api/media/asset-zoom/thumb',
        original_url: '/api/media/asset-zoom/original',
      },
    };

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/api/assets/asset-zoom') return jsonResponse(detail);
        throw new Error(`Unexpected fetch: ${String(input)}`);
      }),
    );

    useAppStore.setState({
      authenticated: true,
      lightboxAssetId: 'asset-zoom',
      lightboxAssetIds: ['asset-zoom'],
    });

    render(<Lightbox />);

    const img = await screen.findByAltText('Zoomable Photo');
    expect(screen.getByText('100%')).toBeTruthy();

    fireEvent.click(img);
    expect(screen.getByText('2.3×')).toBeTruthy();

    fireEvent.click(img);
    expect(screen.getByText('100%')).toBeTruthy();
  });

  it('opens custom right-click ContextMenu instead of default browser menu', async () => {
    useAppStore.setState({
      authenticated: true,
      user: {
        id: 'u-1',
        username: 'ian',
        display_name: 'Ian',
        role: 'admin',
        avatar_color: '#4a4a4a',
      },
    });

    render(
      <div>
        <ContextMenuHost />
        <div data-testid="canvas-surface">Surface</div>
      </div>,
    );

    const surface = screen.getByTestId('canvas-surface');
    fireEvent.contextMenu(surface, { clientX: 120, clientY: 160 });

    expect(await screen.findByRole('menu', { name: 'Context actions' })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: /Ask AI Assistant/i })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: /Open Settings/i })).toBeTruthy();
  });
});
