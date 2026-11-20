import React, { useEffect, useRef, useState } from 'react';
import {
  ArrowUp,
  Download,
  Eye,
  MessageSquare,
  PanelLeft,
  Play,
  Plus,
  Sparkles,
  Trash2,
  X,
} from 'lucide-react';
import { mediaOriginalUrl, mediaThumbUrl, warmupChat } from '../../lib/api';
import { notifyError, notifySuccess } from '../../store/feedback';
import { useAppStore } from '../../store/useAppStore';
import type { AssetKind, ChatTier, RouterPlan } from '../../types/api';
import { openContextMenu } from '../ContextMenu';

const CHAT_TIER_LABELS: Record<ChatTier, string> = {
  tier0: 'Classic Search',
  tier1: 'Gemma 4 · Fast',
  tier2: 'Gemma 4 · Balanced',
  tier3: 'Gemma 4 · Max + VLM',
};

const KIND_LABELS: Record<AssetKind, string> = {
  photo: 'Photo',
  video: 'Video',
  live_photo: 'Live Photo',
  motion_photo: 'Motion Photo',
  raw_jpg_stack: 'RAW + JPG',
};

const STARTER_PROMPTS: ReadonlyArray<{ title: string; subtitle: string; prompt: string }> = [
  {
    title: 'Explore nature & scenes',
    subtitle: 'Semantic visual search across your library',
    prompt: 'Show me waterfall photos',
  },
  {
    title: 'Find landscapes & mountains',
    subtitle: 'Search by scenery, mood, or lighting',
    prompt: 'Show me mountain landscapes',
  },
  {
    title: 'Filter by camera or gear',
    subtitle: 'Deterministic EXIF metadata lookup',
    prompt: 'Show me favorite photos or videos',
  },
  {
    title: 'Search text inside photos',
    subtitle: 'Offline OCR full-text recognition',
    prompt: 'Find photos with signs or text',
  },
];

