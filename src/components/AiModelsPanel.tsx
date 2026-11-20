import React, { useCallback, useEffect, useState } from 'react';
import { Cpu, Download, Pause, Play, RefreshCw, Trash2, X } from 'lucide-react';
import {
  cancelAiModelDownload,
  deleteAiModel,
  downloadAiModel,
  pauseAiIndexing,
  resumeAiIndexing,
  setAiChatTier,
  setAiVisionTier,
  triggerAiReindex,
  updateAiSettings,
} from '../lib/api';
import { formatBytes } from '../lib/format';
import { notifyError, notifySuccess } from '../store/feedback';
import { errorMessage, useAppStore } from '../store/useAppStore';
import type { AiModelStatus, AiTier, ChatTier } from '../types/api';
import { Badge, SegmentedControl, ToggleRow } from './ui';

const TIER_OPTIONS: ReadonlyArray<readonly [AiTier, string]> = [
  ['tier1', 'Tier 1 · Base'],
  ['tier2', 'Tier 2 · Large'],
  ['tier3', 'Tier 3 · SO400M'],
];

const CHAT_TIER_OPTIONS: ReadonlyArray<readonly [ChatTier, string]> = [
  ['tier0', 'Tier 0 Classic'],
  ['tier1', 'Tier 1 Gemma-4-E2B'],
  ['tier2', 'Tier 2 Gemma-4-E4B'],
  ['tier3', 'Tier 3 Gemma-4-26B + VLM'],
];

function formatChatTierLabel(tier: ChatTier): string {
  switch (tier) {
    case 'tier0':
      return 'Tier 0 Classic';
    case 'tier1':
      return 'Tier 1 Gemma-4-E2B';
    case 'tier2':
      return 'Tier 2 Gemma-4-E4B';
    case 'tier3':
      return 'Tier 3 Gemma-4-26B + VLM';
  }
}

const IDLE_TIMEOUT_OPTIONS: ReadonlyArray<readonly [number, string]> = [
  [60, '1m'],
  [300, '5m'],
  [900, '15m'],
  [0, 'Never'],
];

function formatTierLabel(tier: AiTier): string {
  switch (tier) {
    case 'tier1':
      return 'Tier 1';
    case 'tier2':
      return 'Tier 2';
    case 'tier3':
      return 'Tier 3';
  }
}

function statusBadgeTone(status: AiModelStatus): 'ok' | 'info' | 'danger' | 'muted' {
  switch (status) {
    case 'installed':
      return 'ok';
    case 'downloading':
      return 'info';
    case 'error':
      return 'danger';
    default:
      return 'muted';
  }
}

function statusBadgeLabel(status: AiModelStatus): string {
  switch (status) {
    case 'installed':
      return 'Installed ✓';
    case 'downloading':
      return 'Downloading';
    case 'error':
      return 'Error';
    default:
      return 'Not Installed';
  }
}

