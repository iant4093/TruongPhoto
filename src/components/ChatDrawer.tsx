import React, { useEffect, useRef, useState } from 'react';
import {
  MessageSquare,
  Play,
  Plus,
  Send,
  Sparkles,
  Trash2,
  X,
} from 'lucide-react';
import { mediaThumbUrl } from '../lib/api';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { notifyError, notifySuccess } from '../store/feedback';
import { useAppStore } from '../store/useAppStore';
import type { AssetKind, ChatTier, RouterPlan } from '../types/api';
import { Dropdown } from './Dropdown';

const CHAT_TIER_BADGES: Record<ChatTier, string> = {
  tier0: 'Classic Tier 0',
  tier1: 'Gemma-4 Tier 1',
  tier2: 'Gemma-4 Tier 2',
  tier3: 'Tier 3 + VLM',
};

const KIND_LABELS: Record<AssetKind, string> = {
  photo: 'Photo',
  video: 'Video',
  live_photo: 'Live Photo',
  motion_photo: 'Motion Photo',
  raw_jpg_stack: 'RAW + JPG',
};

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
  if (plan.lens_model) {
    chips.push(`Lens: ${plan.lens_model}`);
  }
  if (plan.focal_length_min_mm != null || plan.focal_length_max_mm != null) {
    const fMin = plan.focal_length_min_mm;
    const fMax = plan.focal_length_max_mm;
    const fmtMm = (v: number) => `${Number.isInteger(v) ? v : Number(v.toFixed(1))}mm`;
    if (fMin != null && fMax != null && Math.abs(fMin - fMax) < 0.05) {
      chips.push(`Focal: ${fmtMm(fMin)}`);
    } else if (fMin != null && fMax != null) {
      chips.push(`Focal: ${fmtMm(fMin)}–${fmtMm(fMax)}`);
    } else if (fMin != null) {
      chips.push(`Focal: ≥ ${fmtMm(fMin)}`);
    } else if (fMax != null) {
      chips.push(`Focal: ≤ ${fmtMm(fMax)}`);
    }
  }
  if (plan.aperture_min_f != null || plan.aperture_max_f != null) {
    const aMin = plan.aperture_min_f;
    const aMax = plan.aperture_max_f;
    const fmtF = (v: number) => `f/${Number(v.toFixed(1))}`;
    if (aMin != null && aMax != null && Math.abs(aMin - aMax) < 0.05) {
      chips.push(`Aperture: ${fmtF(aMin)}`);
    } else if (aMin != null && aMax != null) {
      chips.push(`Aperture: ${fmtF(aMin)}–${fmtF(aMax)}`);
    } else if (aMin != null) {
      chips.push(`Aperture: ≥ ${fmtF(aMin)}`);
    } else if (aMax != null) {
      chips.push(`Aperture: ≤ ${fmtF(aMax)}`);
    }
  }
  if (plan.iso_min != null || plan.iso_max != null) {
    const iMin = plan.iso_min;
    const iMax = plan.iso_max;
    if (iMin != null && iMax != null && iMin === iMax) {
      chips.push(`ISO: ${iMin}`);
    } else if (iMin != null && iMax != null) {
      chips.push(`ISO: ${iMin}–${iMax}`);
    } else if (iMin != null) {
      chips.push(`ISO: ≥ ${iMin}`);
    } else if (iMax != null) {
      chips.push(`ISO: ≤ ${iMax}`);
    }
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
      chips.push(`Date range: ${from} → ${to}`);
    } else if (from) {
      chips.push(`Date range: From ${from}`);
    } else if (to) {
      chips.push(`Date range: Until ${to}`);
    }
  }

  return chips;
}