function formatTimestamp(seconds: number): string {
  const totalSec = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(totalSec / 3600);
  const mins = Math.floor((totalSec % 3600) / 60);
  const secs = totalSec % 60;
  if (hours > 0) {
    return `${hours}:${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
  }
  return `${mins}:${String(secs).padStart(2, '0')}`;
}

function formatUtcDate(ms: number): string {
  if (!Number.isFinite(ms)) return '';
  return new Date(ms).toISOString().slice(0, 10);
}

function formatShortTime(ms: number): string {
  if (!Number.isFinite(ms)) return '';
  const diffSec = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (diffSec < 60) return 'Just now';
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

function buildFilterChips(plan: RouterPlan | null): string[] {
  if (!plan) return [];
  const chips: string[] = [];

  if (plan.kind) {
    chips.push(`Kind: ${KIND_LABELS[plan.kind] ?? plan.kind}`);
  }
  if (plan.location_query) {
    chips.push(`Location: ${plan.location_query}`);
  }
  for (const person of plan.person_names ?? []) {
    if (person.trim()) {
      chips.push(`Person: ${person.trim()}`);
    }
  }
  if (plan.camera_model) {
    chips.push(`Camera: ${plan.camera_model}`);
  }
  for (const tag of plan.tags ?? []) {
    if (tag.trim()) {
      chips.push(`Tag: ${tag.trim()}`);
    }
  }
  if (plan.ocr_terms) {
    chips.push(`OCR: ${plan.ocr_terms}`);
  }
  if (plan.favorite === true) {
    chips.push('Favorites');
  } else if (plan.favorite === false) {
    chips.push('Exclude Favorites');
  }
  if (plan.date_from_ms != null || plan.date_to_ms != null) {
    const from = plan.date_from_ms != null ? formatUtcDate(plan.date_from_ms) : null;
    const to = plan.date_to_ms != null ? formatUtcDate(plan.date_to_ms) : null;
    if (from && to) {
      chips.push(`Date: ${from} → ${to}`);
    } else if (from) {
      chips.push(`From ${from}`);
    } else if (to) {
      chips.push(`Until ${to}`);
    }
  }

  return chips;
}

export const ChatView: React.FC = () => {
  const chatSessions = useAppStore((s) => s.chatSessions);
  const activeChatSessionId = useAppStore((s) => s.activeChatSessionId);
  const chatMessages = useAppStore((s) => s.chatMessages);
  const chatSending = useAppStore((s) => s.chatSending);
  const chatPendingStartedAt = useAppStore((s) => s.chatPendingStartedAt);
  const aiDashboard = useAppStore((s) => s.aiDashboard);

  const loadChatSessions = useAppStore((s) => s.loadChatSessions);
  const createNewChatSession = useAppStore((s) => s.createNewChatSession);
  const selectChatSession = useAppStore((s) => s.selectChatSession);
  const removeChatSession = useAppStore((s) => s.removeChatSession);
  const sendChatPrompt = useAppStore((s) => s.sendChatPrompt);
  const refreshAiDashboard = useAppStore((s) => s.refreshAiDashboard);
  const openLightbox = useAppStore((s) => s.openLightbox);
  const setActiveNav = useAppStore((s) => s.setActiveNav);

  const [promptInput, setPromptInput] = useState('');
  const [threadsOpen, setThreadsOpen] = useState(() =>
    typeof window !== 'undefined' ? window.innerWidth >= 960 : true,
  );
  const [thinkingSecs, setThinkingSecs] = useState(0);
  const [warmingModel, setWarmingModel] = useState(false);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const activeTier: ChatTier = aiDashboard?.state.active_chat_tier ?? 'tier0';
  const tierLabel = CHAT_TIER_LABELS[activeTier];
  const activeSession = chatSessions.find((s) => s.id === activeChatSessionId) ?? null;

  // Load sessions, AI model state, and pre-warm Gemma 4 on GPU when entering Ask AI.
  useEffect(() => {
    void loadChatSessions().catch(() => {});
    if (!aiDashboard) {
      void refreshAiDashboard().catch(() => {});
    }
    void warmupChat()
      .then((res) => {
        if (res.warming) {
          setWarmingModel(true);
          window.setTimeout(() => setWarmingModel(false), 8000);
        }
      })
      .catch(() => {});
  }, [aiDashboard, loadChatSessions, refreshAiDashboard]);

  // Live elapsed timer for the animated thinking bubble.
  useEffect(() => {
    if (!chatSending) {
      setThinkingSecs(0);
      return;
    }
    const base = chatPendingStartedAt ?? Date.now();
    setThinkingSecs(Math.max(0, Math.floor((Date.now() - base) / 1000)));
    const id = window.setInterval(() => {
      setThinkingSecs(Math.max(0, Math.floor((Date.now() - base) / 1000)));
    }, 500);
    return () => window.clearInterval(id);
  }, [chatPendingStartedAt, chatSending]);

  // Smooth auto-scroll when new messages arrive or thinking starts.
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView?.({ behavior: 'smooth', block: 'end' });
  }, [chatMessages.length, chatSending]);

  // Auto-resize composer textarea cleanly.
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(180, Math.max(44, el.scrollHeight))}px`;
  }, [promptInput]);

  useEffect(() => {
    textareaRef.current?.focus();
  }, [activeChatSessionId]);

  const submitPrompt = async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed || chatSending) return;
    setPromptInput('');
    try {
      await sendChatPrompt(trimmed);
    } catch (err) {
      notifyError(err, 'Chat search failed');
    } finally {
      textareaRef.current?.focus();
    }
  };

  const handleFormSubmit = (event: React.FormEvent) => {
    event.preventDefault();
    void submitPrompt(promptInput);
  };

  const handleComposerKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void submitPrompt(promptInput);
    }
  };

  const handleNewChat = async () => {
    try {
      await createNewChatSession();
      textareaRef.current?.focus();
    } catch (err) {
      notifyError(err, 'Could not create conversation');
    }
  };

  const handleDeleteSession = async (sessionId: string) => {
    try {
      await removeChatSession(sessionId);
      notifySuccess('Deleted conversation');
    } catch (err) {
      notifyError(err, 'Could not delete conversation');
    }
  };

  const thinkingLabel =
    activeTier === 'tier0'
      ? 'Searching library…'
      : thinkingSecs >= 5
        ? 'Thinking with Gemma 4 & searching library…'
        : 'Thinking with Gemma 4…';

  return (
    <div className="chat-workspace" aria-label="Ask AI Assistant">
      {threadsOpen && (
        <aside className="chat-threads-rail" aria-label="Conversations">
          <div className="chat-threads-header">
            <button
              type="button"
              className="button button-primary chat-new-btn"
              disabled={chatSending}
              onClick={() => void handleNewChat()}
            >
              <Plus size={14} />
              <span>New Chat</span>
            </button>
            <button
              type="button"
              className="icon-button chat-threads-close-mobile"
              aria-label="Hide conversations"
              onClick={() => setThreadsOpen(false)}
            >
              <X size={15} />
            </button>
          </div>

          <div className="chat-threads-list">
            {chatSessions.length === 0 ? (
              <div className="chat-threads-empty">
                No conversations yet. Ask a question to start one.
              </div>
            ) : (
              chatSessions.map((session) => {
                const isActive = session.id === activeChatSessionId;
                return (
                  <div
                    key={session.id}
                    className={`chat-thread-row${isActive ? ' is-active' : ''}`}
                    onContextMenu={(e) =>
                      openContextMenu(e, [
                        {
                          id: 'thread-open',
                          label: 'Open Conversation',
                          icon: <MessageSquare size={14} />,
                          onSelect: () => {
                            void selectChatSession(session.id);
                          },
                        },
                        { type: 'separator', id: 'thread-sep' },
                        {
                          id: 'thread-del',
                          label: 'Delete Conversation',
                          icon: <Trash2 size={14} />,
                          danger: true,
                          onSelect: () => handleDeleteSession(session.id),
                        },
                      ])
                    }
                  >
                    <button
                      type="button"
                      className="chat-thread-btn"
                      disabled={chatSending}
                      onClick={() => {
                        void selectChatSession(session.id).catch((err) =>
                          notifyError(err, 'Could not load conversation'),
                        );
                        if (window.innerWidth < 960) setThreadsOpen(false);
                      }}
                    >
                      <span className="chat-thread-title">{session.title}</span>
                      <span className="chat-thread-time">
                        {formatShortTime(session.updated_at_ms)}
                      </span>
                    </button>
                    <button
                      type="button"
                      className="icon-button chat-thread-delete"
                      aria-label={`Delete ${session.title}`}
                      title="Delete conversation"
                      disabled={chatSending}
                      onClick={(e) => {
                        e.stopPropagation();
                        void handleDeleteSession(session.id);
                      }}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                );
              })
            )}
          </div>

          <div className="chat-threads-footer">
            <div className="row" style={{ justifyContent: 'space-between', gap: 6 }}>
              <span className="row" style={{ gap: 6, fontSize: 12, color: 'var(--text-secondary)' }}>
                <Sparkles size={13} />
                <span>{tierLabel}</span>
              </span>
              <button
                type="button"
                className="button button-quiet"
                style={{ minHeight: 24, padding: '2px 7px', fontSize: 11 }}
                onClick={() => setActiveNav('settings')}
              >
                Models
              </button>
            </div>
            {warmingModel && (
              <div style={{ fontSize: 11, color: 'var(--text-muted)', marginTop: 4 }}>
                Warming up local model on GPU…
              </div>
            )}
          </div>
        </aside>
      )}

      <div className="chat-main-pane">
        <div className="chat-main-topbar">
          <div className="row" style={{ gap: 8, minWidth: 0 }}>
            <button
              type="button"
              className={`icon-button${threadsOpen ? ' is-active' : ''}`}
              aria-label={threadsOpen ? 'Hide conversations' : 'Show conversations'}
              title={threadsOpen ? 'Hide conversations' : 'Show conversations'}
              onClick={() => setThreadsOpen((prev) => !prev)}
            >
              <PanelLeft size={16} />
            </button>
            <div className="chat-main-title">
              {activeSession?.title || 'New Conversation'}
            </div>
            <span className="badge badge-info">{tierLabel}</span>
          </div>

          <div className="row" style={{ gap: 6 }}>
            <button
              type="button"
              className="button"
              disabled={chatSending}
              onClick={() => void handleNewChat()}
            >
              <Plus size={14} />
              <span>New Chat</span>
            </button>
          </div>
        </div>

        <div className="chat-stream-scroll">
          <div className="chat-stream-inner">
            {chatMessages.length === 0 && !chatSending ? (
              <div className="chat-welcome">
                <div className="chat-welcome-icon" aria-hidden="true">
                  <Sparkles size={22} strokeWidth={1.6} />
                </div>
                <h1 className="chat-welcome-title">Ask anything about your photos</h1>
                <p className="chat-welcome-subtitle">
                  100% local &amp; private. Search by scene, people, camera, location, or text
                  inside images—and refine across turns.
                </p>

                <div className="chat-starter-grid">
                  {STARTER_PROMPTS.map((item) => (
                    <button
                      key={item.prompt}
                      type="button"
                      className="chat-starter-card"
                      onClick={() => void submitPrompt(item.prompt)}
                    >
                      <span className="chat-starter-title">{item.title}</span>
                      <span className="chat-starter-sub">{item.subtitle}</span>
                    </button>
                  ))}
                </div>
              </div>
            ) : (
              <div className="chat-thread-messages">
                {chatMessages.map((msg) => {
                  if (msg.role === 'user') {
                    return (
                      <div key={msg.id} className="chat-bubble-row is-user">
                        <div className="chat-bubble is-user">{msg.content}</div>
                      </div>
                    );
                  }

                  const filterChips = buildFilterChips(msg.plan);
                  const allHitIds = msg.hits.map((h) => h.asset.id);
                  const suggestions = msg.plan?.follow_up_suggestions ?? [];

                  return (
                    <div key={msg.id} className="chat-bubble-row is-assistant">
                      <div className="chat-assistant-avatar" aria-hidden="true">
                        <Sparkles size={13} />
                      </div>
                      <div className="chat-bubble is-assistant">
                        <div className="row" style={{ gap: 8, marginBottom: 4 }}>
                          <span className="chat-assistant-name">TruongPhoto AI</span>
                          {msg.elapsed_ms != null && (
                            <span className="chat-elapsed-pill">
                              {(msg.elapsed_ms / 1000).toFixed(1)}s
                            </span>
                          )}
                          {msg.reranked && (
                            <span className="chat-rerank-badge">
                              <Sparkles size={10} aria-hidden="true" />
                              <span>VLM Reranked</span>
                            </span>
                          )}
                        </div>

                        <div className="chat-assistant-text">{msg.content}</div>

                        {filterChips.length > 0 && (
                          <div className="chat-filter-chips" aria-label="Applied filters">
                            {filterChips.map((chip) => (
                              <span key={chip} className="chat-filter-chip">
                                {chip}
                              </span>
                            ))}
                          </div>
                        )}

                        {msg.hits.length > 0 && (
                          <div className="chat-results-block">
                            <div className="chat-results-meta">
                              {msg.hits.length} matching {msg.hits.length === 1 ? 'item' : 'items'}
                            </div>
                            <div className="chat-hit-grid-full">
                              {msg.hits.map((hit) => {
                                const title =
                                  hit.asset.title || hit.asset.camera_model || 'Media item';
                                const hasTs = hit.timestamp_sec != null && hit.timestamp_sec > 0;
                                return (
                                  <button
                                    key={`${msg.id}-${hit.asset.id}-${hit.timestamp_sec ?? 'photo'}`}
                                    type="button"
                                    className="chat-hit-thumb"
                                    aria-label={`Open ${title}`}
                                    onClick={() =>
                                      openLightbox(
                                        hit.asset.id,
                                        allHitIds,
                                        hit.timestamp_sec ?? null,
                                      )
                                    }
                                    onContextMenu={(e) =>
                                      openContextMenu(e, [
                                        {
                                          id: 'chat-hit-open',
                                          label: 'Open in Viewer',
                                          icon: <Eye size={14} />,
                                          onSelect: () =>
                                            openLightbox(
                                              hit.asset.id,
                                              allHitIds,
                                              hit.timestamp_sec ?? null,
                                            ),
                                        },
                                        {
                                          id: 'chat-hit-download',
                                          label: 'Download Original',
                                          icon: <Download size={14} />,
                                          onSelect: () => {
                                            window.open(
                                              mediaOriginalUrl(hit.asset.id, { download: true }),
                                              '_blank',
                                              'noopener',
                                            );
                                          },
                                        },
                                      ])
                                    }
                                  >
                                    <img
                                      src={mediaThumbUrl(hit.asset.id)}
                                      alt={title}
                                      loading="lazy"
                                    />
                                    {hasTs && (
                                      <span className="search-ts-badge">
                                        <Play size={9} fill="currentColor" />
                                        <span>{formatTimestamp(hit.timestamp_sec!)}</span>
                                      </span>
                                    )}
                                  </button>
                                );
                              })}
                            </div>
                          </div>
                        )}

                        {suggestions.length > 0 && (
                          <div className="chat-suggestions" style={{ padding: 0, marginTop: 4 }}>
                            {suggestions.map((suggestion) => (
                              <button
                                key={suggestion}
                                type="button"
                                className="chat-suggestion-pill"
                                disabled={chatSending}
                                onClick={() => void submitPrompt(suggestion)}
                              >
                                {suggestion}
                              </button>
                            ))}
                          </div>
                        )}
                      </div>
                    </div>
                  );
                })}

                {chatSending && (
                  <div className="chat-bubble-row is-assistant" role="status" aria-live="polite">
                    <div className="chat-assistant-avatar is-pulsing" aria-hidden="true">
                      <Sparkles size={13} />
                    </div>
                    <div className="chat-bubble is-assistant is-thinking">
                      <div className="chat-thinking-line">
                        <span className="chat-thinking-dots" aria-hidden="true">
                          <span />
                          <span />
                          <span />
                        </span>
                        <span className="chat-thinking-label">{thinkingLabel}</span>
                        {thinkingSecs > 0 && (
                          <span className="chat-elapsed-pill">{thinkingSecs}s</span>
                        )}
                      </div>
                    </div>
                  </div>
                )}
              </div>
            )}
            <div ref={messagesEndRef} />
          </div>
        </div>

        <div className="chat-composer-dock">
          <form className="chat-composer-shell" onSubmit={handleFormSubmit}>
            <textarea
              ref={textareaRef}
              rows={1}
              className="chat-composer-input"
              aria-label="Ask AI prompt"
              placeholder="Ask about your photos, people, places, or text…"
              value={promptInput}
              disabled={chatSending}
              onChange={(e) => setPromptInput(e.target.value)}
              onKeyDown={handleComposerKeyDown}
            />
            <button
              type="submit"
              className="chat-send-btn"
              aria-label="Send"
              disabled={chatSending || !promptInput.trim()}
              title="Send (Enter)"
            >
              <ArrowUp size={16} strokeWidth={2.2} />
            </button>
          </form>
          <div className="chat-composer-hint">
            Enter to send · Shift+Enter for new line · 100% local on-device AI
          </div>
        </div>
      </div>
    </div>
  );
};