export const AiModelsPanel: React.FC = () => {
  const aiDashboard = useAppStore((s) => s.aiDashboard);
  const refreshAiDashboard = useAppStore((s) => s.refreshAiDashboard);

  const [loading, setLoading] = useState(!aiDashboard);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busyModelIds, setBusyModelIds] = useState<Record<string, boolean>>({});
  const [busyControl, setBusyControl] = useState<string | null>(null);

  const loadDashboard = useCallback(async () => {
    setLoading((prev) => (aiDashboard ? prev : true));
    setLoadError(null);
    try {
      await refreshAiDashboard();
    } catch (err) {
      setLoadError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, [aiDashboard, refreshAiDashboard]);

  useEffect(() => {
    let active = true;
    void refreshAiDashboard()
      .then(() => {
        if (active) {
          setLoading(false);
          setLoadError(null);
        }
      })
      .catch((err) => {
        if (active) {
          setLoading(false);
          setLoadError(errorMessage(err));
        }
      });
    return () => {
      active = false;
    };
  }, [refreshAiDashboard]);

  const setModelBusy = (id: string, busy: boolean) => {
    setBusyModelIds((prev) => ({ ...prev, [id]: busy }));
  };

  const handleDownloadModel = async (id: string, name: string) => {
    setModelBusy(id, true);
    try {
      await downloadAiModel(id);
      await refreshAiDashboard();
      notifySuccess(`Started download for ${name}`);
    } catch (err) {
      notifyError(err, `Could not download ${name}`);
    } finally {
      setModelBusy(id, false);
    }
  };

  const handleCancelModel = async (id: string, name: string) => {
    setModelBusy(id, true);
    try {
      await cancelAiModelDownload(id);
      await refreshAiDashboard();
      notifySuccess(`Cancelled download for ${name}`);
    } catch (err) {
      notifyError(err, `Could not cancel ${name}`);
    } finally {
      setModelBusy(id, false);
    }
  };

  const handleDeleteModel = async (id: string, name: string) => {
    setModelBusy(id, true);
    try {
      await deleteAiModel(id);
      await refreshAiDashboard();
      notifySuccess(`Removed local weights for ${name}`);
    } catch (err) {
      notifyError(err, `Could not delete ${name}`);
    } finally {
      setModelBusy(id, false);
    }
  };

  const handleSetTier = async (tier: AiTier) => {
    if (!aiDashboard || aiDashboard.state.active_vision_tier === tier) return;
    setBusyControl('tier');
    try {
      await setAiVisionTier(tier);
      await refreshAiDashboard();
      notifySuccess(`Scheduled vision tier switch to ${formatTierLabel(tier)}`);
    } catch (err) {
      notifyError(err, 'Could not change vision tier');
    } finally {
      setBusyControl(null);
    }
  };

  const handleSetChatTier = async (tier: ChatTier, vlmRerankEnabled?: boolean) => {
    if (!aiDashboard) return;
    const nextVlm =
      vlmRerankEnabled !== undefined
        ? vlmRerankEnabled
        : tier === 'tier3'
          ? Boolean(aiDashboard.state.vlm_rerank_enabled)
          : false;
    setBusyControl('chat-tier');
    try {
      await setAiChatTier({ tier, vlm_rerank_enabled: nextVlm });
      await refreshAiDashboard();
      notifySuccess(`Switched conversational search to ${formatChatTierLabel(tier)}`);
    } catch (err) {
      notifyError(err, 'Could not update chat tier');
    } finally {
      setBusyControl(null);
    }
  };

  const handleTogglePause = async () => {
    if (!aiDashboard) return;
    setBusyControl('pause');
    try {
      if (aiDashboard.state.paused) {
        await resumeAiIndexing();
        notifySuccess('Resumed background AI indexing');
      } else {
        await pauseAiIndexing();
        notifySuccess('Paused background AI indexing');
      }
      await refreshAiDashboard();
    } catch (err) {
      notifyError(err, 'Could not update AI indexing state');
    } finally {
      setBusyControl(null);
    }
  };

  const handleReindex = async (scope: 'missing' | 'all') => {
    setBusyControl(`reindex-${scope}`);
    try {
      await triggerAiReindex(scope);
      await refreshAiDashboard();
      notifySuccess(
        scope === 'all' ? 'Queued full AI reindex' : 'Queued AI indexing for pending assets',
      );
    } catch (err) {
      notifyError(err, 'Could not queue AI reindex');
    } finally {
      setBusyControl(null);
    }
  };

  const handleIdleUnloadChange = async (idleUnloadSecs: number) => {
    setBusyControl('idle');
    try {
      await updateAiSettings({ idle_unload_secs: idleUnloadSecs });
      await refreshAiDashboard();
      notifySuccess('Updated AI memory unload timeout');
    } catch (err) {
      notifyError(err, 'Could not update AI settings');
    } finally {
      setBusyControl(null);
    }
  };

  if (loading && !aiDashboard) {
    return (
      <div className="card card-body view-loading" role="status">
        Loading AI models &amp; indexing status…
      </div>
    );
  }

  if (loadError && !aiDashboard) {
    return (
      <div className="error-banner error-banner-dismissible" role="alert">
        <span>{loadError}</span>
        <button type="button" className="button" onClick={() => void loadDashboard()}>
          Retry
        </button>
      </div>
    );
  }

  if (!aiDashboard) return null;

  const { ort_available, ort_ep, hardware, state, models, queue } = aiDashboard;
  const activeChatTier: ChatTier = state.active_chat_tier ?? 'tier0';
  const vlmRerankEnabled = Boolean(state.vlm_rerank_enabled);
  const totalAssets = queue.indexed_assets + queue.pending_assets;
  const stagingTotal = Math.max(1, totalAssets);
  const stagingDone = Math.max(0, stagingTotal - queue.staging_remaining);
  const stagingPct = Math.min(100, Math.round((stagingDone / stagingTotal) * 100));

  return (
    <div className="ai-dashboard">
      <div className="ai-hardware-banner">
        <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
          <Cpu size={15} />
          <span>
            Detected {formatBytes(hardware.total_ram_bytes)} RAM ({hardware.cpu_cores} cores) →{' '}
            <strong>{formatTierLabel(hardware.recommended_vision_tier)} recommended</strong>
          </span>
        </div>
        <span
          className={`ai-ep-badge ${ort_available ? 'is-available' : 'is-unavailable'}`}
          data-testid="ai-ep-badge"
        >
          {ort_available ? ort_ep : 'Not Loaded'}
        </span>
      </div>

      <div className="ai-tier-bar">
        <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 10 }}>
          <div>
            <div style={{ fontWeight: 600, fontSize: 13 }}>Vision Embedding Tier</div>
            <div className="view-subtitle" style={{ marginTop: 2 }}>
              Active: {formatTierLabel(state.active_vision_tier)} · {queue.indexed_assets} indexed ·{' '}
              {queue.pending_assets} pending
            </div>
          </div>

          <SegmentedControl
            value={state.staging_vision_tier ?? state.active_vision_tier}
            options={TIER_OPTIONS}
            onChange={(tier) => void handleSetTier(tier)}
          />
        </div>

        {state.staging_vision_tier && (
          <div className="stack" style={{ gap: 6 }}>
            <div className="row" style={{ justifyContent: 'space-between', fontSize: 12 }}>
              <span>
                Staging migration to {formatTierLabel(state.staging_vision_tier)} (live search stays
                on {formatTierLabel(state.active_vision_tier)})
              </span>
              <span className="property-value">
                {queue.staging_remaining} remaining ({stagingPct}%)
              </span>
            </div>
            <div
              className="ai-model-progress"
              role="progressbar"
              aria-label="Vision tier staging progress"
              aria-valuenow={stagingPct}
              aria-valuemin={0}
              aria-valuemax={100}
            >
              <div style={{ width: `${stagingPct}%` }} />
            </div>
          </div>
        )}

        <div
          className="row"
          style={{
            justifyContent: 'space-between',
            flexWrap: 'wrap',
            gap: 10,
            paddingTop: 8,
            borderTop: '1px solid var(--border-subtle)',
          }}
        >
          <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
            <button
              type="button"
              className="button"
              disabled={busyControl === 'pause'}
              onClick={() => void handleTogglePause()}
            >
              {state.paused ? <Play size={13} /> : <Pause size={13} />}
              <span>{state.paused ? 'Resume Indexing' : 'Pause Indexing'}</span>
            </button>

            <button
              type="button"
              className="button"
              disabled={busyControl === 'reindex-missing'}
              onClick={() => void handleReindex('missing')}
            >
              <RefreshCw size={13} />
              <span>Reindex</span>
            </button>

            <button
              type="button"
              className="button button-quiet"
              disabled={busyControl === 'reindex-all'}
              onClick={() => void handleReindex('all')}
            >
              <span>Reindex All</span>
            </button>
          </div>

          <label className="row" style={{ gap: 8, fontSize: 12 }}>
            <span className="workspace-label">Idle Memory Unload</span>
            <select
              aria-label="Idle Memory Unload"
              className="text-input"
              style={{ width: 'auto', minHeight: 30, padding: '4px 8px' }}
              value={state.idle_unload_secs}
              disabled={busyControl === 'idle'}
              onChange={(e) => void handleIdleUnloadChange(Number(e.target.value))}
            >
              {IDLE_TIMEOUT_OPTIONS.map(([secs, label]) => (
                <option key={secs} value={secs}>
                  {label}
                </option>
              ))}
            </select>
          </label>
        </div>
      </div>

      <div className="chat-tier-section">
        <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 10 }}>
          <div>
            <div style={{ fontWeight: 600, fontSize: 13 }}>Conversational Chat Tier</div>
            <div className="view-subtitle" style={{ marginTop: 2 }}>
              Active: {formatChatTierLabel(activeChatTier)} · Recommended:{' '}
              {formatTierLabel(hardware.recommended_chat_tier)} (instant hot-swap, zero re-indexing)
            </div>
          </div>

          <SegmentedControl
            value={activeChatTier}
            options={CHAT_TIER_OPTIONS}
            onChange={(tier) => {
              if (tier !== activeChatTier) {
                void handleSetChatTier(tier);
              }
            }}
          />
        </div>

        <div style={{ borderTop: '1px solid var(--border-subtle)', paddingTop: 10 }}>
          <ToggleRow
            label="VLM Thumbnail Reranking"
            description="Use Tier 3 multimodal Gemma-4-26B to visually rerank top candidate thumbnails (enabled on Tier 3)."
            checked={vlmRerankEnabled}
            disabled={activeChatTier !== 'tier3' || busyControl === 'chat-tier'}
            onChange={(next) => void handleSetChatTier(activeChatTier, next)}
          />
        </div>
      </div>

      <div className="stack">
        {models.map((model) => {
          const isBusy = Boolean(busyModelIds[model.id]);
          const totalBytes = model.bytes_total > 0 ? model.bytes_total : model.approx_bytes;
          const pct =
            totalBytes > 0
              ? Math.min(100, Math.round((model.bytes_downloaded / totalBytes) * 100))
              : 0;

          return (
            <div key={model.id} className="ai-model-card" data-model-id={model.id}>
              <div
                className="row"
                style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 8 }}
              >
                <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
                  <strong style={{ fontSize: 13 }}>{model.name}</strong>
                  <span className="property-value">{model.id}</span>
                  <Badge>{model.license}</Badge>
                  {model.tier && <Badge tone="info">{formatTierLabel(model.tier)}</Badge>}
                  <Badge tone={statusBadgeTone(model.status)}>
                    {statusBadgeLabel(model.status)}
                  </Badge>
                </div>

                <div className="row" style={{ gap: 8 }}>
                  <span className="property-value">{formatBytes(model.approx_bytes)}</span>

                  {(model.status === 'not_installed' || model.status === 'error') && (
                    <button
                      type="button"
                      className="button button-primary"
                      disabled={isBusy}
                      onClick={() => void handleDownloadModel(model.id, model.name)}
                    >
                      <Download size={13} />
                      <span>Download</span>
                    </button>
                  )}

                  {model.status === 'downloading' && (
                    <button
                      type="button"
                      className="button"
                      disabled={isBusy}
                      onClick={() => void handleCancelModel(model.id, model.name)}
                    >
                      <X size={13} />
                      <span>Cancel</span>
                    </button>
                  )}

                  {model.status === 'installed' && (
                    <button
                      type="button"
                      className="button button-danger-quiet"
                      disabled={isBusy}
                      onClick={() => void handleDeleteModel(model.id, model.name)}
                    >
                      <Trash2 size={13} />
                      <span>Delete</span>
                    </button>
                  )}
                </div>
              </div>

              <div className="view-subtitle" style={{ marginTop: 0 }}>
                {model.hf_repo}
              </div>

              {model.status === 'downloading' && (
                <div className="stack" style={{ gap: 4 }}>
                  <div className="row" style={{ justifyContent: 'space-between', fontSize: 11 }}>
                    <span>
                      {formatBytes(model.bytes_downloaded)} / {formatBytes(totalBytes)}
                    </span>
                    <span className="property-value">{pct}%</span>
                  </div>
                  <div
                    className="ai-model-progress"
                    role="progressbar"
                    aria-label={`${model.name} download progress`}
                    aria-valuenow={pct}
                    aria-valuemin={0}
                    aria-valuemax={100}
                  >
                    <div style={{ width: `${pct}%` }} />
                  </div>
                </div>
              )}

              {model.status === 'error' && model.error_message && (
                <div className="error-banner" role="alert" style={{ marginBottom: 0 }}>
                  {model.error_message}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
};