export const ChatDrawer: React.FC = () => {
  const chatDrawerOpen = useAppStore((s) => s.chatDrawerOpen);
  const chatSessions = useAppStore((s) => s.chatSessions);
  const activeChatSessionId = useAppStore((s) => s.activeChatSessionId);
  const chatMessages = useAppStore((s) => s.chatMessages);
  const chatSending = useAppStore((s) => s.chatSending);
  const aiDashboard = useAppStore((s) => s.aiDashboard);

  const setChatDrawerOpen = useAppStore((s) => s.setChatDrawerOpen);
  const loadChatSessions = useAppStore((s) => s.loadChatSessions);
  const createNewChatSession = useAppStore((s) => s.createNewChatSession);
  const selectChatSession = useAppStore((s) => s.selectChatSession);
  const removeChatSession = useAppStore((s) => s.removeChatSession);
  const sendChatPrompt = useAppStore((s) => s.sendChatPrompt);
  const openLightbox = useAppStore((s) => s.openLightbox);

  const [promptInput, setPromptInput] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const activeTier: ChatTier = aiDashboard?.state.active_chat_tier ?? 'tier0';
  const tierBadgeLabel = CHAT_TIER_BADGES[activeTier];

  useEffect(() => {
    if (!chatDrawerOpen) return;
    void loadChatSessions().catch(() => {});
  }, [chatDrawerOpen, loadChatSessions]);

  useEffect(() => {
    if (!chatDrawerOpen) return;
    const layer = pushEscapeLayer();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer)) {
        event.preventDefault();
        event.stopPropagation();
        setChatDrawerOpen(false);
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [chatDrawerOpen, setChatDrawerOpen]);

  useEffect(() => {
    if (chatDrawerOpen) {
      inputRef.current?.focus();
    }
  }, [chatDrawerOpen]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView?.({ block: 'end' });
  }, [chatMessages.length]);

  if (!chatDrawerOpen) {
    return null;
  }

  const submitPrompt = async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed || chatSending) return;
    try {
      await sendChatPrompt(trimmed);
    } catch (err) {
      notifyError(err, 'Chat search failed');
    }
  };

  const handleFormSubmit = (event: React.FormEvent) => {
    event.preventDefault();
    const text = promptInput.trim();
    if (!text || chatSending) return;
    setPromptInput('');
    void submitPrompt(text);
  };

  const handleNewChat = async () => {
    try {
      await createNewChatSession();
      inputRef.current?.focus();
    } catch (err) {
      notifyError(err, 'Could not create chat session');
    }
  };

  const handleDeleteSession = async () => {
    if (!activeChatSessionId) return;
    try {
      await removeChatSession(activeChatSessionId);
      notifySuccess('Deleted chat session');
    } catch (err) {
      notifyError(err, 'Could not delete chat session');
    }
  };

  return (
    <div className="chat-drawer-slot">
      <aside className="chat-drawer" aria-label="Ask AI conversational search">
        <header className="chat-header">
          <div className="row" style={{ justifyContent: 'space-between', gap: 8 }}>
            <div className="row" style={{ gap: 6 }}>
              <Sparkles size={14} aria-hidden="true" />
              <strong style={{ fontSize: 13 }}>Ask AI</strong>
              <span className="badge badge-info">{tierBadgeLabel}</span>
            </div>

            <button
              type="button"
              className="icon-button"
              aria-label="Close Ask AI"
              onClick={() => setChatDrawerOpen(false)}
            >
              <X size={16} />
            </button>
          </div>

          <div className="row" style={{ gap: 6 }}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <Dropdown
                label="Chat session"
                value={activeChatSessionId ?? ''}
                disabled={chatSending}
                placeholder={
                  chatSessions.length === 0 ? 'New conversation' : 'Select conversation…'
                }
                options={chatSessions.map((session) => [session.id, session.title] as const)}
                onChange={(nextId) => {
                  if (!nextId) return;
                  void selectChatSession(nextId).catch((err) =>
                    notifyError(err, 'Could not load chat session'),
                  );
                }}
              />
            </div>

            <button
              type="button"
              className="button"
              style={{ minHeight: 30, padding: '4px 9px' }}
              disabled={chatSending}
              onClick={() => void handleNewChat()}
            >
              <Plus size={13} />
              <span>New Chat</span>
            </button>

            <button
              type="button"
              className="icon-button"
              aria-label="Delete chat session"
              title="Delete chat session"
              disabled={!activeChatSessionId || chatSending}
              onClick={() => void handleDeleteSession()}
            >
              <Trash2 size={14} />
            </button>
          </div>
        </header>

        <div className="chat-messages">
          {chatMessages.length === 0 ? (
            <div className="empty-state" style={{ marginTop: 12 }}>
              <MessageSquare size={24} strokeWidth={1.3} />
              <div className="empty-state-title">Conversational Library Search</div>
              <div className="empty-state-description">
                Ask for moments, people, places, OCR text, or video timestamps and refine across
                turns.
              </div>
            </div>
          ) : (
            chatMessages.map((msg) => {
              if (msg.role === 'user') {
                return (
                  <div key={msg.id} className="chat-turn is-user">
                    {msg.content}
                  </div>
                );
              }

              const filterChips = buildFilterChips(msg.plan);
              const allHitIds = msg.hits.map((h) => h.asset.id);
              const suggestions = msg.plan?.follow_up_suggestions ?? [];

              return (
                <div key={msg.id} className="chat-turn is-assistant">
                  {msg.reranked && (
                    <span className="chat-rerank-badge">
                      <Sparkles size={11} aria-hidden="true" />
                      <span>VLM Reranked</span>
                    </span>
                  )}

                  <div>{msg.content}</div>

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
                    <div className="chat-hit-grid">
                      {msg.hits.map((hit) => {
                        const title = hit.asset.title || hit.asset.camera_model || 'Media item';
                        const hasTs = hit.timestamp_sec != null && hit.timestamp_sec > 0;
                        return (
                          <button
                            key={`${msg.id}-${hit.asset.id}-${hit.timestamp_sec ?? 'photo'}`}
                            type="button"
                            className="chat-hit-thumb"
                            aria-label={`Open ${title}`}
                            onClick={() =>
                              openLightbox(hit.asset.id, allHitIds, hit.timestamp_sec ?? null)
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
                  )}

                  {suggestions.length > 0 && (
                    <div className="chat-suggestions" style={{ padding: 0 }}>
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
              );
            })
          )}
          {chatSending && (
            <div className="chat-turn is-assistant is-thinking" role="status" aria-live="polite">
              <div className="chat-thinking-line">
                <span className="chat-thinking-dots" aria-hidden="true">
                  <span />
                  <span />
                  <span />
                </span>
                <span className="chat-thinking-label">Thinking…</span>
              </div>
            </div>
          )}
          <div ref={messagesEndRef} />
        </div>

        <form className="chat-composer" onSubmit={handleFormSubmit}>
          <input
            ref={inputRef}
            type="text"
            className="text-input"
            aria-label="Ask AI prompt"
            placeholder="Ask about photos or videos…"
            value={promptInput}
            disabled={chatSending}
            onChange={(e) => setPromptInput(e.target.value)}
          />
          <button
            type="submit"
            className="button button-primary"
            aria-label="Send"
            disabled={chatSending || !promptInput.trim()}
          >
            <Send size={13} />
            <span>{chatSending ? 'Thinking…' : 'Send'}</span>
          </button>
        </form>
      </aside>
    </div>
  );
};
